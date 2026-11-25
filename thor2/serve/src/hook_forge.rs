//! NO SHELL COMMAND RUNS THOR'S HOOK (2026-11-25).
//!
//! THOR's UserPromptSubmit hook writes the record of what the owner typed (`prompt_record`), and
//! every check that accepts the owner's own words trusts that record. The hook is THOR's program
//! `serve` run with the `hook` subcommand and a payload on its standard input, so a shell
//! command that runs it by hand with an invented payload would write a message the owner never
//! typed. The PreToolUse hook therefore refuses a Bash or PowerShell command that runs `serve`
//! with `hook` (also through `bash -c`, `cmd /c`, `Start-Process`, `cargo run -- hook`, the
//! wrappers `timeout`, `env` (with `-i` and `NAME=value`), `nice`, `stdbuf`, `time`, `sudo`, `doas`,
//! `nohup`, `exec`, `xargs` and `find -exec`, and `[Diagnostics.Process]::Start`, with or without
//! `.exe`, a folder in front, quotes or escapes inside the words).
//!
//! A hand run with `--no-record` (writes nothing at all), `--diagnose` (a probe: it never writes the
//! owner's prompt record, starts an evaluation or moves the setting-file record), `--help` or
//! `--version` is not refused. A hand run without one of them is, whatever event its payload holds:
//! the payload comes from a file or a pipe and is not seen here, and only the typed-prompt event
//! writes the owner's record.
//!
//! The program that runs has to be THOR's `serve` itself: `serve` as a cargo package name
//! (`cargo test -p serve hook`), a search word (`grep serve hook`) or another program's name
//! (`npx serve`) is not it, and is never refused.
//!
//! HONESTLY NOT SEEN: a script file that does it (the command only names the script), a copy of
//! the program under another name, a variable that holds the program, and a program that builds the
//! words at run time. Those are
//! covered by the rest of the wall (the memory's folder cannot be written by a call that is seen)
//! and by transparency, not by this check.

use crate::write_guard::{command_name, command_segments, SHELLS};

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

/// Flags that make a hand run of the hook a probe that writes nothing that counts as evidence:
/// `--no-record` (writes nothing at all), `--diagnose` (never writes the owner's prompt record, an
/// evaluation start or a move of the setting-file record), and the calls that only ask for help or the
/// version (AGENTS.md: such a call is never refused).
const PROBE_FLAGS: [&str; 6] = ["--no-record", "--diagnose", "--help", "-h", "--version", "-v"];

/// Does a word among these carry one of the probe flags (the words may hold several arguments)?
fn is_a_probe(words: &[String]) -> bool {
    words.iter().flat_map(|w| w.split_whitespace()).any(|w| PROBE_FLAGS.contains(&bare(w).as_str()))
}

/// Programs that only carry another program and its words along: what stands between them and the
/// program they run is flags, their values, numbers (a time limit, a niceness) and `NAME=value`.
const WRAPPERS: [&str; 22] = [
    "timeout", "env", "nice", "ionice", "stdbuf", "time", "sudo", "doas", "nohup", "exec", "command", "builtin", "call", "setsid",
    "start", "start-process", "saps", "invoke-command", "icm", "invoke-expression", "iex", "xargs",
];

/// Flags of the wrappers that take the next word as their value (`sudo -u bob`, `timeout -s KILL`).
const VALUE_FLAGS: [&str; 12] = ["-u", "-g", "-s", "-k", "-n", "-c", "-p", "-C", "--user", "--group", "--signal", "--kill-after"];

/// A time limit or a count: `5`, `5s`, `10m`, `1.5h`.
fn is_a_number_or_duration(word: &str) -> bool {
    let body = word.trim_end_matches(|c: char| matches!(c, 's' | 'm' | 'h' | 'd'));
    !body.is_empty() && body.chars().all(|c| c.is_ascii_digit() || c == '.')
}

/// The words as plain tokens (a program name, a flag value), for a one-line .NET call.
fn tokens(text: &str) -> Vec<String> {
    text.split(|c: char| !(c.is_alphanumeric() || matches!(c, '.' | '-' | '_')))
        .filter(|t| !t.is_empty())
        .map(|t| t.to_ascii_lowercase())
        .collect()
}

fn segment_runs_the_hook(words: &[String], depth: usize) -> bool {
    // `[Diagnostics.Process]::Start('serve.exe', '--db x hook')`: the names are inside one argument.
    let joined = words.join(" ");
    if joined.to_ascii_lowercase().contains("process]::start") {
        let toks = tokens(&joined);
        if toks.iter().any(|t| t == "serve" || t == "serve.exe")
            && toks.iter().any(|t| t == "hook")
            && !toks.iter().any(|t| PROBE_FLAGS.contains(&t.as_str()))
        {
            return true;
        }
    }
    let mut via_xargs = false;
    let mut wrapped = false;
    let mut after_flag = false;
    for (i, word) in words.iter().enumerate() {
        let unquoted: String = word.chars().filter(|c| !matches!(c, '`' | '\'' | '"')).collect();
        let name = command_name(&unquoted);
        let rest = &words[i + 1..];
        if is_serve(word) {
            // The program that runs is THOR's own `serve`. Through `xargs` the subcommand may come
            // from standard input, so a missing or unreadable one counts.
            let sub = subcommand_of(rest);
            return match sub.as_deref() {
                Some("hook") => !is_a_probe(rest),
                Some(s) => via_xargs && (s.starts_with('{') || s.starts_with('$') || s.starts_with('%')),
                None => via_xargs,
            };
        }
        match name.as_str() {
            "find" => {
                // `find . -exec serve.exe --db x hook {} \;`
                return (0..rest.len()).any(|j| {
                    matches!(rest[j].as_str(), "-exec" | "-execdir" | "-ok" | "-okdir")
                        && rest.get(j + 1).is_some_and(|p| is_serve(p))
                        && subcommand_of(&rest[j + 2..]).as_deref() == Some("hook")
                        && !is_a_probe(&rest[j + 2..])
                });
            }
            "cargo" => {
                // Only `cargo run` can run THOR's serve, and only what follows `--` goes to it. A test
                // filter (`cargo test -p serve hook`) or a build never runs the hook.
                let runs = rest.iter().find(|w| !w.starts_with('-') && !w.starts_with('+')).is_some_and(|w| matches!(w.as_str(), "run" | "r"));
                let dashes = rest.iter().position(|w| w == "--");
                let names_serve = rest.iter().take(dashes.unwrap_or(rest.len())).any(|w| is_serve(w));
                return runs
                    && names_serve
                    && dashes.is_some_and(|d| subcommand_of(&rest[d + 1..]).as_deref() == Some("hook") && !is_a_probe(&rest[d + 1..]));
            }
            _ => {}
        }
        if SHELLS.contains(&name.as_str()) || WRAPPERS.contains(&name.as_str()) {
            wrapped = true;
            via_xargs |= name == "xargs";
            after_flag = false;
            continue;
        }
        if word.starts_with('-') || word.starts_with('/') && word.len() <= 3 {
            after_flag = VALUE_FLAGS.contains(&word.as_str());
            continue;
        }
        if is_a_number_or_duration(word) || (word.contains('=') && !word.contains(' ')) || after_flag {
            after_flag = false;
            continue;
        }
        // Another program: it is not the one the wrapper runs.
        if wrapped && depth < 3 {
            // `bash -c "serve hook"`: a word that is a whole command of its own.
            return words[i..].iter().filter(|w| w.contains(' ')).any(|w| command_text_runs_the_hook(w, depth + 1));
        }
        return false;
    }
    // Nothing but wrappers, flags and numbers: look inside the words that hold a whole command.
    wrapped && depth < 3 && words.iter().filter(|w| w.contains(' ')).any(|w| command_text_runs_the_hook(w, depth + 1))
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
            "serve.exe --db x hook --actor probe",
            "serve.exe hook < payload.json",
            "cargo run -p serve --release -- --db x hook",
            "timeout 5 serve.exe --db x hook",
            "timeout -s KILL 5s serve --db x hook",
            "env -i serve.exe --db x hook",
            "env -i FOO=1 BAR=2 serve.exe --db x hook",
            "nice -n 10 serve hook",
            "stdbuf -oL serve hook",
            "time serve hook",
            "doas serve hook",
            "sudo -u bob serve hook",
            "nohup serve hook",
            "exec serve hook",
            "echo hook | xargs serve.exe --db x",
            "echo hook | xargs -n1 serve.exe --db x",
            "find . -exec serve.exe --db x hook {} \\;",
            "find . -execdir serve --db x hook {} +",
            "[Diagnostics.Process]::Start('serve.exe', '--db x hook')",
            "[System.Diagnostics.Process]::Start(\"C:\\t\\serve.exe\", \"hook\")",
            "timeout 5 bash -c 'serve hook'",
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
            // A probe writes nothing that counts as evidence; a call that only asks for help or the version
            // is never refused.
            "serve.exe hook --help",
            "serve.exe --db x hook --help",
            "serve hook --version",
            "serve.exe --db x hook --no-record",
            "echo {} | serve.exe --db x hook --diagnose",
            "serve.exe --db x hook --no-record --actor probe",
            "timeout 5 serve.exe --db x hook --no-record",
            "find . -exec serve.exe --db x hook --diagnose {} \\;",
            "cargo run --bin serve -- hook --no-record",
            "[Diagnostics.Process]::Start('serve.exe', '--db x hook --no-record')",
            // `serve` is a package name, a test filter or a search word, not THOR's program.
            "cargo test -p serve hook",
            "cargo test --release --features semantic -p serve -- hook",
            "cargo build -p serve --bin serve",
            "cargo clippy -p serve -- hook",
            "cargo run -p ops --bin doctor -- hook",
            "cargo run --bin serve -- status",
            "npx serve hook",
            "npx serve -s hook",
            "grep serve hook",
            "timeout 5 grep serve hook",
            "echo hook | grep serve",
            "echo hook | xargs grep serve",
            "xargs -n1 serve.exe --db x status",
            "env -i grep -rn serve hook",
            "nice -n 5 cargo test -p serve hook",
            "find . -name serve -exec grep hook {} \\;",
            "find . -exec serve.exe --db x status {} \\;",
            "sudo systemctl restart serve hook",
            "[Diagnostics.Process]::Start('notepad.exe', 'hook')",
        ] {
            let input = serde_json::json!({ "command": command });
            assert!(refusal("Bash", Some(&input)).is_none(), "{command}");
        }
        let edit = serde_json::json!({ "command": "serve hook" });
        assert!(refusal("Write", Some(&edit)).is_none(), "only a shell is looked at");
    }
}
