use embassy_nrf::saadc::Saadc;
use homescope_common::wire::Millivolts;

pub struct Battery {
    saadc: Saadc<'static, 1>,
    battery_divider_ratio: u32,
}

impl Battery {
    pub async fn init(saadc: Saadc<'static, 1>, battery_divider_ratio: u32) -> Self {
        saadc.calibrate().await;

        Self {
            saadc,
            battery_divider_ratio,
        }
    }

    pub async fn measure(&mut self) -> Millivolts {
        let mut buf: [i16; 1] = [0; 1];

        self.saadc.sample(&mut buf).await;

        let raw = buf[0].max(0) as u32;

        // max: 4095*3600*5 ≈ 74M, fits u32; result ≤ 18000, fits u16
        let battery_mv = (raw * (3600 * self.battery_divider_ratio) / 4096) as u16;

        defmt::debug!("battery_mv: {}", battery_mv);
        Millivolts(battery_mv)
    }
}
