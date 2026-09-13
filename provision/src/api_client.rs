use std::time::Duration;

use homescope_api_types::{
    devices::{DeviceKeyResponse, DeviceSummary, ProvisionDevicePayload},
    error::{ApiErrorBody, ApiErrorCode},
};
use homescope_common::device_addr::DeviceAddr;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use ureq::{
    Agent, Body, RequestBuilder,
    http::{Response, StatusCode},
};

use crate::store::ApiTarget;

/// End to end for one request — DNS, connect, send, and reading the body.
///
/// ureq sets no timeout by default. That is survivable for a single interactive
/// call and not for `verify`, which makes dozens: one stalled connection would
/// hang it past its own deadline with nothing on screen.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

pub struct ApiClient {
    agent: Agent,
    target: ApiTarget,
}

impl ApiClient {
    pub fn new(target: ApiTarget) -> Self {
        let config = Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(REQUEST_TIMEOUT))
            .build();
        Self {
            agent: Agent::new_with_config(config),
            target,
        }
    }

    /// What a confirmation prompt should name — the profile, not the URL.
    pub fn label(&self) -> &str {
        &self.target.label
    }

    pub fn url(&self) -> &str {
        &self.target.url
    }

    fn format_url(&self, url: &str) -> String {
        format!("{}{}", self.target.url, url)
    }

    fn add_auth_header<T>(&self, req: RequestBuilder<T>) -> RequestBuilder<T> {
        req.header(
            "Authorization",
            format!("Bearer {}", self.target.token.expose()),
        )
    }

    /// Pre-flight: does the API answer, and does it accept this token?
    ///
    /// ⚠️ Worth running before anything destructive that happens before a device
    /// can be named. On the `--unlock` path the chip erase happens *before* the
    /// tool has ever spoken to the API, so an unverified token costs a board.
    pub fn check_auth(&self) -> Result<(), ApiClientError> {
        let response = self
            .add_auth_header(self.agent.get(self.format_url("/devices")))
            .call()?;

        reject_on_error_status(response).map(drop)
    }

    fn post<R, T>(&self, path: &str, send_body: &R) -> Result<T, ApiClientError>
    where
        T: for<'de> Deserialize<'de>,
        R: Serialize,
    {
        handle_response(
            self.add_auth_header(self.agent.post(self.format_url(path)))
                .send_json(send_body)?,
        )
    }

    fn post_empty<T>(&self, path: &str) -> Result<T, ApiClientError>
    where
        T: for<'de> Deserialize<'de>,
    {
        handle_response(
            self.add_auth_header(self.agent.post(self.format_url(path)))
                .send_empty()?,
        )
    }

    fn get<T>(&self, path: &str) -> Result<T, ApiClientError>
    where
        T: for<'de> Deserialize<'de>,
    {
        handle_response(
            self.add_auth_header(self.agent.get(self.format_url(path)))
                .call()?,
        )
    }

    pub fn provision(
        &self,
        send_body: &ProvisionDevicePayload,
    ) -> Result<DeviceKeyResponse, ApiClientError> {
        self.post("/devices", send_body)
    }

    pub fn rotate_key(&self, device_addr: DeviceAddr) -> Result<DeviceKeyResponse, ApiClientError> {
        self.post_empty(&format!("/devices/{device_addr}/rotate-key"))
    }

    /// The registry's view of one device, or `None` if it is not registered.
    pub fn device(&self, device_addr: DeviceAddr) -> Result<Option<DeviceSummary>, ApiClientError> {
        absent_if_not_registered(self.get(&format!("/devices/{device_addr}")))
    }

    /// Every registered device, in whatever order the API returns them.
    pub fn devices(&self) -> Result<Vec<DeviceSummary>, ApiClientError> {
        self.get("/devices")
    }
}

/// Maps the API's "no such device" to `None`.
///
/// ⚠️ Only `device_not_found` does. A 404 carrying any other code — `not_found`
/// from a route this API does not have — stays an error, so "this API cannot
/// answer" never reads as "this device is not registered", which `provision`
/// treats as leave to go ahead.
fn absent_if_not_registered<T>(
    result: Result<T, ApiClientError>,
) -> Result<Option<T>, ApiClientError> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(ApiClientError::Rejected { body, .. }) if body.code == ApiErrorCode::DeviceNotFound => {
            Ok(None)
        }
        Err(err) => Err(err),
    }
}

#[derive(Debug, Error)]
pub enum ApiClientError {
    #[error("api rejected request with status: {}, code: `{}`; message: `{}`", .status, .body.code, .body.message)]
    Rejected {
        status: StatusCode,
        body: ApiErrorBody,
    },

    #[error(transparent)]
    TransportError(#[from] ureq::Error),

    #[error("couldn't deserialize response body, status: {}, err: {}", .status, .error)]
    ResponseDeserializationError {
        status: StatusCode,
        error: ureq::Error,
    },
}

/// Turns a non-2xx into [`ApiClientError::Rejected`], carrying the API's own
/// `message` — which the API already redacts. Split out of [`handle_response`]
/// so [`ApiClient::check_auth`] can reuse the rejection path without needing a
/// body type to deserialize into.
fn reject_on_error_status(mut response: Response<Body>) -> Result<Response<Body>, ApiClientError> {
    let status = response.status();

    if !status.is_success() {
        let error_body = response
            .body_mut()
            .read_json::<ApiErrorBody>()
            .map_err(|error| ApiClientError::ResponseDeserializationError { status, error })?;

        return Err(ApiClientError::Rejected {
            status,
            body: error_body,
        });
    }

    Ok(response)
}

fn handle_response<T: for<'de> Deserialize<'de>>(
    response: Response<Body>,
) -> Result<T, ApiClientError> {
    let mut response = reject_on_error_status(response)?;
    let status = response.status();

    let parsed_body = response
        .body_mut()
        .read_json()
        .map_err(|error| ApiClientError::ResponseDeserializationError { status, error })?;

    Ok(parsed_body)
}

#[cfg(test)]
mod test {
    use super::*;

    fn rejected(status: StatusCode, code: ApiErrorCode) -> ApiClientError {
        ApiClientError::Rejected {
            status,
            body: ApiErrorBody::new(code, "m"),
        }
    }

    #[test]
    fn a_device_the_api_returns_is_some() {
        assert!(matches!(absent_if_not_registered::<u8>(Ok(7)), Ok(Some(7))));
    }

    #[test]
    fn device_not_found_is_none() {
        assert!(matches!(
            absent_if_not_registered::<u8>(Err(rejected(
                StatusCode::NOT_FOUND,
                ApiErrorCode::DeviceNotFound
            ))),
            Ok(None)
        ));
    }

    /// ⚠️ The case that must not collapse into `None`: a 404 for a route the
    /// API does not have. Read as "not registered", `provision` would take it as
    /// leave to go ahead.
    #[test]
    fn a_404_for_a_missing_route_stays_an_error() {
        assert!(matches!(
            absent_if_not_registered::<u8>(Err(rejected(
                StatusCode::NOT_FOUND,
                ApiErrorCode::NotFound
            ))),
            Err(ApiClientError::Rejected { .. })
        ));
    }

    #[test]
    fn other_rejections_stay_errors() {
        assert!(matches!(
            absent_if_not_registered::<u8>(Err(rejected(
                StatusCode::UNAUTHORIZED,
                ApiErrorCode::Unauthorized
            ))),
            Err(ApiClientError::Rejected { .. })
        ));
    }
}
