#![no_std]
#![no_main]

use crate::scanned_packet::ScannedPacketChannel;
use defmt::{error, info, unwrap};
use embassy_executor::Spawner;
use embassy_futures::join::join3;
use embassy_futures::select::{Either, select};
use embassy_nrf::gpio::Output;
use embassy_nrf::mode::Async;
use embassy_nrf::peripherals::{self, RNG};
use embassy_nrf::usb::vbus_detect::HardwareVbusDetect;
use embassy_nrf::usb::{self, Driver};
use embassy_nrf::{bind_interrupts, rng};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_time::{Duration, Instant, Timer};
use embassy_usb::class::cdc_acm::{CdcAcmClass, ControlChanged, State};
use embassy_usb::driver::EndpointError;
use embassy_usb::{Builder, Config};
use homescope_board::board;
use homescope_common::frame;
use homescope_common::observation::SensorObservation;
use nrf_sdc::mpsl::MultiprotocolServiceLayer;
use nrf_sdc::{self as sdc, mpsl};
use static_cell::StaticCell;

use {defmt_rtt as _, panic_probe as _};

mod ble_scan;
mod lru_cache;
mod scanned_packet;

enum BlinkEvent {
    Inc,
    Dec,
}

static BLINK_EVENTS: Channel<CriticalSectionRawMutex, BlinkEvent, 8> = Channel::new();

const DTR_SETTLE_MS: u64 = 50;

bind_interrupts!(struct Irqs {
    RNG => rng::InterruptHandler<RNG>;
    EGU0_SWI0 => nrf_sdc::mpsl::LowPrioInterruptHandler;
    CLOCK_POWER => nrf_sdc::mpsl::ClockInterruptHandler, usb::vbus_detect::InterruptHandler;
    RADIO => nrf_sdc::mpsl::HighPrioInterruptHandler;
    TIMER0 => nrf_sdc::mpsl::HighPrioInterruptHandler;
    RTC0 => nrf_sdc::mpsl::HighPrioInterruptHandler;
    USBD => usb::InterruptHandler<peripherals::USBD>;
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
        .support_ext_scan()
        .support_ext_central()
        .support_le_coded_phy()
        .central_count(1)?
        .build(p, rng, mpsl, mem)
}

fn led_on() {
    let _ = BLINK_EVENTS.try_send(BlinkEvent::Inc);
}

fn led_off() {
    let _ = BLINK_EVENTS.try_send(BlinkEvent::Dec);
}

async fn wait_for_dtr_off(cdc_control: &ControlChanged<'_>) {
    while cdc_control.dtr() {
        cdc_control.control_changed().await
    }
}

async fn wait_for_dtr_on(cdc_control: &ControlChanged<'_>) {
    while !cdc_control.dtr() {
        cdc_control.control_changed().await
    }
}

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    #[cfg(feature = "wait-for-rtt")]
    while !defmt_rtt::in_blocking_mode() {}

    let p = embassy_nrf::init(Default::default());

    let board = board!(p);

    let mut led = embassy_nrf::gpio::Output::new(
        board.led,
        embassy_nrf::gpio::Level::High,
        embassy_nrf::gpio::OutputDrive::Standard,
    );

    led.set_low();
    Timer::after_millis(100).await;
    led.set_high();

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

    let mut sdc_mem = sdc::Mem::<2648>::new();

    let sdc = unwrap!(build_sdc(sdc_p, &mut rng, mpsl, &mut sdc_mem));

    // CDC
    // Create the driver, from the HAL.
    let driver = Driver::new(p.USBD, Irqs, HardwareVbusDetect::new(Irqs));

    let mut serial_buf = [0u8; 12];

    let device_addr = homescope_board::chip::device_addr();

    // Create embassy-usb Config
    let mut config = Config::new(0xc0de, 0xcafe);
    config.manufacturer = Some("Homescope");
    config.product = Some("Homescope Receiver");
    config.serial_number = Some(device_addr.encode_hex(&mut serial_buf));
    config.max_power = 100;
    config.max_packet_size_0 = 64;

    // Create embassy-usb DeviceBuilder using the driver and config.
    // It needs some buffers for building the descriptors.
    let mut config_descriptor = [0; 256];
    let mut bos_descriptor = [0; 256];
    let mut msos_descriptor = [0; 256];
    let mut control_buf = [0; 64];

    let mut state = State::new();

    let mut builder = Builder::new(
        driver,
        config,
        &mut config_descriptor,
        &mut bos_descriptor,
        &mut msos_descriptor,
        &mut control_buf,
    );

    // Create classes on the builder.
    let (mut cdc_sender, _, cdc_control) =
        CdcAcmClass::new(&mut builder, &mut state, 64).split_with_control();

    // Build the builder.
    let mut usb = builder.build();

    // Run the USB device.
    let usb_fut = usb.run();

    let observations_channel: ScannedPacketChannel = Channel::new();

    let ble_fut = ble_scan::run(sdc, &observations_channel, device_addr);

    let usb_writer_fut = async {
        loop {
            info!("waiting for cdc connection...");
            cdc_sender.wait_connection().await;
            info!("cdc connection established. waiting for dtr...");
            wait_for_dtr_on(&cdc_control).await;
            info!("dtr is on!");
            Timer::after_millis(DTR_SETTLE_MS).await;

            loop {
                info!("waiting for sensor packet...");
                let scanned_packet = observations_channel.receive().await;
                info!("packet received");

                led_on();

                if !cdc_sender.dtr() {
                    error!("dtr is off, retrying!");
                    led_off();
                    break;
                }

                let age_ms = u32::try_from((scanned_packet.captured_at.elapsed()).as_millis())
                    .unwrap_or(u32::MAX);

                let observation = SensorObservation {
                    device_addr: scanned_packet.device_addr,
                    rssi: scanned_packet.rssi,
                    age_ms,
                    packet: &scanned_packet.packet,
                };

                let mut frame_buffer: [u8; frame::MAX_LEN] = [0; _];
                let frame_len = match frame::encode(&mut frame_buffer, &observation) {
                    Ok(len) => len,
                    Err(err) => {
                        defmt::error!("failed to encode frame: {}", err);
                        continue;
                    }
                };

                let write_frame = async {
                    for chunk in
                        frame_buffer[..frame_len].chunks(cdc_sender.max_packet_size() as usize)
                    {
                        cdc_sender.write_packet(chunk).await?;
                    }

                    if frame_len % cdc_sender.max_packet_size() as usize == 0 {
                        cdc_sender.write_packet(&[]).await?;
                    }

                    Ok::<(), EndpointError>(())
                };

                let result = select(write_frame, wait_for_dtr_off(&cdc_control)).await;

                led_off();

                match result {
                    Either::First(Ok(_)) => {}

                    Either::First(Err(err)) => {
                        error!("write error! {}", err);
                        break;
                    }

                    Either::Second(_) => {
                        error!("dtr was set to off during cdc write!");
                        break;
                    }
                }
            }
        }
    };

    spawner.spawn(unwrap!(blinker_task(led, &BLINK_EVENTS)));

    // Run everything concurrently.
    // If we had made everything `'static` above instead, we could do this using separate tasks instead.
    join3(usb_fut, usb_writer_fut, ble_fut).await;
}

const MIN_BLINK_TIME_MS: u64 = 15;

#[embassy_executor::task]
async fn blinker_task(
    mut led: Output<'static>,
    blink_channel: &'static Channel<CriticalSectionRawMutex, BlinkEvent, 8>,
) {
    let mut on_time = Instant::now();
    let mut ref_count: u32 = 0;

    loop {
        let event = blink_channel.receive().await;

        match event {
            BlinkEvent::Inc => {
                ref_count = ref_count.saturating_add(1);
                if ref_count == 1 && led.is_set_high() {
                    led.set_low();
                    on_time = Instant::now();
                }
            }

            BlinkEvent::Dec => {
                ref_count = ref_count.saturating_sub(1);

                if ref_count == 0 && led.is_set_low() {
                    let elapsed = on_time.elapsed();
                    let duration = Duration::from_millis(MIN_BLINK_TIME_MS);

                    if elapsed < duration {
                        select(blink_channel.ready_to_receive(), async {
                            Timer::after(duration - elapsed).await;
                            led.set_high();
                        })
                        .await;
                    } else {
                        led.set_high();
                    }
                }
            }
        }
    }
}
