use defmt::{expect, info};
use embassy_nrf::{gpio::Output, twim::Twim};
use embassy_time::{Delay, Duration, Instant, Timer, with_timeout};
use sht4x::{Measurement, Sht4xAsync};

const SHT4X_POWER_UP_DURATION: Duration = Duration::from_millis(10);

pub struct Sensors {
    sht: Sht4xAsync<Twim<'static>, Delay>,
    power: Output<'static>,
    enabled_at: Option<Instant>,
}

impl Sensors {
    pub fn new(twim: Twim<'static>, power: Output<'static>) -> Self {
        let enabled_at = if power.is_set_high() {
            Some(Instant::now())
        } else {
            None
        };

        Self {
            sht: Sht4xAsync::new(twim),
            power,
            enabled_at,
        }
    }

    fn enable(&mut self) {
        if self.power.is_set_high() {
            return;
        }

        self.power.set_high();
        self.enabled_at = Some(Instant::now());
    }

    fn disable(&mut self) {
        if self.power.is_set_low() {
            return;
        }

        self.power.set_low();
        self.enabled_at = None;
    }

    async fn settle(&self, duration: Duration) {
        Timer::at(expect!(self.enabled_at, "sensor power pin is off") + duration).await;
    }

    pub async fn read(&mut self) -> Result<Readings, SensorError> {
        self.enable();
        let result = self.read_all().await;
        self.disable();

        result
    }

    async fn read_all(&mut self) -> Result<Readings, SensorError> {
        self.settle(SHT4X_POWER_UP_DURATION).await;
        let measurement = with_timeout(
            Duration::from_millis(100),
            self.sht.measure(sht4x::Precision::High, &mut Delay),
        )
        .await??;
        info!("measurement: {}", &measurement);

        Ok(Readings::from_measurement(&measurement))
    }
}

pub struct Readings {
    pub temp_cdegc: i16,
    pub rh_cpercent: u16,
}

impl Readings {
    pub fn from_measurement(m: &Measurement) -> Self {
        let t = m.temperature_milli_celsius();
        let t = (if t >= 0 { t + 5 } else { t - 5 }) / 10;
        let temp_cdegc = t.clamp(-4500, 13_000) as i16;

        let rh = (m.humidity_milli_percent().clamp(0, 100_000) + 5) / 10;
        let rh_cpercent = rh as u16;

        Self {
            temp_cdegc,
            rh_cpercent,
        }
    }
}

#[derive(defmt::Format)]
pub enum SensorError {
    Sht(sht4x::Error<embassy_nrf::twim::Error>),
    Timeout,
}

impl From<sht4x::Error<embassy_nrf::twim::Error>> for SensorError {
    fn from(value: sht4x::Error<embassy_nrf::twim::Error>) -> Self {
        Self::Sht(value)
    }
}

impl From<embassy_time::TimeoutError> for SensorError {
    fn from(_: embassy_time::TimeoutError) -> Self {
        Self::Timeout
    }
}
