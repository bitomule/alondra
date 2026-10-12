use sha2::{Digest, Sha256};

pub const MAX_OPERATION_COUNTER: u64 = 9_007_199_254_740_991;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OperationId {
    pub epoch_tag: String,
    pub generation: u64,
    pub sequence: u64,
    pub nonce: String,
}

impl OperationId {
    pub fn parse(value: &str) -> Option<Self> {
        if value.len() > 128 {
            return None;
        }
        let mut parts = value.split('_');
        if parts.next()? != "op" {
            return None;
        }
        let epoch_tag = parts.next()?;
        let generation = counter(parts.next()?)?;
        let sequence = counter(parts.next()?)?;
        let nonce = parts.next()?;
        if parts.next().is_some()
            || epoch_tag.len() != 32
            || !epoch_tag.bytes().all(lower_hex)
            || !uuid_v4(nonce)
            || nonce.bytes().any(|byte| byte.is_ascii_uppercase())
        {
            return None;
        }
        Some(Self {
            epoch_tag: epoch_tag.into(),
            generation,
            sequence,
            nonce: nonce.into(),
        })
    }

    pub fn matches_epoch(&self, epoch: &str) -> bool {
        self.epoch_tag == epoch_tag(epoch)
    }
}

fn lower_hex(byte: u8) -> bool {
    byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)
}
fn counter(value: &str) -> Option<u64> {
    if value.is_empty()
        || value.starts_with('0')
        || !value.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    let value: u64 = value.parse().ok()?;
    (value <= MAX_OPERATION_COUNTER).then_some(value)
}
pub fn epoch_tag(epoch: &str) -> String {
    Sha256::digest(epoch.as_bytes())[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
pub fn uuid_v4(value: &str) -> bool {
    value.len() == 36
        && [8, 13, 18, 23]
            .iter()
            .all(|index| value.as_bytes()[*index] == b'-')
        && value
            .bytes()
            .enumerate()
            .all(|(index, byte)| [8, 13, 18, 23].contains(&index) || byte.is_ascii_hexdigit())
        && value.as_bytes()[14] == b'4'
        && matches!(
            value.as_bytes()[19].to_ascii_lowercase(),
            b'8' | b'9' | b'a' | b'b'
        )
}
pub fn valid_action_id(value: &str) -> bool {
    uuid_v4(value) || OperationId::parse(value).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn structured_identity_has_exact_canonical_counters_epoch_and_nonce() {
        let valid = format!(
            "op_{}_1_{}_5f0c2d9e-8d0b-4c71-9b0e-2b6f5d8f1a10",
            epoch_tag("epoch"),
            MAX_OPERATION_COUNTER
        );
        let parsed = OperationId::parse(&valid).unwrap();
        assert!(parsed.matches_epoch("epoch"));
        assert!(!parsed.matches_epoch("other"));
        for bad in [
            valid.replace("_1_", "_01_"),
            valid.replace("_1_", "_0_"),
            valid.replace("_1_", "_-1_"),
            valid.replace(
                &MAX_OPERATION_COUNTER.to_string(),
                &(MAX_OPERATION_COUNTER + 1).to_string(),
            ),
            valid.replace("4c71", "1c71"),
            valid.to_ascii_uppercase(),
            valid.clone() + "_extra",
        ] {
            assert!(OperationId::parse(&bad).is_none(), "{bad}");
        }
        assert!(valid_action_id("5F0C2D9E-8D0B-4C71-9B0E-2B6F5D8F1A10"));
        assert!(!valid_action_id("not-a-uuid"));
    }
}
