//! NO SHELL COMMAND RUNS THOR'S HOOK (2026-11-25).
//!
//! THOR's UserPromptSubmit hook writes the record of what the owner typed (`prompt_record`), and
//! every check that accepts the owner's own words trusts that record. The hook is THOR's program
//! `serve` run with the `hook` subcommand and a payload on its standard input, so a shell
//! command that runs it by hand with an invented payload would write a message the owner never
//! typed. The PreToolUse hook therefore refuses a Bash or PowerShell command that runs `serve`
//! with `hook` (also through `bash -c`, `cmd /c`, `Start-Process`, `cargo run`, `sudo` and the
//! like, with or without `.exe`, a folder in front, quotes or escapes inside the words).
//!
//! HONESTLY NOT SEEN: a script file that does it (the command only names the script), a copy of
//! the program under another name, and a program that builds the words at run time. Those are
//! covered by the rest of the wall (the memory's folder cannot be written by a call that is seen)
//! and by transparency, not by this check.

use crate::write_guard::{command_name, command_segments, leader_of, SHELLS};

/// A word with the spellings that hide it taken out: no quote, backtick or backslash, lower case.
fn bare(word: &str) -> String {
    word.chars().filter(|c| !matches!(c, '\\' | '`' | '\'' | '"')).collect::<String>().to_ascii_lowercase()
}

/// Is this word the program `serve` (a path in front and `.exe` behind are fine)?
fn is_serve(word: &str) -> bool {
    let unquoted: String = word.chars().filter(|c| !matches!(c, '`' | '\'' | '"')).collect();
    command_name(&unquoted) == "serve" || command_name(&bare(word)) == "serve"
}

/// The subcommand of `serve` in these words: the first word that is no flag, skipping the value of
/// `--db` (its only global flag). The words may hold several arguments in one string.
fn subcommand_of(words: &[String]) -> Option<String> {
    let flat: Vec<String> = words.iter().flat_map(|w| w.split_whitespace().map(str::to_string)).collect();
    let mut it = flat.iter();
    while let Some(w) = it.next() {
        let w = bare(w);
        if w == "--db" {
            it.next();
        } else if w == "--" || w.starts_with('-') {
            continue;
        } else {
            return Some(w);
        }
    }
    None
}

/// What follows the word `serve` in these words, when one is there.
fn after_serve(words: &[String]) -> Option<&[String]> {
    words.iter().position(|w| is_serve(w)).map(|i| &words[i + 1..])
}

/// Programs that carry another program and its words as arguments.
const LAUNCHERS: [&str; 7] = ["start-process", "start", "saps", "cargo", "invoke-expression", "iex", "invoke-command"];

fn segment_runs_the_hook(words: &[String], depth: usize) -> bool {
    let Some((name, rest)) = leader_of(words) else { return false };
    let leader = words.len() - rest.len() - 1;
    if is_serve(&words[leader]) {
        return subcommand_of(rest).as_deref() == Some("hook");
    }
    if depth >= 3 || !(SHELLS.contains(&name.as_str()) || LAUNCHERS.contains(&name.as_str())) {
        return false;
    }
    // `Start-Process serve.exe hook`, `cargo run --bin serve -- hook`, `cmd /c serve.exe hook`: the
    // program and what follows it are arguments of the launcher.
    if after_serve(rest).is_some_and(|after| subcommand_of(after).as_deref() == Some("hook")) {
        return true;
    }
    // `bash -c "serve hook"`: a word that is a whole command of its own.
    rest.iter().filter(|w| w.contains(' ')).any(|w| command_text_runs_the_hook(w, depth + 1))
}

fn command_text_runs_the_hook(command: &str, depth: usize) -> bool {
    command_segments(command).iter().any(|words| segment_runs_the_hook(words, depth))
}

/// Does this shell command run THOR's `serve` with the `hook` subcommand?
pub fn command_runs_the_hook(command: &str) -> bool {
    command_text_runs_the_hook(command, 0)
}

/// The hook's refusal of a Bash or PowerShell call that runs THOR's hook by hand; `None` for every
/// other call.
pub fn refusal(tool_name: &str, tool_input: Option<&serde_json::Value>) -> Option<String> {
    if !matches!(tool_name, "Bash" | "PowerShell") {
        return None;
    }
    let command = tool_input?.get("command")?.as_str()?;
    command_runs_the_hook(command).then(|| {
        "[THOR] That command runs THOR's own hook program by hand. The hook writes THOR's record of what the owner typed, and \
         that record is only good when the owner really typed it, so nothing was done. THOR's hooks are run by Claude Code, \
         never by a command. Reading with `serve` (status, doctor, lookup) is fine."
            .to_string()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_common_spellings_that_run_the_hook_are_refused() {
        for command in [
            "serve hook",
            "serve.exe hook",
            "./serve hook < payload.json",
            "echo '{}' | C:\\Users\\x\\thor2\\bin\\serve.exe hook",
            "\"C:\\Users\\x\\thor2\\bin\\serve.exe\" --db C:\\s\\thor.db hook",
            "& 'C:\\Users\\x\\thor2\\bin\\serve.exe' hook",
            "SERVE.EXE HOOK",
            "serve ho\"\"ok",
            "serve h\\ook",
            "sudo serve hook",
            "cd /tmp && serve hook",
            "env THOR_X=1 serve hook",
            "bash -c \"serve hook\"",
            "powershell -Command \"& serve.exe hook\"",
            "cmd /c serve.exe hook",
            "Start-Process serve.exe -ArgumentList hook",
            "Start-Process serve.exe -ArgumentList '--db a hook'",
            "cargo run --bin serve -- hook",
            "sh -c 'echo x; serve --db a hook'",
        ] {
            let input = serde_json::json!({ "command": command });
            assert!(refusal("Bash", Some(&input)).is_some(), "{command}");
            assert!(refusal("PowerShell", Some(&input)).is_some(), "{command}");
        }
    }

    #[test]
    fn a_command_that_only_mentions_the_words_is_not_refused() {
        for command in [
            "serve status",
            "serve --db thor.db lookup hook",
            "echo serve hook",
            "git commit -m \"fix the serve hook\"",
            "grep -rn hook serve/src",
            "cargo test -p serve",
            "cargo run --bin serve -- status",
            "ls thor2/serve/src/hook_forge.rs",
            "python - <<'EOF'\n# serve hook\nEOF",
        ] {
            let input = serde_json::json!({ "command": command });
            assert!(refusal("Bash", Some(&input)).is_none(), "{command}");
        }
        let edit = serde_json::json!({ "command": "serve hook" });
        assert!(refusal("Write", Some(&edit)).is_none(), "only a shell is looked at");
    }
}
