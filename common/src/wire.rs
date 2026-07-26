use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[error("output buffer too small")]
pub struct BufferTooSmall;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[error("input too short")]
pub struct Truncated;

/// A value with a fixed-size little-endian wire representation.
pub trait Wire: Sized {
    /// Encoded size on the wire, in bytes.
    const SIZE: usize;

    /// The underlying primitive integer type
    type Repr;

    /// Decodes from the front of `bytes`; trailing bytes are ignored.
    fn decode(bytes: &[u8]) -> Result<Self, Truncated>;

    /// Writes `Self::SIZE` bytes to the front of `out`, returning the count.
    fn encode(&self, out: &mut [u8]) -> Result<usize, BufferTooSmall>;
}

/// Defines a unit newtype together with its `Wire` impl,
/// so the inner integer type is written exactly once.
macro_rules! wire_units {
    ( $( $name:ident($inner:ty) ),* $(,)? ) => { $(
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        #[cfg_attr(feature = "serde", derive(::serde::Serialize, ::serde::Deserialize))]
        pub struct $name(pub $inner);

        impl Wire for $name {
            const SIZE: usize = core::mem::size_of::<$inner>();
            type Repr = $inner;

            fn decode(bytes: &[u8]) -> Result<Self, Truncated> {
                let (&chunk, _rest) = bytes
                    .split_first_chunk()
                    .ok_or(Truncated)?;
                Ok(Self(<$inner>::from_le_bytes(chunk)))
            }

            fn encode(&self, out: &mut [u8]) -> Result<usize, BufferTooSmall> {
                let out = out.get_mut(..Self::SIZE).ok_or(BufferTooSmall)?;
                out.copy_from_slice(&self.0.to_le_bytes());
                Ok(Self::SIZE)
            }
        }
    )* };
}

wire_units! {
    Millivolts(u16),
    CentiCelsius(i16),
    CentiPercent(u16),
    Dbm(i8),
}

#[cfg(feature = "defmt")]
impl defmt::Format for Millivolts {
    fn format(&self, f: defmt::Formatter) {
        defmt::write!(f, "{}mV", self.0)
    }
}

#[cfg(feature = "defmt")]
impl defmt::Format for CentiCelsius {
    fn format(&self, f: defmt::Formatter) {
        if self.0 < 0 {
            defmt::write!(f, "-");
        }

        defmt::write!(
            f,
            "{}.{=u8:02}°C",
            (self.0 / 100).abs(),
            (self.0 % 100).unsigned_abs() as u8
        )
    }
}

#[cfg(feature = "defmt")]
impl defmt::Format for CentiPercent {
    fn format(&self, f: defmt::Formatter) {
        defmt::write!(f, "{}.{=u8:02}%", self.0 / 100, (self.0 % 100) as u8)
    }
}

#[cfg(feature = "defmt")]
impl defmt::Format for Dbm {
    fn format(&self, f: defmt::Formatter) {
        defmt::write!(f, "{}dBm", self.0)
    }
}
