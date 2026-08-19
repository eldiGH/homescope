use std::{
    env::VarError::{NotPresent, NotUnicode},
    str::FromStr,
};

use anyhow::{Context as _, bail};

pub fn env_var_or<T: FromStr>(key: &str, default: T) -> anyhow::Result<T>
where
    T::Err: std::error::Error + Send + Sync + 'static,
{
    match std::env::var(key) {
        Ok(val) => val
            .parse()
            .with_context(|| format!("{key} has invalid value")),
        Err(NotPresent) => Ok(default),
        Err(NotUnicode(_)) => bail!("{key} is not valid unicode"),
    }
}

pub fn env_var<T: FromStr>(key: &str) -> anyhow::Result<T>
where
    T::Err: std::error::Error + Send + Sync + 'static,
{
    match std::env::var(key) {
        Ok(val) => val
            .parse()
            .with_context(|| format!("{key} has invalid value")),
        Err(NotPresent) => bail!("{key} not provided"),
        Err(NotUnicode(_)) => bail!("{key} is not valid unicode"),
    }
}
