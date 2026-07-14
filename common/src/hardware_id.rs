#[cfg(feature = "wire")]
use bytemuck::{Pod, Zeroable};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "wire", repr(transparent))]
#[cfg_attr(feature = "wire", derive(Pod, Zeroable))]
pub struct HardwareId(pub u64);

impl HardwareId {
    pub fn encode_hex<'a>(&self, buf: &'a mut [u8; 16]) -> &'a str {
        const HEX: &[u8; 16] = b"0123456789ABCDEF";
        for (i, b) in buf.iter_mut().enumerate() {
            *b = HEX[((self.0 >> (60 - 4 * i)) & 0xF) as usize];
        }
        core::str::from_utf8(buf).expect("every byte comes from hex array")
    }
}

impl core::fmt::Display for HardwareId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let mut buf = [0u8; 16];

        f.write_str(self.encode_hex(&mut buf))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HardwareIdParseError {
    BadFormat,
    NotHex,
}

impl core::fmt::Display for HardwareIdParseError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::BadFormat => f.write_str("expected format XXXXXXXXXXXXXXXX"),
            Self::NotHex => f.write_str("contains non-hex characters"),
        }
    }
}

impl core::error::Error for HardwareIdParseError {}

impl core::str::FromStr for HardwareId {
    type Err = HardwareIdParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.len() != 16 {
            return Err(HardwareIdParseError::BadFormat);
        }

        if !s.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(HardwareIdParseError::NotHex);
        }

        let hardware_id = u64::from_str_radix(s, 16).map_err(|_| HardwareIdParseError::NotHex)?;

        Ok(Self(hardware_id))
    }
}

#[cfg(feature = "serde")]
impl serde::Serialize for HardwareId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut buf = [0u8; 16];

        serializer.serialize_str(self.encode_hex(&mut buf))
    }
}

#[cfg(feature = "serde")]
impl<'de> serde::Deserialize<'de> for HardwareId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s: &str = serde::Deserialize::deserialize(deserializer)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}
