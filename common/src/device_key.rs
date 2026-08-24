//! The per-device AEAD key, in the clear.
//!
//! One device, one key — [`packet::cipher::PacketCipher`] binds this to a
//! [`DeviceAddr`] and never sees another. It lives in the sensor's UICR and,
//! sealed under a KEK, in the API's `devices` table; this type is the shape it
//! takes in memory in between.
//!
//! [`DeviceAddr`]: crate::device_addr::DeviceAddr
//! [`packet::cipher::PacketCipher`]: crate::packet::cipher::PacketCipher
//!
//! # Handling rules
//!
//! **Not `Clone`, not `Copy` — deliberately.** `Copy` would defeat
//! [`ZeroizeOnDrop`] outright: every use would scatter duplicates across stack
//! frames and only the one binding that happens to be dropped would ever be
//! cleared. A secret wants exactly one owner and a move-only API, so `Drop`
//! means something. Do not derive either, and do not derive `Clone` on
//! anything that *contains* one.
//!
//! **`Debug` is redacted; `Display` does not exist.** `Debug` is the impl that
//! fires by accident — a `#[derive(Debug)]` on an enclosing struct, a `{:?}`
//! in a `tracing` field — so it has to be safe. `Display` is only ever reached
//! deliberately, so its absence turns `{}` on a key into a compile error
//! rather than a leaked secret. There is no `Serialize` for the same reason:
//! the one place a key is serialized (the show-once provisioning response) can
//! convert explicitly.
//!
//! **Zeroization is not a guarantee.** It clears this binding, and nothing
//! about copies made elsewhere — the `Vec` an AEAD `decrypt` returns, sqlx row
//! buffers, anything swapped to disk. It narrows the window; it does not close
//! it.

use thiserror::Error;
use zeroize::{Zeroize, ZeroizeOnDrop};

#[derive(Zeroize, ZeroizeOnDrop)]
pub struct DeviceKey([u8; Self::SIZE]);

impl core::fmt::Debug for DeviceKey {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "DeviceKey(<redacted>)")
    }
}

impl DeviceKey {
    pub const SIZE: usize = 32;

    pub const fn from_bytes(bytes: [u8; Self::SIZE]) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; Self::SIZE] {
        &self.0
    }

    #[cfg(feature = "keygen")]
    pub fn generate() -> Result<Self, KeyGenError> {
        let mut bytes: [u8; Self::SIZE] = [0; _];

        getrandom::fill(&mut bytes)?;

        Ok(Self(bytes))
    }

    /// Renders a device key as uppercase hex — the only place a plaintext key
    /// becomes text.
    ///
    /// `DeviceKey` deliberately implements neither `Display` nor `Serialize`, so
    /// `{}` on a key, or a `#[derive(Serialize)]` on a struct holding one, is a
    /// compile error rather than a leak. That makes this function the single
    /// explicit escape hatch, and the one thing to grep for when asking how a key
    /// could reach a wire.
    ///
    /// # Why hex, not base64
    ///
    /// Provisioning writes the key into `UICR.CUSTOMER` and reads it back to
    /// verify byte-for-byte, and that record's layout is chosen so a hex dump of
    /// `0x10001084` shows the key in order (see `homescope-board`'s
    /// `chip::device_key`). Hex is the only encoding comparable against such a
    /// dump by eye — which is what you are reduced to when a freshly provisioned
    /// node's packets fail their AEAD tag with no other symptom. Base64 would
    /// save twenty characters and cost that.
    ///
    /// # Zeroization
    ///
    /// Taking the key by value means it drops, and so zeroizes, when this returns.
    /// The `String` does not, and neither do serde's serialization buffer or
    /// hyper's write buffer — all three are heap allocations still holding the
    /// plaintext when they are freed. `Zeroizing<String>` would clear the first
    /// and none of the rest, so the window is narrowed at the source and open
    /// downstream by design. Read that as a reason not to add further copies of
    /// the rendered key, not as a gap to close here.
    pub fn to_hex(&self) -> HexKey {
        const HEX_CHARS: &[u8; 16] = b"0123456789ABCDEF";

        let mut hex_buffer = [0u8; HexKey::SIZE];

        for (i, byte) in self.as_bytes().iter().enumerate() {
            hex_buffer[i * 2] = HEX_CHARS[(byte >> 4) as usize];
            hex_buffer[i * 2 + 1] = HEX_CHARS[(byte & 0x0F) as usize];
        }

        HexKey(hex_buffer)
    }

    pub fn from_hex(hex: &str) -> Result<Self, FromHexError> {
        if hex.len() != HexKey::SIZE {
            return Err(FromHexError::Len(hex.len()));
        }

        if let Some(invalid_char) = hex.chars().find(|c| !c.is_ascii_hexdigit()) {
            return Err(FromHexError::InvalidChar(invalid_char));
        }
        let mut buffer = [0u8; Self::SIZE];

        for (i, byte) in buffer.iter_mut().enumerate() {
            let start = i * 2;
            let end = start + 2;
            let val = &hex[start..end];

            *byte = u8::from_str_radix(val, 16).expect("already verified whole hex string");
        }

        Ok(Self(buffer))
    }
}

#[cfg(feature = "keygen")]
pub type KeyGenError = getrandom::Error;

#[derive(Debug, Error)]
pub enum FromHexError {
    #[error("key has invalid len: {0}; should be {len} characters", len = HexKey::SIZE)]
    Len(usize),

    #[error("key has invalid, non-hex character: {0}")]
    InvalidChar(char),
}

#[derive(ZeroizeOnDrop)]
pub struct HexKey([u8; HexKey::SIZE]);
impl HexKey {
    pub const SIZE: usize = DeviceKey::SIZE * 2;

    pub fn as_str(&self) -> &str {
        core::str::from_utf8(&self.0).expect("hex digits are ASCII")
    }
}

#[cfg(test)]
mod test {
    use std::{format, string::String};

    use super::*;

    const KEY: DeviceKey = DeviceKey::from_bytes([0xAB; DeviceKey::SIZE]);

    /// A key whose leading bytes are all distinct, so a test can catch a
    /// reversed or shifted nibble that `[0xAB; 32]` would sail through.
    const MIXED_BYTES: [u8; DeviceKey::SIZE] = {
        let mut bytes = [0u8; DeviceKey::SIZE];
        bytes[0] = 0x00;
        bytes[1] = 0x0F;
        bytes[2] = 0xA5;
        bytes[DeviceKey::SIZE - 1] = 0xFF;
        bytes
    };

    fn mixed_hex() -> String {
        format!("000FA5{}FF", "00".repeat(DeviceKey::SIZE - 4))
    }

    #[test]
    fn bytes_round_trip() {
        assert_eq!(KEY.as_bytes(), &[0xAB; DeviceKey::SIZE]);
    }

    /// `Debug` is the impl that reaches a log line by accident. If this ever
    /// starts printing bytes, every `{:?}` on an enclosing struct leaks a key.
    #[test]
    fn debug_is_redacted() {
        assert_eq!(format!("{KEY:?}"), "DeviceKey(<redacted>)");
    }

    /// Fixed input, fixed output. The rendering is a contract with
    /// `homescope-provision`, which decodes this string back into the 32 bytes
    /// it writes to `UICR.CUSTOMER` — a change of case or byte order here is a
    /// device whose packets fail their AEAD tag with no other symptom.
    ///
    /// It is also what a hex dump of `0x10001084` is compared against by eye,
    /// which is the reason the encoding is hex rather than base64.
    #[test]
    fn key_renders_as_uppercase_hex_in_order() {
        assert_eq!(
            DeviceKey::from_bytes(MIXED_BYTES).to_hex().as_str(),
            mixed_hex(),
        );
    }

    /// Length is what a client validates before decoding, so pin it separately
    /// from the byte order above.
    #[test]
    fn key_renders_to_two_chars_per_byte() {
        let key = DeviceKey::from_bytes([0xAB; DeviceKey::SIZE]).to_hex();
        let hex = key.as_str();

        assert_eq!(hex.len(), DeviceKey::SIZE * 2);
        assert_eq!(hex, "AB".repeat(DeviceKey::SIZE));
    }

    /// The other half of the same contract, checked against the same literal
    /// rather than by round-tripping `to_hex`.
    ///
    /// A round-trip would pass even if both halves reversed their nibbles
    /// together, which is precisely the change that would break every device
    /// already provisioned.
    #[test]
    fn from_hex_reads_the_documented_string() {
        let key = DeviceKey::from_hex(&mixed_hex()).expect("valid hex");

        assert_eq!(key.as_bytes(), &MIXED_BYTES);
    }

    /// Provisioning responses and hand-typed keys both arrive uppercase, but
    /// anything that has been through a shell pipeline may not have.
    #[test]
    fn from_hex_accepts_either_case() {
        let upper = DeviceKey::from_hex(&"AB".repeat(DeviceKey::SIZE)).expect("uppercase");
        let lower = DeviceKey::from_hex(&"ab".repeat(DeviceKey::SIZE)).expect("lowercase");

        assert_eq!(upper.as_bytes(), lower.as_bytes());
        assert_eq!(upper.as_bytes(), &[0xAB; DeviceKey::SIZE]);
    }

    #[test]
    fn from_hex_rejects_the_wrong_length() {
        let short = "AB".repeat(DeviceKey::SIZE - 1);
        let long = "AB".repeat(DeviceKey::SIZE + 1);

        assert!(matches!(
            DeviceKey::from_hex(&short),
            Err(FromHexError::Len(62))
        ));
        assert!(matches!(
            DeviceKey::from_hex(&long),
            Err(FromHexError::Len(66))
        ));
        assert!(matches!(DeviceKey::from_hex(""), Err(FromHexError::Len(0))));
    }

    #[test]
    fn from_hex_rejects_non_hex_digits() {
        let mut hex = mixed_hex();
        hex.replace_range(0..1, "Z");

        assert!(matches!(
            DeviceKey::from_hex(&hex),
            Err(FromHexError::InvalidChar(_))
        ));
    }

    /// Errors must never echo the key, because the caller logs them.
    ///
    /// `from_hex` runs over operator input and over provisioning responses, and
    /// the natural thing to write in an error message — "could not parse
    /// `<the key>`" — puts a plaintext key in a log file that outlives it.
    #[test]
    fn from_hex_errors_do_not_echo_the_input() {
        let mut hex = mixed_hex();
        hex.replace_range(0..1, "Z");

        let message = format!("{}", DeviceKey::from_hex(&hex).unwrap_err());

        assert!(
            !message.contains("A5"),
            "error message leaked key material: {message}"
        );
    }

    /// `u8::from_str_radix` accepted a leading `+`, so `"+A"` parsed
    /// as `0x0A` and a mistyped key silently decoded to a *different* key than
    /// was typed — a node whose UICR looks right in a hex dump and whose
    /// packets fail their tag anyway.
    #[test]
    fn from_hex_rejects_a_signed_digit_pair() {
        let hex = format!("+A{}", "00".repeat(DeviceKey::SIZE - 1));

        assert!(DeviceKey::from_hex(&hex).is_err());
    }

    /// `hex.len()` counts bytes, so a 64-*byte* string holding multi-byte
    /// UTF-8 passes the length check and then panics inside `&hex[start..end]`
    /// when the slice cuts a character. The input here comes from a CLI
    /// argument or an HTTP response, so a panic is reachable from outside.
    #[test]
    fn from_hex_rejects_non_ascii_without_panicking() {
        let hex = format!("€{}", "6".repeat(HexKey::SIZE - "€".len()));
        assert_eq!(
            hex.len(),
            HexKey::SIZE,
            "the length check must not catch it"
        );

        assert!(DeviceKey::from_hex(&hex).is_err());
    }

    /// The rendered key is a `HexKey`, not a `String`, so the buffer is cleared
    /// on drop and `{}` on it is still a compile error — the caller has to
    /// reach for `as_str`, which is the thing to grep for.
    #[test]
    fn hex_key_is_the_documented_length() {
        assert_eq!(HexKey::SIZE, DeviceKey::SIZE * 2);
        assert_eq!(
            DeviceKey::from_bytes(MIXED_BYTES).to_hex().as_str().len(),
            HexKey::SIZE
        );
    }
}
