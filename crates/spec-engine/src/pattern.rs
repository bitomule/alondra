const ANY_SEGMENTS: &str = "**";

pub(crate) fn matches(pattern: &str, path: &str) -> bool {
    let pattern = collapsed(pattern);
    let path: Vec<&str> = path.split('/').collect();
    segments(&pattern, &path)
}

fn collapsed(pattern: &str) -> Vec<&str> {
    let mut out: Vec<&str> = Vec::new();
    for segment in pattern.split('/') {
        if segment == ANY_SEGMENTS && out.last() == Some(&ANY_SEGMENTS) {
            continue;
        }
        out.push(segment);
    }
    out
}

/// `table[i][j]` says whether `pattern[i..]` matches `path[j..]`, filled from the end.
fn segments(pattern: &[&str], path: &[&str]) -> bool {
    let width = path.len() + 1;
    let mut table = vec![false; (pattern.len() + 1) * width];
    table[pattern.len() * width + path.len()] = true;
    for i in (0..pattern.len()).rev() {
        let last = i + 1 == pattern.len();
        for j in (0..=path.len()).rev() {
            let rest = table[(i + 1) * width + j];
            table[i * width + j] = if pattern[i] == ANY_SEGMENTS {
                if last {
                    j < path.len()
                } else {
                    rest || (j < path.len() && table[i * width + j + 1])
                }
            } else {
                j < path.len() && segment_matches(pattern[i], path[j]) && table[(i + 1) * width + j + 1]
            };
        }
    }
    table[0]
}

fn segment_matches(pattern: &str, segment: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let segment: Vec<char> = segment.chars().collect();
    let width = segment.len() + 1;
    let mut table = vec![false; (pattern.len() + 1) * width];
    table[pattern.len() * width + segment.len()] = true;
    for i in (0..pattern.len()).rev() {
        for j in (0..=segment.len()).rev() {
            table[i * width + j] = if pattern[i] == '*' {
                table[(i + 1) * width + j] || (j < segment.len() && table[i * width + j + 1])
            } else {
                j < segment.len() && segment[j] == pattern[i] && table[(i + 1) * width + j + 1]
            };
        }
    }
    table[0]
}

/// Whether some path below `folder` could match the pattern.
pub(crate) fn reaches_below(pattern: &str, folder: &str) -> bool {
    let pattern: Vec<&str> = pattern.split('/').collect();
    let folder: Vec<&str> = folder.split('/').collect();
    for (index, expected) in folder.iter().enumerate() {
        match pattern.get(index) {
            None => return false,
            Some(&ANY_SEGMENTS) => return true,
            Some(segment) if segment_matches(segment, expected) => {}
            Some(_) => return false,
        }
    }
    pattern.len() > folder.len()
}

/// Whether a relative path or pattern stays inside the repository.
pub(crate) fn is_inside(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.starts_with('\\')
        && path.split('/').all(|segment| !segment.is_empty() && segment != "." && segment != "..")
}

/// A folder setting without its trailing `/`, when it stays inside the repository.
pub(crate) fn folder(dir: &str) -> Option<String> {
    let trimmed = dir.trim_end_matches('/');
    (is_inside(trimmed) && !trimmed.contains('*')).then(|| trimmed.to_string())
}
