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
}
