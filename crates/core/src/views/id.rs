/// Domain IDs are opaque, but view keys and action IDs need a bounded wire grammar.
/// Slashes and a fragment marker are reserved for the canonical GitHub PR identity.
pub fn valid(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 256
        && (id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            || github_pr(id))
}

fn github_pr(id: &str) -> bool {
    let Some(rest) = id.strip_prefix("github.com/") else {
        return false;
    };
    let Some((repo, number)) = rest.split_once('#') else {
        return false;
    };
    let mut parts = repo.split('/');
    let (Some(owner), Some(name), None) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    let segment = |s: &str| {
        !s.is_empty()
            && s != "."
            && s != ".."
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.')
    };
    segment(owner)
        && segment(name)
        && !number.is_empty()
        && number.bytes().all(|b| b.is_ascii_digit())
        && number.parse::<u64>().is_ok_and(|number| number > 0)
}

/// Encode an opaque ID as exactly one URL path segment.
pub fn path_segment(id: &str) -> String {
    let mut result = String::with_capacity(id.len());
    for byte in id.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.') {
            result.push(char::from(byte));
        } else {
            result.push_str(&format!("%{byte:02X}"));
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_real_ids_without_admitting_paths() {
        for id in [
            "hf-weekly-2026-09-30",
            "a98d034b-249e-4487-a89b-57de38c26372",
            "github.com/bitomule/alondra#154",
        ] {
            assert!(valid(id), "{id}");
        }
        for id in [
            "",
            "../secret",
            "github.com/../repo#154",
            "github.com/a/b#1/other",
            "github.com/a/b#0",
            "github.com/a/b#1?x=2",
            "%2F",
            "a:b",
            "a@b",
        ] {
            assert!(!valid(id), "{id}");
        }
        assert_eq!(
            path_segment("github.com/a/b#154"),
            "github.com%2Fa%2Fb%23154"
        );
    }
}
