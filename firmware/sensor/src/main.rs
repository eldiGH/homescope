#![no_std]
#![no_main]

use crate::battery::Battery;
use crate::packet_builder::{PacketBuilder, PacketSignal};
use crate::sensors::Sensors;
use crate::seq_counter::SeqCounter;
use defmt::unwrap;
use embassy_executor::Spawner;
use embassy_nrf::interrupt::InterruptExt;
use embassy_nrf::mode::Async;
use embassy_nrf::peripherals::{self, RNG};
use embassy_nrf::{bind_interrupts, rng, twim};
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Timer};
use homescope_board::board;
use nrf_sdc::mpsl::MultiprotocolServiceLayer;
use nrf_sdc::{self as sdc, mpsl};
use static_cell::StaticCell;

use {defmt_rtt as _, panic_probe as _};

mod battery;
mod ble_advertise;
mod packet_builder;
mod sensors;
mod seq_counter;

bind_interrupts!(struct Irqs {
    RNG => rng::InterruptHandler<RNG>;
    EGU0_SWI0 => nrf_sdc::mpsl::LowPrioInterruptHandler;
    CLOCK_POWER => nrf_sdc::mpsl::ClockInterruptHandler;
    RADIO => nrf_sdc::mpsl::HighPrioInterruptHandler;
    TIMER0 => nrf_sdc::mpsl::HighPrioInterruptHandler;
    RTC0 => nrf_sdc::mpsl::HighPrioInterruptHandler;
    TWISPI0 => twim::InterruptHandler<peripherals::TWISPI0>;
    SAADC => embassy_nrf::saadc::InterruptHandler;
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

const CADENCE_DURATION: Duration = Duration::from_secs(60);
static PACKET_SIGNAL: PacketSignal = Signal::new();

#[embassy_executor::task]
async fn telemetry_task(
    mut sensors: Sensors,
    mut battery: Battery,
    packet_signal: &'static PacketSignal,
    mut packet_builder: PacketBuilder,
) -> ! {
    loop {
        match sensors.read().await {
            Ok(readings) => {
                // TODO(bitmap) - we could send battery_mv when bitmap will be added, even if
                // sensors.read fail
                let battery_mv = battery.read_mv().await;

                match packet_builder.build(readings, battery_mv).await {
                    Ok(packet) => packet_signal.signal(packet),
                    Err(error) => {
                        defmt::error!("seq reservation failed, skipping packet: {}", error)
                    }
                }
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
    #[cfg(feature = "wait-for-rtt")]
    while !defmt_rtt::in_blocking_mode() {}

    let mut config = embassy_nrf::config::Config::default();
    config.time_interrupt_priority = embassy_nrf::interrupt::Priority::P2;

    let p = embassy_nrf::init(config);

    embassy_nrf::interrupt::RNG.set_priority(embassy_nrf::interrupt::Priority::P2);
    embassy_nrf::interrupt::TWISPI0.set_priority(embassy_nrf::interrupt::Priority::P2);
    embassy_nrf::interrupt::SAADC.set_priority(embassy_nrf::interrupt::Priority::P2);

    let board = board!(p);

    let mut led = embassy_nrf::gpio::Output::new(
        board.led,
        embassy_nrf::gpio::Level::High,
        embassy_nrf::gpio::OutputDrive::Standard,
    );

    let sensors_power_pin = board.sensors_power.map(|pin| {
        embassy_nrf::gpio::Output::new(
            pin,
            embassy_nrf::gpio::Level::High,
            embassy_nrf::gpio::OutputDrive::HighDrive,
        )
    });

    let twim = twim::Twim::new(
        p.TWISPI0,
        Irqs,
        board.i2c_sda,
        board.i2c_scl,
        twim::Config::default(),
        &mut [],
    );

    let sensors = Sensors::new(twim, sensors_power_pin);

    let channel = embassy_nrf::saadc::ChannelConfig::single_ended(board.battery_adc);
    let battery = Battery::init(
        embassy_nrf::saadc::Saadc::new(
            p.SAADC,
            Irqs,
            embassy_nrf::saadc::Config::default(),
            [channel],
        ),
        board.battery_divider_ratio,
    )
    .await;

    let mpsl_p =
        mpsl::Peripherals::new(p.RTC0, p.TIMER0, p.TEMP, p.PPI_CH19, p.PPI_CH30, p.PPI_CH31);

    let lfclk_cfg = mpsl::raw::mpsl_clock_lfclk_cfg_t {
        source: mpsl::raw::MPSL_CLOCK_LF_SRC_RC as u8,
        rc_ctiv: mpsl::raw::MPSL_RECOMMENDED_RC_CTIV as u8,
        rc_temp_ctiv: mpsl::raw::MPSL_RECOMMENDED_RC_TEMP_CTIV as u8,
        accuracy_ppm: mpsl::raw::MPSL_DEFAULT_CLOCK_ACCURACY_PPM as u16,
        skip_wait_lfclk_started: mpsl::raw::MPSL_DEFAULT_SKIP_WAIT_LFCLK_STARTED != 0,
    };

    static SESSION_MEM: StaticCell<mpsl::SessionMem<1>> = StaticCell::new();
    let session_mem = SESSION_MEM.init(mpsl::SessionMem::new());

    static MPSL: StaticCell<MultiprotocolServiceLayer> = StaticCell::new();
    let mpsl = MPSL.init(unwrap!(mpsl::MultiprotocolServiceLayer::with_timeslots(
        mpsl_p, Irqs, lfclk_cfg, session_mem
    )));

    spawner.spawn(unwrap!(mpsl_task(&*mpsl)));

    let sdc_p = sdc::Peripherals::new(
        p.PPI_CH17, p.PPI_CH18, p.PPI_CH20, p.PPI_CH21, p.PPI_CH22, p.PPI_CH23, p.PPI_CH24,
        p.PPI_CH25, p.PPI_CH26, p.PPI_CH27, p.PPI_CH28, p.PPI_CH29,
    );

    let mut rng = rng::Rng::new(p.RNG, Irqs);

    let mut sdc_mem = sdc::Mem::<1120>::new();
    let sdc_flash = sdc::mpsl::Flash::take(mpsl, p.NVMC);

    let seq_counter = unwrap!(SeqCounter::load(sdc_flash).await);
    let packet_builder = PacketBuilder::new(seq_counter);

    spawner.spawn(unwrap!(telemetry_task(
        sensors,
        battery,
        &PACKET_SIGNAL,
        packet_builder
    )));
    let sdc = unwrap!(build_sdc(sdc_p, &mut rng, mpsl, &mut sdc_mem));

    ble_advertise::run(
        sdc,
        &mut led,
        &PACKET_SIGNAL,
        homescope_board::device_addr(),
    )
    .await;
}
