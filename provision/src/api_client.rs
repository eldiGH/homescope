use homescope_api_types::{
    devices::{DeviceKeyResponse, ProvisionDevicePayload},
    error::ApiErrorBody,
};
use homescope_common::device_addr::DeviceAddr;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use ureq::{
    Agent, Body, RequestBuilder,
    http::{Response, StatusCode},
};

use crate::store::ApiTarget;

pub struct ApiClient {
    agent: Agent,
    target: ApiTarget,
}

impl ApiClient {
    pub fn new(target: ApiTarget) -> Self {
        let config = Agent::config_builder().http_status_as_error(false).build();
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
    /// ⚠️ Worth running before anything destructive. On the `--unlock` path the
    /// chip erase happens *before* the tool has ever spoken to the API, so an
    /// unverified token costs a board.
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

    #[allow(dead_code)]
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
