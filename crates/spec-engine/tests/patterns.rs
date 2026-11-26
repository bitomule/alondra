use alondra_spec_engine::matches;

fn answers(pattern: &str, rows: &[(&str, bool)]) {
    for &(path, answer) in rows {
        assert_eq!(matches(pattern, path), answer, "{pattern} against {path}");
    }
}

// spec: spec-engine.patterns/star-stays-in-one-segment
#[test]
fn a_single_star_matches_inside_one_segment_only() {
    answers(
        "crates/*/Cargo.toml",
        &[("crates/billing/Cargo.toml", true), ("crates/billing/sub/Cargo.toml", false), ("crates/Cargo.toml", false)],
    );
}

// spec: spec-engine.patterns/double-star-crosses-segments
#[test]
fn a_double_star_matches_any_number_of_whole_segments() {
    answers(
        "crates/billing/**/*.rs",
        &[
            ("crates/billing/lib.rs", true),
            ("crates/billing/src/tax/rates.rs", true),
            ("crates/billing/src/tax/rates.txt", false),
            ("crates/ledger/src/lib.rs", false),
        ],
    );
}

// spec: spec-engine.patterns/trailing-double-star-takes-everything-below
#[test]
fn a_trailing_double_star_matches_everything_below_the_folder() {
    answers(
        "specs/**",
        &[("specs/billing/boundary.toml", true), ("specs/a", true), ("specs", false), ("specsheet/a", false)],
    );
}

// spec: spec-engine.patterns/literal-pattern-matches-itself
#[test]
fn a_pattern_without_stars_matches_only_that_path() {
    answers("Cargo.toml", &[("Cargo.toml", true), ("crates/billing/Cargo.toml", false)]);
}

// spec: spec-engine.patterns/matching-is-case-sensitive
#[test]
fn matching_tells_upper_from_lower_case() {
    answers("docs/*.md", &[("docs/README.md", true), ("docs/README.MD", false)]);
}

// spec: spec-engine.patterns/repeated-double-stars-are-one
#[test]
fn consecutive_double_stars_match_what_one_matches() {
    let rows = [("a/x", true), ("a/b/c/x", true), ("a/b", false)];
    answers("a/**/**/x", &rows);
    answers("a/**/x", &rows);
    let below = [("specs", false), ("specs/a", true), ("specs/a/b", true)];
    answers("specs/**/**", &below);
    answers("specs/**", &below);
}
