use bt_hci::cmd::le::*;
use bt_hci::controller::ControllerCmdSync;
use defmt::{info, unwrap};
use embassy_futures::join::join;
use embassy_nrf::gpio::Output;
use embassy_time::{Duration, Timer};
use homescope_common::{device_id::DeviceId, packet::SensorPacket};
use trouble_host::prelude::*;

use crate::sensors::ReadingsSignal;

const BURST_DURATION: Duration = Duration::from_millis(400);

pub async fn run<C>(
    controller: C,
    led_pin: &mut Output<'_>,
    readings_signal: &'static ReadingsSignal,
) where
    C: Controller
        + for<'t> ControllerCmdSync<LeSetExtAdvData<'t>>
        + ControllerCmdSync<LeClearAdvSets>
        + ControllerCmdSync<LeSetExtAdvParams>
        + ControllerCmdSync<LeSetAdvSetRandomAddr>
        + ControllerCmdSync<LeReadNumberOfSupportedAdvSets>
        + for<'t> ControllerCmdSync<LeSetExtAdvEnable<'t>>
        + for<'t> ControllerCmdSync<LeSetExtScanResponseData<'t>>,
{
    let address: Address = Address::random([0xff, 0x8f, 0x1a, 0x05, 0xe4, 0xff]);
    info!("Our address = {:?}", address);

    let mut resources: HostResources<_, DefaultPacketPool, 0, 0> = HostResources::new();
    let stack = trouble_host::new(controller, &mut resources)
        .set_random_address(address)
        .build();
    let mut runner = stack.runner();
    let mut peripheral = stack.peripheral();

    let mut adv_data = [0; 31];

    info!("Starting advertising");

    let mut seq = 0;

    let device_id = {
        let high = u64::from(embassy_nrf::pac::FICR.deviceid(1).read());
        let low = u64::from(embassy_nrf::pac::FICR.deviceid(0).read());

        DeviceId(high << 32 | low)
    };

    let _ = join(runner.run(), async {
        loop {
            let readings = readings_signal.wait().await;
            led_pin.set_low();

            {
                let payload = SensorPacket {
                    seq,
                    battery_mv: 100,
                    device_id,
                    rh_cpercent: readings.rh_cpercent,
                    temp_cdegc: readings.temp_cdegc,
                };
                seq += 1;

                let len = unwrap!(AdStructure::encode_slice(
                    &[AdStructure::ManufacturerSpecificData {
                        company_identifier: 0xFFFF,
                        payload: payload.as_bytes(),
                    }],
                    &mut adv_data[..],
                ));
                let params = AdvertisementParameters {
                    interval_max: Duration::from_millis(20),
                    interval_min: Duration::from_millis(20),
                    ..Default::default()
                };

                let sets = [AdvertisementSet {
                    params,
                    data: Advertisement::ExtNonconnectableNonscannableUndirected {
                        anonymous: false,
                        adv_data: &adv_data[..len],
                    },
                    address: None,
                }];

                let mut handles = AdvertisementSet::handles(&sets);

                let _advertiser = unwrap!(peripheral.advertise_ext(&sets, &mut handles).await);

                Timer::after(BURST_DURATION).await;
            }

            led_pin.set_high();
        }
    })
    .await;
}
