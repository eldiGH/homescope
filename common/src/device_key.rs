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
}

#[cfg(feature = "keygen")]
pub type KeyGenError = getrandom::Error;

#[cfg(test)]
mod test {
    use std::format;

    use super::*;

    const KEY: DeviceKey = DeviceKey::from_bytes([0xAB; DeviceKey::SIZE]);

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
}
