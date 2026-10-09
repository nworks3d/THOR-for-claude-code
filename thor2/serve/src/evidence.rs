//! The two lookups the Response Guard's "checked evidence" needs and the
//! `respond` module (which does no I/O) is handed: does a cited `path:line`
//! name a real line, and which sha-shaped tokens name a real commit. They live
//! in the library, not in the Stop hook's binary, so a replay of recorded
//! replies (`sweep`) asks the very same functions the hook does.

use std::path::Path;

/// Whether a `path:line` citation from an answer names a real line: `cited`
/// resolves (relative to `root`, or as written when absolute) to a file that
/// lies under `root` - the session's own working directory, already
/// canonicalized by the caller - and that file has at least `line` lines.
/// Line 0, a directory, a path that climbs out of `root` (`../`), and anything
/// that cannot be read all answer `false`: a citation has to be checkable to
/// count as evidence (see `respond`'s own "checked citations" section).
pub fn cited_line_exists(root: &Path, cited: &str, line: u64) -> bool {
    if line == 0 {
        return false;
    }
    let Ok(file) = root.join(cited).canonicalize() else { return false };
    if !file.starts_with(root) || !file.is_file() {
        return false;
    }
    let Ok(bytes) = std::fs::read(&file) else { return false };
    let newlines = bytes.iter().filter(|&&b| b == b'\n').count() as u64;
    let lines = if bytes.last().is_some_and(|&b| b != b'\n') { newlines + 1 } else { newlines };
    line <= lines
}

/// Which of `candidates` (sha-shaped tokens from an answer) name a real commit
/// in the git repository `cwd` stands in - `None` when no repository answers
/// (git missing, `cwd` in no repository, any error), so the Response Guard
/// judges a sha by its shape instead (see `respond`'s own "checked evidence"
/// section). One `git cat-file --batch-check` for the whole list, reading only
/// the local object store: nothing is fetched, nothing is written. An
/// ambiguous short sha names real objects, so it counts as real.
pub fn real_commits(cwd: &Path, candidates: &[String]) -> Option<std::collections::HashSet<String>> {
    use std::io::Write as _;
    let mut command = std::process::Command::new("git");
    command
        .arg("-C")
        .arg(cwd)
        .args(["cat-file", "--batch-check"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        // CREATE_NO_WINDOW: a hook never opens a console window.
        command.creation_flags(0x0800_0000);
    }
    let mut child = command.spawn().ok()?;
    let input: String = candidates.iter().map(|c| format!("{c}^{{commit}}\n")).collect();
    // A write error (git already gone) shows up in the exit status below.
    let _ = child.stdin.take()?.write_all(input.as_bytes());
    let out = child.wait_with_output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let answers: Vec<&str> = text.lines().collect();
    if answers.len() != candidates.len() {
        return None;
    }
    Some(
        candidates
            .iter()
            .zip(answers)
            .filter(|(_, answer)| !answer.ends_with(" missing"))
            .map(|(c, _)| c.clone())
            .collect(),
    )
}
