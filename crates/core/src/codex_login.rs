//! What the apps show of a Codex device sign-in; the sign-in itself runs in `crates/system`.

pub fn verification_url(value: &str) -> bool {
    matches!(value, "https://auth.openai.com/codex/device" | "https://auth.openai.com/codex/device/")
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Challenge {
    pub url: String,
    pub code: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_login_verification_link_rejects_lookalikes_and_credentials() {
        assert!(verification_url("https://auth.openai.com/codex/device"));
        for url in ["http://auth.openai.com/codex/device", "https://auth.openai.com.evil.test/codex/device", "https://auth.openai.com@evil.test/codex/device", "https://auth.openai.com/codex/device?secret=x", "https://evil.test/"] { assert!(!verification_url(url)); }
    }
}
