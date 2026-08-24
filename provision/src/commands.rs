use anyhow::bail;
use homescope_api_types::devices::ProvisionDevicePayload;
use homescope_common::uicr_record::RecordHeader;

use crate::{
    api_client::ApiClient,
    chip::{Chip, Connection},
};

mod messages {
    pub fn device_locked_warning() -> &'static str {
        "The device is locked - please provision or reprovision it with --unlock flag."
    }

    pub fn device_already_unlocked() -> &'static str {
        "The device is already unlocked. Please omit --unlock flag."
    }
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

pub fn provision(api_client: &ApiClient, unlock: bool, name: String) -> anyhow::Result<()> {
    let mut chip = connect(unlock)?;
    let record = chip.read_state()?;

    let send_body = ProvisionDevicePayload {
        name,
        device_addr: record.device_addr,
    };

    #[allow(unused_variables)]
    let response = api_client.provision(&send_body)?;

    // println!(
    //     "Device {} ({}) successfully provisioned",
    //     response.name, response.device_addr
    // );

    Ok(())
}

pub fn rotate_key(api_client: &ApiClient, unlock: bool) -> anyhow::Result<()> {
    let mut chip = connect(unlock)?;
    let record = chip.read_state()?;

    #[allow(unused_variables)]
    let response = api_client.rotate_key(record.device_addr)?;

    // println!(
    //     "Key for device `{}` ({}) successfully rotated",
    //     response.name, response.device_addr
    // );

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
