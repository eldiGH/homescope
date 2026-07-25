use defmt::warn;
use embassy_nrf::gpio::Output;
use embassy_time::{Duration, Instant, Timer};
use embedded_hal_async::i2c::I2c;
use heapless::Vec;
use homescope_common::measurement::Measurement;

mod sht4x;
pub use sht4x::Sht4x;

mod battery;
pub use battery::Battery;

const SHT4X_POWER_UP_DURATION: Duration = Duration::from_millis(10);

pub struct Sensors<I2C: I2c> {
    sht: Sht4x<I2C>,
    battery: Battery,
    power: Option<Output<'static>>,
}

impl<I2C: I2c> Sensors<I2C> {
    pub fn new(sht: Sht4x<I2C>, battery: Battery, power: Option<Output<'static>>) -> Self {
        Self {
            sht,
            battery,
            power,
        }
    }

    pub async fn measure(&mut self) -> Vec<Measurement, { Measurement::VARIANT_COUNT }>
    where
        I2C::Error: defmt::Format,
    {
        let power_rail = RailGuard::enable(&mut self.power);

        let mut measurements: Vec<Measurement, { Measurement::VARIANT_COUNT }> = Vec::new();

        let battery = self.battery.measure().await;
        defmt::unwrap!(measurements.push(Measurement::Battery(battery)));

        power_rail.settle(SHT4X_POWER_UP_DURATION).await;
        match self.sht.measure().await {
            Ok(r) => {
                defmt::unwrap!(measurements.push(Measurement::Temperature(r.temperature)));
                defmt::unwrap!(measurements.push(Measurement::Humidity(r.relative_humidity)));
            }

            Err(e) => warn!("sht read failed, dropping T/H this cycle: {}", e),
        };

        measurements
    }
}

struct RailGuard<'a> {
    pin: &'a mut Option<Output<'static>>,
    enabled_at: Instant,
}
impl<'a> RailGuard<'a> {
    fn enable(pin: &'a mut Option<Output<'static>>) -> Self {
        if let Some(pin) = pin {
            pin.set_high();
        }
        Self {
            pin,
            enabled_at: Instant::now(),
        }
    }

    async fn settle(&self, duration: Duration) {
        if self.pin.is_none() {
            return;
        }

        Timer::at(self.enabled_at + duration).await;
    }
}

impl Drop for RailGuard<'_> {
    fn drop(&mut self) {
        if let Some(pin) = self.pin {
            pin.set_low();
        }
    }
}
