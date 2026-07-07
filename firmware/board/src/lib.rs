#![no_std]

#[cfg(all(feature = "xiao", feature = "db40"))]
compile_error!("enable exactly one board feature");
#[cfg(not(any(feature = "xiao", feature = "db40")))]
compile_error!("enable a board feature: `xiao` or `db40`");

#[cfg(feature = "db40")]
#[macro_export]
macro_rules! led_pin {
    ($p:ident) => {
        $p.P0_13
    };
}

#[cfg(feature = "xiao")]
#[macro_export]
macro_rules! led_pin {
    ($p:ident) => {
        $p.P0_30
    };
}

#[cfg(feature = "db40")]
#[macro_export]
macro_rules! i2c_sda_pin {
    ($p:ident) => {
        $p.P0_26
    };
}

#[cfg(feature = "db40")]
#[macro_export]
macro_rules! i2c_scl_pin {
    ($p:ident) => {
        $p.P0_27
    };
}
