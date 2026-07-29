//! Protocol v1 body: a bare TV section.
//!
//! ```text
//! | id: u8 | value: N | id: u8 | value: N | …
//! ```
//!
//! `id` is a `MeasurementId`; the value's width and meaning are implied by it,
//! so there is no per-field length byte and an unknown id makes everything
//! after it unparseable. Decoding folds the section into [`Metrics`], which is
//! version-independent — that is what lets a later version change this layout
//! without disturbing anything downstream.

use crate::{
    measurement::Measurement,
    packet::{DecodeError, Measurements, Metrics},
};

pub const VERSION: u8 = 1;

fn set<T>(
    slot: &mut Option<T>,
    val: T,
    wrap: impl FnOnce(T) -> Measurement,
) -> Result<(), DecodeError> {
    if slot.is_some() {
        return Err(DecodeError::Duplicate(wrap(val)));
    }

    *slot = Some(val);

    Ok(())
}

pub fn decode(body: &[u8]) -> Result<Metrics, DecodeError> {
    let measurements = Measurements { rest: body };

    let mut battery = None;
    let mut temperature = None;
    let mut relative_humidity = None;

    let mut has_measurements = false;

    for measurement in measurements {
        match measurement? {
            Measurement::Battery(bat) => set(&mut battery, bat, Measurement::Battery),
            Measurement::Temperature(temp) => set(&mut temperature, temp, Measurement::Temperature),
            Measurement::Humidity(rh) => set(&mut relative_humidity, rh, Measurement::Humidity),
        }?;

        has_measurements = true;
    }

    if !has_measurements {
        return Err(DecodeError::Empty);
    }

    Ok(Metrics {
        battery,
        temperature,
        relative_humidity,
    })
}
