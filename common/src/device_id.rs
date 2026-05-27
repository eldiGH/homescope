#[cfg(feature = "wire")]
use bytemuck::{Pod, Zeroable};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "wire", repr(transparent))]
#[cfg_attr(feature = "wire", derive(Pod, Zeroable))]
pub struct DeviceId(pub u64);

impl core::fmt::Display for DeviceId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{:08X}-{:08X}", (self.0 >> 32) as u32, self.0 as u32)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceIdParseError {
    BadFormat,
    NotHex,
}

impl core::fmt::Display for DeviceIdParseError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::BadFormat => f.write_str("expected format XXXXXXXX-XXXXXXXX"),
            Self::NotHex => f.write_str("contains non-hex characters"),
        }
    }
}

impl core::error::Error for DeviceIdParseError {}

impl core::str::FromStr for DeviceId {
    type Err = DeviceIdParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (h, l) = s.split_once('-').ok_or(DeviceIdParseError::BadFormat)?;
        if h.len() != 8 || l.len() != 8 {
            return Err(DeviceIdParseError::BadFormat);
        }

        let high = u32::from_str_radix(h, 16).map_err(|_| DeviceIdParseError::NotHex)?;
        let low = u32::from_str_radix(l, 16).map_err(|_| DeviceIdParseError::NotHex)?;

        Ok(Self((u64::from(high) << 32) | u64::from(low)))
    }
}

#[cfg(feature = "serde")]
impl serde::Serialize for DeviceId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use core::fmt::Write;
        let mut buf: heapless::String<17> = heapless::String::new();

        write!(buf, "{}", self).map_err(serde::ser::Error::custom)?;

        serializer.serialize_str(&buf)
    }
}

#[cfg(feature = "serde")]
impl<'de> serde::Deserialize<'de> for DeviceId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s: &str = serde::Deserialize::deserialize(deserializer)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}
