use alloc::string::String;

/// A site's identifier: `odin`, `thor`, `home`.
///
/// One string used in four places — a gateway's `SITE`, the topic segment in
/// `homescope/<site>/sensors/…`, the broker user `homescope-<site>` and the
/// `sites.name` column — so all four share one grammar: a single MQTT topic
/// level made of `[a-z0-9-]`. The database enforces the same rule with a
/// `CHECK` (migration 20261005101457); this type is the Rust side of it.
///
/// Narrower than MQTT requires on purpose. MQTT would allow almost anything
/// but `/`, `+` and `#`, yet the name also ends up in a broker username, an ACL
/// pattern and a Grafana variable, and uppercase would make `Thor` and `thor`
/// two different sites to the broker but one to a human.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Site(String);

impl Site {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl core::fmt::Display for Site {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SiteParseError {
    Empty,
    InvalidCharacter,
}

impl core::fmt::Display for SiteParseError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Empty => f.write_str("site name is empty"),
            Self::InvalidCharacter => f.write_str("site name may only contain a-z, 0-9 and '-'"),
        }
    }
}

impl core::error::Error for SiteParseError {}

impl core::str::FromStr for Site {
    type Err = SiteParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.is_empty() {
            return Err(SiteParseError::Empty);
        }

        let allowed = |b: u8| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-';
        if !s.bytes().all(allowed) {
            return Err(SiteParseError::InvalidCharacter);
        }

        Ok(Self(s.into()))
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn accepts_the_topic_level_grammar() {
        for name in ["odin", "thor", "house-2", "0", "-"] {
            let site: Site = name.parse().expect(name);

            assert_eq!(site.as_str(), name);
        }
    }

    #[test]
    fn rejects_an_empty_name() {
        assert_eq!("".parse::<Site>(), Err(SiteParseError::Empty));
    }

    /// The MQTT-special characters first — each would turn one topic level
    /// into several, or into a wildcard — then what the grammar narrows away.
    #[test]
    fn rejects_everything_outside_the_grammar() {
        for name in [
            "a/b", "+", "#", "Thor", "a b", "a_b", "a.b", "ódin", " odin", "odin\n",
        ] {
            assert_eq!(
                name.parse::<Site>(),
                Err(SiteParseError::InvalidCharacter),
                "{name:?}"
            );
        }
    }
}
