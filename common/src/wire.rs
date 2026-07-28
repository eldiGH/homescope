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

/// Defines a unit newtype together with its `Wire`, `Display` and — under the
/// `defmt` feature — `defmt::Format` impls, so the repr, the scale and the unit
/// suffix are each written exactly once.
///
/// `Name(repr) / scale as "suffix"`, where a scale of 1 renders the value as a
/// plain integer and 100 as two decimal places.
///
/// Normalised to i64 so one body serves both signed and unsigned units:
/// `self.0 < 0` would trip `unused_comparisons` on a u16, and `.abs()`
/// does not exist on unsigned integers at all.
macro_rules! wire_units {
    ( $( $name:ident($inner:ty) / $scale:literal as $unit:literal ),* $(,)? ) => { $(
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

        impl $name {
            pub fn as_f64(self) -> f64 { f64::from(self.0) / $scale as f64 }
        }

        const _: () = assert!($scale == 1 || $scale == 100, "defmt_unit hard-codes two fraction digits; \
        add a FormattedNumber variant before using another scale");

        impl ::core::fmt::Display for $name {
            fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                fmt_unit(f, i64::from(self.0), $scale, $unit)
            }
        }

        #[cfg(feature = "defmt")]
        impl ::defmt::Format for $name {
            fn format(&self, f: ::defmt::Formatter) {
                defmt_unit(f, i64::from(self.0), $scale, $unit)
            }
        }
    )* };
}

wire_units! {
    Millivolts(u16)     / 1     as "mV",
    CentiCelsius(i16)   / 100   as "°C",
    CentiPercent(u16)   / 100   as "%",
    Dbm(i8)             / 1     as "dBm"
}

/// The parts of a rendered number, ready to drop into a format string.
///
/// This exists because `defmt::write!` interns its format string at compile
/// time — the text never reaches the device, only an index and the raw
/// arguments — so the string must be a literal at the call site and cannot be
/// passed in or computed. Splitting the *arithmetic* out here is therefore the
/// most that `Display` and `defmt::Format` can share: each backend supplies
/// nothing but its own literal.
///
/// One variant per fraction width, so adding a scale fails to compile in both
/// backends until both are handled.
enum FormattedNumber {
    Decimal2 {
        whole: u64,
        fraction: u64,
        sign: &'static str,
    },

    Int {
        value: i64,
    },
}

/// Splits `value` into the pieces each backend's format string needs.
///
/// `scale` is only ever 1 or 100 — the `const` assert in `wire_units!`
/// rejects anything else at compile time, which is what lets the `else`
/// branch assume two fraction digits.
fn format_number(value: i64, scale: u64) -> FormattedNumber {
    if scale <= 1 {
        return FormattedNumber::Int { value };
    }

    const EMPTY: &str = "";
    const MINUS: &str = "-";

    // Take the sign before dividing: -50 centi is "-0.50", but -50 / 100 is 0,
    // which has already lost it.
    FormattedNumber::Decimal2 {
        sign: if value < 0 { MINUS } else { EMPTY },
        whole: (value / scale as i64).unsigned_abs(),
        fraction: (value % scale as i64).unsigned_abs(),
    }
}

fn fmt_unit(
    f: &mut core::fmt::Formatter<'_>,
    value: i64,
    scale: u64,
    unit: &str,
) -> core::fmt::Result {
    match format_number(value, scale) {
        FormattedNumber::Int { value } => write!(f, "{}{}", value, unit),
        FormattedNumber::Decimal2 {
            whole,
            fraction,
            sign,
        } => write!(f, "{sign}{whole}.{fraction:02}{unit}"),
    }
}

#[cfg(feature = "defmt")]
fn defmt_unit(f: defmt::Formatter, value: i64, scale: u64, unit: &str) {
    match format_number(value, scale) {
        FormattedNumber::Int { value } => defmt::write!(f, "{=i64}{=str}", value, unit),
        FormattedNumber::Decimal2 {
            whole,
            fraction,
            sign,
        } => defmt::write!(
            f,
            "{=str}{=u64}.{=u64:02}{=str}",
            sign,
            whole,
            fraction,
            unit
        ),
    }
}

#[cfg(test)]
mod test {
    use std::format;

    use super::*;

    #[test]
    fn scale_one_units_render_as_integers() {
        assert_eq!(format!("{}", Millivolts(2950)), "2950mV");
        assert_eq!(format!("{}", Millivolts(0)), "0mV");
        assert_eq!(format!("{}", Dbm(-67)), "-67dBm");
    }

    #[test]
    fn centi_units_render_two_fraction_digits() {
        assert_eq!(format!("{}", CentiCelsius(2150)), "21.50°C");
        assert_eq!(format!("{}", CentiPercent(4875)), "48.75%");
        assert_eq!(format!("{}", CentiPercent(5000)), "50.00%");
    }

    /// A fraction below ten has to be zero-padded, or 21.05 reads as 21.5 —
    /// a tenfold error that still looks like a plausible temperature.
    #[test]
    fn single_digit_fractions_are_padded() {
        assert_eq!(format!("{}", CentiCelsius(2105)), "21.05°C");
        assert_eq!(format!("{}", CentiCelsius(2100)), "21.00°C");
        assert_eq!(format!("{}", CentiPercent(4805)), "48.05%");
    }

    /// The sign is taken before the division, because -50 / 100 is 0 and no
    /// longer carries it. Values between -1 and 0 are the only ones where the
    /// whole part cannot supply the sign on its own.
    #[test]
    fn negative_centi_values_keep_their_sign() {
        assert_eq!(format!("{}", CentiCelsius(-50)), "-0.50°C");
        assert_eq!(format!("{}", CentiCelsius(-5)), "-0.05°C");
        assert_eq!(format!("{}", CentiCelsius(-1250)), "-12.50°C");
        assert_eq!(format!("{}", CentiCelsius(0)), "0.00°C");
    }

    #[test]
    fn as_f64_divides_by_the_scale() {
        assert_eq!(Millivolts(2950).as_f64(), 2950.0);
        assert_eq!(CentiCelsius(2105).as_f64(), 21.05);
        assert_eq!(CentiCelsius(-1250).as_f64(), -12.5);
        assert_eq!(CentiPercent(4875).as_f64(), 48.75);
        assert_eq!(Dbm(-67).as_f64(), -67.0);
    }

    #[test]
    fn round_trip_is_little_endian() {
        let mut buf = [0u8; 2];

        assert_eq!(Millivolts(2950).encode(&mut buf), Ok(2));
        assert_eq!(buf, [0x86, 0x0B]);
        assert_eq!(Millivolts::decode(&buf), Ok(Millivolts(2950)));

        assert_eq!(CentiCelsius(-1250).encode(&mut buf), Ok(2));
        assert_eq!(buf, [0x1E, 0xFB]);
        assert_eq!(CentiCelsius::decode(&buf), Ok(CentiCelsius(-1250)));

        let mut byte = [0u8; 1];
        assert_eq!(Dbm(-67).encode(&mut byte), Ok(1));
        assert_eq!(byte, [0xBD]);
        assert_eq!(Dbm::decode(&byte), Ok(Dbm(-67)));
    }

    /// Decoding reads from the front and ignores what follows, which is what
    /// lets `Measurement::decode` hand it the whole remaining packet.
    #[test]
    fn decode_ignores_trailing_bytes_but_rejects_short_input() {
        assert_eq!(
            Millivolts::decode(&[0x86, 0x0B, 0xFF, 0xFF]),
            Ok(Millivolts(2950))
        );
        assert_eq!(Millivolts::decode(&[0x86]), Err(Truncated));
        assert_eq!(Dbm::decode(&[]), Err(Truncated));
    }

    #[test]
    fn encode_rejects_a_buffer_one_byte_short() {
        assert_eq!(Millivolts(1).encode(&mut [0u8; 1]), Err(BufferTooSmall));
        assert_eq!(Dbm(1).encode(&mut []), Err(BufferTooSmall));
    }

    #[test]
    fn size_matches_the_repr() {
        assert_eq!(Millivolts::SIZE, 2);
        assert_eq!(CentiCelsius::SIZE, 2);
        assert_eq!(CentiPercent::SIZE, 2);
        assert_eq!(Dbm::SIZE, 1);
    }
}
