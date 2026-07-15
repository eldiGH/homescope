#![no_std]

use embassy_nrf::{Peri, gpio::AnyPin, saadc::AnyInput};
use homescope_common::hardware_id::HardwareId;

#[cfg(all(feature = "xiao", feature = "db40"))]
compile_error!("enable exactly one board feature");
#[cfg(not(any(feature = "xiao", feature = "db40")))]
compile_error!("enable a board feature: `xiao` or `db40`");

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

#[cfg(feature = "xiao")]
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

pub fn hardware_id() -> HardwareId {
    let high = u64::from(embassy_nrf::pac::FICR.deviceid(1).read());
    let low = u64::from(embassy_nrf::pac::FICR.deviceid(0).read());

    HardwareId(high << 32 | low)
}

pub fn mac_addr() -> [u8; 6] {
    let [b4, b5, _, _] = embassy_nrf::pac::FICR.deviceaddr(1).read().to_le_bytes();
    let [b0, b1, b2, b3] = embassy_nrf::pac::FICR.deviceaddr(0).read().to_le_bytes();

    [b0, b1, b2, b3, b4, b5 | 0xC0]
}
