use std::io::{IsTerminal as _, Write as _, stderr, stdin};

use anyhow::{Context as _, bail};
use homescope_api_types::devices::{DeviceKeyResponse, ProvisionDevicePayload};
use homescope_common::{device_key::DeviceKey, uicr_record::RecordHeader};
use zeroize::Zeroize as _;

use crate::{
    api_client::ApiClient,
    chip::{self, Chip, Connection},
    cli::{ApiArgs, ConfirmArgs},
    confirm::{self, ConfirmError},
    output,
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

/// ⚠️ Never prompts and never refuses. A locked chip is a *state* to report,
/// not a failure — and the old message told the reader to pass `--unlock`, a
/// flag `info` does not have.
pub fn info() -> anyhow::Result<()> {
    match Chip::connect()? {
        Connection::Locked(locked) => {
            output::identity_locked(&locked.probe_description(), chip::TARGET);
        }

        Connection::Attached(mut chip) => {
            let state = chip.read_state()?;
            output::identity(&chip.probe_description(), chip::TARGET, &state);

            println!("{}", state.device_addr);
        }
    }

    Ok(())
}

pub fn provision(
    api: &ApiArgs,
    confirm_args: &ConfirmArgs,
    unlock: bool,
    name: String,
) -> anyhow::Result<()> {
    let api_client = resolve_client(api)?;

    // ⚠️ Before `connect`, because --unlock erases the chip to make it
    // readable. A token that turns out to be wrong afterwards costs a board.
    output::step("Checking credentials", || api_client.check_auth())?;

    let mut chip = connect(unlock, confirm_args.yes)?;
    chip.halt()?;

    let state = chip.read_state()?;
    output::identity(&chip.probe_description(), chip::TARGET, &state);

    confirm_record(&state.record, Action::Provision, confirm_args.yes)?;

    let send_body = ProvisionDevicePayload {
        name,
        device_addr: state.device_addr,
    };

    // ⚠️ Confirm *before* the mint, not before the erase. The mint is
    // destructive to the registry — it invalidates the running sensor's key the
    // moment it returns — so confirming after it asks a question whose answer
    // can no longer change anything.
    let mut response = output::step(&format!("Registering {:?}", send_body.name), || {
        api_client.provision(&send_body)
    })?;

    install_key(&mut chip, &state.record, &mut response, "provision")?;

    println!("{}\t{}", response.device_addr, response.name);
    output::outcome(&format!(
        "Provisioned {:?} as {}",
        response.name, response.device_addr
    ));

    Ok(())
}

pub fn rotate_key(api: &ApiArgs, confirm_args: &ConfirmArgs, unlock: bool) -> anyhow::Result<()> {
    let api_client = resolve_client(api)?;

    output::step("Checking credentials", || api_client.check_auth())?;

    let mut chip = connect(unlock, confirm_args.yes)?;
    chip.halt()?;

    let state = chip.read_state()?;
    output::identity(&chip.probe_description(), chip::TARGET, &state);

    confirm_record(&state.record, Action::Rotate, confirm_args.yes)?;

    let mut response = output::step("Requesting a new key", || {
        api_client.rotate_key(state.device_addr)
    })?;

    install_key(&mut chip, &state.record, &mut response, "rotate")?;

    println!("{}\t{}", response.device_addr, response.name);
    output::outcome(&format!(
        "Rotated the key for {:?} ({})",
        response.name, response.device_addr
    ));

    Ok(())
}

enum Action {
    Provision,
    Rotate,
}

/// The chip half of the preconditions.
///
/// ⚠️ The chip record and the registry row are **different facts**, and they
/// come apart in exactly the cases a precondition exists for — a chip-erased
/// deployed sensor reads `Blank` while its row is present, a board from another
/// deployment reads `Present` with no row at all. So the API owns the
/// *refusals* (it already raises 409 and 404 before storing anything) and the
/// chip owns only the *confirmations*. Guessing at fleet membership from the
/// record header would be wrong in two of the five cases.
///
/// ⚠️ In particular `rotate` must **not** refuse on `Blank`: that is the
/// post-chip-erase recovery path. The row exists, the device needs a new key,
/// and there is nothing on the chip to see. Refuse here and that board becomes
/// unprovisionable, because `provision` will 409 on the row.
fn confirm_record(
    record: &RecordHeader,
    action: Action,
    assume_yes: bool,
) -> Result<(), ConfirmError> {
    let question = match (action, record) {
        // Nothing to destroy. The happy path stays prompt-free.
        (_, RecordHeader::Blank) => return Ok(()),

        (_, RecordHeader::Malformed(err)) => format!(
            "This board holds a record this tool cannot read ({err}).\n\
             It may be someone else's data, or a newer tool's record. Overwrite it?"
        ),

        (Action::Provision, RecordHeader::Present) => {
            "This board already holds a key, which provisioning destroys.".to_owned()
        }

        (Action::Rotate, RecordHeader::Present) => {
            "This board is reporting under its current key.\n\
             It goes dark from the moment the new key is issued until the write lands."
                .to_owned()
        }
    };

    confirm::yes_no(&question, assume_yes)
}

/// Everything after the mint.
///
/// ⚠️ A failure anywhere in here leaves the device **dark**, and the issued key
/// is not recoverable — there is no resume, only a fresh mint. That is why it
/// gets its own message rather than surfacing as a generic error.
fn install_key(
    chip: &mut Chip,
    record: &RecordHeader,
    response: &mut DeviceKeyResponse,
    retry_command: &str,
) -> anyhow::Result<()> {
    // Decode, then wipe the hex before anything else can fail — §2 asks that
    // the plaintext key cross an interface exactly once. Note the zeroize runs
    // before the `?`, so the error path does not skip it.
    let key = DeviceKey::from_hex(&response.key);
    response.key.zeroize();
    let key = key?;

    let installed = (|| -> anyhow::Result<()> {
        if !record.is_blank() {
            output::step("Erasing UICR record", || chip.erase_uicr_record())?;
        }

        // `write_uicr_record` reads the record back and compares, so this step
        // covers §0's verify. ⚠️ It must happen before any flashing: a bad key
        // under good firmware fails silently, where good firmware with no key
        // announces itself over RTT.
        output::step("Writing and verifying UICR record", || {
            chip.write_uicr_record(key)
        })?;

        output::step("Resetting", || chip.reset())?;

        Ok(())
    })();

    installed.with_context(|| {
        format!(
            "the UICR write failed after the API issued a new key\n\n  \
             {} ({:?}) will not report until it is re-keyed.\n  \
             Re-run `homescope-provision {retry_command}` — the issued key is\n  \
             not recoverable and a new one will be minted.",
            response.device_addr, response.name,
        )
    })
}

fn connect(unlock: bool, assume_yes: bool) -> anyhow::Result<Box<Chip>> {
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

            // ⚠️ This is the one confirmation that cannot name what it destroys:
            // APPROTECT means the address is unreadable until *after* the erase.
            // Blind consent gets a typed word rather than a keystroke.
            //
            // It also runs before the tool has any device-specific knowledge,
            // which is why `check_auth` already ran — otherwise a wrong token
            // would be discovered with the board already blank.
            output::identity_locked(&locked_chip.probe_description(), chip::TARGET);

            confirm::typed(
                "Unlocking erases the entire chip — firmware, key and seq counter.\n\
                 This board cannot be identified until after the erase.",
                "ERASE",
                assume_yes,
            )?;

            output::step("Erasing chip to unlock", || locked_chip.erase_to_unlock())?
        }
    };

    Ok(chip)
}
