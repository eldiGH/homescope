use homescope_api_types::devices::{DeviceKeyStatus, DeviceSummary};

use crate::devices::{
    keys::{KekRing, KeyFault, open_key_column},
    store::DeviceActivity,
};

pub fn classify(activity: DeviceActivity, kek_ring: &KekRing) -> DeviceSummary {
    let DeviceActivity { record, last_seen } = activity;

    let key_status = match open_key_column(record.key, record.device_addr, kek_ring) {
        Ok(_) => DeviceKeyStatus::Ok,
        Err(err) => err.into(),
    };

    DeviceSummary {
        device_addr: record.device_addr,
        name: record.name,
        key_status,
        key_valid_from: record.key_valid_from,
        last_seen,
    }
}

impl From<KeyFault> for DeviceKeyStatus {
    fn from(value: KeyFault) -> Self {
        match value {
            KeyFault::Missing => DeviceKeyStatus::Missing,
            KeyFault::Invalid(_) => DeviceKeyStatus::Invalid,
            KeyFault::KekUnavailable(_) => DeviceKeyStatus::KekUnavailable,
            KeyFault::Unopenable(_) => DeviceKeyStatus::Unopenable,
        }
    }
}

#[cfg(test)]
mod test {
    use chrono::{DateTime, Utc};
    use homescope_common::{device_addr::DeviceAddr, device_key::DeviceKey};

    use super::*;
    use crate::devices::{keys::SealedDeviceKey, store::DeviceRecord};

    const ADDR: DeviceAddr = DeviceAddr([0x01, 0x02, 0x03, 0x04, 0x05, 0x06]);
    const OTHER_ADDR: DeviceAddr = DeviceAddr([0x01, 0x02, 0x03, 0x04, 0x05, 0x07]);

    const DEK: DeviceKey = DeviceKey::from_bytes([0x5A; DeviceKey::SIZE]);

    /// The byte a `kek_ver` lives at. Pinned by `keys::test::known_answer`,
    /// which asserts the whole stored layout — this test only needs to reach
    /// it, not to define it.
    const KEK_VER_OFFSET: usize = 1;

    fn sealed_column(addr: DeviceAddr) -> Vec<u8> {
        SealedDeviceKey::seal(&KekRing::for_test(), &DEK, addr)
            .as_bytes()
            .to_vec()
    }

    fn key_valid_from() -> DateTime<Utc> {
        DateTime::from_timestamp(1_753_000_000, 0).expect("valid timestamp")
    }

    fn activity(key: Option<Vec<u8>>, last_seen: Option<DateTime<Utc>>) -> DeviceActivity {
        DeviceActivity {
            record: DeviceRecord {
                id: 1,
                device_addr: ADDR,
                name: "kitchen".into(),
                key,
                key_valid_from: key_valid_from(),
            },
            last_seen,
        }
    }

    fn status_of(key: Option<Vec<u8>>) -> DeviceKeyStatus {
        classify(activity(key, None), &KekRing::for_test()).key_status
    }

    /// The four faults and the success, each mapped to the state a client
    /// sees. `keys::test` covers *why* each fault is raised; this covers that
    /// none of them are conflated on the way to the wire — which is the whole
    /// value of the endpoint, since the four have four different remedies.
    #[test]
    fn classify_walks_the_key_ladder() {
        assert!(matches!(
            status_of(Some(sealed_column(ADDR))),
            DeviceKeyStatus::Ok
        ));

        assert!(matches!(status_of(None), DeviceKeyStatus::Missing));

        assert!(matches!(
            status_of(Some(vec![0u8; 10])),
            DeviceKeyStatus::Invalid
        ));

        let mut orphaned = sealed_column(ADDR);
        orphaned[KEK_VER_OFFSET] = 0xFF; // a generation that will never be loaded
        assert!(matches!(
            status_of(Some(orphaned)),
            DeviceKeyStatus::KekUnavailable
        ));

        // Sealed against a different device, so the AAD — and the tag — no
        // longer match this row.
        assert!(matches!(
            status_of(Some(sealed_column(OTHER_ADDR))),
            DeviceKeyStatus::Unopenable
        ));
    }

    /// A summary is built even when the key is unusable — that is the point of
    /// the type. A row whose key is missing must still render its name, or the
    /// endpoint cannot tell you *which* device needs provisioning.
    ///
    /// Every field is checked, not just the identifying ones: the JSON golden
    /// that used to catch a mis-plumbed column moved to `api-types` along with
    /// the type, and `api-types` cannot see `classify`. So the activity →
    /// summary copy is pinned here or nowhere.
    #[test]
    fn a_broken_key_still_yields_a_summary() {
        let seen = DateTime::from_timestamp(1_753_003_600, 0).expect("valid timestamp");

        let summary = classify(activity(None, Some(seen)), &KekRing::for_test());

        assert_eq!(summary.device_addr, ADDR);
        assert_eq!(summary.name, "kitchen");
        assert_eq!(summary.key_valid_from, key_valid_from());
        assert!(matches!(summary.key_status, DeviceKeyStatus::Missing));
        assert_eq!(summary.last_seen, Some(seen));
    }

    /// `last_seen` and the key status are independent facts: a key that opens
    /// says nothing about whether the device has used it yet. A freshly
    /// provisioned board is exactly this — `OK`, and never heard from — and it
    /// is the state `verify` waits to see change.
    #[test]
    fn a_working_key_can_still_have_never_reported() {
        let summary = classify(
            activity(Some(sealed_column(ADDR)), None),
            &KekRing::for_test(),
        );

        assert!(matches!(summary.key_status, DeviceKeyStatus::Ok));
        assert_eq!(summary.last_seen, None);
    }
}
