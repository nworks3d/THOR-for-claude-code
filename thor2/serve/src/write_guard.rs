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
//! home variables are expanded, `..` is resolved, a trailing dot or `::$DATA` is dropped, and a
//! `\\localhost\c$` share or an 8.3 short name is read as the path it stands for. Trees copied or
//! extracted into these folders, wildcard and piped deletes and `-EncodedCommand` are read too
//! (`docs/WHAT-THOR-DOES.md`, D7b). A shell can build a path at run time (a variable, a command
//! substitution, a script file that writes it, `eval`, a link made in an earlier step): that
//! cannot be seen here, and this guard does not claim to see it. THOR's own programs and the installer are not hooked,
//! so they are not affected.

use std::path::{Path, PathBuf};

mod forms;
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
    /// File names kept wherever they live, in lower case (`with_name_lock`): a write aimed at a path
    /// ending in one of them is refused, whatever folder it is spelled into and however the name is
    /// spelled (brace forms, backslash escapes, quotes split inside it).
    names: Vec<String>,
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
        for name in ["LOCALAPPDATA", "USERPROFILE", "HOME", "APPDATA", "HOMEDRIVE", "HOMEPATH", "CLAUDE_CONFIG_DIR", "PROGRAMDATA"] {
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
        let keep = |p: &Path| non_empty(forms::clean_path(&fold(&p.to_string_lossy())));
        Protected {
            store_dir: store_dir.and_then(keep),
            programs_dir: programs_dir.and_then(keep),
            claude_dirs: claude_dirs.iter().filter_map(|p| keep(p)).collect(),
            vars,
            names: Vec::new(),
        }
    }

    /// A guard that keeps nothing but files with these names (see `names`).
    pub fn with_name_lock(names: &[&str]) -> Self {
        Protected { names: names.iter().map(|n| n.to_ascii_lowercase()).collect(), ..Protected::default() }
    }

    /// The same guard that also keeps files with these names.
    pub fn and_name_lock(mut self, names: &[&str]) -> Self {
        self.names.extend(names.iter().map(|n| n.to_ascii_lowercase()));
        self
    }

    /// Whether a path as written in a call ends in a locked file name, in any of its spellings.
    fn names_a_locked_file(&self, written: &str) -> bool {
        self.names.iter().any(|name| spells_name(written, name))
    }

    /// Whether a text holds a locked file name in any of its spellings (a one-line program).
    fn mentions_a_locked_file(&self, text: &str) -> bool {
        self.names.iter().any(|name| brace_forms(text).iter().any(|f| plain_spelling(f).contains(name.as_str())))
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
        if path.is_empty() {
            return path;
        }
        if is_absolute(&path) {
            return forms::clean_path(&path);
        }
        match cwd {
            Some(dir) if !path.starts_with('$') && !path.starts_with('%') => forms::clean_path(&fold(&format!("{dir}/{path}"))),
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
        if self.protected_dirs().iter().any(|dir| *dir == path || dir.starts_with(&format!("{path}/"))) {
            return true;
        }
        // A folder of a saved conversation (`projects/<project>`, `projects/<project>/<session>`).
        let last = path.rsplit('/').next().unwrap_or("");
        if !last.contains('.') && self.projects_dirs().iter().any(|d| path.starts_with(&format!("{d}/"))) {
            return true;
        }
        self.wildcard_removes_protected(&path)
    }

    /// Whether a wildcard path (`<store>/*`, `<store>/thor.db*`, `projects/*/*.jsonl`, `C:/data/*`)
    /// takes a protected file, or a protected folder, with it. A pattern that cannot match a state
    /// file (`<store>/*.txt`) is free.
    fn wildcard_removes_protected(&self, path: &str) -> bool {
        if !forms::has_wildcard(path) {
            return false;
        }
        let segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
        let mut kinds: Vec<(String, &[&str])> = Vec::new();
        kinds.extend(self.store_dir.clone().map(|d| (d, &STATE_SAMPLES[..])));
        kinds.extend(self.programs_dir.clone().map(|d| (d, &ANY_SAMPLES[..])));
        kinds.extend(self.projects_dirs().into_iter().map(|d| (d, &PROJECT_SAMPLES[..])));
        kinds.iter().any(|(dir, samples)| {
            let dir_segs: Vec<&str> = dir.split('/').filter(|s| !s.is_empty()).collect();
            let shared = segs.len().min(dir_segs.len());
            if !(0..shared).all(|i| forms::wildcard_matches(segs[i], dir_segs[i])) {
                return false;
            }
            // The pattern names the folder or one above it, or reaches a file inside it.
            segs.len() <= dir_segs.len() || samples.iter().any(|s| forms::wildcard_matches(segs[segs.len() - 1], s))
        })
    }

    /// Whether copying or extracting a whole tree into this folder fills a protected place: the
    /// memory's folder itself, THOR's programs, a folder of saved conversations.
    fn receives_tree(&self, written: &str, cwd: Option<&str>) -> bool {
        let path = self.resolve(written, cwd);
        if path.is_empty() || !is_absolute(&path) {
            return false;
        }
        let last = path.rsplit('/').next().unwrap_or("");
        let projects = self.projects_dirs();
        self.store_dir.as_deref() == Some(path.as_str())
            || self.programs_dir.as_deref().is_some_and(|d| path == d || path.starts_with(&format!("{d}/")))
            || projects.iter().any(|d| path == *d || (path.starts_with(&format!("{d}/")) && !last.contains('.')))
    }

    /// Whether a path is a protected folder, or inside one (a link to it would open it up).
    fn touches_dir(&self, written: &str, cwd: Option<&str>) -> bool {
        let path = self.resolve(written, cwd);
        !path.is_empty() && self.protected_dirs().iter().any(|dir| path == *dir || path.starts_with(&format!("{dir}/")))
    }

    /// Whether a path as written in a call is one of the protected files.
    fn protects(&self, written: &str, cwd: Option<&str>) -> bool {
        self.names_a_locked_file(written) || self.protects_by_place(written, cwd)
    }

    /// Whether a path as written in a call is a protected file because of the folder it is in.
    fn protects_by_place(&self, written: &str, cwd: Option<&str>) -> bool {
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
        // The longest names first, so `$homedrive` is not read as `$home` and `drive`.
        let mut vars: Vec<&(String, String)> = self.vars.iter().collect();
        vars.sort_by_key(|(n, _)| std::cmp::Reverse(n.len()));
        for (name, value) in vars {
            for form in [format!("%{name}%"), format!("${{env:{name}}}"), format!("${{{name}}}")] {
                if out.contains(&form) {
                    out = out.replace(&form, value);
                }
            }
            for form in [format!("$env:{name}"), format!("${name}")] {
                out = replace_word(&out, &form, value);
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

/// Names a wildcard is tried against: a state file of the memory's folder, any file of THOR's
/// programs, a saved conversation or a folder of one.
const STATE_SAMPLES: [&str; 5] = ["thor.db", "thor.db-wal", "state.json", "session.jsonl", "mark.stamp"];
const ANY_SAMPLES: [&str; 3] = ["serve.exe", "install.exe", "data"];
const PROJECT_SAMPLES: [&str; 2] = ["session.jsonl", "project"];

/// Files in THOR's folder that the owner (or his assistant, on his word) edits by hand.
pub(crate) const EDITED_BY_HAND: [&str; 9] = [
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

/// A text with the spellings that hide a file name taken out: lower case, no backslash, no
/// backtick, no quote. (A backslash is both a path separator and a shell escape; for a name check
/// only the characters matter, so both readings reduce to the same text.)
fn plain_spelling(text: &str) -> String {
    text.chars().filter(|c| !matches!(c, '\\' | '`' | '\'' | '"')).collect::<String>().to_ascii_lowercase()
}

/// The strings a shell brace expression stands for, at most 64: `a{b,c}d` is `abd` and `acd`, and
/// `a{b}d` (which a shell leaves alone) is both `a{b}d` and `abd`. The text itself comes first.
fn brace_forms(text: &str) -> Vec<String> {
    let mut forms = vec![text.to_string()];
    let mut at = 0;
    while at < forms.len() && forms.len() < 64 {
        let form = forms[at].clone();
        at += 1;
        let Some(open) = form.find('{') else { continue };
        let Some(close) = form[open..].find('}').map(|c| c + open) else { continue };
        let inner = &form[open + 1..close];
        let (head, tail) = (&form[..open], &form[close + 1..]);
        let alternatives: Vec<&str> = inner.split(',').collect();
        if alternatives.len() == 1 {
            forms.push(format!("{head}{inner}{tail}"));
            continue;
        }
        for alt in alternatives {
            forms.push(format!("{head}{alt}{tail}"));
        }
    }
    forms
}

/// Whether `written` (a path, a redirect target, an argument) is a file called `name` (lower case),
/// however it is spelled: with brace forms, backslash escapes, quotes split inside the name, a
/// folder in front, or a Windows trailing dot or stream (`name:stream`) behind it.
fn spells_name(written: &str, name: &str) -> bool {
    brace_forms(written).iter().any(|form| {
        let plain = plain_spelling(form);
        let before_stream = plain.split(':').next().unwrap_or("");
        let trimmed = |t: &str| t.trim_end_matches(|c| c == '.' || c == ' ').ends_with(name);
        trimmed(&plain) || trimmed(before_stream)
    })
}

/// Whether a comparison-form path starts at a root.
fn is_absolute(path: &str) -> bool {
    path.starts_with('/') || path.as_bytes().get(1) == Some(&b':')
}

fn non_empty(s: String) -> Option<String> {
    (!s.is_empty()).then_some(s)
}

/// `form` replaced by `value` where no letter, digit or `_` follows it (`$home` in `$home/x`, not
/// in `$homedrive`).
fn replace_word(text: &str, form: &str, value: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(form) {
        let after = &rest[at + form.len()..];
        out.push_str(&rest[..at]);
        if after.chars().next().is_some_and(|c| c.is_alphanumeric() || c == '_') {
            out.push_str(form);
        } else {
            out.push_str(value);
        }
        rest = after;
    }
    out.push_str(rest);
    out
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
    // The administrative share of this computer's own drive (`//localhost/c$/x`) is `c:/x`.
    p = forms::unshare(&p);
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
    writes_protected(protected, tool_name, tool_input, cwd).then(refusal_text)
}

/// Whether the call writes to a protected file (the test `refusal` is built on).
pub fn writes_protected(
    protected: &Protected,
    tool_name: &str,
    tool_input: Option<&serde_json::Value>,
    cwd: Option<&str>,
) -> bool {
    let Some(input) = tool_input else { return false };
    let cwd = cwd.map(fold).filter(|c| is_absolute(c));
    let cwd = cwd.as_deref();
    match tool_name.to_ascii_lowercase().as_str() {
        "edit" | "write" | "multiedit" | "notebookedit" => ["file_path", "filePath", "notebook_path", "notebookPath", "path"]
            .iter()
            .any(|k| input.get(k).and_then(|v| v.as_str()).is_some_and(|p| protected.protects(p, cwd))),
        "bash" | "powershell" => input
            .get("command")
            .and_then(|v| v.as_str())
            .is_some_and(|c| command_writes_protected(protected, c, cwd, 0)),
        _ => false,
    }
}

/// The words of every segment of a shell command (a segment ends at `;`, `&&`, `||`, `|`, `&` or a
/// new line), quotes taken out. Redirect targets are left out.
pub(crate) fn command_segments(command: &str) -> Vec<Vec<String>> {
    let mut segments = Vec::new();
    let mut segment: Vec<String> = Vec::new();
    let mut redirected = false;
    for tok in tokenize(command) {
        match tok {
            Tok::Word(w) => {
                if redirected {
                    redirected = false;
                } else {
                    segment.push(w);
                }
            }
            Tok::Redirect => redirected = true,
            Tok::Border => {
                segments.push(std::mem::take(&mut segment));
                redirected = false;
            }
        }
    }
    segments.push(segment);
    segments
}

/// One word of a command, or the marker of a redirect.
#[derive(Debug, PartialEq, Eq)]
enum Tok {
    Word(String),
    /// `>`, `>>` or `>|`: the next word is written to.
    Redirect,
    /// A segment border: `;`, `&&`, `||`, `|`, `&` or a new line.
    /// `true` for a pipe (`|`, not `||`): the next command reads what this one prints.
    Border(bool),
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
                let pipe = c == '|' && chars.get(i + 1) != Some(&'|');
                while i + 1 < chars.len() && matches!(chars[i + 1], '|' | '&') && c != '\n' && c != ';' {
                    i += 1;
                }
                out.push(Tok::Border(pipe));
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
pub(crate) fn command_name(word: &str) -> String {
    let name = word.replace('\\', "/");
    let name = name.rsplit('/').next().unwrap_or(&name).to_ascii_lowercase();
    name.strip_suffix(".exe").map(str::to_string).unwrap_or(name)
}

const SKIPPED_LEADERS: [&str; 10] = ["sudo", "env", "nohup", "command", "builtin", "exec", "time", "call", "start", "&"];
pub(crate) const SHELLS: [&str; 8] = ["bash", "sh", "zsh", "powershell", "pwsh", "cmd", "dash", "ksh"];
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
    // Each command: its words, the files it redirects into, and whether it pipes into the next.
    let mut segments: Vec<(Vec<String>, Vec<String>, bool)> = Vec::new();
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
            Tok::Border(pipe) => {
                segments.push((std::mem::take(&mut segment), std::mem::take(&mut targets), pipe));
                redirected = false;
            }
        }
    }
    segments.push((segment, targets, false));
    // The folder the command works in moves with every `cd`, so a relative path after it is read
    // against the right folder.
    let mut here: Option<String> = cwd.map(str::to_string);
    for (at, (words, redirects, _)) in segments.iter().enumerate() {
        if redirects.iter().any(|t| p.protects(t, here.as_deref())) {
            return true;
        }
        if segment_writes_protected(p, words, here.as_deref(), depth, command) {
            return true;
        }
        if at > 0 && segments[at - 1].2 && removes_what_it_is_fed(words) && lists_protected(p, &segments, at, here.as_deref()) {
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

const REMOVERS: [&str; 9] = ["rm", "del", "erase", "rmdir", "rd", "remove-item", "ri", "unlink", "shred"];
/// Commands that list files and print their names, so what follows the pipe can act on them.
const LISTERS: [&str; 8] = ["gci", "dir", "ls", "get-childitem", "get-item", "gi", "find", "fd"];
/// Commands that pass a listing on, renamed or in another order, without choosing from it.
const PASS_THROUGH: [&str; 6] = ["select-object", "select", "sort-object", "sort", "tee-object", "out-string"];

/// Whether a command in a pipeline removes the files whose names it is fed: `Remove-Item` or `rm`
/// with no path of its own, `xargs rm`, `ForEach-Object { Remove-Item $_ }`.
fn removes_what_it_is_fed(words: &[String]) -> bool {
    let Some((name, rest)) = leader_of(words) else { return false };
    if REMOVERS.contains(&name.as_str()) {
        return !rest.iter().any(|w| !w.starts_with('-') && !(w.starts_with('/') && w.len() <= 3));
    }
    matches!(name.as_str(), "xargs" | "foreach-object" | "foreach" | "%") && rest.iter().any(|w| REMOVERS.contains(&command_name(w).as_str()))
}

/// Whether the command that feeds the removal at `at` lists a protected folder (and chooses
/// nothing from it that a state file could not match).
fn lists_protected(p: &Protected, segments: &[(Vec<String>, Vec<String>, bool)], at: usize, cwd: Option<&str>) -> bool {
    let mut j = at;
    while j > 0 && segments[j - 1].2 {
        j -= 1;
        let words = &segments[j].0;
        let Some((name, _)) = leader_of(words) else { return false };
        if LISTERS.contains(&name.as_str()) {
            return lister_reaches_protected(p, words, cwd);
        }
        if !PASS_THROUGH.contains(&name.as_str()) {
            return false;
        }
    }
    false
}

/// A `Get-ChildItem <store>`, `ls <store>/*`, `find <store>`: the folder (or a wildcard that could
/// cover its state files) named, with no `-Filter`, `-Include` or `-name` that rules them out.
fn lister_reaches_protected(p: &Protected, words: &[String], cwd: Option<&str>) -> bool {
    let words = forms::split_colon_params(words);
    let Some((name, rest)) = leader_of(&words) else { return false };
    let filters: Vec<String> = rest
        .iter()
        .enumerate()
        .filter(|(_, w)| matches!(w.to_ascii_lowercase().as_str(), "-filter" | "-include" | "-name" | "-iname"))
        .filter_map(|(i, _)| rest.get(i + 1).cloned())
        .collect();
    let lower: Vec<String> = filters.iter().map(|f| f.to_ascii_lowercase()).collect();
    let could_match_state = |f: &String| STATE_SAMPLES.iter().chain(PROJECT_SAMPLES.iter()).chain(ANY_SAMPLES.iter()).any(|s| forms::wildcard_matches(f, s));
    if !lower.is_empty() && !lower.iter().any(could_match_state) {
        return false;
    }
    let _ = name;
    rest.iter().any(|w| {
        if w.starts_with('-') {
            return false;
        }
        let path = p.resolve(w, cwd);
        !path.is_empty() && (p.touches_dir(w, cwd) || p.wildcard_removes_protected(&path))
    })
}

/// The command word of a segment (after `sudo`, `env`, assignments and the like) and what follows it.
pub(crate) fn leader_of(words: &[String]) -> Option<(String, &[String])> {
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
    // A file copied INTO a folder takes the name of its source; only the folder's place matters here,
    // a locked file name does not (copying the record file somewhere else is a read).
    sources.iter().any(|s| p.protects_by_place(&format!("{folder}/{}", base_name(s)), cwd))
}

fn segment_writes_protected(p: &Protected, words: &[String], cwd: Option<&str>, depth: usize, source: &str) -> bool {
    // PowerShell's `-Path:C:\x` is the two words `-Path` and `C:\x`.
    let normalized = forms::split_colon_params(words);
    let words: &[String] = &normalized;
    let Some((name, rest)) = leader_of(words) else { return false };
    let at = words.len() - rest.len() - 1;
    let is_windows_copier = matches!(name.as_str(), "xcopy" | "robocopy");
    let flag = |w: &String| {
        w.starts_with('-')
            || (w.starts_with('/') && w.len() <= 3 && !w.contains('.'))
            || (is_windows_copier && w.starts_with('/') && !w[1..].contains(['/', '\\', '.']))
    };
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
    let has_flag = |names: &[&str]| rest.iter().any(|w| names.contains(&w.to_ascii_lowercase().as_str()));
    match name.as_str() {
        n if SHELLS.contains(&n) => {
            // `bash -c '...'`, `powershell -Command "..."`: read the quoted program too.
            // `cmd /c mklink ...` and `bash -c cp ...` carry the command as plain words.
            let inline = rest
                .iter()
                .position(|w| matches!(w.to_ascii_lowercase().as_str(), "/c" | "/k" | "-c" | "-lc" | "-command" | "-c:"))
                .is_some_and(|i| segment_writes_protected(p, &rest[i + 1..], cwd, depth + 1, source));
            // `powershell -EncodedCommand <base64>`: the program is hidden in the text.
            let encoded = matches!(n, "powershell" | "pwsh")
                && rest.iter().enumerate().any(|(i, w)| {
                    forms::is_encoded_command_flag(w)
                        && rest.get(i + 1).and_then(|payload| forms::decode_encoded_command(payload)).is_some_and(|program| command_writes_protected(p, &program, cwd, depth + 1))
                });
            depth < 3 && (inline || encoded || rest.iter().any(|w| w.contains(' ') && command_writes_protected(p, w, cwd, depth + 1)))
        }
        "iex" | "invoke-expression" => depth < 3 && command_writes_protected(p, &rest.join(" "), cwd, depth + 1),
        "rm" | "del" | "erase" | "rmdir" | "rd" | "remove-item" | "ri" | "unlink" | "shred" => {
            positionals.iter().any(|w| p.removal_protects(w, cwd))
        }
        "tee" | "truncate" | "touch" | "tee-object" | "export-csv" | "export-clixml" | "export-alias" => any(&positionals),
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
        "cp" | "copy" | "copy-item" | "cpi" | "xcopy" | "robocopy" | "rsync" => {
            // Only the DESTINATION is written: the one named by -Destination or -t, else the last
            // (the second for xcopy and robocopy). A folder that receives a file named like a
            // protected one counts; so does a whole tree copied into a protected folder.
            let named = after_flag(&["-destination", "-t", "--target-directory"]);
            let (sources, dest): (Vec<&String>, Option<&String>) = match named.first() {
                Some(d) => (positionals.iter().copied().filter(|w| w != d).collect(), Some(*d)),
                None if is_windows_copier => (positionals.iter().take(1).copied().collect(), positionals.get(1).copied()),
                None => match positionals.split_last() {
                    Some((last, init)) => (init.to_vec(), Some(*last)),
                    None => (Vec::new(), None),
                },
            };
            let whole_tree = is_windows_copier
                || name == "rsync"
                || rest.iter().any(|w| {
                    let l = w.to_ascii_lowercase();
                    matches!(l.as_str(), "-r" | "--recursive" | "-a" | "--archive") || l.starts_with("-rec") || (l.starts_with('-') && !l.starts_with("--") && l[1..].contains(['r', 'a']) && l.len() <= 4 && name == "cp")
                });
            dest.is_some_and(|d| dest_receives(p, d, &sources, cwd) || (whole_tree && p.receives_tree(d, cwd)))
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
            // `-o f`, `-sSLo f`, `-of`, `--output f`, `--output=f`, PowerShell's `-OutFile f`.
            let targets = after_flag(&["-o", "--output", "-outfile"]);
            let direct = rest.iter().any(|w| w.strip_prefix("--output=").is_some_and(|v| p.protects(v, cwd)));
            let clustered = forms::cluster_values(rest, &['o']).iter().any(|(_, v)| p.protects(v, cwd));
            direct || clustered || any(&targets) || after_flag(&["--output-dir"]).iter().any(|w| p.touches_dir(w, cwd))
        }
        "wget" => {
            let direct = rest.iter().any(|w| w.strip_prefix("--output-document=").is_some_and(|v| p.protects(v, cwd)));
            let clustered = forms::cluster_values(rest, &['O', 'o']).iter().any(|(_, v)| p.protects(v, cwd));
            let folders = forms::cluster_values(rest, &['P']).iter().any(|(_, v)| p.touches_dir(v, cwd));
            direct
                || clustered
                || folders
                || any(&after_flag(&["-o", "--output-document", "--output-file", "-outfile"]))
                || after_flag(&["-p", "--directory-prefix"]).iter().any(|w| p.touches_dir(w, cwd))
        }
        "invoke-webrequest" | "iwr" | "invoke-restmethod" | "irm" | "start-bitstransfer" => any(&after_flag(&["-outfile", "-destination"])),
        // Only the forms that write a file: `-hashfile` and `-dump` only read.
        "certutil" => {
            let writes = ["-urlcache", "-decode", "-decodehex", "-encode", "-encodehex", "-split", "-verifyctl"].iter().any(|f| has_flag(&[f]));
            writes && rest.iter().any(|w| p.protects(w, cwd))
        }
        "bitsadmin" => has_flag(&["/transfer", "/addfile"]) && rest.iter().any(|w| p.protects(w, cwd)),
        "sed" => {
            let in_place = rest.iter().any(|w| w == "-i" || (w.starts_with("-i") && !w.starts_with("--")) || w.starts_with("--in-place"));
            (in_place && any(&positionals)) || rest.iter().any(|w| sed_script_writes_protected(p, w, cwd))
        }
        "find" => {
            // `find <store> -delete` and `find <store> -exec rm {} ;`, unless `-name` rules out state files.
            let paths: Vec<&String> = rest.iter().take_while(|w| !w.starts_with('-') && !matches!(w.as_str(), "(" | "!")).collect();
            let deletes = rest.iter().enumerate().any(|(i, w)| {
                w == "-delete"
                    || (matches!(w.as_str(), "-exec" | "-execdir" | "-ok" | "-okdir")
                        && rest.get(i + 1).is_some_and(|c| REMOVERS.contains(&command_name(c).as_str()) || matches!(command_name(c).as_str(), "mv" | "truncate")))
            });
            let names: Vec<String> = after_flag(&["-name", "-iname"]).iter().map(|n| n.to_ascii_lowercase()).collect();
            let could_match = |f: &String| STATE_SAMPLES.iter().chain(PROJECT_SAMPLES.iter()).chain(ANY_SAMPLES.iter()).any(|s| forms::wildcard_matches(f, s));
            let ruled_out = !names.is_empty() && !names.iter().any(could_match);
            deletes && !ruled_out && paths.iter().any(|w| p.touches_dir(w, cwd) || p.removal_protects(w, cwd))
        }
        "tar" => {
            let letters_x = rest.first().is_some_and(|w| !w.starts_with('-') && w.chars().all(|c| c.is_ascii_alphabetic()) && w.contains('x'));
            let extracts = letters_x
                || rest.iter().any(|w| w == "--extract" || w == "--get" || (w.starts_with('-') && !w.starts_with("--") && w.contains('x')));
            let dirs = forms::cluster_values(rest, &['C']);
            let long = rest.iter().filter_map(|w| w.strip_prefix("--directory=")).chain(after_flag(&["--directory"]).into_iter().map(String::as_str));
            extracts && (dirs.iter().any(|(_, d)| p.receives_tree(d, cwd)) || long.into_iter().any(|d| p.receives_tree(d, cwd)))
        }
        "unzip" => forms::cluster_values(rest, &['d']).iter().any(|(_, d)| p.receives_tree(d, cwd)),
        "7z" | "7za" | "7zr" => {
            rest.first().is_some_and(|c| matches!(c.as_str(), "x" | "e")) && rest.iter().any(|w| w.strip_prefix("-o").is_some_and(|d| !d.is_empty() && p.receives_tree(d, cwd)))
        }
        "expand-archive" => {
            let dest = after_flag(&["-destinationpath"]);
            let dest: Vec<&String> = if dest.is_empty() { positionals.iter().skip(1).copied().collect() } else { dest };
            dest.iter().any(|d| p.receives_tree(d, cwd))
        }
        "compress-archive" => any(&after_flag(&["-destinationpath"])),
        "git" => git_writes_protected(p, rest, &positionals, cwd),
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
            (DOTNET_WRITES.iter().any(|m| whole.contains(m)) || matches!(name.as_str(), "new-item" | "ni" | "set-item" | "si"))
                && (p.names_any(&words[at..].join(" ")) || p.mentions_a_locked_file(&whole) || (links && p.mentions_any(&whole)))
        }
    }
}

/// The `w` command of a `sed` script, which writes a file: `sed -n 'w <protected>'`,
/// `sed 's/a/b/w <protected>'`, `sed '1w <protected>'`.
fn sed_script_writes_protected(p: &Protected, script: &str, cwd: Option<&str>) -> bool {
    let chars: Vec<char> = script.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        if !matches!(c, 'w' | 'W') {
            continue;
        }
        let before_ok = i == 0 || matches!(chars[i - 1], '/' | ';' | '{' | '}' | ' ' | '\n' | '$' | '!' | ',') || chars[i - 1].is_ascii_digit();
        if !before_ok {
            continue;
        }
        let target: String = chars[i + 1..].iter().collect::<String>().lines().next().unwrap_or("").trim().to_string();
        if !target.is_empty() && p.protects(&target, cwd) {
            return true;
        }
    }
    false
}

/// A `git` command that writes a protected file: the older `checkout --` and `restore` forms, an
/// output file (`git archive -o`, `git diff --output`), a clone or a checkout-index into a
/// protected folder, and a command that rewrites the working tree of a protected folder
/// (`git -C <store> reset --hard`, `git -C <store> clean -fd`).
fn git_writes_protected(p: &Protected, rest: &[String], positionals: &[&String], cwd: Option<&str>) -> bool {
    // The subcommand is the first word that is not a global option or the value of one.
    let mut sub: Option<(usize, String)> = None;
    let mut dirs: Vec<String> = Vec::new();
    let mut i = 0;
    while i < rest.len() {
        let w = rest[i].as_str();
        match w {
            "-C" | "-c" | "--git-dir" | "--work-tree" | "--namespace" | "--exec-path" => {
                if matches!(w, "-C" | "--git-dir" | "--work-tree") {
                    if let Some(v) = rest.get(i + 1) {
                        dirs.push(v.clone());
                    }
                }
                i += 2;
            }
            _ if w.starts_with("--git-dir=") || w.starts_with("--work-tree=") => {
                dirs.push(w.split_once('=').map(|(_, v)| v.to_string()).unwrap_or_default());
                i += 1;
            }
            _ if w.starts_with('-') => i += 1,
            _ => {
                sub = Some((i, w.to_ascii_lowercase()));
                break;
            }
        }
    }
    let Some((at, sub)) = sub else { return false };
    let args = &rest[at + 1..];
    let value_of = |names: &[&str]| -> Vec<String> {
        let mut found = Vec::new();
        for (j, w) in args.iter().enumerate() {
            for n in names {
                if let Some(v) = w.strip_prefix(&format!("{n}=")) {
                    found.push(v.to_string());
                } else if w == n {
                    if let Some(next) = args.get(j + 1) {
                        found.push(next.clone());
                    }
                }
            }
        }
        found
    };
    match sub.as_str() {
        "checkout" => match args.iter().position(|w| w == "--") {
            Some(j) if args[j + 1..].iter().any(|w| p.protects(w, cwd)) => return true,
            _ => {}
        },
        "restore" => {
            if positionals.iter().skip(1).any(|w| p.protects(w, cwd)) {
                return true;
            }
        }
        "archive" | "diff" | "log" | "show" | "format-patch" | "bundle" => {
            if value_of(&["--output", "-o"]).iter().any(|v| p.protects(v, cwd) || p.receives_tree(v, cwd)) {
                return true;
            }
        }
        "clone" => {
            let operands: Vec<&String> = args.iter().filter(|w| !w.starts_with('-')).collect();
            if operands.len() >= 2 && operands.last().is_some_and(|d| p.receives_tree(d, cwd)) {
                return true;
            }
        }
        "checkout-index" => {
            if value_of(&["--prefix"]).iter().any(|v| p.receives_tree(v, cwd)) {
                return true;
            }
        }
        _ => {}
    }
    const REWRITES_THE_TREE: [&str; 15] =
        ["reset", "clean", "checkout", "restore", "switch", "stash", "merge", "rebase", "pull", "apply", "am", "cherry-pick", "revert", "rm", "mv"];
    if REWRITES_THE_TREE.contains(&sub.as_str()) {
        let targets: Vec<String> = if dirs.is_empty() { cwd.map(str::to_string).into_iter().collect() } else { dirs };
        return targets.iter().any(|d| p.touches_dir(d, cwd));
    }
    false
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

    /// A guard whose home variables are fixed, so the spelling tests read the same everywhere.
    fn guard_with_variables() -> Protected {
        let mut g = Protected::new(
            Some(Path::new("C:/data/store")),
            Some(Path::new("C:/tools/thor2/bin")),
            &[PathBuf::from("C:/Users/alex/.claude"), PathBuf::from("C:/alt/claude")],
        );
        g.vars = [
            ("userprofile", "c:/users/alex"),
            ("home", "c:/users/alex"),
            ("homedrive", "c:"),
            ("homepath", "/users/alex"),
            ("localappdata", "c:/users/alex/appdata/local"),
            ("claude_config_dir", "c:/alt/claude"),
        ]
        .iter()
        .map(|(n, v)| (n.to_string(), v.to_string()))
        .collect();
        g
    }

    fn shell_as(g: &Protected, tool: &str, command: &str) -> bool {
        let input = serde_json::json!({ "command": command });
        refusal(g, tool, Some(&input), None).is_some()
    }

    #[test]
    fn other_spellings_of_a_protected_path_are_refused() {
        let g = guard_with_variables();
        for command in [
            "echo x > C:/data/store/thor.db.",
            "echo x > 'C:\\data\\store\\unpin-owner-go.json::$DATA'",
            "echo x > C:/Users/alex/.claude/projects/p/s.jsonl::$DATA",
            "echo x > C:/Users/alex/.claude/projects/p/s.jsonl.",
            "echo x > \"C:/Users/alex/.claude/projects/p/s.jsonl \"",
            "echo x > \\\\localhost\\c$\\data\\store\\a.json",
            "echo x > //127.0.0.1/c$/data/store/a.json",
            "echo x > \\\\?\\UNC\\localhost\\c$\\data\\store\\a.json",
            "echo x > \\\\.\\C:\\data\\store\\a.json",
            "echo x > ${HOME}/.claude/projects/p/s.jsonl",
            "echo x > %HOMEDRIVE%%HOMEPATH%\\.claude\\projects\\p\\s.jsonl",
            "echo x > $CLAUDE_CONFIG_DIR/projects/p/s.jsonl",
            "echo x > ${CLAUDE_CONFIG_DIR}/projects/p/s.jsonl",
        ] {
            assert!(shell_as(&g, "Bash", command), "{command}");
        }
        for command in [
            "Set-Content -Path ${env:USERPROFILE}\\.claude\\projects\\p\\s.jsonl -Value x",
            "Add-Content $env:HOMEDRIVE$env:HOMEPATH\\.claude\\projects\\p\\s.jsonl x",
            "Set-Content $env:CLAUDE_CONFIG_DIR\\projects\\p\\s.jsonl x",
            "Set-Content ${env:CLAUDE_CONFIG_DIR}\\projects\\p\\s.jsonl x",
            "Set-Content -Path:C:\\data\\store\\a.json -Value x",
            "Remove-Item -Path:C:\\data\\store\\thor.db",
            "Remove-Item -LiteralPath:\\\\localhost\\c$\\data\\store\\thor.db",
        ] {
            assert!(shell_as(&g, "PowerShell", command), "{command}");
        }
        // The same names a protected file only by these spellings; a look-alike is another file.
        for command in [
            "echo x > C:/data/store/notes.txt.",
            "echo x > $HOMEDRIVE/other/s.jsonl",
            "echo x > $HOMEPATHX/.claude/projects/p/s.jsonl",
            "echo x > \\\\otherhost\\c$\\data\\store\\a.json",
            "echo x > C:/data/store2/a.json::$DATA",
            "echo x > C:/Users/alex/.claude/projects/p/notes.txt::$DATA",
        ] {
            assert!(!shell_as(&g, "Bash", command), "{command}");
        }
    }

    #[test]
    fn a_short_8_3_name_is_read_as_the_folder_it_stands_for() {
        let tmp = std::env::temp_dir().join("thor-guard-8dot3-folder-with-a-long-name");
        let _ = std::fs::create_dir_all(&tmp);
        let listing = std::process::Command::new("cmd").args(["/c", &format!("for %I in (\"{}\") do @echo %~sI", tmp.display())]).output();
        let short = listing.ok().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default();
        if cfg!(windows) && short.contains('~') {
            let g = Protected::new(Some(&tmp), None, &[]);
            let input = serde_json::json!({ "command": format!("echo x > {short}\\thor.db") });
            assert!(refusal(&g, "Bash", Some(&input), None).is_some(), "{short}");
            let other = serde_json::json!({ "command": format!("echo x > {short}\\notes.txt") });
            assert!(refusal(&g, "Bash", Some(&other), None).is_none());
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn copying_or_extracting_a_whole_tree_into_a_protected_folder_is_refused() {
        for command in [
            "cp -r src C:/data/store",
            "cp -a src/. C:/data/store/",
            "cp -rf src C:/data/store",
            "cp -r src C:/Users/alex/.claude/projects/p",
            "robocopy C:\\scratch\\x C:\\data\\store /E",
            "robocopy C:\\scratch\\empty C:\\data\\store /MIR",
            "robocopy /MIR C:\\scratch\\empty C:\\data\\store",
            "robocopy C:\\scratch\\empty C:\\data\\store /PURGE",
            "xcopy C:\\s C:\\data\\store /E /Y",
            "rsync -a src/ C:/data/store/",
            "tar -xf a.tar -C C:/data/store",
            "tar -C C:/data/store -xzf a.tgz",
            "tar xf a.tar --directory=C:/data/store",
            "tar -xf a.tar --directory C:/data/store",
            "unzip a.zip -d C:/data/store",
            "unzip -o -d C:/data/store a.zip",
            "7z x a.zip -oC:/data/store",
            "git clone https://example.test/r.git C:/data/store",
            "git clone --depth 1 https://example.test/r.git C:/tools/thor2/bin",
            "git -C C:/scratch checkout-index -a --prefix=C:/data/store/",
        ] {
            assert!(shell(command), "{command}");
        }
        for command in [
            "Copy-Item C:\\s\\* C:\\data\\store -Recurse -Force",
            "Copy-Item -Path C:\\s -Destination C:\\data\\store -Recurse",
            "Expand-Archive -Path a.zip -DestinationPath C:\\data\\store -Force",
            "Expand-Archive a.zip C:\\data\\store",
            "Expand-Archive -Path a.zip -DestinationPath:C:\\data\\store",
        ] {
            assert!(shell_as(&guard(), "PowerShell", command), "{command}");
        }
        for command in [
            "cp -r C:/data/store C:/scratch/copy",
            "cp -r src C:/scratch/out",
            "cp -r src C:/data/store/sub",
            "cp notes.txt C:/data/store",
            "robocopy C:\\data\\store C:\\scratch /E",
            "robocopy C:\\a C:\\b /MIR",
            "tar -czf C:/scratch/a.tgz -C C:/data/store .",
            "tar -xf a.tar -C C:/scratch/out",
            "tar -tf a.tar",
            "unzip a.zip -d C:/scratch/out",
            "unzip -l a.zip",
            "git clone https://example.test/r.git C:/scratch/r",
            "git clone https://example.test/r.git",
            "rsync -a C:/data/store/ C:/scratch/copy/",
        ] {
            assert!(!shell(command), "{command}");
        }
        assert!(!shell_as(&guard(), "PowerShell", "Expand-Archive -Path a.zip -DestinationPath C:\\scratch\\out"));
        assert!(!shell_as(&guard(), "PowerShell", "Copy-Item C:\\data\\store -Destination C:\\scratch -Recurse"));
    }

    #[test]
    fn a_wildcard_or_piped_delete_of_a_protected_folder_is_refused() {
        for command in [
            "rm -rf C:/data/store/*",
            "rm -f C:/data/store/thor.db*",
            "rm C:/data/store/*.json",
            "rm -rf C:/data/*",
            "rm -rf C:/Users/alex/.claude/projects/p",
            "rm -rf C:/Users/alex/.claude/projects/p/some-session-id",
            "rm -rf C:/Users/alex/.claude/projects/*",
            "rm C:/Users/alex/.claude/projects/p/*.jsonl",
            "rm C:/Users/alex/.claude/projects/*/*.jsonl",
            "rm C:/tools/thor2/bin/*.exe",
            "ls C:/data/store | xargs rm",
            "ls C:/data/store/ | xargs -I{} rm {}",
            "find C:/data/store -delete",
            "find C:/data/store -type f -delete",
            "find C:/data/store -type f -exec rm {} \\;",
            "find C:/Users/alex/.claude/projects -name '*.jsonl' -delete",
            "find C:/data/store -name 'thor.db*' -delete",
        ] {
            assert!(shell(command), "{command}");
        }
        for command in [
            "Remove-Item C:\\data\\store\\* -Recurse -Force",
            "del C:\\data\\store\\*.*",
            "gci C:\\data\\store | Remove-Item",
            "Get-ChildItem C:\\data\\store -Recurse | Remove-Item -Force",
            "Get-ChildItem -Path:C:\\data\\store | Remove-Item",
            "Get-ChildItem C:\\data\\store | ForEach-Object { Remove-Item $_.FullName }",
            "gci C:\\data\\store | % { rm $_ }",
            "gci C:\\data\\store | select -expand FullName | rm",
            "Get-ChildItem C:\\Users\\alex\\.claude\\projects -Recurse | Remove-Item",
            "Remove-Item C:\\Users\\alex\\.claude\\projects\\p -Recurse",
            "gci C:\\data\\store\\* | Remove-Item",
        ] {
            assert!(shell_as(&guard(), "PowerShell", command), "{command}");
        }
        for command in [
            "rm -rf C:/scratch/*",
            "rm C:/data/store/*.txt",
            "rm C:/data/store/notes.txt",
            "rm -rf C:/data/store2/*",
            "rm -rf C:/data/other/*",
            "rm C:/Users/alex/.claude/projects/p/notes.txt",
            "rm C:/Users/alex/.claude/projects/p/*.txt",
            "rm C:/Users/alex/.claude/settings.json",
            "rm -rf C:/Users/alex/.claude/cache",
            "ls C:/data/store | wc -l",
            "ls C:/data/store | grep thor | head",
            "ls C:/scratch | xargs rm",
            "find C:/data/store -name '*.txt' -delete",
            "find C:/scratch -delete",
            "find C:/data/store -type f -print",
            "find C:/data/store -name 'thor.db*' | xargs ls",
        ] {
            assert!(!shell(command), "{command}");
        }
        for command in [
            "gci C:\\data\\store | Where-Object { $_.Length -gt 5 } | Remove-Item",
            "gci C:\\data\\store -Filter *.txt | Remove-Item",
            "gci C:\\scratch | Remove-Item",
            "Get-ChildItem C:\\data\\store | Measure-Object",
            "Remove-Item C:\\scratch\\* -Recurse",
            "Get-ChildItem C:\\data\\store | Select-Object Name",
        ] {
            assert!(!shell_as(&guard(), "PowerShell", command), "{command}");
        }
    }

    #[test]
    fn more_commands_that_write_a_file_are_refused() {
        for command in [
            "sed -n 'w C:/data/store/a.json' f",
            "sed 's/a/b/w C:/data/store/a.json' f",
            "sed -n '1w C:/Users/alex/.claude/projects/p/s.jsonl' f",
            "sed --in-place=.bak s/a/b/ C:/data/store/a.json",
            "touch C:/data/store/session-watermark.json",
            "touch C:/Users/alex/.claude/projects/p/s.jsonl",
            "certutil -urlcache -split -f http://example.test/x C:\\data\\store\\a.json",
            "certutil -decode in.b64 C:\\data\\store\\a.json",
            "bitsadmin /transfer j http://example.test/x C:\\data\\store\\a.json",
            "git archive -o C:/data/store/a.json HEAD",
            "git archive --output=C:/data/store/a.json HEAD",
            "git -C src diff --output=C:/data/store/a.json",
            "git -C C:/data/store reset --hard",
            "git -C C:/data/store clean -fdx",
            "git -C C:/data/store checkout .",
            "git -C C:/data/store stash",
            "git --git-dir=C:/data/store/.git --work-tree=C:/data/store checkout -f",
            "git -C C:/tools/thor2/bin pull",
            "curl -sSLo C:/data/store/a.json http://example.test/x",
            "curl -sSL -o C:/data/store/a.json http://example.test/x",
            "curl -oC:/data/store/a.json http://example.test/x",
            "curl http://example.test/x -fsSo C:/data/store/a.json",
            "wget -qO C:/data/store/a.json http://example.test/x",
            "wget -O C:/data/store/a.json -q http://example.test/x",
            "wget -qOC:/data/store/a.json http://example.test/x",
            "wget -q -P C:/data/store http://example.test/x",
            "wget -qP C:/data/store http://example.test/x",
        ] {
            assert!(shell(command), "{command}");
        }
        for command in [
            "iwr http://example.test/x -OutFile C:\\data\\store\\a.json",
            "Invoke-WebRequest -Uri http://example.test/x -OutFile:C:\\data\\store\\a.json",
            "Invoke-RestMethod http://example.test/x -OutFile C:\\data\\store\\a.json",
            "ni C:\\data\\store\\a.json -Force",
            "New-Item -Force -Path:C:\\data\\store\\a.json",
            "New-Item C:\\data\\store\\a.json -ItemType File -Force",
            "'x' | Tee-Object -FilePath C:\\data\\store\\a.json",
            "Get-Process | Export-Csv -Path:C:\\data\\store\\a.json",
            "Get-Process | Export-Clixml C:\\data\\store\\a.json",
            "Invoke-Expression \"Set-Content C:\\data\\store\\a.json x\"",
            "iex 'Remove-Item C:\\data\\store\\thor.db'",
        ] {
            assert!(shell_as(&guard(), "PowerShell", command), "{command}");
        }
        for command in [
            "sed -n 'w C:/scratch/out.txt' f",
            "sed -n 's/a/b/w C:/scratch/out.txt' f",
            "sed -n '1,5p' C:/data/store/a.json",
            "sed -n 's/new/w x/p' C:/data/store/a.json",
            "touch C:/data/store/notes.txt",
            "touch C:/scratch/x",
            "certutil -hashfile C:\\data\\store\\thor.db SHA256",
            "git archive -o C:/scratch/a.zip HEAD",
            "git diff --output=C:/scratch/a.patch",
            "git -C C:/data/store log --oneline",
            "git -C C:/data/store status",
            "git -C C:/data/store diff",
            "git -C C:/work reset --hard",
            "git push -o ci.skip origin main",
            "curl -sSLo C:/scratch/a.json http://example.test/x",
            "curl -sSL http://example.test/x -o C:/scratch/a.json",
            "curl -s http://example.test/x -H 'Accept: x'",
            "curl -s -X POST http://localhost:3000/e -d '{\"name\":\"t\"}'",
            "wget -qO- http://example.test/x",
            "wget -qO C:/scratch/x http://example.test/x",
            "wget -q -P C:/scratch http://example.test/x",
        ] {
            assert!(!shell(command), "{command}");
        }
        for command in [
            "iwr http://example.test/x -OutFile C:\\scratch\\a.json",
            "ni C:\\scratch\\a.json -Force",
            "New-Item -Path:C:\\scratch\\a.json -Force",
            "Get-Process | Export-Csv -Path C:\\scratch\\a.csv",
            "'x' | Tee-Object -FilePath C:\\scratch\\a.txt",
            "Invoke-Expression \"Set-Content C:\\scratch\\a.txt x\"",
            "Set-Content -Path:C:\\scratch\\a.txt -Value x",
        ] {
            assert!(!shell_as(&guard(), "PowerShell", command), "{command}");
        }
    }

    /// `text` as the base64 of its UTF-16 little-endian bytes, as `-EncodedCommand` wants it.
    fn encoded(text: &str) -> String {
        const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let bytes: Vec<u8> = text.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
        let mut out = String::new();
        for chunk in bytes.chunks(3) {
            let n = chunk.iter().enumerate().fold(0u32, |acc, (i, b)| acc | (u32::from(*b) << (16 - 8 * i)));
            for k in 0..4 {
                if k <= chunk.len() {
                    out.push(ALPHABET[((n >> (18 - 6 * k)) & 63) as usize] as char);
                } else {
                    out.push('=');
                }
            }
        }
        out
    }

    #[test]
    fn an_encoded_powershell_command_is_read_before_it_is_judged() {
        let bad = encoded("Set-Content C:\\data\\store\\unpin-owner-go.json '[]'");
        let good = encoded("Set-Content C:\\scratch\\notes.txt 'x'");
        for flag in ["-EncodedCommand", "-enc", "-e", "-ec", "-ENCODEDCOMMAND"] {
            assert!(shell_as(&guard(), "PowerShell", &format!("powershell -NoProfile {flag} {bad}")), "{flag}");
            assert!(shell_as(&guard(), "Bash", &format!("pwsh {flag} {bad}")), "{flag}");
            assert!(!shell_as(&guard(), "PowerShell", &format!("powershell -NoProfile {flag} {good}")), "{flag}");
        }
        assert!(shell_as(&guard(), "Bash", &format!("powershell.exe -NonInteractive -EncodedCommand {bad}")));
        assert!(!shell_as(&guard(), "PowerShell", "powershell -EncodedCommand not-base64!"));
        assert_eq!(forms::decode_encoded_command(&encoded("Add-Content a b")).as_deref(), Some("Add-Content a b"));
    }

    #[test]
    fn the_file_tools_read_a_filepath_key_and_any_case_of_the_tool_name() {
        let g = guard();
        for (tool, key) in [("Write", "filePath"), ("Edit", "filePath"), ("write", "file_path"), ("edit", "filePath"), ("NotebookEdit", "notebookPath"), ("MultiEdit", "filePath")] {
            let input = serde_json::json!({ key: "C:/data/store/unpin-owner-go.json" });
            assert!(refusal(&g, tool, Some(&input), None).is_some(), "{tool} {key}");
            let fine = serde_json::json!({ key: "C:/work/notes.md" });
            assert!(refusal(&g, tool, Some(&fine), None).is_none(), "{tool} {key}");
        }
        let read = serde_json::json!({ "filePath": "C:/data/store/thor.db" });
        assert!(refusal(&g, "Read", Some(&read), None).is_none());
    }

    #[test]
    fn the_path_helpers_read_the_names_the_file_system_reads() {
        assert_eq!(forms::clean_path("c:/data/store/thor.db."), "c:/data/store/thor.db");
        assert_eq!(forms::clean_path("c:/data/store./thor.db "), "c:/data/store/thor.db");
        assert_eq!(forms::clean_path("c:/data/store/thor.db::$data"), "c:/data/store/thor.db");
        assert_eq!(forms::clean_path("c:/data/store/thor.db:stream:$data"), "c:/data/store/thor.db");
        assert_eq!(forms::unshare("//localhost/c$/data/store/x"), "c:/data/store/x");
        assert_eq!(forms::unshare("echo //127.0.0.1/d$/x"), "echo d:/x");
        assert_eq!(forms::unshare("//otherhost/c$/x"), "//otherhost/c$/x");
        assert!(forms::wildcard_matches("*.json", "a.json") && forms::wildcard_matches("thor.db*", "thor.db-wal") && forms::wildcard_matches("*", "x"));
        assert!(!forms::wildcard_matches("*.txt", "thor.db") && !forms::wildcard_matches("thor.d?", "thor.db-wal"));
        assert_eq!(forms::split_colon_params(&["-Path:C:\\x".to_string(), "-c:".to_string(), "--a:b".to_string()]), ["-Path", "C:\\x", "-c:", "--a:b"]);
        assert_eq!(forms::cluster_values(&["-sSLo".to_string(), "f".to_string()], &['o']), [('o', "f".to_string())]);
        assert_eq!(forms::cluster_values(&["-oPATH".to_string()], &['o']), [('o', "PATH".to_string())]);
    }

    /// Not run by default: like `replay_the_real_calls`, but over the real-calls file itself
    /// (THOR_REPLAY_FILE, one JSON row per call: `k`, `tool`, `in`, `cwd`, `ok`).
    #[test]
    #[ignore]
    fn replay_the_real_calls_file() {
        let var = |n: &str| std::env::var(n).unwrap_or_default();
        let g = Protected::new(
            Some(Path::new(&var("THOR_REPLAY_STORE"))),
            Some(Path::new(&var("THOR_REPLAY_PROGRAMS"))),
            &[PathBuf::from(var("THOR_REPLAY_CLAUDE"))],
        );
        let text = std::fs::read_to_string(var("THOR_REPLAY_FILE")).unwrap_or_default();
        let (mut total, mut refused) = (0, 0);
        for line in text.lines().filter(|l| l.contains("\"k\":\"c\"")) {
            let Ok(row) = serde_json::from_str::<serde_json::Value>(line) else { continue };
            let tool = row["tool"].as_str().unwrap_or("");
            total += 1;
            let cwd = row["cwd"].as_str();
            if refusal(&g, tool, row.get("in"), cwd).is_some() {
                refused += 1;
                println!("REFUSED ok={} {} {}", row["ok"], tool, row["in"].to_string().chars().take(400).collect::<String>());
            }
        }
        println!("calls {total}, refused {refused}");
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
