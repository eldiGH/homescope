//! The MQTT topic an [`ObservationEnvelope`] travels on:
//!
//! ```text
//! homescope/<site>/sensors/<device-addr>/envelope
//! ```
//!
//! The gateway builds it through [`Display`](core::fmt::Display) and the API
//! parses it through [`FromStr`](core::str::FromStr) — one definition for both
//! ends, the way `frame::encode` and `frame::parse` share theirs. The gateway
//! and the API ship separately, so the golden test below is what actually pins
//! the format.
//!
//! The site is the *publishing gateway's*, not the device's: transport
//! provenance. Under per-site broker ACLs it is the one piece of site
//! information the broker vouches for, which is why it travels in the topic
//! rather than in the envelope — see docs/design/site-room-topology.md.
//!
//! [`ObservationEnvelope`]: crate::observation_envelope::ObservationEnvelope

use crate::{
    device_addr::{DeviceAddr, DeviceAddrParseError},
    site::{Site, SiteParseError},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvelopeTopic {
    pub site: Site,
    pub device_addr: DeviceAddr,
}

impl EnvelopeTopic {
    /// Every site, every device — what the API subscribes to. The test
    /// `subscription_matches_built_topics` keeps it in step with `Display`.
    pub const SUBSCRIPTION: &str = "homescope/+/sensors/+/envelope";
}

impl core::fmt::Display for EnvelopeTopic {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "homescope/{}/sensors/{}/envelope",
            self.site, self.device_addr
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvelopeTopicParseError {
    /// Not five levels of `homescope/_/sensors/_/envelope` — including the
    /// pre-site `homescope/sensors/<device-addr>/envelope`.
    Shape,
    Site(SiteParseError),
    DeviceAddr(DeviceAddrParseError),
}

impl core::fmt::Display for EnvelopeTopicParseError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Shape => f.write_str("expected homescope/<site>/sensors/<device-addr>/envelope"),
            Self::Site(err) => write!(f, "bad site level: {err}"),
            Self::DeviceAddr(err) => write!(f, "bad device address level: {err}"),
        }
    }
}

impl core::error::Error for EnvelopeTopicParseError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Shape => None,
            Self::Site(err) => Some(err),
            Self::DeviceAddr(err) => Some(err),
        }
    }
}

impl core::str::FromStr for EnvelopeTopic {
    type Err = EnvelopeTopicParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // Six pulls for five levels: the sixth must be `None`, which is what
        // rejects a topic with an extra level.
        let mut levels = s.split('/');
        let (Some("homescope"), Some(site), Some("sensors"), Some(addr), Some("envelope"), None) = (
            levels.next(),
            levels.next(),
            levels.next(),
            levels.next(),
            levels.next(),
            levels.next(),
        ) else {
            return Err(EnvelopeTopicParseError::Shape);
        };

        Ok(Self {
            site: site.parse().map_err(EnvelopeTopicParseError::Site)?,
            device_addr: addr.parse().map_err(EnvelopeTopicParseError::DeviceAddr)?,
        })
    }
}

#[cfg(test)]
mod test {
    use alloc::string::ToString as _;

    use super::*;

    const GOLDEN: &str = "homescope/odin/sensors/CEA99627BD3F/envelope";

    fn odin_topic() -> EnvelopeTopic {
        EnvelopeTopic {
            site: "odin".parse().expect("valid site"),
            device_addr: "CEA99627BD3F".parse().expect("valid address"),
        }
    }

    /// The wire format, stated as a literal. If this fails after a deliberate
    /// change, every deployed gateway and API disagree until both are updated —
    /// and the broker ACLs name the old shape too.
    #[test]
    fn builds_the_golden_topic() {
        assert_eq!(odin_topic().to_string(), GOLDEN);
    }

    #[test]
    fn parses_the_golden_topic() {
        assert_eq!(GOLDEN.parse::<EnvelopeTopic>(), Ok(odin_topic()));
    }

    /// The gateway always renders the address in uppercase; the parser takes
    /// either case, the way `DeviceAddr::from_str` does everywhere else.
    #[test]
    fn parses_a_lowercase_address() {
        let topic: EnvelopeTopic = "homescope/odin/sensors/cea99627bd3f/envelope"
            .parse()
            .expect("valid topic");

        assert_eq!(topic, odin_topic());
    }

    #[test]
    fn rejects_the_wrong_shape() {
        for topic in [
            // the pre-site topic the API subscribed to until 2026-10
            "homescope/sensors/CEA99627BD3F/envelope",
            "homescope/odin/sensors/CEA99627BD3F/envelope/extra",
            "homescope/odin/sensors/CEA99627BD3F/envelope/",
            "homescope/odin/sensors/CEA99627BD3F",
            "other/odin/sensors/CEA99627BD3F/envelope",
            "homescope/odin/devices/CEA99627BD3F/envelope",
            "homescope/odin/sensors/CEA99627BD3F/state",
            "homescope/odin/bridge/state",
            "",
        ] {
            assert_eq!(
                topic.parse::<EnvelopeTopic>(),
                Err(EnvelopeTopicParseError::Shape),
                "{topic:?}"
            );
        }
    }

    #[test]
    fn rejects_a_bad_site_level() {
        assert_eq!(
            "homescope/Odin/sensors/CEA99627BD3F/envelope".parse::<EnvelopeTopic>(),
            Err(EnvelopeTopicParseError::Site(
                SiteParseError::InvalidCharacter
            ))
        );
        assert_eq!(
            "homescope//sensors/CEA99627BD3F/envelope".parse::<EnvelopeTopic>(),
            Err(EnvelopeTopicParseError::Site(SiteParseError::Empty))
        );
    }

    #[test]
    fn rejects_a_bad_address_level() {
        assert_eq!(
            "homescope/odin/sensors/CEA99627BD3/envelope".parse::<EnvelopeTopic>(),
            Err(EnvelopeTopicParseError::DeviceAddr(
                DeviceAddrParseError::BadFormat
            ))
        );
        assert_eq!(
            "homescope/odin/sensors/CEA99627BD3G/envelope".parse::<EnvelopeTopic>(),
            Err(EnvelopeTopicParseError::DeviceAddr(
                DeviceAddrParseError::NotHex
            ))
        );
    }

    /// MQTT's single-level wildcard, enough of it to check the subscription.
    fn matches(filter: &str, topic: &str) -> bool {
        let (mut filter, mut topic) = (filter.split('/'), topic.split('/'));
        loop {
            match (filter.next(), topic.next()) {
                (None, None) => return true,
                (Some("+"), Some(_)) => {}
                (Some(f), Some(t)) if f == t => {}
                _ => return false,
            }
        }
    }

    /// A subscription that drifted from the built topic would receive nothing,
    /// with no error anywhere — the broker simply has no match to deliver.
    #[test]
    fn subscription_matches_built_topics() {
        assert!(matches(EnvelopeTopic::SUBSCRIPTION, GOLDEN));
        assert!(!matches(
            EnvelopeTopic::SUBSCRIPTION,
            "homescope/sensors/CEA99627BD3F/envelope"
        ));
    }
}
