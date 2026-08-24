//! The provisioning record in `UICR.CUSTOMER` — the per-device AEAD key as it
//! is stored on the chip.
//!
//! **This is a wire format.** `homescope-provision` writes these exact words
//! over SWD and `homescope-board`'s `chip::device_key` reads them back on every
//! boot. The two halves are compiled for different targets from different
//! workspaces and never share a type across that boundary, so this module is
//! the only place a mistake in the layout can be caught — which is what the
//! known-answer tests at the bottom are for.
//!
//! # Record layout
//!
//! ```text
//! 0x10001080   b"HK"       2 B   magic
//! 0x10001082   version     1 B   currently 1
//! 0x10001083   0x00        1 B   padding
//! 0x10001084   key        32 B   key byte i lives at 0x10001084 + i
//! ```
//!
//! 36 bytes — 9 of the 32 words in the 128-byte `CUSTOMER` block. The header
//! comes first for the same reason `ver` precedes `seq` on the air packet: a
//! version field has to be readable without already knowing the layout it
//! describes.
//!
//! # Words, not bytes
//!
//! [`encode`] and [`decode`] speak `[u32; UICR_RECORD_WORDS]` because both
//! callers do — firmware reads `UICR.customer(i)`, the provisioning tool reads
//! and writes through probe-rs, and `NVMC` programs UICR one word at a time.
//! A byte-oriented API would push the little-endian conversion out into two
//! crates and leave the single decision most likely to be made backwards
//! untested in both.
//!
//! Words are little-endian, so key byte `i` lands at `0x10001084 + i` and a hex
//! dump of the region shows the key in order. Getting that backwards produces a
//! device whose packets fail their AEAD tag with no other symptom — the same
//! failure class `packet::cipher`'s known-answer test guards against, and
//! guarded here the same way.
//!
//! # Blank and half-written records
//!
//! Erased flash reads as all-ones, so [`UicrRecord::Blank`] is decided before
//! the magic check: "never provisioned" needs a different message from
//! "something overwrote your key", and only the first of them means *go ahead
//! and write*.
//!
//! Blankness is judged on the **header word alone**, not the whole record, and
//! that is load-bearing. The provisioning tool writes the key words first and
//! the header word last, which makes the header a commit marker: a run
//! interrupted between the two leaves a key sitting behind an erased header.
//! That has to read as [`UicrRecord::Blank`] so the operator is told to run the
//! tool again, rather than [`RecordError::InvalidMagic`], which means *stop,
//! something else owns this UICR*.
//!
//! # The padding byte is alignment, not a growth slot
//!
//! UICR is write-once-per-bit with no erase short of `NVMC.ERASEUICR`, so a
//! field cannot be added to a record that has already been written. Adding one
//! is a version bump and a re-provision, which is exactly what the version byte
//! exists to make legible.
//!
//! # Versioning
//!
//! UICR survives an ordinary reflash — that is the whole reason the key lives
//! there — so firmware advances while the record stays whatever the tool of the
//! day wrote. New parser, old data. Unlike the air packet, though, this never
//! grows a second parser: a device is within reach of a probe by definition, so
//! a version mismatch means *re-provision this board*, not *support both
//! formats*.
//!
//! # Note on zeroization
//!
//! The byte buffers [`encode`] and [`decode`] build are not zeroized. On the
//! device that would protect nothing: `PacketCipher` holds the key for the
//! whole uptime and UICR holds it until the chip is erased, so anyone able to
//! read that stack frame has SWD and can read `0x10001084` directly. On the
//! host the argument is weaker — a provisioning run leaves a plaintext key in
//! freed stack memory — but the same run holds the key in a [`DeviceKey`] and
//! puts it on a wire regardless. Read that as a reason not to add further
//! copies, not as a question that has been settled.

use thiserror::Error;

use crate::device_key::DeviceKey;

/// Size of the record in bytes: a 4-byte header followed by the key.
pub const UICR_RECORD_LEN: usize = 4 + DeviceKey::SIZE;

/// Size of the record in 32-bit words, which is how UICR is addressed.
pub const UICR_RECORD_WORDS: usize = UICR_RECORD_LEN / 4;

const UICR_MAGIC: [u8; 2] = *b"HK";

const RECORD_VERSION: u8 = 1;

/// `UICR.CUSTOMER` is 32 words, and UICR is addressed a word at a time — a
/// record that overflowed the block or sat off a word boundary would not be
/// writable at all. Asserted at compile time rather than in a test so it holds
/// for the firmware build, which does not run tests.
const _: () = {
    const CUSTOMER_WORDS: usize = 32;

    assert!(
        UICR_RECORD_LEN.is_multiple_of(4),
        "record must be word-aligned"
    );
    assert!(
        UICR_RECORD_WORDS <= CUSTOMER_WORDS,
        "record must fit UICR.CUSTOMER"
    );
};

mod layout {
    use core::ops::Range;

    use crate::device_key::DeviceKey;

    pub const MAGIC: Range<usize> = 0..2;
    pub const VERSION: usize = MAGIC.end;
    pub const PADDING: usize = VERSION + 1;

    const KEY_START: usize = PADDING + 1;
    const KEY_END: usize = KEY_START + DeviceKey::SIZE;
    pub const KEY: Range<usize> = KEY_START..KEY_END;

    pub const KEY_WORDS: Range<usize> = KEY_START / 4..KEY_END / 4;

    const _: () = assert!(KEY_START.is_multiple_of(4), "key must be aligned");
}

/// Builds the words to write to `UICR.CUSTOMER[0..9]`.
///
/// The caller writes the **key words first and word 0 last**, so that an
/// interrupted run leaves a record that [`decode`] reports as
/// [`UicrRecord::Blank`] rather than as corrupt. See the module docs.
pub fn encode(key: &DeviceKey) -> [u32; UICR_RECORD_WORDS] {
    let mut buf = [0u8; UICR_RECORD_LEN];

    buf[layout::MAGIC].copy_from_slice(&UICR_MAGIC);
    buf[layout::VERSION] = RECORD_VERSION;
    buf[layout::PADDING] = 0;

    buf[layout::KEY].copy_from_slice(key.as_bytes());

    let mut words = [0u32; UICR_RECORD_WORDS];
    let (chunks, _) = buf.as_chunks::<4>();
    for (word, bytes) in words.iter_mut().zip(chunks) {
        *word = u32::from_le_bytes(*bytes);
    }

    words
}

/// Reads `UICR.CUSTOMER[0..9]` back into one of three states.
///
/// Every outcome is a fact about the chip in front of you, not a failure of
/// this function — see [`UicrRecord`] for what each one tells the caller to do.
pub fn decode(words: &[u32; UICR_RECORD_WORDS]) -> UicrRecord {
    match decode_header(words[0]) {
        RecordHeader::Blank => return UicrRecord::Blank,
        RecordHeader::Malformed(err) => return UicrRecord::Malformed(err),
        RecordHeader::Present => {}
    }

    let mut key = [0u8; DeviceKey::SIZE];
    for (word, chunk) in words[layout::KEY_WORDS].iter().zip(key.chunks_exact_mut(4)) {
        chunk.copy_from_slice(&word.to_le_bytes());
    }

    UicrRecord::Provisioned(DeviceKey::from_bytes(key))
}

/// Classifies `UICR.CUSTOMER[0]` on its own, without reading the key.
///
/// The header word is written **last**, which makes it a commit marker: a
/// well-formed header implies the key words behind it are complete. That is
/// what lets every reader except the firmware — `info`, the provisioning
/// tool's overwrite check — answer its question from this one word and never
/// pull the key across SWD at all. See the module docs for the write order
/// this relies on.
///
/// [`decode`] is this function plus the key words.
pub fn decode_header(word: u32) -> RecordHeader {
    if word == u32::MAX {
        return RecordHeader::Blank;
    }

    let bytes = word.to_le_bytes();

    if bytes[layout::MAGIC] != UICR_MAGIC {
        return RecordHeader::Malformed(RecordError::InvalidMagic);
    }

    let version = bytes[layout::VERSION];
    if version != RECORD_VERSION {
        return RecordHeader::Malformed(RecordError::UnsupportedVersion(bytes[layout::VERSION]));
    }

    if bytes[layout::PADDING] != 0 {
        return RecordHeader::Malformed(RecordError::InvalidPadding);
    }

    RecordHeader::Present
}

/// What `UICR.CUSTOMER[0]` says about the record behind it.
///
/// The same three states as [`UicrRecord`], minus the key — for the callers
/// that need to know *whether* a device is provisioned and have no business
/// reading *what with*. On the host that is every caller but the write-back
/// verification, which compares against words it already holds.
///
/// [`Present`](Self::Present) carries no version number on purpose. There is
/// only one readable version, so a field here could hold nothing but
/// `RECORD_VERSION`; anything else is already reported as
/// [`RecordError::UnsupportedVersion`], which names the number it found so the
/// operator can be told which tool wrote the board. Give this variant a
/// version field on the day a second one becomes readable, not before.
#[derive(Debug)]
pub enum RecordHeader {
    /// Erased header — an unprovisioned board, or a run interrupted before its
    /// final write. Both mean *run the tool*.
    Blank,
    /// Written, but not a record this version can read.
    Malformed(RecordError),
    /// A well-formed header of the current version. Because the header is
    /// written last, the key words behind it are complete.
    Present,
}

/// Why a non-erased `CUSTOMER` block is not a record this version can read.
///
/// These describe the *format*, not what to do about it. The advice differs by
/// caller — a sensor logs "reflash or re-provision" over RTT and halts, the
/// provisioning tool refuses to overwrite without a human — so each one words
/// its own message and this enum stays neutral.
#[derive(Debug, Error)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum RecordError {
    /// Not erased, and not our record. Something else wrote `CUSTOMER[0]`;
    /// find out what before erasing, because that data may matter.
    #[error("UICR record header has invalid magic")]
    InvalidMagic,

    /// The reserved byte is not zero — either a tool wrote a field this
    /// version does not know about, or the record is damaged.
    #[error("UICR record header has invalid padding")]
    InvalidPadding,

    /// A record written by a newer tool. There is deliberately no second
    /// parser; re-provision the board.
    #[error("UICR record version is not supported: {0}")]
    UnsupportedVersion(u8),
}

/// What `UICR.CUSTOMER` holds, as three states rather than a `Result`.
///
/// [`Blank`](Self::Blank) is not a failure — a blank chip is the ordinary input
/// to provisioning, and the state that the tool is allowed to write over
/// without asking. Modelling it as an error would mean every call site writing
/// an arm that says "this error is not an error".
///
/// The three variants exist because they call for three different actions:
///
/// | state | provisioning tool | sensor firmware at boot |
/// |---|---|---|
/// | [`Blank`](Self::Blank) | write, no prompt | halt: "not provisioned" |
/// | [`Malformed`](Self::Malformed) | refuse; needs a human | halt: "provisioned wrongly" |
/// | [`Provisioned`](Self::Provisioned) | refuse unless reprovisioning | run |
///
/// Note that [`Provisioned`](Self::Provisioned) means the record is
/// *well-formed*, not that the key matches the one the API holds for this
/// device. A rotated key still decodes cleanly here and still fails every AEAD
/// tag, so a tool reporting this state should not claim more than it knows.
#[derive(Debug)]
pub enum UicrRecord {
    /// Erased UICR — a new or chip-erased board, or a provisioning run that
    /// was interrupted before it wrote the header word.
    Blank,
    /// Non-erased, but not a record this version can read.
    Malformed(RecordError),
    /// A well-formed record, carrying the key it holds.
    Provisioned(DeviceKey),
}

#[cfg(test)]
mod test {
    use super::*;

    /// A key whose every byte differs, so a test can catch a reversal that
    /// `[0xAB; 32]` would sail straight through.
    const KEY_BYTES: [u8; DeviceKey::SIZE] = {
        let mut bytes = [0u8; DeviceKey::SIZE];
        let mut i = 0;
        while i < DeviceKey::SIZE {
            bytes[i] = i as u8;
            i += 1;
        }
        bytes
    };

    /// The literal words `KEY_BYTES` must produce, written out by hand rather
    /// than computed — see `encodes_to_the_documented_words` for why.
    ///
    /// Word 0 is `[b'H', b'K', 1, 0]` read little-endian; the rest are the key
    /// in order, four bytes to a word, also little-endian.
    const ENCODED: [u32; UICR_RECORD_WORDS] = [
        0x0001_4B48,
        0x0302_0100,
        0x0706_0504,
        0x0B0A_0908,
        0x0F0E_0D0C,
        0x1312_1110,
        0x1716_1514,
        0x1B1A_1918,
        0x1F1E_1D1C,
    ];

    fn words_to_bytes(words: &[u32; UICR_RECORD_WORDS]) -> [u8; UICR_RECORD_LEN] {
        let mut bytes = [0u8; UICR_RECORD_LEN];
        for (word, chunk) in words.iter().zip(bytes.chunks_exact_mut(4)) {
            chunk.copy_from_slice(&word.to_le_bytes());
        }
        bytes
    }

    /// Fixed input, fixed output — the one test that can catch a change to the
    /// byte order or the header layout.
    ///
    /// A round-trip through [`decode`] cannot: both halves share the same
    /// little-endian loop, so a reversal changes both and the round-trip stays
    /// green while every device already in the field stops decrypting. The
    /// expected words are therefore literals, computed by hand.
    #[test]
    fn encodes_to_the_documented_words() {
        assert_eq!(encode(&DeviceKey::from_bytes(KEY_BYTES)), ENCODED);
    }

    /// The claim the module docs make about a hex dump: key byte `i` sits at
    /// `0x10001084 + i`, so `xxd` of the region shows the key in order.
    ///
    /// This is the property that is impossible to check from the firmware side
    /// and easy to get backwards on the host side.
    #[test]
    fn key_byte_i_lands_at_offset_four_plus_i() {
        let bytes = words_to_bytes(&encode(&DeviceKey::from_bytes(KEY_BYTES)));

        assert_eq!(&bytes[0..2], b"HK");
        assert_eq!(bytes[2], RECORD_VERSION);
        assert_eq!(bytes[3], 0);

        for (i, &byte) in KEY_BYTES.iter().enumerate() {
            assert_eq!(bytes[4 + i], byte, "key byte {i} is misplaced");
        }
    }

    /// The other direction, also from literals rather than from `encode`.
    #[test]
    fn decodes_the_documented_words() {
        let UicrRecord::Provisioned(key) = decode(&ENCODED) else {
            panic!("a well-formed record should decode");
        };

        assert_eq!(key.as_bytes(), &KEY_BYTES);
    }

    /// A factory-fresh or chip-erased board. This is the tool's most common
    /// input and the only state it may overwrite without asking.
    #[test]
    fn erased_uicr_reads_as_blank() {
        assert!(matches!(
            decode(&[u32::MAX; UICR_RECORD_WORDS]),
            UicrRecord::Blank
        ));
    }

    /// The header word is a commit marker: the tool writes the key words first
    /// and word 0 last, so an interrupted run leaves a key behind an erased
    /// header. That must read as `Blank` — "run the tool again" — and not as
    /// `InvalidMagic`, which tells the operator to stop and investigate.
    ///
    /// Judging blankness on the whole record instead of the header word would
    /// get exactly this case wrong.
    #[test]
    fn interrupted_provision_reads_as_blank() {
        let mut words = ENCODED;
        words[0] = u32::MAX;

        assert!(matches!(decode(&words), UicrRecord::Blank));
    }

    /// Erased flash is all-ones, so a header whose *first* byte is `0xFF` and
    /// whose rest is not has been written by something. Reporting that as
    /// `Blank` would invite the tool to overwrite someone else's data.
    #[test]
    fn a_partly_erased_header_is_not_blank() {
        let mut words = ENCODED;
        words[0] = 0x0001_4BFF;

        assert!(matches!(
            decode(&words),
            UicrRecord::Malformed(RecordError::InvalidMagic)
        ));
    }

    #[test]
    fn foreign_data_is_rejected_by_magic() {
        let mut words = ENCODED;
        words[0] = 0x1234_5678;

        assert!(matches!(
            decode(&words),
            UicrRecord::Malformed(RecordError::InvalidMagic)
        ));
    }

    /// The version is reported back so the message can name it — there is no
    /// second parser, so the only useful response is "re-provision with a tool
    /// that writes version N".
    #[test]
    fn a_newer_version_is_named_in_the_error() {
        let mut words = ENCODED;
        words[0] = 0x0002_4B48;

        assert!(matches!(
            decode(&words),
            UicrRecord::Malformed(RecordError::UnsupportedVersion(2))
        ));
    }

    /// Padding is checked because it is the one byte a future version could
    /// claim; a non-zero value means the record was written by a tool this
    /// parser does not understand, even when the version byte matches.
    #[test]
    fn non_zero_padding_is_rejected() {
        let mut words = ENCODED;
        words[0] = 0x0101_4B48;

        assert!(matches!(
            decode(&words),
            UicrRecord::Malformed(RecordError::InvalidPadding)
        ));
    }

    /// The header decoder is what `info` and the tool's overwrite check call,
    /// and the point of it is that neither has to read the key to decide. This
    /// is the answer they get for a provisioned board.
    #[test]
    fn the_header_alone_recognises_a_written_record() {
        assert!(matches!(decode_header(ENCODED[0]), RecordHeader::Present));
    }

    /// The same question on a factory-fresh board, from the same single word.
    #[test]
    fn the_header_alone_recognises_an_erased_record() {
        assert!(matches!(decode_header(u32::MAX), RecordHeader::Blank));
    }

    /// [`decode`] does not re-validate the key words, and must not: the header
    /// is written last, so a well-formed header already proves they are there.
    ///
    /// That is the property the whole design leans on. It is why `Blank` is the
    /// only half-written state there is, and why a caller holding one word may
    /// report on a board without pulling its key across SWD. If this ever stops
    /// holding, header-only inspection stops being sound.
    #[test]
    fn a_well_formed_header_is_trusted_for_the_key_words() {
        let mut words = [u32::MAX; UICR_RECORD_WORDS];
        words[0] = ENCODED[0];

        let UicrRecord::Provisioned(key) = decode(&words) else {
            panic!("a well-formed header should be taken at its word");
        };

        assert_eq!(key.as_bytes(), &[0xFF; DeviceKey::SIZE]);
    }

    /// The header is four bytes and the key is the rest — stated as a test so
    /// that growing the record without revisiting the layout constants fails
    /// here rather than on a bench.
    ///
    /// The block-size and alignment invariants are `const` assertions at module
    /// scope instead, since they have to hold for the firmware build too.
    #[test]
    fn the_record_is_a_header_plus_a_key() {
        assert_eq!(UICR_RECORD_LEN, 4 + DeviceKey::SIZE);
        assert_eq!(UICR_RECORD_WORDS, 9);
        assert_eq!(layout::KEY.end - layout::KEY.start, DeviceKey::SIZE);
    }
}
