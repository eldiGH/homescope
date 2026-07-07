#![no_std]
#![no_main]

use crate::sensors::{ReadingsSignal, Sensors};
use defmt::unwrap;
use embassy_executor::Spawner;
use embassy_nrf::mode::Async;
use embassy_nrf::peripherals::{self, RNG};
use embassy_nrf::{bind_interrupts, rng, twim};
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Timer};
use homescope_board::{i2c_scl_pin, i2c_sda_pin};
use nrf_sdc::mpsl::MultiprotocolServiceLayer;
use nrf_sdc::{self as sdc, mpsl};
use static_cell::StaticCell;

use {defmt_rtt as _, panic_probe as _};

mod ble_advertise;
mod sensors;

bind_interrupts!(struct Irqs {
    RNG => rng::InterruptHandler<RNG>;
    EGU0_SWI0 => nrf_sdc::mpsl::LowPrioInterruptHandler;
    CLOCK_POWER => nrf_sdc::mpsl::ClockInterruptHandler;
    RADIO => nrf_sdc::mpsl::HighPrioInterruptHandler;
    TIMER0 => nrf_sdc::mpsl::HighPrioInterruptHandler;
    RTC0 => nrf_sdc::mpsl::HighPrioInterruptHandler;
    TWISPI0 => twim::InterruptHandler<peripherals::TWISPI0>;
});

#[embassy_executor::task]
async fn mpsl_task(mpsl: &'static MultiprotocolServiceLayer<'static>) -> ! {
    mpsl.run().await
}
fn build_sdc<'d, const N: usize>(
    p: nrf_sdc::Peripherals<'d>,
    rng: &'d mut rng::Rng<Async>,
    mpsl: &'d MultiprotocolServiceLayer,
    mem: &'d mut sdc::Mem<N>,
) -> Result<nrf_sdc::SoftdeviceController<'d>, nrf_sdc::Error> {
    sdc::Builder::new()?
        .support_ext_adv()
        .support_le_coded_phy()
        .default_tx_power(8)?
        .build(p, rng, mpsl, mem)
}

const CADENCE_DURATION: Duration = Duration::from_secs(1);
static READINGS_SIGNAL: ReadingsSignal = Signal::new();

#[embassy_executor::task]
async fn sensor_task(mut sensors: Sensors, readings_signal: &'static ReadingsSignal) -> ! {
    loop {
        match sensors.read().await {
            Ok(readings) => {
                readings_signal.signal(readings);
            }

            Err(error) => {
                defmt::error!("errors while reading: {}", error);
            }
        };

        Timer::after(CADENCE_DURATION).await;
    }
}

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let p = embassy_nrf::init( Default::default());

    let mut led = embassy_nrf::gpio::Output::new(
        homescope_board::led_pin!(p),
        embassy_nrf::gpio::Level::High,
        embassy_nrf::gpio::OutputDrive::Standard,
    );

    let sensors_power_pin = embassy_nrf::gpio::Output::new(
        p.P0_05,
        embassy_nrf::gpio::Level::High,
        embassy_nrf::gpio::OutputDrive::HighDrive,
    );

    let twim = twim::Twim::new(
        p.TWISPI0,
        Irqs,
        i2c_sda_pin!(p),
        i2c_scl_pin!(p),
        twim::Config::default(),
        &mut [],
    );

    let sensors = Sensors::new(twim, sensors_power_pin);
    spawner.spawn(unwrap!(sensor_task(sensors, &READINGS_SIGNAL)));

    let mpsl_p =
        mpsl::Peripherals::new(p.RTC0, p.TIMER0, p.TEMP, p.PPI_CH19, p.PPI_CH30, p.PPI_CH31);

    let lfclk_cfg = mpsl::raw::mpsl_clock_lfclk_cfg_t {
        source: mpsl::raw::MPSL_CLOCK_LF_SRC_RC as u8,
        rc_ctiv: mpsl::raw::MPSL_RECOMMENDED_RC_CTIV as u8,
        rc_temp_ctiv: mpsl::raw::MPSL_RECOMMENDED_RC_TEMP_CTIV as u8,
        accuracy_ppm: mpsl::raw::MPSL_DEFAULT_CLOCK_ACCURACY_PPM as u16,
        skip_wait_lfclk_started: mpsl::raw::MPSL_DEFAULT_SKIP_WAIT_LFCLK_STARTED != 0,
    };

    static MPSL: StaticCell<MultiprotocolServiceLayer> = StaticCell::new();
    let mpsl = MPSL.init(unwrap!(mpsl::MultiprotocolServiceLayer::new(
        mpsl_p, Irqs, lfclk_cfg
    )));

    spawner.spawn(unwrap!(mpsl_task(&*mpsl)));

    let sdc_p = sdc::Peripherals::new(
        p.PPI_CH17, p.PPI_CH18, p.PPI_CH20, p.PPI_CH21, p.PPI_CH22, p.PPI_CH23, p.PPI_CH24,
        p.PPI_CH25, p.PPI_CH26, p.PPI_CH27, p.PPI_CH28, p.PPI_CH29,
    );

    let mut rng = rng::Rng::new(p.RNG, Irqs);

    let mut sdc_mem = sdc::Mem::<1120>::new();

    let sdc = unwrap!(build_sdc(sdc_p, &mut rng, mpsl, &mut sdc_mem));

    ble_advertise::run(sdc, &mut led, &READINGS_SIGNAL).await;
}
