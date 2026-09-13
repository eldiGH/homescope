use chrono::{DateTime, Utc};
use homescope_common::device_addr::DeviceAddr;
use serde::{Deserialize, Serialize};
use strum::VariantArray;

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

/// One device as `GET /devices` and `GET /devices/{addr}` render it: what the
/// registry holds, whether its key is usable, and whether the device has been
/// heard from under that key.
///
/// `Debug` is fine here, unlike on [`DeviceKeyResponse`]: nothing in a summary
/// is secret — the key's *status* travels, never the key.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceSummary {
    pub device_addr: DeviceAddr,
    pub name: String,
    pub key_status: DeviceKeyStatus,
    pub key_valid_from: DateTime<Utc>,

    /// The newest reading since `key_valid_from`; `null` means registered but
    /// never reported under the current key.
    ///
    /// ⚠️ Required *and* nullable. Serde treats a missing `Option` field as
    /// `None` on its own, which would make an API that predates this field
    /// indistinguishable from a device that has never reported — `verify`
    /// would poll a healthy sensor until it gave up. Naming
    /// `Option::deserialize` as `deserialize_with` switches that implicit
    /// default off: absence is a parse error, while `null` still reads as
    /// `None`.
    #[serde(deserialize_with = "Option::deserialize")]
    pub last_seen: Option<DateTime<Utc>>,
}

/// Whether the API can use a device's stored key. The states are kept distinct
/// on the wire because each has a different fix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, VariantArray)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum DeviceKeyStatus {
    /// No key on file. Provision or rotate.
    Missing,

    /// The stored bytes are not a sealed key this API can parse. Rotate.
    Invalid,

    /// Sealed under a KEK generation the API has not loaded. The key itself may
    /// be fine: load that generation rather than rotating, which would force a
    /// re-flash of a device whose key was never wrong.
    KekUnavailable,

    /// The seal does not open for this device under the loaded KEK — sealed
    /// for another device, or corrupted. Rotate.
    Unopenable,

    /// The key opens, so this device's packets can be decrypted.
    Ok,

    /// A status this build does not know: the same deserialize-only landing
    /// pad as `ApiErrorCode::Unknown`, never sent by the API.
    #[serde(other)]
    Unknown,
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

    /// The summary golden, in both states `lastSeen` can be in. Built from
    /// literals rather than through `classify` — this crate cannot see the KEK
    /// — so `classify`'s field plumbing is pinned by the API's own tests.
    ///
    /// ⚠️ A never-seen device renders `"lastSeen": null`, not an absent key.
    /// `null` is an answer, and the client requires the key to be present
    /// (see `device_summary_requires_last_seen_to_be_present`), so adding
    /// `skip_serializing_if` here would break every client on the first
    /// device that has not reported yet.
    #[test]
    fn device_summary_wire_shape() {
        let an_hour_later =
            DateTime::from_timestamp(KEY_VALID_FROM + 3600, 0).expect("valid timestamp");

        for (last_seen, rendered) in [
            (None, json!(null)),
            (Some(an_hour_later), json!("2025-07-20T09:26:40Z")),
        ] {
            let summary = DeviceSummary {
                device_addr: ADDR,
                name: "kitchen".to_owned(),
                key_status: DeviceKeyStatus::Ok,
                key_valid_from: key_valid_from(),
                last_seen,
            };

            assert_eq!(
                serde_json::to_value(&summary).expect("serializes"),
                json!({
                    "deviceAddr": "C60504030201",
                    "name": "kitchen",
                    "keyStatus": "OK",
                    "keyValidFrom": "2025-07-20T08:26:40Z",
                    "lastSeen": rendered,
                })
            );
        }
    }

    /// Until this move `DeviceSummary` was `Serialize`-only, so the direction
    /// `homescope-provision` needs had never run at all. Reader-backed for the
    /// same reason as `device_key_response_parses_from_a_reader`.
    #[test]
    fn device_summary_parses_from_a_reader() {
        let body = br#"{
            "deviceAddr": "C60504030201",
            "name": "kitchen",
            "keyStatus": "KEK_UNAVAILABLE",
            "keyValidFrom": "2025-07-20T08:26:40Z",
            "lastSeen": "2025-07-20T09:26:40Z"
        }"#;

        let summary: DeviceSummary = serde_json::from_reader(&body[..]).expect("reader-backed");

        assert_eq!(summary.device_addr, ADDR);
        assert_eq!(summary.name, "kitchen");
        assert!(matches!(
            summary.key_status,
            DeviceKeyStatus::KekUnavailable
        ));
        assert_eq!(summary.key_valid_from, key_valid_from());
        assert_eq!(
            summary.last_seen,
            DateTime::from_timestamp(KEY_VALID_FROM + 3600, 0)
        );
    }

    /// The API will grow fields here — a site, a room — before every installed
    /// `provision` is rebuilt, and those binaries must keep reading summaries.
    #[test]
    fn device_summary_tolerates_an_added_field() {
        let summary: DeviceSummary = serde_json::from_value(json!({
            "deviceAddr": "C60504030201",
            "name": "kitchen",
            "keyStatus": "OK",
            "keyValidFrom": "2025-07-20T08:26:40Z",
            "lastSeen": null,
            "site": "home",
        }))
        .expect("an added field is not a parse failure");

        assert_eq!(summary.device_addr, ADDR);
    }

    /// ⚠️ The other direction of skew from the test above: a newer `provision`
    /// reading an older API that has no `lastSeen` at all. That must be a parse
    /// failure, because the alternative is reading every device as "never
    /// reported" and letting `verify` poll a healthy sensor until it times out.
    /// Serde would default the missing `Option` to `None` silently — the
    /// `deserialize_with` on the field is what this test holds in place.
    #[test]
    fn device_summary_requires_last_seen_to_be_present() {
        let absent = serde_json::from_value::<DeviceSummary>(json!({
            "deviceAddr": "C60504030201",
            "name": "kitchen",
            "keyStatus": "OK",
            "keyValidFrom": "2025-07-20T08:26:40Z",
        }));

        assert!(
            absent.is_err(),
            "an absent lastSeen must not read as never seen"
        );

        let never_seen: DeviceSummary = serde_json::from_value(json!({
            "deviceAddr": "C60504030201",
            "name": "kitchen",
            "keyStatus": "OK",
            "keyValidFrom": "2025-07-20T08:26:40Z",
            "lastSeen": null,
        }))
        .expect("null is an answer, not an absence");

        assert_eq!(never_seen.last_seen, None);
    }

    /// The key-status strings, as literals — `rename_all` can rewrite all of
    /// them in one edit, and a test deriving its expectations would follow.
    ///
    /// The `match` is exhaustive, so a new variant fails to compile here until
    /// its string is pinned, and `VariantArray` supplies the iteration so no
    /// variant can be pinned and then left out of the loop.
    #[test]
    fn every_key_status_has_a_pinned_wire_string() {
        use DeviceKeyStatus::*;

        for &status in DeviceKeyStatus::VARIANTS {
            let expected = match status {
                Missing => "MISSING",
                Invalid => "INVALID",
                KekUnavailable => "KEK_UNAVAILABLE",
                Unopenable => "UNOPENABLE",
                Ok => "OK",
                Unknown => "UNKNOWN",
            };

            assert_eq!(
                serde_json::to_value(status).expect("serializes"),
                json!(expected),
                "{status:?} does not serialize to its pinned string"
            );

            assert_eq!(
                serde_json::from_value::<DeviceKeyStatus>(json!(expected)).expect("deserializes"),
                status,
                "{expected} does not parse back to {status:?}"
            );
        }
    }

    /// The deployment-skew guard, mirroring `ApiErrorCode::Unknown`.
    #[test]
    fn an_unrecognised_key_status_becomes_unknown() {
        let status: DeviceKeyStatus =
            serde_json::from_value(json!("REVOKED")).expect("never fails");

        assert!(matches!(status, DeviceKeyStatus::Unknown));
    }

    /// Why the catch-all matters more here than on a single response: `list`
    /// deserializes a `Vec`, so without it one row with a status this build
    /// does not know fails the listing for the entire fleet.
    #[test]
    fn a_fleet_listing_survives_one_unrecognised_status() {
        let body = br#"[
            {
                "deviceAddr": "C60504030201",
                "name": "kitchen",
                "keyStatus": "OK",
                "keyValidFrom": "2025-07-20T08:26:40Z",
                "lastSeen": "2025-07-20T09:26:40Z"
            },
            {
                "deviceAddr": "C60504030202",
                "name": "garage",
                "keyStatus": "REVOKED",
                "keyValidFrom": "2025-07-20T08:26:40Z",
                "lastSeen": null
            }
        ]"#;

        let fleet: Vec<DeviceSummary> = serde_json::from_reader(&body[..]).expect("reader-backed");

        assert_eq!(fleet.len(), 2);
        assert!(matches!(fleet[0].key_status, DeviceKeyStatus::Ok));
        assert!(matches!(fleet[1].key_status, DeviceKeyStatus::Unknown));
    }
}
