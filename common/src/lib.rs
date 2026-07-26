//! Wire formats shared by the Homescope firmware, gateway and API.
//!
//! `no_std` and allocation-free by default. Every dependency is optional and
//! gated behind a feature, so each consumer compiles only the layers it uses.
//!
//! # Features
//!
//! Nothing is enabled by default — consumers opt in:
//!
//! - **`codec`** — the binary protocol: `packet` (the air payload),
//!   `observation` (what the receiver saw) and `frame` (USB-CDC framing and
//!   CRC). Pulls `crc` and `memchr`. Sensor, receiver and gateway need it; a
//!   consumer that only speaks JSON does not.
//! - **`serde`** — `Serialize`/`Deserialize` for the shared types, plus the
//!   `reading` and `observation_envelope` DTOs. Pulls `serde`, `chrono` and
//!   `base64`, and turns on `alloc`. Host-side in practice, but it still
//!   builds for `thumbv7em-none-eabi`.
//! - **`defmt`** — `defmt::Format` for the shared types, so firmware can log
//!   them over RTT. Pulls `defmt`.
//!
//! The features compose: with `codec` and `serde` both on,
//! `ObservationEnvelope::from_observation` turns a decoded observation into
//! the MQTT payload. That combination is the gateway's configuration.
//!
//! Note that `codec` does **not** gate the [`wire`] module — [`wire::Wire`]
//! and the unit newtypes are always available, being the primitives the rest
//! is built from.

#![no_std]

#[cfg(feature = "serde")]
extern crate alloc;

#[cfg(test)]
extern crate std;

#[cfg(feature = "codec")]
pub mod packet;

#[cfg(feature = "codec")]
pub mod observation;

#[cfg(feature = "serde")]
pub mod reading;

#[cfg(feature = "codec")]
pub mod frame;

pub mod device_addr;

pub mod measurement;
pub mod wire;

#[cfg(feature = "serde")]
pub mod observation_envelope;
