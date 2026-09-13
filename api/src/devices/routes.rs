use axum::{
    Router,
    extract::State,
    http::StatusCode,
    routing::{get, post},
};
use homescope_api_types::{
    devices::{DeviceKeyResponse, DeviceSummary, ProvisionDevicePayload},
    error::{ApiErrorCode, DeviceAlreadyExistsDetails},
};
use homescope_common::device_addr::DeviceAddr;
use tracing::{error, info, instrument};

use crate::{
    devices::{DeviceRegistry, registry::DeviceError},
    http::{
        AppRouter, AppState,
        error::ApiError,
        extract::{Json, Path},
    },
};

impl From<DeviceError> for ApiError {
    fn from(value: DeviceError) -> Self {
        match value {
            DeviceError::AlreadyExists { name, device_addr } => ApiError::with_details(
                StatusCode::CONFLICT,
                format!("device {device_addr} already exists as '{name}' - rotate its key instead"),
                DeviceAlreadyExistsDetails { device_addr, name },
            ),

            DeviceError::NotFound => ApiError::new(
                StatusCode::NOT_FOUND,
                ApiErrorCode::DeviceNotFound,
                "device not found",
            ),

            DeviceError::Db(err) => {
                error!(%err, "device db operation failed");
                ApiError::internal()
            }

            DeviceError::KeyGen(err) => {
                error!(%err, "device key generation failed");
                ApiError::internal()
            }
        }
    }
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
            key: key.to_hex().as_str().to_owned(),
            device_addr: device.device_addr,
            name: device.name.clone(),
            key_valid_from: device.key_valid_from,
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
            device_addr: device.device_addr,
            name: device.name.clone(),
            key_valid_from: device.key_valid_from,
            key: key.to_hex().as_str().to_owned(),
        }),
    ))
}

#[instrument(skip_all, fields(%device_addr))]
async fn get_device(
    State(device_registry): State<DeviceRegistry>,
    Path(device_addr): Path<DeviceAddr>,
) -> Result<(StatusCode, Json<DeviceSummary>), ApiError> {
    // Through `DeviceError::NotFound`, so a missing device reads the same here
    // as it does from `rotate-key` — one message, one place to change it.
    let device = device_registry
        .summary(device_addr)
        .await?
        .ok_or(DeviceError::NotFound)?;

    Ok((StatusCode::OK, Json(device)))
}

#[instrument(skip_all)]
async fn get_devices(
    State(device_registry): State<DeviceRegistry>,
) -> Result<(StatusCode, Json<Vec<DeviceSummary>>), ApiError> {
    let devices = device_registry.summaries().await?;

    Ok((StatusCode::OK, Json(devices)))
}

pub fn router() -> AppRouter {
    Router::<AppState>::new()
        .route("/", post(provision_device).get(get_devices))
        .route("/{device_addr}", get(get_device))
        .route("/{device_addr}/rotate-key", post(rotate_device_key))
}

// The hex rendering these handlers put in a `DeviceKeyResponse` is tested
// beside its implementation, in `homescope_common::device_key` — it is a
// contract with `homescope-provision`, not with axum.

/// `From<DeviceError>` is where the device errors meet the wire, and it is its
/// own code path — the `DeviceError::Db` arm does not go through
/// `From<sqlx::Error>`, so that conversion's test does not cover it.
#[cfg(test)]
mod test {
    use axum::{body::to_bytes, response::IntoResponse as _};
    use serde_json::json;

    use super::*;

    const ADDR: DeviceAddr = DeviceAddr([0x01, 0x02, 0x03, 0x04, 0x05, 0x06]);

    async fn rendered(err: DeviceError) -> (StatusCode, serde_json::Value) {
        let response = ApiError::from(err).into_response();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");

        (
            status,
            serde_json::from_slice(&bytes).expect("body is json"),
        )
    }

    /// The 409 is what `homescope-provision` branches on, and `details` is the
    /// part it can act on without scraping `message`. Both are asserted in
    /// full: the name and address in `details` must be the *existing* row's.
    #[tokio::test]
    async fn a_duplicate_device_is_a_conflict_naming_the_existing_row() {
        let (status, body) = rendered(DeviceError::AlreadyExists {
            device_addr: ADDR,
            name: "kitchen".to_owned(),
        })
        .await;

        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(
            body,
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

    #[tokio::test]
    async fn a_missing_device_is_a_404_without_details() {
        let (status, body) = rendered(DeviceError::NotFound).await;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(
            body,
            json!({ "code": "device_not_found", "message": "device not found" })
        );
    }

    /// Same rule as `http::error`'s db test, on the arm that test cannot reach:
    /// `sqlx::Error`'s `Display` names tables and constraints — including the
    /// `devices_device_addr_key` constraint the duplicate check matches on.
    #[tokio::test]
    async fn a_db_failure_behind_a_device_operation_is_not_echoed() {
        let (status, body) = rendered(DeviceError::Db(sqlx::Error::RowNotFound)).await;

        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(
            body,
            json!({ "code": "internal_error", "message": "internal server error" })
        );
    }
}
