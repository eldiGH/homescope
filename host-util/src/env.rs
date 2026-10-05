use std::{
    env::VarError::{NotPresent, NotUnicode},
    str::FromStr,
};

use anyhow::{Context as _, bail};

/// `None` when the variable is unset. A variable that is set but does not
/// parse is an error, never a silent `None`.
pub fn env_var_opt<T: FromStr>(key: &str) -> anyhow::Result<Option<T>>
where
    T::Err: std::error::Error + Send + Sync + 'static,
{
    match std::env::var(key) {
        Ok(val) => val
            .parse()
            .map(Some)
            .with_context(|| format!("{key} has invalid value")),
        Err(NotPresent) => Ok(None),
        Err(NotUnicode(_)) => bail!("{key} is not valid unicode"),
    }
}

pub fn env_var_or<T: FromStr>(key: &str, default: T) -> anyhow::Result<T>
where
    T::Err: std::error::Error + Send + Sync + 'static,
{
    Ok(env_var_opt(key)?.unwrap_or(default))
}

pub fn env_var<T: FromStr>(key: &str) -> anyhow::Result<T>
where
    T::Err: std::error::Error + Send + Sync + 'static,
{
    env_var_opt(key)?.with_context(|| format!("{key} not provided"))
}
