use bme680::{I2CAddress, IIRFilterSize, OversamplingSetting, PowerMode, SettingsBuilder};
use defmt::info;
use embassy_nrf::twim::Twim;
use embassy_time::{Delay, Duration, Timer};

pub enum ReadErr {
    Dev,
    SetSensorSettings,
    SetSensorMode,
    GetProfileDuration,
    GetSensorData,
    DurationParse,
}

pub async fn read_bme(twim: Twim<'_>) -> Result<(), ReadErr> {
    let mut delayer = Delay;

    let mut dev =
        bme680::Bme680::init(twim, &mut delayer, I2CAddress::Primary).map_err(|_| ReadErr::Dev)?;

    let settings = SettingsBuilder::new()
        .with_humidity_oversampling(OversamplingSetting::OS2x)
        .with_pressure_oversampling(OversamplingSetting::OS4x)
        .with_temperature_oversampling(OversamplingSetting::OS8x)
        .with_temperature_filter(IIRFilterSize::Size3)
        .with_gas_measurement(core::time::Duration::from_millis(100), 320, 25)
        .with_temperature_offset(0.0)
        .with_run_gas(true)
        .build();

    dev.set_sensor_settings(&mut delayer, settings)
        .map_err(|_| ReadErr::SetSensorSettings)?;
    let profile_duration = dev
        .get_profile_dur(&settings.0)
        .map_err(|_| ReadErr::GetProfileDuration)?;

    // Read sensor data
    loop {
        dev.set_sensor_mode(&mut delayer, PowerMode::ForcedMode)
            .map_err(|_| ReadErr::SetSensorMode)?;

        Timer::after(Duration::try_from(profile_duration).map_err(|_| ReadErr::DurationParse)?)
            .await;

        let (data, _state) = dev
            .get_sensor_data(&mut delayer)
            .map_err(|_| ReadErr::GetSensorData)?;

        info!("Temperature {}°C", data.temperature_celsius());
        info!("Pressure {}hPa", data.pressure_hpa());
        info!("Humidity {}%", data.humidity_percent());
        info!("Gas Resistence {}Ω", data.gas_resistance_ohm());

        Timer::after_secs(3).await;
    }
}
