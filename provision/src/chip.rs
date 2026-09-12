use std::time::Duration;

use homescope_common::{
    device_addr::DeviceAddr,
    device_key::DeviceKey,
    uicr_record::{self, RecordHeader},
};
use probe_rs::{
    Core, MemoryInterface, Permissions, Session,
    architecture::arm::ArmError,
    probe::{DebugProbeInfo, list::Lister},
};
use thiserror::Error;

use crate::chip::memory::{MemoryExt, Mismatch};

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
pub enum WriteRecordError {
    #[error(transparent)]
    Probe(#[from] probe_rs::Error),

    #[error(transparent)]
    Nvmc(#[from] nvmc::Error),

    #[error("verification failed at address 0x{:08X}: expected 0x{:08X}, found 0x{:08X}", .0.address, .0.expected, .0.actual)]
    VerificationFailed(Mismatch),
}
