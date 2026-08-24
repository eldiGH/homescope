use axum::{
    Router,
    extract::State,
    http::StatusCode,
    routing::{get, post},
};
use homescope_api_types::{
    devices::{DeviceKeyResponse, ProvisionDevicePayload},
    error::ApiErrorCode,
};
use homescope_common::device_addr::DeviceAddr;
use tracing::{error, info, instrument};

use crate::{
    devices::{DeviceRegistry, registry::DeviceError, summary::DeviceSummary},
    http::{
        AppRouter, AppState,
        error::ApiError,
        extract::{Json, Path},
    },
};

impl From<DeviceError> for ApiError {
    fn from(value: DeviceError) -> Self {
        match value {
            DeviceError::AlreadyExists => ApiError::new(
                StatusCode::CONFLICT,
                ApiErrorCode::DeviceAlreadyExists,
                "device already exists - rotate its key instead",
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
    let device = device_registry.summary(device_addr).await?.ok_or_else(|| {
        ApiError::new(
            StatusCode::NOT_FOUND,
            ApiErrorCode::DeviceNotFound,
            "device was not found",
        )
    })?;

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
