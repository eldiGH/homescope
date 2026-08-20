use chrono::{DateTime, Utc};
use homescope_common::device_addr::DeviceAddr;
use serde::Serialize;

use crate::devices::{
    keys::{KekRing, KeyFault, open_key_column},
    store::DeviceRecord,
};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceSummary {
    pub device_addr: DeviceAddr,
    pub name: String,
    pub key_status: DeviceKeyStatus,
    pub key_valid_from: DateTime<Utc>,
}

#[derive(Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum DeviceKeyStatus {
    Missing,
    Invalid,
    KekUnavailable,
    Unopenable,
    Ok,
}

impl DeviceSummary {
    pub fn classify(record: DeviceRecord, kek_ring: &KekRing) -> Self {
        let key_status = match open_key_column(record.key, record.device_addr, kek_ring) {
            Ok(_) => DeviceKeyStatus::Ok,
            Err(err) => err.into(),
        };

        Self {
            device_addr: record.device_addr,
            name: record.name,
            key_valid_from: record.key_valid_from,
            key_status,
        }
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
    use homescope_common::device_key::DeviceKey;

    use super::*;
    use crate::devices::keys::SealedDeviceKey;

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

    fn record(key: Option<Vec<u8>>) -> DeviceRecord {
        DeviceRecord {
            id: 1,
            device_addr: ADDR,
            name: "kitchen".into(),
            key,
            key_valid_from: DateTime::from_timestamp(1_753_000_000, 0).expect("valid timestamp"),
        }
    }

    fn status_of(key: Option<Vec<u8>>) -> DeviceKeyStatus {
        DeviceSummary::classify(record(key), &KekRing::for_test()).key_status
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
    #[test]
    fn a_broken_key_still_yields_a_summary() {
        let summary = DeviceSummary::classify(record(None), &KekRing::for_test());

        assert_eq!(summary.device_addr, ADDR);
        assert_eq!(summary.name, "kitchen");
    }

    /// `DeviceSummary` is the response body of both device read endpoints, so
    /// its serialized shape is an API contract that `#[serde(rename_all)]`, a
    /// renamed field or a newly added column can all break silently. Asserting
    /// the whole object is the review checkpoint the derive does not have: a
    /// field added to `DeviceRecord` and plumbed through here fails this test
    /// before it reaches a client.
    #[test]
    fn summary_renders_the_documented_json() {
        assert_eq!(
            serde_json::to_value(DeviceSummary::classify(
                record(Some(sealed_column(ADDR))),
                &KekRing::for_test()
            ))
            .expect("serializable"),
            serde_json::json!({
                "deviceAddr": "060504030201",
                "name": "kitchen",
                "keyStatus": "OK",
                "keyValidFrom": "2025-07-20T08:26:40Z",
            })
        );
    }

    /// homescope-provision will branch on these strings, so they are pinned
    /// literally rather than through the enum. Nothing else fails if
    /// `rename_all` changes — the code still compiles and every round-trip
    /// still passes.
    #[test]
    fn key_status_renders_as_screaming_snake_case() {
        for (status, expected) in [
            (DeviceKeyStatus::Missing, "MISSING"),
            (DeviceKeyStatus::Invalid, "INVALID"),
            (DeviceKeyStatus::KekUnavailable, "KEK_UNAVAILABLE"),
            (DeviceKeyStatus::Unopenable, "UNOPENABLE"),
            (DeviceKeyStatus::Ok, "OK"),
        ] {
            assert_eq!(
                serde_json::to_value(status).expect("serializable"),
                serde_json::Value::from(expected)
            );
        }
    }
}
