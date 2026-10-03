//! Command anchors that never fired, and why a call cannot reach them.
//!
//! WHY THIS EXISTS. A Rule or Orientation bound to a Target of kind `command`
//! fires on two surfaces. At a real call it fires only when a call's simple
//! command STARTS with the anchor (`shell::anchor_reaches`), or - for a call
//! that is not a shell command - when the call's WHOLE tool name equals it
//! (`mcp__thor__remember`, `Agent`). At the owner's own prompt it fires when
//! the prompt contains the anchor's last word or phrase (`prompt::names_target`),
//! so an anchor written as a keyword ("Raspberry Pi 5", "remember") is not dead
//! by itself: it fires when the owner types it. What no call can ever reach is
//! an anchor that is a stored sentence, a tool's short name, a fragment without
//! its program ("push --all") or a program that is gone ("thor.exe import") -
//! and when the owner never types it either, the rule never fires and nothing
//! says so. A field review of one store found about 205 such items, and a
//! second look found that 33 of 37 global prose anchors DID fire when their
//! phrase was typed: so this module only ever says "never fired", never "dead".
//! `doctor` counts them (`command anchors never fired`) and the write gate says
//! so in a note when a new anchor is of this kind (never a refusal: a rule
//! waiting for a tool that does not exist yet, or for a phrase the owner will
//! type, is legitimate).
//!
//! HOW AN ANCHOR IS JUDGED, in this order, stopping at the first that holds.
//! Nothing here guesses; every verdict names the fact it stands on.
//! 1. FIRED BEFORE, on either surface. Some real call in the log
//!    (`legit::Corpus`, the calls a rule was served on, refused ones included)
//!    was reached by the anchor, by the live matcher itself
//!    (`shell::reaches_any`); or some item was ever served by the owner's own
//!    prompt on the anchor's phrase (the `(prompt) word` place a prompt firing
//!    records). Either one: not judged. The named limit is the log's own: only
//!    a call or prompt that fired some rule is in it at all.
//! 2. NOT DECIDABLE, so not judged: an anchor that is more than one simple
//!    command, or whose first word is a path (`./x.sh`, `C:\x\y.exe`), a `~` or
//!    a `$` - what they name depends on a folder or a variable no reader here
//!    knows.
//! 3. STARTS WITH A FLAG (`-f x`): a call starts with its program, never a flag.
//! 4. BARE TOOL NAME: one word, equal to the short name of a tool THOR itself
//!    serves (`remember`, `shelve`), and not a program here. A tool call offers
//!    its whole name, so the short one matches no tool call; it still fires
//!    when the owner's prompt contains the word.
//! 5. A PROGRAM HERE, so a call can reach it and not judged: the first word is
//!    an executable on PATH or in the folders given (the store's `bin`), a
//!    PowerShell cmdlet by shape (an approved verb, a hyphen, a noun), a shell
//!    builtin or alias, the name of a tool the hook sees (`mcp__...`, or one
//!    capitalised word such as `Agent`), or the program of some real call in the
//!    log - which is how a command that lives only on another machine (`docker`
//!    over ssh) is recognised without claiming it exists here.
//! 6. FRAGMENT OF A LONGER COMMAND: the anchor's words stand, right after a
//!    program (or after its flags), in a real call (`push --all` in `git push
//!    --all`). The program is named - derived from a recorded call, never from
//!    a list.
//! 7. Otherwise NEVER REACHED AND NOT A PROGRAM HERE: no real call started with
//!    it and its first word is not a program here. This does NOT claim the
//!    command does not exist anywhere: it may live on another machine.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::legit::Corpus;
use crate::shell::{self, Dialect, Segment};

/// Why a call cannot reach an anchor that has never fired.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reason {
    /// One word, the short name of a tool THOR serves.
    BareToolName { tail: String },
    /// The first word is a flag.
    FlagFirst,
    /// The anchor's words stand after `program` in a real call.
    Fragment { program: String, call: String },
    /// No real call started with it and its first word is not a program here.
    NeverReached { first: String },
}

impl Reason {
    /// The short name of the class, as `doctor` counts it.
    pub fn class(&self) -> &'static str {
        match self {
            Reason::BareToolName { .. } => "bare tool name",
            Reason::FlagFirst => "starts with a flag",
            Reason::Fragment { .. } => "fragment of a longer command",
            Reason::NeverReached { .. } => "never reached, not a program here",
        }
    }

    /// The class and the one-line fix, for a list.
    pub fn short(&self) -> String {
        match self {
            Reason::BareToolName { tail } => format!("{}: as a command bind mcp__thor__{tail}", self.class()),
            Reason::FlagFirst => format!("{}: as a command put the program word first", self.class()),
            Reason::Fragment { program, .. } => format!("{}: as a command start it with '{program}'", self.class()),
            Reason::NeverReached { first } => format!("{}: no real call started with '{first}'", self.class()),
        }
    }

    /// Why no call can reach it, in plain words. Always says what the anchor
    /// does match, so a keyword is never called dead.
    pub fn why(&self, anchor: &str) -> String {
        let how = format!(
            "as a command, '{anchor}' only matches a call that starts with it, and otherwise it fires only when the \
             owner's prompt contains it - and neither has happened yet"
        );
        match self {
            Reason::BareToolName { tail } => format!(
                "'{tail}' is the short name of a tool THOR serves, and a tool call offers its WHOLE name \
                 (mcp__thor__{tail}), so it matches no tool call; {how}"
            ),
            Reason::FlagFirst => format!("'{anchor}' starts with a flag, and a call starts with its program; {how}"),
            Reason::Fragment { program, call } => {
                format!("real calls carry these words after '{program}' ('{call}'), never at their start; {how}")
            }
            Reason::NeverReached { first } => format!(
                "no real call in the log ever started with '{anchor}', and '{first}' is not found on PATH or in THOR's \
                 own bin folder, and is not a cmdlet or a tool name the hook sees (it may exist elsewhere: another \
                 machine, a folder not on PATH); {how}"
            ),
        }
    }

    /// What to do about it. Each fix is conditional on what the anchor was
    /// meant to be: a command, a keyword the owner types, or a file or action.
    pub fn fix(&self, anchor: &str) -> String {
        match self {
            Reason::BareToolName { tail } => {
                let moment = if matches!(tail.as_str(), "remember" | "revise") {
                    ", or bind the remember moment, which fires for both of them"
                } else {
                    ""
                };
                format!(
                    "if you meant the tool, bind the whole tool name as your tool list shows it (mcp__thor__{tail} when the \
                     server is called thor){moment}; if you meant the word the owner types, leave it"
                )
            }
            Reason::FlagFirst => "if it is a command, write the program word first, as a real call types it; if it is a \
                 keyword the owner types, leave it"
                .to_string(),
            Reason::Fragment { program, .. } => format!(
                "if it is a command, start it with the program word ('{program} {anchor}'), or anchor the program alone and \
                 name the rest in the rule's text; if it is a keyword the owner types, leave it"
            ),
            Reason::NeverReached { .. } => "if it is a command, start it with the program word as a real call types it; if it \
                 is a keyword or phrase the owner types, it is fine as it stands; if the rule is about a file or an action, \
                 bind a path or a moment instead; if it waits for a program or tool that does not exist yet, leave it"
                .to_string(),
        }
    }
}

/// How `judge` reads PowerShell cmdlet names: the approved verbs.
const CMDLET_VERBS: &[&str] = &[
    "add", "approve", "assert", "backup", "block", "build", "checkpoint", "clear", "close", "compare", "complete", "compress",
    "confirm", "connect", "convert", "convertfrom", "convertto", "copy", "debug", "deny", "deploy", "disable", "disconnect",
    "dismount", "edit", "enable", "enter", "exit", "expand", "export", "find", "foreach", "format", "get", "grant", "group",
    "hide", "import", "initialize", "install", "invoke", "join", "limit", "lock", "measure", "merge", "mount", "move", "new",
    "open", "optimize", "out", "ping", "pop", "protect", "publish", "push", "read", "receive", "redo", "register", "remove",
    "rename", "repair", "request", "reset", "resize", "resolve", "restart", "restore", "resume", "revoke", "save", "search",
    "select", "send", "set", "show", "skip", "split", "start", "step", "stop", "submit", "suspend", "switch", "sync", "test",
    "trace", "unblock", "undo", "uninstall", "unlock", "unprotect", "unpublish", "unregister", "update", "use", "wait", "watch",
    "where", "write",
];

/// Shell builtins and the usual PowerShell and cmd aliases: not files on PATH,
/// still things a call starts with.
const BUILTINS: &[&str] = &[
    ".", "alias", "bg", "break", "builtin", "cat", "cd", "chdir", "clear", "cls", "command", "continue", "copy", "cp", "declare",
    "del", "dir", "dirs", "echo", "eval", "exec", "exit", "export", "false", "fg", "gc", "gci", "gi", "history", "iex", "iwr",
    "irm", "jobs", "kill", "let", "local", "ls", "md", "mkdir", "move", "mv", "popd", "printf", "ps", "pushd", "pwd", "rd",
    "read", "ren", "return", "rm", "rmdir", "sc", "set", "shift", "sl", "sleep", "source", "start", "test", "trap", "true",
    "type", "ulimit", "umask", "unalias", "unset", "wait",
];

/// Programs that take a script or code as their argument: what follows them is
/// never a subcommand of theirs.
const INTERPRETERS: &[&str] = &[
    "bash", "cmd", "deno", "java", "node", "perl", "php", "powershell", "pwsh", "python", "python3", "py", "ruby", "sh",
    "zsh", "bun", "npx",
];

/// Whether `word` has the shape of a PowerShell cmdlet: an approved verb, one
/// hyphen, a noun (`Get-Command`, `start-process`, never `vectors-status`).
fn is_cmdlet_shape(word: &str) -> bool {
    let Some((verb, noun)) = word.split_once('-') else { return false };
    CMDLET_VERBS.contains(&verb.to_lowercase().as_str())
        && !noun.is_empty()
        && noun.chars().all(|c| c.is_ascii_alphanumeric())
        && noun.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
}

/// Whether `word` is a tool name the hook sees: `mcp__...`, or - alone - one
/// capitalised word (`Agent`, `Artifact`, `Skill`).
fn is_tool_name_shape(word: &str, alone: bool) -> bool {
    word.starts_with("mcp__")
        || (alone
            && word.chars().next().is_some_and(|c| c.is_ascii_uppercase())
            && word.chars().all(|c| c.is_ascii_alphanumeric()))
}

/// A program name as it is compared: no folder, no `.exe`, lowercase.
fn program_key(word: &str) -> String {
    let base = word.rsplit(['/', '\\']).next().unwrap_or(word).to_lowercase();
    base.strip_suffix(".exe").map(str::to_string).unwrap_or(base)
}

/// Whether the text is more than one simple command or carries a
/// substitution: such an anchor is read by the live matcher by its raw text,
/// which this module does not judge.
fn is_compound(anchor: &str) -> bool {
    ["&&", "||", ";", "|", "$(", "`", "\n"].iter().any(|s| anchor.contains(s))
}

/// The executable file extensions of this machine, lowercase with the dot. On a
/// platform whose files have no such extension: one empty entry.
fn executable_extensions() -> Vec<String> {
    if !cfg!(windows) {
        return vec![String::new()];
    }
    let from_env = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_string());
    let mut out: Vec<String> =
        from_env.split(';').map(|e| e.trim().to_lowercase()).filter(|e| e.starts_with('.')).collect();
    out.push(".ps1".to_string());
    out.push(String::new());
    out
}

fn is_executable_file(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else { return false };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// The values of an item's `command` bindings, in order.
pub fn command_anchors(item: &model::item::Item) -> Vec<&str> {
    item.bindings
        .iter()
        .filter_map(|b| match b {
            model::item::Binding::Target { kind: model::item::TargetKind::Command, value } => Some(value.as_str()),
            _ => None,
        })
        .collect()
}

/// The folders besides `PATH` where THOR's own programs live: the `bin` folder
/// beside the store, and the folder of the running program.
pub fn own_dirs(db: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(parent) = db.and_then(Path::parent) {
        dirs.push(parent.join("bin"));
    }
    if let Some(exe_dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf)) {
        dirs.push(exe_dir);
    }
    dirs
}

/// The corpus's calls without the pseudo-calls a prompt firing leaves in it
/// (`(prompt) word`): those are not typed commands.
fn real_calls(corpus: &Corpus) -> impl Iterator<Item = &crate::legit::RealCall> {
    corpus.calls().iter().filter(|c| !c.text.starts_with("(prompt)"))
}

/// Judges anchors against the real calls of one log and one machine.
pub struct Judge<'a> {
    corpus: &'a Corpus,
    /// The calls a session had refused, cut into simple commands: a refused
    /// call was still reached by the anchor.
    refused: Vec<Vec<Segment>>,
    tails: HashSet<String>,
    dirs: Vec<PathBuf>,
    extensions: Vec<String>,
    /// The programs real calls ran, as `program_key` reads them.
    run: HashSet<String>,
}

impl<'a> Judge<'a> {
    /// A judge for this machine: the folders on `PATH` plus `extra_dirs` (the
    /// store's `bin` folder and the folder of the running program). `tool_tails`
    /// are the short names of the tools THOR serves.
    pub fn new(corpus: &'a Corpus, tool_tails: &[String], extra_dirs: &[PathBuf]) -> Self {
        let mut dirs: Vec<PathBuf> = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
        dirs.extend(extra_dirs.iter().cloned());
        Self::with_dirs(corpus, tool_tails, dirs)
    }

    /// A judge that looks for programs in exactly `dirs` (a test, or a caller
    /// that already knows its folders).
    pub fn with_dirs(corpus: &'a Corpus, tool_tails: &[String], dirs: Vec<PathBuf>) -> Self {
        let refused: Vec<Vec<Segment>> =
            corpus.refused().iter().map(|r| shell::segments(&r.text, Dialect::Sh)).collect();
        let mut run = HashSet::new();
        let all = real_calls(corpus).map(|c| c.segments.as_slice()).chain(refused.iter().map(Vec::as_slice));
        for segments in all {
            for seg in segments.iter().filter(|s| !s.flat) {
                if !seg.program.is_empty() {
                    run.insert(program_key(&seg.program));
                }
                if let Some(first) = seg.words.first() {
                    run.insert(program_key(first));
                }
            }
        }
        Judge {
            corpus,
            refused,
            tails: tool_tails.iter().map(|t| t.to_lowercase()).collect(),
            dirs,
            extensions: executable_extensions(),
            run,
        }
    }

    /// Whether `anchor` ever fired: some real call was reached by it, or some
    /// item was served by the owner's own prompt on its phrase.
    fn fired_before(&self, anchor: &str) -> bool {
        let phrase = model::normalize::last_segment(&model::normalize::normalize_target(anchor)).to_string();
        real_calls(self.corpus).any(|c| c.reached_by(anchor))
            || self.refused.iter().any(|segs| shell::reaches_any(anchor, segs))
            || self.corpus.prompted(&phrase)
    }

    /// Whether `word` names an executable file in one of the folders.
    fn on_disk(&self, word: &str) -> bool {
        let lower = word.to_lowercase();
        let names: Vec<&str> = if lower == word { vec![word] } else { vec![word, lower.as_str()] };
        self.dirs.iter().any(|dir| {
            names.iter().any(|name| {
                self.extensions.iter().any(|ext| {
                    let file = if ext.is_empty() { (*name).to_string() } else { format!("{name}{ext}") };
                    is_executable_file(&dir.join(file))
                })
            })
        })
    }

    /// Whether `word` is a program here (item 5 of this module's doc comment).
    fn is_program(&self, word: &str) -> bool {
        let key = program_key(word);
        self.run.contains(&key) || BUILTINS.contains(&key.as_str()) || is_cmdlet_shape(word) || self.on_disk(word)
    }

    /// The program and the call of a real call in which `words` stand as the
    /// continuation of a program: right after it, or after nothing but flags
    /// and their values (`serve --db X search --kind report`). An interpreter
    /// (`python`, `node`...) takes a script, not a subcommand, so what follows
    /// it is never read as a fragment.
    fn fragment_of(&self, words: &[String]) -> Option<(String, String)> {
        let n = words.len();
        for call in real_calls(self.corpus) {
            for seg in call.segments.iter().filter(|s| !s.flat) {
                let program = program_key(seg.head.first().map(String::as_str).unwrap_or(""));
                if program.is_empty() || INTERPRETERS.contains(&program.as_str()) || seg.head.len() <= n {
                    continue;
                }
                let mut k = 1;
                let hit = loop {
                    if k + n > seg.head.len() {
                        break false;
                    }
                    if seg.head[k..k + n].iter().zip(words).all(|(w, a)| w.to_lowercase() == *a) {
                        break true;
                    }
                    // Step over a flag, and over the one value it may carry.
                    if !seg.head[k].starts_with('-') {
                        break false;
                    }
                    k += if seg.head.get(k + 1).is_some_and(|v| !v.starts_with('-')) { 2 } else { 1 };
                };
                if hit {
                    let shown: String = call.text.chars().take(80).collect();
                    return Some((program, shown));
                }
            }
        }
        None
    }

    /// Why `anchor` cannot be reached, or `None` when it can, or when that
    /// cannot be decided (see this module's doc comment).
    pub fn judge(&self, anchor: &str) -> Option<Reason> {
        let anchor = anchor.trim();
        if anchor.is_empty() || is_compound(anchor) {
            return None;
        }
        if self.fired_before(anchor) {
            return None;
        }
        let words: Vec<String> = anchor.split_whitespace().map(|w| w.to_lowercase()).collect();
        let first_typed = anchor.split_whitespace().next()?.trim_matches(|c| c == '\'' || c == '"');
        if first_typed.starts_with('-') {
            return Some(Reason::FlagFirst);
        }
        if first_typed.contains(['/', '\\']) || first_typed.starts_with(['.', '~', '$']) {
            return None;
        }
        let alone = words.len() == 1;
        let first = first_typed.to_lowercase();
        if alone && self.tails.contains(&first) && !self.is_program(first_typed) {
            return Some(Reason::BareToolName { tail: first });
        }
        if self.is_program(first_typed) || is_tool_name_shape(first_typed, alone) {
            return None;
        }
        if let Some((program, call)) = self.fragment_of(&words) {
            return Some(Reason::Fragment { program, call });
        }
        Some(Reason::NeverReached { first: first_typed.to_string() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use thor_core::event_store::EventStore;

    const S1: &str = "0de7dc52-1b97-4b66-948f-61e4438043b2";

    fn tails() -> Vec<String> {
        ["remember", "revise", "shelve", "lookup"].iter().map(|s| s.to_string()).collect()
    }

    fn served(store: &mut EventStore, trigger: &str) {
        crate::deliver::record_delivery_with_trigger(store, S1, S1, "hook", "2026-09-28T00:00:00Z", &["r".to_string()], Some(trigger));
    }

    /// A folder holding one fake program, `realtool`.
    fn bin_with_realtool() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let name = if cfg!(windows) { "realtool.exe" } else { "realtool" };
        let file = dir.path().join(name);
        std::fs::write(&file, b"x").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        dir
    }

    fn verdict(calls: &[&str], anchor: &str) -> Option<Reason> {
        let mut store = EventStore::in_memory().unwrap();
        for call in calls {
            served(&mut store, call);
        }
        let corpus = Corpus::load(&store);
        let bin = bin_with_realtool();
        Judge::with_dirs(&corpus, &tails(), vec![bin.path().to_path_buf()]).judge(anchor)
    }

    #[test]
    fn a_bare_tool_short_name_is_unreachable_and_names_the_full_form() {
        let found = verdict(&[], "remember").expect("remember can never equal mcp__thor__remember");
        assert_eq!(found, Reason::BareToolName { tail: "remember".to_string() });
        assert!(found.fix("remember").contains("mcp__thor__remember"), "{}", found.fix("remember"));
        assert!(found.fix("remember").contains("remember moment"), "{}", found.fix("remember"));
        assert!(matches!(verdict(&[], "Shelve"), Some(Reason::BareToolName { .. })), "anchors match without case");
        assert!(verdict(&[], "lookup").is_some());
    }

    #[test]
    fn the_whole_tool_name_and_a_capitalised_tool_name_are_reachable() {
        assert_eq!(verdict(&[], "mcp__thor__remember"), None);
        assert_eq!(verdict(&[], "Agent"), None);
        assert_eq!(verdict(&[], "Artifact"), None);
    }

    #[test]
    fn a_program_on_the_path_is_reachable_and_a_missing_one_is_not() {
        assert_eq!(verdict(&[], "realtool"), None);
        assert_eq!(verdict(&[], "realtool run --fast"), None);
        let found = verdict(&[], "geankerd feit").expect("not a program");
        assert_eq!(found, Reason::NeverReached { first: "geankerd".to_string() });
        assert!(found.why("geankerd feit").contains("may"), "it must not claim the command exists nowhere");
        assert!(matches!(verdict(&[], "Raspberry Pi 5"), Some(Reason::NeverReached { .. })));
        assert!(matches!(verdict(&[], "upload_file"), Some(Reason::NeverReached { .. })));
    }

    #[test]
    fn a_program_that_shadows_a_tool_name_is_a_program() {
        let store = EventStore::in_memory().unwrap();
        let corpus = Corpus::load(&store);
        let dir = tempfile::tempdir().unwrap();
        let name = if cfg!(windows) { "lookup.exe" } else { "lookup" };
        let file = dir.path().join(name);
        std::fs::write(&file, b"x").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let judge = Judge::with_dirs(&corpus, &tails(), vec![dir.path().to_path_buf()]);
        assert_eq!(judge.judge("lookup"), None);
    }

    #[test]
    fn an_anchor_the_owners_prompt_fired_is_not_judged_and_the_wording_never_calls_a_keyword_dead() {
        assert!(verdict(&[], "Raspberry Pi 5").is_some());
        assert_eq!(verdict(&["(prompt) raspberry pi 5"], "Raspberry Pi 5"), None);
        assert_eq!(verdict(&["(prompt) remember, git"], "remember"), None);
        // A prompt that fired on another phrase clears nothing.
        assert!(verdict(&["(prompt) raspberry"], "Raspberry Pi 5").is_some());
        // And a prompt firing is not a program that a real call ran.
        let found = verdict(&["(prompt) geankerd"], "geankerd feit").expect("still never fired");
        assert!(matches!(found, Reason::NeverReached { .. }), "{found:?}");
        let why = found.why("geankerd feit");
        assert!(why.contains("only matches a call that starts with it") && why.contains("owner's prompt contains it"), "{why}");
        let fix = found.fix("geankerd feit");
        assert!(fix.contains("if it is a command") && fix.contains("fine as it stands"), "{fix}");
        let bare = verdict(&[], "remember").unwrap().why("remember");
        assert!(bare.contains("owner's prompt contains it"), "{bare}");
    }

    #[test]
    fn an_anchor_that_fired_before_is_reachable_whatever_its_shape() {
        assert!(verdict(&[], "geankerd feit").is_some());
        assert_eq!(verdict(&["geankerd feit now"], "geankerd feit"), None);
        assert_eq!(verdict(&["shelve"], "shelve"), None);
    }

    #[test]
    fn a_call_its_own_session_had_refused_still_counts_as_reached() {
        let mut store = EventStore::in_memory().unwrap();
        served(&mut store, "sshpass -p x host");
        crate::deliver::record_gate_outcome(&mut store, S1, S1, "hook", "2026-09-28T00:00:01Z", true, "r", "sshpass -p x host");
        let corpus = Corpus::load(&store);
        assert!(corpus.calls().is_empty() && corpus.refused().len() == 1);
        let judge = Judge::with_dirs(&corpus, &tails(), Vec::new());
        assert_eq!(judge.judge("sshpass"), None);
    }

    #[test]
    fn a_powershell_cmdlet_shape_is_a_program_but_a_made_up_hyphenated_word_is_not() {
        assert_eq!(verdict(&[], "Get-Command"), None);
        assert_eq!(verdict(&[], "start-process thor.exe x"), None);
        assert!(matches!(verdict(&[], "vectors-status"), Some(Reason::NeverReached { .. })));
        assert!(matches!(verdict(&[], "git-filter-repo"), Some(Reason::NeverReached { .. })));
    }

    #[test]
    fn a_command_that_lives_on_another_machine_is_judged_only_by_what_real_calls_did() {
        // No real call ran docker: flagged as never reached, without claiming it is nowhere.
        let found = verdict(&[], "docker inspect thor-mcp").expect("never reached");
        assert!(matches!(&found, Reason::NeverReached { first } if first == "docker"), "{found:?}");
        assert!(found.why("docker inspect thor-mcp").contains("another machine"));
        // A real call ran it over ssh with other arguments: it is a program that the log knows.
        assert_eq!(verdict(&["ssh nas docker ps"], "docker inspect thor-mcp"), None);
        assert_eq!(verdict(&["ssh nas docker ps"], "docker"), None);
    }

    #[test]
    fn a_fragment_is_named_after_the_program_a_real_call_puts_in_front_of_it() {
        let found = verdict(&["git push --all origin"], "push --all").expect("a fragment");
        assert!(matches!(&found, Reason::Fragment { program, .. } if program == "git"), "{found:?}");
        assert!(found.fix("push --all").contains("git push --all"), "{}", found.fix("push --all"));
        // After a flag and its value: still the program's own continuation.
        let found = verdict(&["serve.exe --db X search --kind report"], "search --kind report").expect("a fragment");
        assert!(matches!(&found, Reason::Fragment { program, .. } if program == "serve"), "{found:?}");
        // An interpreter takes a script, not a subcommand; and a word between is not a flag.
        assert!(matches!(verdict(&["python farm.py rebuild-prod"], "rebuild-prod"), Some(Reason::NeverReached { .. })));
        assert!(matches!(verdict(&["git foo push --all"], "push --all"), Some(Reason::NeverReached { .. })));
        // The same text with no recorded call to derive it from is just never reached.
        assert!(matches!(verdict(&[], "push --all"), Some(Reason::NeverReached { .. })));
        // A fragment whose words are not in the call is not claimed.
        assert!(matches!(verdict(&["git push origin"], "push --all"), Some(Reason::NeverReached { .. })));
    }

    #[test]
    fn a_flag_first_anchor_is_unreachable() {
        assert_eq!(verdict(&[], "--force"), Some(Reason::FlagFirst));
    }

    #[test]
    fn what_cannot_be_decided_is_not_judged() {
        assert_eq!(verdict(&[], "./deploy.sh now"), None);
        assert_eq!(verdict(&[], "C:\\tools\\x.exe run"), None);
        assert_eq!(verdict(&[], "cd x && make"), None);
        assert_eq!(verdict(&[], ""), None);
    }

    #[test]
    fn a_shell_builtin_is_a_program() {
        assert_eq!(verdict(&[], "cd"), None);
        assert_eq!(verdict(&[], "echo"), None);
    }
}
