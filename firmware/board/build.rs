//! Generates `memory.x` for the selected board into `OUT_DIR`, and puts it on
//! the linker search path.
//!
//! Every board follows one layout policy:
//!
//! - the application starts at `app_start`,
//! - the top [`STORAGE_LEN`] bytes below `flash_top` are reserved as persistent
//!   storage (the sensor's seq checkpoint — see `sensor/src/seq_counter.rs`),
//! - the application gets everything in between.
//!
//! Only the per-board *facts* differ (`app_start`, `flash_top`, the RAM window);
//! they live in the [`BOARDS`] table. [`Layout::derive`] turns those facts into a
//! validated set of addresses, and [`Layout::render`] emits the linker script.
//! Hand-maintained `memory-*.x` files used to duplicate that derivation per
//! board, which is how the XIAO's FLASH length silently grew to overlap the
//! bootloader.
//!
//! Nothing is ever emitted into the STORAGE region: it holds no output section,
//! only the `__storage_start` / `__storage_end` symbols marking its bounds. That
//! keeps it out of the ELF, so `probe-rs` sector-erase and UF2 flashing both
//! leave the counter intact (a chip-erase still wipes it).

use std::path::PathBuf;
use std::{env, fs};

/// nRF52840 flash page size — the erase granularity (nRF52840 PS, NVMC chapter).
const PAGE_SIZE: u32 = 4096;

/// Two pages: the checkpoint ping-pongs between them so that a crash mid-update
/// never leaves zero valid copies.
const STORAGE_LEN: u32 = 2 * PAGE_SIZE;

/// The board-varying facts. Pure data — no derived or validated fields; that is
/// [`Layout`]'s job.
#[derive(Clone, Copy)]
struct BoardMemory {
    /// Cargo feature that selects this board, e.g. `CARGO_FEATURE_DB40`.
    feature_env: &'static str,
    /// Human-readable name, for the generated file's header comment.
    name: &'static str,
    /// Lowest address the application may occupy.
    app_start: u32,
    /// First address the application must *not* touch.
    flash_top: u32,
    /// What lives at `flash_top` — used in comments and assert messages.
    flash_top_desc: &'static str,
    ram_start: u32,
    ram_len: u32,
    /// Extra lines for the generated header comment: whatever a reader needs to
    /// know about why this board's addresses are what they are.
    notes: &'static [&'static str],
}

/// Every supported board. Adding one is a single entry here — selection and
/// validation below are board-count agnostic.
const BOARDS: &[BoardMemory] = &[
    BoardMemory {
        feature_env: "CARGO_FEATURE_DB40",
        name: "Raytac MDBT50Q-DB-40",
        app_start: 0x0000_0000,
        flash_top: 0x0010_0000,
        flash_top_desc: "top of the 1 MB flash",
        ram_start: 0x2000_0000,
        ram_len: 256 * 1024,
        notes: &["Bare module: no MBR, no SoftDevice, no bootloader."],
    },
    BoardMemory {
        feature_env: "CARGO_FEATURE_XIAO",
        name: "Seeed XIAO nRF52840 Plus",
        app_start: 0x0002_7000,
        flash_top: 0x000F_4000,
        flash_top_desc: "start of the Adafruit UF2 bootloader",
        ram_start: 0x2002_0000,
        ram_len: 128 * 1024,
        notes: &[
            "Adafruit UF2 bootloader v0.9.2 with SoftDevice S140 7.3.0 pre-installed.",
            "",
            "0x00000000 - 0x00000FFF  Nordic MBR",
            "0x00001000 - 0x00026FFF  SoftDevice S140 7.3.0 (inert — we use nrf-sdc)",
            "0x000F4000 - 0x000FDFFF  Adafruit UF2 bootloader  \\",
            "0x000FE000 - 0x000FEFFF  MBR params page           > bootloader-owned",
            "0x000FF000 - 0x000FFFFF  bootloader settings      /",
        ],
    },
];

/// A validated flash/RAM layout, derived from a [`BoardMemory`]. If one of these
/// exists, its addresses are self-consistent — [`Layout::derive`] panics
/// otherwise, failing the build.
struct Layout {
    board: BoardMemory,
    storage_start: u32,
    flash_len: u32,
}

impl Layout {
    /// Derive the storage/application split from a board's facts, validating
    /// alignment and that everything fits. Panics (failing the build) on any
    /// inconsistency.
    fn derive(board: BoardMemory) -> Self {
        let BoardMemory {
            name,
            app_start,
            flash_top,
            ..
        } = board;

        assert_eq!(
            app_start % PAGE_SIZE,
            0,
            "{name}: app_start {app_start:#X} is not page-aligned"
        );
        assert_eq!(
            flash_top % PAGE_SIZE,
            0,
            "{name}: flash_top {flash_top:#X} is not page-aligned"
        );

        let storage_start = flash_top.checked_sub(STORAGE_LEN).unwrap_or_else(|| {
            panic!("{name}: flash_top {flash_top:#X} leaves no room for STORAGE")
        });

        let flash_len = storage_start.checked_sub(app_start).unwrap_or_else(|| {
            panic!(
                "{name}: no room for the application between app_start {app_start:#X} \
                 and STORAGE at {storage_start:#X}"
            )
        });
        assert!(flash_len > 0, "{name}: application region is empty");

        Self {
            board,
            storage_start,
            flash_len,
        }
    }

    /// Emit the linker script for this layout.
    fn render(&self) -> String {
        let BoardMemory {
            name,
            app_start,
            flash_top,
            flash_top_desc,
            ram_start,
            ram_len,
            notes,
            ..
        } = self.board;
        let Self {
            storage_start,
            flash_len,
            ..
        } = *self;

        let app_end = storage_start - 1;
        let storage_end = flash_top - 1;
        let flash_len_k = flash_len / 1024;
        let storage_len_k = STORAGE_LEN / 1024;

        let notes: String = notes
            .iter()
            .map(|line| {
                if line.is_empty() {
                    "\n".to_string()
                } else {
                    format!("   {line}\n")
                }
            })
            .collect();

        format!(
            r#"/* GENERATED by homescope-board/build.rs — do not edit.
   Change the board tables in that build script instead.

   Board: {name}

{notes}
   Flash map:
     {app_start:#010X} - {app_end:#010X}  application            ({flash_len_k} KiB)
     {storage_start:#010X} - {storage_end:#010X}  seq checkpoint storage ({storage_len_k} KiB)

   Application ceiling: {flash_top:#010X} — {flash_top_desc}
*/

MEMORY
{{
  FLASH   : ORIGIN = {app_start:#010X}, LENGTH = {flash_len:#X}
  STORAGE : ORIGIN = {storage_start:#010X}, LENGTH = {STORAGE_LEN:#X}
  RAM     : ORIGIN = {ram_start:#010X}, LENGTH = {ram_len:#X}
}}

/* STORAGE holds no output section, so nothing lands in the ELF. These symbols
   have no contents — their *addresses* are the payload. */
__storage_start = ORIGIN(STORAGE);
__storage_end   = ORIGIN(STORAGE) + LENGTH(STORAGE);

ASSERT(ORIGIN(STORAGE) % {PAGE_SIZE} == 0,
  "STORAGE must start on a flash page boundary");
ASSERT(LENGTH(STORAGE) % {PAGE_SIZE} == 0,
  "STORAGE must be a whole number of flash pages");
ASSERT(ORIGIN(FLASH) + LENGTH(FLASH) == ORIGIN(STORAGE),
  "FLASH must abut STORAGE — no gap, no overlap");
ASSERT(ORIGIN(STORAGE) + LENGTH(STORAGE) == {flash_top:#X},
  "STORAGE must end at the {flash_top_desc}");
"#
        )
    }
}

/// Pick the board whose Cargo feature is enabled, requiring exactly one.
fn select_board() -> BoardMemory {
    let selected: Vec<&BoardMemory> = BOARDS
        .iter()
        .filter(|board| env::var_os(board.feature_env).is_some())
        .collect();

    match selected.as_slice() {
        [board] => **board,
        [] => panic!(
            "no board feature selected — enable exactly one of: {}",
            feature_list()
        ),
        _ => panic!(
            "multiple board features enabled ({}); Cargo features are additive, so \
             `--features board-<x>` still pulls in the default. Pass \
             `--no-default-features` alongside the one you want.",
            selected
                .iter()
                .map(|b| b.name)
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

fn feature_list() -> String {
    BOARDS
        .iter()
        .map(|b| b.feature_env)
        .collect::<Vec<_>>()
        .join(", ")
}

fn main() {
    let script = Layout::derive(select_board()).render();

    let out = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR is always set by cargo"));
    fs::write(out.join("memory.x"), script).expect("failed to write memory.x");

    println!("cargo:rustc-link-search={}", out.display());
    println!("cargo:rerun-if-changed=build.rs");
}
