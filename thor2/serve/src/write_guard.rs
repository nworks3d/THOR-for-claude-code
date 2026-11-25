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

mod interp;

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
        // A hook may start with only one of the two home variables set: each stands for the other.
        for (from, to) in [("userprofile", "home"), ("home", "userprofile")] {
            if !vars.iter().any(|(n, _)| n == to) {
                if let Some(value) = vars.iter().find(|(n, _)| n == from).map(|(_, v)| v.clone()) {
                    vars.push((to.to_string(), value));
                }
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

    /// The folders where saved conversations are kept.
    fn projects_dirs(&self) -> Vec<String> {
        self.claude_dirs.iter().map(|d| format!("{d}/projects")).collect()
    }

    /// The protected folders themselves.
    fn protected_dirs(&self) -> Vec<String> {
        let mut dirs: Vec<String> = Vec::new();
        dirs.extend(self.store_dir.clone());
        dirs.extend(self.programs_dir.clone());
        dirs.extend(self.projects_dirs());
        dirs
    }

    /// A path as written in a call, in comparison form. A relative path is read against the
    /// folder the command works in (`cwd`) when that is known.
    fn resolve(&self, written: &str, cwd: Option<&str>) -> String {
        let path = self.expand(written);
        if path.is_empty() || is_absolute(&path) {
            return path;
        }
        match cwd {
            Some(dir) if !path.starts_with('$') && !path.starts_with('%') => fold(&format!("{dir}/{path}")),
            _ => path,
        }
    }

    /// Whether a removal or move of this path takes a protected folder with it: the folder
    /// itself, or a folder that holds it.
    fn removal_protects(&self, written: &str, cwd: Option<&str>) -> bool {
        if self.protects(written, cwd) {
            return true;
        }
        let path = self.resolve(written, cwd);
        if path.is_empty() || !is_absolute(&path) || path.ends_with(':') {
            return false;
        }
        self.protected_dirs().iter().any(|dir| *dir == path || dir.starts_with(&format!("{path}/")))
    }

    /// Whether a path is a protected folder, or inside one (a link to it would open it up).
    fn touches_dir(&self, written: &str, cwd: Option<&str>) -> bool {
        let path = self.resolve(written, cwd);
        !path.is_empty() && self.protected_dirs().iter().any(|dir| path == *dir || path.starts_with(&format!("{dir}/")))
    }

    /// Whether a path as written in a call is one of the protected files.
    fn protects(&self, written: &str, cwd: Option<&str>) -> bool {
        let path = self.resolve(written, cwd);
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
    /// Whether a text mentions a protected folder or file at all, with a trailing slash or not.
    fn mentions_any(&self, text: &str) -> bool {
        let folded = self.expand(text);
        self.protected_dirs().iter().any(|d| folded.contains(d.as_str()))
    }

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
const EDITED_BY_HAND: [&str; 9] = [
    "guard-response-rulebook.json",
    "guard-capture-rulebook",
    "guard-capture-trigger",
    "guard-capture-mode",
    "guard-judge-config",
    "work-folders.json",
    "stale-guard-mode.json",
    "no-project-ask-mode.json",
    "thor-mode.json",
];

/// Whether a comparison-form path starts at a root.
fn is_absolute(path: &str) -> bool {
    path.starts_with('/') || path.as_bytes().get(1) == Some(&b':')
}

fn non_empty(s: String) -> Option<String> {
    (!s.is_empty()).then_some(s)
}

/// A path in comparison form: lower case, forward slashes, `/c/x` as `c:/x`, no `.` or `..`
/// segments, no repeated slashes, no trailing slash.
fn fold(path: &str) -> String {
    let mut p = path.replace('\\', "/").to_ascii_lowercase();
    // The long-path forms `\\?\C:\x` and `\\.\C:\x` name the same file as `C:\x`.
    for prefix in ["//?/unc/", "//?/", "//./"] {
        if let Some(rest) = p.strip_prefix(prefix) {
            p = if prefix.ends_with("unc/") { format!("//{rest}") } else { rest.to_string() };
            break;
        }
    }
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
     own files, or THOR's programs), so nothing was done. Do not look for another way to write it. Reading these files, \
     or copying them somewhere else, is fine. To change one of THOR's settings use the matching option of the install \
     program (its --help lists them). If the change is really needed, tell the person in one plain sentence what and \
     why, and let them do it themselves."
        .to_string()
}

/// The hook's refusal of a call that writes to a protected file, or `None`.
///
/// `cwd` is the folder the call works in: a relative path is read against it, and a `cd` inside a
/// command moves it.
pub fn refusal(
    protected: &Protected,
    tool_name: &str,
    tool_input: Option<&serde_json::Value>,
    cwd: Option<&str>,
) -> Option<String> {
    let input = tool_input?;
    let cwd = cwd.map(fold).filter(|c| is_absolute(c));
    let cwd = cwd.as_deref();
    let hit = match tool_name {
        "Edit" | "Write" | "MultiEdit" | "NotebookEdit" => ["file_path", "notebook_path", "path"]
            .iter()
            .any(|k| input.get(k).and_then(|v| v.as_str()).is_some_and(|p| protected.protects(p, cwd))),
        "Bash" | "PowerShell" => input
            .get("command")
            .and_then(|v| v.as_str())
            .is_some_and(|c| command_writes_protected(protected, c, cwd, 0)),
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

const MOVERS_OF_THE_WORKING_FOLDER: [&str; 5] = ["cd", "chdir", "pushd", "set-location", "sl"];
const INTERPRETERS: &[&str] =
    &["python", "python3", "py", "pythonw", "node", "nodejs", "deno", "bun", "perl", "ruby", "php", "awk", "gawk", "lua"];
const SQL_WRITES: &[&str] =
    &["insert ", "update ", "delete ", "drop ", "alter ", "create ", "vacuum", "replace ", ".restore", ".import", ".clone", "pragma "];
/// Flags of the installer: a word `install` that carries one is THOR's program, not the Unix copy command.
const INSTALLER_FLAGS: &[&str] = &["--db", "--set-", "--topic", "--uninstall", "--register-root", "--tidy", "--copy-from", "--project"];

fn command_writes_protected(p: &Protected, command: &str, cwd: Option<&str>, depth: usize) -> bool {
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
    // The folder the command works in moves with every `cd`, so a relative path after it is read
    // against the right folder.
    let mut here: Option<String> = cwd.map(str::to_string);
    for (words, redirects) in &segments {
        if redirects.iter().any(|t| p.protects(t, here.as_deref())) {
            return true;
        }
        if segment_writes_protected(p, words, here.as_deref(), depth, command) {
            return true;
        }
        if let Some((name, rest)) = leader_of(words) {
            if MOVERS_OF_THE_WORKING_FOLDER.contains(&name.as_str()) {
                let target = rest.iter().find(|w| !w.starts_with('-') && !(w.starts_with('/') && w.len() <= 3));
                if let Some(target) = target {
                    let moved = p.resolve(target, here.as_deref());
                    if is_absolute(&moved) {
                        here = Some(moved);
                    }
                }
            }
        }
    }
    false
}

/// The command word of a segment (after `sudo`, `env`, assignments and the like) and what follows it.
fn leader_of(words: &[String]) -> Option<(String, &[String])> {
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
    let leader = words.get(at)?;
    Some((command_name(leader), &words[at + 1..]))
}

fn base_name(path: &str) -> String {
    let path = path.replace('\\', "/");
    path.trim_end_matches('/').rsplit('/').next().unwrap_or("").to_string()
}

/// Whether copying or moving `sources` to `dest` writes a protected file: either `dest` is one, or
/// it is a folder that would receive a file with the name of one.
fn dest_receives(p: &Protected, dest: &str, sources: &[&String], cwd: Option<&str>) -> bool {
    if p.protects(dest, cwd) {
        return true;
    }
    let folder = dest.trim_end_matches(|c| c == '/' || c == '\\');
    sources.iter().any(|s| p.protects(&format!("{folder}/{}", base_name(s)), cwd))
}

fn segment_writes_protected(p: &Protected, words: &[String], cwd: Option<&str>, depth: usize, source: &str) -> bool {
    let Some((name, rest)) = leader_of(words) else { return false };
    let at = words.len() - rest.len() - 1;
    let flag = |w: &String| w.starts_with('-') || (w.starts_with('/') && w.len() <= 3 && !w.contains('.'));
    let positionals: Vec<&String> = rest.iter().filter(|w| !flag(w)).collect();
    let any = |items: &[&String]| items.iter().any(|w| p.protects(w, cwd));
    let after_flag = |names: &[&str]| -> Vec<&String> {
        let mut found = Vec::new();
        for (i, w) in rest.iter().enumerate() {
            let lower = w.to_ascii_lowercase();
            if names.contains(&lower.as_str()) {
                if let Some(next) = rest.get(i + 1) {
                    found.push(next);
                }
            }
        }
        found
    };
    match name.as_str() {
        n if SHELLS.contains(&n) => {
            // `bash -c '...'`, `powershell -Command "..."`: read the quoted program too.
            // `cmd /c mklink ...` and `bash -c cp ...` carry the command as plain words.
            let inline = rest
                .iter()
                .position(|w| matches!(w.to_ascii_lowercase().as_str(), "/c" | "/k" | "-c" | "-lc" | "-command" | "-c:"))
                .is_some_and(|i| segment_writes_protected(p, &rest[i + 1..], cwd, depth + 1, source));
            depth < 3 && (inline || rest.iter().any(|w| w.contains(' ') && command_writes_protected(p, w, cwd, depth + 1)))
        }
        "rm" | "del" | "erase" | "rmdir" | "rd" | "remove-item" | "ri" | "unlink" | "shred" => {
            positionals.iter().any(|w| p.removal_protects(w, cwd))
        }
        "tee" | "truncate" => any(&positionals),
        "mv" | "move" | "move-item" | "mi" | "rename-item" | "ren" | "rni" => {
            // The source is removed and the destination is written.
            let named = after_flag(&["-destination"]);
            let (sources, dest): (Vec<&String>, Option<&String>) = match (named.first(), positionals.split_last()) {
                (Some(d), _) => (positionals.iter().copied().filter(|w| w != d).collect(), Some(*d)),
                (None, Some((last, init))) => (init.to_vec(), Some(*last)),
                (None, None) => (Vec::new(), None),
            };
            positionals.iter().any(|w| p.removal_protects(w, cwd)) || dest.is_some_and(|d| dest_receives(p, d, &sources, cwd))
        }
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
            // Only the DESTINATION is written: the one named by -Destination or -t, else the last
            // (the second for xcopy and robocopy). A folder that receives a file named like a
            // protected one counts.
            let named = after_flag(&["-destination", "-t", "--target-directory"]);
            let (sources, dest): (Vec<&String>, Option<&String>) = match named.first() {
                Some(d) => (positionals.iter().copied().filter(|w| w != d).collect(), Some(*d)),
                None if matches!(name.as_str(), "xcopy" | "robocopy") => (positionals.iter().take(1).copied().collect(), positionals.get(1).copied()),
                None => match positionals.split_last() {
                    Some((last, init)) => (init.to_vec(), Some(*last)),
                    None => (Vec::new(), None),
                },
            };
            dest.is_some_and(|d| dest_receives(p, d, &sources, cwd))
        }
        "install" if is_the_unix_install(&words[at], rest) => {
            let named = after_flag(&["-t", "--target-directory"]);
            let (sources, dest): (Vec<&String>, Option<&String>) = match named.first() {
                Some(d) => (positionals.iter().copied().filter(|w| w != d).collect(), Some(*d)),
                None => match positionals.split_last() {
                    Some((last, init)) => (init.to_vec(), Some(*last)),
                    None => (Vec::new(), None),
                },
            };
            dest.is_some_and(|d| dest_receives(p, d, &sources, cwd))
        }
        "dd" => rest.iter().any(|w| w.to_ascii_lowercase().strip_prefix("of=").is_some_and(|v| p.protects(v, cwd))),
        "curl" => {
            let targets = after_flag(&["-o", "--output"]);
            let direct = rest.iter().any(|w| w.strip_prefix("--output=").is_some_and(|v| p.protects(v, cwd)));
            direct || any(&targets) || after_flag(&["--output-dir"]).iter().any(|w| p.touches_dir(w, cwd))
        }
        "wget" => {
            let direct = rest.iter().any(|w| w.strip_prefix("--output-document=").is_some_and(|v| p.protects(v, cwd)));
            direct
                || any(&after_flag(&["-o", "--output-document", "--output-file"]))
                || after_flag(&["-p", "--directory-prefix"]).iter().any(|w| p.touches_dir(w, cwd))
        }
        "sed" => rest.iter().any(|w| w == "-i" || (w.starts_with("-i") && !w.starts_with("--")) || w == "--in-place") && any(&positionals),
        "git" => {
            let sub = positionals.first().map(|w| w.to_ascii_lowercase());
            match sub.as_deref() {
                Some("checkout") => match rest.iter().position(|w| w == "--") {
                    Some(i) => rest[i + 1..].iter().any(|w| p.protects(w, cwd)),
                    None => false,
                },
                Some("restore") => any(&positionals[1..]),
                _ => false,
            }
        }
        // A link, a junction or a hard link to a protected folder opens it up to every later write.
        "ln" | "mklink" => positionals.iter().any(|w| p.touches_dir(w, cwd)),
        "sqlite3" => {
            let whole = words[at..].join(" ").to_ascii_lowercase();
            p.mentions_any(&whole) && SQL_WRITES.iter().any(|m| whole.contains(m))
        }
        n if INTERPRETERS.contains(&n) => {
            // `perl -pi -e ... <protected>` edits the file in place.
            let in_place = matches!(n, "perl" | "ruby")
                && rest.iter().any(|w| w.starts_with('-') && !w.starts_with("--") && w.contains('i'))
                && positionals.iter().any(|w| p.protects(w, cwd));
            in_place || interp::writes_protected(p, n, source, cwd, depth)
        }
        _ => {
            // A one-line .NET or `New-Item` write names its target inside a longer argument.
            let whole = words[at..].join(" ").to_ascii_lowercase();
            let links = ["symboliclink", "junction", "hardlink"].iter().any(|m| whole.contains(m));
            (DOTNET_WRITES.iter().any(|m| whole.contains(m)) || name == "new-item" || name == "set-item")
                && (p.names_any(&words[at..].join(" ")) || (links && p.mentions_any(&whole)))
        }
    }
}

/// The word `install` as the Unix copy command: written bare, with none of the installer's own flags.
fn is_the_unix_install(raw: &str, rest: &[String]) -> bool {
    raw.eq_ignore_ascii_case("install") && !rest.iter().any(|w| INSTALLER_FLAGS.iter().any(|f| w.starts_with(f)))
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
        refusal(&guard(), "Bash", Some(&input), None).is_some()
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
                assert!(refusal(&g, tool, Some(&input), None).is_some(), "{tool} {path}");
            }
        }
        let note = serde_json::json!({ "notebook_path": "C:/data/store/x.json" });
        assert!(refusal(&g, "NotebookEdit", Some(&note), None).is_some());
        for path in ["C:/work/acme-shop/notes.md", "C:/Users/alex/.claude/settings.json", "C:/data/store2/thor.db", "C:/Users/alex/.claude/projects/x/notes.txt"] {
            let input = serde_json::json!({ "file_path": path });
            assert!(refusal(&g, "Write", Some(&input), None).is_none(), "{path}");
        }
        let read = serde_json::json!({ "file_path": "C:/data/store/thor.db" });
        assert!(refusal(&g, "Read", Some(&read), None).is_none(), "reading is free");
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

    fn shell_in(command: &str, cwd: &str) -> bool {
        let input = serde_json::json!({ "command": command });
        refusal(&guard(), "Bash", Some(&input), Some(cwd)).is_some()
    }

    #[test]
    fn the_ordinary_other_forms_of_the_same_write_are_refused() {
        for command in [
            "cp session-watermark.json C:/data/store/",
            "cp session-watermark.json C:/data/store",
            "cp forged.json 'C:\\data\\store\\'",
            "mv forged.jsonl C:/Users/alex/.claude/projects/p/",
            "Copy-Item .\\forged.json C:\\data\\store\\",
            "rm -rf C:/data/store",
            "rm -rf C:/Users/alex/.claude/projects",
            "Remove-Item -Recurse C:\\data",
            "python -c \"open('C:/data/store/a.json','w').write('x')\"",
            "node -e \"require('fs').writeFileSync('C:/data/store/a.json','x')\"",
            "perl -pi -e 's/a/b/' C:/data/store/a.json",
            "dd if=x of=C:/data/store/thor.db",
            "curl -o C:/data/store/a.json http://example.test/x",
            "wget -O C:/data/store/a.json http://example.test/x",
            "install -m 644 forged.json C:/data/store/",
            "ln -s C:/data/store /tmp/link",
            "cmd /c mklink /J C:\\scratch\\l C:\\data\\store",
            "awk 'BEGIN{print \"x\" > \"C:/data/store/a.json\"}'",
            "sqlite3 C:/data/store/thor.db \"delete from events\"",
            "echo x > \\\\?\\C:\\data\\store\\a.json",
            "New-Item -ItemType SymbolicLink -Path C:\\scratch\\l -Value C:\\data\\store",
        ] {
            assert!(shell(command), "{command}");
        }
    }

    #[test]
    fn a_relative_path_is_read_against_the_working_folder_and_a_cd() {
        assert!(shell_in("echo x >> session-watermark.json", "C:/data/store"));
        assert!(shell_in("cd C:/Users/alex/.claude/projects/p && echo x >> s.jsonl", "C:/work"));
        assert!(shell_in("cd ../.claude/projects/p; Add-Content s.jsonl x", "C:/Users/alex/work"));
        assert!(shell_in("rm thor.db", "C:/data/store"));
        assert!(!shell_in("cd C:/work && echo x > notes.txt", "C:/data/store"));
        assert!(!shell_in("echo x > notes.txt", "C:/work"));
        assert!(!shell_in("cp C:/data/store/thor.db .", "C:/scratch"));
        let input = serde_json::json!({ "file_path": "session-watermark.json" });
        assert!(refusal(&guard(), "Write", Some(&input), Some("C:/data/store")).is_some());
        assert!(refusal(&guard(), "Write", Some(&input), Some("C:/work")).is_none());
        assert!(refusal(&guard(), "Write", Some(&input), None).is_none(), "no folder known: nothing to resolve against");
    }

    #[test]
    fn the_steps_the_docs_tell_the_assistant_to_take_are_free() {
        for command in [
            "install --set-connections off --db C:/data/store/thor.db",
            "install --set-backup local --db C:/data/store/thor.db",
            "C:/tools/thor2/bin/install.exe --set-git-hooks off --db C:/data/store/thor.db",
            "& \"C:\\tools\\thor2\\bin\\install.exe\" --topic --db \"C:\\data\\store\\thor.db\"",
            "C:/tools/thor2/bin/serve.exe --db C:/data/store/thor.db inventory",
            "node C:/work/tool.mjs --db C:/data/store/thor.db",
            "python -c \"print(open('C:/data/store/a.json').read())\"",
            "sqlite3 C:/data/store/thor.db \"select count(*) from events\"",
            "cp C:/data/store/thor.db C:/scratch/",
            "cp notes.txt C:/data/store/notes.txt",
        ] {
            assert!(!shell(command), "{command}");
        }
        // Files the owner edits by hand stay editable, with a tool or a command.
        for name in ["guard-capture-mode.json", "stale-guard-mode.json", "no-project-ask-mode.json", "work-folders.json", "thor-mode.json"] {
            let input = serde_json::json!({ "file_path": format!("C:/data/store/{name}") });
            assert!(refusal(&guard(), "Write", Some(&input), None).is_none(), "{name}");
        }
    }

    #[test]
    fn the_refusal_says_what_to_do_instead() {
        let text = refusal_text();
        assert!(text.contains("install program") && text.contains("tell the person"), "{text}");
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
            if refusal(&g, &call.tool, Some(&call.input), None).is_some() {
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
    fn a_one_line_program_is_refused_only_when_its_write_is_aimed_at_a_protected_path() {
        for command in [
            "python -c \"open(r'C:/Users/alex/.claude/projects/x/s.jsonl','w')\"",
            "python -c \"open('C:/data/store/a.json', mode='a').write('x')\"",
            "py -3 -I -c \"open(r'C:\\Users\\alex\\.claude\\projects\\x\\s.jsonl','a')\"",
            "python -I -c \"from pathlib import Path; Path('C:/data/store/a.json').write_text('x')\"",
            "python -c \"import os; os.remove('C:/data/store/thor.db-wal')\"",
            "python -c \"import os; os.unlink(r'C:\\Users\\alex\\.claude\\projects\\x\\s.jsonl')\"",
            "python -c \"import shutil; shutil.rmtree('C:/data/store')\"",
            "python -c \"import shutil; shutil.copy('f.json', 'C:/data/store/a.json')\"",
            "python -c \"import os; os.rename('C:/data/store/thor.db', 'C:/scratch/x')\"",
            "python -c \"p = 'C:/data/store/a.json'; open(p, 'w').write('x')\"",
            "python -c \"from pathlib import Path; p = Path('C:/data/store/a.json'); p.unlink()\"",
            "python -c \"import os; open(os.path.join('C:/data/store', 'a.json'), 'wb')\"",
            "python -c \"import sqlite3; sqlite3.connect('C:/data/store/thor.db').execute('delete from event')\"",
            "python -c \"import os; os.system('echo x > C:/data/store/a.json')\"",
            "python -c \"import subprocess; subprocess.run(['rm', '-f', 'C:/data/store/thor.db'])\"",
            "node -e \"require('fs').writeFileSync('C:/data/store/a.json','x')\"",
            "node -e \"require('fs').appendFileSync(String.raw`C:/Users/alex/.claude/projects/x/s.jsonl`, 'x')\"",
            "node -e \"require('fs').unlinkSync('C:/data/store/a.json')\"",
            "node -e \"fs.createWriteStream('C:/data/store/a.json')\"",
            "perl -e 'open(F, \">\", \"C:/data/store/a.json\"); print F 1'",
            "perl -e 'unlink(\"C:/data/store/a.json\")'",
            "perl -i.bak -pe 's/a/b/' C:/data/store/a.json",
            "ruby -e \"File.write('C:/data/store/a.json', 'x')\"",
            "php -r \"file_put_contents('C:/data/store/a.json', 'x');\"",
            "awk 'BEGIN{printf \"x\" >> \"C:/data/store/a.json\"}'",
        ] {
            assert!(shell(command), "{command}");
        }
    }

    #[test]
    fn a_one_line_program_that_writes_elsewhere_or_only_reads_is_free() {
        for command in [
            // -I is Python's isolated flag, not sed -i
            "python -I -c \"print(open('C:/data/store/a.json').read())\"",
            "py -3 -I -W ignore -c \"import fitz; d = fitz.open(r'C:\\Users\\alex\\.claude\\projects\\x\\a.pdf'); print(d.page_count)\"",
            // a word that writes, aimed at something else
            "python -c \"x = []; x.append(open('C:/data/store/a.json').read()); print(x)\"",
            "python -c \"import os; os.remove('C:/scratch/old.txt'); print(open('C:/data/store/a.json').read())\"",
            "python -I -c \"import sqlite3; c = sqlite3.connect('file:C:/data/store/thor.db?mode=ro', uri=True); print(c.execute('select count(*) from event').fetchone()); open(r'C:/scratch/out.txt','w').write('x')\"",
            "python -c \"import sqlite3; print(sqlite3.connect('C:/data/store/thor.db').execute('select 1').fetchone())\"",
            "python -c \"open('C:/scratch/copy.txt','w').write(open('C:/Users/alex/.claude/projects/x/s.jsonl').read())\"",
            "python -c \"import shutil; shutil.copy('C:/data/store/thor.db', 'C:/scratch/thor.db')\"",
            "python -c \"import json; [print(json.loads(l)['type']) for l in open(r'C:/Users/alex/.claude/projects/x/s.jsonl')]\"",
            "python -c \"from pathlib import Path; Path('C:/scratch/a.txt').write_text(Path('C:/data/store/a.json').read_text())\"",
            "python -c \"p = 'C:/data/store/a.json'; open('C:/scratch/n.txt','w').write(open(p).read())\"",
            "node -e \"require('fs').writeFileSync('C:/scratch/out.txt', require('fs').readFileSync('C:/data/store/a.json'))\"",
            "node -e \"console.log(require('fs').readFileSync('C:/data/store/a.json','utf8'))\"",
            "perl -ne 'print if /x/' C:/data/store/a.json",
            "perl -e 'open(F, \">\", \"C:/scratch/o.txt\"); print F 1'",
            "awk '$1 > 5' C:/data/store/a.json",
            "awk 'BEGIN{print \"x\" > \"C:/scratch/o.txt\"}'",
            "python -c \"print('append this and remove that')\"",
        ] {
            assert!(!shell(command), "{command}");
        }
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
