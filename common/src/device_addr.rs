#[cfg(feature = "wire")]
use bytemuck::{Pod, Zeroable};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "wire", repr(transparent))]
#[cfg_attr(feature = "wire", derive(Pod, Zeroable))]
pub struct DeviceAddr(pub [u8; 6]);

impl DeviceAddr {
    pub fn encode_hex<'a>(&self, buf: &'a mut [u8; 12]) -> &'a str {
        const HEX: &[u8; 16] = b"0123456789ABCDEF";

        let (chunks, _) = buf.as_chunks_mut::<2>();
        for ([hi, lo], byte) in chunks.iter_mut().zip(self.0.iter().rev()) {
            *hi = HEX[usize::from(byte >> 4)];
            *lo = HEX[usize::from(byte & 0xF)];
        }

        core::str::from_utf8(buf).expect("every byte comes from hex array")
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
        let s: &str = serde::Deserialize::deserialize(deserializer)?;
        s.parse().map_err(serde::de::Error::custom)
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
