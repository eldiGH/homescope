use std::{collections::HashMap, sync::RwLock};

use homescope_common::hardware_id::HardwareId;

use crate::devices::store::Device;

pub struct Cache {
    map: RwLock<HashMap<HardwareId, Device>>,
}

impl Cache {
    pub fn new(devices: Vec<Device>) -> Self {
        let cache = devices.into_iter().map(|d| (d.hardware_id, d)).collect();

        Self {
            map: RwLock::new(cache),
        }
    }

    pub fn get(&self, hardware_id: HardwareId) -> Option<Device> {
        self.map
            .read()
            .expect("device cache lock poisoned")
            .get(&hardware_id)
            .cloned()
    }
}
