//! The firmware artifact store: named images the tool can flash.
//!
//! ```text
//! ~/.local/share/homescope/firmware/
//!   manifest.toml
//!   a3f1c2….elf
//! ```
//!
//! The data directory, not the config directory: these are artifacts the tool
//! manages and can regenerate from a build, where credentials cannot be
//! regenerated at all.
//!
//! **Why a store rather than a path.** `--firmware <path>` puts "whatever ELF
//! was in the directory you ran from" into the one command that writes flash.
//! A name you picked once, from a list, is a better input to that decision than
//! a path you retyped — which is also why there is deliberately no CWD default.
//!
//! ⚠️ **The tool never builds.** `add` consumes an artifact some other producer
//! made — a justfile recipe, CI, a download. A provisioning CLI that shells out
//! to `cargo` acquires a source checkout, a `thumbv7em` toolchain and a rustup
//! target as hard runtime requirements, welds the fleet tool's release cycle to
//! the firmware's, and makes "which commit did this node get" mean "whatever was
//! uncommitted at 11pm".

use std::{
    fs,
    io::{self, BufRead as _, IsTerminal as _, Write as _, stderr, stdin},
    path::{Path, PathBuf},
    process::Command,
};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use thiserror::Error;

use crate::{elf, store};

const STORE_SUBDIR: &str = "firmware";
const MANIFEST_FILE: &str = "manifest.toml";

/// How much of the ELF's SHA-256 names it. Twelve hex characters is 48 bits —
/// ample against accidental collision among a handful of local builds, and
/// short enough to read out of a table.
const ID_LEN: usize = 12;

/// One stored image.
///
/// ⚠️ `app_start` and `storage` are read **out of the ELF**, never copied from a
/// build config or a flag. That is what makes them trustworthy: the manifest can
/// be wrong about `name` — a human typed it — but it cannot be wrong about where
/// the image loads or where its persistent storage lives, which are the two
/// facts the flash guard and the seq clearing depend on.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Artifact {
    /// Hash of the ELF, and the artifact's real identity.
    pub id: String,

    /// What you type. The one hand-entered field, and the only signal for
    /// *which board* an image is for — see [`Store::add`].
    pub name: String,

    pub app_start: u64,

    /// `[start, end)` of the seq checkpoint region, when the image declares it.
    /// Absent for an image with no `__storage_start`/`__storage_end` — a
    /// receiver build, say, which keeps no counter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage: Option<[u64; 2]>,

    pub built_at: DateTime<Utc>,

    /// The commit the ELF was built from, when it came out of a checkout.
    ///
    /// The provenance that matters when a sealed node misbehaves eleven months
    /// later. ⚠️ Recorded, never enforced: every build is dirty right now, and a
    /// tool that refuses the normal case gets bypassed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git: Option<String>,

    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub dirty: bool,
}

impl Artifact {
    /// The storage range as a half-open pair, if the image declares one.
    pub fn storage_range(&self) -> Option<(u64, u64)> {
        self.storage.map(|[start, end]| (start, end))
    }
}

#[derive(Default, Serialize, Deserialize)]
struct Manifest {
    #[serde(default, rename = "firmware")]
    artifacts: Vec<Artifact>,
}

pub struct Store {
    dir: PathBuf,
    manifest: Manifest,
}

impl Store {
    pub fn load() -> Result<Self, FirmwareError> {
        Self::at(store::data_dir()?.join(STORE_SUBDIR))
    }

    /// Loads from an explicit directory, so tests need no environment.
    fn at(dir: PathBuf) -> Result<Self, FirmwareError> {
        let path = dir.join(MANIFEST_FILE);

        let manifest = match fs::read_to_string(&path) {
            // An absent store is an empty store, not an error: `firmware list`
            // on a fresh machine should say "nothing stored", not fail.
            Err(err) if err.kind() == io::ErrorKind::NotFound => Manifest::default(),
            Err(source) => return Err(FirmwareError::Read { path, source }),
            Ok(contents) => {
                toml::from_str(&contents).map_err(|source| FirmwareError::Parse { path, source })?
            }
        };

        Ok(Self { dir, manifest })
    }

    pub fn artifacts(&self) -> &[Artifact] {
        &self.manifest.artifacts
    }

    pub fn get(&self, name: &str) -> Option<&Artifact> {
        self.manifest.artifacts.iter().find(|a| a.name == name)
    }

    /// Where the stored ELF for an artifact lives.
    pub fn image_path(&self, artifact: &Artifact) -> PathBuf {
        self.dir.join(format!("{}.elf", artifact.id))
    }

    /// Reads an ELF, copies it in, and records what it says about itself.
    ///
    /// ⚠️ **Names are unique and re-adding replaces.** Rebuilding and re-adding
    /// under the same name is the normal case — `just firmware-build xiao` twice
    /// — so a name points at one current image rather than accumulating
    /// versions. Git holds the history; the store answers "what can I flash
    /// now".
    ///
    /// ⚠️ Nothing here can tell which *board* an image is for. Since the
    /// bootloader was dropped every board shares one layout, so `app_start` no
    /// longer distinguishes them and the name is the only signal. Flashing a
    /// DB-40 image to a XIAO is no longer a brick, but it drives the wrong pins.
    pub fn add(&mut self, path: &Path, name: &str) -> Result<Artifact, FirmwareError> {
        if name.is_empty() || name.chars().any(|c| c.is_whitespace() || c.is_control()) {
            return Err(FirmwareError::InvalidName(name.to_owned()));
        }

        let bytes = fs::read(path).map_err(|source| FirmwareError::Read {
            path: path.to_owned(),
            source,
        })?;

        let image = elf::read(&bytes).map_err(|source| FirmwareError::Elf {
            path: path.to_owned(),
            source,
        })?;

        let id = hex(&Sha256::digest(&bytes)[..ID_LEN / 2]);
        let (git, dirty) = provenance(path);

        let artifact = Artifact {
            id,
            name: name.to_owned(),
            app_start: image.app_start,
            storage: image.storage.map(|(start, end)| [start, end]),
            built_at: Utc::now(),
            git,
            dirty,
        };

        fs::create_dir_all(&self.dir).map_err(|source| FirmwareError::Write {
            path: self.dir.clone(),
            source,
        })?;

        let image_path = self.image_path(&artifact);
        fs::write(&image_path, &bytes).map_err(|source| FirmwareError::Write {
            path: image_path,
            source,
        })?;

        let replaced = self.replace(artifact.clone());
        self.save()?;

        // After the manifest is safely written, not before: an orphaned file is
        // waste, a missing one is a broken row.
        if let Some(old) = replaced
            && old.id != artifact.id
            && !self.manifest.artifacts.iter().any(|a| a.id == old.id)
        {
            let _ = fs::remove_file(self.image_path(&old));
        }

        Ok(artifact)
    }

    /// Forgets a name, deleting its image when nothing else points at it.
    ///
    /// Names go stale — a board feature gets renamed, a variant is retired — and
    /// a stale row is worse than clutter: the picker offers it, and picking the
    /// wrong image now means wrong pins rather than a refusal, since every image
    /// links at the same address.
    pub fn remove(&mut self, name: &str) -> Result<Option<Artifact>, FirmwareError> {
        let Some(at) = self.manifest.artifacts.iter().position(|a| a.name == name) else {
            return Ok(None);
        };

        let removed = self.manifest.artifacts.remove(at);
        self.save()?;

        if !self.manifest.artifacts.iter().any(|a| a.id == removed.id) {
            let _ = fs::remove_file(self.image_path(&removed));
        }

        Ok(Some(removed))
    }

    fn replace(&mut self, artifact: Artifact) -> Option<Artifact> {
        let replaced = match self
            .manifest
            .artifacts
            .iter()
            .position(|a| a.name == artifact.name)
        {
            Some(at) => Some(std::mem::replace(
                &mut self.manifest.artifacts[at],
                artifact,
            )),
            None => {
                self.manifest.artifacts.push(artifact);
                None
            }
        };

        self.manifest.artifacts.sort_by(|a, b| a.name.cmp(&b.name));

        replaced
    }

    fn save(&self) -> Result<(), FirmwareError> {
        let contents = toml::to_string_pretty(&self.manifest)?;
        let path = self.dir.join(MANIFEST_FILE);

        fs::write(&path, contents).map_err(|source| FirmwareError::Write { path, source })
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Best-effort git provenance for an ELF sitting inside a checkout.
///
/// ⚠️ Shelling out to `git` is not the `cargo` prohibition in this module's
/// docs: this reads metadata and tolerates every failure, where building would
/// make a toolchain a runtime requirement. An ELF from CI or a download simply
/// records no commit.
fn provenance(path: &Path) -> (Option<String>, bool) {
    let Some(dir) = path.parent() else {
        return (None, false);
    };

    let git = |args: &[&str]| {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .ok()?;

        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
    };

    let Some(commit) = git(&["rev-parse", "--short", "HEAD"]).filter(|c| !c.is_empty()) else {
        return (None, false);
    };

    // Untracked files are not part of what was built; modified tracked ones are.
    let dirty = git(&["status", "--porcelain", "--untracked-files=no"])
        .is_some_and(|status| !status.is_empty());

    (Some(commit), dirty)
}

/// Chooses an artifact when `--firmware` was not given.
///
/// A numbered list rather than a fuzzy select: the interactive value here is
/// entirely "choose from a list you cannot remember", and a numbered prompt
/// buys that with no dependency and no terminal handling to get wrong. The
/// picker is the one interactive point — there is no interactive *mode*,
/// because a menu wrapping `provision`/`flash` would be a second surface to
/// keep in step with the first, and subcommands are scriptable, completable and
/// greppable in shell history where a menu is none of those.
///
/// ⚠️ Fails closed with no terminal, like every other prompt: a script that
/// omits `--firmware` gets an error naming the flag, never an arbitrary pick.
pub fn pick(store: &Store, now: DateTime<Utc>) -> Result<&Artifact, PickError> {
    let artifacts = store.artifacts();

    if artifacts.is_empty() {
        return Err(PickError::StoreEmpty);
    }

    if !stdin().is_terminal() {
        return Err(PickError::NoTty);
    }

    let width = name_width(artifacts);
    let mut err = stderr();
    let _ = writeln!(err, "Stored firmware:\n");
    for (n, artifact) in artifacts.iter().enumerate() {
        let _ = writeln!(err, "  {:>2}  {}", n + 1, describe(artifact, now, width));
    }
    let _ = write!(err, "\nWhich firmware? [1-{}] ", artifacts.len());
    let _ = err.flush();

    let mut answer = String::new();
    stdin().lock().read_line(&mut answer)?;

    answer
        .trim()
        .parse::<usize>()
        .ok()
        .filter(|choice| (1..=artifacts.len()).contains(choice))
        .map(|choice| &artifacts[choice - 1])
        .ok_or(PickError::NotAChoice)
}

/// The name column, sized to the longest name so both listings line up.
pub fn name_width(artifacts: &[Artifact]) -> usize {
    artifacts
        .iter()
        .map(|artifact| artifact.name.chars().count())
        .max()
        .unwrap_or(0)
}

/// One artifact on one line, for the picker and for `firmware list`.
pub fn describe(artifact: &Artifact, now: DateTime<Utc>, name_width: usize) -> String {
    let provenance = match (&artifact.git, artifact.dirty) {
        (Some(commit), true) => format!("{commit}+dirty"),
        (Some(commit), false) => commit.clone(),
        (None, _) => "no commit".to_owned(),
    };

    format!(
        "{:<name_width$} {:<14} {:<12} built {}",
        artifact.name,
        provenance,
        artifact.id,
        crate::output::age(artifact.built_at, now),
    )
}

#[derive(Debug, Error)]
pub enum PickError {
    #[error(
        "no firmware stored\n\nadd one first: homescope-provision firmware add <PATH> --name <NAME>"
    )]
    StoreEmpty,

    #[error("cannot ask which firmware: stdin is not a terminal\n\npass --firmware <NAME>")]
    NoTty,

    #[error("not one of the offered choices")]
    NotAChoice,

    #[error(transparent)]
    Io(#[from] io::Error),
}

#[derive(Debug, Error)]
pub enum FirmwareError {
    #[error(transparent)]
    Store(#[from] store::StoreError),

    #[error("could not read {}: {source}", .path.display())]
    Read { path: PathBuf, source: io::Error },

    #[error("could not write {}: {source}", .path.display())]
    Write { path: PathBuf, source: io::Error },

    #[error("{} is malformed: {source}", .path.display())]
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },

    #[error(transparent)]
    Serialize(#[from] toml::ser::Error),

    #[error("{} is not usable firmware: {source}", .path.display())]
    Elf { path: PathBuf, source: elf::Error },

    #[error(
        "`{0}` is not a usable name: names are typed at a prompt and printed in a table, so they may not be empty or contain whitespace"
    )]
    InvalidName(String),
}

#[cfg(test)]
mod test {
    use tempfile::TempDir;

    use super::*;

    /// A minimal but real ELF: one PT_LOAD at `base` plus the two storage
    /// symbols, built by `elf::test`'s fixture so both modules agree on shape.
    fn elf_bytes(base: u32, filler: u8) -> Vec<u8> {
        crate::elf::test::fixture(base, Some((0x000F_E000, 0x0010_0000)), filler)
    }

    fn store_at(tmp: &TempDir) -> Store {
        Store::at(tmp.path().join("firmware")).expect("empty store")
    }

    fn write_elf(tmp: &TempDir, name: &str, base: u32, filler: u8) -> PathBuf {
        let path = tmp.path().join(name);
        fs::write(&path, elf_bytes(base, filler)).unwrap();
        path
    }

    /// An absent store is an empty store — `firmware list` on a fresh machine
    /// reports nothing rather than failing.
    #[test]
    fn a_missing_store_loads_empty() {
        let tmp = TempDir::new().unwrap();

        assert!(store_at(&tmp).artifacts().is_empty());
    }

    #[test]
    fn adding_records_what_the_elf_says() {
        let tmp = TempDir::new().unwrap();
        let mut store = store_at(&tmp);

        let artifact = store
            .add(&write_elf(&tmp, "sensor.elf", 0, 0xAA), "sensor-db40")
            .expect("adds");

        assert_eq!(artifact.name, "sensor-db40");
        assert_eq!(artifact.app_start, 0);
        assert_eq!(artifact.storage, Some([0x000F_E000, 0x0010_0000]));
        assert_eq!(artifact.id.len(), ID_LEN);
        assert!(store.image_path(&artifact).exists());
    }

    /// The store survives the process: a second `load` sees the same rows.
    #[test]
    fn the_manifest_round_trips_through_disk() {
        let tmp = TempDir::new().unwrap();
        let mut store = store_at(&tmp);
        store
            .add(&write_elf(&tmp, "a.elf", 0, 0xAA), "sensor-db40")
            .unwrap();

        let reopened = store_at(&tmp);
        let artifact = reopened.get("sensor-db40").expect("found by name");

        assert_eq!(artifact.app_start, 0);
        assert_eq!(artifact.storage, Some([0x000F_E000, 0x0010_0000]));
    }

    /// ⚠️ Rebuilding and re-adding under one name is the normal case, so a name
    /// points at the current image rather than accumulating versions — and the
    /// superseded ELF does not linger on disk.
    #[test]
    fn re_adding_a_name_replaces_it_and_cleans_up() {
        let tmp = TempDir::new().unwrap();
        let mut store = store_at(&tmp);

        let first = store
            .add(&write_elf(&tmp, "a.elf", 0, 0xAA), "sensor-db40")
            .unwrap();
        let second = store
            .add(&write_elf(&tmp, "b.elf", 0, 0xBB), "sensor-db40")
            .unwrap();

        assert_ne!(first.id, second.id, "different bytes, different id");
        assert_eq!(store.artifacts().len(), 1);
        assert_eq!(store.get("sensor-db40").unwrap().id, second.id);
        assert!(!store.image_path(&first).exists(), "old image left behind");
        assert!(store.image_path(&second).exists());
    }

    /// Identical bytes hash the same, so re-adding the very same build under the
    /// same name must not delete the image it just wrote.
    #[test]
    fn re_adding_identical_bytes_keeps_the_image() {
        let tmp = TempDir::new().unwrap();
        let mut store = store_at(&tmp);
        let path = write_elf(&tmp, "a.elf", 0, 0xAA);

        store.add(&path, "sensor-db40").unwrap();
        let again = store.add(&path, "sensor-db40").unwrap();

        assert_eq!(store.artifacts().len(), 1);
        assert!(store.image_path(&again).exists());
    }

    #[test]
    fn removing_a_name_drops_its_image() {
        let tmp = TempDir::new().unwrap();
        let mut store = store_at(&tmp);
        let artifact = store
            .add(&write_elf(&tmp, "a.elf", 0, 0xAA), "sensor-xiao")
            .unwrap();

        let removed = store.remove("sensor-xiao").unwrap().expect("was stored");

        assert_eq!(removed.id, artifact.id);
        assert!(store.artifacts().is_empty());
        assert!(!store.image_path(&artifact).exists());
        assert!(store_at(&tmp).get("sensor-xiao").is_none(), "still on disk");
    }

    /// Two names can share one image — identical bytes hash identically — so
    /// removing one must not delete the bytes the other still needs.
    #[test]
    fn removing_one_of_two_names_sharing_an_image_keeps_it() {
        let tmp = TempDir::new().unwrap();
        let mut store = store_at(&tmp);
        let path = write_elf(&tmp, "a.elf", 0, 0xAA);
        store.add(&path, "sensor-xiao").unwrap();
        let kept = store.add(&path, "sensor-xiao-expansion").unwrap();

        store.remove("sensor-xiao").unwrap();

        assert!(store.image_path(&kept).exists(), "shared image deleted");
    }

    #[test]
    fn removing_an_unknown_name_is_not_an_error() {
        let tmp = TempDir::new().unwrap();

        assert!(store_at(&tmp).remove("never-stored").unwrap().is_none());
    }

    #[test]
    fn two_names_can_hold_different_images() {
        let tmp = TempDir::new().unwrap();
        let mut store = store_at(&tmp);

        store
            .add(&write_elf(&tmp, "a.elf", 0, 0xAA), "sensor-xiao")
            .unwrap();
        store
            .add(&write_elf(&tmp, "b.elf", 0, 0xBB), "sensor-db40")
            .unwrap();

        // Sorted, because the picker and `firmware list` both show them in order.
        let names: Vec<&str> = store.artifacts().iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["sensor-db40", "sensor-xiao"]);
    }

    /// A name is typed at a prompt and printed in a table; whitespace in one
    /// would split a row the way a device name would.
    #[test]
    fn names_with_whitespace_or_empty_are_refused() {
        let tmp = TempDir::new().unwrap();
        let mut store = store_at(&tmp);
        let path = write_elf(&tmp, "a.elf", 0, 0xAA);

        for name in ["", "two words", "tab\there", "line\nbreak"] {
            assert!(
                matches!(store.add(&path, name), Err(FirmwareError::InvalidName(_))),
                "{name:?} should be refused"
            );
        }
    }

    #[test]
    fn a_file_that_is_not_an_elf_is_refused_by_name() {
        let tmp = TempDir::new().unwrap();
        let mut store = store_at(&tmp);
        let path = tmp.path().join("notes.txt");
        fs::write(&path, b"this is not an ELF").unwrap();

        let err = store.add(&path, "sensor-db40").unwrap_err();

        assert!(matches!(err, FirmwareError::Elf { .. }));
        assert!(err.to_string().contains("notes.txt"));
    }
}
