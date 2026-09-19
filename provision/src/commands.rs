use std::{
    fs,
    io::{IsTerminal as _, Write as _, stderr, stdin, stdout},
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context as _, anyhow, bail};
use chrono::{DateTime, SecondsFormat, Utc};
use homescope_api_types::devices::{DeviceKeyResponse, DeviceKeyStatus, ProvisionDevicePayload};
use homescope_common::{device_addr::DeviceAddr, device_key::DeviceKey, uicr_record::RecordHeader};
use zeroize::Zeroize as _;

use crate::{
    api_client::{ApiClient, ApiClientError},
    chip::{self, Chip, Connection},
    cli::{ApiArgs, ConfirmArgs, FirmwareArgs, FirmwareCommand, LogArgs, ProbeArgs},
    confirm::{self, ConfirmError},
    firmware::{self, Artifact},
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

pub fn firmware(command: FirmwareCommand) -> anyhow::Result<()> {
    let mut store = firmware::Store::load()?;

    match command {
        FirmwareCommand::Add { path, name } => {
            let artifact = output::step(&format!("Reading {}", path.display()), || {
                store.add(&path, &name)
            })?;

            writeln!(
                stderr(),
                "\nStored {:?}\n  loads at {:#X}\n  seq storage {}",
                artifact.name,
                artifact.app_start,
                match artifact.storage_range() {
                    Some((start, end)) => format!("{start:#X}..{end:#X}"),
                    None => "none declared".to_owned(),
                }
            )?;

            println!("{}\t{}", artifact.name, artifact.id);
        }

        FirmwareCommand::Remove { name } => match store.remove(&name)? {
            Some(artifact) => {
                writeln!(stderr(), "Removed {:?} ({})", artifact.name, artifact.id)?;
            }
            None => writeln!(stderr(), "No stored firmware called {name:?}")?,
        },

        FirmwareCommand::List => {
            let now = Utc::now();
            let artifacts = store.artifacts();

            if artifacts.is_empty() {
                writeln!(
                    stderr(),
                    "No firmware stored.\n\nadd one: homescope-provision firmware add <PATH> --name <NAME>"
                )?;
                return Ok(());
            }

            let width = firmware::name_width(artifacts);
            for artifact in artifacts {
                println!("{}", firmware::describe(artifact, now, width));
            }
        }
    }

    Ok(())
}

/// The image to flash, from `--firmware` or the picker.
///
/// Returns the artifact *and* its path, because the store owns where the bytes
/// live and nothing else should be constructing that path.
fn choose_firmware(args: &FirmwareArgs) -> anyhow::Result<(Artifact, PathBuf)> {
    let store = firmware::Store::load()?;

    let artifact = match &args.firmware {
        Some(name) => store.get(name).cloned().ok_or_else(|| {
            anyhow!("no stored firmware called {name:?}\n\nsee: homescope-provision firmware list")
        })?,
        None => firmware::pick(&store, Utc::now())?.clone(),
    };

    let path = store.image_path(&artifact);

    Ok((artifact, path))
}

/// Firmware for `provision`/`rotate`, where flashing is optional.
///
/// ⚠️ No picker here, unlike [`flash`]. Omitting `--firmware` means "do not
/// flash", so re-keying a board that already runs firmware stays a single
/// silent command — and a prompt that appeared on every rotate is a prompt that
/// stops being read.
fn optional_firmware(args: &FirmwareArgs) -> anyhow::Result<Option<(Artifact, PathBuf)>> {
    args.firmware
        .is_some()
        .then(|| choose_firmware(args))
        .transpose()
}

/// Puts firmware on a board and leaves its key alone.
///
/// ⚠️ Does **not** clear the seq counter. No new key is minted here, and
/// clearing the counter under a live key re-emits nonces the device has already
/// used — see `Chip::erase_storage`.
pub fn flash(args: &FirmwareArgs, probe: &ProbeArgs, logs: &LogArgs) -> anyhow::Result<()> {
    let (artifact, image) = choose_firmware(args)?;

    let mut chip = match Chip::connect(probe.probe.as_deref())? {
        Connection::Locked(_) => bail!(messages::device_locked_warning()),
        Connection::Attached(chip) => chip,
    };

    let state = chip.read_state()?;
    output::identity(&chip.probe_description(), chip::TARGET, &state);

    // A flash leaves UICR alone, so a board with no usable record keeps not
    // having one. Worth saying — but not worth refusing: flashing a blank board
    // is ordinary bring-up.
    if !matches!(state.record, RecordHeader::Present) {
        writeln!(
            stderr(),
            "\nnote: this board holds no usable key, so it will not report until it is \
             provisioned.\n      Flashing does not change that."
        )?;
    }

    let before = chip.read_uicr_words()?;

    flash_image(&mut chip, &artifact, &image)?;

    // Cheap, and it catches a wrong-range image or a flash algorithm that
    // reached further than it claimed.
    output::step("Confirming the key record is untouched", || {
        let after = chip.read_uicr_words()?;
        (after == before)
            .then_some(())
            .ok_or_else(|| anyhow!("the UICR record changed during flashing"))
    })?;

    output::step("Resetting", || chip.reset())?;

    report_boot(&mut chip, &image, logs)?;

    println!("{}\t{}", state.device_addr, artifact.name);
    output::outcome(&format!(
        "Flashed {:?} ({}) onto {}",
        artifact.name, artifact.id, state.device_addr
    ));

    Ok(())
}

fn flash_image(chip: &mut Chip, artifact: &Artifact, image: &Path) -> anyhow::Result<()> {
    output::step(&format!("Flashing {:?}", artifact.name), || {
        chip.flash(image, artifact.app_start)
    })?;

    Ok(())
}

/// ⚠️ Never prompts and never refuses. A locked chip is a *state* to report,
/// not a failure — and the old message told the reader to pass `--unlock`, a
/// flag `info` does not have.
pub fn info(probe: &ProbeArgs) -> anyhow::Result<()> {
    match Chip::connect(probe.probe.as_deref())? {
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
    firmware_args: &FirmwareArgs,
    probe: &ProbeArgs,
    logs: &LogArgs,
    confirm_args: &ConfirmArgs,
    unlock: bool,
    name: String,
) -> anyhow::Result<()> {
    // Before the probe is touched: choosing firmware can prompt, and a prompt
    // should not sit between a halt and a mint.
    let firmware = optional_firmware(firmware_args)?;

    let api_client = resolve_client(api)?;
    check_auth_before_unlock(&api_client, unlock)?;

    let mut chip = connect(probe, unlock, confirm_args.yes)?;

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
    let mut response = output::step(&format!("Registering {:?}", send_body.name), || {
        mint_key(&api_client, state.device_addr, None, "provision", || {
            api_client.provision(&send_body)
        })
    })?;

    install_key(
        &mut chip,
        &state.record,
        &mut response,
        firmware.as_ref(),
        logs,
        "provision",
    )?;

    println!("{}\t{}", response.device_addr, response.name);
    output::outcome(&format!(
        "Provisioned {:?} as {}",
        response.name, response.device_addr
    ));

    Ok(())
}

pub fn rotate_key(
    api: &ApiArgs,
    firmware_args: &FirmwareArgs,
    probe: &ProbeArgs,
    logs: &LogArgs,
    confirm_args: &ConfirmArgs,
    unlock: bool,
) -> anyhow::Result<()> {
    let firmware = optional_firmware(firmware_args)?;

    let api_client = resolve_client(api)?;
    check_auth_before_unlock(&api_client, unlock)?;

    let mut chip = connect(probe, unlock, confirm_args.yes)?;

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

    // ⚠️ Rotating is not the fix for a key the API cannot *unseal*. The key on
    // the board may be perfectly good — the API is simply missing the KEK
    // generation it was sealed under — and rotating forces a re-flash of a
    // device whose key was never wrong. Said above the prompt rather than made
    // into a second one: it is advice, and there are cases where rotating
    // anyway is right.
    if existing.key_status == DeviceKeyStatus::KekUnavailable {
        writeln!(
            stderr(),
            "\n⚠️  the API cannot unseal this device's key — it is sealed under a KEK\n    \
             generation the API has not loaded. Loading that generation is usually the\n    \
             fix; rotating replaces a key that may be perfectly good and costs a reflash."
        )?;
    }

    confirm_record(
        &state.record,
        Action::Rotate {
            name: &existing.name,
        },
        confirm_args.yes,
    )?;
    chip.halt()?;

    let mut response = output::step("Requesting a new key", || {
        mint_key(
            &api_client,
            state.device_addr,
            Some(existing.key_valid_from),
            "rotate",
            || api_client.rotate_key(state.device_addr),
        )
    })?;

    install_key(
        &mut chip,
        &state.record,
        &mut response,
        firmware.as_ref(),
        logs,
        "rotate",
    )?;

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

/// Reads the board's own log after a reset — level 0 verification.
///
/// ⚠️ Never fails the run on its own. The board has already been keyed and
/// flashed by this point; whatever the log says, the operation happened. What
/// it can do is tell you *now*, at the bench with the probe still attached,
/// that the firmware says it has no key — instead of after a walk to the
/// cupboard and a timed-out `verify`.
fn report_boot(chip: &mut Chip, image: &Path, args: &LogArgs) -> anyhow::Result<()> {
    let window = Duration::from_secs(args.logs);
    if window.is_zero() {
        return Ok(());
    }

    let elf = fs::read(image)?;
    let mut err = stderr();

    writeln!(err, "\nBoot log ({}s):", args.logs)?;

    let summary = match chip.stream_logs(&elf, window, &mut err) {
        Ok(summary) => summary,
        // The board is keyed and flashed either way, so a broken log reader
        // must not look like a broken provision.
        Err(problem) => {
            writeln!(err, "  (could not read the log: {problem})")?;
            return Ok(());
        }
    };

    if summary.is_silent() {
        writeln!(
            err,
            "  (nothing decoded: {} channel(s), {} bytes, core {} — either the board \
             is not running, or this image was built without DEFMT_LOG)",
            summary.channels,
            summary.bytes,
            chip.core_status().unwrap_or_else(|_| "unknown".to_owned())
        )?;
    } else if !summary.errors.is_empty() {
        writeln!(
            err,
            "\n⚠️  the firmware reported {} error(s) above — the board is keyed and \
             flashed,\n    but it is saying something is wrong.",
            summary.errors.len()
        )?;
    }

    Ok(())
}

/// What the registry says happened, when a mint failed without answering.
#[derive(Debug, PartialEq, Eq)]
enum Mint {
    /// The key was issued. It crossed the wire once and is gone.
    Committed,
    /// The registry is untouched; the device is exactly as it was.
    Untouched,
    /// The follow-up query failed too, so neither can be ruled out.
    Unknown,
}

/// Runs a mint and, when it fails *ambiguously*, asks the registry whether it
/// landed.
///
/// ⚠️ This is the one failure that leaves a device dark while reporting
/// something that sounds retryable. `POST /devices` and `rotate-key` return the
/// plaintext key exactly once; if the connection drops after the API commits,
/// the tool never sees the key, the board never receives it, and the registry
/// holds a key nothing can use. Without this check the operator is told
/// "request failed" and has no way to tell that apart from "nothing happened".
///
/// The discriminator is `key_valid_from`, which the API sets on every mint:
/// `provision` starts from an unregistered device, `rotate` from a known epoch,
/// and either way a changed answer means the key was issued.
fn mint_key(
    api_client: &ApiClient,
    device_addr: DeviceAddr,
    epoch_before: Option<DateTime<Utc>>,
    retry_command: &str,
    request: impl FnOnce() -> Result<DeviceKeyResponse, ApiClientError>,
) -> anyhow::Result<DeviceKeyResponse> {
    let err = match request() {
        Ok(response) => return Ok(response),
        // The API answered and refused, so nothing was stored — let it speak.
        Err(err) if !err.may_have_committed() => return Err(err.into()),
        Err(err) => err,
    };

    let outcome = match api_client.device(device_addr) {
        Ok(found) => classify_mint(epoch_before, found.map(|summary| summary.key_valid_from)),
        Err(_) => Mint::Unknown,
    };

    Err(match outcome {
        Mint::Untouched => anyhow!(err)
            .context("the request failed and the registry is unchanged; nothing was issued"),

        Mint::Committed => anyhow!(
            "the API issued a new key but the response never arrived\n\n  \
             {device_addr} is registered with a key it never received, so it will not\n  \
             report. Re-run `homescope-provision {retry_command}` — the issued key is\n  \
             not recoverable and a new one will be minted.\n\n  \
             (caused by: {err})"
        ),

        Mint::Unknown => anyhow!(
            "the request failed, and asking the API what happened failed too\n\n  \
             {device_addr} may or may not have been issued a key. Check with\n  \
             `homescope-provision list`, then re-run `homescope-provision {retry_command}`\n  \
             if it holds a key the board never received.\n\n  \
             (caused by: {err})"
        ),
    })
}

/// Reads the registry's before/after key epochs as a verdict on the mint.
fn classify_mint(before: Option<DateTime<Utc>>, now: Option<DateTime<Utc>>) -> Mint {
    match (before, now) {
        // Registered when it was not, or a newer epoch than we started with:
        // the mint landed and took the key with it.
        (None, Some(_)) => Mint::Committed,
        (Some(before), Some(now)) if now > before => Mint::Committed,

        (Some(_), Some(_)) | (None, None) => Mint::Untouched,

        // A device that was registered a moment ago is gone. Something else is
        // wrong, and guessing either way is worse than saying so.
        (Some(_), None) => Mint::Unknown,
    }
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
    firmware: Option<&(Artifact, PathBuf)>,
    logs: &LogArgs,
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

        // ⚠️ After the key is verified, not before: a bad key under good
        // firmware fails silently, where good firmware with no key announces
        // itself over RTT.
        if let Some((artifact, image)) = firmware {
            flash_image(chip, artifact, image)?;

            // ⚠️ Only here, and only because a *new* key was just installed.
            // The range comes from the image being flashed — the only thing
            // that knows where this firmware keeps its counter — which is also
            // why there is no way to ask for this without flashing.
            if let Some((start, end)) = artifact.storage_range() {
                output::step("Clearing the seq counter", || {
                    chip.erase_storage(start, end)
                })?;
            }
        }

        output::step("Resetting", || chip.reset())?;

        if let Some((_, image)) = firmware {
            report_boot(chip, image, logs)?;
        }

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
pub fn verify(
    api: &ApiArgs,
    probe: &ProbeArgs,
    address: Option<DeviceAddr>,
    timeout: Duration,
) -> anyhow::Result<()> {
    let client = resolve_client(api)?;

    let device_addr = match address {
        Some(device_addr) => {
            output::address(device_addr);
            device_addr
        }
        None => address_from_probe(probe)?,
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
fn address_from_probe(probe: &ProbeArgs) -> anyhow::Result<DeviceAddr> {
    match Chip::connect(probe.probe.as_deref())? {
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
        WaitError::Unreachable(secs, problem) => anyhow!(
            "could not reach the API while waiting for {name:?} ({device_addr})\n\n  \
             The last {secs}s of polling all failed: {problem}\n  \
             Nothing was learned about the board — check the API, then re-run."
        ),

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

fn connect(probe: &ProbeArgs, unlock: bool, assume_yes: bool) -> anyhow::Result<Box<Chip>> {
    let chip = match Chip::connect(probe.probe.as_deref())? {
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

#[cfg(test)]
mod test {
    use super::*;

    fn at(offset_secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_753_000_000 + offset_secs, 0).expect("valid timestamp")
    }

    /// `provision` starts from an unregistered device, so a row appearing at all
    /// means the API committed — and the key it returned is gone.
    #[test]
    fn a_new_registration_means_the_key_was_issued() {
        assert_eq!(classify_mint(None, Some(at(0))), Mint::Committed);
    }

    /// `rotate` starts from a known epoch; only a *newer* one is a fresh mint.
    #[test]
    fn a_newer_key_epoch_means_the_key_was_issued() {
        assert_eq!(classify_mint(Some(at(0)), Some(at(60))), Mint::Committed);
    }

    /// The common case, and the one worth getting right: the request never
    /// reached the registry, so the device is untouched and a retry is safe.
    #[test]
    fn an_unchanged_epoch_means_nothing_happened() {
        assert_eq!(classify_mint(Some(at(0)), Some(at(0))), Mint::Untouched);
        assert_eq!(classify_mint(None, None), Mint::Untouched);
    }

    /// ⚠️ Never guessed. A device that was registered a moment ago and is now
    /// absent means something other than this mint is wrong, and both "your key
    /// is lost" and "nothing happened" would be inventions.
    #[test]
    fn a_vanished_device_is_not_guessed_at() {
        assert_eq!(classify_mint(Some(at(0)), None), Mint::Unknown);
    }
}
