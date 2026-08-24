use chrono::{DateTime, Utc};
use homescope_common::device_addr::DeviceAddr;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProvisionDevicePayload {
    pub name: String,
    pub device_addr: DeviceAddr,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceKeyResponse {
    pub device_addr: DeviceAddr,
    pub name: String,
    pub key_valid_from: DateTime<Utc>,
    pub key: String,
}

/// Golden JSON for the device DTOs.
///
/// Sharing these structs between `homescope-api` and `homescope-provision`
/// removed the compile error that used to police the wire format: renaming a
/// field now updates both sides in lockstep and builds green, while every
/// already-installed `provision` binary breaks. The API container and the
/// workstation CLI ship independently, so source agreement is not deployment
/// agreement — these literals are what actually pins the JSON.
#[cfg(test)]
mod test {
    use serde_json::json;

    use super::*;

    /// The address in the goldens below. Matches `DeviceAddr`'s own tests,
    /// where the byte order and the `0xC0` static-random marking are pinned;
    /// here it is only a fixed value that renders as `C60504030201`.
    const ADDR: DeviceAddr = DeviceAddr([0x01, 0x02, 0x03, 0x04, 0x05, 0xC6]);

    /// 2025-07-20T08:26:40Z. Fixed rather than `Utc::now()` so the rendered
    /// timestamp format — RFC 3339, `Z`, no fractional seconds when they are
    /// zero — is part of what the golden pins.
    const KEY_VALID_FROM: i64 = 1_753_000_000;

    fn key_valid_from() -> DateTime<Utc> {
        DateTime::from_timestamp(KEY_VALID_FROM, 0).expect("valid timestamp")
    }

    #[test]
    fn provision_request_wire_shape() {
        let payload = ProvisionDevicePayload {
            name: "kitchen".to_owned(),
            device_addr: ADDR,
        };

        assert_eq!(
            serde_json::to_value(&payload).expect("serializes"),
            json!({ "name": "kitchen", "deviceAddr": "C60504030201" })
        );
    }

    /// The server side of the same contract: what the API's `Json` extractor
    /// must accept. Asserted separately from serialization because only one
    /// direction is exercised on each host, so a `Serialize`-only change
    /// would otherwise pass unnoticed.
    #[test]
    fn provision_request_parses_from_its_wire_shape() {
        let payload: ProvisionDevicePayload = serde_json::from_value(json!({
            "name": "kitchen",
            "deviceAddr": "C60504030201",
        }))
        .expect("deserializes");

        assert_eq!(payload.name, "kitchen");
        assert_eq!(payload.device_addr, ADDR);
    }

    #[test]
    fn provision_request_rejects_snake_case_field_names() {
        assert!(
            serde_json::from_value::<ProvisionDevicePayload>(json!({
                "name": "kitchen",
                "device_addr": "C60504030201",
            }))
            .is_err(),
            "camelCase is the contract; snake_case must not also work"
        );
    }

    #[test]
    fn device_key_response_wire_shape() {
        let response = DeviceKeyResponse {
            device_addr: ADDR,
            name: "kitchen".to_owned(),
            key_valid_from: key_valid_from(),
            key: "5A".repeat(32),
        };

        assert_eq!(
            serde_json::to_value(&response).expect("serializes"),
            json!({
                "deviceAddr": "C60504030201",
                "name": "kitchen",
                "keyValidFrom": "2025-07-20T08:26:40Z",
                "key": "5A5A5A5A5A5A5A5A5A5A5A5A5A5A5A5A\
                        5A5A5A5A5A5A5A5A5A5A5A5A5A5A5A5A",
            })
        );
    }

    /// The path `homescope-provision` actually takes: `ureq`'s
    /// `Body::read_json` is `serde_json::from_reader`, which — unlike axum's
    /// `from_slice` — can never lend out a borrow of its input.
    ///
    /// Regression: `DeviceAddr::deserialize` asked for a borrowed `&str`, so
    /// the API parsed these bodies happily while every response to the
    /// provisioning tool failed with `invalid type: string "…", expected a
    /// borrowed string`. `homescope_common` has the unit-level guard; this is
    /// the composed one, over the struct a client really receives.
    #[test]
    fn device_key_response_parses_from_a_reader() {
        let body = br#"{
            "deviceAddr": "C60504030201",
            "name": "kitchen",
            "keyValidFrom": "2025-07-20T08:26:40Z",
            "key": "5A5A5A5A5A5A5A5A5A5A5A5A5A5A5A5A5A5A5A5A5A5A5A5A5A5A5A5A5A5A5A5A"
        }"#;

        let response: DeviceKeyResponse =
            serde_json::from_reader(&body[..]).expect("reader-backed");

        assert_eq!(response.device_addr, ADDR);
        assert_eq!(response.name, "kitchen");
        assert_eq!(response.key_valid_from, key_valid_from());
        assert_eq!(response.key, "5A".repeat(32));
    }

    /// Unknown fields must be ignored, not rejected.
    ///
    /// The API may grow a field before the workstation's `provision` binary
    /// is rebuilt. `deny_unknown_fields` would turn that ordinary rollout
    /// into a hard failure on every response, which is why it is absent —
    /// this test is what says the absence is deliberate.
    #[test]
    fn device_key_response_tolerates_an_added_field() {
        let response: DeviceKeyResponse = serde_json::from_value(json!({
            "deviceAddr": "C60504030201",
            "name": "kitchen",
            "keyValidFrom": "2025-07-20T08:26:40Z",
            "key": "5A5A5A5A5A5A5A5A5A5A5A5A5A5A5A5A5A5A5A5A5A5A5A5A5A5A5A5A5A5A5A5A",
            "site": "home",
        }))
        .expect("an added field is not a parse failure");

        assert_eq!(response.device_addr, ADDR);
    }
}
