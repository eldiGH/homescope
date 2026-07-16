use std::{collections::HashMap, sync::RwLock};

use homescope_common::device_addr::DeviceAddr;

use crate::devices::store::Device;

pub struct Cache {
    map: RwLock<HashMap<DeviceAddr, Device>>,
}

impl Cache {
    pub fn new(devices: Vec<Device>) -> Self {
        let cache = devices.into_iter().map(|d| (d.device_addr, d)).collect();

        Self {
            map: RwLock::new(cache),
        }
    }

    pub fn get(&self, device_addr: DeviceAddr) -> Option<Device> {
        self.map
            .read()
            .expect("device cache lock poisoned")
            .get(&device_addr)
            .cloned()
    }
}
