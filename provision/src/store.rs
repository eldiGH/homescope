//! On-disk state: which APIs we know about, and the credentials for them.
//!
//! Two files, deliberately separate, joined in memory:
//!
//! ```text
//! ~/.config/homescope/config.toml       0644  profiles (name → URL) + the default
//! ~/.config/homescope/credentials.toml  0600  one bearer token per profile
//!                                  │
//!                            Store (owns both)
//!                                  │
//!                            ApiTarget  ← what a command takes
//! ```
//!
//! Keeping them apart makes "is this file a secret" a property of the *path*:
//! `config.toml` is the one you can paste into an issue or sync into dotfiles,
//! and only `credentials.toml` gets the mode check on read.
//!
//! Commands never see a profile and a token separately — [`Store::resolve`]
//! either produces an [`ApiTarget`] with a non-optional token, or fails once
//! with a message naming the fix. Same reason `Device.key` is non-optional in
//! the API: an `Option` here would make five call sites re-ask one question.

use std::{
    collections::BTreeMap,
    env, fmt,
    fs::{self, DirBuilder, OpenOptions},
    io::{self, Write as _},
    os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _, PermissionsExt as _},
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use thiserror::Error;

const APP_DIR: &str = "homescope";
const CONFIG_FILE: &str = "config.toml";
const CREDENTIALS_FILE: &str = "credentials.toml";

const DIR_MODE: u32 = 0o700;
const CONFIG_MODE: u32 = 0o644;
const CREDENTIALS_MODE: u32 = 0o600;

/// The only supported way to pass a token without a stored profile.
///
/// `/proc/<pid>/cmdline` is world-readable and `/proc/<pid>/environ` is
/// 0400 owner-only, which is why this exists and `--token` does not.
const TOKEN_ENV: &str = "HOMESCOPE_TOKEN";

/// A bearer token for the Homescope API.
///
/// ⚠️ No `Display`, and `Debug` is redacted — `{token}` is a compile error and
/// `{token:?}` cannot leak. Same guardrail as `DeviceKey`, one tier weaker:
/// this is not zeroized, because a `String` reallocates on the way in and the
/// tool is short-lived. Do not add a `Display` impl; use [`Token::expose`] so
/// every place the secret escapes is greppable.
#[derive(Clone, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Token(String);

impl Token {
    pub fn new(raw: String) -> Self {
        Self(raw)
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Token(<REDACTED>)")
    }
}

/// One profile's entry in `credentials.toml`.
///
/// ⚠️ No `Debug`: this holds a bearer token.
#[derive(Clone, Serialize, Deserialize)]
pub struct Credentials {
    pub token: Token,

    /// Who the API said we are, cached for display only — never authorization.
    /// The API decides that, every time.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
}

/// `credentials.toml` in full.
///
/// `flatten` puts the profile names at the document root (`[dev]`, not
/// `[credentials.dev]`) while keeping the named type to hang load/save off.
///
/// This does *not* forfeit room to grow: serde matches declared fields first
/// and the flattened map takes only the remainder, so a future top-level key
/// is just a new field — declared **before** `profiles`, because TOML cannot
/// express a bare key after a table. A file written without it still loads,
/// and a stray scalar at the root fails loudly (`expected struct Credentials`)
/// rather than being absorbed as a profile.
///
/// ⚠️ The one real constraint: a profile may not share a name with a declared
/// field. That also fails loudly, so it costs an error message, not silence.
#[derive(Default, Serialize, Deserialize)]
pub struct CredentialsFile {
    #[serde(flatten)]
    profiles: BTreeMap<String, Credentials>,
}

/// One profile's entry in `config.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    pub api_url: String,
}

/// `config.toml` in full.
///
/// Flattened the same way as [`CredentialsFile`], so the two files read alike:
/// a profile is a `[name]` table in both, and only the *settings* carry a bare
/// key. See that type for why a top-level field is still available here.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct ConfigFile {
    /// ⚠️ Must stay declared *before* `profiles`: TOML cannot express a bare
    /// key after a table, and the serializer rejects the attempt.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_profile: Option<String>,

    #[serde(flatten)]
    pub profiles: BTreeMap<String, Profile>,
}

/// A resolved API to talk to. The token is not optional: by the time one of
/// these exists, the "are we logged in" question is already answered.
///
/// ⚠️ No `Debug`: this holds a bearer token.
pub struct ApiTarget {
    /// What confirmations print — the profile name, or the raw URL when
    /// `--api-url` was used. A human reads `prod` faster than a URL.
    pub label: String,
    pub url: String,
    pub token: Token,
}

pub struct Store {
    dir: PathBuf,
    config: ConfigFile,
    credentials: CredentialsFile,
}

impl Store {
    /// ⚠️ Neither file being absent is an error — a fresh machine must still be
    /// able to run `info`, which needs no network and no credentials.
    pub fn load() -> Result<Self, StoreError> {
        let dir = config_dir()?;

        Ok(Self {
            config: read_toml(&dir.join(CONFIG_FILE), false)?.unwrap_or_default(),
            credentials: read_toml(&dir.join(CREDENTIALS_FILE), true)?.unwrap_or_default(),
            dir,
        })
    }

    pub fn config(&self) -> &ConfigFile {
        &self.config
    }

    /// `--api-url` → `--profile` (or `HOMESCOPE_PROFILE`, via clap) → the
    /// configured default. There is deliberately no built-in fallback URL: a
    /// tool that silently defaults to localhost will one day silently not.
    pub fn resolve(
        &self,
        profile: Option<&str>,
        api_url: Option<&str>,
    ) -> Result<ApiTarget, StoreError> {
        if let Some(url) = api_url {
            // ⚠️ Never falls back to a stored token. Sending prod's credentials
            // to a URL typed by hand is the failure this whole scheme exists to
            // prevent, and "helpfully" reusing one is how you would cause it.
            let token = env::var(TOKEN_ENV)
                .ok()
                .filter(|token| !token.is_empty())
                .ok_or(StoreError::NoEnvToken)?;

            return Ok(ApiTarget {
                label: url.to_owned(),
                url: normalize_url(url),
                token: Token::new(token),
            });
        }

        let name = profile
            .or(self.config.default_profile.as_deref())
            .ok_or(StoreError::NoProfile)?;

        let configured = self
            .config
            .profiles
            .get(name)
            .ok_or_else(|| StoreError::UnknownProfile(name.to_owned()))?;

        let credentials = self
            .credentials
            .profiles
            .get(name)
            .ok_or_else(|| StoreError::NoCredentials(name.to_owned()))?;

        Ok(ApiTarget {
            label: name.to_owned(),
            url: configured.api_url.clone(),
            token: credentials.token.clone(),
        })
    }

    /// The URL `login` would store for this profile — an explicit `api_url`,
    /// else whatever the profile already points at.
    ///
    /// Exposed so `login` can verify a token against the right URL *before*
    /// writing anything, without having to re-derive it (or reach past the
    /// normalisation this applies).
    pub fn login_url(&self, profile: &str, api_url: Option<&str>) -> Result<String, StoreError> {
        match api_url {
            Some(url) => Ok(normalize_url(url)),
            None => Ok(self
                .config
                .profiles
                .get(profile)
                .ok_or_else(|| StoreError::UnknownProfile(profile.to_owned()))?
                .api_url
                .clone()),
        }
    }

    /// Records `credentials` for `profile`, creating the profile when
    /// `api_url` is given. The first profile saved becomes the default.
    pub fn login(
        &mut self,
        profile: &str,
        api_url: Option<&str>,
        credentials: Credentials,
    ) -> Result<(), StoreError> {
        let api_url = self.login_url(profile, api_url)?;

        self.config
            .profiles
            .insert(profile.to_owned(), Profile { api_url });

        if self.config.default_profile.is_none() {
            self.config.default_profile = Some(profile.to_owned());
        }

        self.credentials
            .profiles
            .insert(profile.to_owned(), credentials);

        // Config first: if the second write fails, the profile exists without
        // credentials and `resolve` says "run login", which is true. The other
        // order reports an unknown profile, which is not.
        self.save_config()?;
        self.save_credentials()
    }

    /// Drops the token but keeps the profile, so logging back in does not need
    /// the URL again. Returns whether there was anything to remove.
    pub fn logout(&mut self, profile: &str) -> Result<bool, StoreError> {
        if self.credentials.profiles.remove(profile).is_none() {
            return Ok(false);
        }

        self.save_credentials()?;

        Ok(true)
    }

    fn save_config(&self) -> Result<(), StoreError> {
        self.save(CONFIG_FILE, &self.config, CONFIG_MODE)
    }

    fn save_credentials(&self) -> Result<(), StoreError> {
        self.save(CREDENTIALS_FILE, &self.credentials, CREDENTIALS_MODE)
    }

    /// ⚠️ Serializes the *whole* file. Both maps are load-modify-save; writing
    /// a single entry would delete every other profile.
    fn save<T: Serialize>(&self, name: &str, value: &T, mode: u32) -> Result<(), StoreError> {
        ensure_dir(&self.dir)?;

        let contents = toml::to_string_pretty(value)?;

        write_atomic(&self.dir.join(name), &contents, mode)
    }
}

/// Paths are joined as `{url}{path}` with the path leading in `/`, so a stored
/// URL must not end in one. Normalising on the way *in* keeps every reader of
/// the file — and every printed confirmation — seeing the same string.
fn normalize_url(url: &str) -> String {
    url.trim_end_matches('/').to_owned()
}

/// XDG only — this is a Linux workstation CLI. If macOS or Windows ever
/// matter, swap in `etcetera`'s base strategy rather than growing this.
fn config_dir() -> Result<PathBuf, StoreError> {
    if let Some(dir) = env::var_os("XDG_CONFIG_HOME") {
        let path = PathBuf::from(dir);

        // The spec says to ignore the variable unless it holds an absolute
        // path — which covers the empty-string case too.
        if path.is_absolute() {
            return Ok(path.join(APP_DIR));
        }
    }

    let home = env::var_os("HOME").ok_or(StoreError::NoHome)?;

    Ok(PathBuf::from(home).join(".config").join(APP_DIR))
}

fn ensure_dir(dir: &Path) -> Result<(), StoreError> {
    DirBuilder::new()
        .recursive(true)
        .mode(DIR_MODE)
        .create(dir)
        .map_err(|source| StoreError::Write {
            path: dir.to_owned(),
            source,
        })
}

/// Refuse a credentials file anyone else can read. This is what `ssh` does
/// with private keys, and refusing rather than warning is why people notice a
/// bad umask instead of scrolling past it.
fn check_private(path: &Path, metadata: &fs::Metadata) -> Result<(), StoreError> {
    let mode = metadata.permissions().mode() & 0o777;

    if mode & 0o077 != 0 {
        return Err(StoreError::Permissions {
            path: path.to_owned(),
            mode,
        });
    }

    Ok(())
}

/// `Ok(None)` means the file is absent, which is never an error here — only a
/// malformed file is.
fn read_toml<T: DeserializeOwned>(path: &Path, private: bool) -> Result<Option<T>, StoreError> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(StoreError::Read {
                path: path.to_owned(),
                source,
            });
        }
    };

    if private {
        check_private(path, &metadata)?;
    }

    let contents = fs::read_to_string(path).map_err(|source| StoreError::Read {
        path: path.to_owned(),
        source,
    })?;

    toml::from_str(&contents)
        .map(Some)
        .map_err(|source| StoreError::Parse {
            path: path.to_owned(),
            source,
        })
}

/// Write via a temp file and a rename, so a reader sees the old file or the
/// new one and never a half-written one.
///
/// Four details carry the weight:
///
/// - **`create_new`** — `OpenOptions::mode` is applied *only* when `open`
///   actually creates the file, so writing in place over an existing 0644 file
///   would silently leave it 0644. It also refuses to follow a planted symlink.
/// - **`write_all`, not `write`** — a short write is legal and `write` reports
///   it in a return value rather than an error, so the plain call can truncate
///   the file silently.
/// - **`sync_all` before the rename** — rename orders the name change, not the
///   data blocks. Without the fsync a power cut can leave a zero-length file.
/// - **the temp lives beside the target** — `rename` is only atomic within one
///   filesystem, and the target's directory is already 0700.
fn write_atomic(path: &Path, contents: &str, mode: u32) -> Result<(), StoreError> {
    let tmp = {
        let mut name = path
            .file_name()
            .ok_or_else(|| StoreError::InvalidFilename(path.to_owned()))?
            .to_os_string();

        // OsString, not format!, so a non-UTF-8 path is not mangled on the way
        // through. The pid keeps two concurrent runs off each other's temp.
        name.push(format!(".tmp.{}", std::process::id()));

        path.with_file_name(name)
    };

    // A run killed between create and rename leaves one behind, and
    // `create_new` would then fail forever on a name nobody knows about.
    let _ = fs::remove_file(&tmp);

    let written = (|| -> io::Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .open(&tmp)?;

        file.write_all(contents.as_bytes())?;
        file.sync_all()?;
        drop(file);

        fs::rename(&tmp, path)
    })();

    if written.is_err() {
        let _ = fs::remove_file(&tmp);
    }

    written.map_err(|source| StoreError::Write {
        path: path.to_owned(),
        source,
    })
}

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("cannot locate the config directory: neither XDG_CONFIG_HOME nor HOME is set")]
    NoHome,

    #[error("{} is readable by other users (mode {mode:04o})\n\nrun: chmod 600 {}", .path.display(), .path.display())]
    Permissions { path: PathBuf, mode: u32 },

    #[error("could not read {}: {source}", .path.display())]
    Read { path: PathBuf, source: io::Error },

    #[error("could not write {}: {source}", .path.display())]
    Write { path: PathBuf, source: io::Error },

    #[error("{} is malformed: {source}", .path.display())]
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },

    #[error("{} is not a file path", .0.display())]
    InvalidFilename(PathBuf),

    #[error(transparent)]
    Serialize(#[from] toml::ser::Error),

    #[error(
        "no profile selected\n\npass --profile <NAME>, or run `homescope-provision login` to set a default"
    )]
    NoProfile,

    #[error(
        "unknown profile `{0}`\n\nrun: homescope-provision login --profile {0} --api-url <URL>"
    )]
    UnknownProfile(String),

    #[error("no credentials for profile `{0}`\n\nrun: homescope-provision login --profile {0}")]
    NoCredentials(String),

    #[error(
        "--api-url requires the token in {TOKEN_ENV}\n\nstored profile credentials are never reused for a URL passed on the command line"
    )]
    NoEnvToken,
}
