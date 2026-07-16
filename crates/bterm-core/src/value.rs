//! The structured value type that flows through pipes.

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;

/// Largest integer magnitude representable exactly in a JS number (2^53).
/// The lexer rejects literals beyond this so shell-authored values can't
/// silently lose precision crossing the WASM boundary.
pub const MAX_SAFE_INT: i64 = 9_007_199_254_740_992;

/// Structured value. Tables are `List` of `Record`s. Serializes to natural
/// JSON (records as objects), matching how values cross to JavaScript.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    List(Vec<Value>),
    Record(IndexMap<String, Value>),
}

impl Value {
    pub fn record(entries: impl IntoIterator<Item = (String, Value)>) -> Value {
        Value::Record(entries.into_iter().collect())
    }

    pub fn type_name(&self) -> &'static str {
        match self {
            Value::Null => "null",
            Value::Bool(_) => "bool",
            Value::Int(_) => "int",
            Value::Float(_) => "float",
            Value::Str(_) => "string",
            Value::List(_) => "list",
            Value::Record(_) => "record",
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_int(&self) -> Option<i64> {
        match self {
            Value::Int(i) => Some(*i),
            _ => None,
        }
    }

    /// Is this a `List` whose items are all `Record`s (i.e. a table)?
    /// Empty lists count as (empty) tables.
    pub fn is_table(&self) -> bool {
        match self {
            Value::List(items) => items.iter().all(|v| matches!(v, Value::Record(_))),
            _ => false,
        }
    }

    /// Ordering for `sort-by` and `where gt/lt/ge/le`. Numeric types compare
    /// across Int/Float; strings and bools compare within type; everything
    /// else is incomparable (`None`).
    pub fn partial_cmp_values(&self, other: &Value) -> Option<Ordering> {
        use Value::*;
        match (self, other) {
            (Int(a), Int(b)) => Some(a.cmp(b)),
            (Float(a), Float(b)) => a.partial_cmp(b),
            (Int(a), Float(b)) => (*a as f64).partial_cmp(b),
            (Float(a), Int(b)) => a.partial_cmp(&(*b as f64)),
            (Str(a), Str(b)) => Some(a.cmp(b)),
            (Bool(a), Bool(b)) => Some(a.cmp(b)),
            (Null, Null) => Some(Ordering::Equal),
            _ => None,
        }
    }

    /// Total order for sorting: values group by type rank (numbers, then
    /// strings, bools, null, lists, records) and compare within the rank;
    /// numbers cross-compare Int/Float and NaN sorts after every other
    /// number. Unlike `partial_cmp_values`, this never violates
    /// `slice::sort_by`'s total-order contract on mixed-type columns.
    pub fn total_cmp_values(&self, other: &Value) -> Ordering {
        fn rank(v: &Value) -> u8 {
            match v {
                Value::Int(_) | Value::Float(_) => 0,
                Value::Str(_) => 1,
                Value::Bool(_) => 2,
                Value::Null => 3,
                Value::List(_) => 4,
                Value::Record(_) => 5,
            }
        }
        use Value::*;
        match rank(self).cmp(&rank(other)) {
            Ordering::Equal => match (self, other) {
                (Int(a), Int(b)) => a.cmp(b),
                (Int(a), Float(b)) => (*a as f64).total_cmp(b),
                (Float(a), Int(b)) => a.total_cmp(&(*b as f64)),
                (Float(a), Float(b)) => a.total_cmp(b),
                (Str(a), Str(b)) => a.cmp(b),
                (Bool(a), Bool(b)) => a.cmp(b),
                (List(a), List(b)) => a.len().cmp(&b.len()),
                (Record(a), Record(b)) => a.len().cmp(&b.len()),
                _ => Ordering::Equal,
            },
            other_rank => other_rank,
        }
    }

    /// Any NaN/Infinity anywhere in this value? (JSON cannot represent
    /// them; `to json` refuses instead of silently writing `null`.)
    pub fn has_non_finite(&self) -> bool {
        match self {
            Value::Float(f) => !f.is_finite(),
            Value::List(items) => items.iter().any(Value::has_non_finite),
            Value::Record(map) => map.values().any(Value::has_non_finite),
            _ => false,
        }
    }

    /// Loose equality used by `where eq/ne`: same as PartialEq except numeric
    /// types compare across Int/Float.
    pub fn loose_eq(&self, other: &Value) -> bool {
        match (self, other) {
            (Value::Int(a), Value::Float(b)) => (*a as f64) == *b,
            (Value::Float(a), Value::Int(b)) => *a == (*b as f64),
            _ => self == other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_round_trip_is_natural() {
        let v = Value::record([
            ("name".to_string(), Value::Str("rust".into())),
            ("stars".to_string(), Value::Int(42)),
        ]);
        let json = serde_json::to_string(&v).expect("serialize");
        assert_eq!(json, r#"{"name":"rust","stars":42}"#);
        let back: Value = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, v);
    }

    #[test]
    fn untagged_numbers_deserialize_to_int_then_float() {
        let v: Value = serde_json::from_str("3").expect("int");
        assert_eq!(v, Value::Int(3));
        let v: Value = serde_json::from_str("3.5").expect("float");
        assert_eq!(v, Value::Float(3.5));
    }

    #[test]
    fn cross_numeric_compare() {
        assert_eq!(
            Value::Int(2).partial_cmp_values(&Value::Float(2.5)),
            Some(Ordering::Less)
        );
        assert!(Value::Int(2).loose_eq(&Value::Float(2.0)));
    }

    #[test]
    fn table_detection() {
        assert!(Value::List(vec![Value::record([("a".into(), Value::Int(1))])]).is_table());
        assert!(!Value::List(vec![Value::Int(1)]).is_table());
        assert!(!Value::Int(1).is_table());
    }
}
