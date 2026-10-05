//! MQTT connection settings, read from the environment the same way by the
//! gateway and the API:
//!
//! | Variable                                | Default                         |
//! |-----------------------------------------|---------------------------------|
//! | `MQTT_HOST`                             | required                        |
//! | `MQTT_PORT`                             | `1883`                          |
//! | `MQTT_CLIENT_ID`                        | per binary, see [`MqttConfig::from_env`] |
//! | `MQTT_USERNAME` + `MQTT_PASSWORD_PATH`  | both or neither                 |
//!
//! The password is read from a file, never from the environment — the same
//! rule as the KEK and the admin token: an environment variable lands in
//! `/proc/<pid>/environ` and in `podman inspect`. In production the file is a
//! podman secret mounted under `/run/secrets`.

use std::{
    fmt, fs,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, bail};
use rumqttc::MqttOptions;

use crate::env::{env_var, env_var_opt, env_var_or};

#[derive(Debug)]
pub struct MqttConfig {
    pub host: String,
    pub port: u16,
    pub client_id: String,
    pub credentials: Option<MqttCredentials>,
}

impl MqttConfig {
    /// `default_client_id` is used unless `MQTT_CLIENT_ID` overrides it.
    ///
    /// A client id must be unique on its broker — a second connection under
    /// the same id disconnects the first, and the two then evict each other in
    /// a loop — and on a broker shared with other systems a bare `api` or
    /// `gateway` is an easy collision. Each binary therefore passes a
    /// `homescope-` name; the override exists for two gateways at one site.
    pub fn from_env(default_client_id: String) -> anyhow::Result<Self> {
        Ok(Self {
            host: env_var("MQTT_HOST")?,
            port: env_var_or("MQTT_PORT", 1883)?,
            client_id: env_var_or("MQTT_CLIENT_ID", default_client_id)?,
            credentials: MqttCredentials::resolve(
                env_var_opt("MQTT_USERNAME")?,
                env_var_opt("MQTT_PASSWORD_PATH")?,
            )?,
        })
    }

    /// Options with this config applied. Callers add what is theirs — the
    /// API's durable session, for one.
    pub fn options(&self) -> MqttOptions {
        let mut options = MqttOptions::new(&self.client_id, &self.host, self.port);

        if let Some(credentials) = &self.credentials {
            options.set_credentials(&credentials.username, &credentials.password);
        }

        options
    }
}

/// Not zeroized, deliberately: rumqttc keeps its own plain copy inside
/// `MqttOptions` for every reconnect, so wiping this one would protect nothing.
/// What this type does guarantee is that `Debug` never prints the password.
pub struct MqttCredentials {
    pub username: String,
    password: String,
}

impl fmt::Debug for MqttCredentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MqttCredentials")
            .field("username", &self.username)
            .field("password", &"<redacted>")
            .finish()
    }
}

impl MqttCredentials {
    /// Both variables or neither. Exactly one set is a misconfiguration —
    /// connecting anonymously instead would only surface later, as a broker
    /// refusing the connection or, with an ACL, as silence.
    fn resolve(
        username: Option<String>,
        password_path: Option<PathBuf>,
    ) -> anyhow::Result<Option<Self>> {
        match (username, password_path) {
            (None, None) => Ok(None),

            (Some(username), Some(path)) => {
                // Set-but-empty is a typo in an env file, not a request for
                // anonymous access; say so instead of failing on a blank path.
                if username.is_empty() {
                    bail!("MQTT_USERNAME is empty");
                }
                if path.as_os_str().is_empty() {
                    bail!("MQTT_PASSWORD_PATH is empty");
                }

                Ok(Some(Self {
                    username,
                    password: read_password(&path)?,
                }))
            }

            (Some(_), None) => bail!("MQTT_USERNAME is set but MQTT_PASSWORD_PATH is not"),
            (None, Some(_)) => bail!("MQTT_PASSWORD_PATH is set but MQTT_USERNAME is not"),
        }
    }
}

/// The first line, surrounding whitespace trimmed — the admin token's
/// convention, so a trailing newline from `echo` or `openssl rand` is harmless.
fn read_password(path: &Path) -> anyhow::Result<String> {
    let contents = fs::read_to_string(path)
        .with_context(|| format!("couldn't read MQTT password file: {}", path.display()))?;

    let password = contents.lines().next().unwrap_or_default().trim();
    if password.is_empty() {
        bail!("MQTT password file `{}` is empty", path.display());
    }

    Ok(password.to_owned())
}

#[cfg(test)]
mod test {
    use super::*;

    /// A file under the system temp dir, removed when dropped.
    struct TempFile(PathBuf);

    impl TempFile {
        fn new(name: &str, contents: &str) -> Self {
            let path = std::env::temp_dir()
                .join(format!("homescope-host-util-{}-{name}", std::process::id()));
            fs::write(&path, contents).expect("temp file is writable");

            Self(path)
        }
    }

    impl Drop for TempFile {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }

    #[test]
    fn neither_variable_means_anonymous() {
        assert!(MqttCredentials::resolve(None, None).unwrap().is_none());
    }

    #[test]
    fn one_variable_without_the_other_is_an_error() {
        let err = MqttCredentials::resolve(Some("homescope-api".into()), None).unwrap_err();
        assert!(err.to_string().contains("MQTT_PASSWORD_PATH"), "{err}");

        let err = MqttCredentials::resolve(None, Some("/run/secrets/mqtt".into())).unwrap_err();
        assert!(err.to_string().contains("MQTT_USERNAME"), "{err}");
    }

    #[test]
    fn reads_the_first_line_trimmed() {
        let file = TempFile::new("first-line", "  s3cret \nignored\n");

        let credentials =
            MqttCredentials::resolve(Some("homescope-api".into()), Some(file.0.clone()))
                .unwrap()
                .expect("credentials");

        assert_eq!(credentials.username, "homescope-api");
        assert_eq!(credentials.password, "s3cret");
    }

    #[test]
    fn an_empty_password_file_is_an_error() {
        let file = TempFile::new("empty", "\n");

        let err = MqttCredentials::resolve(Some("homescope-api".into()), Some(file.0.clone()))
            .unwrap_err();

        assert!(err.to_string().contains("is empty"), "{err}");
    }

    #[test]
    fn a_missing_password_file_names_its_path() {
        let path = PathBuf::from("/nonexistent/homescope-mqtt-password");

        let err = MqttCredentials::resolve(Some("homescope-api".into()), Some(path)).unwrap_err();

        assert!(
            err.to_string()
                .contains("/nonexistent/homescope-mqtt-password"),
            "{err}"
        );
    }

    #[test]
    fn an_empty_username_is_an_error() {
        let file = TempFile::new("empty-username", "s3cret\n");

        let err = MqttCredentials::resolve(Some(String::new()), Some(file.0.clone())).unwrap_err();

        assert!(err.to_string().contains("MQTT_USERNAME is empty"), "{err}");
    }

    #[test]
    fn an_empty_password_path_is_an_error() {
        let err = MqttCredentials::resolve(Some("homescope-api".into()), Some(PathBuf::new()))
            .unwrap_err();

        assert!(
            err.to_string().contains("MQTT_PASSWORD_PATH is empty"),
            "{err}"
        );
    }

    #[test]
    fn debug_never_prints_the_password() {
        let file = TempFile::new("debug", "s3cret\n");
        let credentials =
            MqttCredentials::resolve(Some("homescope-api".into()), Some(file.0.clone()))
                .unwrap()
                .expect("credentials");

        let debug = format!("{credentials:?}");

        assert!(!debug.contains("s3cret"), "{debug}");
        assert!(debug.contains("homescope-api"), "{debug}");
    }

    #[test]
    fn options_carry_the_client_id_and_credentials() {
        let file = TempFile::new("options", "s3cret\n");
        let config = MqttConfig {
            host: "broker".into(),
            port: 1884,
            client_id: "homescope-api".into(),
            credentials: MqttCredentials::resolve(
                Some("homescope-api".into()),
                Some(file.0.clone()),
            )
            .unwrap(),
        };

        let options = config.options();
        let login = options.credentials().expect("credentials applied");

        assert_eq!(options.client_id(), "homescope-api");
        assert_eq!(options.broker_address(), ("broker".into(), 1884));
        assert_eq!(
            (login.username.as_str(), login.password.as_str()),
            ("homescope-api", "s3cret")
        );
    }
}
