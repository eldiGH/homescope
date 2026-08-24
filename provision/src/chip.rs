use homescope_common::{
    device_addr::DeviceAddr,
    uicr_record::{self, RecordHeader},
};
use probe_rs::{
    Core, MemoryInterface as _, Permissions, Session,
    architecture::arm::ArmError,
    probe::{DebugProbeInfo, list::Lister},
};
use thiserror::Error;

const TARGET: &str = "nRF52840_xxAA";

const FICR_DEVICE_ADDR: u64 = 0x1000_00A4;
const UICR_CUSTOMER_DATA: u64 = 0x1000_1080;

pub enum Connection {
    Attached(Box<Chip>),
    Locked(Box<LockedChip>),
}

pub struct LockedChip {
    probe: DebugProbeInfo,
}

impl LockedChip {
    pub fn erase_to_unlock(self) -> Result<Box<Chip>, ConnectError> {
        Ok(Box::new(Chip {
            session: self
                .probe
                .open()?
                .attach(TARGET, Permissions::new().allow_erase_all())?,
        }))
    }
}

pub struct ChipState {
    pub device_addr: DeviceAddr,
    pub record: RecordHeader,
}

pub struct Chip {
    session: Session,
}

impl Chip {
    pub fn connect() -> Result<Connection, ConnectError> {
        let lister = Lister::new();

        let probes = lister.list_all();

        let probe = match probes.len() {
            0 => return Err(ConnectError::NoProbe),
            1 => probes.first().unwrap(),
            _ => return Err(ConnectError::AmbiguousProbe(probes)),
        };

        let connection = match probe.open()?.attach(TARGET, Permissions::new()) {
            Ok(session) => Connection::Attached(Box::new(Self { session })),
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
        let header_word = core.read_word_32(UICR_CUSTOMER_DATA)?;

        Ok(uicr_record::decode_header(header_word))
    }

    fn read_device_addr(core: &mut Core) -> Result<DeviceAddr, probe_rs::Error> {
        let mut buf = [0u32; 2];
        core.read_32(FICR_DEVICE_ADDR, &mut buf)?;

        Ok(DeviceAddr::from_ficr(buf[0], buf[1]))
    }

    pub fn read_state(&mut self) -> Result<ChipState, probe_rs::Error> {
        let mut core = self.session.core(0).unwrap();

        Ok(ChipState {
            record: Self::read_uicr_header(&mut core)?,
            device_addr: Self::read_device_addr(&mut core)?,
        })
    }
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

fn format_probe_info(probe: &DebugProbeInfo) -> String {
    match &probe.serial_number {
        Some(serial_number) => format!("{} ({})", probe.identifier, serial_number),
        None => format!("{} (no serial)", probe.identifier),
    }
}

fn format_probe_infos(probes: &[DebugProbeInfo]) -> Vec<String> {
    probes.iter().map(format_probe_info).collect()
}
