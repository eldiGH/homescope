use std::{path::Path, time::Duration};

use homescope_common::{
    device_addr::DeviceAddr,
    device_key::DeviceKey,
    uicr_record::{self, RecordHeader},
};
use probe_rs::{
    Core, MemoryInterface, Permissions, Session,
    architecture::arm::ArmError,
    flashing::{DownloadOptions, ElfLoader, ElfOptions, download_file_with_options},
    probe::{DebugProbeInfo, list::Lister},
};
use thiserror::Error;

use crate::{
    chip::memory::{MemoryExt, Mismatch},
    elf::PAGE_SIZE,
};

/// An erased flash word. The reset vector read back as this is what a board
/// with nothing at `0x0` looks like to the CPU.
const ERASED: u32 = u32::MAX;

mod memory;
mod nvmc;

pub const TARGET: &str = "nRF52840_xxAA";

const FICR_DEVICE_ADDR: u64 = 0x1000_00A4;
const UICR_CUSTOMER: u64 = 0x1000_1080;

pub enum Connection {
    Attached(Box<Chip>),
    Locked(Box<LockedChip>),
}

pub struct LockedChip {
    probe: DebugProbeInfo,
}

impl LockedChip {
    /// Names the probe for the identity block. A locked chip has nothing else
    /// to identify it — the address is behind APPROTECT.
    pub fn probe_description(&self) -> String {
        format_probe_info(&self.probe)
    }

    pub fn erase_to_unlock(self) -> Result<Box<Chip>, ConnectError> {
        let session = self
            .probe
            .open()?
            .attach(TARGET, Permissions::new().allow_erase_all())?;

        Ok(Box::new(Chip {
            session,
            probe: self.probe,
        }))
    }
}

pub struct ChipState {
    pub device_addr: DeviceAddr,
    pub record: RecordHeader,
}

pub struct Chip {
    session: Session,
    probe: DebugProbeInfo,
}

impl Chip {
    /// Names the probe for the identity block.
    pub fn probe_description(&self) -> String {
        format_probe_info(&self.probe)
    }

    pub fn connect() -> Result<Connection, ConnectError> {
        let lister = Lister::new();

        let probes = lister.list_all();

        let probe = match probes.len() {
            0 => return Err(ConnectError::NoProbe),
            1 => probes.first().unwrap(),
            _ => return Err(ConnectError::AmbiguousProbe(probes)),
        };

        let connection = match probe.open()?.attach(TARGET, Permissions::new()) {
            Ok(session) => Connection::Attached(Box::new(Self {
                session,
                probe: probe.clone(),
            })),
            Err(probe_rs::Error::Arm(ArmError::MissingPermissions(_))) => {
                Connection::Locked(Box::new(LockedChip {
                    probe: probe.clone(),
                }))
            }
            Err(err) => return Err(ConnectError::Probe(err)),
        };

        Ok(connection)
    }

    fn read_uicr_header(core: &mut Core) -> Result<RecordHeader, probe_rs::Error> {
        let header_word = core.read_word_32(UICR_CUSTOMER)?;

        Ok(uicr_record::decode_header(header_word))
    }

    fn read_device_addr(core: &mut Core) -> Result<DeviceAddr, probe_rs::Error> {
        let buf: [u32; DeviceAddr::WORDS_NEEDED] = core.read_words(FICR_DEVICE_ADDR)?;

        Ok(DeviceAddr::from_ficr(buf[0], buf[1]))
    }

    pub fn read_state(&mut self) -> Result<ChipState, probe_rs::Error> {
        let mut core = self.session.core(0)?;

        Ok(ChipState {
            record: Self::read_uicr_header(&mut core)?,
            device_addr: Self::read_device_addr(&mut core)?,
        })
    }

    pub fn erase_uicr_record(&mut self) -> Result<(), WriteRecordError> {
        let mut core = self.session.core(0)?;

        nvmc::with_erase_enabled(&mut core, |e| e.erase_uicr())?;
        if let Some(mismatch) =
            core.find_mismatch(UICR_CUSTOMER, &[u32::MAX; uicr_record::UICR_RECORD_WORDS])?
        {
            return Err(WriteRecordError::VerificationFailed(mismatch));
        }

        Ok(())
    }

    pub fn write_uicr_record(&mut self, key: DeviceKey) -> Result<(), WriteRecordError> {
        let record = uicr_record::encode(&key);

        let mut core = self.session.core(0)?;

        nvmc::with_write_enabled(&mut core, |w| {
            w.write_words(UICR_CUSTOMER + 4, &record[1..])?;
            w.write_word(UICR_CUSTOMER, record[0])?;

            Ok(())
        })?;

        if let Some(mismatch) = core.find_mismatch(UICR_CUSTOMER, &record[..])? {
            return Err(WriteRecordError::VerificationFailed(mismatch));
        }

        Ok(())
    }

    /// The raw UICR record words, for comparing a region against itself.
    pub fn read_uicr_words(
        &mut self,
    ) -> Result<[u32; uicr_record::UICR_RECORD_WORDS], probe_rs::Error> {
        self.session.core(0)?.read_words(UICR_CUSTOMER)
    }

    /// Writes a firmware image to flash and reads it back.
    ///
    /// ⚠️ `do_chip_erase` stays **false**. It is faster, and it would erase the
    /// UICR key this run just wrote and verified — the one setting here that
    /// turns a successful provision into a dark device.
    ///
    /// ⚠️ The guard is inside rather than beside: an image that starts above
    /// `0x0` expects something beneath it — a bootloader — and flashing it onto
    /// a board where that region is erased leaves the CPU reading `0xFFFFFFFF`
    /// as its initial stack pointer and reset vector. The board looks dead. No
    /// image we build has an offset since the XIAO bootloader was dropped, so
    /// this fires only on a stale or foreign artifact, which is exactly when
    /// nobody is expecting it.
    pub fn flash(&mut self, image: &Path, app_start: u64) -> Result<(), FlashError> {
        if app_start > 0 {
            let beneath = self.session.core(0)?.read_word_32(0)?;

            if beneath == ERASED {
                return Err(FlashError::NothingBeneath { app_start });
            }
        }

        // `#[non_exhaustive]`, so it is built by mutation rather than a literal.
        let mut options = DownloadOptions::default();
        options.verify = true;
        options.do_chip_erase = false;

        download_file_with_options(
            &mut self.session,
            image,
            ElfLoader(ElfOptions::default()),
            options,
        )?;

        Ok(())
    }

    /// Erases the seq checkpoint pages named by the image being flashed.
    ///
    /// ⚠️ Only ever called in the same operation that installs a **new** key.
    /// Clearing the counter under a live key re-emits nonces the device has
    /// already used, which for ChaCha20-Poly1305 leaks the Poly1305 key and
    /// lets an attacker forge packets for that device — a break, not a
    /// degradation. Inside `provision`/`rotate` it is safe because UICR is
    /// erased before the new key lands, so no window exists where an old key
    /// and a fresh counter coexist. There is deliberately no standalone
    /// `reset-seq` command, because that window is all it would be.
    pub fn erase_storage(&mut self, start: u64, end: u64) -> Result<(), EraseStorageError> {
        if start >= end || !start.is_multiple_of(PAGE_SIZE) || !end.is_multiple_of(PAGE_SIZE) {
            return Err(EraseStorageError::NotPageAligned { start, end });
        }

        let mut core = self.session.core(0)?;

        nvmc::with_erase_enabled(&mut core, |eraser| {
            for page in (start..end).step_by(PAGE_SIZE as usize) {
                eraser.erase_page(page)?;
            }

            Ok(())
        })?;

        let words = ((end - start) / 4) as usize;
        if let Some(mismatch) = core.find_mismatch(start, &vec![ERASED; words])? {
            return Err(EraseStorageError::StillSet(mismatch));
        }

        Ok(())
    }

    pub fn halt(&mut self) -> Result<(), probe_rs::Error> {
        self.session.core(0)?.halt(Duration::from_secs(1))?;
        Ok(())
    }

    pub fn reset(&mut self) -> Result<(), probe_rs::Error> {
        self.session.core(0)?.reset()?;
        Ok(())
    }
}

fn format_probe_info(probe: &DebugProbeInfo) -> String {
    match &probe.serial_number {
        Some(serial_number) => format!("{} ({})", probe.identifier, serial_number),
        None => format!("{} (no serial)", probe.identifier),
    }
}

fn format_probe_infos(probes: &[DebugProbeInfo]) -> Vec<String> {
    probes.iter().map(format_probe_info).collect()
}

#[derive(Debug, Error)]
pub enum ConnectError {
    #[error("no probe found")]
    NoProbe,

    // TODO: `--probe <serial>` is named below but not implemented yet. See
    // docs/design/provisioning.md § Postponed.
    #[error("{} probes connected:\n  {}\n\npass --probe <serial> to select one", .0.len(), format_probe_infos(.0).join("\n  "))]
    AmbiguousProbe(Vec<DebugProbeInfo>),

    #[error(transparent)]
    Probe(#[from] probe_rs::Error),
}

impl From<probe_rs::probe::DebugProbeError> for ConnectError {
    fn from(value: probe_rs::probe::DebugProbeError) -> Self {
        Self::Probe(probe_rs::Error::Probe(value))
    }
}

#[derive(Debug, Error)]
pub enum FlashError {
    #[error(
        "this image loads at {app_start:#X}, so it expects a bootloader beneath it — \
         and flash at 0x0 is erased.\n\n  \
         Flashing it would leave the CPU reading 0xFFFFFFFF as its stack pointer and\n  \
         reset vector, and the board would look dead. Every current image links at 0x0;\n  \
         this artifact predates the bootloader being dropped, or is not ours."
    )]
    NothingBeneath { app_start: u64 },

    #[error(transparent)]
    Probe(#[from] probe_rs::Error),

    #[error(transparent)]
    Download(#[from] probe_rs::flashing::FileDownloadError),
}

#[derive(Debug, Error)]
pub enum EraseStorageError {
    #[error("storage region {start:#X}..{end:#X} is not whole pages of {PAGE_SIZE} bytes")]
    NotPageAligned { start: u64, end: u64 },

    #[error(transparent)]
    Probe(#[from] probe_rs::Error),

    #[error(transparent)]
    Nvmc(#[from] nvmc::Error),

    #[error("the seq counter is still set at 0x{:08X} after erasing: found 0x{:08X}", .0.address, .0.actual)]
    StillSet(Mismatch),
}

#[derive(Debug, Error)]
pub enum WriteRecordError {
    #[error(transparent)]
    Probe(#[from] probe_rs::Error),

    #[error(transparent)]
    Nvmc(#[from] nvmc::Error),

    #[error("verification failed at address 0x{:08X}: expected 0x{:08X}, found 0x{:08X}", .0.address, .0.expected, .0.actual)]
    VerificationFailed(Mismatch),
}
