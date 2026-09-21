# Device provisioning (identity + AEAD key)

> **Status: 🔶 in progress.** Decisions settled 2026-07-20; the tool's final
> shape settled 2026-08-26 in §0. Built so far: device keys envelope-encrypted
> at rest (§4); the UICR record, its firmware reader and the tool's write path
> (§5); and `homescope-provision` build-order steps 1–4 — preconditions,
> confirmations and output, `login` with named profiles, `list`, `verify`, the
> firmware artifact store, `--firmware` flashing and seq clearing.
>
> **Read §0 first.** Everything after it predates the tool existing, and §0
> supersedes several of those passages by name. Inline ✅ / ⚠️ markers record
> what has since been built, or built differently; the original reasoning stays
> beside them.

**Supersedes the build-time `DEVICE_KEY` env → `link_section` sketch** in
[packet-tv-aead.md](packet-tv-aead.md) §4 and `docs/architecture.md` "Key provisioning".

Scope: how a new sensor goes from a blank nRF52840 to a registered, keyed,
flashed node. Lands with step ④ (AEAD) of the packet/crypto block, but the
CLI and the `devices`-row shape can be built earlier.

Guiding constraint: **one firmware binary for the whole fleet**. Anything that
makes the image per-device (build-time key injection) is rejected on that
ground alone — it breaks shared CI artifacts and makes any future OTA
per-device.

---

# 0. Final tool shape — settled 2026-08-26

Everything below §0 predates the tool existing. This section is the design as
it stands after building the UICR write path, and it **supersedes** the
earlier passages it names. Read it first.

## What the tool is

**`homescope-provision` is the only thing that gives a board an identity in
the fleet, and the only place outside the API where a device key exists in
cleartext.** Every feature is in service of one of those two facts, or of not
having to come back to the bench a second time.

That settles more than it looks like it does:

- It runs with the board in your hand, once per device, at human speed. It can
  be interactive, chatty and slow. It does not need `--json`, concurrency, or
  a daemon.
- It is **not a build tool and not a debugger**. `cargo run --features
  board-xiao` already flashes and streams RTT and is faster at it. Anything
  the tool does with firmware is about *artifacts*, not about the edit loop.
  (See [firmware-variants.md](firmware-variants.md) § the tool owns the store, not the build.)
- Everything it prints is either "which board is this" or "what do I write on
  the enclosure".

## Invariants — put these at the top of `main.rs`

1. The key exists in the tool's memory and in UICR. Never a file, a log,
   stdout, a `Debug` impl, or a retry buffer.
2. Any failure after the mint means the device is dark and a **new** key must
   be minted. No resume, no recovery of the in-flight key.
3. Never read a key off a chip. (§3 refuses `GET /devices/{addr}/key`; a
   `dump` subcommand is the same hole with a probe attached.)
4. UICR header word last, always.
5. Verify UICR **before** flashing — a bad key under good firmware is a silent
   failure; good firmware with no key announces itself over RTT.
6. **The seq counter may only be cleared in the same operation that installs a
   new key.** See § seq counter below.
7. Fail closed with no TTY.

## Command surface

```
homescope-provision [--url URL] [--probe SERIAL] [--yes]

  login / logout / whoami          credentials for an API URL
  list                             fleet: address, name, key status, last seen
  info                             what is on this probe right now
  provision <name> --firmware ELF  blank board → fleet member
  rotate           [--firmware ELF]  new key for an existing member
  flash             --firmware ELF   firmware only, no key change
  verify                           is this board actually producing readings
  firmware add / list              the artifact store
  (later) lock / unlock
```

Two existing promises to make real: **`--probe <serial>`** (the
`AmbiguousProbe` error already tells the user to pass a flag that does not
exist) and **`--yes`**.

✅ **As built (2026-09-13):** `login` / `logout` / `whoami`, `list`, `info`,
`provision <name>`, `rotate` and `verify [ADDRESS]`. Two shapes changed on the
way. The API is chosen per command — `--profile` or `--api-url`, flattened into
the commands that talk to it — rather than by a global `--url` (§ Auth below).
`--yes` is flattened into the destructive commands rather than global, because
`info` and `login` destroy nothing and should not accept a flag that skips a
prompt they never ask. ✅ **Extended 2026-09-19:** `firmware add/list/remove`,
`flash`, `--firmware` on `provision`/`rotate`, `--probe <serial>` and `--logs`.
⏳ Not yet: `lock`/`unlock`, which waits on the firmware `Debug::Disallowed`
change.

`list` is worth more than it looks — `GET /devices` already returns the union
you need, and the first question when a sensor goes quiet is "does the API
think this device has a working key", which today is answered with `curl`.

## Preconditions: the chip and the registry are different facts

⚠️ **This supersedes § "Explicit subcommands, prompts only for destruction"
below**, which says `provision` on an already-provisioned board should refuse
*"already registered as 'kitchen'"*. Note what that message contains: a
**name**. The chip cannot tell you that. All the chip has is `RecordHeader` —
"there is a record here" — which is a different question from "is this address
in the fleet". The two come apart in exactly the cases preconditions exist for:

| | chip record | registry row |
|---|---|---|
| fresh board | Blank | absent |
| normal deployed sensor | Present | present |
| **chip-erased deployed sensor** | Blank | **present** |
| **board from another deployment** | Present | **absent** |
| **half-failed provision** | Blank | present, `key = MISSING` |

So: **the API owns the refusal, the chip owns the confirmation.** The API
already does its half correctly — 409 `device already exists - rotate its key
instead` and 404 `device not found`, both raised before anything is stored.
Do not duplicate that locally by guessing from the record header; the guess is
wrong in two of the five rows.

✅ **As built (2026-09-13):** the tool *asks* the API before prompting —
`GET /devices/{addr}` straight after reading the chip — and refuses early on the
answer: `provision` of a registered device, `rotate` of an unregistered one.
That is still the API owning the refusal rather than a guess from the record
header, and the `POST` stays authoritative if the registry changes between the
two requests.

⚠️ **The check to *not* add: `rotate` refusing on a `Blank` record.** Row 3 is
the post-chip-erase recovery path — the row exists, the device needs a new
key, and there is nothing on the chip to see. If `rotate` refuses on Blank and
`provision` 409s on the registry, that board is unprovisionable and you are
deleting rows by hand. `rotate` accepts any record state.

**`provision`** — "this board is new to the fleet"

| record | |
|---|---|
| `Blank` | proceed. No erase, no prompt. The happy path must be prompt-free. |
| `Present` | **confirm** — a key is being destroyed. Then let the API 409 if it is actually in the fleet. |
| `Malformed(e)` | **confirm**, printing `e`. Someone else's data, or a newer tool's record. |

**`rotate`** — "this board is in the fleet, give it a new key"

| record | |
|---|---|
| `Present` | **confirm** — the live sensor goes dark until the write lands. |
| `Blank` | proceed, no prompt. Nothing to destroy. |
| `Malformed(e)` | **confirm**, printing `e`. |

**`info`** never prompts, never refuses, and reports `Locked` as a *state*
rather than failing. (✅ Fixed — it used to `bail!`, with a message telling the
reader to pass `--unlock`, a flag `info` does not have.)

One rule: *prompt exactly when a step destroys something the tool can see, or
something it cannot see and therefore cannot rule out.* Everything else runs
silent — that is § "prompts only for destruction" stated operationally, and
why merging detection with consent is rejected.

## Confirmations

Four, and one is not like the others.

The three record-level ones are **y/N, default No**, with the identity block
printed immediately above — the failure they guard is *wrong board attached*,
and the address on screen is what catches it.

`erase_to_unlock` is different: APPROTECT means nothing is readable, so the
tool **cannot name the board it is about to wipe**. Blind consent deserves
**type-to-confirm** (`ERASE`) rather than a keystroke. It is weak, but it
costs a deliberate act rather than a reflex.

⚠️ On that path the erase happens **before the tool has ever spoken to the
API**, so a wrong `--token` costs a board. That is the strongest argument for
the pre-flight below.

Mechanics:

- `--yes` is **global**, next to `--url`. It is a mode, not a per-command
  option; `--token`/`--unlock` are already duplicated across two subcommands.
  ⚠️ *As built it is flattened, not global:* clap's `#[command(flatten)]`
  removes the duplication this bullet worried about, and `info`/`login` should
  not accept a flag for a prompt they never ask.
- Gate on `std::io::stdin().is_terminal()` (`std::io::IsTerminal`, no new
  dependency).
- **Fail closed**: no TTY and no `--yes` ⇒ error naming `--yes`, never an
  implicit yes.
- Prompt text and the read go to **stderr/stdin**, never stdout.
- ⚠️ **Return `Result<(), Aborted>`, not `bool`.** We have already been bitten
  by exactly this shape — `verify_words(...)?;` returning a discarded `bool`
  was a verification step that verified nothing. A `bool` at a safety gate can
  be called and ignored; a `?` cannot. `#[must_use]` is the weaker version.
  ✅ Built as `Result<(), ConfirmError>`, with the TTY policy a pure function
  under test.
- Hand-roll it. `dialoguer` exists; this is ten lines and the part that
  matters (TTY policy, failing closed) is a decision no crate makes for you.

## Ordering, and the window that needs a name

```
connect → read state → print identity → CONFIRM → API mint → erase → write → verify → flash → clear seq → reset → report
                                                  ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
                                                                  device is dark in here
```

✅ **As built (2026-09-13):** `connect → read state → identity → look up →
CONFIRM → halt → mint → erase → write + verify → reset → report`. Flashing and
seq clearing arrive with build-order step 4. The halt moved *after* consent:
everything before the prompt is a plain memory read that leaves a running
sensor running, so declining costs the board nothing.

**Confirm before the mint, not before the erase.** The mint is destructive *to
the registry*: `POST rotate-key` invalidates the running sensor's key the
moment it returns. Confirming after it means the device is already broken by
the time the question is asked.

**Any failure after the mint needs its own message.** The hard rule in §1 is
that the plaintext key crosses an interface exactly once, so a failed write is
not a generic error — the device is dark and stays dark:

```
error: the UICR write failed after the API issued a new key

  C60504030201 ("kitchen") will not report until it is re-keyed.
  Re-run `homescope-provision rotate` — the issued key is not
  recoverable and a new one will be minted.
```

## Output

Lead with identity, in **every** command including the destructive ones. This
block is the whole answer to "is this the right board":

```
Probe    CMSIS-DAP (E6614C311B7C9E2F)
Target   nRF52840_xxAA
Address  C60504030201
Record   blank
```

Locked variant reports the state instead of failing:

```
Status   locked (APPROTECT) — address unreadable
```

Then one resolved line per fallible step, and a final line you can copy onto
the enclosure:

```
Registering "kitchen" … ok
Writing UICR record … ok
Verifying … ok
Flashing sensor-xiao … ok
Resetting … ok

Provisioned "kitchen" as C60504030201
```

**Streams**: diagnostics, step lines and prompts → **stderr**; the one durable
fact (address, or `ADDR\tNAME`) → **stdout**. Then
`homescope-provision provision kitchen >> labels.tsv` does the obvious thing
and the prompt still reaches you.

### Never printed

- **The key.** Not on success, not on error, not under a future `--verbose`.
- ⚠️ **`DeviceKeyResponse` derives `Serialize, Deserialize` and *not* `Debug`**
  — so `{response:?}` and `error!(?response)` are compile errors. That is
  load-bearing and one convenience derive away from a fleet key in a log line.
  It deserves the same kind of comment the API carries about `TraceLayer`:
  *no Debug: this struct holds a plaintext DEK.*
- **The admin token**, including no echo when read from stdin.
- ✅ *Fixed:* `response.key` used to be an unzeroized `String` outliving the
  write. It is now zeroized straight after `DeviceKey::from_hex`, before the
  `?`, so the error path cannot skip it.

Error rendering is fine as-is: `ApiClientError::Rejected` surfaces the API's
`message`, which the API already redacts (`sqlx::Error`'s Display never
reaches a body).

## Firmware flashing

**Take a path, require it, never search the CWD, never build.** The
reasoning, the artifact store that replaces raw paths, and the
`firmware add`/`firmware list` seam are in [firmware-variants.md](firmware-variants.md).

Two guards, both cheap:

- ⚠️ **Cross-check the ELF against the board before flashing.** If the image's
  lowest load address is `0x27000` it expects a bootloader beneath it — read
  `0x00000000` over SWD and refuse if it is erased. Two words, and it turns
  the XIAO brick (§ "UF2 is retired — and the trap that comes with it") into
  an error message.
- ⚠️ **`DownloadOptions { verify: true, do_chip_erase: false, .. }`.**
  `do_chip_erase` defaults to `false` (probe-rs 0.32,
  `src/flashing/download.rs:126`) and **must stay false** — a chip erase after
  the UICR write destroys the key just verified. `verify: true` gives
  read-back for free.

`flash` as a standalone subcommand: yes, but it is "put this release artifact
on an already-provisioned board without a source tree". It is not the dev
loop and should not try to be.

## Clearing the seq counter — the ELF already knows

⚠️ **This supersedes the "should the seq address live in UICR?" question.** It
should not, and nothing needs to store it: the addresses are **absolute
symbols in the firmware ELF's symbol table**.

```
$ nm firmware/target/thumbv7em-none-eabi/release/homescope-sensor | grep -i storage
000f4000 A __storage_end
000f2000 A __storage_start
```

`board/build.rs` emits `__storage_start = ORIGIN(STORAGE)` into the generated
`memory.x`; `seq_counter.rs` declares them `extern "C"`; they land as `A`
(absolute — no section, no contents) exactly as the build script's doc comment
says. **The image you are about to flash is the authority on where its own
storage lives**, per board, forever, with no second copy to drift.

UICR is the wrong home for it twice over: the storage address is a
**per-build** fact (`0x000F2000` on XIAO, `0x000FE000` on DB-40) that would
live in **per-device** memory, going stale the first time a board changed
variant, and unrewritable without an `ERASEUICR`.

This also settles the coupling: **clearing requires an ELF, so it happens
exactly when you flash.** No `--firmware`, no clear. Not a limitation — without
the ELF you are guessing at a page address, and guessing wrong erases the UF2
bootloader.

Mechanically small: `ERASEPAGE` at `0x4001_E508`, one write per page, straight
into the existing `Eraser` guard.

**Is it needed?** No. A fresh key makes the counter's value irrelevant, and
`key_valid_from` means the ingest replay check accepts a restart at zero. It
buys determinism, which is worth having while debugging. Do it.

### ⚠️ A cleared counter is invisible to an already-running receiver

Found on the bench 2026-09-19, the first time `rotate --firmware` cleared a
counter for real. The device went silent and nothing in the provisioning path
was wrong: firmware healthy over RTT, sensors reading, advertising every cycle.

`firmware/receiver/src/ble_scan.rs` dedups a burst per device with

```rust
if cache.get(&device_addr).is_some_and(|cached| seq <= *cached) { continue; }
```

The dongle's cache still held the pre-clear high-water mark (24605); the board
restarted at 1024. **Every genuine packet was dropped**, and would have been for
the ~16 days it takes a once-a-minute counter to climb back. The cache lives in
the dongle's RAM, so replugging it is the immediate cure.

Two things follow.

**`<=` is the wrong comparison for a burst dedup.** `==` drops the ~20 repeats
of one burst, which is the whole job. The extra inequality buys replay
protection the receiver was never meant to provide — it is deliberately
semantics-blind and keyless — and which belongs to the API's per-device seq
check, where the key epoch is known (see
[ingest-db-error-handling.md](ingest-db-error-handling.md)).

⚠️ **Worse, `<=` turns a forgeable field into a denial of service.** `seq` is
cleartext and unauthenticated at the receiver by design. Anyone in range can
advertise one packet carrying a victim's address and `seq = 0xFFFFFFFF`, and
that device is dropped until the dongle is power-cycled. The AEAD tag stops the
forgery reaching the database; it does not stop the forgery silencing a real
device upstream of it. This is the same family as the note in `CLAUDE.md` about
foreign advertisers evicting cache slots, but sharper — eviction degrades, this
one is targeted and sticky.

So the receiver should drop only `seq == cached`, and provisioning should keep
clearing the counter. Until that lands, ⚠️ **re-keying a device requires
replugging the receiver**, or it stays invisible.

⚠️ **Never offer a standalone `reset-seq`.** Clearing the counter under a
*live* key is nonce reuse across the entire history of that key — the collapse
`seq_counter.rs`'s module docs are written about, not a degradation. Inside
`provision`/`rotate` it is safe because UICR is erased before the write, so no
window exists where an old key and a fresh counter coexist. A standalone
command is nothing *but* that window.

## Auth — refinements to §3

The `login` → config-file shape in §3 stands. Four changes:

- ⚠️ **Key credentials per API, not one global token.** There will be a dev
  instance and a Pi, later two sites, and a single `token = "..."` silently
  sends prod credentials to a dev box or vice versa.
  ✅ *As built (2026-09-13): named profiles rather than URL keys.* A
  confirmation has to name the API it acts on, and `prod` is read at a glance
  where a URL at the end of a line is not; profiles also stop URL normalisation
  from being a correctness problem. Two files, split so "is this a secret" is a
  property of the path: `~/.config/homescope/config.toml` (0644 —
  `default_profile` and a `[name]` table per profile) and `credentials.toml`
  (0600 — a `[name]` table per token). `--api-url` stays as an escape hatch and
  takes its token from `HOMESCOPE_TOKEN` only, never from a stored profile.

- **Remove `--token` rather than deprecating it.** A flag that exists gets
  used and lands in shell history. `HOMESCOPE_TOKEN` is a genuinely better CI
  escape hatch, not just a differently-shaped one: `/proc/<pid>/cmdline` is
  world-readable, `/proc/<pid>/environ` is 0400 owner-only.
- **Check the file's mode on read, not just on write.** 0600 on create, and
  refuse to use it if it is group- or world-readable. That is what `ssh` does,
  and it catches the two realistic failures: a bad `cp`, and a dotfiles repo
  that got synced.
- **`whoami` earns its place immediately**, before users exist: it validates
  the stored credential against the URL, which is the pre-flight the
  destructive paths need. ✅ Built; it names the profile rather than a user,
  since the API has one shared admin token and no identity to report.

**No refresh tokens** — confirming §3's reasoning against the "maybe a refresh
one?" instinct. Refresh rotation bounds damage when access tokens travel
widely (browsers, many services, log aggregators). This one travels from one
CLI on one workstation to one API. When users land, a long-lived session token
in a 0600 file has the same risk profile as the SSH key next to it, and
revocability comes from a server-side token list, not from rotation.

Per-device ownership later changes nothing client-side: the server returns 403
for an address you do not own, and `ApiErrorCode::Unknown` being
`#[serde(other)]` already means an old binary meeting a new `DEVICE_FORBIDDEN`
code renders the server's `message` instead of choking.

## Verification — three levels

⚠️ This is § "No ack step" turned into a command. That section is right that
an ack records only what the *CLI thought* happened; these levels each prove a
different amount of the real chain.

**L0 — free, no changes anywhere.** After reset, attach RTT and decode defmt
against the ELF just flashed. The firmware already logs `device is empty - not
provisioned`, `device is not correctly provisioned: {}`, and `[flash] scanned
for seq: running from: N`. Reporting which appeared catches the most likely
provisioning failure with no firmware change and no network. The `.defmt`
section is in the ELF and non-alloc, so it never reaches flash — that is what
`defmt-decoder` needs. Once the boot probe lands
([firmware-variants.md](firmware-variants.md)), the detected-sensor log line joins this.

**L1 — one firmware log line.** Log the sealed packet bytes when built. The
tool decodes the frame and, **holding the key it just minted**, runs
`SensorPacket::parse` + `decode` locally. Proves UICR → cipher → seq counter →
sensors → TV encoding, with no receiver, no gateway, no new API surface.
Logging ciphertext is safe — it is exactly what goes on air. Only possible
during `provision`/`rotate`; a standalone `verify` on a deployed board has no
key (by design) and can only do L0 plus "does the cleartext header parse".

**L2 — the real end to end, and the cheapest to build.** Poll
`GET /devices/{addr}` until a reading appears. Proves RF, receiver, gateway,
MQTT **and** the API's decrypt under the sealed key. The query is already
specified in §1: `MAX(seq) FROM readings WHERE device_id = $1 AND time >
key_valid_from`, where NULL means "registered but has never reported under
this key". Cost is one column on the `DeviceSummary` response.

**Build L2 first.** It is the most convincing and the least work.

✅ **L0 and L1 built 2026-09-19**, after L2. L0 streams the boot log over RTT
after the reset; L1 decodes a packet the board logged, using the key just
written. Both run inside `provision`/`rotate` and neither fails the run — by
then the board is keyed and flashed, so calling that a failed run would be
false; they warn instead.

⚠️ **L1 costs one relaxation worth naming.** The key used to be zeroized the
moment it reached UICR; now a `PacketCipher` is built from it first, so it
outlives the write by a few seconds. Still memory-only — never a file, a log,
stdout or a `Debug` impl — and still minted once and never re-read.

⚠️ **L1 couples the tool to a firmware format string.** It matches the
`packet: [..]` line `packet_builder.rs` emits. A reworded line makes level 1 go
quiet rather than fail, deliberately: a silent check is better than one that
accuses a healthy board.

✅ **Built as `verify` (2026-09-13).** It reads `lastSeen` — the newest reading
time since `key_valid_from` — rather than `MAX(seq)`, and passes only on a value
*newer* than the one on file when waiting began, so on a deployed board it
answers "is it reporting now" rather than "did it ever". Both timestamps come
from the server; the workstation's clock is never compared.

**Division of labour**: L1 tests the device, L2 tests the fleet. When L2
fails, L1 is what says which side of the radio the problem is on.

⚠️ **Rejected: proxying a defmt-captured packet to a test ingest endpoint.**
(Re-examined 2026-09-19 and still rejected, but the reasoning below has a hole
worth recording: it assumes network implies a gateway in earshot, which is
false — a basement bench may have one and not the other. What actually settles
it is that `list` already reports whether the API can unseal a device's key, so
composing that with a local decrypt gives the same assurance with no new
endpoint, no second decode path to keep in step with ingest, and nothing that
could one refactor later start persisting what it was sent.)
It sits between L1 and L2 and is dominated by both — against L1 it costs a new
admin-authenticated packet-injection endpoint plus a second ingest path to
keep in sync with the MQTT one, to prove strictly less than L2; against L2 it
exercises a code path no device ever uses. L1 already covers the
no-infrastructure case, by decrypting locally with the key the tool is holding
at exactly that moment and never again.

## Locking — §6's hardening gotcha is CONFIRMED

§6 settled this already (separate subcommand, defer until the fleet is
stable) and correctly predicted the hardening interaction, flagging it as
*"verify what the config actually does before trusting the lock"*. It has now
been verified, and the answer is the bad one — see § "The hardening gotcha" in
§6 for the code references.

Short form: **every provisioned sensor currently unlocks itself on power-up.**
`embassy_nrf::init` with a default-derived config writes `UICR.APPROTECT` and
the `APPROTECT.DISABLE` register at every boot.

The consequence for the tool: `lock` is not a tool feature on its own, it is a
coordinated change — firmware sets `config.debug = Debug::Disallowed` (or
`NotConfigured`) **and** the tool writes the UICR word. Ship one without the
other and the board either never locks, or fights itself every boot.

One more cost worth naming: a locked board can only be re-keyed via
`ERASEALL`, which is the `--unlock` path. Locking makes the existing recovery
path the *only* path — an argument for having exercised it against a genuinely
locked board first, which has not happened yet.

## Getting a device's name for the prompt

✅ **Both options built (2026-09-13).** Option 1 went further than a message:
the 409 carries typed `details` (`DeviceAlreadyExistsDetails { deviceAddr,
name }`). The stability objection below was answered rather than ignored —
`code` is the tag, `details` is held as raw JSON and read only under its own
code, so an unknown code with details, or a known code that later gains them,
costs the details and never the body. Payloads are declared with
`error_details!`, which makes a second payload type for one code a compile
error. Option 2 is the device lookup `provision` and `rotate` now make before
prompting.

The offer was to put `name` in `ApiError`'s response body. **Don't** —
`ApiErrorBody` is `{code, message}`, and per-code payload fields break the
shape that `ApiErrorCode::Unknown` + `#[serde(other)]` exists to keep stable.

Two better options, and the second is the real one:

1. Put the name in the human `message` server-side: *"device already exists as
   'kitchen' — rotate its key instead"*. Free, no contract change.
2. **A pre-flight `GET /devices/{addr}`** — because the name's actual job is
   to make the *confirmation* meaningful (`re-key "kitchen"
   (C60504030201)?` rather than a hex string you have to recognise), and the
   409 arrives long after you have already confirmed. The same request
   validates the token before the unlock erase, so one round trip pays for
   both, satisfying §1 step 0's pre-flight requirement.

Cost, so it is not a surprise: `DeviceSummary` lives in
`api/src/devices/summary.rs`, is `Serialize`-only, and its `classify` reaches
into `keys::KekRing` and `store::DeviceRecord`. Consuming it means moving the
DTO to `api-types`, adding `Deserialize`, leaving `classify` behind as a free
function, giving `DeviceKeyStatus` a `#[serde(other)] Unknown`, and writing
the golden tests `api-types` requires of everything in it.

## What is already built (2026-08-25, extended 2026-09-13)

The probe-rs spike in §2 is **answered** — both questions came out yes, with
caveats worth keeping:

- **UICR writes**: done, via **direct NVMC register writes**, not
  `probe_rs::flashing`. ⚠️ The loader is the obvious tool and the wrong one:
  `FlashBuilder` keys staged data by address in a `BTreeMap` (insertion order
  discarded), materialises pages as full sector buffers pre-filled with the
  erase value, and programs ascending. A UICR write through it is one
  ERASEUICR plus one 4 KiB program **starting at the lowest address — the
  header word first**, inverting the commit-marker property §5 is built
  around. Its contract is a superset of what is needed, in exactly the place
  where the surplus is fatal.
- ⚠️ **`ERASEUICR`, not the debug erase sequence and not ERASEALL.** The
  *debug* erase sequence is what is unimplemented: nRF52's `ArmDebugSequence`
  implements `debug_device_unlock` and nothing else, so `sequence_erase_all`
  returns `None`. The only path that legitimately wants the whole chip is the
  APPROTECT unlock, which `LockedChip::erase_to_unlock` owns.

  ⚠️ **Corrected 2026-09-19:** an earlier draft of this bullet said "asking
  probe-rs for a chip erase fails at runtime". That is too broad. `probe-rs
  erase --chip nRF52840_xxAA` **works** on an unlocked chip — it goes through
  the flash algorithm's erase-all, not the debug sequence — and is what the
  XIAO migration uses. Only the locked-chip path needs CTRL-AP.
- ⚠️ **`CONFIG.WEN` is a mode, not a bitfield** — `Ren`/`Wen`/`Een` are
  0/1/2, and the erase registers are only honoured in `Een`. Writing the key
  with `Wen` set and then poking ERASEUICR is a silent no-op. Hence the
  `Writer`/`Eraser` typestate guards.
- **Probe-rs batches memory writes**, so the `Ren` restore must be flushed
  *after* it is issued, not before.
- **CTRL-AP `ERASEALL` recovery of a locked chip**: implemented as
  `LockedChip::erase_to_unlock`, **not yet exercised against a genuinely
  locked board** — see § Locking, which is why that stays deferred.
- **Build-order steps 1–3 (2026-09-13)**: fail-closed confirmations
  (`confirm.rs`), the identity block and stdout/stderr split (`output.rs`),
  named profiles with a 0600 credentials file (`store.rs`), `list`, and `verify`
  (`verify.rs`). API side: `DeviceSummary` moved to `api-types` with a
  required-but-nullable `lastSeen`, and the 409 carries typed `details`.

### Exercised on hardware (2026-09-19)

A DB-40 on a CMSIS-DAP probe, against the dev API, broker, receiver and
gateway. What the bench confirmed that no unit test can:

- **The whole loop closes.** `rotate` → the firmware reads the new key from
  UICR → radio → receiver → gateway → MQTT → the API opens the packet →
  `verify` passes. Proof that the key the tool writes is the key the firmware
  seals with and the key the API opens with.
- **The commit marker holds.** Across three writes, `CUSTOMER[0]` stayed
  `0x00014b48` — `"HK"`, version 1, pad 0 — while the eight key words changed
  each time.
- ⚠️ **Halting really is after consent.** `DHCSR` (`0xE000EDF0`) read
  `0x01050001` after every declined or refused run: `S_HALT` clear, `S_SLEEP`
  set — a running executor idling in WFI. Declining costs the board nothing.
- **The seq counter survives a re-key.** After a rotate the sensor resumed at
  seq 24576 — a 1024-reservation boundary — rather than restarting at zero,
  because `ERASEUICR` leaves application flash alone. Safe in this direction:
  a surviving counter under a *new* key is unremarkable (§ "Always full-erase").
- **All four key faults map end to end**, forced by editing `devices.key`:
  NULL → `MISSING`, a truncated blob → `INVALID`, `kek_ver = 0xFF` →
  `KEK_UNAVAILABLE`, a flipped ciphertext byte → `UNOPENABLE` — each with its
  own remedy from `verify`.
- **The AAD binding is not theoretical.** Changing a row's `device_addr` while
  leaving its key blob alone made the row read `UNOPENABLE`: the tag is bound to
  the address, so a blob cannot be moved between rows (§4 § AAD).
- **`verify` waits for a *newer* reading.** With `lastSeen` already at 14:18 it
  did not pass on that; it waited for 14:19:08.
- **Preconditions on live data**: `provision` on a registered board refuses by
  name; `rotate` and `verify` on an unregistered one point at `provision`; a
  non-TTY without `--yes` fails closed after printing identity and fleet, before
  any halt or mint. Aborts exit 1.

Still unexercised: `erase_to_unlock` against a genuinely locked board — see
§ Locking, which is why locking stays deferred.

## Superseded

- ⚠️ **§ "Always full-erase" is superseded.** `ERASEUICR` plus (optionally)
  clearing the two STORAGE pages from the ELF's own symbols gives the same
  determinism without the whole-chip erase, and avoids the XIAO
  `0x27000`-relink trap the same section flags. `ERASEALL` remains **only**
  the APPROTECT unlock path.
- ⚠️ **§1 step 3 ("ERASEALL (always)") is superseded** by the ordering under
  § Ordering above.
- ⚠️ **§ "Explicit subcommands, prompts only for destruction"** — the *rule*
  stands, the *mechanism* is superseded by § Preconditions above: the
  refusal belongs to the API, not to a guess from the record header.
- The seq-counter-address-in-UICR question is closed; see § Clearing the seq
  counter.
- "Node variants are one codebase behind Cargo features" is superseded by
  [firmware-variants.md](firmware-variants.md) — one firmware per *board*, sensors detected at
  boot.

## Build order

1. ✅ Preconditions, confirmations, output format — no dependencies, and they
   make the destructive paths safe to run tired.
2. ✅ `login`/`whoami` + config file — kills the argv token, gives the pre-flight.
3. ✅ `DeviceSummary` → `api-types` + `lastSeen`, then `verify` (L2) and `list`.
   `provision`/`rotate` also look the device up after reading the chip (name in
   the prompt, refusal before any halt or mint) and halt only after consent; the
   `/devices` token check now runs only on the `--unlock` path.
4. ✅ `firmware add`/`list` + the picker, then `--firmware` flashing with both
   guards, and seq clearing on its back. Done 2026-09-19. The image became the
   authority on its own layout: `elf.rs` reads `app_start` and
   `__storage_start`/`__storage_end` out of the artifact, so nothing stores a
   second copy of an address that could drift.
5. ✅ L0 and L1 — the boot log, and decoding a packet the board built. Done
   2026-09-19, earlier than "if and when": the case is real (network but no
   receiver in range), and L0 paid for itself the same afternoon by showing a
   wedged I²C bus that looked like a provisioning failure.
6. `lock`/`unlock`, after the firmware `Debug::Disallowed` change.

## Postponed (recorded 2026-09-13)

Deliberately left for later while building steps 1–3. Most have a `TODO`
comment at the place they would land.

- **A dedicated identity endpoint for `whoami`.** The token check calls
  `GET /devices` instead. That was kept on purpose: it proves auth, the router
  and the database in one request — everything the mint needs — where a
  token-compare endpoint could return 200 while the mint then fails. Revisit
  when the API gains users (`whoami` then reports a subject, and
  `Credentials.subject` / `expires_at` finally have a source), or when the fleet
  is large enough that listing it is a real payload.
- ✅ **`--probe <serial>`** (2026-09-19). Every command that touches a board
  takes it, and `AmbiguousProbe` names the serials to choose from.
- 🔶 **HTTPS enforcement** (2026-09-19). Refusing `http://` to a non-loopback
  host is built — see §2 § HTTPS, and the flag not to add. ⏳ `HOMESCOPE_CA_CERT`
  pinning is not, so a self-signed API still needs an SSH tunnel.
- **A mint whose response never arrives.** If the connection drops after the API
  commits a key but before the response lands, the tool reports a failed step it
  cannot tell apart from "nothing happened" — yet the registry now holds a key
  the board never received. Re-querying the device and comparing
  `key_valid_from` would turn that into the proper "device is dark, rotate
  again" message. Graceful HTTP shutdown in the API narrows the same window from
  the other side ([api-graceful-shutdown.md](api-graceful-shutdown.md)).
- ✅ **`verify` surviving a network blip** (2026-09-19). Polling *is* the retry,
  so a transient failure now just means that tick learned nothing. A refusal
  still fails immediately — a 401 does not fix itself. ⚠️ The timeout
  distinguishes "no reading arrived" from "could not reach the API", because
  the first sends you to look at the board and the second does not. The initial
  lookup still fails fast, deliberately: without a baseline there is nothing to
  wait for.
- **`verify`'s default timeout.** 180 s assumes today's 60 s cadence, and has to
  grow when production moves to 1–5 min between bursts.
- ✅ **A warning before rotating a `KEK_UNAVAILABLE` device** (2026-09-19).
  Printed above the existing prompt rather than made into a second one: it is
  advice, and rotating anyway is sometimes right.
- **`provision --verify`.** Chaining the L2 wait onto a successful provision.
  `provision … && verify` already does it, since `verify` reads the address
  from the probe.
- **Database-backed API tests** (`#[sqlx::test]`). The `lastSeen` lateral join,
  and the name-lookup race that turns a 409 into a 500 when the conflicting row
  is deleted mid-request, are verified only by reading and by one run against
  the dev database.
- **API-side hardening for the admin routes:** rate limiting, and an
  `ADMIN_API` gate or a separate listener (§3 § Securing it).

---

## Key facts that make this easy

- **FICR is readable over SWD with no firmware flashed.** `DEVICEADDR` is
  memory-mapped flash; a blank chip straight out of the reel answers a probe
  read. So there is **no chicken-and-egg**: read the address first, register
  the device, then flash. (Only APPROTECT blocks this, and it's off from the
  factory.)

  ```
  FICR base        0x10000000
  DEVICEADDRTYPE   0x100000A0
  DEVICEADDR[0]    0x100000A4   (low 32 bits)
  DEVICEADDR[1]    0x100000A8   (high 16 bits in the low half-word)
  ```

  ```bash
  probe-rs read b32 --chip nRF52840_xxAA 0x100000A4 2
  ```

  ✅ Baked into `provision/src/chip.rs` and read by every command that touches
  a board.

- ⚠️ **Register the *advertising* address, not raw FICR.**
  `firmware/board/src/lib.rs` applies `b5 | 0xC0` to make it a random-static
  address. The provisioning tool must apply the identical transform.
  ✅ *Done:* the derivation is `DeviceAddr::from_ficr` in `common`, used by both
  the firmware and the tool, so the two cannot disagree.

## 1. Preferred flow for adding a new device

⚠️ *Historical sketch.* The flow as built is §0 § Ordering, and `--site` /
`--room` wait on [site-room-topology.md](site-room-topology.md).

Runs on the **workstation with the probe attached** (the bench where firmware
is flashed today), *not* on the Pi:

```
homescope-provision --name "kitchen" --site home --room kitchen

 0. pre-flight: API reachable + token accepted        ← see "no safe abort", §6
 1. probe-rs: read FICR DEVICEADDR → DeviceAddr::from_ficr() (shared common fn)
      (+ read UICR / detect APPROTECT → is this a working device? confirm)
 2. POST {API_URL}/devices {deviceAddr, name, site, room}
      → API generates the 32-byte key, inserts the row, returns the key ONCE
      ← LAST NON-DESTRUCTIVE STEP on an unlocked chip
 3. ERASEALL (always — see §2 "Always full-erase")   ⚠️ SUPERSEDED: ERASEUICR only
 4. write the UICR record — key words FIRST, header word LAST (see §5)
      (+ REGOUT0 = 3.0 V on custom PCB)
 5. read UICR back, verify byte-for-byte   ← before flashing; see below
 6. cargo flash / probe-rs download the generic firmware image
 7. print the DeviceAddr so the physical unit can be labelled
```

Locking is deliberately **not** a step here — it is a separate subcommand,
runnable later on an already-deployed device (§6):

```
homescope-provision lock            # write UICR.APPROTECT, reset, verify locked
```

Step 2 sits where it does deliberately: network, DNS, TLS and auth are the
things most likely to fail, and on an unlocked chip they fail *before*
anything has been erased, so an aborted run leaves a working device untouched.
(That property is lost on a locked chip — §6.)

Step 5 is not optional: UICR bits can't be rewritten without an erase, so a
botched write is unrecoverable in place. Verify before committing to the app
flash.

**Provision before sealing an enclosure.** A sealed unit with unreachable SWD
pads can be UF2-reflashed but **not** re-keyed — UF2 cannot write UICR. If a
node is meant to be sealed permanently, use the flash-page variant instead
(see §5 alternatives), which the firmware itself can write over USB-CDC.

**Re-provisioning after a chip erase = mint a NEW key** (`POST
/devices/{addr}/rotate-key`), never restore the old one. ⚠️ A chip erase also
wipes the **seq counter** pages, so the sensor restarts at seq = 0 — and the
air nonce is derived from `seq` with no random component. Same key + seq
restarting from 0 = **nonce reuse**, which for ChaCha20-Poly1305 is a break,
not a degradation (XOR of two ciphertexts sharing a keystream reveals the
plaintexts and exposes the Poly1305 auth key). A fresh DEK makes it impossible
by construction. Same reasoning as the seq counter's jump-ahead: a counter
that can rewind is a security bug.

The DB is the source of truth for *what key a device should be using now* —
**not** a recovery vault to restore from. See §3: no read-back endpoint.

## 2. New binary crate: `homescope-provision`

A **workstation CLI**, in the host workspace (its own crate, or a second bin
in `host-util`). Responsibilities: FICR read → API registration → UICR write
→ verify → firmware flash.

**Explicitly rejected: driving the probe from the API.** The earlier sketch
had SWD attached to the API host with an HTTP endpoint orchestrating it. No:
the API is a rootless container on the Pi; that would mean USB device
passthrough, `probe-rs` as a runtime dependency, and an HTTP endpoint that
writes firmware — an endpoint that writes firmware is an endpoint that writes
firmware. The API's job is the registry; the probe stays on the bench.

Implementation notes:

- probe-rs as a **library** (`probe-rs` crate), not a subprocess. Parsing CLI
  output for the read-back verification result is exactly the fragility you
  don't want in the step whose whole job is proving the key landed.
- **The workstation never holds the KEK.** That is the point of the API
  generating and sealing: the CLI holds one plaintext DEK for a few seconds
  and drops it. Don't be tempted to give it a DB connection and let it seal
  the row directly once it has one — that would put the KEK on every bench.
- Uses the shared `DeviceAddr::from_ficr()` from `common`.

### probe-rs spike — do this before writing any of the CLI

✅ **ANSWERED 2026-08-25 — see §0 § What is already built.** Both came out yes.
UICR writes go through **direct NVMC register writes**, not probe-rs's flash
loader (which would reorder the record and write the header word first);
recovery of a locked chip is implemented but **not yet exercised against a
genuinely locked board**.

Two capability questions, both "does the tooling actually implement this for
this target", both cheap to answer with a board and twenty minutes, and both
able to invalidate the design if the answer is no:

1. **UICR writes** on nRF52840 (flash-loader coverage varies by version).
   Fallbacks: `nrfutil`/`nrfjprog --memwr`, or a one-shot provisioning
   firmware that receives the key over RTT/USB-CDC and writes UICR itself via
   NVMC. That fallback is a *completely different tool*, so finding out after
   building the probe-rs version would hurt.
2. **CTRL-AP `ERASEALL` recovery of a locked chip** (§6). Discovering this
   isn't supported *after* locking a board is a bad afternoon.

### HTTPS, and the flag not to add

HTTP over TLS, not a custom protocol on a raw TLS socket. A custom protocol
would need framing, versioning, error signalling and auth designed from
scratch — four solved problems, four ways to be subtly wrong — and the case
for one needs high frequency, low latency, streaming, or a constrained client.
This is one request per device, once, from a workstation with axum already on
the other end.

⚠️ **Do not add an `--insecure` / `danger_accept_invalid_certs` flag.** Not
"add it and document it". It gets used once for the dev stack and then lives
in a shell history forever, and a provisioning tool that skips cert
verification hands device keys to whoever is on the path. To accept a
self-signed cert for your own deployment, take a CA path from config
(`HOMESCOPE_CA_CERT`) and pin to it — a strictly different thing from
disabling verification.

Dev-stack carve-out: permit `http://` **only when the host resolves to
loopback**. That rule cannot be misapplied to production by accident, which is
the property a safety valve needs.

✅ **Enforced 2026-09-19** in `store::parse_url`, which every path that builds
an `ApiTarget` passes through — including a URL read back out of `config.toml`,
because that file is one a person edits and a check only on the way in is a
check you can hand-edit around. `https://` always; `http://` only for a host
that is *literally* loopback (`localhost`, `127.0.0.0/8`, `::1`), compared
whole rather than by prefix — ⚠️ `localhost.example.com` is a name anyone can
register, and a `starts_with` there would hand a device key to its owner in the
clear. The refusal names the SSH tunnel, since that is the answer for a
deployment without a certificate.

⏳ `HOMESCOPE_CA_CERT` pinning is still unbuilt, so a self-signed API needs the
tunnel rather than its own certificate. There is still no `--insecure` flag.

Client-side handling of the returned key — same class of leak as the API side:

- `serde` materialises it into a `String`/`Vec<u8>` before it reaches a
  `DeviceKey`. Wrap the intermediate in `Zeroizing`. (✅ As built, the string is
  zeroized straight after decoding instead.)
- Never print it. Not under `--verbose`, not on error. `DeviceKey` has no
  `Display` and a redacted `Debug`, so lean on that rather than re-deriving it.

### Always full-erase

⚠️ **SUPERSEDED 2026-08-26 — see §0 § Superseded.** `ERASEALL` is now *only*
the APPROTECT unlock path. `ERASEUICR` plus clearing the two STORAGE pages
(located from the firmware ELF's own symbols) gives the same determinism
without the whole-chip erase, and sidesteps the XIAO relink trap flagged two
subsections below. Kept for the reasoning, which is still the reasoning.

`ERASEALL` on every provision and re-provision, unconditionally. Reasons in
order:

- **Determinism.** Every provisioned device starts identical — blank UICR, seq
  at zero, fresh key, fresh epoch — rather than "whatever was there".
- It folds the UICR erase into the same operation, so the
  `ERASEUICR`-also-resets-`REGOUT0` gotcha disappears: `REGOUT0` is written
  fresh regardless.
- It is the only path that works once APPROTECT is on (§6), so it is the code
  path needed anyway. Making it the *only* path means it gets exercised every
  time instead of being discovered broken during a recovery.
- Flash endurance is 10k cycles against a handful of provisionings. Irrelevant.

**Losing the seq counter is fine, and not a cost.** A reset counter is only
dangerous under a *reused* key, which "always mint a new key" makes
unreachable; a surviving counter under a new key is unremarkable. And
`key_valid_from` means the ingest replay check accepts the restart
([ingest-db-error-handling.md](ingest-db-error-handling.md)). One less variable.

**Detection for safety, erase unconditionally.** Read UICR (or detect a locked
chip) *first* and require confirmation before wiping a board that already
holds a valid record — then erase regardless. That split is better than
erasing conditionally: one code path, and the operator still gets the warning.

### Explicit subcommands, prompts only for destruction

⚠️ **Rule stands, mechanism SUPERSEDED 2026-08-26 — see §0 § Preconditions.**
"`provision` refuses with *already registered as 'kitchen'*" cannot be done
from the chip: the chip knows only that *a* record exists, not whether the
address is in the fleet, and the two come apart for a chip-erased sensor and
for a board from another deployment. The refusal belongs to the API's 409/404;
the chip's record drives the *confirmation*.

The two-endpoints decision in §3 is about the **server** not inferring intent.
The **CLI** has a human at the keyboard and the board in front of it, so
detecting and reporting is fine — it isn't silent. But keep the two jobs apart:

- **The subcommand chooses the operation.** `provision` on an
  already-provisioned board refuses with a useful message ("already registered
  as 'kitchen', use `reprovision`") rather than quietly switching. Not friction
  for its own sake: a wrong subcommand usually means the wrong board is
  attached, and refusing is how that gets noticed.
- **The prompt guards the destruction** — `ERASEALL` against a board holding a
  valid record or an active lock, whichever subcommand got there.

Merging them produces a prompt on every run, which is how prompts stop being
read. Fail closed with no TTY: non-interactive requires the explicit subcommand
plus `--yes`.

### UF2 is retired — and the trap that comes with it

✅ **DONE 2026-09-19.** The second option below was taken: the XIAO relinks at
`0x0` and its factory MBR, SoftDevice and bootloader are erased, so every board
shares one layout and `board-*` selects only pins and the ADC. `flash_uf2.sh`
and `tools/uf2/` are gone. Two things this section did not foresee: the XIAO
also reserved 128 KiB of RAM for the SoftDevice it never starts — which is why
the receiver could not be built for it — and migrating a board moves its seq
counter from `0x000F2000` to `0x000FE000`, so it must be chip-erased and
**re-keyed**, never merely reflashed. See `docs/flashing.md`.

The bootloader **cannot write UICR**, so it can never be the recovery path for
the thing that actually matters. The DB-40 and the custom PCB don't have one
at all, and on the XIAO it costs ~156 KB to an MBR plus an S140 the firmware
never starts. Any device that can be provisioned can be flashed.

⚠️ **Full-erase provisioning requires the app to link at `0x0`.**
`memory-xiao.x` puts it at `0x27000` because the bootloader sits below. Erase a
XIAO and flash a `board-xiao` build and flash `0x0` is `0xFF` — the CPU reads
its initial SP and reset vector as `0xFFFFFFFF` and locks up. The board looks
dead. Recoverable, but it will cost an evening.

Two ways out: exclude XIAOs from the erase path, or relink XIAO to `0x0` and
retire the bootloader for good. Prefer the second — it collapses the two
linker scripts into one and leaves `board-xiao` meaning only what it should
mean, which is pins. (`flash_uf2.sh` dies with it. It has already been erased
once by accident, which is a fair measure of how much it is worth.)

### No ack step

Nothing to confirm back to the API after a successful write. An ack would
record that the *CLI thought* it worked; what matters is whether the device is
producing authenticated readings under the new key — and that is already
queryable with the replay check's own query:

```sql
SELECT MAX(seq) FROM readings WHERE device_id = $1 AND time > key_valid_from
```

NULL means "registered but has never reported under this key", which is
exactly the mid-provisioning state an ack column would have tracked, except it
proves the whole chain rather than one step of it. The UICR read-back covers
the other half — the device demonstrably has the key before you leave the
bench. No gap left for an ack to fill.

Corollary, and the hard rule that makes it safe: **the plaintext key crosses
an interface exactly once.** Any failure at any step ⇒ re-run ⇒ a new key. No
resume, no recovery of the in-flight key, no read-back (§3).

## 3. API: key generation + registration endpoint

**The API generates the key**, not the CLI. This is the API-token issuance
pattern: DB is the source of truth, the secret is returned exactly once in the
creation response, and rotation is an explicit separate operation rather than
an accidental side effect of re-running the tool.

- `POST /devices` `{deviceAddr, name, site, room}` → 201 with the key in the
  response body, **once**. Key from a CSPRNG (`rand::rngs::OsRng` /
  `getrandom`), 32 bytes.
- **409 on an already-registered address.** Re-running the CLI must not
  silently orphan a deployed sensor by minting a second key.
- `POST /devices/{deviceAddr}/rotate-key` — explicit, separate, returns the
  new key once. **404 if the address is not registered.** Rotation on-device
  means erase+rewrite UICR (see §5).

**Two endpoints, not one that infers intent from state** (settled 2026-08-07).
The tempting simplification is a single endpoint that creates when the address
is unknown and rotates when it is known. Rejected: consider a mis-read address
— wrong board on the bench, probe glitch, two devices connected. Under
inference, "reprovision" silently creates a phantom row, and worse, "provision
a new board" silently **rotates the key of a working sensor**, which goes dark
with no indication why.

Silently rotating a live device's key is destructive, and destructive
operations must be explicit — the same rule that gates `ERASEALL` behind a
confirmation. Letting the server infer intent from its own state undoes that
gate. So the client states intent and the server verifies it matches reality;
the two handlers share one internal mint-seal-store function and differ only
in which mismatch they refuse. The resulting friction is *useful*: a re-run
after a half-failed provision returns 409, which is true and actionable
("already registered, use rotate"), rather than silently succeeding.

**`name`**: required on create (the schema already says `NOT NULL`, and a
device you cannot find in Grafana is not usefully provisioned). Optional on
rotate, updating only when supplied — acceptable *for now*, since without
management endpoints the alternative is hand-written SQL. Move it to
`PATCH /devices/{deviceAddr}` when those land. The rule to hold: the key
endpoint is the one that returns a secret, so its surface stays as small as it
can be. Don't let it drift into being the general device-mutation endpoint
just because it was the one that existed first.

**Response-body hygiene**: the key is in the response body, so nothing may log
bodies on that route. `tower-http`'s `TraceLayer` doesn't by default — put a
comment on the handler saying why that must stay true.
- **`GET /devices/{deviceAddr}/key` — decided 2026-07-20: never build it.**
  Rotate-only. Reasons, strongest first: ① restoring an old key after a chip
  erase risks **nonce reuse** (§1 — the erase resets the seq counter too);
  ② a read-back endpoint is a permanent fleet-exfiltration path, silently
  usable from anywhere by whoever holds the admin token — not building it
  beats building it carefully; ③ it saves no work, since re-provisioning
  rewrites UICR regardless.
  Consequence: **a DEK exists in cleartext at exactly two moments** —
  generation (the `POST` response) and the UICR write. Never again on any
  external interface. The API still reads keys *internally* at startup to
  populate `DeviceRegistry`; it just never *serves* one.

### Securing it

This is the first non-read-only surface in the API, and it hands out fleet
secrets. Minimum bar before it exists:

- **Admin bearer token**, generated once by `deploy/deploy.sh` like the other
  secrets. Not a password, not basic auth, not a login flow — see below.
- **Do not expose it on the public/VPN-facing listener** if the HTTP server
  ever gains one. Options, in order of preference: bind admin routes to a
  separate loopback-only listener; or a separate axum `Router` merged only
  when an `ADMIN_API=true` env is set, so the production container can run
  without the routes existing at all.
- Rate-limit + log every call (address, outcome, timestamp). Provisioning is
  low-frequency by nature; anything bursty is an attack.
- **HTTPS, always** (hardened 2026-08-07 — was "TLS or a trusted path"). The
  response body *is* the key, so there is no network on which plaintext HTTP is
  acceptable, LAN included. Client-side cert rules and the loopback-only
  `http://` carve-out for the dev stack are in §2; the short version is that
  there is no `--insecure` flag.
- **Don't ship the endpoint unauthenticated, even temporarily.** A "temporary"
  unauthenticated key-minting endpoint outlives the intent, and its blast
  radius is the whole fleet. The admin bearer token needs no user system and is
  ~10 lines — for a single-operator deployment it is the right amount of auth,
  not a stopgap. If even that isn't ready when testing needs to happen, bind
  the listener to loopback and reach it over an SSH tunnel: costs nothing and
  is honest about what is protecting it.

⏳ *As of 2026-09-13:* the admin routes share the API's single `HTTP_BIND`
listener — no `ADMIN_API` gate and no rate limiting yet. Requests are traced by
`TraceLayer`, which does not log bodies.

### Auth design (settled 2026-08-07)

**A static bearer token, not a login flow.** A login flow is not merely more
work here, it is *worse*: it exchanges a password for a token, so there are now
two secrets instead of one and the weaker of them is human-chosen. It adds
password hashing, issuance, expiry and refresh, and ends up bounded by the
password. A 256-bit random token has none of those parts and is not guessable.

Refresh tokens bound the damage when access tokens travel widely — browsers,
many services, logs. This one travels from one CLI to one API. Nothing to
bound. Revisit only when there are actually multiple users to distinguish.

- **Generate**: 32 bytes from a CSPRNG, hex or base64. **Not a UUID** — v4 has
  ample entropy, but UUID is a format for uniqueness rather than
  unpredictability and not every generator on the path is cryptographic. Same
  effort either way; one of them leaves no question.
- **Deliver as a podman secret, named by `ADMIN_TOKEN_PATH`** — *not* an env
  var. ⚠️ Earlier drafts of this note said `ADMIN_TOKEN` from the environment,
  which contradicts the KEK reasoning in §4: env vars land in
  `/proc/<pid>/environ`, child processes and `podman inspect`. `setup_kek` in
  `deploy/deploy.sh` is the template; the machinery already exists.
- **Compare in constant time** (`subtle::ConstantTimeEq`). `==` on byte slices
  short-circuits.
- **Do not hash it at rest.** Server-side hashing helps when the store is more
  exposed than the process — a user table, say. This sits in a podman secret
  next to the KEK, which is the far more valuable target. It buys nothing here
  and adds a step to get wrong.
- **Client side: never take the token on the command line.** argv is visible in
  `ps` and lands in shell history. A small `homescope-provision auth`
  subcommand reads it from **stdin** and writes `~/.config/homescope/config.toml`
  at mode 0600 alongside the API URL. That is the useful 5% of a login
  command without the other 95%. ✅ Built as `login`, reading stdin with echo
  off; the token lands in a 0600 `credentials.toml` beside a 0644
  `config.toml` (§0 § Auth).
- **Log every call** — address, outcome, timestamp. Never the token.
- **Revocation** = regenerate the secret + restart. Correct amount of ceremony
  for one operator.

**Forward-compatible, so it isn't throwaway.** When users eventually exist, the
same `Authorization: Bearer` header carries a session token and the only thing
that changes is the middleware: "compare against the static one" becomes "look
it up". The CLI does not change at all. This builds the client half of the real
system and stubs the server half, which is the right order.

Pair it with the network scoping above (admin routes bound to the VPN/LAN
interface, or gated behind `ADMIN_API=true` so the production container can run
without the routes existing). Then the token is defence in depth rather than
the only thing standing there — and given the two-house VPN rollout is coming
anyway, "reachable only from the VPN" is free.

## 4. Encrypting device keys at rest in the DB

✅ **Implemented 2026-08-06** — `api/src/devices/keys.rs`. Everything below is
the design as built; deltas from the original sketch are noted inline.
(Originally flagged 2026-07-20 as "not previously considered".)

Still open in this section: the duplicate-key-material check (§ *Storage*
below is silent on it — the O(n²) pass over the loaded ring, guarding against
a "rotation" that pastes the same key under a new generation; the KEK-file
parser today rejects only a duplicated generation *number*). The one-shot
backfill binary was dropped — see § Rolling it out.

The device key cannot be hashed: the API needs the plaintext to decrypt AEAD
payloads. So `devices.key` is a plaintext secret sitting in the database —
**and in every `deploy/backup-db.sh` tarball**. A leaked backup would be a
full fleet compromise (symmetric AEAD ⇒ verify = forge ⇒ an attacker could
forge readings for every device).

Standard answer: **envelope encryption (KEK/DEK)** — the pattern that turns
"protect N secrets" into "protect 1 secret".

- **DEK** (Data Encryption Key) = the per-device 32-byte AEAD key; encrypts
  measurements. Per-device so a stolen sensor can't forge the fleet.
- **KEK** (Key Encryption Key) = one master key whose *only* job is wrapping
  DEKs. Lives in the API's environment / secrets file, generated once by
  `deploy.sh`, **never in the DB, never in a backup**.

### What it protects against (be precise)

| Scenario | Helps? |
|---|---|
| `backup-db.sh` tarball leaks (cloud sync, old drive, wrong repo) | ✅ keys inert — **the point** |
| DB dump shared for debugging / restored into dev | ✅ |
| Attacker gets DB read access (injection, exposed port, stolen creds) | ✅ KEK isn't in the DB |
| Attacker fully roots the Pi | ❌ they get env *and* DB |
| Sensor physically stolen | ❌ irrelevant — that key is in UICR |

The asymmetry being exploited: **backups travel, the KEK doesn't.** This is
not a defense against someone who owns the box.

### Storage: one `BYTEA` column on `devices`

```sql
ALTER TABLE devices ADD COLUMN key BYTEA NOT NULL;
```

```
[ver: 1][kek_ver: 1][nonce: 24][ciphertext: 32][tag: 16]   = 74 bytes
```

- **XChaCha20-Poly1305** (same crate as the packet AEAD, different type). The
  24-byte nonce is *random* here — there's no seq counter at rest — and 192
  bits removes any birthday-bound argument. Deliberately a different
  primitive choice from the air packet, where the persisted seq makes the
  12-byte nonce provably unique.
- `ciphertext` is 32 B because ChaCha20 is a **stream** cipher (output length
  = input length, no padding). `tag` is always 16 B (RFC 8439, no truncation).
- **One column, not three.** Nonce/ciphertext/tag are one cryptographic value
  with one lifetime; nothing queries on them. Splitting invites states the
  schema permits but the crypto forbids.
- **Not a separate table.** Always-joined 1:1 = join tax with no payer;
  column-level `GRANT`/`REVOKE` handles "Grafana must not see keys"; `NOT
  NULL` makes "registered ⇒ keyed" a schema fact. *(The one real argument for
  a `device_keys` table is rotation history — keeping an old DEK valid for
  in-flight packets. With UICR provisioning you're standing at the device with
  a probe, so the window is seconds. Revisit only if that changes; the blob
  format wouldn't need to.)*

### No key history — overwrite in place

Decided 2026-07-20. **No history table, no `revoked_at`, no tombstones.**

The structural reason: the DEK protects data *in flight* only. The API
decrypts at ingest and stores **plaintext readings** in the hypertable, so a
key guards nothing that still exists the moment its rows land. (Contrast: if
`readings` held ciphertext decrypted on read, key history would be a hard
requirement — discarding a key would destroy every row it touched. That is
where the "never delete a key" instinct comes from, and it is not this
architecture.) The only other candidate use, decrypting in-flight packets
during a rotation, is a seconds-long window while you stand at the device with
a probe; `ON CONFLICT DO NOTHING` absorbs it.

Worth adding, and often confused with key history — metadata, not secrets:

```sql
ALTER TABLE devices ADD COLUMN key_valid_from TIMESTAMPTZ NOT NULL DEFAULT now();
```

Answers the debugging question that *will* come up: "why did this sensor go
dark?" → "re-keyed on the 14th, reflash didn't take."

**Renamed from `key_updated_at` (2026-08-07).** It stopped being only
metadata: the ingest replay check uses it as the **key epoch boundary**, so it
now carries a load-bearing meaning that the old name actively contradicted.
See [ingest-db-error-handling.md](ingest-db-error-handling.md) § "Per-device seq check" — the query is
`MAX(seq) FROM readings WHERE device_id = $1 AND time > key_valid_from`, and a
freshly provisioned device gets NULL, which is what lets its counter restart
at zero without being rejected as a replay.

"Updated" describes a write to the *column*. "Valid from" describes the
lifetime of the *key*, which is the thing the query is actually asking about,
and the two come apart at KEK rotation (below). One column, not two: the only
extra fact a `rewrapped_at` would hold is already derivable, since `kek_ver` is
byte 1 of the blob — `SELECT count(*) FROM devices WHERE get_byte(key, 1) <> N`
answers "which rows still need re-wrapping" authoritatively rather than by
proxy. A column duplicating a derivable fact only creates a way for the two to
disagree.

### Versioning

- `ver` — wrapping **format** (cipher, nonce length, AAD construction). The
  one byte that genuinely can't be retrofitted: without it, "is byte 0 a
  version or the start of a nonce?" has no answer.
- `kek_ver` — which KEK generation wrapped this row. **1 byte, starting at
  1** — a sequential generation counter, not a random identifier (Tink uses 4
  bytes because its key IDs *are* random). 0 stays reserved-invalid so an
  all-zeros blob fails loudly instead of parsing as generation 0.
- Two version numbers, two charters — and neither is `SensorPacket::ver`.
  The UICR record (§5) carries a *third*, independent one.

Strictly, `kek_ver` is optional: AEAD already identifies the key, since the
wrong KEK gives a tag failure rather than garbage, so a rotation could just
try `current` then `previous`. It's kept anyway — it's one byte, it makes
rotation a zero-downtime operation instead of a stop-script-start, and it's
the standard shape worth learning.

### AAD = `blob[0..2] ‖ device_addr` (8 bytes)

**Build it from the stored header bytes, not from re-serialized fields.**
Re-serialization can silently disagree between wrap and unwrap, and a header
field added later would otherwise land unauthenticated by default;
"everything before the nonce" is self-maintaining.

- `device_addr` — **the load-bearing field.** Every row is wrapped under the
  same KEK, so without it an attacker with DB write access moves device A's
  blob into B's row: unwraps cleanly, valid tag, no error, and the API now
  holds the wrong key for B. Classic confused deputy — the tag proves *"made
  by the KEK holder"*, not *"belongs to this row"*. Use `device_addr`, not
  `devices.id` (an implementation detail a restore could renumber).
- `ver` / `kek_ver` — self-enforcing (a flip → wrong format or wrong KEK →
  tag fails anyway). Included because it's free and removes the need to
  re-derive that argument.
- **Nonce is *not* in the AAD** — it's already an AEAD input, bound by
  construction.

The test for any candidate field: *would tampering with it cause harm the tag
wouldn't otherwise catch?*

### Runtime mechanics

**As built**: not one env var per generation — a single file named by
`KEK_PATH`, holding every generation plus `current`:

```
# rotated 2026-08-06
current = 2
1 = <64 hex chars>     # kept only while rows still reference it
2 = <64 hex chars>
```

✅ **Delivered as a podman secret in production** (2026-08-06, `deploy.sh`
`setup_kek` + `Secret=` in `api.container`): mounted on tmpfs at
`/run/secrets/kek`, `KEK_PATH` names the path. `podman secret create -` reads
stdin so the generated key never touches a filesystem outside podman's storage,
and `podman secret exists` keeps the converge idempotent. Dev uses a plain file
(`api/.env.default` → `KEK_PATH=./kek`); the app reads a path either way, so
there is no code difference between the two.

One file means rotation is an **atomic replace + restart** rather than editing
several unit-file variables and hoping none was missed. `#` comments and blank
lines are skipped; generation numbers are explicit, never positional (see the
versioning note above). ⚠️ **`dotenvy` is deliberately not used** — it loads
into the process environment, which would put the KEK back in
`/proc/<pid>/environ` and `podman inspect`, the exact exposure the file
avoids. `.gitignore` and `.containerignore` exclude `kek` / `*.kek`.

- Wrapping (new provisioning) always uses `current`.
- Unwrapping dispatches on the row's `kek_ver`.
- **Startup fails loudly** if the KEK is missing, fails to authenticate, or a
  row references an unloaded generation. A silent skip = one device quietly
  stops ingesting; a silent plaintext fallback defeats the whole thing.
  ⚠️ *As built, a bad row is skipped rather than fatal — but not silently.* A
  missing or unparseable KEK file still stops startup. A row whose key will not
  open is logged and counted in the startup summary, and `GET /devices` reports
  it by cause (`MISSING`, `INVALID`, `KEK_UNAVAILABLE`, `UNOPENABLE`), which
  `homescope-provision list` shows. One bad row no longer takes ingest down for
  the whole fleet.
- `DeviceRegistry` unwraps at load time and holds plaintext DEKs in memory
  only.
- KEK rotation = add the new generation to the file, re-wrap all rows, bump
  `current`, drop the old line. No downtime.
- ⚠️ **No error raised while parsing the KEK file may echo a *value*** — field
  names and line numbers only. The file *is* the KEKs, and errors end up in
  logs. There is a test asserting this (`errors_never_echo_key_material`).
- ⚠️ **Redact in Rust**: newtype the key with a hand-written `Debug` printing
  `Key(<redacted>)` (a `#[derive(Debug)]` will eventually reach a `tracing`
  line) and `zeroize` the plaintext on drop.

### Two rotations, don't conflate them

- **DEK rotation** — a device needs a new key (provisioning, re-provisioning
  after chip erase, suspected tampering). One row, `UPDATE` in place, write
  UICR, reflash. No ceremony, no history kept. This is the common case, and
  it is the *only* re-provisioning path (§1, §3 — old keys are never
  restored).
- **KEK rotation** — the master secret leaked; re-wrap every row. Rare,
  break-glass. This is what `kek_ver` exists for.

⚠️ **The re-wrap must preserve `key_valid_from`.** It rewrites `key` while the
DEK underneath is unchanged, so touching the timestamp would move the ingest
replay check's epoch boundary past every existing reading: `MAX(seq)` returns
NULL and each device accepts one arbitrary seq before the window closes again.
A maintenance operation that has nothing to do with device keys would silently
disarm replay protection fleet-wide. This is exactly the confusion the rename
from `key_updated_at` was meant to make impossible — the re-wrap does not
change when the key became valid, so the `UPDATE` should not name the column
at all.

### Operational rule

⚠️ **Back up the KEK somewhere other than the database backups.** Same drive
means one theft gets both — the exact scenario this defends against. And
losing it is unrecoverable in the walk-to-every-sensor-with-a-probe sense, not
the restore-from-backup sense.

Result: a leaked DB dump is inert without the KEK. This is exactly the model
cloud KMS implements; doing it in ~30 lines here is the cheap version.

### Rolling it out: expand / migrate / contract

The column landed **nullable** (phase 1). This is the standard three-phase
shape, and the reason it is three is that schema changes and code deploys are
never atomic — there is always a window where one is a step ahead, including
during a rollback.

1. **Expand** ✅ — `key BYTEA` nullable, `key_valid_from NOT NULL DEFAULT now()`.
   Code tolerates NULL: `store` carries it as an `Option`, and
   `keys::open_key_column` classifies it as `KeyFault::Missing`, so the
   registry skips that device and `GET /devices` reports `MISSING`.
2. **Migrate** ⏳ — **no backfill script** (decided 2026-08-07). The earlier
   plan was a one-shot binary sealing a placeholder key into every NULL row,
   *not* SQL, since sealing needs the KEK and each row's `device_addr` as AAD.
   Dropped: with one deployed sensor, "backfill" and "provision" are the same
   operation on the same board, and the placeholder key would exist only to be
   overwritten minutes later by `homescope-provision` — a key nothing could
   ever use, since there is no read-back path by design (§3). Provisioning the
   device sets `key` and `key_valid_from` and takes the NULL count to zero,
   which is the actual precondition for phase 3. Revisit only if rows ever
   exist for hardware that cannot be reached with a probe — and note that is
   already impossible-by-construction for a sealed enclosure, because UF2
   cannot write UICR (§5).
3. **Contract** ⏳ — `SET NOT NULL`, drop `KeyFault::Missing`, and `DeviceRow.key`
   becomes `Vec<u8>`. The wire's `DeviceKeyStatus::Missing` stays, for clients
   older than the change.

⚠️ **Do not commit phase 3 until no row in production has a NULL key.** With
the backfill dropped, provisioning is what takes that count to zero.
`RUN_MIGRATIONS=true` in `api.container` applies everything pending on the next
deploy, so an unrelated release would run `SET NOT NULL` against NULL rows and
the API would fail to start. The rule generalises: *never commit a
migration whose precondition is not already true in production.*

On a table large enough to matter, avoid the `ACCESS EXCLUSIVE` full scan:

```sql
ALTER TABLE devices ADD CONSTRAINT key_not_null CHECK (key IS NOT NULL) NOT VALID;
ALTER TABLE devices VALIDATE CONSTRAINT key_not_null;  -- SHARE UPDATE EXCLUSIVE
ALTER TABLE devices ALTER COLUMN key SET NOT NULL;     -- now instant
ALTER TABLE devices DROP CONSTRAINT key_not_null;
```

Plaintext column is the acceptable *shortcut* for local dev only, and only if
labelled as one.

## 5. Where the secret lives on-device: UICR

**Decision: `UICR.CUSTOMER[0..9]` at `0x10001080`** — a 36-byte record holding
the 32-byte key, inside the 128-byte CUSTOMER block (`0x10001080`–`0x100010FC`);
layout below.

Naming: **FICR** = Factory Information Configuration Registers (read-only,
factory-programmed — `DEVICEADDR`, `DEVICEID`). **UICR** = User Information
Configuration Registers at `0x10001000` (writable non-volatile user space).
There is no "CICR".

### Record layout (settled 2026-08-06 — reader implemented)

A small versioned record, not a bare key. The firmware half lives in
`firmware/board/src/chip.rs` (`chip::device_key()`, behind the `device-key`
feature); `homescope-provision` is the only writer.

```
0x10001080   b"HK"       2 B   magic
0x10001082   version     1 B   currently 1
0x10001083   0x00        1 B   padding
0x10001084   key        32 B   key byte i at 0x10001084 + i
```

36 bytes of the 128-byte `CUSTOMER` block, 9 of its 32 words. Header first,
for the same reason `ver` precedes `seq` on the air packet: a version field
has to be readable without already knowing the layout it describes.

**Byte order.** Words are read little-endian, so a hex dump of the region
shows the key in order. This is the one part of the contract that nothing
type-checks — the writer is a different crate and the reader is on a target
the writer's tests never build for. A tool that groups bytes into words the
other way produces a device whose packets fail their AEAD tag with no other
symptom. Same failure class as `packet::cipher`'s AAD field order, and the
same fix: a **host-side layout assertion in `homescope-provision`** that pins
the 36 bytes it emits against a literal. Round-tripping the tool against
itself proves nothing. ✅ *Built* — as known-answer `[u32; 9]` literals in
`common/src/uicr_record.rs`'s tests, beside the one encoder both sides use.

**The padding byte is alignment, not a growth slot.** UICR is
write-once-per-bit with no erase short of `NVMC.ERASEUICR`, so a field cannot
be added to a record that has already been written. Adding one is a version
bump and a re-provision.

**Versioning is deliberately single-valued.** UICR survives an ordinary
reflash — the whole point — so firmware advances while the record stays
whatever the tool of the day wrote: new parser, old data. But unlike the air
packet there is never a second parser here. A node in the field must stay
visible because you cannot reach it; a node being provisioned is under a probe
by definition. Version mismatch ⇒ `UnsupportedVersion` ⇒ *re-provision this
board*.

### Write order: key words first, header word last

⚠️ **The provisioning tool must write `CUSTOMER[1..9]` (the key) before
`CUSTOMER[0]` (the header).**

The reader detects a blank record by testing word 0 against `0xFFFFFFFF`. That
catches an untouched chip, but not a *partial* one: if provisioning dies
between the header write and the key writes — power loss, probe knocked loose,
tool panic — a device is left with a valid header in front of 32 bytes of
`0xFF`, and `chip::device_key()` returns that as a key. UICR cannot be
rewritten without `ERASEUICR`, so the board is stuck in that state, and the
only symptom is (again) an AEAD tag failure.

Writing the header last makes the magic a **commit marker**: word 0 is only
valid if everything it describes already landed. The bad state becomes
unreachable rather than merely detectable. It costs nothing but ordering, and
it is the reason the reader does *not* also defensively reject an all-`0xFF`
key — that would paper over a writer bug that is free to prevent.

Read-back verification (§1 step 4) is still required and is a separate
guarantee: ordering protects against an *interrupted* write, read-back against
a *wrong* one — bits that didn't take, a flash-loader that silently skipped
the UICR region, the endianness mistake above. UICR reads back over SWD like
any flash, so the check is just a memory read of the 36 bytes and a compare
against what was sent. Verify before flashing the application, not after:
a botched UICR write is unrecoverable in place, and there is no point putting
firmware on a board that will need `ERASEUICR` anyway.

### Why UICR

1. **One firmware binary for the whole fleet** — no per-device ELF, shared CI
   artifact stays shared, future OTA stays fleet-wide. The decisive argument.
2. **Survives reflash.** `probe-rs run` / `cargo flash` erase only the pages
   they write; UICR is untouched. Reflash fifty times during development and
   the device keeps its identity. (The `link_section`-in-app-flash approach
   re-injects on every reflash and needs the key present on the build machine
   every time.)
3. **The key never enters build artifacts** — nothing in `target/`, nothing in
   shell history via `DEVICE_KEY=… cargo build`, no path to CI.
4. **UICR is on the path anyway** — the custom PCB needs
   `UICR.REGOUT0 = 3.0 V` for the VDDH topology. Same provisioning step, same
   tool.

### Costs, honestly

- UICR is flash: write-once per bit; the only erase is `NVMC.ERASEUICR` (or
  full `ERASEALL`). **Key rotation = erase UICR + rewrite.**
  ⚠️ **Erasing UICR resets `REGOUT0` to 1.8 V**, which will brown-out a VDDH
  board — the rotation sequence must rewrite REGOUT0 in the same operation.
- **`ERASEALL` / `probe-rs erase --chip-erase` destroys the key.** Recovery is
  re-provisioning from the DB (§1), not regeneration.
- UICR is readable over SWD like any flash. Confidentiality rests entirely on
  **APPROTECT**.
  ⚠️ **Do not enable APPROTECT during development**: on nRF52840 the only way
  back to a debug session is `ERASEALL`, which wipes both the key and REGOUT0.
  Treat APPROTECT as a later deployment-hardening decision — and know that
  nRF52 APPROTECT has published glitch bypasses; it raises attacker cost, it
  is not a vault.

### Alternative considered: dedicated app-flash page

Same trick as the seq checkpoint pages ([packet-tv-aead.md](packet-tv-aead.md) §3), written
by a provisioning routine in the firmware.

- **Pro**: as much space as wanted (key + site + room + calibration constants,
  a versioned struct); and **the firmware can write it itself**, so a sealed
  node with no SWD access can be provisioned over USB-CDC.
- **Con**: must be carved out in `memory.x` and kept honest against the
  linker; a careless `probe-rs download` can clobber it.

**Use it for any node intended to be permanently sealed.** UICR otherwise.
The firmware-side accessor is one function either way, so migrating later is
cheap.

## 6. APPROTECT — locking the chip (settled 2026-08-07)

### What it actually does

`UICR.APPROTECT` disables the **whole debug access port**. Nothing is readable
over SWD — not flash, not RAM, not UICR, and **not FICR**. FICR isn't specially
protected, it's collateral. RTT goes with it, since RTT is memory reads through
that same AP.

What remains is the **CTRL-AP**, which exposes exactly one useful operation:
`ERASEALL`. It wipes flash, UICR and RAM and restores debug access. It is the
only way back in, and it destroys the key by definition.

### Why this doesn't conflict with the read-back verification

Because **locking is the last step**. Every read the tool needs happens on an
unlocked chip; the lock is written after the app is flashed (§1 step 7). There
is no point in the flow where the tool must read a locked device.

Follow the lock with a **positive confirmation** — reset, then attempt a read
that should now fail. A lock that silently didn't take is worse than no lock,
because it produces false confidence.

### Rejected: a small "reporter" firmware that dumps FICR/UICR

The idea is to work around not being able to read a locked chip. It can't:
flashing the reporter goes through the **same debug AP** that reading does, so
if the chip is locked it can't be flashed either — and if you `ERASEALL` first
to unlock it, UICR is gone and there is nothing left to report. It solves
nothing that step ordering doesn't solve for free, and it adds a second
firmware image and a second wire protocol to keep in step with §5's record
layout.

It does gesture at one real gap: **a locked device has no diagnostic channel.**
No RTT means a boot failure (`DeviceKeyError::Blank`, a bad record, a panic) is
completely invisible. The answer is an LED blink code or accepting that "are
packets arriving" is the only signal — not a reporter firmware.

### Re-provisioning a locked device

**`ERASEALL` does not touch FICR.** It is factory-programmed, read-only, and
not erasable by any user operation — no NVMC command reaches it. Flash, UICR
and RAM go; `DEVICEADDR` stays. So identity survives the wipe and the flow is
the normal one with a step in front:

```
ERASEALL (CTRL-AP)  → unlocks, wipes flash + UICR + RAM
read FICR           → same device_addr as before
… identical from here
```

Concretely: the DB row still matches, so it is a key rotation on the *same*
row and the readings history is preserved. You never lose track of which
sensor you are holding.

Two consequences to build around:

**The safety check degrades gracefully.** A locked chip can't have its UICR
read to answer "is this already provisioned?" — but it doesn't need to, because
*locked is itself the signal*: you only lock after provisioning. So the
confirmation prompt fires on "locked **or** UICR holds a valid record". Same
prompt, two paths to it.

⚠️ **Locking removes the safe-abort property.** On an unlocked chip the order
is read FICR → API call → erase, so a network or auth failure leaves a working
device untouched. On a locked chip you must **destroy before you can
identify** — the erase is what makes FICR readable. If the API call then fails,
you are holding a blank board and the only way out is forward.

The inversion can't be removed, but most of it drains away with a **pre-flight
call** (§1 step 0): check API reachability and token acceptance before any
destructive step, even though the device can't be named yet. That catches the
failures that actually happen — API down, wrong token, wrong URL, expired cert
— while the device is still intact. What's left is a failure specific to one
`device_addr`, which is rare and happens at the bench with the probe attached.

### ⚠️ The hardening gotcha — CONFIRMED 2026-08-26

On newer nRF52840 build codes APPROTECT is *hardened*: protection is active at
reset by default, and firmware must write the APPROTECT peripheral's `DISABLE`
register at startup for debug to work at all. Probe sessions work today, so
something in the embassy-nrf startup path is already doing this.

Consequence: on those parts **"locked" is a two-part condition** — UICR says
protect, *and* the firmware must not un-protect. If `embassy_nrf::init`
unconditionally writes the disable, the UICR lock is defeated by our own boot
code and the chip is wide open while looking locked.

**That is exactly what happens.** `embassy-nrf` 0.10's config defaults to
`Debug::Allowed` (`src/lib.rs:630`), and that arm, for nRF52 parts at or above
`APPROTECT_MIN_BUILD_CODE`, does both halves (`src/lib.rs:852-863`):

```rust
if build_code >= chip::APPROTECT_MIN_BUILD_CODE {
    // UICR.APPROTECT = HwDisabled
    let res = uicr_write(consts::UICR_APPROTECT, consts::APPROTECT_DISABLED);
    needs_reset |= res == WriteResult::Written;
    // APPROTECT.DISABLE = SwDisabled
    (0x4000_0558 as *mut u32).write_volatile(consts::APPROTECT_DISABLED);
}
```

`firmware/sensor/src/main.rs` calls `embassy_nrf::init` with a
default-derived `Config`, so **every provisioned sensor unlocks itself on
every power-up.** A `lock` subcommand written today would write the UICR word,
the next boot would try to un-write it, and the result is a board that reads
as locked to whoever wrote it and is open to anyone with a probe.

So locking requires a firmware change first: `config.debug =
Debug::Disallowed` (or `NotConfigured`). Ship the tool half alone and the lock
is theatre; ship the firmware half alone and nothing breaks, which makes it
the safe one to land first. The "positive confirmation" step above — reset,
then attempt a read that should now fail — is what would have caught this, and
is non-negotiable whenever this does land.

### Locking is a separate subcommand, not a provisioning step

`UICR.APPROTECT` erases to `0xFF` (disabled), and *enabling* it means clearing
bits — a legal write-once-per-bit write. So a provisioned device can be locked
later with **one word write and a reset**; no erase, no re-provision, no new
key.

That makes `homescope-provision lock` its own subcommand rather than a `--lock`
flag on `provision`. Two gains: the decision moves off the moment you are least
able to make it (mid-bring-up), and locking becomes available for devices that
are already deployed and working.

### Should we lock? Not yet

**What it buys.** Physical possession of a sensor no longer yields its key. But
per-device keys already bound that to forging readings for *that one sensor*,
with no lateral movement. An attacker holding the outdoor node can get the same
effect with a lighter.

**What it costs.** No RTT on deployed hardware, during exactly the phase where
firmware reliability and power draw are still being chased. Any diagnosis
becomes `ERASEALL`, which destroys the evidence being sought — seq counter,
flash state, everything. Plus the loss of safe-abort on re-provision above.
Both costs are diagnostic and recovery affordances, and both matter most right
now.

Also worth knowing: **`device_addr` isn't secret.** It is broadcast in
cleartext in every advertisement, so the address of a locked device can always
be recovered off the air — no probe needed.

So: leave every device unlocked for now, and turn locking on when the fleet is
stable enough that field diagnostics aren't needed — after confirming the
hardening interaction above. Because enabling it needs no erase, deferring
costs nothing but a probe visit, and the `lock` subcommand can be written
whenever. Locking is the right end state; it is the wrong thing to have on
while still learning what breaks.

## Concepts this exercise touches

- Trust-on-first-provision vs. factory-injected keys — physical access at
  provisioning time is the root of trust here
- Why symmetric keys force "generate centrally, distribute once", vs. the
  asymmetric alternative where the device generates and never exposes its
  private half (a real option if telemetry ever moves to signed-not-encrypted)
- Envelope encryption / KEK-DEK hierarchies — the reason cloud KMS exists;
  "protect N secrets" → "protect 1 secret"
- AAD as context binding, and the confused-deputy attack it prevents (a valid
  tag proves *who made it*, never *where it belongs*)
- Stream vs block ciphers (no padding ⇒ ciphertext length = plaintext length)
- Why nonce discipline differs between the same algorithm's two uses here:
  counter-derived on the air packet, random-192-bit at rest
- Secret issuance patterns: show-once responses, no read-back, explicit
  rotation
- nRF52 UICR/FICR, NVMC erase granularity, APPROTECT and its limits
