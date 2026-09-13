use std::{
    io::{IsTerminal as _, Write as _, stderr, stdin, stdout},
    time::Duration,
};

use anyhow::{Context as _, anyhow, bail};
use chrono::{SecondsFormat, Utc};
use homescope_api_types::devices::{DeviceKeyResponse, ProvisionDevicePayload};
use homescope_common::{device_addr::DeviceAddr, device_key::DeviceKey, uicr_record::RecordHeader};
use zeroize::Zeroize as _;

use crate::{
    api_client::ApiClient,
    chip::{self, Chip, Connection},
    cli::{ApiArgs, ConfirmArgs},
    confirm::{self, ConfirmError},
    output,
    store::{ApiTarget, Credentials, Store, Token},
    verify::{self, Progress, WaitError},
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

/// The fleet as the API sees it: a table on a terminal, bare tab-separated rows
/// into a pipe. The count and the API it came from go to stderr, so they never
/// become a row a script has to skip.
pub fn list(api: &ApiArgs) -> anyhow::Result<()> {
    let client = resolve_client(api)?;
    let devices = client.devices()?;
    let for_terminal = stdout().is_terminal();

    if for_terminal {
        let plural = if devices.len() == 1 { "" } else { "s" };
        writeln!(
            stderr(),
            "{} device{plural} on {}\n",
            devices.len(),
            client.label()
        )?;

        if devices.is_empty() {
            return Ok(());
        }
    }

    print!(
        "{}",
        output::fleet_listing(&devices, Utc::now(), for_terminal)
    );

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
    check_auth_before_unlock(&api_client, unlock)?;

    let mut chip = connect(unlock, confirm_args.yes)?;

    let state = chip.read_state()?;
    output::identity(&chip.probe_description(), chip::TARGET, &state);

    let registered = api_client.device(state.device_addr)?;
    output::fleet(api_client.label(), registered.as_ref(), Utc::now());

    // The API would refuse this with a 409. Asking first means refusing before
    // a prompt, a halt or a mint — but the POST stays the authority: the
    // registry can change between the two requests, and it still 409s.
    if let Some(existing) = registered {
        bail!(
            "{} is already registered as {:?} — rotate its key instead: \
             `homescope-provision rotate`",
            state.device_addr,
            existing.name
        );
    }

    confirm_record(&state.record, Action::Provision, confirm_args.yes)?;
    chip.halt()?;

    let send_body = ProvisionDevicePayload {
        name,
        device_addr: state.device_addr,
    };

    // ⚠️ Confirm *before* the mint, not before the erase. The mint is
    // destructive to the registry — it invalidates the running sensor's key the
    // moment it returns — so confirming after it asks a question whose answer
    // can no longer change anything.
    // TODO: a transport error here is ambiguous — the API may have committed the
    // key. Re-query the device and report "dark, rotate again" if
    // key_valid_from moved. See docs/design/provisioning.md § Postponed.
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
    check_auth_before_unlock(&api_client, unlock)?;

    let mut chip = connect(unlock, confirm_args.yes)?;

    let state = chip.read_state()?;
    output::identity(&chip.probe_description(), chip::TARGET, &state);

    let registered = api_client.device(state.device_addr)?;
    output::fleet(api_client.label(), registered.as_ref(), Utc::now());

    // Same reasoning as `provision`, inverted: the API would 404 this. ⚠️ Note
    // what is *not* refused — a `Blank` record on a registered device is the
    // post-chip-erase recovery path, and must go through.
    let Some(existing) = registered else {
        bail!(
            "{} is not registered — provision it instead: \
             `homescope-provision provision <NAME>`",
            state.device_addr
        );
    };

    // TODO: warn before rotating a device whose key status is KEK_UNAVAILABLE —
    // loading the KEK is the fix. See docs/design/provisioning.md § Postponed.
    confirm_record(
        &state.record,
        Action::Rotate {
            name: &existing.name,
        },
        confirm_args.yes,
    )?;
    chip.halt()?;

    // TODO: same ambiguity as `provision` if the connection drops mid-mint.
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

/// Proves the token before anything is erased, on the one path where that has
/// to happen before the device can be looked up.
///
/// ⚠️ `--unlock` erases the chip to make its address readable at all, so the
/// usual proof — the device lookup, which fails on a bad token like any other
/// request — would arrive with the board already blank. Every other path gets
/// that proof from the lookup, before anything is halted or written, and skips
/// this round trip.
fn check_auth_before_unlock(api_client: &ApiClient, unlock: bool) -> anyhow::Result<()> {
    if unlock {
        output::step("Checking credentials", || api_client.check_auth())?;
    }

    Ok(())
}

// ⚠️ Why `chip.halt()` comes *after* `confirm_record` in both commands above:
// everything before the prompt is a plain memory read that leaves a running
// sensor running. Halting first would stop it for as long as the question
// stayed unanswered. probe-rs does release the halt when the session drops
// (clearing DHCSR.C_DEBUGEN), so a declined prompt never left a board stopped
// for good — but declining should cost the board nothing at all.

enum Action<'a> {
    Provision,
    Rotate { name: &'a str },
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

        (Action::Rotate { name }, RecordHeader::Present) => format!(
            "{name:?} holds its current key on this board.\n\
             It goes dark from the moment the new key is issued until the write lands."
        ),
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

/// Waits for the API to hear from a device under its current key — level 2 of
/// the verification ladder, and the one that proves the whole chain.
///
/// ⚠️ Never halts or resets the board. Reading its address over the probe is a
/// plain memory read, and the session is dropped before the wait: `verify` must
/// not stop the sensor it is waiting to hear from.
pub fn verify(api: &ApiArgs, address: Option<DeviceAddr>, timeout: Duration) -> anyhow::Result<()> {
    let client = resolve_client(api)?;

    let device_addr = match address {
        Some(device_addr) => {
            output::address(device_addr);
            device_addr
        }
        None => address_from_probe()?,
    };

    let summary = client.device(device_addr)?.ok_or_else(|| {
        anyhow!(
            "{device_addr} is not registered with {} — provision it first",
            client.label()
        )
    })?;

    output::fleet(client.label(), Some(&summary), Utc::now());

    if let Progress::Undecryptable(status) = verify::assess(None, &summary) {
        return Err(wait_failure(
            WaitError::Undecryptable(status),
            &summary.name,
            device_addr,
        ));
    }

    let reported_at = output::step(
        &format!(
            "Waiting for a new reading from {:?} (up to {}s)",
            summary.name,
            timeout.as_secs()
        ),
        || verify::wait_for_reading(&client, device_addr, summary.last_seen, timeout),
    )
    .map_err(|err| wait_failure(err, &summary.name, device_addr))?;

    println!(
        "{device_addr}\t{}",
        reported_at.to_rfc3339_opts(SecondsFormat::Secs, true)
    );
    output::outcome(&format!(
        "{:?} ({device_addr}) reported under its current key at {}",
        summary.name,
        output::timestamp(reported_at)
    ));

    Ok(())
}

/// Reads the attached board's address and releases the probe.
fn address_from_probe() -> anyhow::Result<DeviceAddr> {
    match Chip::connect()? {
        Connection::Attached(mut chip) => {
            let state = chip.read_state()?;
            output::identity(&chip.probe_description(), chip::TARGET, &state);

            Ok(state.device_addr)
        }

        Connection::Locked(locked) => {
            output::identity_locked(&locked.probe_description(), chip::TARGET);

            bail!(
                "the attached board is locked, so its address cannot be read — pass it \
                 explicitly: `homescope-provision verify <ADDRESS>`"
            )
        }
    }
}

/// Words a failed wait for the terminal. A timeout gets a checklist, because it
/// is the one failure that does not say where the break is.
fn wait_failure(err: WaitError, name: &str, device_addr: DeviceAddr) -> anyhow::Error {
    match err {
        WaitError::TimedOut(secs) => anyhow!(
            "no new reading from {name:?} ({device_addr}) within {secs}s\n\n  \
             The API reports no problem with its key, so the break is between the board\n  \
             and the API. Check that the board is powered and running firmware, that a\n  \
             receiver is in range, and that the receiver, gateway and broker are up.\n  \
             A board that reports less often than every {secs}s needs a longer --timeout."
        ),
        other => anyhow!("{name:?} ({device_addr}): {other}"),
    }
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
