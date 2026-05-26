#![no_std]

#[cfg(feature = "wire")]
pub mod packet;

#[cfg(feature = "wire")]
pub mod observation;

#[cfg(feature = "serde")]
pub mod reading;

#[cfg(feature = "wire")]
pub mod frame;
