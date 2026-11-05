//! Strict JSON reading: duplicate keys and non-finite constants are
//! errors, never last-wins.

use std::fmt;

use serde::Deserialize;
use serde::de::{self, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Number, Value};

struct Strict(Value);

struct StrictVisitor;

impl<'de> Visitor<'de> for StrictVisitor {
    type Value = Strict;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a JSON value")
    }

    fn visit_bool<E>(self, v: bool) -> Result<Strict, E> {
        Ok(Strict(Value::Bool(v)))
    }

    fn visit_i64<E>(self, v: i64) -> Result<Strict, E> {
        Ok(Strict(Value::from(v)))
    }

    fn visit_u64<E>(self, v: u64) -> Result<Strict, E> {
        Ok(Strict(Value::from(v)))
    }

    fn visit_f64<E: de::Error>(self, v: f64) -> Result<Strict, E> {
        Number::from_f64(v)
            .map(|n| Strict(Value::Number(n)))
            .ok_or_else(|| E::custom("non-finite JSON constant forbidden"))
    }

    fn visit_str<E>(self, v: &str) -> Result<Strict, E> {
        Ok(Strict(Value::String(v.to_owned())))
    }

    fn visit_string<E>(self, v: String) -> Result<Strict, E> {
        Ok(Strict(Value::String(v)))
    }

    fn visit_unit<E>(self) -> Result<Strict, E> {
        Ok(Strict(Value::Null))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Strict, A::Error> {
        let mut items = Vec::new();
        while let Some(Strict(v)) = seq.next_element()? {
            items.push(v);
        }
        Ok(Strict(Value::Array(items)))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut access: A) -> Result<Strict, A::Error> {
        let mut map = Map::new();
        while let Some(key) = access.next_key::<String>()? {
            if map.contains_key(&key) {
                return Err(de::Error::custom(format!(
                    "duplicate JSON key: {}",
                    crate::json::canonical_string(&key)
                )));
            }
            let Strict(v) = access.next_value()?;
            map.insert(key, v);
        }
        Ok(Strict(Value::Object(map)))
    }
}

impl<'de> Deserialize<'de> for Strict {
    fn deserialize<D: de::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        d.deserialize_any(StrictVisitor)
    }
}

/// Parse JSON text, rejecting duplicate object keys.
pub fn loads(text: &str) -> Result<Value, String> {
    serde_json::from_str::<Strict>(text)
        .map(|s| s.0)
        .map_err(|e| {
            let msg = e.to_string();
            if msg.starts_with("duplicate JSON key") {
                msg
            } else {
                format!("invalid JSON: {msg}")
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_duplicates_and_keeps_order() {
        assert!(loads(r#"{"a": 1, "a": 2}"#).is_err());
        assert!(loads("[NaN]").is_err());
        let v = loads(r#"{"z": 1, "a": [1.0, 2]}"#).unwrap_or_default();
        assert_eq!(
            v.as_object().map(|m| m.keys().cloned().collect::<Vec<_>>()),
            Some(vec!["z".into(), "a".into()])
        );
        assert!(v["a"][0].is_f64() && v["a"][1].is_u64());
    }
}
