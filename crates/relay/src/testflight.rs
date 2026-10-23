use anyhow::{Result, ensure};
use serde_json::Value;

const VARIABLE: &str = "ALONDRA_TESTFLIGHT_URL";

pub fn from_env() -> Result<Option<String>> {
    parse(std::env::var(VARIABLE).ok())
}

fn parse(value: Option<String>) -> Result<Option<String>> {
    let Some(value) = value.map(|v| v.trim().to_owned()).filter(|v| !v.is_empty()) else {
        return Ok(None);
    };
    let url = reqwest::Url::parse(&value)?;
    let code = url.path().strip_prefix("/join/").unwrap_or("");
    ensure!(
        url.scheme() == "https"
            && url.host_str() == Some("testflight.apple.com")
            && url.port().is_none()
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none()
            && !code.is_empty()
            && code.chars().all(|c| c.is_ascii_alphanumeric()),
        "{VARIABLE} must be a TestFlight public link: https://testflight.apple.com/join/CODE"
    );
    Ok(Some(url.into()))
}

/// Only for responses to someone who holds a session or has just created their account.
pub fn offer(body: &mut Value, link: Option<&str>) {
    if let (Some(link), Some(object)) = (link, body.as_object_mut()) {
        object.insert("testflight_url".into(), link.into());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn unset_or_empty_means_no_link() {
        assert_eq!(parse(None).unwrap(), None);
        assert_eq!(parse(Some("  ".into())).unwrap(), None);
    }

    #[test]
    fn accepts_a_testflight_public_link() {
        assert_eq!(
            parse(Some("https://testflight.apple.com/join/yuRS3apy".into())).unwrap().as_deref(),
            Some("https://testflight.apple.com/join/yuRS3apy")
        );
    }

    #[test]
    fn refuses_anything_that_is_not_a_testflight_public_link() {
        for value in [
            "http://testflight.apple.com/join/yuRS3apy",
            "https://testflight.apple.com.evil.example/join/yuRS3apy",
            "https://evil.example/join/yuRS3apy",
            "https://testflight.apple.com/v1/app/123",
            "https://testflight.apple.com/join/",
            "https://testflight.apple.com/join/yu RS",
            "https://testflight.apple.com:8443/join/yuRS3apy",
            "https://user@testflight.apple.com/join/yuRS3apy",
            "https://testflight.apple.com/join/yuRS3apy?next=https://evil.example",
            "javascript:alert(1)",
        ] {
            assert!(parse(Some(value.into())).is_err(), "{value}");
        }
    }

    #[test]
    fn offers_the_link_only_when_configured() {
        let mut body = json!({"status": "active"});
        offer(&mut body, None);
        assert_eq!(body, json!({"status": "active"}));
        offer(&mut body, Some("https://testflight.apple.com/join/yuRS3apy"));
        assert_eq!(body["testflight_url"], "https://testflight.apple.com/join/yuRS3apy");
    }
}
