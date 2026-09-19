use std::time::{Duration, Instant};

use probe_rs::{Core, MemoryInterface as _};
use thiserror::Error;

const NVMC_CONFIG: u64 = 0x4001_E504;
const NVMC_READY: u64 = 0x4001_E400;

const NVMC_ERASEUICR: u64 = 0x4001_E514;
const ERASEUICR: u32 = 1;

const NVMC_ERASEPAGE: u64 = 0x4001_E508;

const NVMC_READY_TIMEOUT: Duration = Duration::from_secs(15);

#[repr(u32)]
enum NvmcMode {
    ReadOnly = 0,
    WriteEnabled = 1,
    EraseEnabled = 2,
}

fn set_mode(core: &mut Core, mode: NvmcMode) -> Result<(), probe_rs::Error> {
    core.write_word_32(NVMC_CONFIG, mode as u32)?;
    Ok(())
}

fn finish<T>(core: &mut Core, closure_result: Result<T, Error>) -> Result<T, Error> {
    let restored = set_mode(core, NvmcMode::ReadOnly);
    let flushed = core.flush();
    let value = closure_result?;
    restored?;
    flushed?;

    Ok(value)
}

pub fn with_write_enabled<'probe, T>(
    core: &mut Core<'probe>,
    f: impl FnOnce(&mut Writer<'_, 'probe>) -> Result<T, Error>,
) -> Result<T, Error> {
    set_mode(core, NvmcMode::WriteEnabled)?;
    let closure_result = f(&mut Writer { core: &mut *core });
    finish(core, closure_result)
}

pub fn with_erase_enabled<'probe, T>(
    core: &mut Core<'probe>,
    f: impl FnOnce(&mut Eraser<'_, 'probe>) -> Result<T, Error>,
) -> Result<T, Error> {
    set_mode(core, NvmcMode::EraseEnabled)?;
    let closure_result = f(&mut Eraser { core: &mut *core });
    finish(core, closure_result)
}

fn write_words(core: &mut Core, address: u64, words: &[u32]) -> Result<(), Error> {
    for (i, &word) in words.iter().enumerate() {
        write_word(core, address + (i as u64 * 4), word)?;
    }

    Ok(())
}

fn write_word(core: &mut Core, address: u64, word: u32) -> Result<(), Error> {
    let now = Instant::now();

    core.write_word_32(address, word)?;
    while core.read_word_32(NVMC_READY)? & 1 == 0 {
        if now.elapsed() > NVMC_READY_TIMEOUT {
            return Err(Error::Timeout);
        }
    }

    Ok(())
}

pub struct Writer<'core, 'probe> {
    core: &'core mut Core<'probe>,
}

impl<'core, 'probe> Writer<'core, 'probe> {
    pub fn write_words(&mut self, address: u64, words: &[u32]) -> Result<(), Error> {
        write_words(self.core, address, words)
    }

    pub fn write_word(&mut self, address: u64, word: u32) -> Result<(), Error> {
        write_word(self.core, address, word)
    }
}

pub struct Eraser<'core, 'probe> {
    core: &'core mut Core<'probe>,
}

impl<'core, 'probe> Eraser<'core, 'probe> {
    pub fn erase_uicr(&mut self) -> Result<(), Error> {
        write_word(self.core, NVMC_ERASEUICR, ERASEUICR)
    }

    /// Erases one flash page — the erase granularity of the nRF52840's NVMC.
    ///
    /// ⚠️ `address` is written to `ERASEPAGE` as-is, and the hardware erases the
    /// page containing it. Callers pass a page start; an address inside a page
    /// would erase that whole page anyway, which is exactly the surprise worth
    /// not having. `elf::read` rejects a storage range that is not page-aligned
    /// before it can reach here.
    pub fn erase_page(&mut self, address: u64) -> Result<(), Error> {
        write_word(self.core, NVMC_ERASEPAGE, address as u32)
    }
}

#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    Probe(#[from] probe_rs::Error),

    #[error("timeout waiting for NVMC operation")]
    Timeout,
}
