use thiserror::Error;

use crate::wire::{BufferTooSmall, CentiCelsius, CentiPercent, Millivolts, Truncated, Wire};

#[derive(Debug, Copy, Clone, PartialEq, Eq, Error)]
#[error("unknown MeasurementId: {0:#04x}")]
pub struct MeasurementIdUnknownError(pub u8);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum DecodeError {
    #[error(transparent)]
    Truncated(#[from] Truncated),
    #[error(transparent)]
    UnknownId(#[from] MeasurementIdUnknownError),
}

/// The id => type table. Maps wire ids to payload types;
/// how each type encodes/decodes is the type's own business (`Wire`).
macro_rules! measurements {
    ( $( $variant:ident = $id:literal => $ty:path as $ctor:ident ),* $(,)? ) => {
        #[repr(u8)]
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        #[cfg_attr(feature = "defmt", derive(defmt::Format))]
        enum MeasurementId { $( $variant = $id, )* }

        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        #[cfg_attr(feature = "defmt", derive(defmt::Format))]
        pub enum Measurement { $( $variant($ty), )* }

        impl TryFrom<u8> for MeasurementId {
            type Error = MeasurementIdUnknownError;

            fn try_from(value: u8) -> Result<Self, Self::Error> {
                match value {
                    $( $id => Ok(Self::$variant), )*
                    other => Err(MeasurementIdUnknownError(other)),
                }
            }
        }

        impl Measurement {
            pub const VARIANT_COUNT: usize = [$($id),*].len();
            const ID_LEN: usize = size_of::<MeasurementId>();

            /// Largest possible encoded size across all measurement kinds.
            /// Handy for sizing firmware buffers:
            /// `let mut buf = [0u8; Measurement::MAX_ENCODED_LEN];`
            pub const MAX_ENCODED_LEN: usize = {
                let mut max = 0;
                $(
                    let len = 1 + <$ty as Wire>::SIZE;
                    if len > max { max = len; }
                )*
                max
            };

            /// Wire format: `[id: u8, value: little-endian integer]`.
            /// Trailing bytes after the value are ignored.
            pub fn decode(bytes: &[u8]) -> Result<(Self, usize), DecodeError> {
                let (&id, rest) = bytes.split_first().ok_or(Truncated)?;
                Ok(match MeasurementId::try_from(id)? {
                    $(
                        MeasurementId::$variant => {
                            (Self::$variant(<$ty as Wire>::decode(rest)?), <$ty>::SIZE + Self::ID_LEN)
                        }
                    )*
                })
            }

            /// Writes `[id, value...]` to the front of `out`,
            /// returning the number of bytes written.
            pub fn encode(&self, out: &mut [u8]) -> Result<usize, BufferTooSmall> {
                match self {
                    $(
                        Self::$variant(value) => {
                            let (id_byte, payload) =
                                out.split_first_mut().ok_or(BufferTooSmall)?;
                            *id_byte = $id;
                            Ok(Self::ID_LEN + value.encode(payload)?)
                        }
                    )*
                }
            }

            $( pub const fn $ctor(value: <$ty as Wire>::Repr) -> Self {
                Self::$variant($ty(value))
            } )*
        }
    };
}

measurements! {
    Battery     = 0x01 => Millivolts    as battery,
    Temperature = 0x02 => CentiCelsius  as temperature,
    Humidity    = 0x03 => CentiPercent  as humidity,
}

#[cfg(test)]
mod tests {
    use std::format;

    use super::*;

    #[test]
    fn round_trip() {
        let all = [
            Measurement::Battery(Millivolts(2950)),
            Measurement::Temperature(CentiCelsius(-1250)),
            Measurement::Humidity(CentiPercent(4875)),
        ];
        let mut buf = [0u8; Measurement::MAX_ENCODED_LEN];
        for m in all {
            let n = m.encode(&mut buf).unwrap();
            assert_eq!(n, 3);
            assert_eq!(Measurement::decode(&buf[..n]).unwrap(), (m, n));
        }
    }

    #[test]
    fn decode_errors() {
        assert_eq!(
            Measurement::decode(&[]),
            Err(DecodeError::Truncated(Truncated))
        );
        assert_eq!(
            Measurement::decode(&[0x01, 0x00]),
            Err(DecodeError::Truncated(Truncated))
        );
        assert_eq!(
            Measurement::decode(&[0x7F, 0x00, 0x00]),
            Err(DecodeError::UnknownId(MeasurementIdUnknownError(0x7F)))
        );
    }

    #[test]
    fn encode_checks_buffer() {
        let mut buf = [0u8; 2];
        assert_eq!(
            Measurement::Battery(Millivolts(1)).encode(&mut buf),
            Err(BufferTooSmall)
        );
        assert_eq!(Measurement::MAX_ENCODED_LEN, 3);
    }

    #[test]
    fn thiserror_display() {
        let e = DecodeError::from(MeasurementIdUnknownError(0x7F));
        assert_eq!(format!("{e}"), "unknown MeasurementId: 0x7f");
    }
}
