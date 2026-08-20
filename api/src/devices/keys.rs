use std::{collections::HashMap, fs, path::Path};

use anyhow::{Context as _, bail};
use chacha20poly1305::{
    KeyInit as _, XChaCha20Poly1305, XNonce,
    aead::{Aead as _, Generate, Payload},
};
use homescope_common::{device_addr::DeviceAddr, device_key::DeviceKey};
use thiserror::Error;
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

mod layout {
    use std::ops::Range;

    use homescope_common::device_key::DeviceKey;

    const VER_SIZE: usize = 1;
    const KEK_VER_SIZE: usize = 1;
    pub const NONCE_SIZE: usize = 24;
    const TAG_SIZE: usize = 16;
    pub const SEALED_SIZE: usize = DeviceKey::SIZE + TAG_SIZE;

    pub const VER: usize = 0;

    pub const KEK_VER: usize = VER + VER_SIZE;

    const NONCE_START: usize = KEK_VER + KEK_VER_SIZE;
    const NONCE_END: usize = NONCE_START + NONCE_SIZE;
    pub const NONCE: Range<usize> = NONCE_START..NONCE_END;

    const SEALED_START: usize = NONCE_END;
    const SEALED_END: usize = SEALED_START + SEALED_SIZE;
    pub const SEALED: Range<usize> = SEALED_START..SEALED_END;

    const _: () = assert!(
        TAG_SIZE == size_of::<chacha20poly1305::Tag>(),
        "Poly1305 tag length"
    );
    const _: () = assert!(
        NONCE_SIZE == size_of::<chacha20poly1305::XNonce>(),
        "XChaCha nonce length"
    );
    const _: () = assert!(SEALED_END == 74, "sealed key layout is a DB format");
}

#[derive(Debug, Clone)]
pub struct SealedDeviceKey([u8; Self::SIZE]);

impl SealedDeviceKey {
    pub const VERSION: u8 = 1;

    pub const SIZE: usize = layout::SEALED.end;

    pub fn ver(&self) -> u8 {
        self.0[layout::VER]
    }

    pub fn kek_ver(&self) -> u8 {
        self.0[layout::KEK_VER]
    }

    fn nonce(&self) -> XNonce {
        let bytes: [u8; layout::NONCE_SIZE] = self.0[layout::NONCE]
            .try_into()
            .expect("nonce size will match");

        XNonce::from(bytes)
    }

    fn sealed(&self) -> &[u8; layout::SEALED_SIZE] {
        self.0[layout::SEALED]
            .try_into()
            .expect("sealed size will match")
    }

    pub fn seal(keyring: &KekRing, dek: &DeviceKey, device_addr: DeviceAddr) -> Self {
        Self::seal_with_nonce(keyring, dek, device_addr, XNonce::generate())
    }

    /// The body of [`seal`](Self::seal) with the nonce supplied rather than
    /// drawn at random.
    ///
    /// Exists so the known-answer test can pin the exact output bytes; a
    /// random nonce would make the sealed blob different every run and the
    /// only assertion left would be a round-trip, which cannot detect a
    /// changed AAD or a shifted offset (seal and open would move together).
    /// Never call this with a nonce that is not freshly random.
    fn seal_with_nonce(
        keyring: &KekRing,
        dek: &DeviceKey,
        device_addr: DeviceAddr,
        nonce: XNonce,
    ) -> Self {
        let (kek_ver, kek) = keyring.get_current();
        let cipher = XChaCha20Poly1305::new(&kek.0.into());

        let aad = Self::associated_data(Self::VERSION, kek_ver, device_addr);

        let sealed = cipher
            .encrypt(
                &nonce,
                Payload {
                    msg: dek.as_bytes(),
                    aad: &aad,
                },
            )
            .expect("32-byte plaintext with 8-byte AAD is far below ChaCha20-Poly1305 limits");

        let mut key_bytes: [u8; Self::SIZE] = [0; _];
        key_bytes[layout::VER] = Self::VERSION;
        key_bytes[layout::KEK_VER] = kek_ver;
        key_bytes[layout::NONCE].copy_from_slice(&nonce);
        key_bytes[layout::SEALED].copy_from_slice(&sealed);

        Self(key_bytes)
    }

    pub fn open(&self, keyring: &KekRing, device_addr: DeviceAddr) -> Result<DeviceKey, OpenError> {
        let kek_ver = self.kek_ver();

        let kek = keyring
            .get(kek_ver)
            .ok_or(OpenError::KekNotLoaded(kek_ver))?;
        let cipher = XChaCha20Poly1305::new(&kek.0.into());

        let key = Zeroizing::new(
            cipher
                .decrypt(
                    &self.nonce(),
                    Payload {
                        msg: self.sealed(),
                        aad: &Self::associated_data(self.ver(), kek_ver, device_addr),
                    },
                )
                .map_err(|_| OpenError::Authentication)?,
        );

        Ok(DeviceKey::from_bytes(key[..].try_into().expect(
            "sealed region is a fixed size; plaintext is always DeviceKey::SIZE",
        )))
    }

    fn associated_data(ver: u8, kek_ver: u8, device_addr: DeviceAddr) -> [u8; 8] {
        let [a0, a1, a2, a3, a4, a5] = device_addr.0;
        [ver, kek_ver, a0, a1, a2, a3, a4, a5]
    }

    pub fn as_bytes(&self) -> &[u8; Self::SIZE] {
        &self.0
    }
}

impl TryFrom<&[u8]> for SealedDeviceKey {
    type Error = ParseError;

    fn try_from(value: &[u8]) -> Result<Self, Self::Error> {
        let bytes: [u8; Self::SIZE] = value
            .try_into()
            .map_err(|_| ParseError::InvalidLen { len: value.len() })?;

        let ver = bytes[layout::VER];

        if ver != Self::VERSION {
            return Err(ParseError::UnsupportedVersion(ver));
        }

        Ok(Self(bytes))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum OpenError {
    #[error("authentication failed")]
    Authentication,

    #[error("kek generation {0} is not loaded - check KEK file")]
    KekNotLoaded(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ParseError {
    #[error("invalid length, should be {expected}: {len}", expected = SealedDeviceKey::SIZE)]
    InvalidLen { len: usize },

    #[error("unsupported version: {0}")]
    UnsupportedVersion(u8),
}

#[derive(Zeroize, ZeroizeOnDrop)]
pub struct Kek([u8; 32]);

impl core::fmt::Debug for Kek {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Kek(<redacted>)")
    }
}

impl TryFrom<&str> for Kek {
    type Error = KekParseError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        if value.len() != 64 {
            return Err(KekParseError::InvalidLen(value.len()));
        }

        let mut out = [0u8; 32];

        for (byte, pair) in out.iter_mut().zip(value.as_bytes().chunks_exact(2)) {
            let pair = std::str::from_utf8(pair).map_err(|_| KekParseError::InvlidChar)?;
            *byte = u8::from_str_radix(pair, 16).map_err(|_| KekParseError::InvlidChar)?;
        }

        Ok(Self(out))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum KekParseError {
    #[error("invalid key length: {0}, should be: {len}", len = 32)]
    InvalidLen(usize),

    #[error("invalid key format, should be hexadecimal string")]
    InvlidChar,
}

pub struct KekRing {
    keys: HashMap<u8, Kek>,
    current_ver: u8,
}

impl KekRing {
    pub fn load(kek_file: &Path) -> anyhow::Result<Self> {
        let content = Zeroizing::new(
            fs::read_to_string(kek_file)
                .with_context(|| format!("couldn't load kek file: {}", kek_file.display()))?,
        );

        Self::parse(&content).with_context(|| format!("invalid kek file: {}", kek_file.display()))
    }

    /// Parses the KEK file format, split out from [`load`](Self::load) so the
    /// parser is testable without touching the filesystem.
    ///
    /// ⚠️ `content` is key material. No error raised here may echo a *value*
    /// — field names and line numbers only.
    fn parse(content: &str) -> anyhow::Result<Self> {
        let mut current_ver: Option<u8> = None;
        let mut keys: HashMap<u8, Kek> = HashMap::new();

        for (index, line) in content.lines().enumerate() {
            let line_num = index + 1;

            let line = line
                .split_once('#')
                .map_or(line, |(before, _)| before)
                .trim();
            if line.is_empty() {
                continue;
            }

            let (key, val) = line
                .split_once('=')
                .map(|(key, val)| (key.trim(), val.trim()))
                .with_context(|| format!("invalid syntax at line: {line_num}"))?;

            match key {
                "current" => {
                    if current_ver.is_some() {
                        bail!("`current` version defined more than once at line: {line_num}");
                    }

                    let version: u8 = val.parse().with_context(|| {
                        format!(
                            "invalid version: `{val}`, should be a proper number at line: {line_num}"
                        )
                    })?;

                    current_ver = Some(version);
                }

                kek_ver => {
                    let kek_ver = kek_ver.parse::<u8>().with_context(|| {
                        format!("invalid kek version `{kek_ver}` at line: {line_num}")
                    })?;

                    let kek = Kek::try_from(val)
                        .with_context(|| format!("invalid kek at line: {line_num}"))?;

                    if keys.insert(kek_ver, kek).is_some() {
                        bail!("duplicate kek generation {kek_ver} at line: {line_num}");
                    }
                }
            }
        }

        let current_ver = current_ver.context("current version not found in kek file")?;
        if !keys.contains_key(&current_ver) {
            bail!("there is no key for `current = {current_ver}`")
        }

        Ok(Self { keys, current_ver })
    }

    pub fn get(&self, kek_ver: u8) -> Option<&Kek> {
        self.keys.get(&kek_ver)
    }

    pub fn get_current(&self) -> (u8, &Kek) {
        (
            self.current_ver,
            self.keys
                .get(&self.current_ver)
                .expect("validated in KekRing::load"),
        )
    }

    /// A one-generation ring for tests elsewhere in `devices`, which need *a*
    /// ring to build a registry but do not care what is in it.
    ///
    /// `parse` is private to this module, so a sibling module cannot build one
    /// itself. This is the seam rather than widening `parse`'s visibility,
    /// because production code has exactly one legitimate source of KEKs and
    /// that is the file named by `KEK_PATH`.
    #[cfg(test)]
    pub fn for_test() -> Self {
        Self::parse(
            "current = 1\n\
             1 = 1F2E3D4C5B6A79889796A5B4C3D2E1F00F1E2D3C4B5A69788796A5B4C3D2E1F0\n",
        )
        .expect("valid kek ring")
    }
}

pub fn open_key_column(
    column: Option<Vec<u8>>,
    device_addr: DeviceAddr,
    kek_ring: &KekRing,
) -> Result<DeviceKey, KeyFault> {
    let raw_key = column.ok_or(KeyFault::Missing)?;

    let sealed = SealedDeviceKey::try_from(&raw_key[..]).map_err(KeyFault::Invalid)?;

    sealed.open(kek_ring, device_addr).map_err(|err| match err {
        OpenError::KekNotLoaded(ver) => KeyFault::KekUnavailable(ver),
        OpenError::Authentication => KeyFault::Unopenable(OpenError::Authentication),
    })
}

#[derive(Debug, Error)]
pub enum KeyFault {
    #[error("key is missing")]
    Missing,

    #[error("key has invalid format: {0}")]
    Invalid(#[from] ParseError),

    #[error("kek generation {0} is not loaded")]
    KekUnavailable(u8),

    #[error("key couldn't be opened: {0}")]
    Unopenable(OpenError),
}

#[cfg(test)]
mod test {
    use super::*;

    const ADDR: DeviceAddr = DeviceAddr([0x01, 0x02, 0x03, 0x04, 0x05, 0x06]);
    const OTHER_ADDR: DeviceAddr = DeviceAddr([0x01, 0x02, 0x03, 0x04, 0x05, 0x07]);

    const KEK_1_HEX: &str = "1F2E3D4C5B6A79889796A5B4C3D2E1F00F1E2D3C4B5A69788796A5B4C3D2E1F0";
    const KEK_2_HEX: &str = "A0B1C2D3E4F50617283940516273849506172839405162738495A6B7C8D9E0F1";

    const DEK: DeviceKey = DeviceKey::from_bytes([
        0xCE, 0x57, 0xF1, 0xC9, 0x9D, 0xA6, 0x14, 0x42, 0x14, 0x0A, 0x9F, 0x58, 0xD2, 0xC4, 0x54,
        0x7B, 0xDB, 0x68, 0x40, 0xDC, 0xCB, 0xFE, 0x41, 0x56, 0x86, 0x26, 0x3D, 0xD8, 0xAC, 0x2B,
        0x0D, 0x1B,
    ]);

    const NONCE: [u8; layout::NONCE_SIZE] = [
        0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF,
        0x00, 0x13, 0x37, 0xC0, 0xDE, 0xF0, 0x0D, 0xBE, 0xEF,
    ];

    /// A ring holding generation 1 only, with `current = 1`.
    fn ring() -> KekRing {
        KekRing::parse(&format!("current = 1\n1 = {KEK_1_HEX}\n")).expect("valid kek file")
    }

    fn seal(keyring: &KekRing) -> SealedDeviceKey {
        SealedDeviceKey::seal_with_nonce(keyring, &DEK, ADDR, XNonce::from(NONCE))
    }

    // ---- format ----------------------------------------------------------

    /// Fixed inputs, fixed output bytes.
    ///
    /// **This is the only test that can fail when the stored format changes.**
    /// Round-tripping cannot: `seal` and `open` derive the AAD from the same
    /// helper and read the same offsets, so reordering the AAD fields or
    /// shifting a slice boundary moves both halves together and every
    /// round-trip stays green — while every row already in `devices` becomes
    /// permanently unopenable.
    ///
    /// If this test fails, the question is not "how do I update the
    /// expectation" but "what happens to the rows already written".
    #[test]
    fn known_answer() {
        assert_eq!(
            seal(&ring()).0,
            [
                // ver, kek_ver
                0x01, 0x01, //
                // nonce
                0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xAA, 0xBB, 0xCC, 0xDD, 0xEE,
                0xFF, 0x00, 0x13, 0x37, 0xC0, 0xDE, 0xF0, 0x0D, 0xBE, 0xEF, //
                // ciphertext
                0x41, 0xD0, 0x2E, 0x69, 0x1A, 0x14, 0x87, 0xE0, 0xD6, 0x4D, 0x6D, 0xA5, 0x3B, 0x03,
                0x77, 0x11, 0x1A, 0x3A, 0x7A, 0x60, 0x61, 0xCC, 0xF8, 0xA0, 0x45, 0x27, 0xB0, 0x11,
                0xD0, 0xFC, 0xAD, 0xCE, //
                // tag
                0xBF, 0x32, 0x87, 0x3C, 0x30, 0xF6, 0xE3, 0x99, 0x71, 0x1E, 0xA1, 0xD6, 0x09, 0xE2,
                0xD8, 0xBD,
            ]
        );
    }

    #[test]
    fn header_is_readable_without_the_kek() {
        let sealed = seal(&ring());

        assert_eq!(sealed.ver(), SealedDeviceKey::VERSION);
        assert_eq!(sealed.kek_ver(), 1);
    }

    #[test]
    fn round_trip() {
        let ring = ring();

        assert_eq!(
            seal(&ring)
                .open(&ring, ADDR)
                .expect("open failed")
                .as_bytes(),
            DEK.as_bytes()
        );
    }

    #[test]
    fn seal_draws_a_fresh_nonce_every_time() {
        let ring = ring();

        assert_ne!(
            SealedDeviceKey::seal(&ring, &DEK, ADDR).0,
            SealedDeviceKey::seal(&ring, &DEK, ADDR).0,
            "a repeated nonce under one KEK would be catastrophic"
        );
    }

    // ---- associated data -------------------------------------------------

    /// The field that is actually load-bearing. Every row is sealed under the
    /// *same* KEK, so without `device_addr` in the AAD an attacker with write
    /// access could move one device's blob onto another's row: it would open
    /// cleanly, tag valid, and the API would silently hold the wrong key.
    #[test]
    fn aad_binds_device_addr() {
        let ring = ring();

        assert_eq!(
            seal(&ring).open(&ring, OTHER_ADDR).unwrap_err(),
            OpenError::Authentication
        );
    }

    /// `ver` is read back out of the blob to rebuild the AAD, so flipping the
    /// stored byte also flips the AAD and the tag stops matching. (Reaching
    /// `open` at all requires bypassing `TryFrom`, which rejects the version
    /// first — see `parse_rejects_unsupported_version`.)
    #[test]
    fn aad_binds_ver() {
        let ring = ring();
        let mut sealed = seal(&ring);
        sealed.0[layout::VER] = 2;

        assert_eq!(
            sealed.open(&ring, ADDR).unwrap_err(),
            OpenError::Authentication
        );
    }

    /// Flipping `kek_ver` to a generation that *is* loaded selects the wrong
    /// key, so this fails as an authentication error rather than
    /// `KekNotLoaded`.
    #[test]
    fn aad_binds_kek_ver() {
        let ring = KekRing::parse(&format!("current = 1\n1 = {KEK_1_HEX}\n2 = {KEK_2_HEX}\n"))
            .expect("valid kek file");

        let mut sealed = seal(&ring);
        sealed.0[layout::KEK_VER] = 2;

        assert_eq!(
            sealed.open(&ring, ADDR).unwrap_err(),
            OpenError::Authentication
        );
    }

    #[test]
    fn tampered_ciphertext_is_rejected() {
        let ring = ring();
        let mut sealed = seal(&ring);
        sealed.0[layout::SEALED.start] ^= 0x01;

        assert_eq!(
            sealed.open(&ring, ADDR).unwrap_err(),
            OpenError::Authentication
        );
    }

    #[test]
    fn tampered_nonce_is_rejected() {
        let ring = ring();
        let mut sealed = seal(&ring);
        sealed.0[layout::NONCE.start] ^= 0x01;

        assert_eq!(
            sealed.open(&ring, ADDR).unwrap_err(),
            OpenError::Authentication
        );
    }

    #[test]
    fn tampered_tag_is_rejected() {
        let ring = ring();
        let mut sealed = seal(&ring);
        sealed.0[SealedDeviceKey::SIZE - 1] ^= 0x01;

        assert_eq!(
            sealed.open(&ring, ADDR).unwrap_err(),
            OpenError::Authentication
        );
    }

    // ---- kek selection ---------------------------------------------------

    /// The whole reason `kek_ver` is stored: a blob sealed under a generation
    /// the running process has not loaded must say *which* generation is
    /// missing, not fail as an indistinguishable auth error.
    #[test]
    fn missing_kek_generation_is_named() {
        let sealed = seal(
            &KekRing::parse(&format!("current = 2\n2 = {KEK_2_HEX}\n")).expect("valid kek file"),
        );

        assert_eq!(
            sealed.open(&ring(), ADDR).unwrap_err(),
            OpenError::KekNotLoaded(2)
        );
    }

    /// Same generation number, different key material — the version byte is a
    /// lookup hint, never a proof. Authentication is what actually decides.
    #[test]
    fn wrong_kek_under_the_right_generation_is_rejected() {
        let impostor =
            KekRing::parse(&format!("current = 1\n1 = {KEK_2_HEX}\n")).expect("valid kek file");

        assert_eq!(
            seal(&ring()).open(&impostor, ADDR).unwrap_err(),
            OpenError::Authentication
        );
    }

    // ---- parsing a stored blob -------------------------------------------

    #[test]
    fn parse_accepts_what_seal_produced() {
        let sealed = seal(&ring());

        assert_eq!(
            SealedDeviceKey::try_from(&sealed.0[..])
                .expect("parse failed")
                .0,
            sealed.0
        );
    }

    /// The length is exact, not a minimum — a longer column value is a corrupt
    /// row, not a blob with something appended.
    #[test]
    fn parse_rejects_wrong_length() {
        let sealed = seal(&ring());
        let mut too_long = sealed.0.to_vec();
        too_long.push(0x00);

        assert_eq!(
            SealedDeviceKey::try_from(&sealed.0[..SealedDeviceKey::SIZE - 1]).unwrap_err(),
            ParseError::InvalidLen {
                len: SealedDeviceKey::SIZE - 1
            }
        );
        assert_eq!(
            SealedDeviceKey::try_from(&too_long[..]).unwrap_err(),
            ParseError::InvalidLen {
                len: SealedDeviceKey::SIZE + 1
            }
        );
        assert_eq!(
            SealedDeviceKey::try_from(&[][..]).unwrap_err(),
            ParseError::InvalidLen { len: 0 }
        );
    }

    #[test]
    fn parse_rejects_unsupported_version() {
        let mut bytes = seal(&ring()).0;
        bytes[layout::VER] = 2;

        assert_eq!(
            SealedDeviceKey::try_from(&bytes[..]).unwrap_err(),
            ParseError::UnsupportedVersion(2)
        );
    }

    // ---- the key column --------------------------------------------------

    /// `open_key_column` is the single ladder from a nullable `bytea` to a
    /// usable key, and each caller depends on a different half of it: the
    /// registry needs the `Ok` arm to build a cipher at startup, the summary
    /// endpoint needs each `Err` arm to tell an operator which of four
    /// different things to go and do. The rungs are checked in order, so a row
    /// failing an early one never reaches the later ones.
    #[test]
    fn key_column_opens_a_stored_key() {
        let ring = ring();
        let column = seal(&ring).0.to_vec();

        assert_eq!(
            open_key_column(Some(column), ADDR, &ring)
                .expect("should have opened")
                .as_bytes(),
            DEK.as_bytes()
        );
    }

    /// Phase 1 of the key migration leaves the column nullable, so an
    /// unprovisioned device is representable in the database. Distinct from a
    /// corrupt one: the remedy is to provision the device, not to go looking
    /// for what damaged the row.
    #[test]
    fn key_column_reports_a_null_column() {
        assert!(matches!(
            open_key_column(None, ADDR, &ring()),
            Err(KeyFault::Missing)
        ));
    }

    #[test]
    fn key_column_reports_a_malformed_blob() {
        assert!(matches!(
            open_key_column(Some(vec![0u8; 10]), ADDR, &ring()),
            Err(KeyFault::Invalid(ParseError::InvalidLen { len: 10 }))
        ));
    }

    /// The generation number must survive the flattening. It is the whole
    /// actionable content of this fault — it names the line missing from the
    /// KEK file — and it is what separates *restore the KEK* from *rotate the
    /// key*, where rotating would destroy the sealed DEK permanently along
    /// with every other row under the same generation.
    #[test]
    fn key_column_names_the_missing_generation() {
        let column = seal(
            &KekRing::parse(&format!("current = 2\n2 = {KEK_2_HEX}\n")).expect("valid kek file"),
        )
        .0
        .to_vec();

        assert!(matches!(
            open_key_column(Some(column), ADDR, &ring()),
            Err(KeyFault::KekUnavailable(2))
        ));
    }

    /// A blob that parses and names a loaded generation but fails its tag —
    /// here because the KEK bytes differ. Unlike the case above, this row
    /// really is unrecoverable, which is why the two must not collapse into
    /// one status.
    #[test]
    fn key_column_reports_a_failed_tag() {
        let impostor =
            KekRing::parse(&format!("current = 1\n1 = {KEK_2_HEX}\n")).expect("valid kek file");
        let column = seal(&ring()).0.to_vec();

        assert!(matches!(
            open_key_column(Some(column), ADDR, &impostor),
            Err(KeyFault::Unopenable(OpenError::Authentication))
        ));
    }

    /// The AAD binding seen from the caller that relies on it: a blob moved
    /// onto another device's row is a failed tag, not a silently wrong key.
    #[test]
    fn key_column_rejects_a_blob_from_another_row() {
        let ring = ring();
        let column = seal(&ring).0.to_vec();

        assert!(matches!(
            open_key_column(Some(column), OTHER_ADDR, &ring),
            Err(KeyFault::Unopenable(OpenError::Authentication))
        ));
    }

    /// Every fault reaches an operator as a log line, so each has to carry
    /// enough to act on. `Missing` is the only one whose name is the whole
    /// story; the other three flatten a richer error and must not lose it on
    /// the way up.
    #[test]
    fn faults_render_their_cause() {
        assert_eq!(
            KeyFault::Invalid(ParseError::InvalidLen { len: 10 }).to_string(),
            "key has invalid format: invalid length, should be 74: 10"
        );
        assert_eq!(
            KeyFault::KekUnavailable(7).to_string(),
            "kek generation 7 is not loaded"
        );
        assert_eq!(
            KeyFault::Unopenable(OpenError::Authentication).to_string(),
            "key couldn't be opened: authentication failed"
        );
    }

    // ---- kek hex ---------------------------------------------------------

    #[test]
    fn kek_hex_is_decoded_msb_first() {
        assert_eq!(
            Kek::try_from("00112233445566778899AABBCCDDEEFF00112233445566778899AABBCCDDEEFF")
                .expect("valid hex")
                .0,
            [
                0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xAA, 0xBB, 0xCC, 0xDD,
                0xEE, 0xFF, 0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xAA, 0xBB,
                0xCC, 0xDD, 0xEE, 0xFF,
            ]
        );
    }

    #[test]
    fn kek_hex_is_case_insensitive() {
        assert_eq!(
            Kek::try_from(KEK_1_HEX).expect("valid hex").0,
            Kek::try_from(&*KEK_1_HEX.to_lowercase())
                .expect("valid hex")
                .0
        );
    }

    /// A 63-character string must not decode 31 bytes and leave the last zero
    /// — `chunks_exact` silently drops the odd trailing byte, so the length
    /// check in front of it is load-bearing.
    #[test]
    fn kek_hex_rejects_wrong_length() {
        assert_eq!(
            Kek::try_from(&KEK_1_HEX[..63]).unwrap_err(),
            KekParseError::InvalidLen(63)
        );
        assert_eq!(Kek::try_from("").unwrap_err(), KekParseError::InvalidLen(0));
    }

    #[test]
    fn kek_hex_rejects_non_hex() {
        let mut bad = KEK_1_HEX.to_string();
        bad.replace_range(0..2, "ZZ");

        assert_eq!(Kek::try_from(&*bad).unwrap_err(), KekParseError::InvlidChar);
    }

    // ---- kek file --------------------------------------------------------

    #[test]
    fn loads_generations_and_current() {
        let ring = KekRing::parse(&format!("current = 2\n1 = {KEK_1_HEX}\n2 = {KEK_2_HEX}\n"))
            .expect("valid kek file");

        assert_eq!(ring.get_current().0, 2);
        assert_eq!(ring.get_current().1.0, Kek::try_from(KEK_2_HEX).unwrap().0);
        assert_eq!(
            ring.get(1).expect("generation 1 loaded").0,
            Kek::try_from(KEK_1_HEX).unwrap().0
        );
        assert!(ring.get(3).is_none());
    }

    /// `current` may name a generation that is not the highest — promoting a
    /// new KEK has to be a separate step from adding it, or a rotation could
    /// never be staged or rolled back.
    #[test]
    fn current_need_not_be_the_newest_generation() {
        let ring = KekRing::parse(&format!("current = 1\n1 = {KEK_1_HEX}\n2 = {KEK_2_HEX}\n"))
            .expect("valid kek file");

        assert_eq!(ring.get_current().0, 1);
    }

    #[test]
    fn comments_and_blank_lines_are_ignored() {
        let ring = KekRing::parse(&format!(
            "# rotated 2026-08-06\n\
             \n\
             current = 1   # promoted after the laptop rebuild\n\
             \t\n\
             1 = {KEK_1_HEX} # original\n"
        ))
        .expect("valid kek file");

        assert_eq!(ring.get_current().0, 1);
        assert_eq!(ring.get_current().1.0, Kek::try_from(KEK_1_HEX).unwrap().0);
    }

    #[test]
    fn missing_current_is_rejected() {
        assert!(KekRing::parse(&format!("1 = {KEK_1_HEX}\n")).is_err());
    }

    #[test]
    fn duplicate_current_is_rejected() {
        assert!(
            KekRing::parse(&format!(
                "current = 1\ncurrent = 2\n1 = {KEK_1_HEX}\n2 = {KEK_2_HEX}\n"
            ))
            .is_err()
        );
    }

    #[test]
    fn duplicate_generation_is_rejected() {
        assert!(
            KekRing::parse(&format!("current = 1\n1 = {KEK_1_HEX}\n1 = {KEK_2_HEX}\n")).is_err()
        );
    }

    /// Otherwise `get_current`'s `expect` would be a live panic path rather
    /// than an invariant established at load.
    #[test]
    fn current_without_a_matching_generation_is_rejected() {
        assert!(KekRing::parse(&format!("current = 2\n1 = {KEK_1_HEX}\n")).is_err());
    }

    #[test]
    fn malformed_lines_are_rejected() {
        for content in [
            "current 1\n",                     // no '='
            "current = notanumber\n",          // non-numeric version
            "current = 999\n",                 // out of u8 range
            "current = 1\nnotaversion = 00\n", // non-numeric generation
            "current = 1\n1 = nothexatall\n",  // bad key material
        ] {
            assert!(
                KekRing::parse(content).is_err(),
                "should have been rejected: {content:?}"
            );
        }
    }

    /// The file *is* the KEKs. An error that echoes a value would put key
    /// material into logs, which is where errors end up.
    #[test]
    fn errors_never_echo_key_material() {
        let err = KekRing::parse(&format!("current = 1\n1 = {KEK_1_HEX}\n1 = {KEK_2_HEX}\n"))
            .err()
            .expect("duplicate generation should be rejected");

        let rendered = format!("{err:#}");
        assert!(
            !rendered.contains(KEK_1_HEX),
            "leaked key material: {rendered}"
        );
        assert!(
            !rendered.contains(KEK_2_HEX),
            "leaked key material: {rendered}"
        );
    }

    #[test]
    fn secrets_are_redacted_in_debug() {
        assert_eq!(
            format!("{:?}", Kek::try_from(KEK_1_HEX).unwrap()),
            "Kek(<redacted>)"
        );
        assert_eq!(format!("{DEK:?}"), "DeviceKey(<redacted>)");
    }
}
