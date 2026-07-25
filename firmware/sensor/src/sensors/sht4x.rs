use defmt::debug;
use embassy_time::{Delay, Duration, TimeoutError, with_timeout};
use embedded_hal_async::i2c;
use homescope_common::wire::{CentiCelsius, CentiPercent};
use sht4x::Sht4xAsync as ShtDriver;

pub struct Sht4x<I2C: i2c::I2c> {
    driver: ShtDriver<I2C, Delay>,
}

impl<I2C: i2c::I2c> Sht4x<I2C> {
    pub fn new(i2c: I2C) -> Self {
        Self {
            driver: ShtDriver::new(i2c),
        }
    }

    pub async fn measure(&mut self) -> Result<Readings, Sht4xError<I2C::Error>> {
        let measurement = with_timeout(
            Duration::from_millis(100),
            self.driver.measure(sht4x::Precision::High, &mut Delay),
        )
        .await??;

        let readings: Readings = measurement.into();

        debug!("{}", readings);

        Ok(readings)
    }
}

pub struct Readings {
    pub temperature: CentiCelsius,
    pub relative_humidity: CentiPercent,
}

impl From<sht4x::Measurement> for Readings {
    fn from(value: sht4x::Measurement) -> Self {
        let t = value.temperature_milli_celsius();
        let t = (if t >= 0 { t + 5 } else { t - 5 }) / 10;
        let temp_cdegc = t.clamp(-4500, 13_000) as i16;

        let rh = (value.humidity_milli_percent().clamp(0, 100_000) + 5) / 10;
        let rh_cpercent = rh as u16;

        Self {
            temperature: CentiCelsius(temp_cdegc),
            relative_humidity: CentiPercent(rh_cpercent),
        }
    }
}

impl defmt::Format for Readings {
    fn format(&self, fmt: defmt::Formatter) {
        defmt::write!(
            fmt,
            "temperature: {} | humidity: {}",
            self.temperature,
            self.relative_humidity
        )
    }
}

#[derive(defmt::Format)]
pub enum Sht4xError<E: i2c::Error> {
    Bus(sht4x::Error<E>),
    Timeout,
}

impl<E: i2c::Error> From<sht4x::Error<E>> for Sht4xError<E> {
    fn from(value: sht4x::Error<E>) -> Self {
        Sht4xError::Bus(value)
    }
}

impl<E: i2c::Error> From<TimeoutError> for Sht4xError<E> {
    fn from(_: TimeoutError) -> Self {
        Sht4xError::Timeout
    }
}
