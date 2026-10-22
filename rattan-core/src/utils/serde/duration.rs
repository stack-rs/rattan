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
    serializer.serialize_str(&format!("{dur:#}"))
}

pub mod option {
    use super::*;
    use serde::{Deserializer, Serializer};

    pub fn serialize<S>(opt: &Option<Duration>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match opt {
            Some(d) => super::serialize(d, serializer),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<Duration>, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct OptionVisitor;
        impl<'de> serde::de::Visitor<'de> for OptionVisitor {
            type Value = Option<Duration>;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a duration or null")
            }
            fn visit_none<E>(self) -> Result<Self::Value, E> {
                Ok(None)
            }
            fn visit_some<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
            where
                D: Deserializer<'de>,
            {
                super::deserialize(deserializer).map(Some)
            }
        }
        deserializer.deserialize_option(OptionVisitor)
    }
}
