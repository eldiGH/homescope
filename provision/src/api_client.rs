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

pub struct ApiClient {
    agent: Agent,
    base_url: String,
    auth_token: String,
}

impl ApiClient {
    pub fn new(auth_token: String, base_url: String) -> Self {
        let config = Agent::config_builder().http_status_as_error(false).build();
        Self {
            agent: Agent::new_with_config(config),
            base_url,
            auth_token,
        }
    }

    fn format_url(&self, url: &str) -> String {
        format!("{}{}", self.base_url, url)
    }

    fn add_auth_header<T>(&self, req: RequestBuilder<T>) -> RequestBuilder<T> {
        req.header("Authorization", format!("Bearer {}", self.auth_token))
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

fn handle_response<T: for<'de> Deserialize<'de>>(
    mut response: Response<Body>,
) -> Result<T, ApiClientError> {
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

    let parsed_body = response
        .body_mut()
        .read_json()
        .map_err(|error| ApiClientError::ResponseDeserializationError { status, error })?;

    Ok(parsed_body)
}
