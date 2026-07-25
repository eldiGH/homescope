#![cfg_attr(not(test), no_std)]

#[cfg(feature = "wire")]
pub mod packet;

#[cfg(feature = "wire")]
pub mod observation;

#[cfg(feature = "serde")]
pub mod reading;

#[cfg(feature = "wire")]
pub mod frame;

pub mod device_addr;

pub mod measurement;
pub mod wire;
