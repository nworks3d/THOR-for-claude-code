//! A forbidden literal that sits only in what a read-only search looks FOR (the pattern of
//! `grep`, `rg`, `findstr`, `Select-String`, `git log --grep`) is not in the call: a search writes
//! nothing. The same literal anywhere else in the call still counts. Also: `--flag value` and
//! `--flag=value` are one flag, and a single-letter flag is found inside a cluster of letters.

use model::shell::{call_segments, literal_at_boundary, literal_in};

const FORBIDDEN: &[&str] = &["Co-Authored-By", "Generated with"];

/// Whether any segment of the call holds one of the literals.
fn refused(tool: &str, command: &str, literal: &str) -> bool {
    call_segments(tool, command).iter().any(|s| literal_in(s, literal))
}

fn any_forbidden(tool: &str, command: &str) -> bool {
    FORBIDDEN.iter().any(|l| refused(tool, command, l))
}

#[test]
fn a_read_only_search_for_the_words_is_free() {
    for command in [
        r#"git log --oneline | grep -ciE "co-authored|generated with|claude|anthropic""#,
        r#"git -C "$R" log main..b --format='%B' | grep -icE 'co-authored|generated with|anthropic'"#,
        r#"git log --format=%B | grep -i 'Co-Authored-By' | wc -l"#,
        r#"git log | grep -e "generated with" -n"#,
        r#"git log | grep --regexp="Co-Authored-By""#,
        r#"git log | rg -i "co-authored-by""#,
        r#"git log | egrep -c 'Co-Authored-By' && echo done"#,
        r#"git log | grep -A 2 "Generated with""#,
        r#"git log --grep='Co-Authored-By' --oneline"#,
        r#"git log --grep "Generated with" --oneline"#,
        r#"git log -S"Co-Authored-By" --oneline"#,
        r#"git -C x grep -n "Co-Authored-By" -- docs"#,
        r#"git log | findstr /i /c:"co-authored-by""#,
        r#"git log | findstr "Co-Authored-By""#,
    ] {
        assert!(!any_forbidden("Bash", command), "{command}");
    }
    for command in [
        r#"git log | Select-String -Pattern "Co-Authored-By""#,
        r#"git log | Select-String "Generated with" -CaseSensitive"#,
        r#"git log | sls -Pattern:"Co-Authored-By""#,
    ] {
        assert!(!any_forbidden("PowerShell", command), "{command}");
    }
}

#[test]
fn the_same_words_anywhere_else_in_the_call_still_count() {
    for command in [
        r#"git commit -m "x" -m "Co-Authored-By: a""#,
        r#"git commit -m "Generated with a tool""#,
        // the pattern is only the first word; a literal in a later word is a file or text
        r#"git log | grep foo "Co-Authored-By""#,
        // a search that feeds a commit: the message is written
        r#"echo "Co-Authored-By: a" | grep -v zzz | git commit -F -"#,
        r#"git log --grep=x | git commit -m "Co-Authored-By: b""#,
        // the flag itself, not its value, is what a rule may forbid
        r#"git log --grep=x -m "Co-Authored-By""#,
        // a pattern file is data of the call
        r#"grep -f patterns.txt "Co-Authored-By""#,
    ] {
        assert!(any_forbidden("Bash", command), "{command}");
    }
    assert!(any_forbidden("PowerShell", r#"git commit -m "Co-Authored-By: a""#));
}

#[test]
fn a_flag_a_rule_forbids_is_still_seen_next_to_a_search() {
    assert!(refused("Bash", "git log --grep=x --oneline", "--grep"), "the flag is not its value");
    assert!(!refused("Bash", "git log --grep=Co-Authored-By", "Co-Authored-By"));
}

#[test]
fn a_long_flag_is_the_same_with_a_space_or_an_equals_sign() {
    assert!(literal_at_boundary("gh repo edit o/r --visibility=public", "--visibility public"));
    assert!(literal_at_boundary("gh repo edit o/r --visibility public", "--visibility=public"));
    assert!(literal_at_boundary("gh repo edit o/r --visibility PUBLIC", "--visibility=public"));
    assert!(!literal_at_boundary("gh repo edit o/r --visibility=private", "--visibility public"));
    assert!(!literal_at_boundary("gh repo edit o/r --visibility-x=public", "--visibility public"));
    // two flags in a row are not one flag with a value
    assert!(!literal_at_boundary("git push --delete=force", "--delete --force"));
    let segs = call_segments("Bash", r#"gh repo edit o/r --visibility="public""#);
    assert!(segs.iter().any(|s| literal_in(s, "--visibility public")));
}

#[test]
fn a_single_letter_flag_is_found_inside_a_cluster_of_letters() {
    assert!(literal_at_boundary("git branch -D x", "-D"));
    assert!(literal_at_boundary("git branch -Df x", "-D"));
    assert!(literal_at_boundary("git branch -fD x", "-D"));
    assert!(literal_at_boundary("git branch -Df x", "git branch -D"));
    assert!(!literal_at_boundary("git branch -d x", "-D"), "the other case is another flag");
    assert!(!literal_at_boundary("git branch -Dx=1", "-D"));
    assert!(!literal_at_boundary("git branch --Df x", "-D"));
    assert!(!literal_at_boundary("git branch -D1 x", "-D"));
    assert!(!literal_at_boundary("git tag -Df x", "git branch -D"));
    assert!(!literal_at_boundary("node -eval 1", "-e"), "a longer single-dash word is a word");
    assert!(!literal_at_boundary("find . -name x", "-e") && !literal_at_boundary("find . -name x", "-m"));
    assert!(literal_at_boundary("rm -rfv x", "-v"));
    assert!(!literal_at_boundary("echo a-Df", "-D"), "not at the start of a word");
}
