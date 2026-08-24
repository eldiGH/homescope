//! Chip-fixed identity, read out of the nRF52840's information registers.
//!
//! Everything here is identical on every nRF52840 regardless of which board
//! the chip is soldered to, which is what separates this module from the
//! [`Board`](crate::Board) struct next door: `Board` holds the resources that
//! *vary* per board, `chip` holds the ones that never do.
//!
//! Two register blocks, easy to confuse:
//!
//! - **FICR** (Factory Information Configuration Registers) — read-only,
//!   programmed by Nordic. `DEVICEADDR` is the advertising address, so device
//!   *identity* needs no provisioning step at all.
//! - **UICR** (User Information Configuration Registers, `0x10001000`) —
//!   writable non-volatile user space. The per-device AEAD key is written
//!   there once at provisioning time, in the record format defined by
//!   [`homescope_common::uicr_record`].
//!
//! Both are memory-mapped flash: reads need no clock, no `NVMC` setup and no
//! peripheral initialisation, so these functions are callable before
//! `embassy_nrf::init`.
//!
//! Neither function here defines a format. `DeviceAddr::from_ficr` and
//! `uicr_record::decode` both live in `homescope-common` because
//! `homescope-provision` calls them too, over SWD, from a different workspace
//! and a different target — a tool that derived the address or unpacked the
//! key even slightly differently would provision a device that works right up
//! until its first packet is decrypted.

#[cfg(feature = "device-key")]
use embassy_nrf::pac::UICR;

use embassy_nrf::pac::FICR;
use homescope_common::device_addr::DeviceAddr;

#[cfg(feature = "device-key")]
use homescope_common::uicr_record::{self, UICR_RECORD_WORDS, UicrRecord};

/// This chip's BLE advertising address, from `FICR.DEVICEADDR`.
///
/// The top two bits of the most significant byte are forced to `1` to mark it
/// a static random address, which is what the address is required to be when
/// it is not a registered public one.
pub fn device_addr() -> DeviceAddr {
    let addr0 = FICR.deviceaddr(0).read();
    let addr1 = FICR.deviceaddr(1).read();

    DeviceAddr::from_ficr(addr0, addr1)
}

/// This device's provisioning record, from `UICR.CUSTOMER`.
///
/// The layout, the byte order and the meaning of each outcome all live in
/// [`homescope_common::uicr_record`], which `homescope-provision` writes
/// through — so the record is described in exactly one place and both halves
/// are covered by the same known-answer tests. All this function does is move
/// nine words off the chip.
///
/// Every variant of [`UicrRecord`] is fatal at boot, but they call for
/// different things: [`Blank`](UicrRecord::Blank) means *provision this board*,
/// [`Malformed`](UicrRecord::Malformed) means *something is wrong with the
/// record that is already there*. Report them separately — on a node with no
/// probe attached, that defmt line is the only diagnosis anyone gets.
///
/// Reads are memory-mapped flash: no clock, no `NVMC` setup, no peripheral
/// init, so this is callable before `embassy_nrf::init`.
#[cfg(feature = "device-key")]
pub fn device_key() -> UicrRecord {
    let mut words = [0u32; UICR_RECORD_WORDS];

    for (i, word) in words.iter_mut().enumerate() {
        *word = UICR.customer(i).read();
    }

    uicr_record::decode(&words)
}
