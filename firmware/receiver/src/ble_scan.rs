use core::cell::RefCell;

use bt_hci::cmd::le::{LeSetExtScanEnable, LeSetExtScanParams};
use bt_hci::controller::ControllerCmdSync;
use defmt::{info, unwrap};
use embassy_futures::join::join;
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, channel::Channel};
use embassy_time::{Duration, Instant};
use homescope_common::device_addr::DeviceAddr;
use homescope_common::{observation::SensorObservation, packet::SensorPacket};
use trouble_host::prelude::*;

use crate::lru_cache::LruCache;

/// Max number of connections
const CONNECTIONS_MAX: usize = 1;
const L2CAP_CHANNELS_MAX: usize = 1;

pub async fn run<C, const N: usize>(
    controller: C,
    channel: &'_ Channel<NoopRawMutex, (Instant, SensorObservation), N>,
    device_addr: DeviceAddr,
) where
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

    let packet_handler = PacketHandler::<N> {
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

struct PacketHandler<'a, const N: usize> {
    channel: &'a Channel<NoopRawMutex, (Instant, SensorObservation), N>,
    seq_cache: RefCell<LruCache<DeviceAddr, u32, 32>>,
}

impl<'a, const N: usize> EventHandler for PacketHandler<'a, N> {
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
                    && payload.len() == size_of::<SensorPacket>()
                {
                    let packet = SensorPacket::from_bytes(payload);
                    let device_addr = DeviceAddr(report.addr.0);

                    let mut cache = self.seq_cache.borrow_mut();

                    if cache
                        .get(&device_addr)
                        .is_some_and(|cached_seq| packet.seq <= *cached_seq)
                    {
                        continue;
                    }

                    cache.insert(device_addr, packet.seq);

                    let observation = SensorObservation {
                        battery_mv: packet.battery_mv,
                        device_addr,
                        rh_cpercent: packet.rh_cpercent,
                        seq: packet.seq,
                        temp_cdegc: packet.temp_cdegc,

                        rssi: report.rssi,
                        age_ms: 0,
                    };

                    if self.channel.is_full() {
                        let _ = self.channel.try_receive();
                    }
                    let _ = self.channel.try_send((Instant::now(), observation));
                }
            }
        }
    }
}
