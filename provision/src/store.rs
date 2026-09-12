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
    env,
    ffi::OsStr,
    fmt,
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
/// and the flattened map takes only the remainder, so a future top-level key is
/// just a new field. A file written without it still loads, and a stray scalar
/// at the root fails loudly (`expected struct Credentials`) rather than being
/// absorbed as a profile.
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
    /// Declared before `profiles` to match the emitted order. A bare key after
    /// a table header would belong to that table, so settings have to come
    /// first in the file — but `toml` 1.x sorts scalars ahead of tables for
    /// you, so this is house style rather than a correctness requirement.
    /// Verified: swapping these two fields still emits valid TOML.
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
        Self::at(config_dir()?)
    }

    /// Loads from an explicit directory. Splitting this from [`load`](Self::load)
    /// keeps the tests off the process environment, which they could not mutate
    /// safely anyway — they run in parallel threads of one process.
    fn at(dir: PathBuf) -> Result<Self, StoreError> {
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
            // ⚠️ Returns here without ever consulting `self`. That is the
            // no-fallback guarantee: sending prod's stored credentials to a URL
            // typed by hand is the failure this whole scheme exists to prevent,
            // and "helpfully" reusing one is how you would cause it.
            return direct_target(url, env::var(TOKEN_ENV).ok());
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

/// The `--api-url` branch of [`Store::resolve`], with the environment lookup
/// lifted out so the no-fallback rule is testable.
fn direct_target(url: &str, env_token: Option<String>) -> Result<ApiTarget, StoreError> {
    let token = env_token
        .filter(|token| !token.is_empty())
        .ok_or(StoreError::NoEnvToken)?;

    Ok(ApiTarget {
        label: url.to_owned(),
        url: normalize_url(url),
        token: Token::new(token),
    })
}

/// XDG only — this is a Linux workstation CLI. If macOS or Windows ever
/// matter, swap in `etcetera`'s base strategy rather than growing this.
fn config_dir() -> Result<PathBuf, StoreError> {
    config_dir_from(
        env::var_os("XDG_CONFIG_HOME").as_deref(),
        env::var_os("HOME").as_deref(),
    )
}

fn config_dir_from(
    xdg_config_home: Option<&OsStr>,
    home: Option<&OsStr>,
) -> Result<PathBuf, StoreError> {
    if let Some(dir) = xdg_config_home {
        let path = Path::new(dir);

        // The spec says to ignore the variable unless it holds an absolute
        // path — which covers the empty-string case too.
        if path.is_absolute() {
            return Ok(path.join(APP_DIR));
        }
    }

    let home = home.ok_or(StoreError::NoHome)?;

    Ok(Path::new(home).join(".config").join(APP_DIR))
}

/// Creates the config directory at 0700.
///
/// ⚠️ An *existing* directory keeps whatever mode it has — `DirBuilder` only
/// applies the mode to directories it creates. That is deliberate: the
/// credentials file is 0600 either way, so a loose directory leaks filenames
/// and nothing else, and silently chmod'ing a path the user already owns is a
/// bigger surprise than the leak is a risk.
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

#[cfg(test)]
mod test {
    use std::ffi::OsString;

    use tempfile::TempDir;

    use super::*;

    fn credentials(token: &str) -> Credentials {
        Credentials {
            token: Token::new(token.to_owned()),
            subject: None,
            expires_at: None,
        }
    }

    fn mode_of(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    /// `unwrap_err` requires `T: Debug`, and neither `Store` nor `ApiTarget`
    /// has one — deliberately, since both carry a bearer token. So the tests
    /// get the same treatment as everything else that handles one.
    fn expect_err<T>(result: Result<T, StoreError>) -> StoreError {
        match result {
            Ok(_) => panic!("expected an error"),
            Err(err) => err,
        }
    }

    /// Mirrors production: `~/.config` already exists, `~/.config/homescope` is
    /// ours to create. Several tests care about the mode the tool picks for it,
    /// which it only gets to pick when it does the creating.
    fn config_dir(tmp: &TempDir) -> PathBuf {
        tmp.path().join(APP_DIR)
    }

    fn store_at(tmp: &TempDir) -> Result<Store, StoreError> {
        Store::at(config_dir(tmp))
    }

    /// Writes a file into the config directory as if an earlier run had.
    fn seed(tmp: &TempDir, file: &str, contents: &str) -> PathBuf {
        let dir = config_dir(tmp);
        fs::create_dir_all(&dir).unwrap();

        let path = dir.join(file);
        fs::write(&path, contents).unwrap();

        path
    }

    /// A store with one logged-in profile, ready to act on.
    fn logged_in(tmp: &TempDir) -> Store {
        let mut store = store_at(tmp).unwrap();

        store
            .login("dev", Some("http://localhost:8080"), credentials("tok-dev"))
            .unwrap();

        store
    }

    // ---- URL normalisation ------------------------------------------------

    #[test]
    fn a_trailing_slash_is_stripped_from_a_stored_url() {
        // Paths are joined as `{url}{/path}`, so a stored slash means `//devices`.
        assert_eq!(normalize_url("http://host:8080/"), "http://host:8080");
        assert_eq!(normalize_url("http://host:8080///"), "http://host:8080");
        assert_eq!(normalize_url("http://host:8080"), "http://host:8080");
    }

    // ---- XDG resolution ---------------------------------------------------

    #[test]
    fn an_absolute_xdg_config_home_wins() {
        let dir = config_dir_from(Some(OsStr::new("/xdg")), Some(OsStr::new("/home/e"))).unwrap();

        assert_eq!(dir, Path::new("/xdg/homescope"));
    }

    /// The spec says to ignore the variable unless it is an absolute path, and
    /// an empty string is the case that actually shows up in the wild.
    #[test]
    fn an_empty_or_relative_xdg_config_home_is_ignored() {
        for ignored in ["", "relative/path", "./here"] {
            let dir =
                config_dir_from(Some(OsStr::new(ignored)), Some(OsStr::new("/home/e"))).unwrap();

            assert_eq!(dir, Path::new("/home/e/.config/homescope"), "{ignored:?}");
        }
    }

    #[test]
    fn with_neither_variable_there_is_nowhere_to_look() {
        assert!(matches!(
            config_dir_from(None, None),
            Err(StoreError::NoHome)
        ));
    }

    // ---- reading ----------------------------------------------------------

    /// ⚠️ A fresh machine must still run `info`, which needs no credentials.
    #[test]
    fn absent_files_load_as_defaults_rather_than_failing() {
        let dir = TempDir::new().unwrap();
        let store = store_at(&dir).unwrap();

        assert!(store.config().default_profile.is_none());
        assert!(store.config().profiles.is_empty());
    }

    #[test]
    fn a_malformed_file_is_an_error_that_names_it() {
        let dir = TempDir::new().unwrap();
        seed(&dir, CONFIG_FILE, "this is not = = toml");

        let err = expect_err(store_at(&dir));

        assert!(matches!(&err, StoreError::Parse { .. }));
        assert!(err.to_string().contains(CONFIG_FILE));
    }

    /// What `ssh` does with private keys, and for the same reason: a warning
    /// scrolls away, a refusal gets fixed.
    #[test]
    fn group_or_world_readable_credentials_are_refused() {
        let dir = TempDir::new().unwrap();
        let path = seed(&dir, CREDENTIALS_FILE, "[dev]\ntoken = \"t\"\n");

        for loosened in [0o640, 0o644, 0o604, 0o666] {
            fs::set_permissions(&path, fs::Permissions::from_mode(loosened)).unwrap();

            let err = expect_err(store_at(&dir));

            assert!(
                matches!(&err, StoreError::Permissions { .. }),
                "mode {loosened:o} should be refused"
            );
            assert!(err.to_string().contains("chmod 600"));
        }
    }

    /// Only the credentials file is checked — 0644 is correct for config.
    #[test]
    fn a_world_readable_config_file_is_fine() {
        let dir = TempDir::new().unwrap();
        let path = seed(&dir, CONFIG_FILE, "default_profile = \"dev\"\n");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();

        let store = store_at(&dir).unwrap();

        assert_eq!(store.config().default_profile.as_deref(), Some("dev"));
    }

    // ---- writing ----------------------------------------------------------

    #[test]
    fn each_file_is_written_with_its_own_mode() {
        let dir = TempDir::new().unwrap();
        logged_in(&dir);

        assert_eq!(
            mode_of(&config_dir(&dir).join(CREDENTIALS_FILE)),
            CREDENTIALS_MODE
        );
        assert_eq!(mode_of(&config_dir(&dir).join(CONFIG_FILE)), CONFIG_MODE);
        assert_eq!(mode_of(&config_dir(&dir)), DIR_MODE);
    }

    /// ⚠️ The bug that motivated the temp-and-rename: `OpenOptions::mode` is
    /// applied only when `open` actually creates the file, so writing in place
    /// over a loosened file would silently leave it loosened.
    #[test]
    fn writing_over_a_loosened_file_restores_its_mode() {
        let dir = TempDir::new().unwrap();
        let mut store = logged_in(&dir);
        let path = config_dir(&dir).join(CREDENTIALS_FILE);

        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        store.login("dev", None, credentials("tok-2")).unwrap();

        assert_eq!(mode_of(&path), CREDENTIALS_MODE);
    }

    #[test]
    fn no_temp_file_survives_a_write() {
        let dir = TempDir::new().unwrap();
        logged_in(&dir);

        let leftovers: Vec<_> = fs::read_dir(config_dir(&dir))
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .filter(|name| name.to_string_lossy().contains(".tmp"))
            .collect();

        assert!(leftovers.is_empty(), "left behind: {leftovers:?}");
    }

    /// ⚠️ A run killed between create and rename leaves a temp behind, and
    /// `create_new` would then fail forever on a name nobody knows about.
    #[test]
    fn a_stale_temp_file_does_not_wedge_the_next_write() {
        let dir = TempDir::new().unwrap();
        let target = config_dir(&dir).join(CONFIG_FILE);

        let mut stale = OsString::from(CONFIG_FILE);
        stale.push(format!(".tmp.{}", std::process::id()));
        seed(&dir, stale.to_str().unwrap(), "junk from a killed run");

        write_atomic(&target, "default_profile = \"dev\"\n", CONFIG_MODE).unwrap();

        assert!(fs::read_to_string(&target).unwrap().contains("dev"));
        assert!(!config_dir(&dir).join(&stale).exists());
    }

    // ---- load-modify-save -------------------------------------------------

    /// ⚠️ `save` serializes the whole file, so writing one profile's entry
    /// without re-reading the rest would delete every other profile.
    #[test]
    fn logging_in_again_preserves_the_other_profiles() {
        let dir = TempDir::new().unwrap();
        let mut store = logged_in(&dir);

        store
            .login("prod", Some("https://pi.local"), credentials("tok-prod"))
            .unwrap();

        // A fresh process, reading what was actually written to disk.
        let mut reopened = store_at(&dir).unwrap();
        reopened
            .login("dev", None, credentials("tok-dev-rotated"))
            .unwrap();

        let reopened = store_at(&dir).unwrap();

        assert_eq!(
            reopened.resolve(Some("prod"), None).unwrap().token.expose(),
            "tok-prod"
        );
        assert_eq!(
            reopened.resolve(Some("dev"), None).unwrap().token.expose(),
            "tok-dev-rotated"
        );
    }

    #[test]
    fn the_first_profile_saved_becomes_the_default() {
        let dir = TempDir::new().unwrap();
        let mut store = logged_in(&dir);

        store
            .login("prod", Some("https://pi.local"), credentials("tok-prod"))
            .unwrap();

        assert_eq!(store.config().default_profile.as_deref(), Some("dev"));
    }

    /// The profile survives so logging back in does not need the URL again.
    #[test]
    fn logout_drops_the_token_and_keeps_the_profile() {
        let dir = TempDir::new().unwrap();
        let mut store = logged_in(&dir);

        assert!(store.logout("dev").unwrap());

        let reopened = store_at(&dir).unwrap();

        assert!(reopened.config().profiles.contains_key("dev"));
        assert!(matches!(
            reopened.resolve(Some("dev"), None),
            Err(StoreError::NoCredentials(_))
        ));
    }

    #[test]
    fn logout_reports_when_there_was_nothing_to_remove() {
        let dir = TempDir::new().unwrap();
        let mut store = logged_in(&dir);

        assert!(!store.logout("never-logged-in").unwrap());
    }

    // ---- resolution -------------------------------------------------------

    #[test]
    fn resolve_falls_back_to_the_default_profile() {
        let dir = TempDir::new().unwrap();
        let store = logged_in(&dir);

        let target = store.resolve(None, None).unwrap();

        assert_eq!(target.label, "dev");
        assert_eq!(target.url, "http://localhost:8080");
        assert_eq!(target.token.expose(), "tok-dev");
    }

    #[test]
    fn resolve_names_the_fix_for_each_way_it_can_fail() {
        let dir = TempDir::new().unwrap();
        let mut store = logged_in(&dir);

        let unknown = expect_err(store.resolve(Some("nope"), None));
        assert!(matches!(&unknown, StoreError::UnknownProfile(_)));
        assert!(unknown.to_string().contains("--api-url"));

        store.logout("dev").unwrap();
        let no_credentials = expect_err(store.resolve(Some("dev"), None));
        assert!(matches!(&no_credentials, StoreError::NoCredentials(_)));
        assert!(no_credentials.to_string().contains("login"));

        let empty = store_at(&TempDir::new().unwrap()).unwrap();
        assert!(matches!(
            empty.resolve(None, None),
            Err(StoreError::NoProfile)
        ));
    }

    /// ⚠️ The whole point of `--api-url`: an explicit URL must never pick up a
    /// stored token. `resolve` returns before it can consult the store, so a
    /// missing `HOMESCOPE_TOKEN` is a refusal, not a fallback.
    #[test]
    fn an_explicit_url_without_an_env_token_is_refused_not_substituted() {
        let err = expect_err(direct_target("http://typed-by-hand", None));

        assert!(matches!(&err, StoreError::NoEnvToken));
        assert!(err.to_string().contains(TOKEN_ENV));
    }

    #[test]
    fn an_empty_env_token_counts_as_unset() {
        assert!(matches!(
            direct_target("http://host", Some(String::new())),
            Err(StoreError::NoEnvToken)
        ));
    }

    #[test]
    fn an_explicit_url_is_labelled_by_itself_and_normalised() {
        let target = direct_target("http://host:8080/", Some("env-token".to_owned())).unwrap();

        assert_eq!(target.label, "http://host:8080/");
        assert_eq!(target.url, "http://host:8080");
        assert_eq!(target.token.expose(), "env-token");
    }

    // ---- on-disk shape ----------------------------------------------------

    /// Both files read alike: a profile is a `[name]` table, never nested under
    /// the field that holds it.
    #[test]
    fn profiles_are_not_prefixed_in_either_file() {
        let dir = TempDir::new().unwrap();
        logged_in(&dir);

        let config = fs::read_to_string(config_dir(&dir).join(CONFIG_FILE)).unwrap();
        let creds = fs::read_to_string(config_dir(&dir).join(CREDENTIALS_FILE)).unwrap();

        assert!(config.contains("[dev]"), "{config}");
        assert!(!config.contains("[profiles"), "{config}");
        assert!(creds.contains("[dev]"), "{creds}");
        assert!(!creds.contains("[credentials"), "{creds}");
    }

    /// A bare key after a table header would belong to that table, so settings
    /// have to reach the file before the profiles do.
    ///
    /// This pins the emitted shape, not the field order: `toml` 1.x sorts
    /// scalars ahead of tables regardless of declaration order (verified by
    /// swapping them). It would catch a serializer swap that stopped doing so.
    #[test]
    fn settings_serialize_before_the_profile_tables() {
        let dir = TempDir::new().unwrap();
        logged_in(&dir);

        let config = fs::read_to_string(config_dir(&dir).join(CONFIG_FILE)).unwrap();

        let setting = config.find("default_profile").expect("no default_profile");
        let table = config.find("[dev]").expect("no [dev] table");

        assert!(setting < table, "settings must come first:\n{config}");
    }

    /// The one real cost of flattening, recorded so it is a known constraint
    /// rather than a surprise: the failure is loud, not silent.
    #[test]
    fn a_profile_colliding_with_a_settings_key_is_rejected_loudly() {
        let dir = TempDir::new().unwrap();
        seed(
            &dir,
            CONFIG_FILE,
            "[default_profile]\napi_url = \"http://host\"\n",
        );

        assert!(matches!(store_at(&dir), Err(StoreError::Parse { .. })));
    }
}
