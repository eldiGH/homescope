use bt_hci::cmd::le::*;
use bt_hci::controller::ControllerCmdSync;
use defmt::{info, unwrap};
use embassy_futures::join::join;
use embassy_nrf::gpio::Output;
use embassy_time::{Duration, Timer};
use trouble_host::prelude::*;

use crate::packet_builder::PacketSignal;

const BURST_DURATION: Duration = Duration::from_millis(400);

pub async fn run<C>(controller: C, led_pin: &mut Output<'_>, packet_signal: &'static PacketSignal)
where
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

    // Max extended-advertising data in a single AUX_ADV_IND (no chaining).
    const ADV_DATA_MAX: usize = 254;
    let mut adv_data = [0; ADV_DATA_MAX];

    info!("Starting advertising");

    let _ = join(runner.run(), async {
        loop {
            let packet = packet_signal.wait().await;
            led_pin.set_low();

            {
                let len = unwrap!(AdStructure::encode_slice(
                    &[AdStructure::ManufacturerSpecificData {
                        company_identifier: 0xFFFF,
                        payload: packet.as_bytes(),
                    }],
                    &mut adv_data[..],
                ));

                let params = AdvertisementParameters {
                    interval_max: Duration::from_millis(20),
                    interval_min: Duration::from_millis(20),
                    primary_phy: PhyKind::LeCoded,
                    secondary_phy: PhyKind::LeCoded,
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
