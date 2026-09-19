//! What a firmware image says about itself.
//!
//! Two facts, both read from the ELF rather than taken on trust:
//!
//! - **`app_start`** — the lowest flash address the image occupies, which is
//!   what says whether it expects something beneath it.
//! - **`storage`** — the seq checkpoint region, from the `__storage_start` /
//!   `__storage_end` symbols.
//!
//! ⚠️ The storage symbols are **absolute** (`SHN_ABS`), not addresses inside a
//! section: `board/build.rs` emits them from the linker script and the region
//! holds no output section, so nothing lands in the ELF and their *addresses*
//! are the whole payload. That is what keeps the counter out of a flashed image
//! — and it makes the image the authority on where its own storage lives, per
//! board, with no second copy to drift.

use object::{
    Endianness, Object as _, ObjectSymbol as _,
    elf::{PT_LOAD, SHN_ABS},
    read::elf::{ElfFile32, ProgramHeader as _},
};
use thiserror::Error;

/// The symbols `board/build.rs` emits around the seq checkpoint region.
const STORAGE_START: &str = "__storage_start";
const STORAGE_END: &str = "__storage_end";

/// nRF52840 flash page size — the erase granularity (nRF52840 PS, NVMC chapter).
pub const PAGE_SIZE: u64 = 4096;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    /// Lowest address in flash the image occupies.
    pub app_start: u64,

    /// `(start, end)` of the seq checkpoint region, half-open. `None` for an
    /// image that keeps no counter — a receiver build, say.
    pub storage: Option<(u64, u64)>,
}

pub fn read(bytes: &[u8]) -> Result<Image, Error> {
    let file = ElfFile32::<Endianness>::parse(bytes).map_err(|_| Error::NotAnElf)?;
    let endian = file.endian();

    // ⚠️ Physical addresses, not virtual: an initialised-data segment lives in
    // RAM at run time but is *stored* in flash, and only the flash location is
    // relevant to what gets written. Segments with no file contents (`.bss`)
    // occupy no flash at all.
    let app_start = file
        .elf_program_headers()
        .iter()
        .filter(|header| header.p_type(endian) == PT_LOAD && header.p_filesz(endian) > 0)
        .map(|header| u64::from(header.p_paddr(endian)))
        .min()
        .ok_or(Error::NoLoadableSegments)?;

    Ok(Image {
        app_start,
        storage: storage(&file)?,
    })
}

fn storage(file: &ElfFile32<'_, Endianness>) -> Result<Option<(u64, u64)>, Error> {
    let mut start = None;
    let mut end = None;

    for symbol in file.symbols() {
        // An address-carrying symbol that is not in any section: exactly what
        // `__storage_start = ORIGIN(STORAGE)` produces.
        if symbol.elf_symbol().st_shndx.get(file.endian()) != SHN_ABS {
            continue;
        }

        match symbol.name() {
            Ok(STORAGE_START) => start = Some(symbol.address()),
            Ok(STORAGE_END) => end = Some(symbol.address()),
            _ => {}
        }
    }

    match (start, end) {
        (None, None) => Ok(None),

        (Some(start), Some(end)) => {
            // A bad range would be erased page by page against live flash, so it
            // is checked here rather than at the erase.
            if end <= start {
                return Err(Error::StorageNotAscending { start, end });
            }
            if !start.is_multiple_of(PAGE_SIZE) || !end.is_multiple_of(PAGE_SIZE) {
                return Err(Error::StorageUnaligned { start, end });
            }

            Ok(Some((start, end)))
        }

        // One without the other means the linker script changed shape; guessing
        // the missing half would erase an address nothing vouched for.
        _ => Err(Error::StorageIncomplete),
    }
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("not an ELF file this tool can read (expected 32-bit little-endian)")]
    NotAnElf,

    #[error("the image has no loadable segments — nothing to flash")]
    NoLoadableSegments,

    #[error(
        "the image declares only one of `{STORAGE_START}` / `{STORAGE_END}`; \
         it was built by a linker script this tool does not recognise"
    )]
    StorageIncomplete,

    #[error("storage region {start:#X}..{end:#X} does not ascend")]
    StorageNotAscending { start: u64, end: u64 },

    #[error("storage region {start:#X}..{end:#X} is not page-aligned ({PAGE_SIZE} bytes)")]
    StorageUnaligned { start: u64, end: u64 },
}

#[cfg(test)]
pub mod test {
    use super::*;

    // Minimal ELF32 little-endian ARM executable, assembled by hand so these
    // tests need no built firmware. Layout:
    //
    //   header | phdr | payload | symtab | strtab | shstrtab | shdrs
    const EHDR_LEN: u32 = 52;
    const PHDR_LEN: u32 = 32;
    const SHDR_LEN: u32 = 40;
    const SYM_LEN: u32 = 16;

    #[derive(Default)]
    struct Buf(Vec<u8>);

    impl Buf {
        fn u8(&mut self, v: u8) -> &mut Self {
            self.0.push(v);
            self
        }
        fn u16(&mut self, v: u16) -> &mut Self {
            self.0.extend_from_slice(&v.to_le_bytes());
            self
        }
        fn u32(&mut self, v: u32) -> &mut Self {
            self.0.extend_from_slice(&v.to_le_bytes());
            self
        }
        fn bytes(&mut self, v: &[u8]) -> &mut Self {
            self.0.extend_from_slice(v);
            self
        }
    }

    /// One `PT_LOAD` segment at `base`, plus whichever storage symbols are
    /// asked for. `filler` varies the bytes so two fixtures hash differently.
    pub fn fixture(base: u32, storage: Option<(u32, u32)>, filler: u8) -> Vec<u8> {
        symbols_fixture(
            base,
            storage.map(|(s, e)| vec![(STORAGE_START, s), (STORAGE_END, e)]),
            filler,
        )
    }

    /// The general form: name every absolute symbol explicitly, so a test can
    /// emit half a pair or a bad range.
    pub fn symbols_fixture(base: u32, symbols: Option<Vec<(&str, u32)>>, filler: u8) -> Vec<u8> {
        let symbols = symbols.unwrap_or_default();

        let payload = [filler; 64];
        let payload_off = EHDR_LEN + PHDR_LEN;

        // .strtab: a leading NUL, then each symbol name.
        let mut strtab = vec![0u8];
        let name_offsets: Vec<u32> = symbols
            .iter()
            .map(|(name, _)| {
                let at = strtab.len() as u32;
                strtab.extend_from_slice(name.as_bytes());
                strtab.push(0);
                at
            })
            .collect();

        let symtab_off = payload_off + payload.len() as u32;
        let symtab_len = (symbols.len() as u32 + 1) * SYM_LEN; // +1: index 0 is null
        let strtab_off = symtab_off + symtab_len;
        let shstrtab_off = strtab_off + strtab.len() as u32;

        const SH_NAMES: &[u8] = b"\0.symtab\0.strtab\0.shstrtab\0";
        let shoff = shstrtab_off + SH_NAMES.len() as u32;

        let mut buf = Buf::default();

        // ---- ELF header ----
        buf.bytes(&[0x7f, b'E', b'L', b'F'])
            .u8(1) // ELFCLASS32
            .u8(1) // ELFDATA2LSB
            .u8(1) // EV_CURRENT
            .bytes(&[0; 9]) // OSABI + pad
            .u16(2) // ET_EXEC
            .u16(40) // EM_ARM
            .u32(1) // e_version
            .u32(base) // e_entry
            .u32(EHDR_LEN) // e_phoff
            .u32(shoff)
            .u32(0) // e_flags
            .u16(EHDR_LEN as u16)
            .u16(PHDR_LEN as u16)
            .u16(1) // e_phnum
            .u16(SHDR_LEN as u16)
            .u16(4) // e_shnum
            .u16(3); // e_shstrndx

        // ---- program header: PT_LOAD at `base` ----
        buf.u32(PT_LOAD)
            .u32(payload_off)
            .u32(base) // p_vaddr
            .u32(base) // p_paddr
            .u32(payload.len() as u32)
            .u32(payload.len() as u32)
            .u32(5) // PF_R | PF_X
            .u32(4);

        buf.bytes(&payload);

        // ---- .symtab: null symbol, then one absolute symbol per name ----
        buf.bytes(&[0; SYM_LEN as usize]);
        for (offset, (_, value)) in name_offsets.iter().zip(&symbols) {
            buf.u32(*offset) // st_name
                .u32(*value) // st_value
                .u32(0) // st_size
                .u8(0x10) // STB_GLOBAL | STT_NOTYPE
                .u8(0) // st_other
                .u16(SHN_ABS); // no section: the address is the payload
        }

        buf.bytes(&strtab).bytes(SH_NAMES);

        // ---- section headers ----
        let shdr = |buf: &mut Buf, name, kind, off, size, link, info, entsize| {
            buf.u32(name)
                .u32(kind)
                .u32(0) // sh_flags
                .u32(0) // sh_addr
                .u32(off)
                .u32(size)
                .u32(link)
                .u32(info)
                .u32(1) // sh_addralign
                .u32(entsize);
        };

        shdr(&mut buf, 0, 0, 0, 0, 0, 0, 0); // SHT_NULL
        shdr(
            &mut buf, 1, // ".symtab"
            2, // SHT_SYMTAB
            symtab_off, symtab_len, 2, // sh_link -> .strtab
            1, // sh_info: first non-local symbol
            SYM_LEN,
        );
        shdr(
            &mut buf,
            9, // ".strtab"
            3, // SHT_STRTAB
            strtab_off,
            strtab.len() as u32,
            0,
            0,
            0,
        );
        shdr(
            &mut buf,
            17, // ".shstrtab"
            3,
            shstrtab_off,
            SH_NAMES.len() as u32,
            0,
            0,
            0,
        );

        buf.0
    }

    /// The fixture has to be a real ELF, or every test below proves nothing.
    #[test]
    fn the_fixture_parses_as_an_elf() {
        let bytes = fixture(0, Some((0x000F_E000, 0x0010_0000)), 0xAA);

        assert!(ElfFile32::<Endianness>::parse(&bytes[..]).is_ok());
    }

    #[test]
    fn reads_the_load_address_and_storage_range() {
        let image = read(&fixture(0, Some((0x000F_E000, 0x0010_0000)), 0xAA)).expect("reads");

        assert_eq!(image.app_start, 0);
        assert_eq!(image.storage, Some((0x000F_E000, 0x0010_0000)));
    }

    /// The layout a pre-2026-09-19 XIAO build had: an application expecting a
    /// bootloader beneath it. The reader reports it; the flash guard is what
    /// refuses it.
    #[test]
    fn an_offset_image_reports_its_offset() {
        let image = read(&fixture(
            0x0002_7000,
            Some((0x000F_2000, 0x000F_4000)),
            0xAA,
        ))
        .unwrap();

        assert_eq!(image.app_start, 0x0002_7000);
        assert_eq!(image.storage, Some((0x000F_2000, 0x000F_4000)));
    }

    /// A receiver build keeps no seq counter, so it declares no storage — that
    /// is absence, not an error.
    #[test]
    fn an_image_without_storage_symbols_is_fine() {
        let image = read(&fixture(0, None, 0xAA)).expect("reads");

        assert_eq!(image.storage, None);
    }

    /// ⚠️ Half a pair means the linker script changed shape. Guessing the other
    /// half would erase an address nothing vouched for.
    #[test]
    fn half_a_storage_pair_is_an_error() {
        for symbols in [
            vec![(STORAGE_START, 0x000F_E000u32)],
            vec![(STORAGE_END, 0x0010_0000u32)],
        ] {
            let bytes = symbols_fixture(0, Some(symbols), 0xAA);

            assert!(matches!(read(&bytes), Err(Error::StorageIncomplete)));
        }
    }

    /// Both are checked here rather than at the erase, which runs against live
    /// flash a page at a time.
    #[test]
    fn a_backwards_or_unaligned_storage_range_is_refused() {
        let backwards = fixture(0, Some((0x0010_0000, 0x000F_E000)), 0xAA);
        assert!(matches!(
            read(&backwards),
            Err(Error::StorageNotAscending { .. })
        ));

        let empty = fixture(0, Some((0x000F_E000, 0x000F_E000)), 0xAA);
        assert!(matches!(
            read(&empty),
            Err(Error::StorageNotAscending { .. })
        ));

        let unaligned = fixture(0, Some((0x000F_E001, 0x0010_0000)), 0xAA);
        assert!(matches!(
            read(&unaligned),
            Err(Error::StorageUnaligned { .. })
        ));
    }

    #[test]
    fn a_file_that_is_not_an_elf_is_refused() {
        assert!(matches!(
            read(b"#!/bin/sh\necho hi\n"),
            Err(Error::NotAnElf)
        ));
    }
}
