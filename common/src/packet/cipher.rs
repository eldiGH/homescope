//! Authenticated encryption for the air packet body.
//!
//! ChaCha20-Poly1305 with a **per-device 32-byte key**. Only the body is
//! sealed; `ver` and `seq` stay cleartext because both have to be readable
//! before the body can be interpreted — `ver` selects the decoder, `seq` is
//! what the receiver dedups on and what this module derives the nonce from.
//! Cleartext is not the same as untrusted: both are bound in as associated
//! data, so altering either invalidates the tag.
//!
//! ```text
//! on air:  | magic "HP" | ver | seq (4 LE) | ciphertext … | tag (16) |
//!          \___ stripped ___/  \__ AAD, cleartext __/  \__ sealed __/
//! ```
//!
//! The air magic is deliberately **not** authenticated: it is a constant, the
//! receiver strips it before anything downstream sees the packet, and an
//! attacker cannot vary it to any effect (a packet without it never reaches a
//! cipher). Authenticating it would only oblige the API to re-materialise
//! bytes it never receives.
//!
//! Encryption happens on the sensor; decryption happens in the API. Nothing in
//! between holds a key — see `NOTES-packet-tv-aead.md` for why gateways stay
//! keyless.

use super::AEAD_TAG_SIZE;
use chacha20poly1305::{AeadInOut, KeyInit, Nonce};
use thiserror::Error;

use crate::{device_addr::DeviceAddr, device_key::DeviceKey};

const _: () = assert!(
    AEAD_TAG_SIZE == size_of::<chacha20poly1305::Tag>(),
    "Poly1305 tag length"
);

pub struct PacketCipher {
    cipher: chacha20poly1305::ChaCha20Poly1305,
    device_addr: DeviceAddr,
}

impl PacketCipher {
    const DEVICE_ADDR_SIZE: usize = DeviceAddr::SIZE;
    const SEQ_SIZE: usize = size_of::<u32>();
    const VER_SIZE: usize = size_of::<u8>();

    const ASSOCIATED_DATA_SIZE: usize = Self::DEVICE_ADDR_SIZE + Self::SEQ_SIZE + Self::VER_SIZE;

    pub fn new(key: DeviceKey, device_addr: DeviceAddr) -> Self {
        Self {
            device_addr,
            cipher: chacha20poly1305::ChaChaPoly1305::new(key.as_bytes().into()),
        }
    }

    pub fn encrypt_in_place(
        &self,
        ver: u8,
        seq: u32,
        data: &mut [u8],
        tag_out: &mut [u8; AEAD_TAG_SIZE],
    ) -> Result<(), EncryptionError> {
        let associated_data = self.associated_data(seq, ver);
        let nonce = Self::nonce(seq);

        let tag = self
            .cipher
            .encrypt_inout_detached(&nonce, &associated_data, data.into())
            .map_err(|_| EncryptionError::AdOrPlaintextTooLarge)?;

        tag_out.copy_from_slice(&tag);

        Ok(())
    }

    pub fn decrypt_in_place<'a>(
        &self,
        ver: u8,
        seq: u32,
        buf: &'a mut [u8],
    ) -> Result<&'a [u8], DecryptionError> {
        let associated_data = self.associated_data(seq, ver);
        let nonce = Self::nonce(seq);

        let (data, tag) = buf
            .split_last_chunk_mut::<AEAD_TAG_SIZE>()
            .ok_or(DecryptionError::MissingTag)?;
        let tag: &[u8; AEAD_TAG_SIZE] = tag;

        self.cipher
            .decrypt_inout_detached(&nonce, &associated_data, data.into(), tag.into())
            .map_err(|_| DecryptionError::Authentication)?;

        Ok(data)
    }

    /// The cleartext context the ciphertext is bound to:
    ///
    /// ```text
    /// | device_addr: 6 | ver: 1 | seq: 4 LE |
    /// ```
    ///
    /// **This layout is a wire contract.** The sensor and the API build it
    /// independently and must produce identical bytes — same fields, same
    /// order, same endianness. A mismatch is indistinguishable from a wrong
    /// key: every packet fails with [`DecryptionError::Authentication`] and
    /// nothing points at the cause. Change it only alongside a `ver` bump, and
    /// keep the known-answer test in this module as the tripwire.
    ///
    /// Each field earns its place by binding one thing an attacker could
    /// otherwise swap while leaving the ciphertext intact:
    ///
    /// - `device_addr` — replaying one device's packet as another's
    /// - `ver` — feeding an authentic body to a different version's decoder
    /// - `seq` — re-labelling a stale reading as a fresh one
    fn associated_data(&self, seq: u32, ver: u8) -> [u8; Self::ASSOCIATED_DATA_SIZE] {
        let [a0, a1, a2, a3, a4, a5] = self.device_addr.0;
        let [s0, s1, s2, s3] = seq.to_le_bytes();

        [a0, a1, a2, a3, a4, a5, ver, s0, s1, s2, s3]
    }

    /// `seq` in the low four bytes, the remaining eight fixed at zero.
    ///
    /// # Safety of a counter-derived nonce
    ///
    /// ChaCha20-Poly1305 nonces must **never repeat under one key**; reuse
    /// costs confidentiality *and* unforgeability at once, retroactively, for
    /// every packet sharing the nonce. No random component is needed here, but
    /// only because two invariants hold together:
    ///
    /// 1. **Keys are per-device** (`PacketCipher` binds one key to one
    ///    `DeviceAddr`), so two devices at the same `seq` are in different
    ///    keyspaces and do not collide.
    /// 2. **`seq` never repeats or rewinds for a given device.** That is not a
    ///    property of this module — it is enforced by the sensor's flash
    ///    checkpoint with jump-ahead in `firmware/sensor/src/seq_counter.rs`.
    ///
    /// Invariant 2 is the fragile one: a counter that resets to zero on reboot
    /// would reuse every nonce it had already used. **Do not weaken
    /// `seq_counter.rs` without re-deriving this.**
    fn nonce(seq: u32) -> Nonce {
        let mut nonce_buf: [u8; 12] = [0; 12];
        nonce_buf[..size_of::<u32>()].copy_from_slice(&seq.to_le_bytes());

        Nonce::from(nonce_buf)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum EncryptionError {
    #[error("associated data or plaintext too large")]
    AdOrPlaintextTooLarge,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum DecryptionError {
    #[error("data slice too small")]
    MissingTag,

    #[error("authentication failed")]
    Authentication,
}

#[cfg(test)]
mod test {
    use std::{vec, vec::Vec};

    use super::*;
    use crate::packet::VERSION;

    const ADDR: DeviceAddr = DeviceAddr([0x01, 0x02, 0x03, 0x04, 0x05, 0x06]);
    const SEQ: u32 = 21;
    const PLAINTEXT: [u8; 8] = [0x03, 0x9C, 0x24, 0x10, 0xB8, 0xC6, 0x54, 0x38];

    const KEY: DeviceKey = DeviceKey::from_bytes([
        0xCE, 0x57, 0xF1, 0xC9, 0x9D, 0xA6, 0x14, 0x42, 0x14, 0x0A, 0x9F, 0x58, 0xD2, 0xC4, 0x54,
        0x7B, 0xDB, 0x68, 0x40, 0xDC, 0xCB, 0xFE, 0x41, 0x56, 0x86, 0x26, 0x3D, 0xD8, 0xAC, 0x2B,
        0x0D, 0x1B,
    ]);

    fn cipher() -> PacketCipher {
        PacketCipher::new(KEY, ADDR)
    }

    /// Seals `plaintext` the way the sensor does and returns the body the API
    /// would see: `ciphertext || tag`.
    fn sealed(cipher: &PacketCipher, ver: u8, seq: u32, plaintext: &[u8]) -> Vec<u8> {
        let mut body = plaintext.to_vec();
        body.resize(plaintext.len() + AEAD_TAG_SIZE, 0);

        let (data, tag) = body.split_at_mut(plaintext.len());
        let tag: &mut [u8; AEAD_TAG_SIZE] = tag.try_into().expect("tag region is exactly one tag");

        cipher
            .encrypt_in_place(ver, seq, data, tag)
            .expect("sealing failed");

        body
    }

    #[test]
    fn round_trip() {
        let cipher = cipher();
        let mut body = sealed(&cipher, VERSION, SEQ, &PLAINTEXT);

        assert_ne!(
            body[..PLAINTEXT.len()],
            PLAINTEXT,
            "body must not still be readable"
        );
        assert_eq!(body.len(), PLAINTEXT.len() + AEAD_TAG_SIZE);

        assert_eq!(
            cipher.decrypt_in_place(VERSION, SEQ, &mut body),
            Ok(&PLAINTEXT[..])
        );
    }

    /// A packet whose every measurement read failed still has to survive the
    /// crypto layer — rejecting it is `v1`'s job, not this module's.
    #[test]
    fn empty_plaintext_round_trips() {
        let cipher = cipher();
        let mut body = sealed(&cipher, VERSION, SEQ, &[]);

        assert_eq!(body.len(), AEAD_TAG_SIZE);
        assert_eq!(
            cipher.decrypt_in_place(VERSION, SEQ, &mut body),
            Ok(&[][..])
        );
    }

    // --- integrity -------------------------------------------------------

    #[test]
    fn tampered_ciphertext_is_rejected() {
        let cipher = cipher();
        let mut body = sealed(&cipher, VERSION, SEQ, &PLAINTEXT);
        body[0] ^= 0x01;

        assert_eq!(
            cipher.decrypt_in_place(VERSION, SEQ, &mut body),
            Err(DecryptionError::Authentication)
        );
    }

    #[test]
    fn tampered_tag_is_rejected() {
        let cipher = cipher();
        let mut body = sealed(&cipher, VERSION, SEQ, &PLAINTEXT);
        *body.last_mut().unwrap() ^= 0x01;

        assert_eq!(
            cipher.decrypt_in_place(VERSION, SEQ, &mut body),
            Err(DecryptionError::Authentication)
        );
    }

    /// `decrypt_inout_detached` verifies the MAC *before* applying the
    /// keystream, so a rejected packet never leaks plaintext into the caller's
    /// buffer. Pinned because the whole "reject vs store" posture downstream
    /// assumes a failed open yields nothing usable.
    #[test]
    fn failed_open_does_not_release_plaintext() {
        let cipher = cipher();
        let mut body = sealed(&cipher, VERSION, SEQ, &PLAINTEXT);
        let untouched = body.clone();

        body[0] ^= 0x01;
        let expected = body.clone();

        assert!(cipher.decrypt_in_place(VERSION, SEQ, &mut body).is_err());

        assert_eq!(body, expected, "buffer must be left as it was found");
        assert_ne!(
            body[..PLAINTEXT.len()],
            untouched[..PLAINTEXT.len()],
            "sanity: the tampered byte really did change",
        );
    }

    // --- associated data -------------------------------------------------
    //
    // One test per AAD field. Each one is the executable proof that the field
    // is actually wired into `associated_data` — drop a field and exactly one
    // of these goes red.

    /// The reason `ver` is in the AAD. With one version shipped nothing else
    /// in the suite would notice if it fell out, and the day a v2 body layout
    /// lands, an unbound `ver` lets an attacker re-label an authentic v1
    /// packet and have the v2 decoder read fabricated values out of it.
    #[test]
    fn wrong_ver_is_rejected() {
        let cipher = cipher();
        let mut body = sealed(&cipher, VERSION, SEQ, &PLAINTEXT);

        assert_eq!(
            cipher.decrypt_in_place(VERSION + 1, SEQ, &mut body),
            Err(DecryptionError::Authentication)
        );
    }

    /// Binds a reading to its position in the sequence: a captured packet
    /// cannot be replayed under a fresh `seq` to look like a new sample.
    #[test]
    fn wrong_seq_is_rejected() {
        let cipher = cipher();
        let mut body = sealed(&cipher, VERSION, SEQ, &PLAINTEXT);

        assert_eq!(
            cipher.decrypt_in_place(VERSION, SEQ + 1, &mut body),
            Err(DecryptionError::Authentication)
        );
    }

    /// Binds a reading to its device: one node's ciphertext cannot be grafted
    /// onto another node's envelope, even though both are cleartext-addressed.
    #[test]
    fn wrong_device_addr_is_rejected() {
        let mut body = sealed(&cipher(), VERSION, SEQ, &PLAINTEXT);

        let impostor = PacketCipher::new(KEY, DeviceAddr([0x01, 0x02, 0x03, 0x04, 0x05, 0x07]));

        assert_eq!(
            impostor.decrypt_in_place(VERSION, SEQ, &mut body),
            Err(DecryptionError::Authentication)
        );
    }

    #[test]
    fn wrong_key_is_rejected() {
        let mut body = sealed(&cipher(), VERSION, SEQ, &PLAINTEXT);

        let mut other_key = *KEY.as_bytes();
        other_key[0] ^= 0x01;

        assert_eq!(
            PacketCipher::new(DeviceKey::from_bytes(other_key), ADDR)
                .decrypt_in_place(VERSION, SEQ, &mut body),
            Err(DecryptionError::Authentication)
        );
    }

    // --- framing ---------------------------------------------------------

    /// A body too short to even contain a tag is a named rejection, not a
    /// panic — this length arrives off MQTT and is attacker-influenced.
    #[test]
    fn body_shorter_than_a_tag_is_rejected() {
        let cipher = cipher();

        for len in 0..AEAD_TAG_SIZE {
            let mut body = vec![0u8; len];

            assert_eq!(
                cipher.decrypt_in_place(VERSION, SEQ, &mut body),
                Err(DecryptionError::MissingTag),
                "a {len}-byte body cannot hold a tag"
            );
        }
    }

    // --- known-answer ----------------------------------------------------

    /// Pins the entire construction — key schedule, AAD field order and
    /// endianness, and nonce derivation — against fixed inputs.
    ///
    /// Round-trip tests structurally cannot catch a change here: seal and open
    /// share `associated_data` and `nonce`, so reordering the AAD or flipping
    /// the nonce's byte order leaves every round trip green while breaking
    /// every device already in the field. This test is the only thing standing
    /// between that edit and a silent fleet-wide outage.
    ///
    /// **If this fails, you changed the wire format.** Bump `ver` and mint a
    /// new expectation; do not regenerate this array to make it pass.
    #[test]
    fn known_answer() {
        const EXPECTED: [u8; PLAINTEXT.len() + AEAD_TAG_SIZE] = [
            0xB8, 0x8A, 0x00, 0xDA, 0xEE, 0xBE, 0xD8, 0xBA, // ciphertext
            0xD7, 0xD8, 0x13, 0x88, 0x3D, 0x8B, 0x8B, 0x46, // tag
            0x27, 0x2E, 0x78, 0x4E, 0xC2, 0x83, 0x33, 0x5F,
        ];

        assert_eq!(
            sealed(&cipher(), VERSION, SEQ, &PLAINTEXT)[..],
            EXPECTED[..]
        );
    }
}
