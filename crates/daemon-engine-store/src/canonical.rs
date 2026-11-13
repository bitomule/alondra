use anyhow::{Result, ensure};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

/// Explicit recursive ordering keeps the receipt stable even if serde_json's
/// preserve_order feature is enabled by another dependency in a future build.
pub(super) fn bytes(value: &impl Serialize) -> Result<Vec<u8>> {
    let value = serde_json::to_value(value)?;
    let mut bytes = Vec::new();
    append(&value, &mut bytes)?;
    Ok(bytes)
}

pub(super) fn digest(value: &impl Serialize) -> Result<[u8; 32]> {
    Ok(Sha256::digest(bytes(value)?).into())
}

fn append(value: &Value, bytes: &mut Vec<u8>) -> Result<()> {
    match value {
        Value::Object(object) => {
            bytes.push(b'{');
            let mut keys: Vec<_> = object.keys().collect();
            keys.sort_unstable();
            for (index, key) in keys.into_iter().enumerate() {
                if index != 0 {
                    bytes.push(b',');
                }
                serde_json::to_writer(&mut *bytes, key)?;
                bytes.push(b':');
                append(&object[key], bytes)?;
            }
            bytes.push(b'}');
        }
        Value::Array(array) => {
            bytes.push(b'[');
            for (index, item) in array.iter().enumerate() {
                if index != 0 {
                    bytes.push(b',');
                }
                append(item, bytes)?;
            }
            bytes.push(b']');
        }
        Value::Number(number) => {
            let safe = number.as_u64().is_some_and(|n| n <= 9_007_199_254_740_991)
                || number
                    .as_i64()
                    .is_some_and(|n| (-9_007_199_254_740_991..=9_007_199_254_740_991).contains(&n));
            ensure!(safe, "domain command requires safe integers");
            serde_json::to_writer(bytes, value)?;
        }
        _ => serde_json::to_writer(bytes, value)?,
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn canonical_nested_keys_preserve_array_order_and_safe_integer_boundary() {
        let first: Value = serde_json::from_str(r#"{"z":[{"b":2,"a":1},3],"a":"á"}"#).unwrap();
        let second: Value = serde_json::from_str(r#"{"a":"á","z":[{"a":1,"b":2},3]}"#).unwrap();
        assert_eq!(
            bytes(&first).unwrap(),
            r#"{"a":"á","z":[{"a":1,"b":2},3]}"#.as_bytes()
        );
        assert_eq!(digest(&first).unwrap(), digest(&second).unwrap());
        assert_ne!(
            digest(&serde_json::json!([1, 2])).unwrap(),
            digest(&serde_json::json!([2, 1])).unwrap()
        );
        assert!(bytes(&serde_json::json!(9_007_199_254_740_991u64)).is_ok());
        assert!(bytes(&serde_json::json!(9_007_199_254_740_992u64)).is_err());
        assert!(bytes(&serde_json::json!(1.5)).is_err());
    }
}
