use core::cell::RefCell;

use bt_hci::cmd::le::{LeSetExtScanEnable, LeSetExtScanParams};
use bt_hci::controller::ControllerCmdSync;
use defmt::{info, unwrap};
use embassy_futures::join::join;
use embassy_time::{Duration, Instant};
use heapless::Vec;
use homescope_common::device_addr::DeviceAddr;
use homescope_common::packet::SensorPacket;
use homescope_common::wire::Dbm;
use trouble_host::prelude::*;

use crate::lru_cache::LruCache;
use crate::scanned_packet::{ScannedPacket, ScannedPacketChannel};

/// Max number of connections
const CONNECTIONS_MAX: usize = 1;
const L2CAP_CHANNELS_MAX: usize = 1;

pub async fn run<C>(controller: C, channel: &'_ ScannedPacketChannel, device_addr: DeviceAddr)
where
    C: Controller + ControllerCmdSync<LeSetExtScanParams> + ControllerCmdSync<LeSetExtScanEnable>,
{
    let address: Address = Address::random(device_addr.0);

    info!("Our address = {:?}", address);

    let mut resources: HostResources<_, DefaultPacketPool, CONNECTIONS_MAX, L2CAP_CHANNELS_MAX> =
        HostResources::new();
    let stack = trouble_host::new(controller, &mut resources)
        .set_random_address(address)
        .build();
    let central = stack.central();
    let mut runner = stack.runner();

    let packet_handler = PacketHandler {
        channel,
        seq_cache: RefCell::new(LruCache::new()),
    };

    let mut scanner = Scanner::new(central);
    let _ = join(runner.run_with_handler(&packet_handler), async {
        let config = ScanConfig {
            active: false,
            phys: PhySet::Coded,
            interval: Duration::from_secs(1),
            window: Duration::from_secs(1),
            ..Default::default()
        };
        let mut _session = unwrap!(scanner.scan_ext(&config).await);
        // Scan forever

        core::future::pending::<()>().await
    })
    .await;
}

struct PacketHandler<'a> {
    channel: &'a ScannedPacketChannel,
    seq_cache: RefCell<LruCache<DeviceAddr, u32, 32>>,
}

impl<'a> EventHandler for PacketHandler<'a> {
    fn on_ext_adv_reports(&self, reports: bt_hci::param::LeExtAdvReportsIter) {
        for report in reports {
            let Ok(report) = report else {
                continue;
            };

            for ad in AdStructure::decode(report.data) {
                if let Ok(AdStructure::ManufacturerSpecificData {
                    company_identifier: 0xFFFF,
                    payload,
                }) = ad
                {
                    let Ok(packet) = SensorPacket::strip_air_magic(payload) else {
                        continue;
                    };

                    let Ok(SensorPacket { seq, .. }) = SensorPacket::parse(packet) else {
                        defmt::error!("could not parse packet: {}", packet);
                        continue;
                    };
                    let device_addr = DeviceAddr(report.addr.0);

                    let mut cache = self.seq_cache.borrow_mut();

                    if cache
                        .get(&device_addr)
                        .is_some_and(|cached_seq| seq <= *cached_seq)
                    {
                        continue;
                    }

                    cache.insert(device_addr, seq);

                    let Ok(packet) = Vec::from_slice(packet) else {
                        continue;
                    };

                    let scanned_packet = ScannedPacket {
                        rssi: Dbm(report.rssi),
                        device_addr,
                        captured_at: Instant::now(),
                        packet,
                    };

                    if self.channel.is_full() {
                        let _ = self.channel.try_receive();
                    }
                    let _ = self.channel.try_send(scanned_packet);
                }
            }
        }
    }
}
