use serde::{Deserialize, Serialize};
use strum::{Display, VariantArray};

/// The wire shape. Separate from `ApiError` because `StatusCode` isn't
/// `Serialize` and because the body is a contract — it should change only
/// deliberately, not as a side effect of adding a field to the struct.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiErrorBody {
    pub code: ApiErrorCode,
    pub message: String,
}

/// The `code` field of an error body — the part clients branch on.
///
/// `message` is prose and may be reworded freely; this is the contract.
/// Both the wire string and the `Display` string are derived from the
/// variant name by `rename_all` / `serialize_all`, which makes a typo
/// impossible but a *rename* invisible: changing `NotFound` to
/// `RouteNotFound` silently rewrites the JSON. `VariantArray` exists so the
/// test below can be exhaustive and catch exactly that — see
/// `every_code_has_a_pinned_wire_string`. When it fires after a deliberate
/// Rust-side rename, the fix is `#[serde(rename = "…")]` on the variant to
/// hold the old wire string, not a new literal in the test.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Display, VariantArray)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum ApiErrorCode {
    InternalError,

    InvalidBody,
    InvalidParams,
    NotFound,
    MethodNotAllowed,
    Unauthorized,

    DeviceAlreadyExists,
    DeviceNotFound,

    /// A code this build does not know. The API may add codes at any time,
    /// and an older `homescope-provision` must still be able to read the
    /// `message` beside it — a closed enum here turns "device already
    /// exists" into "malformed response".
    ///
    /// `#[serde(other)]` is deserialize-only, so this variant is a landing
    /// pad, never a source: the API constructs the specific codes and this
    /// one never reaches the wire. It would serialize as `"unknown"` if it
    /// ever did, which the test below pins so the round trip stays total.
    #[serde(other)]
    Unknown,
}

#[cfg(test)]
mod test {
    use serde_json::json;

    use super::*;

    /// The wire strings, stated as literals rather than derived.
    ///
    /// `rename_all` can rewrite the entire contract in a single edit, and a
    /// test that also derived the expected strings would follow it silently —
    /// so the expectation is spelled out by hand, which is the same reasoning
    /// as `homescope_common::packet::cipher::known_answer`.
    ///
    /// The `match` is exhaustive on purpose: adding a variant fails to
    /// *compile* here until its string is pinned, which is what stops the
    /// golden set from going stale. `VariantArray` supplies the iteration so
    /// no variant can be pinned and then forgotten at the call site either.
    #[test]
    fn every_code_has_a_pinned_wire_string() {
        for &code in ApiErrorCode::VARIANTS {
            let expected = match code {
                ApiErrorCode::InternalError => "internal_error",
                ApiErrorCode::InvalidBody => "invalid_body",
                ApiErrorCode::InvalidParams => "invalid_params",
                ApiErrorCode::NotFound => "not_found",
                ApiErrorCode::MethodNotAllowed => "method_not_allowed",
                ApiErrorCode::Unauthorized => "unauthorized",
                ApiErrorCode::DeviceAlreadyExists => "device_already_exists",
                ApiErrorCode::DeviceNotFound => "device_not_found",
                ApiErrorCode::Unknown => "unknown",
            };

            assert_eq!(
                serde_json::to_value(code).expect("serializes"),
                json!(expected),
                "{code:?} does not serialize to its pinned string"
            );

            assert_eq!(
                serde_json::from_value::<ApiErrorCode>(json!(expected)).expect("deserializes"),
                code,
                "{expected} does not parse back to {code:?}"
            );

            assert_eq!(
                code.to_string(),
                expected,
                "Display disagrees with the wire for {code:?}"
            );
        }
    }

    /// The deployment-skew guard, and the reason `Unknown` exists.
    ///
    /// `homescope-provision` runs on a workstation and is updated
    /// independently of the API container on the Pi, so an older binary will
    /// meet codes added after it was built. Without the catch-all it fails to
    /// *deserialize* the body and reports "malformed response" — losing the
    /// `message` the API sent precisely to explain itself. The client fails
    /// hardest exactly when the server is trying hardest to be understood.
    #[test]
    fn an_unrecognised_code_becomes_unknown() {
        assert_eq!(
            serde_json::from_value::<ApiErrorCode>(json!("brand_new_code")).expect("never fails"),
            ApiErrorCode::Unknown
        );
    }

    /// The body's field names, also as literals.
    ///
    /// `rename_all = "camelCase"` is inert on `code`/`message` — both are
    /// single words — which makes it exactly the kind of attribute someone
    /// removes as dead weight. It is not dead: it is the naming policy the
    /// next field inherits, and dropping it would make that field diverge
    /// from every other DTO here.
    #[test]
    fn the_error_body_wire_shape() {
        let body = ApiErrorBody {
            code: ApiErrorCode::DeviceAlreadyExists,
            message: "device already exists - rotate its key instead".to_owned(),
        };

        assert_eq!(
            serde_json::to_value(&body).expect("serializes"),
            json!({
                "code": "device_already_exists",
                "message": "device already exists - rotate its key instead",
            })
        );
    }

    /// An error body carrying an unknown code still yields its message.
    ///
    /// This is the whole point of `Unknown` expressed end to end: the two
    /// pieces (catch-all variant, surrounding struct) have to work together,
    /// and it is the struct that a client actually deserializes.
    #[test]
    fn an_unknown_code_still_carries_its_message() {
        let body: ApiErrorBody = serde_json::from_value(json!({
            "code": "key_generation_failed",
            "message": "could not generate a device key",
        }))
        .expect("an unknown code is not a parse failure");

        assert_eq!(body.code, ApiErrorCode::Unknown);
        assert_eq!(body.message, "could not generate a device key");
    }
}
