//! Which calls only LOOK. A call that changes nothing cannot break the thing a
//! write-time fact is about, so `rank::select` serves a fact that was reached
//! only through a file it names on such a call when the fact is about the
//! reading itself (see `readers_of` and `rank::skipped_on_a_read_only_call`).
//!
//! THE DEFECT THIS CLOSES (field test 7, 2026-10-02): `cat server/package.json |
//! grep test` served four facts about restarts, version stamps and the
//! changelog, all anchored at that file and all about CHANGING it.
//!
//! Deliberately conservative: a call is read-only only when EVERY simple
//! command of it is on the lists below and none writes (no redirect, no `-i`,
//! no `-delete`, no `-exec`), and the call was read in full (no flat
//! fallback segment, nothing over `ssh`). Anything not recognised counts as a
//! write, which keeps the old behaviour: serve.

use crate::shell::Segment;

/// Programs that only read, whatever their arguments (output redirects are
/// checked separately).
const READ_ONLY_PROGRAMS: &[&str] = &[
    "cat", "head", "tail", "less", "more", "ls", "dir", "wc", "grep", "egrep", "fgrep", "rg", "stat", "file", "type",
    "tree", "sort", "uniq", "cut", "diff", "cmp", "basename", "dirname", "realpath", "which", "where", "sha256sum",
    "md5sum", "get-content", "gc", "select-string", "sls", "get-childitem", "gci", "pwd", "cd", "echo",
];

/// The moments that CHANGE something. A read-only call is never one of these,
/// even when its text names one (`grep "git push" docs`); reading a secret or
/// live data is still `Credentials` / `ProdData`.
pub const CHANGES_THINGS: &[intent::Action] = &[
    intent::Action::Publish,
    intent::Action::Commit,
    intent::Action::Push,
    intent::Action::Deploy,
    intent::Action::Delete,
    intent::Action::Send,
    intent::Action::Spend,
    intent::Action::Install,
    intent::Action::Configure,
];

/// Programs that cannot name the reader in a fact (they say nothing of what is
/// being read).
const PROGRAMS_WITHOUT_A_READER: &[&str] = &["cd", "echo", "pwd"];

/// `git` subcommands that only read.
const READ_ONLY_GIT: &[&str] = &["log", "status", "diff", "show", "blame", "ls-files", "rev-parse", "shortlog", "describe", "cat-file"];

/// `find` arguments that make it act on what it finds.
const FIND_ACTIONS: &[&str] = &["-delete", "-exec", "-execdir", "-ok", "-okdir", "-fprint", "-fprint0", "-fprintf", "-fls"];

fn program_name(seg: &Segment) -> &str {
    seg.program.strip_suffix(".exe").unwrap_or(&seg.program)
}

/// The words of the segment after its program, skipping git's own global
/// options (`-C path`, `-c key=value`, `--no-pager`).
fn git_subcommand(seg: &Segment) -> Option<&str> {
    let mut words = seg.head.iter().skip(1).map(String::as_str);
    while let Some(word) = words.next() {
        match word {
            "-C" | "-c" | "--git-dir" | "--work-tree" => {
                words.next();
            }
            w if w.starts_with('-') => {}
            w => return Some(w),
        }
    }
    None
}

fn sed_edits_in_place(seg: &Segment) -> bool {
    seg.head.iter().skip(1).any(|w| {
        w == "--in-place"
            || w.starts_with("--in-place=")
            || (w.starts_with('-') && !w.starts_with("--") && w[1..].chars().any(|c| c == 'i'))
    })
}

/// Whether one simple command only reads.
fn segment_is_read_only(seg: &Segment) -> bool {
    if seg.flat || seg.remote || !seg.redirects.is_empty() {
        return false;
    }
    let program = program_name(seg);
    match program {
        "git" => git_subcommand(seg).is_some_and(|sub| READ_ONLY_GIT.contains(&sub)),
        "find" => !seg.head.iter().any(|w| FIND_ACTIONS.contains(&w.as_str())),
        "sed" => !sed_edits_in_place(seg),
        other => READ_ONLY_PROGRAMS.contains(&other),
    }
}

/// Whether the whole call only reads: at least one simple command, and every
/// one of them reads.
pub fn is_read_only_call(segments: &[Segment]) -> bool {
    !segments.is_empty() && segments.iter().all(segment_is_read_only)
}

/// What a fact that is about this reading would call it: the program of every
/// simple command (`grep`, `cat`), and `git diff` for a git one; never `cd`,
/// `echo` or `pwd`, which say nothing of the reading.
pub fn readers_of(segments: &[Segment]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for seg in segments {
        let program = program_name(seg);
        if PROGRAMS_WITHOUT_A_READER.contains(&program) {
            continue;
        }
        let reader = match (program, git_subcommand(seg)) {
            ("git", Some(sub)) => format!("git {sub}"),
            _ => program.to_string(),
        };
        if !out.contains(&reader) {
            out.push(reader);
        }
    }
    out
}

/// Whether `text` names `reader` as a whole word (case-insensitive).
pub fn text_names(text: &str, reader: &str) -> bool {
    let text = text.to_lowercase();
    let mut from = 0;
    while let Some(at) = text[from..].find(reader) {
        let start = from + at;
        let end = start + reader.len();
        let before_ok = !text[..start].chars().next_back().is_some_and(|c| c.is_alphanumeric() || c == '-' || c == '_');
        let after_ok = !text[end..].chars().next().is_some_and(|c| c.is_alphanumeric() || c == '-' || c == '_');
        if before_ok && after_ok {
            return true;
        }
        from = end;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::{segments, Dialect};

    fn read_only(command: &str) -> bool {
        is_read_only_call(&segments(command, Dialect::Sh))
    }

    #[test]
    fn plain_looking_calls_are_read_only() {
        for command in [
            "cat server/package.json | grep -n test",
            "git diff --stat server/lib/pdf.js && cat files/TESTING.md | head -60",
            "ls test && sed -n 1,20p test/a.test.js",
            "git -C /some/repo log --oneline -5",
            "git status",
            "find . -name '*.js'",
            "rg needle src",
            "cd server && grep -n x lib/a.js",
        ] {
            assert!(read_only(command), "{command} only reads");
        }
    }

    #[test]
    fn anything_that_writes_or_is_unknown_is_not() {
        for command in [
            "sed -i s/a/b/ server/package.json",
            "sed -ni 1p file",
            "cat a > b",
            "cat a >> b",
            "find . -name x -delete",
            "find . -exec rm {} ;",
            "git commit -m x",
            "git push",
            "git branch -D x",
            "npm test",
            "node --test",
            "cat a | tee b",
            "bash -c 'cat a'",
            "ssh host cat /etc/hosts",
            "cat $(rm x)",
            "",
        ] {
            assert!(!read_only(command), "{command} must not count as read-only");
        }
    }

    #[test]
    fn the_readers_are_the_programs_and_git_subcommands() {
        let segs = segments("cd server && git diff --stat x && cat a | grep b", Dialect::Sh);
        assert_eq!(readers_of(&segs), vec!["git diff", "cat", "grep"]);
    }

    #[test]
    fn a_fact_names_a_reader_as_a_whole_word_only() {
        assert!(text_names("Gebruik grep -n op die log, niet cat.", "grep"));
        assert!(!text_names("Het GREP-pad is traag", "grep"), "grep-pad is one hyphenated word");
        assert!(!text_names("De category-lijst", "cat"));
        assert!(text_names("run git diff first", "git diff"));
        assert!(!text_names("de suite-versie komt uit package.json", "cat"));
    }
}
