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

    pub fn seal(keyring: &KekRing, dek: DeviceKey, device_addr: DeviceAddr) -> Self {
        let (kek_ver, kek) = keyring.get_current();
        let cipher = XChaCha20Poly1305::new(&kek.0.into());

        let nonce = XNonce::generate();
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
}
