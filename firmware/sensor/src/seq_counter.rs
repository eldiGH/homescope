//! Flash-checkpointed monotonic packet counter.
//!
//! Two-page circular append log of "reserved until" ceilings; on boot the
//! counter resumes at the highest ceiling seen rather than at the last value
//! issued, so a reboot always jumps *forward*, never back.
//!
//! # This counter is load-bearing for the AEAD nonce
//!
//! `common`'s `packet::cipher` derives the ChaCha20-Poly1305 nonce directly
//! from `seq`, with no random component. That is only sound because this
//! module guarantees `seq` never repeats and never rewinds for a given device
//! — key material is per-device, so `seq` is the sole source of nonce
//! uniqueness.
//!
//! Nonce reuse under one key is not a degradation, it is a collapse: it costs
//! confidentiality *and* unforgeability, retroactively, for every packet
//! sharing the nonce. A counter that restarted at zero after a battery swap
//! would reuse every nonce it had already issued.
//!
//! So the jump-ahead is a **security** property, not a wear-levelling
//! convenience. Any change that could let `next_seq` go backwards — shrinking
//! `RESERVATION_BLOCK`, resuming from the last issued value, reusing the
//! region — must be checked against `common/src/packet/cipher.rs::nonce`
//! first.

use nrf_sdc::mpsl::{Flash, FlashError};

unsafe extern "C" {
    static __storage_start: u8;
    static __storage_end: u8;
}

const RESERVATION_BLOCK: u32 = 1024;

const PAGE_SIZE: u32 = 4096;
const WORDS_PER_PAGE: u32 = PAGE_SIZE / 4;
const TOTAL_SLOTS: u32 = 2 * WORDS_PER_PAGE;

pub struct SeqCounter {
    next_seq: u32,
    reserved_until: u32,
    next_slot: Slot,
    flash: Flash<'static>,
}

impl SeqCounter {
    pub async fn load(mut flash: Flash<'static>) -> Result<Self, FlashError> {
        defmt::assert_eq!(
            storage_end() - storage_start(),
            TOTAL_SLOTS * 4,
            "STORAGE region size doesn't match the two-page design"
        );

        let mut max_reserved_until = 0;
        let mut max_slot = Slot(0);

        for page in Page::iter() {
            for slot in page.slots() {
                let reserved_until = read(&mut flash, slot)?;

                if !is_valid_record(reserved_until) {
                    break;
                }

                if reserved_until > max_reserved_until {
                    max_reserved_until = reserved_until;
                    max_slot = slot;
                }
            }
        }

        let next_slot = if max_reserved_until == 0 {
            defmt::info!("[flash] no valid seq records, starting fresh");
            Slot(0)
        } else {
            max_slot.next()
        };

        defmt::info!(
            "[flash] scanned for seq: running from: {}",
            max_reserved_until,
        );

        Ok(Self {
            reserved_until: max_reserved_until,
            next_seq: max_reserved_until,
            flash,
            next_slot,
        })
    }

    pub async fn next(&mut self) -> Result<u32, FlashError> {
        if self.next_seq >= self.reserved_until {
            self.advance_reservation().await?
        }

        let seq = self.next_seq;
        self.next_seq += 1;
        Ok(seq)
    }

    async fn advance_reservation(&mut self) -> Result<(), FlashError> {
        if let Some(page) = self.next_slot.page_started() {
            defmt::info!("[flash] entering page {}, erasing", page);
            erase(&mut self.flash, page).await?;
        }

        let new_ceiling = self.reserved_until + RESERVATION_BLOCK;
        write(&mut self.flash, self.next_slot, new_ceiling).await?;

        let val = read(&mut self.flash, self.next_slot)?;
        defmt::assert_eq!(
            new_ceiling,
            val,
            "[flash] saved value differs from what it should be"
        );

        self.reserved_until = new_ceiling;
        defmt::info!(
            "[flash] reserved until {} at slot {}",
            self.reserved_until,
            self.next_slot
        );

        self.next_slot = self.next_slot.next();

        Ok(())
    }
}

fn storage_start() -> u32 {
    &raw const __storage_start as u32
}

fn storage_end() -> u32 {
    &raw const __storage_end as u32
}

fn read(flash: &mut Flash, slot: Slot) -> Result<u32, FlashError> {
    let mut buf: [u8; 4] = [0; _];

    flash.read(slot.addr(), &mut buf)?;

    Ok(u32::from_le_bytes(buf))
}

async fn write(flash: &mut Flash<'_>, slot: Slot, data: u32) -> Result<(), FlashError> {
    flash.write(slot.addr(), &data.to_le_bytes()).await?;

    Ok(())
}

async fn erase(flash: &mut Flash<'_>, page: Page) -> Result<(), FlashError> {
    let (from, to) = page.byte_range();

    flash.erase(from, to).await?;

    Ok(())
}

fn is_valid_record(w: u32) -> bool {
    w != 0 && w.is_multiple_of(RESERVATION_BLOCK)
}

#[derive(Debug, Clone, Copy, PartialEq, defmt::Format)]
struct Slot(u32);

impl Slot {
    fn next(self) -> Slot {
        Slot((self.0 + 1) % TOTAL_SLOTS)
    }

    fn addr(self) -> u32 {
        storage_start() + self.0 * 4
    }

    fn page_started(self) -> Option<Page> {
        let page = self.page();
        if page.start() == self {
            Some(page)
        } else {
            None
        }
    }

    fn page(self) -> Page {
        match self.0 {
            0..WORDS_PER_PAGE => Page::A,
            WORDS_PER_PAGE..TOTAL_SLOTS => Page::B,
            _ => defmt::unreachable!("invalid slot"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, defmt::Format)]
enum Page {
    A,
    B,
}

impl Page {
    fn byte_range(self) -> (u32, u32) {
        let from = self.start().addr();
        (from, from + PAGE_SIZE)
    }

    fn start(self) -> Slot {
        match self {
            Self::A => Slot(0),
            Self::B => Slot(WORDS_PER_PAGE),
        }
    }

    fn iter() -> impl Iterator<Item = Self> {
        [Page::A, Page::B].into_iter()
    }

    fn slots(self) -> impl Iterator<Item = Slot> {
        let start = self.start().0;

        (start..start + WORDS_PER_PAGE).map(Slot)
    }
}
