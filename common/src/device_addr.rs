#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct DeviceAddr(pub Inner);

type Inner = [u8; 6];

impl DeviceAddr {
    pub const SIZE: usize = size_of::<Inner>();

    pub fn encode_hex<'a>(&self, buf: &'a mut [u8; 12]) -> &'a str {
        const HEX: &[u8; 16] = b"0123456789ABCDEF";

        let (chunks, _) = buf.as_chunks_mut::<2>();
        for ([hi, lo], byte) in chunks.iter_mut().zip(self.0.iter().rev()) {
            *hi = HEX[usize::from(byte >> 4)];
            *lo = HEX[usize::from(byte & 0xF)];
        }

        core::str::from_utf8(buf).expect("every byte comes from hex array")
    }

    pub fn as_i64(&self) -> i64 {
        let [a0, a1, a2, a3, a4, a5] = self.0;

        i64::from_le_bytes([a0, a1, a2, a3, a4, a5, 0, 0])
    }

    /// Builds an address from the two `FICR.DEVICEADDR` words.
    ///
    /// `DEVICEADDR[0]` holds the low 32 bits and `DEVICEADDR[1]` the high 16;
    /// the upper half of the second word is not part of the address and is
    /// discarded.
    ///
    /// The top two bits of the most significant byte are forced to `1`. Nordic
    /// programs `DEVICEADDR` with a random value that is not a registered
    /// public address, so it may only be advertised as a *static random*
    /// address, and the Bluetooth spec marks those by that bit pattern. Doing
    /// it here rather than at the advertiser keeps one definition of "this
    /// device's address" — the same bytes reach the air, the database and the
    /// label on the enclosure.
    ///
    /// This is the only place the address is derived, and both the sensor
    /// firmware and `homescope-provision` call it: a provisioning tool that
    /// computed the address differently would register a device under a name
    /// its own packets never use.
    pub fn from_ficr(addr0: u32, addr1: u32) -> Self {
        let [a0, a1, a2, a3] = addr0.to_le_bytes();
        let [a4, a5, _, _] = addr1.to_le_bytes();

        Self([a0, a1, a2, a3, a4, a5 | 0xC0])
    }
}

impl core::fmt::Display for DeviceAddr {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let mut buf = [0u8; 12];

        f.write_str(self.encode_hex(&mut buf))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceAddrParseError {
    BadFormat,
    NotHex,
}

impl core::fmt::Display for DeviceAddrParseError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::BadFormat => f.write_str("expected format XXXXXXXXXXXX"),
            Self::NotHex => f.write_str("contains non-hex characters"),
        }
    }
}

impl core::error::Error for DeviceAddrParseError {}

impl core::str::FromStr for DeviceAddr {
    type Err = DeviceAddrParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.len() != 12 {
            return Err(DeviceAddrParseError::BadFormat);
        }

        if !s.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(DeviceAddrParseError::NotHex);
        }

        let [b0, b1, b2, b3, b4, b5, _, _] = u64::from_str_radix(s, 16)
            .map_err(|_| DeviceAddrParseError::NotHex)?
            .to_le_bytes();

        Ok(Self([b0, b1, b2, b3, b4, b5]))
    }
}

#[cfg(feature = "serde")]
impl serde::Serialize for DeviceAddr {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut buf = [0u8; 12];

        serializer.serialize_str(self.encode_hex(&mut buf))
    }
}

#[cfg(feature = "serde")]
impl<'de> serde::Deserialize<'de> for DeviceAddr {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct AddrVisitor;

        impl serde::de::Visitor<'_> for AddrVisitor {
            type Value = DeviceAddr;

            fn expecting(&self, formatter: &mut core::fmt::Formatter) -> core::fmt::Result {
                formatter.write_str("a 12-digit hex device addr")
            }

            fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                v.parse().map_err(E::custom)
            }
        }

        deserializer.deserialize_str(AddrVisitor)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceAddrRangeError(pub u64);

impl core::fmt::Display for DeviceAddrRangeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "value is larger than 48 bits: {:#X}", self.0)
    }
}

impl core::error::Error for DeviceAddrRangeError {}

impl TryFrom<u64> for DeviceAddr {
    type Error = DeviceAddrRangeError;

    fn try_from(value: u64) -> Result<Self, Self::Error> {
        let [addr @ .., 0, 0] = value.to_le_bytes() else {
            return Err(DeviceAddrRangeError(value));
        };

        Ok(Self(addr))
    }
}

#[cfg(test)]
mod test {
    use core::str::FromStr as _;

    use super::*;

    /// Fixed FICR words in, fixed address out.
    ///
    /// Two independent decisions live in `from_ficr` — which word holds which
    /// half, and the little-endian split within each — and both are invisible
    /// until a provisioned node advertises under an address the API has never
    /// heard of. The high half of `addr1` is filled with `0xFF` here because
    /// those bits are not part of the address and must be discarded rather
    /// than folded in.
    #[test]
    fn from_ficr_reads_the_words_little_endian() {
        let addr = DeviceAddr::from_ficr(0x0403_0201, 0xFFFF_0605);

        assert_eq!(addr.0, [0x01, 0x02, 0x03, 0x04, 0x05, 0xC6]);
    }

    /// The two top bits mark a static random address. Without them the packet
    /// is advertised under an address type it is not entitled to use.
    #[test]
    fn from_ficr_marks_the_address_static_random() {
        for high in [0x0000_0000, 0x0000_00FF, 0x0000_0040, 0x0000_0080] {
            let addr = DeviceAddr::from_ficr(0, high);

            assert_eq!(
                addr.0[5] & 0xC0,
                0xC0,
                "top two bits must be set for high word {high:#X}"
            );
        }
    }

    /// The bits below the type marker are the chip's, and must survive.
    #[test]
    fn from_ficr_keeps_the_low_bits_of_the_top_byte() {
        let addr = DeviceAddr::from_ficr(0, 0x0000_2500);

        assert_eq!(addr.0[5], 0xE5);
    }

    /// `Display` is most significant byte first, which is how a BLE sniffer,
    /// the `devices` table and the label on an enclosure all render it — the
    /// reverse of the in-memory order.
    #[test]
    fn display_is_most_significant_byte_first() {
        let addr = DeviceAddr::from_ficr(0x0403_0201, 0xFFFF_0605);
        let mut buf = [0u8; 12];

        assert_eq!(addr.encode_hex(&mut buf), "C60504030201");
    }

    /// Parsing undoes `Display`, so an address read off a label round-trips.
    #[test]
    fn parse_undoes_display() {
        let addr = DeviceAddr::from_ficr(0x0403_0201, 0xFFFF_0605);
        let mut buf = [0u8; 12];
        let rendered = addr.encode_hex(&mut buf);

        assert_eq!(DeviceAddr::from_str(rendered), Ok(addr));
    }

    #[test]
    fn parse_rejects_the_wrong_length_and_non_hex() {
        assert_eq!(
            DeviceAddr::from_str("C6050403020"),
            Err(DeviceAddrParseError::BadFormat)
        );
        assert_eq!(
            DeviceAddr::from_str("C605040302011"),
            Err(DeviceAddrParseError::BadFormat)
        );
        assert_eq!(
            DeviceAddr::from_str("C6050403020G"),
            Err(DeviceAddrParseError::NotHex)
        );
    }

    /// `as_i64` is the `devices.device_addr` column, so it has to agree with
    /// the in-memory byte order rather than with `Display`.
    #[test]
    fn as_i64_is_little_endian_and_never_negative() {
        let addr = DeviceAddr::from_ficr(0x0403_0201, 0xFFFF_0605);

        assert_eq!(addr.as_i64(), 0x0000_C605_0403_0201);
        assert!(addr.as_i64() > 0, "48 bits always fit a positive i64");
    }

    #[test]
    fn try_from_u64_rejects_more_than_48_bits() {
        assert_eq!(
            DeviceAddr::try_from(0x0000_C605_0403_0201),
            Ok(DeviceAddr([0x01, 0x02, 0x03, 0x04, 0x05, 0xC6]))
        );
        assert_eq!(
            DeviceAddr::try_from(0x0001_0000_0000_0000),
            Err(DeviceAddrRangeError(0x0001_0000_0000_0000))
        );
    }

    /// The JSON spelling, pinned as a literal.
    ///
    /// `Serialize` routes through `encode_hex`, so this looks redundant next
    /// to `display_is_most_significant_byte_first` — it is not. That test
    /// pins the rendering; this pins that serde *uses* it, and that the
    /// address is a bare string rather than a wrapped or byte-array form.
    /// Both `ObservationEnvelope` and every `homescope-api-types` DTO embed
    /// this, so it is the most widely depended-on line of the JSON contract.
    #[test]
    #[cfg(feature = "serde")]
    fn serializes_as_a_bare_hex_string() {
        let addr = DeviceAddr([0x01, 0x02, 0x03, 0x04, 0x05, 0xC6]);

        assert_eq!(
            serde_json::to_value(addr).expect("serializes"),
            serde_json::json!("C60504030201")
        );
    }

    /// Regression: `Deserialize` was written as `let s: &str = …`, which asks
    /// the deserializer for a string *borrowed out of the input buffer*.
    ///
    /// That works under `serde_json::from_slice` — which is what axum's
    /// `Json` extractor uses — and fails under `from_reader`, which is what
    /// `ureq`'s `Body::read_json` uses in `homescope-provision`. So the API
    /// parsed addresses happily while the provisioning tool got `invalid
    /// type: string "…", expected a borrowed string` on every response.
    ///
    /// A reader-backed deserializer is the only shape that catches it: it has
    /// no persistent buffer to lend out, so it can only ever offer a
    /// transient `&str`. Note the bound does not help — `DeviceAddr` still
    /// satisfies `DeserializeOwned`, because the impl is generic over every
    /// `'de`; the borrow requirement only appears at runtime.
    #[test]
    #[cfg(feature = "serde")]
    fn deserializes_from_a_reader() {
        let json = br#""C60504030201""#;

        let addr: DeviceAddr = serde_json::from_reader(&json[..]).expect("reader-backed");

        assert_eq!(addr, DeviceAddr([0x01, 0x02, 0x03, 0x04, 0x05, 0xC6]));
    }

    /// The second case a borrowing impl fails on, and the one that would have
    /// bitten even without `from_reader`: an escaped string has to be
    /// unescaped into a scratch buffer first, so it too arrives transient.
    #[test]
    #[cfg(feature = "serde")]
    fn deserializes_an_escaped_string() {
        // The same address, with its leading 'C' written as a JSON `\u`
        // escape so that serde_json cannot hand out a slice of the input.
        let escaped_c = "\\u0043";
        let json = alloc::format!("\"{escaped_c}60504030201\"");

        let addr: DeviceAddr = serde_json::from_str(&json).expect("escapes are still hex");

        assert_eq!(addr, DeviceAddr([0x01, 0x02, 0x03, 0x04, 0x05, 0xC6]));
    }

    /// Parse failures must surface as serde errors, not panics, and must not
    /// be silently coerced — a mistyped address in a JSON body is a rejected
    /// request, never a different device.
    #[test]
    #[cfg(feature = "serde")]
    fn rejects_a_malformed_address() {
        for bad in [r#""C605040302""#, r#""C6050403020G""#, "42", "null"] {
            assert!(
                serde_json::from_str::<DeviceAddr>(bad).is_err(),
                "{bad} was accepted"
            );
        }
    }
}
