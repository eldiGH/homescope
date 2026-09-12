use std::io::{IsTerminal as _, Write as _, stderr, stdin};

use anyhow::{Context as _, bail};
use homescope_api_types::devices::ProvisionDevicePayload;
use homescope_common::{device_key::DeviceKey, uicr_record::RecordHeader};

use crate::{
    api_client::ApiClient,
    chip::{Chip, Connection},
    cli::ApiArgs,
    store::{ApiTarget, Credentials, Store, Token},
};

/// Profile name used when nothing else names one, so a first run is just
/// `login --api-url <URL>`.
const DEFAULT_PROFILE: &str = "default";

mod messages {
    pub fn device_locked_warning() -> &'static str {
        "The device is locked - please provision or reprovision it with --unlock flag."
    }

    pub fn device_already_unlocked() -> &'static str {
        "The device is already unlocked. Please omit --unlock flag."
    }
}

/// Reads the API token from stdin.
///
/// ⚠️ Deliberately never a flag. `/proc/<pid>/cmdline` is world-readable for as
/// long as the process runs, and the shell writes its history to disk —
/// `/proc/<pid>/environ` is 0400 owner-only, which is why `HOMESCOPE_TOKEN` is
/// the CI escape hatch and `--token` is not.
///
/// Echo is disabled when stdin is a terminal; a piped stdin is read plainly, so
/// `login < token.txt` and `pass show … | login` both work.
fn read_token() -> anyhow::Result<Token> {
    let raw = if stdin().is_terminal() {
        // Prompt on stderr, not stdout — stdout carries the one durable fact a
        // command produces, so it stays redirectable.
        write!(stderr(), "API token: ")?;
        stderr().flush()?;

        rpassword::read_password().context("couldn't read the token")?
    } else {
        let mut line = String::new();
        stdin()
            .read_line(&mut line)
            .context("couldn't read the token from stdin")?;

        line
    };

    let token = raw.trim().to_owned();

    if token.is_empty() {
        bail!("no token provided");
    }

    Ok(Token::new(token))
}

fn resolve_client(api: &ApiArgs) -> anyhow::Result<ApiClient> {
    let store = Store::load()?;

    Ok(ApiClient::new(
        store.resolve(api.profile.as_deref(), api.api_url.as_deref())?,
    ))
}

/// Picks the profile to act on when a command does not resolve one through
/// [`ApiArgs`]: an explicit name, else the configured default.
fn target_profile(store: &Store, profile: Option<String>) -> Option<String> {
    profile.or_else(|| store.config().default_profile.clone())
}

pub fn login(profile: Option<String>, api_url: Option<String>) -> anyhow::Result<()> {
    let mut store = Store::load()?;

    let profile = target_profile(&store, profile).unwrap_or_else(|| DEFAULT_PROFILE.to_owned());
    let url = store.login_url(&profile, api_url.as_deref())?;

    let token = read_token()?;

    // Verify before storing: a token typed wrong should fail here, not later
    // with a board in your hand and a key already minted.
    write!(stderr(), "Verifying against {url} … ")?;
    stderr().flush()?;

    let client = ApiClient::new(ApiTarget {
        label: profile.clone(),
        url: url.clone(),
        token: token.clone(),
    });

    if let Err(err) = client.check_auth() {
        writeln!(stderr(), "failed")?;
        return Err(err.into());
    }

    writeln!(stderr(), "ok")?;

    store.login(
        &profile,
        Some(&url),
        Credentials {
            token,
            subject: None,
            expires_at: None,
        },
    )?;

    writeln!(stderr(), "Saved credentials for `{profile}` ({url})")?;

    Ok(())
}

pub fn logout(profile: Option<String>) -> anyhow::Result<()> {
    let mut store = Store::load()?;

    let Some(profile) = target_profile(&store, profile) else {
        bail!("no profile selected; pass --profile <NAME>");
    };

    if store.logout(&profile)? {
        writeln!(stderr(), "Removed credentials for `{profile}`")?;
    } else {
        writeln!(stderr(), "No credentials stored for `{profile}`")?;
    }

    Ok(())
}

/// Reports which API a command would talk to, and whether its token still
/// works. Doubles as the pre-flight you run before walking to the bench.
///
/// It names a profile rather than a user because the API authenticates with a
/// single shared admin token today — there is no identity to report yet.
pub fn whoami(api: &ApiArgs) -> anyhow::Result<()> {
    let client = resolve_client(api)?;

    let status = match client.check_auth() {
        Ok(()) => "accepted",
        Err(err) => {
            println!("Profile  {}", client.label());
            println!("API      {}", client.url());
            println!("Token    rejected");

            return Err(err.into());
        }
    };

    println!("Profile  {}", client.label());
    println!("API      {}", client.url());
    println!("Token    {status}");

    Ok(())
}

pub fn info() -> anyhow::Result<()> {
    let mut chip = match Chip::connect()? {
        Connection::Locked(_) => {
            bail!(messages::device_locked_warning());
        }

        Connection::Attached(chip) => chip,
    };

    let state = chip.read_state()?;

    let key_status = match state.record {
        RecordHeader::Blank => "Blank - ready to provision",
        RecordHeader::Malformed(err) => &format!("Malformed - {err}"),
        RecordHeader::Present => "UICR header provisioned correclty, ready for reprovision",
    };

    println!("Device address: {}", state.device_addr);
    println!("Status: {key_status}");

    Ok(())
}

pub fn provision(api: &ApiArgs, unlock: bool, name: String) -> anyhow::Result<()> {
    let api_client = resolve_client(api)?;

    // ⚠️ Before `connect`, because --unlock erases the chip to make it
    // readable. A token that turns out to be wrong afterwards costs a board.
    api_client.check_auth()?;

    let mut chip = connect(unlock)?;
    chip.halt()?;

    let chip_state = chip.read_state()?;

    let send_body = ProvisionDevicePayload {
        name,
        device_addr: chip_state.device_addr,
    };

    let response = api_client.provision(&send_body)?;

    let key = DeviceKey::from_hex(&response.key)?;

    if !chip_state.record.is_blank() {
        chip.erase_uicr_record()?;
    }

    chip.write_uicr_record(key)?;
    chip.reset()?;

    println!(
        "Device {} ({}) successfully provisioned",
        response.name, response.device_addr
    );

    Ok(())
}

pub fn rotate_key(api: &ApiArgs, unlock: bool) -> anyhow::Result<()> {
    let api_client = resolve_client(api)?;

    api_client.check_auth()?;

    let mut chip = connect(unlock)?;
    chip.halt()?;

    let chip_state = chip.read_state()?;

    let response = api_client.rotate_key(chip_state.device_addr)?;

    let key = DeviceKey::from_hex(&response.key)?;
    if !chip_state.record.is_blank() {
        chip.erase_uicr_record()?;
    }

    chip.write_uicr_record(key)?;
    chip.reset()?;

    println!(
        "Key for device `{}` ({}) successfully rotated",
        response.name, response.device_addr
    );

    Ok(())
}

fn connect(unlock: bool) -> anyhow::Result<Box<Chip>> {
    let chip = match Chip::connect()? {
        Connection::Attached(chip) => {
            if unlock {
                bail!(messages::device_already_unlocked());
            }
            chip
        }

        Connection::Locked(locked_chip) => {
            if !unlock {
                bail!(messages::device_locked_warning());
            }
            locked_chip.erase_to_unlock()?
        }
    };

    Ok(chip)
}
