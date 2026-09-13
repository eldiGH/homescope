use homescope_common::device_addr::DeviceAddr;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use strum::{Display, VariantArray};

/// A typed `details` payload, bound to the one [`ApiErrorCode`] it belongs to.
///
/// **`code` is the tag.** It already names which error happened, so `details`
/// carries no discriminator of its own. The obvious way to get that from serde
/// — `#[serde(tag = "code", content = "details")]` — was tried and rejected:
/// its catch-all variant cannot absorb content, so an older client meeting an
/// unknown code *with* details, or a known code that later gained details,
/// fails to parse the whole body and loses `message` with it. Instead the body
/// holds `details` as raw JSON, and [`ApiErrorBody::details`] interprets it
/// only once `code` is known.
///
/// A trait with an associated const rather than an enum: an enum needs every
/// payload in three places — a variant, an arm mapping it to its code, and an
/// arm mapping the code back — with nothing making them agree. Here the pairing
/// is stated once, beside the type, and both directions read it.
///
/// ⚠️ **One details type per code, and one code per details type** — enforced
/// at compile time, not by convention. Declare every payload with
/// `error_details!`, never with a hand-written `impl`:
///
/// - a second type claiming an already-claimed code is `E0119` on
///   `OneDetailsTypePerCode<N>`. Two shapes under one code would stop `code`
///   from determining the payload — and since unknown fields are ignored, a
///   narrower struct could parse the other's payload and *succeed*;
/// - one type claiming a second code is `E0119` on this trait;
/// - a type outside this crate cannot implement it at all (`Sealed`), so every
///   payload lives beside the goldens that pin it.
///
/// A payload that needs to grow gains optional fields on its existing struct.
/// A genuinely different shape is a different error, and gets its own code.
pub trait ApiErrorDetails: claim::Sealed + Serialize + DeserializeOwned {
    const CODE: ApiErrorCode;
}

/// The compile-time half of [`ApiErrorDetails`]'s one-to-one rule.
mod claim {
    /// Keeps `ApiErrorDetails` implementable only inside this crate.
    pub trait Sealed {}

    /// Implemented once per code, keyed by its discriminant, so trait
    /// coherence rejects a second payload type claiming the same code.
    ///
    /// Reading this because of an `E0119` naming this trait? That code already
    /// has a details type: extend that struct, or give the new shape its own
    /// code.
    // `allow`, not `expect`: an item that allows `dead_code` counts as live
    // to the analysis, so an `expect` here could never be fulfilled.
    #[allow(
        dead_code,
        reason = "only ever named in the claim impls; coherence over them is the point"
    )]
    pub trait OneDetailsTypePerCode<const CODE: u8> {}

    /// Never constructed — it only gives the claims a single type to collide on.
    #[allow(
        dead_code,
        reason = "only ever named in the claim impls; coherence over them is the point"
    )]
    pub enum Registry {}
}

/// Declares `$ty` as the details payload for `ApiErrorCode::$code`.
///
/// Emits the trait impl and the coherence claim from the same `$code`, so the
/// two cannot drift apart. ⚠️ A hand-written `impl ApiErrorDetails` inside this
/// crate still compiles and skips the claim: stable Rust cannot make the trait
/// itself demand it (that needs the unstable `generic_const_exprs`), so this
/// macro is where the rule is actually enforced.
macro_rules! error_details {
    ($code:ident => $ty:ty) => {
        impl claim::Sealed for $ty {}

        impl ApiErrorDetails for $ty {
            const CODE: ApiErrorCode = ApiErrorCode::$code;
        }

        impl claim::OneDetailsTypePerCode<{ ApiErrorCode::$code as u8 }> for claim::Registry {}
    };
}

/// Details for [`ApiErrorCode::DeviceAlreadyExists`]: the row already holding
/// the address, so a caller can act on it — offer a key rotation, name the
/// device — without re-parsing `message`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceAlreadyExistsDetails {
    pub device_addr: DeviceAddr,
    pub name: String,
}

error_details!(DeviceAlreadyExists => DeviceAlreadyExistsDetails);

/// The wire shape. Separate from `ApiError` because `StatusCode` isn't
/// `Serialize` and because the body is a contract — it should change only
/// deliberately, not as a side effect of adding a field to the struct.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiErrorBody {
    pub code: ApiErrorCode,
    pub message: String,

    /// Raw until `code` says how to read it — see [`ApiErrorDetails`].
    ///
    /// Private, so the constructors are the only way to set it: that is what
    /// keeps it in step with `code`. `skip_serializing_if` keeps the common
    /// case off the wire entirely rather than as `"details": null`; `default`
    /// lets a body with no `details` key parse as `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    details: Option<Value>,
}

impl ApiErrorBody {
    pub fn new(code: ApiErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            details: None,
        }
    }

    /// A body carrying `details`, with its `code` taken from the details type
    /// rather than passed in — so a body labelled with one code and carrying
    /// another code's payload cannot be constructed.
    pub fn with_details<D: ApiErrorDetails>(message: impl Into<String>, details: D) -> Self {
        Self {
            code: D::CODE,
            message: message.into(),
            details: Some(
                serde_json::to_value(details)
                    // Only a map with non-string keys or a failing `Serialize`
                    // impl can make this fail, and payloads are plain structs.
                    // Deterministic per type, so the goldens exercising each
                    // payload once are what make this `expect` sound.
                    .expect("details payloads are plain structs and always serialize"),
            ),
        }
    }

    /// This body's details as `D`, if `D` is what its `code` carries.
    ///
    /// `None` when the code is not `D::CODE`, when there are no details, or
    /// when they do not parse as `D`. ⚠️ Deliberately never an error: a client
    /// must survive details it does not expect, so a malformed or unfamiliar
    /// payload costs the details and nothing else — `code` and `message` still
    /// arrive. The flip side is that a server-side mistake in a payload's
    /// shape reads as "no details"; the goldens are what catch that.
    pub fn details<D: ApiErrorDetails>(&self) -> Option<D> {
        if self.code != D::CODE {
            return None;
        }

        D::deserialize(self.details.as_ref()?).ok()
    }
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
///
/// `#[repr(u8)]` exists only for `claim::OneDetailsTypePerCode`, which keys on
/// the discriminant. Discriminants never reach the wire — codes travel as names
/// — so reordering variants stays safe; the repr just makes the cast exact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Display, VariantArray)]
#[repr(u8)]
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

    const ADDR: DeviceAddr = DeviceAddr([0x01, 0x02, 0x03, 0x04, 0x05, 0x06]);

    fn conflict() -> DeviceAlreadyExistsDetails {
        DeviceAlreadyExistsDetails {
            device_addr: ADDR,
            name: "kitchen".to_owned(),
        }
    }

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
    ///
    /// Also pins the common case: no `"details"` key at all, not
    /// `"details": null`.
    #[test]
    fn the_error_body_wire_shape() {
        let body = ApiErrorBody::new(ApiErrorCode::DeviceNotFound, "device not found");

        assert_eq!(
            serde_json::to_value(&body).expect("serializes"),
            json!({
                "code": "device_not_found",
                "message": "device not found",
            })
        );
    }

    /// The populated case. No `type` inside `details` — `code` is the tag — and
    /// the code is not passed in at all: it comes from the payload's type.
    #[test]
    fn the_error_body_wire_shape_with_details() {
        let body = ApiErrorBody::with_details(
            "device 060504030201 already exists as 'kitchen' - rotate its key instead",
            conflict(),
        );

        assert_eq!(
            serde_json::to_value(&body).expect("serializes"),
            json!({
                "code": "device_already_exists",
                "message": "device 060504030201 already exists as 'kitchen' - rotate its key instead",
                "details": {
                    "deviceAddr": "060504030201",
                    "name": "kitchen",
                },
            })
        );
    }

    /// Server and client halves agree: what `with_details` writes, `details`
    /// reads back as the same value.
    #[test]
    fn details_round_trip_through_the_wire() {
        let sent = ApiErrorBody::with_details("m", conflict());
        let wire = serde_json::to_string(&sent).expect("serializes");

        let received: ApiErrorBody = serde_json::from_str(&wire).expect("deserializes");

        assert_eq!(
            received.details::<DeviceAlreadyExistsDetails>(),
            Some(conflict())
        );
    }

    /// The path `homescope-provision` takes: `from_reader`, which cannot lend
    /// borrows — the `DeviceAddr` regression's shape. `code` is deliberately
    /// *last*, after `details`: JSON promises no key order, so reading
    /// `details` cannot depend on having seen `code` first. Holding the payload
    /// raw is what makes that order irrelevant.
    #[test]
    fn details_parse_from_a_reader_with_the_code_last() {
        let body = br#"{
            "details": {
                "name": "kitchen",
                "deviceAddr": "060504030201"
            },
            "message": "m",
            "code": "device_already_exists"
        }"#;

        let body: ApiErrorBody = serde_json::from_reader(&body[..]).expect("reader-backed");

        assert_eq!(
            body.details::<DeviceAlreadyExistsDetails>(),
            Some(conflict())
        );
    }

    /// A body with no `details` key — the common case, and every body from
    /// before this field existed — parses and has no details to read.
    #[test]
    fn a_body_without_a_details_key_has_no_details() {
        let body: ApiErrorBody = serde_json::from_value(json!({
            "code": "device_already_exists",
            "message": "device already exists",
        }))
        .expect("a missing details key is not a parse failure");

        assert_eq!(body.details::<DeviceAlreadyExistsDetails>(), None);
    }

    /// ⚠️ `code` is the tag, so a payload is only ever read under its own code
    /// — even when it happens to have exactly the right shape. This is what
    /// untagged details could not do: a future error whose payload also has a
    /// `deviceAddr` and a `name` would have been read as a conflict.
    #[test]
    fn details_are_only_read_under_their_own_code() {
        let body: ApiErrorBody = serde_json::from_value(json!({
            "code": "device_not_found",
            "message": "m",
            "details": { "deviceAddr": "060504030201", "name": "kitchen" },
        }))
        .expect("parses");

        assert_eq!(body.details::<DeviceAlreadyExistsDetails>(), None);
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

    /// ⚠️ The first of the two cases that ruled out `code` as a serde tag: an
    /// unknown code arriving *with* a payload. Under the derive this failed the
    /// whole body; here it must cost nothing but the details.
    #[test]
    fn an_unknown_code_with_details_still_carries_its_message() {
        let body: ApiErrorBody = serde_json::from_value(json!({
            "code": "rate_limited",
            "message": "slow down",
            "details": { "retryAfterSecs": 30, "scope": { "route": "/devices" } },
        }))
        .expect("an unknown code with details is not a parse failure");

        assert_eq!(body.code, ApiErrorCode::Unknown);
        assert_eq!(body.message, "slow down");
        assert_eq!(body.details::<DeviceAlreadyExistsDetails>(), None);
    }

    /// ⚠️ The second: a code this client knows, which a newer API has since
    /// started sending details with. Under the derive every installed client
    /// broke on that change; here adding details to an existing code is safe.
    #[test]
    fn a_known_code_that_later_gained_details_still_parses() {
        let body: ApiErrorBody = serde_json::from_value(json!({
            "code": "not_found",
            "message": "no such route",
            "details": { "route": "/devicez" },
        }))
        .expect("added details on a known code are not a parse failure");

        assert_eq!(body.code, ApiErrorCode::NotFound);
        assert_eq!(body.message, "no such route");
    }

    /// A payload that does not match its own code's type costs the details and
    /// nothing else — including the case where the mismatch is ours.
    #[test]
    fn malformed_details_cost_only_the_details() {
        let body: ApiErrorBody = serde_json::from_value(json!({
            "code": "device_already_exists",
            "message": "m",
            "details": { "deviceAddr": 7, "name": "kitchen" },
        }))
        .expect("malformed details are not a parse failure");

        assert_eq!(body.code, ApiErrorCode::DeviceAlreadyExists);
        assert_eq!(body.details::<DeviceAlreadyExistsDetails>(), None);
    }

    /// The camelCase contract holds inside `details` too: a snake_case
    /// `device_addr` is not accepted as an alternative spelling.
    #[test]
    fn details_reject_snake_case_field_names() {
        let body: ApiErrorBody = serde_json::from_value(json!({
            "code": "device_already_exists",
            "message": "m",
            "details": { "device_addr": "060504030201", "name": "kitchen" },
        }))
        .expect("parses");

        assert_eq!(body.details::<DeviceAlreadyExistsDetails>(), None);
    }
}
