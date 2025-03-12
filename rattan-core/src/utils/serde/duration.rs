//! serde for duration and delay
use std::fmt;
use std::time::Duration;

use serde::{de, ser, Deserializer, Serializer};

pub fn deserialize<'de, D>(deserializer: D) -> Result<Duration, D::Error>
where
    D: Deserializer<'de>,
{
    struct V;

    impl de::Visitor<'_> for V {
        type Value = Duration;

        fn expecting(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
            fmt.write_str("a duration")
        }

        fn visit_str<E>(self, v: &str) -> Result<Duration, E>
        where
            E: de::Error,
        {
            let dur: jiff::Span = v.parse().map_err(E::custom)?;
            Duration::try_from(dur).map_err(E::custom)
        }
    }

    deserializer.deserialize_str(V)
}

pub fn serialize<S>(value: &Duration, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    let dur = jiff::Span::try_from(*value).map_err(ser::Error::custom)?;
    serializer.serialize_str(&format!("{:#}", dur))
}
