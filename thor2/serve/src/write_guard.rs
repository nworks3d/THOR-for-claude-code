//! THE FILES ONLY CLAUDE CODE AND THOR WRITE (always on, no rule in the memory).
//!
//! The owner's words are read from Claude Code's saved conversation, and THOR keeps its
//! own memory, state files and programs in folders of its own. If an assistant could write
//! into those it could forge what the owner said, or change the guard that watches it. So the
//! PreToolUse hook refuses, with nothing done, a call that WRITES to
//!
//! (a) a saved conversation: a `.jsonl` file under the `projects` folder of Claude Code's
//!     folder (the home folder's `.claude`, or `CLAUDE_CONFIG_DIR`);
//! (b) THOR's memory file or any file in the folder it lives in;
//! (c) THOR's own programs (the folder the running program sits in).
//!
//! Covered: the Write, Edit, MultiEdit and NotebookEdit tools, and shell commands (Bash and
//! PowerShell) in which such a path is the TARGET of a write: a redirect into it, tee, cp / mv /
//! Copy-Item / Move-Item onto it (a mv or Move-Item OUT of it removes it too), rm / del /
//! Remove-Item, Set-Content, Add-Content, Out-File, Clear-Content, sed -i, truncate, and
//! `git checkout --` or `git restore` of it. Reading these files, and copying them elsewhere,
//! stays free. Paths compare without regard to case and with either slash, `~` and the usual
//! home variables are expanded, and `..` is resolved. A shell can build a path at run time
//! (a variable, a command substitution, a script that writes it): that cannot be seen here, and
//! this guard does not claim to see it. THOR's own programs and the installer are not hooked,
//! so they are not affected.

use std::path::{Path, PathBuf};

/// The folders and files this guard keeps, already normalised for comparison.
#[derive(Debug, Clone, Default)]
pub struct Protected {
    /// The folder THOR's memory file lives in.
    pub store_dir: Option<String>,
    /// The folder THOR's programs run from.
    pub programs_dir: Option<String>,
    /// Claude Code's folders: where saved conversations are kept (`<dir>/projects`).
    pub claude_dirs: Vec<String>,
    /// (variable name in lower case, value) pairs used to expand a path.
    vars: Vec<(String, String)>,
}

impl Protected {
    /// The real ones: the memory's folder, the running program's folder, Claude Code's folders.
    pub fn current(db: &Path) -> Self {
        let exe_dir = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf));
        let mut claude = Vec::new();
        if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR").filter(|v| !v.is_empty()) {
            claude.push(PathBuf::from(dir));
        }
        let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).map(PathBuf::from);
        if let Some(home) = &home {
            claude.push(home.join(".claude"));
        }
        Self::new(db.parent(), exe_dir.as_deref(), &claude)
    }

    /// Explicit folders (tests, and `current`).
    pub fn new(store_dir: Option<&Path>, programs_dir: Option<&Path>, claude_dirs: &[PathBuf]) -> Self {
        let mut vars = Vec::new();
        for name in ["LOCALAPPDATA", "USERPROFILE", "HOME", "APPDATA"] {
            if let Some(value) = std::env::var_os(name).filter(|v| !v.is_empty()) {
                vars.push((name.to_ascii_lowercase(), fold(&value.to_string_lossy())));
            }
        }
        let keep = |p: &Path| non_empty(fold(&p.to_string_lossy()));
        Protected {
            store_dir: store_dir.and_then(keep),
            programs_dir: programs_dir.and_then(keep),
            claude_dirs: claude_dirs.iter().filter_map(|p| keep(p)).collect(),
            vars,
        }
    }

    /// Whether a path as written in a call is one of the protected files.
    fn protects(&self, written: &str) -> bool {
        let path = self.expand(written);
        if path.is_empty() {
            return false;
        }
        let under = |dir: &str| path == dir || path.starts_with(&format!("{dir}/"));
        if self.programs_dir.as_deref().is_some_and(under) {
            return true;
        }
        if self.store_dir.as_deref().is_some_and(under) {
            let name = path.rsplit('/').next().unwrap_or("");
            // The memory and its state files. A file the owner edits by hand (the reply rules, the
            // work folders, the stale-rule switch, the judge and capture settings) and scripts,
            // logs and notes kept beside them are not state.
            let state = name.contains(".db") || [".json", ".jsonl", ".stamp"].iter().any(|e| name.ends_with(e));
            let by_hand = EDITED_BY_HAND.iter().any(|f| name.starts_with(f));
            if state && !by_hand {
                return true;
            }
        }
        if path.ends_with(".jsonl") {
            if self.claude_dirs.iter().any(|d| path.starts_with(&format!("{d}/projects/"))) {
                return true;
            }
        }
        false
    }

    /// The text of a path with `~` and the home variables written out, then folded.
    fn expand(&self, written: &str) -> String {
        let mut text = written.trim().trim_matches(|c| c == '"' || c == '\'').to_string();
        let lower = text.to_ascii_lowercase();
        if lower == "~" || lower.starts_with("~/") || lower.starts_with("~\\") {
            if let Some((_, home)) = self.vars.iter().find(|(n, _)| n == "userprofile" || n == "home") {
                text = format!("{home}{}", &text[1..]);
            }
        }
        let mut out = fold(&text);
        for (name, value) in &self.vars {
            for form in [format!("%{name}%"), format!("$env:{name}"), format!("${{{name}}}"), format!("${name}")] {
                if out.contains(&form) {
                    out = out.replace(&form, value);
                }
            }
        }
        fold(&out)
    }

    /// Whether anything in a whole command text names a protected path (used for the one-line
    /// .NET and `New-Item` style writes, where the target is not a plain argument).
    fn names_any(&self, text: &str) -> bool {
        let folded = self.expand(text);
        let mut dirs: Vec<&str> = Vec::new();
        dirs.extend(self.store_dir.as_deref());
        dirs.extend(self.programs_dir.as_deref());
        dirs.iter().any(|d| folded.contains(&format!("{d}/")))
            || (folded.contains(".jsonl") && self.claude_dirs.iter().any(|d| folded.contains(&format!("{d}/projects/"))))
    }
}

/// Files in THOR's folder that the owner (or his assistant, on his word) edits by hand.
const EDITED_BY_HAND: [&str; 6] = [
    "guard-response-rulebook.json",
    "guard-capture-rulebook",
    "guard-capture-trigger",
    "guard-judge-config",
    "work-folders.json",
    "stale-guard-mode.json",
];

fn non_empty(s: String) -> Option<String> {
    (!s.is_empty()).then_some(s)
}

/// A path in comparison form: lower case, forward slashes, `/c/x` as `c:/x`, no `.` or `..`
/// segments, no repeated slashes, no trailing slash.
fn fold(path: &str) -> String {
    let mut p = path.replace('\\', "/").to_ascii_lowercase();
    let bytes = p.as_bytes();
    if bytes.len() >= 3 && bytes[0] == b'/' && bytes[1].is_ascii_alphabetic() && bytes[2] == b'/' {
        p = format!("{}:{}", bytes[1] as char, &p[2..]);
    }
    let absolute = p.starts_with('/');
    let mut parts: Vec<&str> = Vec::new();
    for seg in p.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                if parts.last().is_some_and(|l| !l.ends_with(':') && *l != "..") {
                    parts.pop();
                } else if !absolute {
                    parts.push("..");
                }
            }
            s => parts.push(s),
        }
    }
    let joined = parts.join("/");
    if absolute { format!("/{joined}") } else { joined }
}

/// The one plain refusal.
fn refusal_text() -> String {
    "[THOR] That would change a file that only Claude Code and THOR write (a saved conversation, THOR's memory or its \
     own files, or THOR's programs). These files are not edited by hand or by a command, and nothing was done. \
     Reading them, or copying them somewhere else, is fine."
        .to_string()
}

/// The hook's refusal of a call that writes to a protected file, or `None`.
pub fn refusal(protected: &Protected, tool_name: &str, tool_input: Option<&serde_json::Value>) -> Option<String> {
    let input = tool_input?;
    let hit = match tool_name {
        "Edit" | "Write" | "MultiEdit" | "NotebookEdit" => ["file_path", "notebook_path", "path"]
            .iter()
            .any(|k| input.get(k).and_then(|v| v.as_str()).is_some_and(|p| protected.protects(p))),
        "Bash" | "PowerShell" => {
            input.get("command").and_then(|v| v.as_str()).is_some_and(|c| command_writes_protected(protected, c, 0))
        }
        _ => false,
    };
    hit.then(refusal_text)
}

/// One word of a command, or the marker of a redirect.
#[derive(Debug, PartialEq, Eq)]
enum Tok {
    Word(String),
    /// `>`, `>>` or `>|`: the next word is written to.
    Redirect,
    /// A segment border: `;`, `&&`, `||`, `|`, `&` or a new line.
    Border,
}

fn tokenize(command: &str) -> Vec<Tok> {
    let chars: Vec<char> = command.chars().collect();
    let mut out = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut i = 0;
    let flush = |word: &mut String, in_word: &mut bool, out: &mut Vec<Tok>| {
        if *in_word {
            out.push(Tok::Word(std::mem::take(word)));
            *in_word = false;
        }
    };
    while i < chars.len() {
        let c = chars[i];
        match c {
            '\'' | '"' => {
                in_word = true;
                i += 1;
                while i < chars.len() && chars[i] != c {
                    word.push(chars[i]);
                    i += 1;
                }
            }
            '`' if i + 1 < chars.len() => {
                in_word = true;
                i += 1;
                word.push(chars[i]);
            }
            c if c.is_whitespace() && c != '\n' => flush(&mut word, &mut in_word, &mut out),
            '\n' | ';' | '|' | '&' => {
                // `>&` and `2>&1` are handled at the `>`; a lone `&` or `&&` is a border.
                flush(&mut word, &mut in_word, &mut out);
                while i + 1 < chars.len() && matches!(chars[i + 1], '|' | '&') && c != '\n' && c != ';' {
                    i += 1;
                }
                out.push(Tok::Border);
            }
            '>' => {
                // A file descriptor number right before it (`2>`) is not a word.
                if in_word && word.chars().all(|d| d.is_ascii_digit()) {
                    word.clear();
                    in_word = false;
                } else {
                    flush(&mut word, &mut in_word, &mut out);
                }
                while i + 1 < chars.len() && chars[i + 1] == '>' {
                    i += 1;
                }
                if i + 1 < chars.len() && chars[i + 1] == '&' {
                    // `>&2`, `>&1`: a copy of a descriptor, no file.
                    i += 1;
                    while i + 1 < chars.len() && (chars[i + 1].is_ascii_digit() || chars[i + 1] == '-') {
                        i += 1;
                    }
                } else {
                    if i + 1 < chars.len() && chars[i + 1] == '|' {
                        i += 1;
                    }
                    out.push(Tok::Redirect);
                }
            }
            c => {
                in_word = true;
                word.push(c);
            }
        }
        i += 1;
    }
    flush(&mut word, &mut in_word, &mut out);
    out
}

/// The command word as compared: the file name, lower case, with no `.exe`.
fn command_name(word: &str) -> String {
    let name = word.replace('\\', "/");
    let name = name.rsplit('/').next().unwrap_or(&name).to_ascii_lowercase();
    name.strip_suffix(".exe").map(str::to_string).unwrap_or(name)
}

const SKIPPED_LEADERS: [&str; 10] = ["sudo", "env", "nohup", "command", "builtin", "exec", "time", "call", "start", "&"];
const SHELLS: [&str; 8] = ["bash", "sh", "zsh", "powershell", "pwsh", "cmd", "dash", "ksh"];
const DOTNET_WRITES: [&str; 8] =
    ["io.file]::write", "io.file]::append", "io.file]::delete", "io.file]::move", "io.file]::create", "io.file]::open", "streamwriter", "filestream"];

fn command_writes_protected(p: &Protected, command: &str, depth: usize) -> bool {
    let mut segment: Vec<String> = Vec::new();
    let mut redirected = false;
    let mut segments: Vec<(Vec<String>, Vec<String>)> = Vec::new();
    let mut targets: Vec<String> = Vec::new();
    for tok in tokenize(command) {
        match tok {
            Tok::Word(w) => {
                if redirected {
                    targets.push(w);
                    redirected = false;
                } else {
                    segment.push(w);
                }
            }
            Tok::Redirect => redirected = true,
            Tok::Border => {
                segments.push((std::mem::take(&mut segment), std::mem::take(&mut targets)));
                redirected = false;
            }
        }
    }
    segments.push((segment, targets));
    for (words, redirects) in &segments {
        if redirects.iter().any(|t| p.protects(t)) {
            return true;
        }
        if segment_writes_protected(p, words, depth) {
            return true;
        }
    }
    false
}

fn segment_writes_protected(p: &Protected, words: &[String], depth: usize) -> bool {
    let mut at = 0;
    while at < words.len() {
        let name = command_name(&words[at]);
        let is_assignment = words[at].contains('=') && !words[at].starts_with('-') && !words[at].contains('/') && !words[at].contains('\\');
        if SKIPPED_LEADERS.contains(&name.as_str()) || is_assignment {
            at += 1;
        } else {
            break;
        }
    }
    let Some(leader) = words.get(at) else { return false };
    let name = command_name(leader);
    let rest = &words[at + 1..];
    let flag = |w: &String| w.starts_with('-') || (w.starts_with('/') && w.len() <= 3 && !w.contains('.'));
    let positionals: Vec<&String> = rest.iter().filter(|w| !flag(w)).collect();
    let any = |items: &[&String]| items.iter().any(|w| p.protects(w));
    match name.as_str() {
        n if SHELLS.contains(&n) => {
            // `bash -c '...'`, `powershell -Command "..."`: read the quoted program too.
            depth < 3 && rest.iter().any(|w| w.contains(' ') && command_writes_protected(p, w, depth + 1))
        }
        "rm" | "del" | "erase" | "rmdir" | "rd" | "remove-item" | "ri" | "tee" | "truncate" | "unlink" | "shred" | "mv"
        | "move" | "move-item" | "mi" | "rename-item" | "ren" | "rni" => any(&positionals),
        "set-content" | "sc" | "add-content" | "ac" | "clear-content" | "clc" | "out-file" => {
            // The text being written (`-Value`) is data, not a target.
            let mut targets = Vec::new();
            let mut skip = false;
            for w in rest {
                if skip {
                    skip = false;
                } else if matches!(w.to_ascii_lowercase().as_str(), "-value" | "-inputobject") {
                    skip = true;
                } else if !flag(w) {
                    targets.push(w);
                }
            }
            any(&targets)
        }
        "cp" | "copy" | "copy-item" | "cpi" | "xcopy" | "robocopy" => {
            // Only the DESTINATION is written: the one named by -Destination or -t, else the last.
            let named = rest.iter().position(|w| matches!(w.to_ascii_lowercase().as_str(), "-destination" | "-t" | "--target-directory"));
            match named {
                Some(i) => rest.get(i + 1).is_some_and(|w| p.protects(w)),
                None => positionals.last().is_some_and(|w| p.protects(w)),
            }
        }
        "sed" => rest.iter().any(|w| w == "-i" || (w.starts_with("-i") && !w.starts_with("--")) || w == "--in-place") && any(&positionals),
        "git" => {
            let sub = positionals.first().map(|w| w.to_ascii_lowercase());
            match sub.as_deref() {
                Some("checkout") => match rest.iter().position(|w| w == "--") {
                    Some(i) => rest[i + 1..].iter().any(|w| p.protects(w)),
                    None => false,
                },
                Some("restore") => any(&positionals[1..]),
                _ => false,
            }
        }
        _ => {
            // A one-line .NET or `New-Item` write names its target inside a longer argument.
            let whole = words[at..].join(" ").to_ascii_lowercase();
            (DOTNET_WRITES.iter().any(|m| whole.contains(m)) || name == "new-item" || name == "set-item")
                && p.names_any(&words[at..].join(" "))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn guard() -> Protected {
        Protected::new(
            Some(Path::new("C:/data/store")),
            Some(Path::new("C:/tools/thor2/bin")),
            &[PathBuf::from("C:/Users/alex/.claude")],
        )
    }

    fn shell(command: &str) -> bool {
        let input = serde_json::json!({ "command": command });
        refusal(&guard(), "Bash", Some(&input)).is_some()
    }

    #[test]
    fn the_file_tools_are_refused_on_protected_files_whatever_the_case_or_slash() {
        let g = guard();
        for path in [
            "C:\\Users\\Alex\\.claude\\projects\\C--work-acme-shop\\abc.JSONL",
            "c:/users/alex/.claude/projects/x/sub/agent-1.jsonl",
            "C:/Data/Store/thor.db",
            "C:\\Data\\Store\\unpin-owner-go.json",
            "C:/data/store/../store/session-watermark.json",
            "C:/Tools/THOR2/bin/serve.exe",
        ] {
            for tool in ["Write", "Edit", "MultiEdit"] {
                let input = serde_json::json!({ "file_path": path });
                assert!(refusal(&g, tool, Some(&input)).is_some(), "{tool} {path}");
            }
        }
        let note = serde_json::json!({ "notebook_path": "C:/data/store/x.json" });
        assert!(refusal(&g, "NotebookEdit", Some(&note)).is_some());
        for path in ["C:/work/acme-shop/notes.md", "C:/Users/alex/.claude/settings.json", "C:/data/store2/thor.db", "C:/Users/alex/.claude/projects/x/notes.txt"] {
            let input = serde_json::json!({ "file_path": path });
            assert!(refusal(&g, "Write", Some(&input)).is_none(), "{path}");
        }
        let read = serde_json::json!({ "file_path": "C:/data/store/thor.db" });
        assert!(refusal(&g, "Read", Some(&read)).is_none(), "reading is free");
    }

    #[test]
    fn shell_writes_into_protected_files_are_refused() {
        for command in [
            "echo '{}' >> C:/Users/alex/.claude/projects/p/s.jsonl",
            "echo x > /c/Users/alex/.claude/projects/p/s.jsonl",
            "cat a | tee C:\\data\\store\\unpin-owner-go.json",
            "cp forged.json C:/data/store/unpin-owner-go.json",
            "mv forged.jsonl C:/Users/alex/.claude/projects/p/s.jsonl",
            "Copy-Item forged.json -Destination C:\\data\\store\\thor.db",
            "Move-Item C:\\data\\store\\thor.db C:\\elsewhere",
            "rm -f C:/data/store/thor.db-wal",
            "Remove-Item C:\\data\\store\\thor.db",
            "del C:\\tools\\thor2\\bin\\serve.exe",
            "Set-Content -Path C:\\data\\store\\unpin-owner-go.json -Value '[]'",
            "Add-Content C:/Users/alex/.claude/projects/p/s.jsonl '{\"type\":\"user\"}'",
            "'x' | Out-File -FilePath C:\\data\\store\\a.json",
            "Clear-Content C:/data/store/a.json",
            "sed -i 's/a/b/' C:/data/store/a.json",
            "truncate -s 0 C:/data/store/thor.db",
            "git checkout -- C:/data/store/a.json",
            "git restore C:/data/store/a.json",
            "bash -c 'echo x > C:/data/store/a.json'",
            "powershell -Command \"Add-Content C:/data/store/a.json x\"",
            "[System.IO.File]::WriteAllText('C:\\data\\store\\a.json','x')",
            "cd C:/work && echo x>C:/data/store/a.json",
        ] {
            assert!(shell(command), "{command}");
        }
    }

    /// Not run by default: replays the real-call record of a folder (THOR_REPLAY_DIR holds a
    /// real-calls.jsonl, THOR_REPLAY_STORE and THOR_REPLAY_PROGRAMS name the folders to protect,
    /// THOR_REPLAY_CLAUDE Claude Code's folder) and lists every call this guard would refuse.
    #[test]
    #[ignore]
    fn replay_the_real_calls() {
        let var = |n: &str| std::env::var(n).unwrap_or_default();
        let dir = PathBuf::from(var("THOR_REPLAY_DIR"));
        let g = Protected::new(
            Some(Path::new(&var("THOR_REPLAY_STORE"))),
            Some(Path::new(&var("THOR_REPLAY_PROGRAMS"))),
            &[PathBuf::from(var("THOR_REPLAY_CLAUDE"))],
        );
        let calls = crate::corpus::load_calls(&dir.join("thor.db"), crate::time::now_unix() - 5 * 86_400);
        let (mut total, mut refused, mut refused_ok) = (0, 0, 0);
        for call in &calls {
            total += 1;
            if refusal(&g, &call.tool, Some(&call.input)).is_some() {
                refused += 1;
                if call.ok == Some(true) {
                    refused_ok += 1;
                }
                println!("REFUSED ok={:?} {} {}", call.ok, call.tool, call.input.to_string().chars().take(900).collect::<String>());
            }
        }
        println!("calls {total}, refused {refused}, of which ran fine {refused_ok}");
    }

    #[test]
    fn reading_copying_away_and_ordinary_commands_are_free() {
        for command in [
            "cat C:/data/store/thor.db",
            "cp C:/data/store/thor.db C:/scratch/copy.db",
            "Copy-Item C:\\data\\store\\thor.db -Destination C:\\scratch\\thor.db",
            "cp C:/data/store/thor.db C:/data/store/thor.db-wal C:/data/store/thor.db-shm /tmp/copy/",
            "ls C:/data/store > C:/scratch/listing.txt",
            "grep -c user C:/Users/alex/.claude/projects/p/s.jsonl",
            "echo x > C:/work/acme-shop/out.txt",
            "git checkout -b fix && git restore src/main.rs",
            "sed -n '1,5p' C:/data/store/a.json",
            "cargo build 2>&1 | tail -3",
            "echo done 2>&1 >/dev/null",
            "C:/tools/thor2/bin/serve.exe --db C:/data/store/thor.db doctor",
            "rm -rf C:/scratch/old",
            "echo 'mention C:/data/store/thor.db in a note' > C:/scratch/n.txt",
        ] {
            assert!(!shell(command), "{command}");
        }
    }
}
