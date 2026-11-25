//! A forbidden literal or a `requires` trigger of four characters or more is matched without
//! regard to case; a shorter one stays exact.

use model::shell::{find_at_boundary, find_literal, literal_at_boundary, trigger_in};

#[test]
fn a_long_literal_is_found_in_any_case() {
    assert!(literal_at_boundary("gh repo edit --visibility PUBLIC", "--visibility public"));
    assert!(literal_at_boundary("git commit -m 'x' -m 'co-authored-by: a'", "Co-Authored-By"));
    assert!(literal_at_boundary("GIT PUSH --FORCE origin", "--force"));
    assert!(find_at_boundary("Run --Force now", "--force").is_some());
    assert_eq!(find_literal("Some CO-AUTHORED-BY line", "co-authored-by"), Some(5));
}

#[test]
fn a_short_literal_stays_exact() {
    assert!(literal_at_boundary("rm -n x", "-n"));
    assert!(!literal_at_boundary("rm -N x", "-n"), "-N is another switch");
    assert!(!literal_at_boundary("ls -Rf", "-rf"), "three characters stay exact");
    assert!(find_literal("ABC", "abc").is_none());
}

#[test]
fn boundaries_still_hold_in_any_case() {
    assert!(!literal_at_boundary("git push --Force-With-Lease", "--force"), "a longer word is no match");
    assert!(!literal_at_boundary("SSHKEY.mjs", "ssh"));
    assert!(trigger_in("Run /THOR-probe now", "/thor*"));
    assert!(!trigger_in("build/THOROUGH", "/thor*") || trigger_in("build/thorough", "/thor*"));
}
