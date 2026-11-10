//! Strict JSON parsing for managed-execution inputs.
//!
//! `serde_json::Value` keeps the *last* duplicate object key, so a policy or a
//! request could carry two `ops` arrays and leave the reader believing the
//! first is in force. Both the policy specification and the session protocol
//! require rejection, and the keys are only visible inside the deserializer —
//! hence this visitor rather than a post-parse scan.

use serde::de::{Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Value};

use super::error::{ManagedError, ManagedErrorCode};

/// Parse `text` as JSON, refusing any duplicate object key.
pub fn parse_strict(text: &str) -> Result<Value, ManagedError> {
    serde_json::from_str::<StrictValue>(text)
        .map(|value| value.0)
        .map_err(|error| {
            ManagedError::planning(
                ManagedErrorCode::PolicyInvalid,
                format!("input is not valid JSON: {error}"),
            )
        })
}

/// Lower-case hexadecimal.
pub fn hex_lower(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// Parse `text` as JSON with a protocol-specific error code.
pub fn parse_strict_with(text: &str, code: ManagedErrorCode) -> Result<Value, ManagedError> {
    parse_strict(text).map_err(|error| ManagedError::planning(code, error.message))
}

struct StrictValue(Value);

impl<'de> Deserialize<'de> for StrictValue {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(StrictVisitor)
    }
}

struct StrictVisitor;

impl<'de> Visitor<'de> for StrictVisitor {
    type Value = StrictValue;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a JSON value with unique object keys")
    }

    fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::Bool(value)))
    }

    fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::from(value)))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::from(value)))
    }

    fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::from(value)))
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::String(value.to_string())))
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::String(value)))
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::Null))
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::Null))
    }

    fn visit_some<D: Deserializer<'de>>(self, deserializer: D) -> Result<Self::Value, D::Error> {
        StrictValue::deserialize(deserializer)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<Self::Value, A::Error> {
        let mut values = Vec::new();
        while let Some(StrictValue(value)) = sequence.next_element()? {
            values.push(value);
        }
        Ok(StrictValue(Value::Array(values)))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut object = Map::new();
        while let Some(key) = map.next_key::<String>()? {
            if object.contains_key(&key) {
                return Err(serde::de::Error::custom(format!(
                    "duplicate object key '{key}'"
                )));
            }
            let StrictValue(value) = map.next_value()?;
            object.insert(key, value);
        }
        Ok(StrictValue(Value::Object(object)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_duplicate_keys_are_refused_at_any_depth() {
        let error = parse_strict(r#"{"a":{"b":1,"b":2}}"#).expect_err("nested duplicate");
        assert!(error.message.contains("duplicate"));
        assert!(parse_strict(r#"{"a":1,"b":2}"#).is_ok());
    }

    #[test]
    fn test_invalid_json_is_a_planning_error() {
        let error = parse_strict("{").expect_err("truncated input");
        assert_eq!(error.code, ManagedErrorCode::PolicyInvalid);
    }
}
