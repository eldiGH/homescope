use std::fmt::Write as _;

use axum::{
    Router,
    extract::State,
    http::StatusCode,
    routing::{get, post},
};
use chrono::{DateTime, Utc};
use homescope_common::{device_addr::DeviceAddr, device_key::DeviceKey};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use tracing::{error, info, instrument};

use crate::{
    devices::{
        DeviceRegistry,
        registry::DeviceError,
        store::{self, Device, DeviceRowError},
    },
    http::{
        AppRouter, AppState,
        error::ApiError,
        extract::{Json, Path},
    },
};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProvisionDevicePayload {
    name: String,
    device_addr: DeviceAddr,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DeviceKeyResponse {
    #[serde(flatten)]
    device: DeviceResponse,
    key: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DeviceResponse {
    device_addr: DeviceAddr,
    name: String,
    key_valid_from: DateTime<Utc>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DeviceErrorResponse {
    device_addr: DeviceAddr,
    error: String,
}

impl From<&DeviceRowError> for DeviceErrorResponse {
    fn from(value: &DeviceRowError) -> Self {
        Self {
            device_addr: value.device_addr,
            error: value.source.to_string(),
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase", tag = "status")]
enum DeviceOrErrorResponse {
    Ok(DeviceResponse),
    Error(DeviceErrorResponse),
}

impl From<&Device> for DeviceResponse {
    fn from(value: &Device) -> Self {
        Self {
            name: value.name.clone(),
            key_valid_from: value.key_valid_from,
            device_addr: value.device_addr,
        }
    }
}

impl From<Device> for DeviceResponse {
    fn from(value: Device) -> Self {
        Self {
            name: value.name,
            key_valid_from: value.key_valid_from,
            device_addr: value.device_addr,
        }
    }
}

impl From<DeviceError> for ApiError {
    fn from(value: DeviceError) -> Self {
        match value {
            DeviceError::AlreadyExists => ApiError::new(
                StatusCode::CONFLICT,
                "device_already_exists",
                "device already exists - rotate its key instead",
            ),

            DeviceError::NotFound => ApiError::new(
                StatusCode::NOT_FOUND,
                "device_not_found",
                "device not found",
            ),

            DeviceError::Store(err) => {
                error!(%err, "device store operation failed");
                ApiError::internal()
            }

            DeviceError::KeyGen(err) => {
                error!(%err, "device key generation failed");
                ApiError::internal()
            }
        }
    }
}

/// Renders a device key as uppercase hex — the only place a plaintext key
/// becomes text.
///
/// `DeviceKey` deliberately implements neither `Display` nor `Serialize`, so
/// `{}` on a key, or a `#[derive(Serialize)]` on a struct holding one, is a
/// compile error rather than a leak. That makes this function the single
/// explicit escape hatch, and the one thing to grep for when asking how a key
/// could reach a wire.
///
/// # Why hex, not base64
///
/// Provisioning writes the key into `UICR.CUSTOMER` and reads it back to
/// verify byte-for-byte, and that record's layout is chosen so a hex dump of
/// `0x10001084` shows the key in order (see `homescope-board`'s
/// `chip::device_key`). Hex is the only encoding comparable against such a
/// dump by eye — which is what you are reduced to when a freshly provisioned
/// node's packets fail their AEAD tag with no other symptom. Base64 would
/// save twenty characters and cost that.
///
/// # Zeroization
///
/// Taking the key by value means it drops, and so zeroizes, when this returns.
/// The `String` does not, and neither do serde's serialization buffer or
/// hyper's write buffer — all three are heap allocations still holding the
/// plaintext when they are freed. `Zeroizing<String>` would clear the first
/// and none of the rest, so the window is narrowed at the source and open
/// downstream by design. Read that as a reason not to add further copies of
/// the rendered key, not as a gap to close here.
fn key_to_hex(key: DeviceKey) -> String {
    let mut hex_string = String::with_capacity(DeviceKey::SIZE * 2);

    for byte in key.as_bytes() {
        write!(&mut hex_string, "{:02X}", byte).expect("will never error");
    }

    hex_string
}

#[instrument(skip_all, fields(device_addr = %payload.device_addr))]
async fn provision_device(
    State(device_registry): State<DeviceRegistry>,
    Json(payload): Json<ProvisionDevicePayload>,
) -> Result<(StatusCode, Json<DeviceKeyResponse>), ApiError> {
    let (device, key) = device_registry
        .provision(payload.device_addr, &payload.name)
        .await?;

    info!("device provisioned");

    Ok((
        StatusCode::CREATED,
        Json(DeviceKeyResponse {
            key: key_to_hex(key),
            device: (&device.device).into(),
        }),
    ))
}

#[instrument(skip_all, fields(%device_addr))]
async fn rotate_device_key(
    State(device_registry): State<DeviceRegistry>,
    Path(device_addr): Path<DeviceAddr>,
) -> Result<(StatusCode, Json<DeviceKeyResponse>), ApiError> {
    let (device, key) = device_registry.rotate_key(device_addr).await?;

    info!("device key rotated");

    Ok((
        StatusCode::OK,
        Json(DeviceKeyResponse {
            device: (&device.device).into(),
            key: key_to_hex(key),
        }),
    ))
}

#[instrument(skip_all, fields(%device_addr))]
async fn get_device(
    State(pool): State<PgPool>,
    Path(device_addr): Path<DeviceAddr>,
) -> Result<(StatusCode, Json<DeviceOrErrorResponse>), ApiError> {
    let device = store::get_device(&pool, device_addr)
        .await?
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::NOT_FOUND,
                "device_not_found",
                "device was not found",
            )
        })?;

    let response = match device {
        Ok(device) => DeviceOrErrorResponse::Ok(DeviceResponse::from(device)),
        Err(err) => DeviceOrErrorResponse::Error((&err).into()),
    };

    Ok((StatusCode::OK, Json(response)))
}

#[instrument(skip_all)]
async fn get_devices(
    State(pool): State<PgPool>,
) -> Result<(StatusCode, Json<Vec<DeviceOrErrorResponse>>), ApiError> {
    let devices = store::get_devices(&pool).await?;

    Ok((
        StatusCode::OK,
        Json(
            devices
                .into_iter()
                .map(|r| match r {
                    Ok(dev) => DeviceOrErrorResponse::Ok(DeviceResponse::from(dev)),
                    Err(err) => DeviceOrErrorResponse::Error(DeviceErrorResponse::from(&err)),
                })
                .collect(),
        ),
    ))
}

pub fn router() -> AppRouter {
    Router::<AppState>::new()
        .route("/", post(provision_device).get(get_devices))
        .route("/{device_addr}", get(get_device))
        .route("/{device_addr}/rotate-key", post(rotate_device_key))
}

#[cfg(test)]
mod test {
    use super::*;

    /// Fixed input, fixed output. The rendering is a contract with
    /// homescope-provision, which decodes this string back into the 32 bytes
    /// it writes to `UICR.CUSTOMER` — a change of case or byte order here is a
    /// device whose packets fail their AEAD tag with no other symptom.
    #[test]
    fn key_renders_as_uppercase_hex_in_order() {
        let mut bytes = [0u8; DeviceKey::SIZE];
        bytes[0] = 0x00;
        bytes[1] = 0x0F;
        bytes[2] = 0xA5;
        bytes[DeviceKey::SIZE - 1] = 0xFF;

        assert_eq!(
            key_to_hex(DeviceKey::from_bytes(bytes)),
            format!("000FA5{}FF", "00".repeat(DeviceKey::SIZE - 4)),
        );
    }

    /// Length is what a client validates before decoding, so pin it
    /// separately from the byte order above.
    #[test]
    fn key_renders_to_two_chars_per_byte() {
        let hex = key_to_hex(DeviceKey::from_bytes([0xAB; DeviceKey::SIZE]));

        assert_eq!(hex.len(), DeviceKey::SIZE * 2);
        assert_eq!(hex, "AB".repeat(DeviceKey::SIZE));
    }
}
