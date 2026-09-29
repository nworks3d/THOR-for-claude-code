//! The ONE definition of "this anchor reaches this call": a Bash or PowerShell
//! command line cut into the simple commands it runs.
//!
//! WHY THIS EXISTS. A command rule is bound to a command anchor ("git push",
//! "docker exec") and forbids a literal in the call, or requires an answer in
//! it. Until this module the anchor was compared with the FIRST words of the
//! whole line, and the literal was looked for anywhere in it. Measured on the
//! owner's own transcripts (2026-09-28): about half of the real calls a rule
//! was written for never reached it - `cd /srv && docker exec ...`, a command
//! over `ssh`, a commit message fed on a heredoc, a loop, `sudo -u root ...` -
//! while a literal that merely sat in a LATER, unrelated command of the same
//! line was refused as if it belonged to the reached one. So a command tooth
//! is now read per SEGMENT (one simple command) and per UNIT (where its
//! literal is looked for), the same way by every reader: the three hard guards
//! (`serve::guard_verdict`), serving (`serve::rank`) and the write-time proof
//! (`serve::prove`).
//!
//! SEGMENTS (`segments`). The line is scanned once: `;`, `&&`, `||`, `|`, `&`,
//! a newline, `(`/`)` and `{`/`}` end a segment; a redirect never does.
//! Quoting follows the dialect: sh single and double quotes with backslash
//! escapes, `$'...'` and line continuations; PowerShell's doubled quotes,
//! backtick escapes and here-strings. A heredoc body or a here-string is DATA
//! of its segment, never a command of its own - unless it is fed to a shell
//! (`bash -s <<EOF`, `ssh host bash -s <<EOF`, or a bare `ssh host <<EOF`).
//! Nested commands are scanned by this same scanner: `$(...)`, backticks,
//! `<(...)`, `>(...)`, and the string a wrapper runs: `sh`/`bash`/`zsh`/
//! `dash`/`ksh -c` (any single-dash flag cluster carrying `c`), `ssh
//! [options] host command...` (argv joined with spaces, as OpenSSH does;
//! options that take an argument skip it), `eval`, `powershell`/`pwsh
//! -Command`, `cmd /c` and `/k`, and `Invoke-Expression`/`iex`. Descent stops
//! at depth `MAX_DEPTH` (see "WHERE READING STOPS").
//! Arithmetic - `((...))`, `$((...))`, `$[...]` - is one word of data, read
//! as bash reads it: a `<<` in it is a shift, never a heredoc (read as one,
//! it hid every line after it), and the `$(...)` and backtick commands in it
//! still run, so they are read. Where bash reads `((cmd) )` or `$((cmd) )`
//! as commands - the paren after `((` does not close right before a second
//! `)` - so does this scanner.
//!
//! THE HEAD (`Segment::head`). The words with shell syntax taken off the
//! front: reserved words (`do`, `then`, `if`, `!`, `{` ...), `NAME=value`
//! assignments, and the pass-through prefixes `sudo`, `env`, `nohup`, `exec`,
//! `command`, `builtin`, `nice`, `timeout` with their own options; then the
//! first word loses its directory and a `.exe` suffix, and git's own global
//! options (`-C dir`, `-c k=v`, `--git-dir`, `--no-pager` ...) are removed.
//! An anchor reaches a segment when its words are a prefix of the segment's
//! words as typed or of its head, compared case-insensitively. An anchor is
//! tokenized once with the PowerShell dialect (a backslash is literal, so a
//! Windows path is written as typed); an anchor that is itself more than one
//! simple command keeps the old whole-text comparison against the segment's
//! own text (no such anchor carries a check). An anchor whose first word ends
//! in `.exe` reaches only a call whose program is that exact name, `.exe`
//! included (after its directory is dropped): `install.exe` never reaches
//! coreutils `install`, while an anchor written without `.exe` reaches both
//! spellings.
//!
//! THE FOLDER (`Segment::dir`). A segment runs in the folder the `cd`/`pushd`/
//! `popd` chain before it puts it in (in PowerShell also `chdir`, `sl`,
//! `Set-Location`, `Pop-Location`) - read as the shell reads each one
//! (`cd_effect`): one the shell refuses (an operand too many, an option it
//! does not know, an empty folder) moves nothing, and neither does a name
//! the shell does not have (bash has no `chdir`, `sl` or `Set-Location`) or
//! a program run in its place (`sudo cd`, `env cd`, `/usr/bin/cd`). A stack
//! rotation (`pushd +1`) or a named PowerShell stack leaves a folder no
//! reader resolves. And only a `cd` that certainly ran, and took effect in
//! this shell, moves it: the first command of its and-or list (one after
//! `&&` or `||` may not run), not one element of a pipeline of more than one
//! command and not one sent to the background with `&` (in sh both run in a
//! subshell of their own), and only until the end of the group it sits in -
//! `( ... )` (a subshell), `{ ... }`, a loop, an `if` or a `case` (which may
//! not run at all). Where that is not certain the folder stays where it
//! was: a `cd` that did not take effect must never move a command out of the
//! session's project (see
//! `guard_verdict::segment_projects`). `Segment::any_cd_dir` is the folder as
//! if every `cd` that can take effect did; a file write is judged in both.
//! A wrapper's commands start in the folder of the segment that runs them,
//! never over `ssh` (which starts somewhere else).
//!
//! THE UNIT (`Segment::unit`), where a literal or an inner trigger is looked
//! for: the raw text of the reached segment's pipeline, plus the heredoc bodies
//! that pipeline feeds, plus the text of the last `cd` before it that can take
//! effect there (a place a rule names travels with the command that runs there,
//! into a wrapper too; a `cd` inside `( ... )` ends with the subshell, and a
//! piped or backgrounded one never counts). A compound opener (`while`,
//! `until`, `for`, `select`, `if`, `case`; PowerShell `foreach`, `do`,
//! `switch`) runs to its matching closer (`done`/`fi`/`esac`, nesting counted;
//! PowerShell: the brace that closes its first body), else to the end of its
//! line. A literal is found in the unit's raw text, in its dequoted words
//! joined by single spaces, or in the segment's own head joined the same way -
//! so spacing or quoting a word never hides it, and `git -C x push` still
//! carries `git push`.
//!
//! TOKEN BOUNDARIES (`literal_at_boundary`). A literal matched against a call
//! (a forbidden literal, a requires trigger) matches only on token
//! boundaries: a side of the literal whose edge character is a letter, digit
//! or `_` needs its neighbour to be absent, whitespace, a quote, or one of
//! `; | & ( ) { } < > = , : / \`; on the right a `.` also counts when no
//! letter, digit or `_` follows it, and an escaped whitespace (`\n`, `\t`,
//! `\r`, PowerShell's `` `n ``) counts as the whitespace it writes. So
//! `--force` no longer matches inside `--force-with-lease` (it still matches
//! `--force=true`), and `ssh` no longer matches `sshKey.mjs`, `~/.ssh/` or
//! `id_ssh`, while `/usr/bin/ssh`, `C:\bin\ssh` and `printf
//! 'fix\n\nCo-Authored-By: C'` still match. A requires answer and every
//! file-content check keep plain substring matching: being strict there would
//! refuse more, never less.
//!
//! WHERE READING STOPS, AND WHAT HAPPENS THEN. The hook runs this on every
//! shell call, on text nobody controls, and a hook that crashes or times out
//! lets the call through - so a crafted call must never be a way past a rule.
//! Reading costs time and memory roughly linear in the call's length: a
//! folder, a `pushd` stack and a `cd`'s text are shared, never copied, by
//! every segment and every open group that holds them (copied, a 200 KB `cd`
//! before ten thousand open `(` cost 1.9 GB, and under a memory cap the hook
//! aborted), and where a `$(` closes and which `)` matches an arithmetic `(`
//! are found once per text (`Memo`). It stops at four limits: nested
//! `$(`/`<(`/`>(` and arithmetic are followed `MAX_NEST` deep, wrappers
//! `MAX_DEPTH` deep, all units together may hold sixteen times the call's
//! length plus 64 KiB of text, and a `cd` chain is followed to `MAX_DIR_TEXT`
//! bytes (past that its folder is one no reader resolves, so its segments
//! keep the session's project). When one of the first three stops the reading
//! short, the call is ALSO read flat (`flat_segment`): one more segment
//! holding every word of the whole call, reached by an anchor wherever its
//! words stand, with the whole call as its unit. That can refuse more than a
//! full reading would, never less, and only for calls no one types by hand.
//!
//! KNOWN LIMITS, stated rather than left to be rediscovered: a program held in
//! a variable (`& $exe`, `$D push`), a script carried as data (a here-string
//! into a variable into ssh, `base64 -d`), `docker exec c sh -c '...'` (the
//! inner string is an argument of docker, not of a shell), `xargs`,
//! `find -exec`, `su -c`, `stdbuf`, `wsl` and aliases are not descended, and
//! in `env -C dir cmd` the folder is read as the program; a script piped
//! into a shell (`cat <<EOF | bash`) is data; PowerShell `if { } else { }`
//! ends its unit at the first closing brace. A `cd` inside a brace group, a
//! loop or an `if` keeps the folder where it was after the group, even where
//! it did take effect: the conservative side for the project a command is
//! judged under. So does a `cd` run through `eval`, `command -p` or an
//! `iex`, and a `pushd -n` saves a folder no reader resolves. `cd -@` is
//! read as refused, as Git Bash refuses it (bash elsewhere accepts it).

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

/// The shell a command line is written for. A PowerShell tool call is read as
/// PowerShell; everything else (Bash, and every reader with no tool name) as sh.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Dialect {
    Sh,
    PowerShell,
}

/// The dialect of a tool call's command: PowerShell for the PowerShell tool,
/// sh for every other name.
pub fn dialect_for(tool_name: &str) -> Dialect {
    if tool_name.eq_ignore_ascii_case("powershell") {
        Dialect::PowerShell
    } else {
        Dialect::Sh
    }
}

/// One `>`/`>>` redirection of a segment (5d): the file it writes, whether it
/// appends, and whether a real command in the segment produces what is written
/// (a bare `> f` or `: > f` produces nothing and so empties the file).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Redirect {
    pub target: String,
    pub append: bool,
    pub producer: bool,
}

/// One simple command of a call - see this module's doc comment.
#[derive(Debug, Clone)]
pub struct Segment {
    /// The simple command's words, quotes removed, as typed. Redirect targets
    /// are not words (they are in `redirects`).
    pub words: Vec<String>,
    /// The same words with shell syntax taken off the front (see this module's
    /// doc comment, "THE HEAD").
    pub head: Vec<String>,
    /// The program the head runs: its first word with the directory dropped,
    /// lowercased, `.exe` kept - what an anchor written with `.exe` is compared
    /// with.
    pub program: String,
    /// This simple command's own text exactly as typed (quotes kept).
    pub raw: String,
    /// The text a literal or an inner trigger is looked for in: the command
    /// text first (the pipeline, or the whole loop), then the heredoc bodies it
    /// feeds and the last `cd` before it, one per line.
    pub unit: String,
    /// Where the command text ends in `unit` (a byte offset): what a refusal
    /// quotes when the literal sits there, rather than a heredoc or a `cd`.
    pub command_len: usize,
    /// The unit's words with quotes removed, joined by single spaces.
    pub unit_words: String,
    /// The directory this segment certainly runs in, as the call wrote it,
    /// when an earlier `cd`/`pushd`/`Set-Location` on the line that certainly
    /// took effect says so (see this module's doc comment, "THE FOLDER";
    /// chained: a relative one is joined onto the one before it), or the
    /// repository its own `git -C`, `--git-dir` or `GIT_DIR=` names (never a
    /// `--work-tree`, which names no repository). `None`: the session's own
    /// working directory. The text may still hold a `$`, a `~` or a `-`,
    /// which no reader can resolve. Shared with every segment in the same
    /// folder, never copied.
    pub dir: Option<Arc<str>>,
    /// The directory as if every earlier `cd` on the line that can take
    /// effect here did - the same as `dir` unless one of them is not certain
    /// to have run. A file write is judged in both.
    pub any_cd_dir: Option<Arc<str>>,
    /// How many wrappers deep this segment sits (0 = typed at top level).
    pub depth: usize,
    /// Reached through `ssh`: it runs on another machine, so a file it names
    /// is not a local file and its folder says nothing about a local project.
    pub remote: bool,
    /// The `>`/`>>` redirections of this segment.
    pub redirects: Vec<Redirect>,
    /// The whole call read flat (`flat_segment`), added because its structure
    /// could not be read in full: an anchor reaches it wherever its words
    /// stand. Never read for a file write.
    pub flat: bool,
}

impl Segment {
    /// The head joined by single spaces: the call as an anchor sees it.
    pub fn head_text(&self) -> String {
        self.head.join(" ")
    }
}

/// How many wrappers deep `segments` descends before it stops.
pub const MAX_DEPTH: usize = 8;

/// How many `$(`, `<(` or `>(` deep the scanner follows a nested command to
/// find where it closes, before it stops.
pub const MAX_NEST: usize = 64;

/// The longest folder text a `cd` chain is followed to. A longer one becomes
/// `TOO_LONG_DIR`, which holds a `$` and so resolves to no folder at all.
pub const MAX_DIR_TEXT: usize = 1024;
const TOO_LONG_DIR: &str = "$TOO_LONG_DIR";

/// How many bytes of unit text reading one call may build for all its
/// segments together: sixteen times the call, plus 64 KiB.
fn unit_budget(len: usize) -> usize {
    len.saturating_mul(16).saturating_add(64 * 1024)
}

/// Every simple command `command` runs, in order, nested ones included - and,
/// when the call could not be read in full, the whole call read flat as one
/// more segment (see this module's doc comment, "WHERE READING STOPS").
pub fn segments(command: &str, dialect: Dialect) -> Vec<Segment> {
    let mut reading = Reading { out: Vec::new(), budget: unit_budget(command.len()), overflow: false };
    collect(command, dialect, 0, Place::default(), false, &mut reading);
    if reading.overflow {
        reading.out.push(flat_segment(command));
    }
    reading.out
}

/// The whole call read flat: every word of it, cut at whitespace and at every
/// shell separator, quotes taken out, whatever quoting or nesting it sits in.
/// Its unit is the whole call, and an anchor reaches it wherever its words
/// stand (`flat_rest`). Added only when the structure could not be read in
/// full, so a call too deep or too large to read is refused on what it holds
/// rather than let through on what could not be read.
fn flat_segment(command: &str) -> Segment {
    let is_cut = |c: char| c.is_whitespace() || matches!(c, ';' | '&' | '|' | '(' | ')' | '{' | '}' | '<' | '>' | '`');
    let words: Vec<String> = command
        .split(is_cut)
        .map(|w| w.chars().filter(|c| !matches!(c, '"' | '\'')).collect::<String>())
        .filter(|w| !w.is_empty())
        .collect();
    let head: Vec<String> = words.iter().map(|w| base_lower(w)).collect();
    let unit_words = words.join(" ");
    Segment {
        words,
        head,
        program: String::new(),
        raw: command.to_string(),
        unit: command.to_string(),
        command_len: command.len(),
        unit_words,
        dir: None,
        any_cd_dir: None,
        depth: 0,
        remote: false,
        redirects: Vec::new(),
        flat: true,
    }
}

/// The one pseudo-segment of a tool call that carries no command (Agent,
/// Workflow, WebFetch, an MCP tool): the tool's own name is its head, and its
/// unit is the call's flattened input.
pub fn tool_segment(tool: &str, content: &str) -> Segment {
    Segment {
        words: vec![tool.to_string()],
        head: vec![tool.to_string()],
        program: tool.to_lowercase(),
        raw: tool.to_string(),
        unit: content.to_string(),
        command_len: content.len(),
        unit_words: content.split_whitespace().collect::<Vec<_>>().join(" "),
        dir: None,
        any_cd_dir: None,
        depth: 0,
        remote: false,
        redirects: Vec::new(),
        flat: false,
    }
}

// ------------------------------------------------------------------ segments

/// What a `cd`-like command does to the folder the commands after it run in
/// (see this module's doc comment, "THE FOLDER"). Read the way the shell
/// itself reads the command: a form the shell refuses moves nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
enum CdEffect {
    /// Not the shell's own `cd`, `pushd` or `popd` at all: `sudo cd x`,
    /// `env cd x` or `/usr/bin/cd x` run a program, which moves nothing here,
    /// and bash has no `chdir`, `sl` or `Set-Location`.
    None,
    /// The shell refuses it - an operand too many, an option it does not
    /// know, an empty folder name - and the folder stays where it was.
    Refused,
    /// `cd dir`, `pushd dir`: the folder becomes `target` (`~` for a bare
    /// `cd`, the home folder no reader resolves); a `pushd` saves the folder
    /// it leaves on the stack first.
    To { target: String, save: bool },
    /// PowerShell's bare `pushd`: the folder itself is saved on the stack,
    /// and stays.
    SaveHere,
    /// sh's `pushd -n dir`: the stack grows and the folder stays. What was
    /// saved is typed text bash resolves only when it is popped, so it is a
    /// folder no reader resolves.
    SaveUnknown,
    /// `popd`: back to the folder on top of the stack.
    Back,
    /// sh's `popd -n`: the stack loses its top, and the folder stays.
    Drop,
    /// sh's bare `pushd`: the folder and the top of the stack change places.
    Swap,
    /// A stack rotation (`pushd +1`, `popd -0`) or a named PowerShell stack:
    /// the folder, and the stack, can no longer be told.
    Lost,
}

/// A folder no reader resolves (`guard_verdict::effective_dir` reads `-` as
/// the previous folder): where a popped, rotated or unknown stack leaves the
/// folder.
const UNKNOWN_DIR: &str = "-";

/// What `words` (a simple command as typed; `head` is the same with shell
/// syntax taken off the front) does to the folder in dialect `d`. Only a
/// command the shell runs itself moves it: the words taken off in front of
/// the head may only be reserved words, assignments, `command` and
/// `builtin`, and the name must be typed as the shell's own - never with a
/// folder in front of it.
fn cd_effect(words: &[String], head: &[String], d: Dialect) -> CdEffect {
    let Some(skipped) = words.len().checked_sub(head.len()) else { return CdEffect::None };
    let (prefix, typed) = words.split_at(skipped);
    let Some(name) = typed.first() else { return CdEffect::None };
    let in_this_shell =
        |w: &String| RESERVED.contains(&w.as_str()) || is_assignment(w) || matches!(w.as_str(), "command" | "builtin");
    if !prefix.iter().all(in_this_shell) {
        return CdEffect::None;
    }
    let name = name.to_lowercase();
    let args = &typed[1..];
    match d {
        Dialect::Sh => match name.as_str() {
            "cd" => sh_cd(args),
            "pushd" => sh_pushd(args),
            "popd" => sh_popd(args),
            _ => CdEffect::None,
        },
        Dialect::PowerShell => {
            let name = name.strip_prefix("microsoft.powershell.management\\").unwrap_or(&name);
            match name {
                "cd" | "chdir" | "sl" | "set-location" => ps_location(args, PsLocation::Set),
                "pushd" => ps_location(args, PsLocation::Push),
                "popd" | "pop-location" => ps_location(args, PsLocation::Pop),
                _ => CdEffect::None,
            }
        }
    }
}

/// bash's `cd [-L|[-P [-e]]] [dir]` (measured against Git Bash 5.3, where
/// `-@` is refused): options only from `L`, `P` and `e`, then `--` or one
/// folder at most.
fn sh_cd(args: &[String]) -> CdEffect {
    let mut i = 0;
    while let Some(w) = args.get(i) {
        if w == "--" {
            i += 1;
            break;
        }
        if w.len() < 2 || !w.starts_with('-') {
            break;
        }
        if !w[1..].chars().all(|c| matches!(c, 'L' | 'P' | 'e')) {
            return CdEffect::Refused;
        }
        i += 1;
    }
    match &args[i..] {
        [] => CdEffect::To { target: "~".to_string(), save: false },
        [dir] if !dir.is_empty() => CdEffect::To { target: dir.clone(), save: false },
        _ => CdEffect::Refused,
    }
}

/// `+N` or `-N`: a place on bash's directory stack.
fn is_stack_offset(w: &str) -> bool {
    let Some(digits) = w.strip_prefix(['+', '-']) else { return false };
    !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())
}

/// bash's `pushd [-n] [+N | -N | dir]`, as its own option loop reads it.
fn sh_pushd(args: &[String]) -> CdEffect {
    let (mut no_cd, mut rotate) = (false, false);
    let mut i = 0;
    while let Some(w) = args.get(i) {
        if w == "-n" {
            no_cd = true;
        } else if w == "--" {
            i += 1;
            break;
        } else if w == "-" || !w.starts_with(['+', '-']) {
            break;
        } else if is_stack_offset(w) {
            rotate = true;
        } else {
            return CdEffect::Refused;
        }
        i += 1;
    }
    if rotate {
        return CdEffect::Lost;
    }
    match (no_cd, &args[i..]) {
        (true, []) => CdEffect::Refused,
        (true, _) => CdEffect::SaveUnknown,
        (false, []) => CdEffect::Swap,
        (false, [dir]) if !dir.is_empty() => CdEffect::To { target: dir.clone(), save: true },
        _ => CdEffect::Refused,
    }
}

/// bash's `popd [-n] [+N | -N]`: any other word is refused; after `--` the
/// rest is not read.
fn sh_popd(args: &[String]) -> CdEffect {
    let (mut no_cd, mut offset) = (false, false);
    for w in args {
        if w == "-n" {
            no_cd = true;
        } else if w == "--" || w.is_empty() {
            break;
        } else if is_stack_offset(w) {
            offset = true;
        } else {
            return CdEffect::Refused;
        }
    }
    if offset {
        CdEffect::Lost
    } else if no_cd {
        CdEffect::Drop
    } else {
        CdEffect::Back
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PsLocation {
    Set,
    Push,
    Pop,
}

/// The parameters of `Set-Location`, `Push-Location` and `Pop-Location` in
/// Windows PowerShell 5.1, as `Get-Command` lists them: name, aliases, and
/// whether it takes a value. `Pop-Location` has no `path` or `literalpath`.
const PS_LOCATION_PARAMS: &[(&str, &[&str], bool)] = &[
    ("path", &[], true),
    ("literalpath", &["pspath"], true),
    ("passthru", &[], false),
    ("stackname", &[], true),
    ("verbose", &["vb"], false),
    ("debug", &["db"], false),
    ("erroraction", &["ea"], true),
    ("warningaction", &["wa"], true),
    ("informationaction", &["infa"], true),
    ("errorvariable", &["ev"], true),
    ("warningvariable", &["wv"], true),
    ("informationvariable", &["iv"], true),
    ("outvariable", &["ov"], true),
    ("outbuffer", &["ob"], true),
    ("pipelinevariable", &["pv"], true),
    ("usetransaction", &["usetx"], false),
];

/// The parameter `-name` binds to, as PowerShell binds it: a name or alias
/// typed in full, else the ONE parameter whose name or an alias starts with
/// it. `None` when there is none, or more than one (PowerShell refuses the
/// call as ambiguous).
fn ps_param(name: &str, cmd: PsLocation) -> Option<(&'static str, bool)> {
    let name = name.to_lowercase();
    let known = || {
        PS_LOCATION_PARAMS.iter().filter(move |(p, _, _)| cmd != PsLocation::Pop || !matches!(*p, "path" | "literalpath"))
    };
    if let Some((p, _, value)) = known().find(|(p, aliases, _)| *p == name || aliases.contains(&name.as_str())) {
        return Some((p, *value));
    }
    let mut hits = known().filter(|(p, aliases, _)| p.starts_with(&name) || aliases.iter().any(|a| a.starts_with(&name)));
    let (p, _, value) = hits.next()?;
    hits.next().is_none().then_some((p, *value))
}

/// PowerShell's `Set-Location` (`cd`, `chdir`, `sl`), `Push-Location`
/// (`pushd`) and `Pop-Location` (`popd`), bound the way PowerShell binds
/// them (measured on 5.1): at most one folder, named or positional; a
/// parameter it does not know, an ambiguous one or `-UseTransaction` is
/// refused; `-StackName` names a stack this reader does not keep.
fn ps_location(args: &[String], cmd: PsLocation) -> CdEffect {
    let mut folders: Vec<&str> = Vec::new();
    let mut named_stack = false;
    let mut params_ended = false;
    let mut i = 0;
    while let Some(w) = args.get(i) {
        i += 1;
        if !params_ended && w == "--" {
            params_ended = true;
            continue;
        }
        let is_param = !params_ended && w.starts_with('-') && w[1..].starts_with(|c: char| c.is_ascii_alphabetic());
        if !is_param {
            folders.push(w);
            continue;
        }
        let (name, attached) = match w[1..].split_once(':') {
            Some((name, value)) => (name, Some(value)),
            None => (&w[1..], None),
        };
        let Some((param, takes_value)) = ps_param(name, cmd) else { return CdEffect::Refused };
        let value = match (takes_value, attached) {
            (true, None) => {
                let Some(next) = args.get(i) else { return CdEffect::Refused };
                i += 1;
                Some(next.as_str())
            }
            _ => attached,
        };
        match param {
            "path" | "literalpath" => folders.extend(value),
            "stackname" => named_stack = true,
            "usetransaction" => return CdEffect::Refused,
            _ => {}
        }
    }
    match (cmd, folders.as_slice(), named_stack) {
        (_, [_, _, ..], _) | (_, [""], _) => CdEffect::Refused,
        (PsLocation::Set, _, true) => CdEffect::Refused,
        (PsLocation::Set, [], false) => CdEffect::To { target: "~".to_string(), save: false },
        (PsLocation::Set, [dir], false) => CdEffect::To { target: dir.to_string(), save: false },
        (PsLocation::Push, [], true) => CdEffect::Refused,
        (PsLocation::Push, [], false) => CdEffect::SaveHere,
        (PsLocation::Push, [_], true) => CdEffect::Lost,
        (PsLocation::Push, [dir], false) => CdEffect::To { target: dir.to_string(), save: true },
        (PsLocation::Pop, [], true) => CdEffect::Lost,
        (PsLocation::Pop, [], false) => CdEffect::Back,
        (PsLocation::Pop, [_], _) => CdEffect::Refused,
    }
}

/// A directory text that starts from nowhere of its own: a Unix or Windows
/// absolute path, a network path, the home folder or a variable.
fn is_absolute_text(p: &str) -> bool {
    let b = p.as_bytes();
    p.starts_with('/')
        || p.starts_with('\\')
        || p.starts_with('~')
        || p.starts_with('$')
        || (b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':')
}

/// `target` read from `base`: joined onto it by text when relative, as it
/// stands otherwise - and `TOO_LONG_DIR` when that text runs past
/// `MAX_DIR_TEXT` (a chain of a thousand `cd a` must not cost the hook a
/// thousand ever longer folder names).
fn join_dir(base: Option<&str>, target: &str) -> Arc<str> {
    let joined = match base {
        Some(TOO_LONG_DIR) if !is_absolute_text(target) => return Arc::from(TOO_LONG_DIR),
        Some(base) if !is_absolute_text(target) => format!("{}/{target}", base.trim_end_matches(['/', '\\'])),
        _ => target.to_string(),
    };
    if joined.len() > MAX_DIR_TEXT {
        Arc::from(TOO_LONG_DIR)
    } else {
        Arc::from(joined)
    }
}

/// The repository a git call's own options or environment point it at: `-C`
/// (chained when repeated), then `--git-dir`, else a `GIT_DIR=` written in
/// front of it, read from where `-C` put it. Read only when the head's
/// program is git; `None` when nothing names one, and git finds the
/// repository from the folder it runs in. A `--work-tree` or `GIT_WORK_TREE=`
/// chooses nothing: it names where the files are, never which repository git
/// acts on (`git --work-tree=../other push` pushes the repository of the
/// folder it runs in).
fn git_dir_override(words: &[String]) -> Option<Arc<str>> {
    let git_at = words.iter().position(|w| base_lower(w) == "git")?;
    let env_dir = words[..git_at].iter().rev().find_map(|w| w.strip_prefix("GIT_DIR="));
    let mut c_dir: Option<Arc<str>> = None;
    let mut git_dir = None;
    let mut j = git_at + 1;
    while j < words.len() {
        let w = words[j].as_str();
        if w == "-C" {
            if let Some(v) = words.get(j + 1) {
                c_dir = Some(join_dir(c_dir.as_deref(), v));
            }
            j += 2;
        } else if w == "--git-dir" {
            git_dir = words.get(j + 1).map(String::as_str);
            j += 2;
        } else if matches!(w, "-c" | "--work-tree" | "--namespace" | "--exec-path") {
            j += 2;
        } else if let Some(v) = w.strip_prefix("--git-dir=") {
            git_dir = Some(v);
            j += 1;
        } else if w.starts_with("--work-tree=")
            || w.starts_with("--namespace=")
            || w.starts_with("--exec-path=")
            || matches!(
                w,
                "--no-pager" | "-P" | "--paginate" | "-p" | "--bare" | "--no-replace-objects" | "--literal-pathspecs" | "--no-optional-locks"
            )
        {
            j += 1;
        } else {
            break;
        }
    }
    match (c_dir, git_dir.or(env_dir)) {
        (c, Some(named)) => Some(join_dir(c.as_deref(), named)),
        (c, None) => c,
    }
}

fn opens_compound(words: &[String], d: Dialect) -> bool {
    words.iter().find(|w| w.as_str() != "!").is_some_and(|w| {
        let w = w.to_lowercase();
        COMPOUND_OPENERS.contains(&w.as_str()) || (d == Dialect::PowerShell && PS_OPENERS.contains(&w.as_str()))
    })
}

/// The separator a simple command's text ended with - where the scanner cut
/// it. It decides whether a `cd` in it certainly took effect for the commands
/// after it (see this module's doc comment, "THE FOLDER").
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Sep {
    Semi,
    And,
    Or,
    /// `|` or `|&`.
    Pipe,
    /// A single `&`: the command runs in the background, in a subshell.
    Background,
    Newline,
    /// `(`.
    Open,
    /// `)`.
    Close,
    OpenBrace,
    CloseBrace,
    /// The end of the text.
    End,
}

/// One call being read: the segments so far, the unit text still allowed
/// (`unit_budget`), and whether any part could not be read in full.
struct Reading {
    out: Vec<Segment>,
    budget: usize,
    overflow: bool,
}

/// A folder as the call wrote it (see `Segment::dir`), shared: every segment,
/// stack and saved group that holds the same folder holds one copy of it.
type Folder = Option<Arc<str>>;

/// A `pushd` stack, shared the same way: a push or a pop makes a new top
/// over the frames below it, never a copy of them, so saving the stack at
/// every group that opens costs nothing however deep it is.
#[derive(Clone, Default)]
struct Stack(Option<Rc<Frame>>);

struct Frame {
    folder: Folder,
    below: Stack,
}

impl Stack {
    fn push(&mut self, folder: Folder) {
        let below = std::mem::take(self);
        self.0 = Some(Rc::new(Frame { folder, below }));
    }

    /// The folder on top, taken off: `None` when the stack is empty.
    fn pop(&mut self) -> Option<Folder> {
        let top = self.0.take()?;
        *self = top.below.clone();
        Some(top.folder.clone())
    }
}

impl Drop for Stack {
    /// Frame by frame, never by recursion: a stack of a hundred thousand
    /// `pushd`s would otherwise overflow the hook's own stack when dropped,
    /// and a crashed hook lets the call through.
    fn drop(&mut self) {
        let mut next = self.0.take();
        while let Some(frame) = next {
            next = match Rc::try_unwrap(frame) {
                Ok(mut frame) => frame.below.0.take(),
                Err(_) => None,
            };
        }
    }
}

/// The folder and its `pushd` stack before a `cd` that may not have run.
type Undo = (Folder, Stack);

/// Where the commands of one text run, as its `cd`s have moved it (see this
/// module's doc comment, "THE FOLDER"). Cloning it copies five pointers.
#[derive(Clone, Default)]
struct Place {
    /// The folder the next command certainly runs in (`Segment::dir`).
    dir: Folder,
    stack: Stack,
    /// The folder as if every `cd` that can take effect did
    /// (`Segment::any_cd_dir`).
    any_dir: Folder,
    any_stack: Stack,
    /// The text of the last `cd` that can take effect here: part of every
    /// later unit.
    cd_text: Rc<str>,
}

impl Place {
    /// Move as `effect` says; `typed` is the text of the `cd` itself.
    fn apply(&mut self, effect: CdEffect, typed: &str) {
        let unknown = || -> Folder { Some(Arc::from(UNKNOWN_DIR)) };
        match effect {
            CdEffect::None | CdEffect::Refused => {}
            CdEffect::To { target, save } => {
                if save {
                    self.stack.push(self.dir.clone());
                    self.any_stack.push(self.any_dir.clone());
                }
                let dir = Some(join_dir(self.dir.as_deref(), &target));
                self.any_dir = if self.any_dir == self.dir { dir.clone() } else { Some(join_dir(self.any_dir.as_deref(), &target)) };
                self.dir = dir;
                self.cd_text = Rc::from(typed);
            }
            CdEffect::SaveHere => {
                self.stack.push(self.dir.clone());
                self.any_stack.push(self.any_dir.clone());
            }
            CdEffect::SaveUnknown => {
                self.stack.push(unknown());
                self.any_stack.push(unknown());
            }
            // An empty stack is one whose bottom this reader never saw: the
            // folder is then one no reader resolves.
            CdEffect::Back => {
                self.dir = self.stack.pop().unwrap_or_else(unknown);
                self.any_dir = self.any_stack.pop().unwrap_or_else(unknown);
            }
            CdEffect::Drop => {
                self.stack.pop();
                self.any_stack.pop();
            }
            CdEffect::Swap => {
                let top = self.stack.pop().unwrap_or_else(unknown);
                self.stack.push(std::mem::replace(&mut self.dir, top));
                let any_top = self.any_stack.pop().unwrap_or_else(unknown);
                self.any_stack.push(std::mem::replace(&mut self.any_dir, any_top));
            }
            CdEffect::Lost => {
                self.dir = unknown();
                self.any_dir = unknown();
                self.stack = Stack::default();
                self.any_stack = Stack::default();
            }
        }
    }
}

/// A group a `cd` never moves the folder out of: a subshell `( ... )`, a
/// brace group, or a loop/if/case that runs up to the raw it names.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum GroupKind {
    Subshell,
    Brace,
    Compound(usize),
}

struct Group {
    kind: GroupKind,
    /// The place when the group opened.
    saved: Place,
    /// The and-or list around the group, which picks up again after it.
    outer_undo: Option<Undo>,
}

/// The groups open at a point of the text, innermost last, with how many of
/// each kind are open - so a closer only ever searches for a group that is
/// there, and every group is searched past at most once (a line of a
/// hundred thousand unclosed `if`s must not search the whole stack at every
/// command).
#[derive(Default)]
struct Groups {
    open: Vec<Group>,
    count: HashMap<GroupKind, usize>,
}

impl Groups {
    fn push(&mut self, kind: GroupKind, place: &Place, undo: &mut Option<Undo>) {
        *self.count.entry(kind).or_default() += 1;
        self.open.push(Group { kind, saved: place.clone(), outer_undo: undo.take() });
    }

    /// Close the innermost open group of `kind` (and every group opened
    /// inside it): the folder goes back to what it was when the group opened
    /// - for a subshell the any-cd folder and the `cd` text too, since
    /// nothing done in one outlives it - and the and-or list around the group
    /// picks up where it was. `false` when no such group is open.
    fn close(&mut self, kind: GroupKind, place: &mut Place, undo: &mut Option<Undo>) -> bool {
        if self.count.get(&kind).copied().unwrap_or(0) == 0 {
            return false;
        }
        let Some(at) = self.open.iter().rposition(|g| g.kind == kind) else { return false };
        for inner in &self.open[at..] {
            if let Some(n) = self.count.get_mut(&inner.kind) {
                *n -= 1;
            }
        }
        let group = self.open.split_off(at).swap_remove(0);
        let saved = group.saved;
        place.dir = saved.dir;
        place.stack = saved.stack;
        if kind == GroupKind::Subshell {
            place.any_dir = saved.any_dir;
            place.any_stack = saved.any_stack;
            place.cd_text = saved.cd_text;
        }
        *undo = group.outer_undo;
        true
    }
}

/// For every raw, the first and the last raw of its pipeline (the raws of one
/// pipeline always sit next to each other).
fn pipeline_bounds(raws: &[Raw]) -> (Vec<usize>, Vec<usize>) {
    let n = raws.len();
    let (mut first, mut last) = (vec![0; n], vec![0; n]);
    let mut k = 0;
    while k < n {
        let mut j = k;
        while j + 1 < n && raws[j + 1].pipeline == raws[k].pipeline {
            j += 1;
        }
        for i in k..=j {
            first[i] = k;
            last[i] = j;
        }
        k = j + 1;
    }
    (first, last)
}

/// For every raw, the byte offset where its line ends and the last raw that
/// starts before that (the raw itself at the least). Raws start in order, so
/// both are a binary search.
fn line_bounds(command: &str, raws: &[Raw]) -> Vec<(usize, usize)> {
    let newlines: Vec<usize> = command.match_indices('\n').map(|(at, _)| at).collect();
    raws.iter()
        .enumerate()
        .map(|(k, r)| {
            let line_end = newlines.get(newlines.partition_point(|&p| p < r.start)).copied().unwrap_or(command.len());
            let last = raws.partition_point(|x| x.start < line_end).saturating_sub(1).max(k);
            (line_end, last)
        })
        .collect()
}

/// `sums[k]` is the total `size` of raws `0..k`, so the size of any run of
/// raws is one subtraction.
fn prefix_sums(raws: &[Raw], size: impl Fn(&Raw) -> usize) -> Vec<usize> {
    let mut sums = Vec::with_capacity(raws.len() + 1);
    let mut total = 0;
    sums.push(0);
    for r in raws {
        total += size(r);
        sums.push(total);
    }
    sums
}

fn collect(command: &str, d: Dialect, depth: usize, start: Place, remote: bool, reading: &mut Reading) {
    let sh = d == Dialect::Sh;
    let (raws, too_deep) = Scanner::new(command, d).run();
    reading.overflow |= too_deep;
    let n = raws.len();
    let (first_of, last_of) = pipeline_bounds(&raws);
    let ends = compound_ends(&raws, d);
    let lines = line_bounds(command, &raws);
    let heredoc_bytes = prefix_sums(&raws, |r| if r.heredoc.is_empty() { 0 } else { r.heredoc.len() + 1 });
    let word_bytes = prefix_sums(&raws, |r| r.words.iter().map(|w| w.len() + 1).sum());
    let mut place = start;
    let mut groups = Groups::default();
    // Set by the first `cd` of the current and-or list that runs only on a
    // condition (after `&&` or `||`): where the folder was before it. When
    // the list ends the folder goes back there.
    let mut undo: Option<Undo> = None;
    for k in 0..n {
        let raw = &raws[k];
        let prev = k.checked_sub(1).map(|p| raws[p].ended_by);
        match prev {
            // In PowerShell `( ... )` groups an expression in this very
            // scope: a `cd` in it holds after it.
            Some(Sep::Open) if sh => groups.push(GroupKind::Subshell, &place, &mut undo),
            Some(Sep::OpenBrace) => groups.push(GroupKind::Brace, &place, &mut undo),
            _ => {}
        }
        // After `||` a command runs only when the one before it failed, so a
        // conditional `cd` earlier in the list may not have run.
        if prev == Some(Sep::Or) {
            if let Some((dir, stack)) = &undo {
                place.dir = dir.clone();
                place.stack = stack.clone();
            }
        }
        let first_in_list =
            matches!(prev, None | Some(Sep::Semi | Sep::Newline | Sep::Background | Sep::Open | Sep::OpenBrace));
        let compound = (!raw.words.is_empty() && opens_compound(&raw.words, d)).then(|| ends[k].unwrap_or(lines[k].1));
        if let Some(j) = compound {
            groups.push(GroupKind::Compound(j), &place, &mut undo);
        }
        if !raw.words.is_empty() || !raw.redirects.is_empty() {
            let (first, last) = (first_of[k], last_of[k]);
            let (start, end, wf, wt) = match compound {
                Some(j) if ends[k].is_some() => (raw.start, raws[j].end, k, j),
                Some(j) => (raw.start, lines[k].0, k, j),
                None => (raws[first].start, raws[last].end, first, last),
            };
            let text = command.get(start..end.max(start)).unwrap_or("").trim();
            let cd_part = place.cd_text.trim();
            let size = text.len()
                + (heredoc_bytes[wt + 1] - heredoc_bytes[first])
                + if cd_part.is_empty() { 0 } else { cd_part.len() + 1 }
                + (word_bytes[wt + 1] - word_bytes[wf]);
            let own = command.get(raw.start..raw.end.max(raw.start)).unwrap_or("").trim().to_string();
            let (unit, command_len, unit_words) = if size <= reading.budget {
                reading.budget -= size;
                let mut unit = text.to_string();
                let command_len = unit.len();
                for r in &raws[first..=wt] {
                    if !r.heredoc.is_empty() {
                        unit.push('\n');
                        unit.push_str(&r.heredoc);
                    }
                }
                if !cd_part.is_empty() {
                    unit.push('\n');
                    unit.push_str(cd_part);
                }
                let words = raws[wf..=wt].iter().flat_map(|r| r.words.iter().map(String::as_str)).collect::<Vec<_>>();
                (unit, command_len, words.join(" "))
            } else {
                // Over the budget: the segment's own text and heredoc only.
                // The flat reading added at the end holds the rest.
                reading.overflow = true;
                let mut unit = own.clone();
                let command_len = unit.len();
                if !raw.heredoc.is_empty() {
                    unit.push('\n');
                    unit.push_str(&raw.heredoc);
                }
                (unit, command_len, raw.words.join(" "))
            };
            let (head, program) = head_and_program(&raw.words);
            let git_dir = if program == "git" || program == "git.exe" { git_dir_override(&raw.words) } else { None };
            let own_dir = match &git_dir {
                Some(g) => Some(join_dir(place.dir.as_deref(), g)),
                None => place.dir.clone(),
            };
            let own_any = match &git_dir {
                Some(_) if place.any_dir == place.dir => own_dir.clone(),
                Some(g) => Some(join_dir(place.any_dir.as_deref(), g)),
                None => place.any_dir.clone(),
            };
            let cd_before = place.cd_text.clone();
            let producer = raw.words.iter().any(|w| w != ":");
            let redirects = raw
                .redirects
                .iter()
                .map(|(target, append)| Redirect { target: target.clone(), append: *append, producer })
                .collect();
            reading.out.push(Segment {
                words: raw.words.clone(),
                head: head.clone(),
                program,
                raw: own,
                unit,
                command_len,
                unit_words,
                dir: own_dir.clone(),
                any_cd_dir: own_any.clone(),
                depth,
                remote,
                redirects,
                flat: false,
            });
            // A `cd` moves the folder of what follows - when the shell does
            // not refuse it, and when it can take effect here at all: in sh a
            // pipeline of more than one command, and a command sent to the
            // background, each run in a subshell.
            let effect = cd_effect(&raw.words, &head, d);
            let own_subshell = raw.ended_by == Sep::Background || (sh && first != last);
            if !matches!(effect, CdEffect::None | CdEffect::Refused) && !own_subshell {
                if !first_in_list && undo.is_none() {
                    undo = Some((place.dir.clone(), place.stack.clone()));
                }
                place.apply(effect, command.get(raw.start..raw.end.max(raw.start)).unwrap_or(""));
            }
            let mut inner: Vec<(String, Dialect, bool)> = raw.subs.iter().map(|sub| (sub.clone(), d, false)).collect();
            inner.extend(wrapped_commands(&head, &raw.heredoc, d));
            if !inner.is_empty() && depth + 1 >= MAX_DEPTH {
                reading.overflow = true;
            } else {
                for (text, inner_d, to_remote) in inner {
                    // A wrapper's commands start where this one runs, the
                    // place its `cd` named included - never over ssh.
                    let start = if to_remote {
                        Place::default()
                    } else {
                        Place { dir: own_dir.clone(), any_dir: own_any.clone(), cd_text: cd_before.clone(), ..Place::default() }
                    };
                    collect(&text, inner_d, depth + 1, start, remote || to_remote, reading);
                }
            }
        }
        // What ends with this raw: a subshell or brace group, every loop or
        // if/case whose closer it is, and the and-or list.
        let closed = match raw.ended_by {
            Sep::Close if sh => groups.close(GroupKind::Subshell, &mut place, &mut undo),
            Sep::CloseBrace => groups.close(GroupKind::Brace, &mut place, &mut undo),
            _ => false,
        };
        while groups.close(GroupKind::Compound(k), &mut place, &mut undo) {}
        let list_ends = matches!(raw.ended_by, Sep::Semi | Sep::Newline | Sep::Background | Sep::End)
            || (matches!(raw.ended_by, Sep::Close | Sep::CloseBrace) && !closed);
        if list_ends {
            if let Some((dir, stack)) = undo.take() {
                place.dir = dir;
                place.stack = stack;
            }
        }
    }
}

const COMPOUND_OPENERS: &[&str] = &["while", "until", "for", "select", "if", "case"];
const PS_OPENERS: &[&str] = &["foreach", "do", "switch"];

fn closer_for(opener: &str) -> Option<&'static str> {
    match opener {
        "while" | "until" | "for" | "select" => Some("done"),
        "if" => Some("fi"),
        "case" => Some("esac"),
        _ => None,
    }
}

/// For every raw that opens a loop or if/case, the raw that closes it: the
/// matching `done`/`fi`/`esac` (nesting counted), or in PowerShell the `}`
/// that closes the first body brace. `None` when no closer is found, and the
/// caller then reads to the end of the line. One pass over the raws for all
/// of them, never one pass per opener: a line of ten thousand unclosed `if`s
/// must not cost a hundred million steps.
fn compound_ends(raws: &[Raw], d: Dialect) -> Vec<Option<usize>> {
    let n = raws.len();
    let mut ends = vec![None; n];
    if d == Dialect::PowerShell {
        // For a raw ended by `{`, the raw ended by the `}` that matches it.
        let mut matched = vec![None; n];
        let mut open = Vec::new();
        for (j, r) in raws.iter().enumerate() {
            match r.closed_by {
                Some('{') => open.push(j),
                Some('}') => {
                    if let Some(o) = open.pop() {
                        matched[o] = Some(j);
                    }
                }
                _ => {}
            }
        }
        // Read from raw k on: the first `{` opens the body and its match
        // closes it - unless a `}` comes first, which ends the unit at that
        // very `{`.
        let (mut next_open, mut next_close) = (None, None);
        for k in (0..n).rev() {
            match raws[k].closed_by {
                Some('{') => next_open = Some(k),
                Some('}') => next_close = Some(k),
                _ => {}
            }
            ends[k] = match next_open {
                Some(o) if next_close.is_some_and(|c| c < o) => Some(o),
                Some(o) => matched[o],
                None => None,
            };
        }
        return ends;
    }
    let mut open: Vec<(usize, &'static str)> = Vec::new();
    for (j, r) in raws.iter().enumerate() {
        let mut words = r.words.iter().map(String::as_str).peekable();
        while words.next_if(|w| matches!(*w, "do" | "then" | "else" | "elif" | "!" | "{")).is_some() {}
        let Some(w) = words.next() else { continue };
        if let Some(closer) = closer_for(w) {
            open.push((j, closer));
        } else if open.last().is_some_and(|&(_, closer)| closer == w) {
            if let Some((o, _)) = open.pop() {
                ends[o] = Some(j);
            }
        }
    }
    ends
}

const RESERVED: &[&str] = &["!", "{", "}", "then", "else", "elif", "do", "if", "while", "until", "time"];
const SHELLS: &[&str] = &["sh", "bash", "zsh", "dash", "ksh"];

fn is_assignment(w: &str) -> bool {
    let Some(eq) = w.find('=') else { return false };
    let name = &w[..eq];
    let mut cs = name.chars();
    matches!(cs.next(), Some(c) if c.is_ascii_alphabetic() || c == '_') && cs.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// A word's last path part, lowercased, without a `.exe` suffix.
fn base_lower(w: &str) -> String {
    let base = w.rsplit(['/', '\\']).next().unwrap_or(w).to_lowercase();
    base.strip_suffix(".exe").map(str::to_string).unwrap_or(base)
}

/// The words with shell syntax taken off the front - see `Segment::head` -
/// and the program they run (see `Segment::program`).
fn head_and_program(words: &[String]) -> (Vec<String>, String) {
    let mut i = 0;
    while i < words.len() {
        let w = words[i].as_str();
        if RESERVED.contains(&w) || is_assignment(w) {
            i += 1;
            continue;
        }
        match base_lower(w).as_str() {
            "sudo" => {
                i += 1;
                while i < words.len() && words[i].starts_with('-') {
                    let takes_arg = matches!(words[i].as_str(), "-u" | "-g" | "-h" | "-p" | "-C" | "-D" | "-r" | "-t" | "-U");
                    i += if takes_arg { 2 } else { 1 };
                }
            }
            "env" => {
                i += 1;
                while i < words.len() && (words[i].starts_with('-') || is_assignment(&words[i])) {
                    i += if words[i] == "-u" { 2 } else { 1 };
                }
            }
            "nohup" | "exec" | "command" | "builtin" => i += 1,
            "nice" => {
                i += 1;
                if i < words.len() && words[i] == "-n" {
                    i += 2;
                } else if i < words.len() && words[i].starts_with('-') {
                    i += 1;
                }
            }
            "timeout" => {
                i += 1;
                while i < words.len() && words[i].starts_with('-') {
                    i += if matches!(words[i].as_str(), "-s" | "-k") { 2 } else { 1 };
                }
                i += 1; // the duration
            }
            _ => break,
        }
    }
    let mut out: Vec<String> = words[i.min(words.len())..].to_vec();
    let mut program = String::new();
    if let Some(first) = out.first_mut() {
        let base = first.rsplit(['/', '\\']).next().unwrap_or(first).to_string();
        program = base.to_lowercase();
        let lower = base.to_lowercase();
        *first = if lower.ends_with(".exe") { base[..base.len() - 4].to_string() } else { base };
    }
    // git's own global options sit between `git` and its subcommand
    // (`git -C repo push`, `git -c k=v commit`, `git --no-pager log`): an
    // anchor names the subcommand, so they come out of the head.
    if out.first().is_some_and(|w| w.eq_ignore_ascii_case("git")) {
        let mut j = 1;
        while j < out.len() {
            let w = out[j].as_str();
            if matches!(w, "-C" | "-c" | "--git-dir" | "--work-tree" | "--namespace" | "--exec-path") {
                j += 2;
            } else if w.starts_with("--git-dir=")
                || w.starts_with("--work-tree=")
                || w.starts_with("--namespace=")
                || w.starts_with("--exec-path=")
                || matches!(w, "--no-pager" | "-P" | "--paginate" | "-p" | "--bare" | "--no-replace-objects" | "--literal-pathspecs" | "--no-optional-locks")
            {
                j += 1;
            } else {
                break;
            }
        }
        if j > 1 {
            out.drain(1..j.min(out.len()));
        }
    }
    (out, program)
}

/// Command strings a segment runs by way of a wrapper, with their dialect and
/// whether they run on another machine: `sh|bash|zsh|dash|ksh -c STR` (any
/// single-dash flag cluster carrying c), a shell reading a heredoc script,
/// `powershell|pwsh -Command ...`, `cmd /c|/k ...`, `eval ...`,
/// `Invoke-Expression|iex ...`, and `ssh [opts] dest cmd...` (argv joined with
/// spaces, as OpenSSH does), plus a heredoc fed to a bare ssh or to
/// `ssh dest bash -s`.
fn wrapped_commands(head: &[String], heredoc: &str, d: Dialect) -> Vec<(String, Dialect, bool)> {
    let Some(first) = head.first() else { return Vec::new() };
    let name = base_lower(first);
    let rest = &head[1..];
    let mut out = Vec::new();
    if SHELLS.contains(&name.as_str()) {
        let is_c = |w: &String| {
            w.starts_with('-') && !w.starts_with("--") && w.len() > 1 && w[1..].chars().all(|c| c.is_ascii_alphabetic()) && w.contains('c')
        };
        if let Some(pos) = rest.iter().position(is_c) {
            if let Some(inner) = rest.get(pos + 1) {
                out.push((inner.clone(), Dialect::Sh, false));
            }
        } else if !heredoc.is_empty() && rest.iter().all(|w| w.starts_with('-')) {
            out.push((heredoc.to_string(), Dialect::Sh, false));
        }
    } else if name == "powershell" || name == "pwsh" {
        if let Some(pos) = rest.iter().position(|w| w.eq_ignore_ascii_case("-command") || w.eq_ignore_ascii_case("-c")) {
            let inner = rest[pos + 1..].join(" ");
            if !inner.is_empty() {
                out.push((inner, Dialect::PowerShell, false));
            }
        }
    } else if name == "cmd" {
        if let Some(pos) = rest.iter().position(|w| w.eq_ignore_ascii_case("/c") || w.eq_ignore_ascii_case("/k")) {
            let inner = rest[pos + 1..].join(" ");
            if !inner.is_empty() {
                // cmd has no backslash escapes: read as the dialect that
                // keeps a backslash literal.
                out.push((inner, Dialect::PowerShell, false));
            }
        }
    } else if name == "eval" && d == Dialect::Sh {
        let inner = rest.join(" ");
        if !inner.is_empty() {
            out.push((inner, Dialect::Sh, false));
        }
    } else if name == "invoke-expression" || name == "iex" {
        let words: Vec<&String> =
            rest.iter().filter(|w| !w.eq_ignore_ascii_case("-command") && !w.eq_ignore_ascii_case("-c")).collect();
        let inner = words.iter().map(|w| w.as_str()).collect::<Vec<_>>().join(" ");
        if !inner.is_empty() {
            out.push((inner, Dialect::PowerShell, false));
        }
    } else if name == "ssh" {
        let remote = ssh_remote_words(rest);
        let remote_shell = remote.first().is_some_and(|w| SHELLS.contains(&base_lower(w).as_str()));
        if !remote.is_empty() {
            out.push((remote.join(" "), Dialect::Sh, true));
        }
        if !heredoc.is_empty() && (remote.is_empty() || remote_shell) {
            out.push((heredoc.to_string(), Dialect::Sh, true));
        }
    }
    out
}

/// The words after ssh's destination: options (with the ones that take an
/// argument skipping it, glued or not), `--`, the destination, then the rest.
fn ssh_remote_words(rest: &[String]) -> Vec<String> {
    const WITH_ARG: &str = "BbcDEeFIiJLlmOoPpQRSWw";
    let mut i = 0;
    while i < rest.len() {
        let w = &rest[i];
        if w == "--" {
            i += 1;
            break;
        }
        if !w.starts_with('-') || w.len() < 2 {
            break;
        }
        let flags: Vec<char> = w[1..].chars().collect();
        let mut consumed_next = false;
        for (k, c) in flags.iter().enumerate() {
            if WITH_ARG.contains(*c) {
                if k + 1 == flags.len() {
                    consumed_next = true;
                }
                break;
            }
        }
        i += if consumed_next { 2 } else { 1 };
    }
    if i >= rest.len() {
        return Vec::new();
    }
    rest[i + 1..].to_vec()
}

// ------------------------------------------------------------------ scanner

/// One simple command as the scanner cut it, before it becomes a `Segment`.
struct Raw {
    start: usize,
    end: usize,
    words: Vec<String>,
    pipeline: usize,
    heredoc: String,
    subs: Vec<String>,
    /// `(target, append)` of every `>`/`>>` redirection.
    redirects: Vec<(String, bool)>,
    /// PowerShell only: the brace that ended this segment.
    closed_by: Option<char>,
    /// The separator this segment's text ended with.
    ended_by: Sep,
}

impl Raw {
    fn new(start: usize, pipeline: usize) -> Self {
        Raw {
            start,
            end: start,
            words: Vec::new(),
            pipeline,
            heredoc: String::new(),
            subs: Vec::new(),
            redirects: Vec::new(),
            closed_by: None,
            ended_by: Sep::End,
        }
    }
}

/// What the next word is, after a redirection operator.
#[derive(Clone, Copy)]
enum Pending {
    /// The file a `>`/`>>` writes.
    Out(bool),
    /// After `>&`: a file descriptor (dropped) or, rarely, a file.
    Dup,
    /// The file a `<` reads: not a word of the command.
    In,
}

struct Scanner<'a> {
    s: &'a str,
    /// The text's characters with their byte offsets, shared with every
    /// scanner nested inside this one: collected once per text, never once
    /// per `$(` (twenty thousand `$(:)` in a row would otherwise cost twenty
    /// thousand copies of the whole call).
    cs: Rc<[(usize, char)]>,
    d: Dialect,
    i: usize,
    out: Vec<Raw>,
    pipeline: usize,
    cur: Raw,
    word: String,
    word_open: bool,
    pending: Vec<(String, bool, usize)>,
    redirect: Option<Pending>,
    /// Scanning the inside of `$(`: stop at the `)` that closes it. Such a
    /// scanner only looks for that `)` and builds no text of its own.
    stop_on_close: bool,
    paren_depth: usize,
    /// How many `$(`/`<(`/`>(` and arithmetic levels deep this scanner sits
    /// (0: the text itself).
    nest: usize,
    /// A nested command ran deeper than `MAX_NEST`, so where it closes was
    /// not read in full.
    too_deep: bool,
    /// What reading this text has already found, shared with every scanner
    /// nested inside it.
    memo: Rc<RefCell<Memo>>,
}

/// Where a `$(` closes, and which `)` matches a `(` in arithmetic, found
/// once per text. Deciding whether `((` is arithmetic reads ahead to its
/// matching `)`, and a nested command read ahead is met again by the scanner
/// after it: without this, a run of `((` or a nest of `$((` would read the
/// same text again at every step, and the cost would double at every level.
#[derive(Default)]
struct Memo {
    /// Char index just after `$(` -> the char index of the `)` closing it,
    /// and whether reading it ran past `MAX_NEST`.
    closes: HashMap<usize, (usize, bool)>,
    /// Char index of a `(` in arithmetic -> the char index of its `)`.
    parens: HashMap<usize, usize>,
}

impl<'a> Scanner<'a> {
    fn new(s: &'a str, d: Dialect) -> Self {
        Scanner::over(s, s.char_indices().collect::<Vec<_>>().into(), Rc::default(), d, 0)
    }

    fn over(s: &'a str, cs: Rc<[(usize, char)]>, memo: Rc<RefCell<Memo>>, d: Dialect, nest: usize) -> Self {
        Scanner {
            s,
            cs,
            d,
            i: 0,
            out: Vec::new(),
            pipeline: 0,
            cur: Raw::new(0, 0),
            word: String::new(),
            word_open: false,
            pending: Vec::new(),
            redirect: None,
            stop_on_close: false,
            paren_depth: 0,
            nest,
            too_deep: false,
            memo,
        }
    }

    /// Char index of the `)` closing a `$(` whose inside starts at `from`,
    /// found by scanning that inside with the full scanner, so a quote or a
    /// parenthesis inside a heredoc body there can never end it early or late.
    /// Nested `MAX_NEST` deep, it stops following them: the `)` is then
    /// found by counting parentheses outside quotes, and `too_deep` says so
    /// (the call is then also read flat - see this module's doc comment,
    /// "WHERE READING STOPS"). Without that stop, two thousand `$(` in a row
    /// overflowed the hook's stack, and a crashed hook lets the call through.
    /// Found once per text (`Memo`).
    fn closing_paren(&mut self, from: usize) -> usize {
        let known = self.memo.borrow().closes.get(&from).copied();
        let (close, deep) = match known {
            Some(found) => found,
            None if self.nest + 1 >= MAX_NEST => (self.balanced_close(from, ')'), true),
            None => {
                let mut inner = Scanner::over(self.s, Rc::clone(&self.cs), Rc::clone(&self.memo), self.d, self.nest + 1);
                inner.i = from;
                inner.cur.start = self.at(from);
                inner.stop_on_close = true;
                let close = inner.scan();
                (close, inner.too_deep)
            }
        };
        self.memo.borrow_mut().closes.insert(from, (close, deep));
        self.too_deep |= deep;
        close
    }

    /// Char index of the `closer` (`)` or `]`) that ends an arithmetic text
    /// whose inside starts at char index `from` - the end of the text when
    /// none does - read as bash reads `((...))`, `$((...))` and `$[...]`:
    /// parens (or brackets) counted, quotes and backslashes skipped, `${...}`
    /// skipped whole, and a `<<` a shift, never a heredoc. The commands in it
    /// still run: with `keep`, the inside of each `$(...)` and backtick
    /// command is kept there (as char indices). Past `MAX_NEST` levels it
    /// only counts, and says so in `too_deep`.
    fn arith_end(&mut self, from: usize, closer: char, mut keep: Option<&mut Vec<(usize, usize)>>) -> usize {
        if self.nest + 1 >= MAX_NEST {
            self.too_deep = true;
            return self.balanced_close(from, closer);
        }
        self.nest += 1;
        let opener = if closer == ']' { '[' } else { '(' };
        let mut opens: Vec<usize> = Vec::new();
        let mut j = from;
        let end = loop {
            let Some(c) = self.ch(j) else { break self.cs.len() };
            match c {
                '\\' => j += 2,
                '\'' => j = self.after_quote(j + 1),
                '"' => j = self.after_double_quote(j + 1, keep.as_deref_mut()),
                '`' => {
                    let close = self.backtick_close(j + 1);
                    if let Some(keep) = keep.as_deref_mut() {
                        keep.push((j + 1, close));
                    }
                    j = close + 1;
                }
                '$' if self.ch(j + 1) == Some('(') => j = self.dollar_paren_end(j, keep.as_deref_mut()),
                '$' if self.ch(j + 1) == Some('[') => j = self.arith_end(j + 2, ']', keep.as_deref_mut()) + 1,
                '$' if self.ch(j + 1) == Some('{') => j = self.balanced_close(j + 2, '}') + 1,
                c if c == opener => {
                    // A pair already matched is skipped whole - unless its
                    // commands are wanted.
                    let known = if keep.is_none() && closer == ')' { self.memo.borrow().parens.get(&j).copied() } else { None };
                    match known {
                        Some(close) => j = close + 1,
                        None => {
                            opens.push(j);
                            j += 1;
                        }
                    }
                }
                c if c == closer => match opens.pop() {
                    None => break j,
                    Some(open) => {
                        if closer == ')' {
                            self.memo.borrow_mut().parens.insert(open, j);
                        }
                        j += 1;
                    }
                },
                _ => j += 1,
            }
        };
        self.nest -= 1;
        end
    }

    /// Char index of the `)` matching the arithmetic `(` at char index
    /// `open` (`arith_end`), found once per text.
    fn arith_match(&mut self, open: usize) -> usize {
        let known = self.memo.borrow().parens.get(&open).copied();
        if let Some(close) = known {
            return close;
        }
        let close = self.arith_end(open + 1, ')', None);
        self.memo.borrow_mut().parens.insert(open, close);
        close
    }

    /// A `$(` at char index `at` (the `$`), read as bash reads it. In sh,
    /// `$((...))` is arithmetic when the paren after `$((` closes right before
    /// the last `)`; otherwise - `$((cmd) )` - it is a command substitution of
    /// its whole inside, which bash finds as a matched pair. Every other `$(`
    /// closes where `closing_paren` finds its `)`. Returns the char index
    /// just past it; with `keep`, the commands it runs are kept there.
    fn dollar_paren_end(&mut self, at: usize, keep: Option<&mut Vec<(usize, usize)>>) -> usize {
        if self.d == Dialect::Sh && self.ch(at + 2) == Some('(') {
            let inner = self.arith_match(at + 2);
            if self.ch(inner + 1) == Some(')') {
                if keep.is_some() {
                    self.arith_end(at + 3, ')', keep);
                }
                return inner + 2;
            }
            let close = self.arith_end(at + 2, ')', None);
            if let Some(keep) = keep {
                keep.push((at + 2, close));
            }
            return close + 1;
        }
        let close = self.closing_paren(at + 2);
        if let Some(keep) = keep {
            keep.push((at + 2, close));
        }
        close + 1
    }

    /// Char index just past the `'` closing a single-quoted text whose inside
    /// starts at `from` (the end of the text when none does).
    fn after_quote(&self, from: usize) -> usize {
        let mut j = from;
        while let Some(c) = self.ch(j) {
            j += 1;
            if c == '\'' {
                return j;
            }
        }
        self.cs.len()
    }

    /// Char index just past the `"` closing a double-quoted text in
    /// arithmetic whose inside starts at `from`. Its `$(...)` and backtick
    /// commands run: with `keep`, they are kept there.
    fn after_double_quote(&mut self, from: usize, mut keep: Option<&mut Vec<(usize, usize)>>) -> usize {
        let mut j = from;
        while let Some(c) = self.ch(j) {
            match c {
                '\\' => j += 2,
                '"' => return j + 1,
                '`' => {
                    let close = self.backtick_close(j + 1);
                    if let Some(keep) = keep.as_deref_mut() {
                        keep.push((j + 1, close));
                    }
                    j = close + 1;
                }
                '$' if self.ch(j + 1) == Some('(') => j = self.dollar_paren_end(j, keep.as_deref_mut()),
                _ => j += 1,
            }
        }
        self.cs.len()
    }
    fn ch(&self, k: usize) -> Option<char> {
        self.cs.get(k).map(|x| x.1)
    }
    fn at(&self, k: usize) -> usize {
        self.cs.get(k).map(|x| x.0).unwrap_or(self.s.len())
    }
    /// The char index of the first character at or after byte `byte` (the
    /// end of the text when there is none): a binary search, never a walk
    /// from the start - a call of ten thousand heredocs would pay one walk
    /// per line of every body.
    fn index_of_byte(&self, byte: usize) -> usize {
        self.cs.partition_point(|x| x.0 < byte)
    }
    fn push(&mut self, c: char) {
        self.word.push(c);
        self.word_open = true;
    }
    fn push_str(&mut self, t: &str) {
        self.word.push_str(t);
        self.word_open = true;
    }
    fn end_word(&mut self) {
        if !self.word_open {
            return;
        }
        let w = std::mem::take(&mut self.word);
        self.word_open = false;
        match self.redirect.take() {
            None => self.cur.words.push(w),
            Some(Pending::Out(append)) => self.cur.redirects.push((w, append)),
            Some(Pending::Dup) => {
                if !(w == "-" || w.chars().all(|c| c.is_ascii_digit())) {
                    self.cur.redirects.push((w, false));
                }
            }
            Some(Pending::In) => {}
        }
    }
    /// A word that is only a file descriptor number (`2` in `2>`), or
    /// PowerShell's `*` stream, is part of the redirection, never a word.
    fn drop_fd_prefix(&mut self) {
        if self.word_open && (self.word == "*" || (!self.word.is_empty() && self.word.chars().all(|c| c.is_ascii_digit()))) {
            self.word.clear();
            self.word_open = false;
        }
    }
    /// End the current simple command at char index `here`, at separator
    /// `sep`; the next one starts at `next`. Only `|` and `|&` keep the next
    /// one in the same pipeline.
    fn cut(&mut self, here: usize, next: usize, sep: Sep) {
        self.end_word();
        self.redirect = None;
        self.cur.end = self.at(here);
        self.cur.ended_by = sep;
        if sep != Sep::Pipe {
            self.pipeline += 1;
        }
        let fresh = Raw::new(self.at(next), self.pipeline);
        let done = std::mem::replace(&mut self.cur, fresh);
        self.out.push(done);
    }

    /// Char index of the `close` (`)`, `]` or `}`) that balances an already
    /// opened `(`, `[` or `{` whose inside starts at char index `from`,
    /// skipping quoted regions - or the end of the text when none does. Never
    /// nests: one pass.
    fn balanced_close(&self, from: usize, close: char) -> usize {
        let open = match close {
            ']' => '[',
            '}' => '{',
            _ => '(',
        };
        let mut depth = 1usize;
        let mut j = from;
        while let Some(c) = self.ch(j) {
            match c {
                '\'' => {
                    j += 1;
                    while let Some(q) = self.ch(j) {
                        j += 1;
                        if q == '\'' {
                            break;
                        }
                    }
                    continue;
                }
                '"' => {
                    j += 1;
                    while let Some(q) = self.ch(j) {
                        if q == '\\' && self.d == Dialect::Sh {
                            j += 2;
                            continue;
                        }
                        j += 1;
                        if q == '"' {
                            break;
                        }
                    }
                    continue;
                }
                '\\' if self.d == Dialect::Sh => {
                    j += 2;
                    continue;
                }
                c if c == open => depth += 1,
                c if c == close => {
                    depth -= 1;
                    if depth == 0 {
                        return j;
                    }
                }
                _ => {}
            }
            j += 1;
        }
        self.cs.len()
    }

    /// Char index of the backtick closing one whose inside starts at char
    /// index `from`, or the end of the text when none does.
    fn backtick_close(&self, from: usize) -> usize {
        let mut j = from;
        while let Some(c) = self.ch(j) {
            if c == '\\' {
                j += 2;
                continue;
            }
            if c == '`' {
                return j;
            }
            j += 1;
        }
        self.cs.len()
    }

    /// An expansion from char index `at` up to `next` (a command
    /// substitution, a backtick command, arithmetic): text of the current
    /// word, then skipped, with the commands it runs (`keep`, the char
    /// indices of each one's inside) kept to be read. A scanner that only
    /// looks for a `)` keeps nothing - it only notes that a word is open - so
    /// a deep nest costs no copy per level.
    fn expansion(&mut self, at: usize, next: usize, keep: Vec<(usize, usize)>) {
        let next = next.min(self.cs.len());
        if self.stop_on_close {
            self.word_open = true;
        } else {
            for (from, to) in keep {
                let end = self.at(to).min(self.s.len());
                let begin = self.at(from).min(end);
                self.cur.subs.push(self.s[begin..end].to_string());
            }
            let start = self.at(at);
            let raw = self.s[start..self.at(next).max(start)].to_string();
            self.push_str(&raw);
        }
        self.i = next;
    }

    /// A backtick command substitution at `i`.
    fn backtick(&mut self) {
        let at = self.i;
        let close = self.backtick_close(at + 1);
        self.expansion(at, close + 1, vec![(at + 1, close)]);
    }

    /// sh `$'...'` at `i`: a quoted word in which a backslash escapes any
    /// character, a quote included. Read as two plain quotes, `$'a\'b'` would
    /// open a string that is not there and hide the commands after it.
    fn ansi_c_quoted(&mut self) {
        let mut j = self.i + 2;
        let mut text = String::new();
        while let Some(c) = self.ch(j) {
            match c {
                '\\' => {
                    match self.ch(j + 1) {
                        Some('\'') => text.push('\''),
                        Some(n) => {
                            text.push('\\');
                            text.push(n);
                        }
                        None => text.push('\\'),
                    }
                    j += 2;
                }
                '\'' => {
                    j += 1;
                    break;
                }
                _ => {
                    text.push(c);
                    j += 1;
                }
            }
        }
        self.push_str(&text);
        self.i = j;
    }

    /// `$(`, `$((`, `<(`, `>(` and sh's `$[` at `i` (pointing at the
    /// `$`/`<`/`>`): one word of the current command, whose commands are kept
    /// to be read (see `dollar_paren_end` and `arith_end`).
    fn substitution(&mut self) {
        let at = self.i;
        let mut keep = Vec::new();
        let wanted = (!self.stop_on_close).then_some(&mut keep);
        let next = match (self.ch(at), self.ch(at + 1)) {
            (Some('$'), Some('[')) => self.arith_end(at + 2, ']', wanted) + 1,
            (Some('$'), _) => self.dollar_paren_end(at, wanted),
            _ => {
                let close = self.closing_paren(at + 2);
                if let Some(keep) = wanted {
                    keep.push((at + 2, close));
                }
                close + 1
            }
        };
        self.expansion(at, next, keep);
    }

    /// sh's arithmetic command `((...))` at `i`, when bash reads it as one:
    /// the paren after `((` closes right before a second `)`. Then it is one
    /// word, whose `<<` is a shift and never a heredoc, and whose own
    /// commands are kept; `true`. Otherwise - `((cmd) )` is two nested
    /// subshells, as bash runs it - `false`, and nothing is read.
    fn arithmetic_command(&mut self) -> bool {
        let at = self.i;
        let inner = self.arith_match(at + 1);
        if self.ch(inner + 1) != Some(')') {
            return false;
        }
        let mut keep = Vec::new();
        if !self.stop_on_close {
            self.arith_end(at + 2, ')', Some(&mut keep));
        }
        self.expansion(at, inner + 2, keep);
        true
    }

    fn single_quoted(&mut self) {
        let mut j = self.i + 1;
        let mut text = String::new();
        while let Some(c) = self.ch(j) {
            if c == '\'' {
                if self.d == Dialect::PowerShell && self.ch(j + 1) == Some('\'') {
                    text.push('\'');
                    j += 2;
                    continue;
                }
                j += 1;
                break;
            }
            text.push(c);
            j += 1;
        }
        self.push_str(&text);
        self.i = j;
    }

    fn double_quoted(&mut self) {
        self.i += 1;
        self.word_open = true;
        while let Some(c) = self.ch(self.i) {
            match c {
                '"' => {
                    if self.d == Dialect::PowerShell && self.ch(self.i + 1) == Some('"') {
                        self.push('"');
                        self.i += 2;
                        continue;
                    }
                    self.i += 1;
                    return;
                }
                '\\' if self.d == Dialect::Sh => {
                    match self.ch(self.i + 1) {
                        Some(n @ ('"' | '\\' | '$' | '`')) => self.push(n),
                        Some('\n') => {}
                        Some(n) => {
                            self.push('\\');
                            self.push(n);
                        }
                        None => self.push('\\'),
                    }
                    self.i += 2;
                }
                '`' if self.d == Dialect::PowerShell => {
                    if let Some(n) = self.ch(self.i + 1) {
                        self.push(n);
                    }
                    self.i += 2;
                }
                '`' => self.backtick(),
                '$' if self.ch(self.i + 1) == Some('(') => self.substitution(),
                _ => {
                    self.push(c);
                    self.i += 1;
                }
            }
        }
    }

    /// PowerShell here-string `@'...'@` / `@"..."@`: data up to a line that
    /// starts with the closing pair.
    fn here_string(&mut self) {
        let quote = self.ch(self.i + 1).unwrap_or('\'');
        let close = format!("\n{quote}@");
        let body_start = self.at(self.i + 2);
        match self.s[body_start..].find(&close) {
            Some(off) => {
                let body = self.s[body_start..body_start + off].to_string();
                self.push_str(&body);
                let end_byte = body_start + off + close.len();
                self.i = self.index_of_byte(end_byte);
            }
            None => {
                let body = self.s[body_start..].to_string();
                self.push_str(&body);
                self.i = self.cs.len();
            }
        }
    }

    fn heredoc_operator(&mut self) {
        // at `<<`, not `<<<`
        self.end_word();
        self.i += 2;
        let strip_tabs = self.ch(self.i) == Some('-');
        if strip_tabs {
            self.i += 1;
        }
        while matches!(self.ch(self.i), Some(' ' | '\t')) {
            self.i += 1;
        }
        let mut delim = String::new();
        while let Some(c) = self.ch(self.i) {
            match c {
                '\'' | '"' => {
                    let q = c;
                    self.i += 1;
                    while let Some(x) = self.ch(self.i) {
                        self.i += 1;
                        if x == q {
                            break;
                        }
                        delim.push(x);
                    }
                }
                '\\' => {
                    if let Some(x) = self.ch(self.i + 1) {
                        delim.push(x);
                    }
                    self.i += 2;
                }
                c if c.is_whitespace() || ";|&<>()".contains(c) => break,
                _ => {
                    delim.push(c);
                    self.i += 1;
                }
            }
        }
        if !delim.is_empty() {
            let idx = self.out.len();
            self.pending.push((delim, strip_tabs, idx));
        }
    }

    /// Called right after a newline was consumed: read every pending heredoc
    /// body, in order, and hand each to the segment that declared it.
    fn heredoc_bodies(&mut self) {
        let pending = std::mem::take(&mut self.pending);
        for (delim, strip_tabs, idx) in pending {
            let mut body = String::new();
            loop {
                if self.i >= self.cs.len() {
                    break;
                }
                let line_start = self.at(self.i);
                let nl = self.s[line_start..].find('\n').map(|o| line_start + o);
                let line_end = nl.unwrap_or(self.s.len());
                let line = self.s[line_start..line_end].trim_end_matches('\r');
                let cmp = if strip_tabs { line.trim_start_matches('\t') } else { line };
                let next_byte = nl.map(|b| b + 1).unwrap_or(self.s.len());
                self.i = self.index_of_byte(next_byte);
                if cmp == delim {
                    break;
                }
                body.push_str(line);
                body.push('\n');
            }
            if let Some(r) = self.out.get_mut(idx) {
                r.heredoc.push_str(&body);
            } else {
                self.cur.heredoc.push_str(&body);
            }
        }
        self.cur.start = self.at(self.i);
    }

    /// Every simple command of the text, and whether a nested command ran
    /// deeper than `MAX_NEST`.
    fn run(mut self) -> (Vec<Raw>, bool) {
        self.scan();
        let end = self.cs.len();
        self.cut(end, end, Sep::End);
        (self.out, self.too_deep)
    }

    /// The main loop; returns the char index it stopped at (the closing `)`
    /// in `stop_on_close` mode, the end of the text otherwise).
    fn scan(&mut self) -> usize {
        let sh = self.d == Dialect::Sh;
        let ps = self.d == Dialect::PowerShell;
        while let Some(c) = self.ch(self.i) {
            let i = self.i;
            match c {
                '\n' => {
                    self.cut(i, i + 1, Sep::Newline);
                    self.i += 1;
                    if !self.pending.is_empty() {
                        self.heredoc_bodies();
                    }
                }
                ' ' | '\t' | '\r' => {
                    self.end_word();
                    self.i += 1;
                }
                '#' if !self.word_open => {
                    while let Some(x) = self.ch(self.i) {
                        if x == '\n' {
                            break;
                        }
                        self.i += 1;
                    }
                }
                '\\' if sh => {
                    match self.ch(i + 1) {
                        Some('\n') => {}
                        Some(n) => self.push(n),
                        None => {}
                    }
                    self.i += 2;
                }
                '`' if ps => {
                    match self.ch(i + 1) {
                        Some('\n') => {}
                        Some(n) => self.push(n),
                        None => {}
                    }
                    self.i += 2;
                }
                '`' => self.backtick(),
                '\'' => self.single_quoted(),
                '"' => self.double_quoted(),
                '@' if ps && matches!(self.ch(i + 1), Some('\'' | '"')) && matches!(self.ch(i + 2), Some('\n' | '\r')) => {
                    self.here_string()
                }
                '$' if sh && self.ch(i + 1) == Some('\'') => self.ansi_c_quoted(),
                '$' if self.ch(i + 1) == Some('(') => self.substitution(),
                '$' if sh && self.ch(i + 1) == Some('[') => self.substitution(),
                '<' | '>' if sh && self.ch(i + 1) == Some('(') => self.substitution(),
                '<' if ps && self.ch(i + 1) == Some('#') => {
                    let from = self.at(i);
                    match self.s[from..].find("#>") {
                        Some(off) => self.i = self.index_of_byte(from + off + 2),
                        None => self.i = self.cs.len(),
                    }
                }
                '<' if sh && self.ch(i + 1) == Some('<') && self.ch(i + 2) != Some('<') => self.heredoc_operator(),
                '<' => {
                    self.drop_fd_prefix();
                    self.end_word();
                    if self.ch(i + 1) == Some('<') && self.ch(i + 2) == Some('<') {
                        // A here-string: the text that follows is the
                        // command's input, kept as a word of it.
                        self.i += 3;
                    } else {
                        self.i += 1;
                        self.redirect = Some(Pending::In);
                    }
                }
                '>' => {
                    self.drop_fd_prefix();
                    self.end_word();
                    self.i += 1;
                    let mut append = false;
                    if self.ch(self.i) == Some('>') {
                        append = true;
                        self.i += 1;
                    }
                    match self.ch(self.i) {
                        Some('&') => {
                            self.i += 1;
                            self.redirect = Some(Pending::Dup);
                        }
                        Some('|') => {
                            self.i += 1;
                            self.redirect = Some(Pending::Out(append));
                        }
                        _ => self.redirect = Some(Pending::Out(append)),
                    }
                }
                '&' => {
                    if self.ch(i + 1) == Some('&') {
                        self.cut(i, i + 2, Sep::And);
                        self.i += 2;
                    } else if self.ch(i + 1) == Some('>') {
                        self.end_word();
                        self.i += 2;
                        let mut append = false;
                        if self.ch(self.i) == Some('>') {
                            append = true;
                            self.i += 1;
                        }
                        self.redirect = Some(Pending::Out(append));
                    } else {
                        self.cut(i, i + 1, Sep::Background);
                        self.i += 1;
                    }
                }
                '|' => {
                    if self.ch(i + 1) == Some('|') {
                        self.cut(i, i + 2, Sep::Or);
                        self.i += 2;
                    } else if self.ch(i + 1) == Some('&') {
                        self.cut(i, i + 2, Sep::Pipe);
                        self.i += 2;
                    } else {
                        self.cut(i, i + 1, Sep::Pipe);
                        self.i += 1;
                    }
                }
                ';' => {
                    self.cut(i, i + 1, Sep::Semi);
                    self.i += 1;
                }
                '(' if sh && !self.word_open && self.ch(i + 1) == Some('(') && self.arithmetic_command() => {}
                '(' if !self.word_open => {
                    self.paren_depth += 1;
                    self.cut(i, i + 1, Sep::Open);
                    self.i += 1;
                }
                ')' => {
                    if self.stop_on_close && self.paren_depth == 0 {
                        return i;
                    }
                    self.paren_depth = self.paren_depth.saturating_sub(1);
                    self.cut(i, i + 1, Sep::Close);
                    self.i += 1;
                }
                '{' | '}' if ps => {
                    self.cur.closed_by = Some(c);
                    self.cut(i, i + 1, if c == '{' { Sep::OpenBrace } else { Sep::CloseBrace });
                    self.i += 1;
                }
                '{' | '}' if sh && !self.word_open && self.ch(i + 1).is_none_or(|n| n.is_whitespace() || n == ';') => {
                    self.cut(i, i + 1, if c == '{' { Sep::OpenBrace } else { Sep::CloseBrace });
                    self.i += 1;
                }
                _ => {
                    self.push(c);
                    self.i += 1;
                }
            }
        }
        self.cs.len()
    }
}

// ------------------------------------------------------------------ matching

/// Whether `word`, lowercased, is `lower` - without building the lowercased
/// word (this runs for every anchor against every segment of a call).
fn eq_lower(word: &str, lower: &str) -> bool {
    word.chars().flat_map(char::to_lowercase).eq(lower.chars())
}

fn prefix(words: &[String], anchor: &[String]) -> bool {
    words.len() >= anchor.len() && words.iter().zip(anchor).all(|(w, a)| eq_lower(w, a))
}

/// An anchor, read once.
struct Anchor {
    /// Its words, lowercased.
    words: Vec<String>,
    /// Its first word when that word ends in `.exe` (directory dropped,
    /// lowercased): then only a call whose program is exactly that name is
    /// reached.
    exe: Option<String>,
    /// More than one simple command (it carries `&&`, `;`, `|`, `$(`...).
    compound: bool,
}

thread_local! {
    /// Every anchor read so far on this thread, so a call of many segments
    /// reads each anchor once, not once per segment.
    static ANCHORS: RefCell<HashMap<String, Rc<Anchor>>> = RefCell::new(HashMap::new());
}

/// `anchor`, read once per thread (`parse_anchor`).
fn anchor_of(anchor: &str) -> Rc<Anchor> {
    ANCHORS.with(|cache| {
        if let Some(read) = cache.borrow().get(anchor) {
            return Rc::clone(read);
        }
        let read = Rc::new(parse_anchor(anchor));
        let mut cache = cache.borrow_mut();
        if cache.len() >= 4096 {
            cache.clear();
        }
        cache.insert(anchor.to_string(), Rc::clone(&read));
        read
    })
}

fn parse_anchor(anchor: &str) -> Anchor {
    // Backslash is literal in an anchor: a Windows path is written as typed.
    let (parsed, _) = Scanner::new(anchor, Dialect::PowerShell).run();
    let nonempty: Vec<&Raw> = parsed.iter().filter(|r| !r.words.is_empty()).collect();
    if nonempty.len() != 1 || !nonempty[0].subs.is_empty() {
        return Anchor { words: Vec::new(), exe: None, compound: true };
    }
    let words: Vec<String> = nonempty[0].words.iter().map(|w| w.to_lowercase()).collect();
    let exe = words.first().map(|w| w.rsplit(['/', '\\']).next().unwrap_or(w).to_string()).filter(|w| w.ends_with(".exe"));
    Anchor { words, exe, compound: false }
}

/// The old whole-text comparison, kept for an anchor that is itself more than
/// one simple command: its words are the first words of `raw`, as typed or
/// with a leading `sudo`, `NAME=value`, directory and `.exe` taken off.
fn raw_anchor_names(anchor: &str, raw: &str) -> bool {
    let anchor_words: Vec<String> = anchor.split_whitespace().map(str::to_lowercase).collect();
    if anchor_words.is_empty() {
        return false;
    }
    let first_words = |text: &str| -> Vec<String> {
        text.split_whitespace().take(anchor_words.len()).map(str::to_lowercase).collect()
    };
    if first_words(raw) == anchor_words {
        return true;
    }
    let mut words: Vec<&str> = raw.split_whitespace().collect();
    while let Some(first) = words.first() {
        if *first == "sudo" || (first.contains('=') && !first.starts_with('-')) {
            words.remove(0);
        } else {
            break;
        }
    }
    let mut stripped: Vec<String> = words.iter().map(|w| w.to_string()).collect();
    if let Some(first) = stripped.first_mut() {
        let base = first.rsplit(['/', '\\']).next().unwrap_or(first).to_string();
        *first = base.strip_suffix(".exe").or_else(|| base.strip_suffix(".EXE")).unwrap_or(&base).to_string();
    }
    first_words(&stripped.join(" ")) == anchor_words
}

/// The words of `seg` after `anchor`, when `anchor` reaches it; `None` when it
/// does not. For an anchor that is itself compound (whole-text comparison)
/// the rest is empty.
fn rest_after<'s>(anchor: &str, seg: &'s Segment) -> Option<&'s [String]> {
    rest_after_parsed(&anchor_of(anchor), anchor, seg)
}

fn rest_after_parsed<'s>(a: &Anchor, anchor: &str, seg: &'s Segment) -> Option<&'s [String]> {
    if a.compound {
        return raw_anchor_names(anchor, &seg.raw).then_some(&seg.words[seg.words.len()..]);
    }
    if a.words.is_empty() {
        return None;
    }
    if seg.flat {
        return flat_rest(a, seg);
    }
    if let Some(exe) = &a.exe {
        let reached = seg.program == *exe && prefix(seg.head.get(1..).unwrap_or(&[]), &a.words[1..]);
        return reached.then(|| &seg.head[a.words.len().min(seg.head.len())..]);
    }
    if prefix(&seg.head, &a.words) {
        return Some(&seg.head[a.words.len()..]);
    }
    if prefix(&seg.words, &a.words) {
        return Some(&seg.words[a.words.len()..]);
    }
    None
}

/// The words of a flat segment after `a`, found wherever its words stand:
/// the first word compared as typed or without its folder and `.exe` (an
/// anchor written with `.exe` needs that exact program name), every other
/// word as typed, all ignoring case.
fn flat_rest<'s>(a: &Anchor, seg: &'s Segment) -> Option<&'s [String]> {
    let len = a.words.len();
    if seg.words.len() < len {
        return None;
    }
    (0..=seg.words.len() - len)
        .find(|&p| {
            let first = &seg.words[p];
            let first_ok = match &a.exe {
                Some(exe) => first.rsplit(['/', '\\']).next().is_some_and(|base| eq_lower(base, exe)),
                None => seg.head.get(p).is_some_and(|base| *base == a.words[0]) || eq_lower(first, &a.words[0]),
            };
            first_ok && seg.words[p + 1..p + len].iter().zip(&a.words[1..]).all(|(w, x)| eq_lower(w, x))
        })
        .map(|p| &seg.words[p + len..])
}

/// Whether `anchor` reaches `seg`: its words are a prefix of the segment's
/// words as typed or of its head (see this module's doc comment, "THE HEAD"),
/// or - in a flat segment - stand anywhere in it.
pub fn anchor_reaches(anchor: &str, seg: &Segment) -> bool {
    rest_after(anchor, seg).is_some()
}

/// Whether `anchor` reaches any of `segs` (the anchor is read once).
pub fn reaches_any(anchor: &str, segs: &[Segment]) -> bool {
    let a = anchor_of(anchor);
    segs.iter().any(|s| rest_after_parsed(&a, anchor, s).is_some())
}

/// The words that ask a program about itself and run nothing.
const HELP_WORDS: &[&str] = &["--help", "-h", "-?", "/?", "help", "--version", "-V"];

/// Whether `seg`, reached by `anchor`, is a pure help or version call: every
/// word after the anchor is one of `--help`, `-h`, `-?`, `/?`, `help`,
/// `--version`, `-V`, and there is at least one. Such a call asks the program
/// about itself and runs nothing, so a requires check never refuses it (a
/// forbidden check is not exempted).
pub fn is_pure_help_call(anchor: &str, seg: &Segment) -> bool {
    rest_after(anchor, seg).is_some_and(|rest| !rest.is_empty() && rest.iter().all(|w| HELP_WORDS.contains(&w.as_str())))
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn is_boundary_char(c: char) -> bool {
    c.is_whitespace()
        || matches!(c, '"' | '\'' | ';' | '|' | '&' | '(' | ')' | '{' | '}' | '<' | '>' | '=' | ',' | ':' | '/' | '\\')
}

/// Whether the text right before a literal ends in an escaped whitespace -
/// `\n`, `\t`, `\r` (printf, `echo -e`) or PowerShell's `` `n ``, `` `t ``,
/// `` `r `` - which stands for the whitespace it writes: `printf
/// 'fix\n\nCo-Authored-By: C'` puts the trailer on a line of its own.
fn ends_in_escaped_space(before: &str) -> bool {
    let mut back = before.chars().rev();
    matches!((back.next(), back.next()), (Some('n' | 't' | 'r'), Some('\\' | '`')))
}

/// Whether the text right after a literal starts with PowerShell's escaped
/// whitespace (`` `n ``, `` `t ``, `` `r ``). The sh form (`\n`) needs no case of
/// its own: a backslash is a boundary.
fn starts_with_escaped_space(after: &str) -> bool {
    let mut fwd = after.chars();
    matches!((fwd.next(), fwd.next()), (Some('`'), Some('n' | 't' | 'r')))
}

/// The byte offset of the first occurrence of `literal` in `text` that sits on
/// token boundaries (see this module's doc comment, "TOKEN BOUNDARIES"). An
/// escaped whitespace (`\n`, `` `n ``) next to the literal counts as the
/// whitespace it stands for. An empty literal is found at the start, as a
/// plain substring search finds it (the write gate never stores one).
pub fn find_at_boundary(text: &str, literal: &str) -> Option<usize> {
    let (Some(first), Some(last)) = (literal.chars().next(), literal.chars().next_back()) else {
        return Some(0);
    };
    let mut from = 0;
    while let Some(off) = text.get(from..).and_then(|t| t.find(literal)) {
        let at = from + off;
        let before = text[..at].chars().next_back();
        let rest = &text[at + literal.len()..];
        let mut after = rest.chars();
        let left_ok = !is_word_char(first) || before.is_none_or(is_boundary_char) || ends_in_escaped_space(&text[..at]);
        let right_ok = !is_word_char(last)
            || starts_with_escaped_space(rest)
            || match after.next() {
                None => true,
                Some('.') => after.next().is_none_or(|c| !is_word_char(c)),
                Some(c) => is_boundary_char(c),
            };
        if left_ok && right_ok {
            return Some(at);
        }
        from = at + first.len_utf8();
    }
    None
}

/// Whether `literal` occurs in `text` on token boundaries.
pub fn literal_at_boundary(text: &str, literal: &str) -> bool {
    find_at_boundary(text, literal).is_some()
}

/// `literal` with its whitespace collapsed to single spaces - the form it is
/// looked for in a segment's joined words.
fn collapsed(literal: &str) -> String {
    literal.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Where `literal` sits in `seg`, on token boundaries: the text it was found
/// in (the unit's raw text, its joined words, or the joined head) and the
/// literal as found there. `None` when it is in none of them.
pub fn literal_site(seg: &Segment, literal: &str) -> Option<(String, String)> {
    if let Some(at) = find_at_boundary(&seg.unit, literal) {
        // Quote the part it sits in: the command text, or what follows it.
        let cut = seg.command_len.min(seg.unit.len());
        let part = if at < cut { seg.unit.get(..cut) } else { seg.unit.get(cut..) };
        let part = part.map(str::trim).filter(|p| literal_at_boundary(p, literal)).unwrap_or(&seg.unit);
        return Some((part.to_string(), literal.to_string()));
    }
    let short = collapsed(literal);
    if short.is_empty() {
        return None;
    }
    if literal_at_boundary(&seg.unit_words, &short) {
        return Some((seg.unit_words.clone(), short));
    }
    let head = seg.head_text();
    literal_at_boundary(&head, &short).then(|| (head, short))
}

/// Whether a forbidden `literal` is in `seg`'s unit (see this module's doc
/// comment, "THE UNIT").
pub fn literal_in(seg: &Segment, literal: &str) -> bool {
    literal_site(seg, literal).is_some()
}

/// Whether an inner `requires` trigger occurs in `unit`, on token
/// boundaries. A trigger ending in ONE `*` is a prefix: its stem must start
/// on a boundary, and anything may follow it (`/thor*` finds `/thor-probe`
/// and `/thor2`, and - the stated price - `build/thorough`).
pub fn trigger_in(unit: &str, when: &str) -> bool {
    let when = when.trim();
    match star_stem(when) {
        Some(stem) => prefix_at_boundary(unit, stem),
        None => !when.is_empty() && literal_at_boundary(unit, when),
    }
}

/// The stem of a trigger that ends in exactly one `*`.
pub fn star_stem(when: &str) -> Option<&str> {
    let stem = when.strip_suffix('*')?;
    (!stem.is_empty() && !stem.ends_with('*')).then_some(stem)
}

fn prefix_at_boundary(text: &str, stem: &str) -> bool {
    let Some(first) = stem.chars().next() else { return false };
    let mut from = 0;
    while let Some(off) = text.get(from..).and_then(|t| t.find(stem)) {
        let at = from + off;
        let before = text[..at].chars().next_back();
        if !is_word_char(first) || before.is_none_or(is_boundary_char) {
            return true;
        }
        from = at + first.len_utf8();
    }
    false
}

/// Whether an inner `requires` trigger occurs in `seg`: in its unit, its
/// joined words or its joined head (the same three places `literal_in` reads,
/// so quoting a word never hides a trigger either).
pub fn trigger_in_segment(seg: &Segment, when: &str) -> bool {
    if trigger_in(&seg.unit, when) {
        return true;
    }
    let short = collapsed(when);
    trigger_in(&seg.unit_words, &short) || trigger_in(&seg.head_text(), &short)
}

#[cfg(test)]
mod tests;
