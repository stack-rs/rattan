//! serde for ByteSize
use bytesize::ByteSize;
use serde::de::Error;
use serde::{Deserialize, Deserializer, Serializer};

pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<ByteSize>, D::Error>
where
    D: Deserializer<'de>,
{
    let s: Option<String> = Option::deserialize(deserializer)?;
    match s {
        Some(s) => s.parse::<ByteSize>().map(Some).map_err(D::Error::custom),
        None => Ok(None),
    }
}

pub fn serialize<S>(value: &Option<ByteSize>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    match value {
        Some(value) => serializer.serialize_str(&value.to_string()),
        None => serializer.serialize_none(),
    }
}
