use std::fmt;

use serde_json::Value;

/// A place where the live state doesn't match what the manifest asks for.
pub struct Mismatch {
    pub path: String,
    pub expected: Value,
    pub actual: Option<Value>,
}

impl fmt::Display for Mismatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.actual {
            Some(actual) => write!(
                f,
                "{}: expected {}, found {actual}",
                self.path, self.expected
            ),
            None => write!(
                f,
                "{}: expected {}, found nothing",
                self.path, self.expected
            ),
        }
    }
}

/// Compares `expected` against `actual`, ignoring object keys that only `actual` has.
///
/// The API returns far more than we configure, so only what the manifest
/// mentions is checked. Arrays must match element for element, in any order.
pub fn subset(path: &str, expected: &Value, actual: &Value) -> Vec<Mismatch> {
    let mut out = Vec::new();
    walk(path, expected, actual, &mut out);
    out
}

fn walk(path: &str, expected: &Value, actual: &Value, out: &mut Vec<Mismatch>) {
    match (expected, actual) {
        (Value::Object(expected), Value::Object(actual)) => {
            for (key, expected) in expected {
                let path = format!("{path}.{key}");
                match actual.get(key) {
                    Some(actual) => walk(&path, expected, actual, out),
                    None => out.push(Mismatch {
                        path,
                        expected: expected.clone(),
                        actual: None,
                    }),
                }
            }
        }
        (Value::Array(e), Value::Array(a)) if arrays_match(e, a) => {}
        _ if expected == actual => {}
        _ => out.push(Mismatch {
            path: path.to_owned(),
            expected: expected.clone(),
            actual: Some(actual.clone()),
        }),
    }
}

fn arrays_match(expected: &[Value], actual: &[Value]) -> bool {
    if expected.len() != actual.len() {
        return false;
    }
    let mut used = vec![false; actual.len()];
    expected.iter().all(|e| {
        let found = actual
            .iter()
            .enumerate()
            .find(|&(i, a)| !used[i] && subset("", e, a).is_empty());
        found.is_some_and(|(i, _)| {
            used[i] = true;
            true
        })
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn ignores_extra_keys() {
        let expected = json!({ "a": 1 });
        let actual = json!({ "a": 1, "b": 2 });
        assert!(subset("x", &expected, &actual).is_empty());
    }

    #[test]
    fn reports_missing_and_different() {
        let expected = json!({ "a": 1, "b": { "c": true } });
        let actual = json!({ "b": { "c": false } });
        let paths: Vec<_> = subset("x", &expected, &actual)
            .into_iter()
            .map(|m| m.path)
            .collect();
        assert_eq!(paths, ["x.a", "x.b.c"]);
    }

    #[test]
    fn arrays_ignore_order_but_not_length() {
        let expected = json!([{ "type": "a" }, { "type": "b" }]);
        let reordered = json!([{ "type": "b", "id": 1 }, { "type": "a", "id": 2 }]);
        let longer = json!([{ "type": "a" }, { "type": "b" }, { "type": "c" }]);
        assert!(subset("x", &expected, &reordered).is_empty());
        assert_eq!(subset("x", &expected, &longer).len(), 1);
    }

    #[test]
    fn toml_integers_equal_json_integers() {
        let from_toml: Value = toml::from_str::<toml::Table>("n = 0").unwrap()["n"]
            .clone()
            .try_into()
            .unwrap();
        let from_json: Value = serde_json::from_str("0").unwrap();
        assert!(subset("x", &from_toml, &from_json).is_empty());
    }
}
