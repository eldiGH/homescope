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
//!   there once at provisioning time.
//!
//! Both are memory-mapped flash: reads need no clock, no `NVMC` setup and no
//! peripheral initialisation, so these functions are callable before
//! `embassy_nrf::init`.

#[cfg(feature = "device-key")]
use core::ops::Range;
#[cfg(feature = "device-key")]
use embassy_nrf::pac::UICR;

use embassy_nrf::pac::FICR;
use homescope_common::device_addr::DeviceAddr;

#[cfg(feature = "device-key")]
use homescope_common::device_key::DeviceKey;

/// This chip's BLE advertising address, from `FICR.DEVICEADDR`.
///
/// The top two bits of the most significant byte are forced to `1` to mark it
/// a static random address, which is what the address is required to be when
/// it is not a registered public one.
pub fn device_addr() -> DeviceAddr {
    let [b4, b5, _, _] = FICR.deviceaddr(1).read().to_le_bytes();
    let [b0, b1, b2, b3] = FICR.deviceaddr(0).read().to_le_bytes();

    DeviceAddr([b0, b1, b2, b3, b4, b5 | 0xC0])
}

/// This device's AEAD key, from the provisioning record in `UICR.CUSTOMER`.
///
/// # Record layout
///
/// **This is a wire format.** `homescope-provision` writes these exact bytes
/// over SWD and this function is the only reader; the two halves live in
/// different crates and nothing type-checks between them, so the layout is
/// stated here in absolute byte addresses rather than left implicit in the
/// word arithmetic below.
///
/// ```text
/// 0x10001080   b"HK"       2 B   magic
/// 0x10001082   version     1 B   currently 1
/// 0x10001083   0x00        1 B   padding
/// 0x10001084   key        32 B   key byte i lives at 0x10001084 + i
/// ```
///
/// 36 bytes of the 128-byte `CUSTOMER` block. The header comes first for the
/// same reason `ver` precedes `seq` on the air packet: a version field has to
/// be readable without already knowing the layout it describes.
///
/// The byte order is the part that is easy to get wrong and impossible to
/// test from here. Words are read little-endian, so key byte `i` lands at
/// `0x10001084 + i` and a hex dump of the region shows the key in order. A
/// provisioning tool that groups bytes into words the other way produces a
/// device whose packets fail their AEAD tag with no other symptom — the same
/// failure class `packet::cipher`'s known-answer test guards against, and
/// solvable the same way: a host-side layout assertion in the tool.
///
/// The padding byte is alignment, **not** a growth slot. UICR is
/// write-once-per-bit with no erase short of `NVMC.ERASEUICR`, so a field
/// cannot be added to a record that has already been written. Adding one is a
/// version bump and a re-provision, which is exactly what the version byte
/// exists to make legible.
///
/// # Versioning
///
/// UICR survives an ordinary reflash — that is the whole reason the key lives
/// there — so firmware advances while the record stays whatever the tool of
/// the day wrote. New parser, old data. Unlike the air packet, though, this
/// never grows a second parser: the device is in reach of a probe by
/// definition, so a version mismatch means *re-provision this board*, not
/// *support both formats*.
///
/// # Note on zeroization
///
/// The `[u8; 32]` local below is not zeroized, unlike the equivalent handling
/// on the API side. Clearing it would protect nothing: `PacketCipher` holds
/// the key for the entire uptime and UICR holds it until the chip is erased,
/// so the exposure window is unaffected. Anyone who can read this stack frame
/// has SWD, and with SWD they read `0x10001084` directly.
#[cfg(feature = "device-key")]
pub fn device_key() -> Result<DeviceKey, DeviceKeyError> {
    const MAGIC: Range<usize> = 0..2;
    const VER: usize = MAGIC.end;
    const PADDING: usize = VER + 1;
    const MAGIC_BYTES: [u8; 2] = *b"HK";
    const KEY_VERSION: u8 = 1;

    const _: () = assert!(
        1 + DeviceKey::SIZE / 4 <= 32,
        "record must fit UICR.CUSTOMER"
    );

    let header = UICR.customer(0).read();

    if header == u32::MAX {
        return Err(DeviceKeyError::Blank);
    }

    let header_bytes = header.to_le_bytes();

    if header_bytes[MAGIC] != MAGIC_BYTES {
        return Err(DeviceKeyError::BadMagic);
    }

    let ver = header_bytes[VER];
    if ver != KEY_VERSION {
        return Err(DeviceKeyError::UnsupportedVersion(ver));
    }

    if header_bytes[PADDING] != 0 {
        return Err(DeviceKeyError::BadPadding);
    }

    let mut key: [u8; DeviceKey::SIZE] = [0; _];

    for (i, chunk) in key.chunks_exact_mut(4).enumerate() {
        chunk.copy_from_slice(&UICR.customer(i + 1).read().to_le_bytes());
    }

    Ok(DeviceKey::from_bytes(key))
}

/// Why [`device_key`] could not produce a key.
///
/// The variants are split by the action they call for, not by where the parse
/// gave up — a boot failure on a node with no probe attached is only useful if
/// the message says what to do about it.
#[cfg(feature = "device-key")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum DeviceKeyError {
    /// Erased UICR. Normal for a new or chip-erased board — provision it.
    ///
    /// Checked first and separately from [`BadMagic`](Self::BadMagic) because
    /// erased flash reads as all-ones, and "never provisioned" deserves a
    /// different message from "something overwrote your key".
    Blank,
    /// Non-erased, but not our record. Something else wrote `CUSTOMER[0]` —
    /// find out what before erasing, that data may matter.
    BadMagic,
    /// Reserved byte is not zero. Either a tool wrote a field this firmware
    /// does not know about, or the record is damaged. Re-provision.
    BadPadding,
    /// A record this firmware cannot read. Re-provision with a current tool;
    /// there is deliberately no second parser.
    UnsupportedVersion(u8),
}
