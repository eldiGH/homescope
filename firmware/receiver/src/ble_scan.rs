use bt_hci::cmd::le::LeSetScanParams;
use bt_hci::controller::ControllerCmdSync;
use defmt::info;
use embassy_futures::join::join;
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, channel::Channel};
use embassy_time::Duration;
use homescope_common::{observation::SensorObservation, packet::SensorPacket};
use trouble_host::prelude::*;

/// Max number of connections
const CONNECTIONS_MAX: usize = 1;
const L2CAP_CHANNELS_MAX: usize = 1;

pub async fn run<C, const N: usize>(
    controller: C,
    channel: &'_ Channel<NoopRawMutex, SensorObservation, N>,
) where
    C: Controller + ControllerCmdSync<LeSetScanParams>,
{
    // Using a fixed "random" address can be useful for testing. In real scenarios, one would
    // use e.g. the MAC 6 byte array as the address (how to get that varies by the platform).
    let address: Address = Address::random([0xff, 0x8f, 0x1b, 0x06, 0xe4, 0xff]);

    info!("Our address = {:?}", address);

    let mut resources: HostResources<_, DefaultPacketPool, CONNECTIONS_MAX, L2CAP_CHANNELS_MAX> =
        HostResources::new();
    let stack = trouble_host::new(controller, &mut resources)
        .set_random_address(address)
        .build();
    let central = stack.central();
    let mut runner = stack.runner();

    let packet_handler = PacketHandler::<N> { channel };

    let mut scanner = Scanner::new(central);
    let _ = join(runner.run_with_handler(&packet_handler), async {
        let config = ScanConfig {
            active: false,
            phys: PhySet::Coded,
            interval: Duration::from_secs(1),
            window: Duration::from_secs(1),
            ..Default::default()
        };
        let mut _session = scanner.scan(&config).await.unwrap();
        // Scan forever

        core::future::pending::<()>().await
    })
    .await;
}

struct PacketHandler<'a, const N: usize> {
    channel: &'a Channel<NoopRawMutex, SensorObservation, N>,
}

impl<'a, const N: usize> EventHandler for PacketHandler<'a, N> {
    fn on_adv_reports(&self, reports: bt_hci::param::LeAdvReportsIter) {
        for report in reports {
            let Ok(report) = report else {
                continue;
            };

            if report.addr != BdAddr([0xff, 0x8f, 0x1a, 0x05, 0xe4, 0xff]) {
                continue;
            }

            for ad in AdStructure::decode(report.data) {
                if let Ok(AdStructure::ManufacturerSpecificData {
                    company_identifier: 0xFFFF,
                    payload,
                }) = ad
                    && payload.len() == size_of::<SensorPacket>()
                {
                    let packet = SensorPacket::from_bytes(payload);

                    let observation = SensorObservation {
                        battery_mv: packet.battery_mv,
                        device_id: packet.device_id,
                        humidity: packet.humidity,
                        pressure_pa: packet.pressure_pa,
                        seq: packet.seq,
                        temp_cdegc: packet.temp_cdegc,

                        rssi: report.rssi,
                    };

                    if self.channel.is_full() {
                        let _ = self.channel.try_receive();
                    }
                    let _ = self.channel.try_send(observation);
                }
            }
        }
    }
}
