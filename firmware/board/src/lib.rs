#![no_std]

pub mod chip;

use embassy_nrf::{Peri, gpio::AnyPin, saadc::AnyInput};

// One feature per *wiring*, not per MCU. Since the bootloader was dropped every
// board shares one flash layout, so a board feature now selects pins and the
// battery divider and nothing else — which makes the two XIAO harnesses two
// boards as far as this macro is concerned. They are the same silicon with the
// SHT45 on different pins, and that difference used to live as an uncommitted
// diff somebody had to remember to flip.
#[cfg(not(any(
    feature = "db40",
    feature = "xiao-expansion",
    feature = "xiao-breadboard"
)))]
compile_error!("enable exactly one board feature: `db40`, `xiao-expansion` or `xiao-breadboard`");
#[cfg(any(
    all(feature = "db40", feature = "xiao-expansion"),
    all(feature = "db40", feature = "xiao-breadboard"),
    all(feature = "xiao-expansion", feature = "xiao-breadboard"),
))]
compile_error!(
    "enable exactly one board feature; Cargo features are additive, so pass \
     `--no-default-features` alongside the one you want"
);

pub struct Board {
    pub led: Peri<'static, AnyPin>,
    pub i2c_sda: Peri<'static, AnyPin>,
    pub i2c_scl: Peri<'static, AnyPin>,
    pub sensors_power: Option<Peri<'static, AnyPin>>,
    pub battery_adc: AnyInput<'static>,
    pub battery_divider_ratio: u32,
}

#[cfg(feature = "db40")]
#[macro_export]
macro_rules! board {
    ($p:ident) => {
        $crate::Board {
            led: $p.P0_13.into(),
            i2c_sda: $p.P0_26.into(),
            i2c_scl: $p.P0_27.into(),
            sensors_power: ::core::option::Option::Some($p.P0_05.into()),
            battery_adc: ::embassy_nrf::saadc::Input::degrade_saadc(::embassy_nrf::saadc::VddInput),
            battery_divider_ratio: 1,
        }
    };
}

/// XIAO on the Seeed expansion board — the development setup.
///
/// The SHT45 sits on the expansion board's Grove I²C header and is permanently
/// powered, so there is no rail to gate.
#[cfg(feature = "xiao-expansion")]
#[macro_export]
macro_rules! board {
    ($p:ident) => {
        $crate::Board {
            led: $p.P0_30.into(),
            i2c_sda: $p.P0_04.into(),
            i2c_scl: $p.P0_05.into(),
            sensors_power: ::core::option::Option::None,
            battery_adc: ::embassy_nrf::saadc::Input::degrade_saadc(::embassy_nrf::saadc::VddInput),
            battery_divider_ratio: 1,
        }
    };
}

/// XIAO wired on a breadboard — the alkaline soak-test node.
///
/// The SHT45 is direct-wired with its own power line, so the rail *is* gated:
/// the point of the soak test is battery longevity, and a sensor left powered
/// between the 60 s cycles would distort exactly what is being measured.
#[cfg(feature = "xiao-breadboard")]
#[macro_export]
macro_rules! board {
    ($p:ident) => {
        $crate::Board {
            led: $p.P0_30.into(),
            i2c_sda: $p.P1_14.into(),
            i2c_scl: $p.P1_13.into(),
            sensors_power: ::core::option::Option::Some($p.P1_15.into()),
            battery_adc: ::embassy_nrf::saadc::Input::degrade_saadc(::embassy_nrf::saadc::VddInput),
            battery_divider_ratio: 1,
        }
    };
}
