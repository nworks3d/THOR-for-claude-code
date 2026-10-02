//! What was about to happen, in the shape `rank::select` needs: zero or more
//! detected moments, zero or more targets, and the raw context text the
//! closeness key is derived from. Every constructor here is the ONLY place a
//! caller (hook, check, why) turns a real command/path/target into this
//! shape, so all three channels build it identically.

use intent::Action;
use model::item::TargetKind;
use std::path::PathBuf;

/// One whitespace-separated argument, stripped of the shell punctuation that
/// wraps or follows it, and of a leading `--flag=`.
///
/// Deliberately not a shell parser. It never resolves a variable, a glob or a
/// pipeline; it only recovers the literal words a person typed, which is all
/// an anchor match needs.
fn shell_tokens(command: &str) -> Vec<String> {
    command
        .split_whitespace()
        .map(|raw| {
            let t = raw.trim_matches(|c| matches!(c, '"' | '\'' | '(' | ')' | ';' | ',' | '`' | '|' | '<' | '>'));
            match t.split_once('=') {
                // `--db=path` and `FOO=path` both hand over the right half.
                Some((_, rhs)) if !rhs.is_empty() && t.starts_with(|c: char| c == '-' || c.is_ascii_uppercase()) => rhs,
                _ => t,
            }
            .trim_end_matches(['.', ':', ';'])
            .to_string()
        })
        .filter(|t| !t.is_empty())
        .collect()
}

/// The last dot-suffix of a token, when it looks like a file extension:
/// 1 to 8 characters, letters and digits only.
fn extension_of(token: &str) -> Option<&str> {
    let (_, ext) = token.rsplit_once('.')?;
    let ok = !ext.is_empty()
        && ext.len() <= 8
        && ext.chars().all(|c| c.is_ascii_alphanumeric());
    ok.then_some(ext)
}

/// Every file the command NAMES, as an anchor value.
///
/// THE DEFECT THIS CLOSES, measured on 2026-08-07 in two places. A fact
/// anchored at a file fired when that file was opened with a file tool, and
/// stayed silent when a shell command read the very same file. Reproduced
/// here: three facts fired for `thor2/model/src/gate.rs` as a file and zero
/// for `grep -n Refusal thor2/model/src/gate.rs`. Shell is how deploy and
/// infrastructure work happens, so that is exactly where those facts were
/// needed and exactly where they said nothing. A warning about a grep trap in
/// a log file stayed quiet during that grep, and the mistake it named was
/// made.
///
/// Not a widened trigger: the anchor is unchanged, and it still matches only
/// the file it always named. What changes is that the file is now recognised
/// through a second, equally real route.
pub fn paths_in_command(command: &str) -> Vec<String> {
    let mut out = Vec::new();
    for token in shell_tokens(command) {
        if token.contains("://") || token.starts_with('-') || token.contains('*') || token.contains('?') {
            continue;
        }
        let normalised = token.replace('\\', "/");
        let has_separator = normalised.contains('/');
        if !has_separator && extension_of(&normalised).is_none() {
            continue;
        }
        if !out.contains(&normalised) {
            out.push(normalised);
        }
    }
    out
}

/// Every host the command NAMES, as an anchor value: the authority half of a
/// URL, or a bare dotted name.
///
/// A filename is the hard case, because `gate.rs` has exactly the shape of a
/// hostname. A bare token is therefore only read as a host when its last
/// label is NOT a known source extension (`model::gate::FILE_EXTENSIONS`, the
/// same list the write gate uses to tell `guard.rs:610` from `host:8080`).
/// That trades a miss for a false fire on purpose: a real TLD that is also a
/// source extension (`.md`, `.cc`) is skipped, which costs a hint, while the
/// opposite error would put a fact about a server in front of somebody
/// editing a file.
pub fn hosts_in_command(command: &str) -> Vec<String> {
    let mut out = Vec::new();
    for token in shell_tokens(command) {
        let candidate = match token.split_once("://") {
            // Strip scheme, then userinfo, then path, then port.
            Some((_, rest)) => {
                let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
                let authority = authority.rsplit('@').next().unwrap_or(authority);
                authority.split(':').next().unwrap_or(authority).to_string()
            }
            None => {
                if token.contains('/') || token.starts_with('-') {
                    continue;
                }
                // `ssh admin@host` names a host as plainly as a URL does.
                let token = token.rsplit('@').next().unwrap_or(&token).to_string();
                match extension_of(&token) {
                    // A dotted name whose tail is a source extension is a
                    // file, not a host.
                    Some(ext) if model::gate::FILE_EXTENSIONS.contains(&ext.to_lowercase().as_str()) => continue,
                    Some(ext) if ext.chars().all(|c| c.is_ascii_alphabetic()) && ext.len() >= 2 => {
                        token.split(':').next().unwrap_or(&token).to_string()
                    }
                    _ => continue,
                }
            }
        };
        if candidate.is_empty() || !candidate.contains('.') {
            continue;
        }
        if !out.contains(&candidate) {
            out.push(candidate);
        }
    }
    out
}

// ------------------------------------------------------- the shell write hole
//
// MEASURED: a rule anchored at a file with a `contains` or `absent` check
// refuses an Edit/Write of that file, but `rm file`, `truncate -s 0 file`,
// `sed -i ... file`, `echo x > file`, `tee file`, `mv other file` and
// `cp other file` from Bash walk straight past it - a shell write is a
// write, and until now only Edit/Write ever counted as one.
// `shell_write_targets` below is what `serve::absent_guard`'s command guard
// (`serve::guard_verdict::command`) feeds through the SAME
// file-based checks a real Edit/Write already goes through, so the same
// rule refuses both.
//
// THE SAME SEGMENTS AS EVERY COMMAND TOOTH (the door, 2026-09-28). The call
// is read with `serve::shell::segments`, the one tokenizer: a write on its
// own line after a `cd` (`cd x<NL>rm f`), inside `$(...)`, or inside a
// `bash -lc '...'` that is not at the head of the line is found, where the
// old whitespace-and-operator tokenizer missed all three. A heredoc body fed
// to `cat` is data, never a write; a command sent over `ssh` runs on another
// machine, so a file it names is never a local file.

/// A candidate argument names a real path, never a flag or a glob - the two
/// filters every verb-specific reader below shares. GLOBS ARE IGNORED
/// deliberately: a glob names nothing until the shell expands it, and this
/// function never runs a shell, so there is no way to know which real files
/// a pattern like `*.log` would touch - guessing and refusing on that guess
/// would refuse honest work the glob may not even reach the guarded file at
/// all.
fn is_path_arg(w: &str) -> bool {
    !w.starts_with('-') && !w.contains('*') && !w.contains('?')
}

/// What actually happens to the file a shell write touches - never a bare
/// "written", because a rule anchored there needs to know WHICH proof still
/// applies: a `Contains` check has nothing left to hold once the file is
/// gone or emptied, while a location prohibition cares only that the path
/// was touched at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellWriteEffect {
    /// `rm`/`git rm`: the file stops existing.
    Removed,
    /// `truncate -s 0`, or a bare `> f` / `: > f` with nothing feeding it:
    /// the file still exists, but every byte it held is gone.
    Emptied,
    /// `sed -i`, `tee`, a `>`/`>>` redirection with a real producer in front
    /// of it, or a `mv`/`cp` destination: the file's content changes, but
    /// it is not necessarily reduced to nothing.
    Rewritten,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellWrite {
    pub path: String,
    pub effect: ShellWriteEffect,
}

/// A shell write together with a directory of the segment that makes it
/// (`shell::Segment::dir`, or `shell::Segment::any_cd_dir` when an earlier
/// `cd` may not have run): a relative path in it is relative to that folder,
/// not to the session's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellWriteAt {
    pub write: ShellWrite,
    pub dir: Option<std::sync::Arc<str>>,
}

/// Every real WRITE a shell command performs directly, bypassing the
/// Edit/Write tool entirely, paired with what actually happens to the file -
/// see this module's own "the shell write hole" section above for the
/// measurement this closes, and `ShellWriteEffect`'s own doc comment for why
/// the effect is never collapsed to a bare "written". Read in the sh dialect;
/// the hook reads the call's own segments with [`shell_writes_in`].
///
/// FLAGS ARE SKIPPED (a token starting with `-` is never read as a path -
/// `rm -rf` names one flag and zero files on its own). GLOBS ARE IGNORED
/// (see `is_path_arg`'s own doc comment for why guessing would cost more
/// than it catches). A QUOTED PATH WITH A SPACE is read whole.
///
/// `sh -c "..."`, `bash -lc "..."`, `powershell -Command "..."` and every
/// other wrapper `serve::shell` descends are read as the command they run,
/// never as the wrapper's own words - a Bash tool call routed through one of
/// these would otherwise name no file at all.
pub fn shell_write_targets(command: &str) -> Vec<ShellWrite> {
    shell_writes_in(&crate::shell::segments(command, crate::shell::Dialect::Sh)).into_iter().map(|w| w.write).collect()
}

/// [`shell_write_targets`] over a call already cut into `segments`, each
/// write paired with its segment's directory - twice when an earlier `cd`
/// that may not have run would put it elsewhere (`Segment::any_cd_dir`), so
/// it is judged in both folders. A segment reached through `ssh` is skipped:
/// the file it names is on another machine. So is the whole call read flat
/// (`Segment::flat`), whose words are not one command.
pub fn shell_writes_in(segments: &[crate::shell::Segment]) -> Vec<ShellWriteAt> {
    let mut out = Vec::new();
    for segment in segments {
        if segment.remote || segment.flat {
            continue;
        }
        let mut writes = Vec::new();
        collect_segment_writes(&segment.head, &mut writes);
        for redirect in &segment.redirects {
            collect_redirect(redirect, &mut writes);
        }
        for write in writes {
            out.push(ShellWriteAt { write: write.clone(), dir: segment.dir.clone() });
            if segment.any_cd_dir != segment.dir {
                out.push(ShellWriteAt { write, dir: segment.any_cd_dir.clone() });
            }
        }
    }
    out
}

/// The size `truncate -s <n>` was given, and the file it names, if any -
/// `-s 0` and `-s0` both spell the same flag. `-s 0` (and only `-s 0`)
/// empties the file; any other size still rewrites it (it may grow, or
/// shrink to something other than nothing), so only the zero case is ever
/// reported as `Emptied`.
fn collect_truncate(rest: &[&str], out: &mut Vec<ShellWrite>) {
    let mut size: Option<&str> = None;
    let mut file: Option<&str> = None;
    let mut i = 0;
    while i < rest.len() {
        let w = rest[i];
        if w == "-s" {
            size = rest.get(i + 1).copied();
            i += 2;
            continue;
        }
        if let Some(v) = w.strip_prefix("-s") {
            if !v.is_empty() {
                size = Some(v);
            }
            i += 1;
            continue;
        }
        if is_path_arg(w) {
            file = Some(w);
        }
        i += 1;
    }
    if let Some(f) = file {
        let effect = if size == Some("0") { ShellWriteEffect::Emptied } else { ShellWriteEffect::Rewritten };
        out.push(ShellWrite { path: f.to_string(), effect });
    }
}

/// `sed -i`'s own target files: the first non-flag argument is the SCRIPT
/// ('s/a/b/'), never a file - a glob character there is the script's own
/// regex syntax, not a filesystem glob, so only the arguments AFTER it are
/// ever read as paths. With fewer than two non-flag arguments (a script with
/// no file named, reading stdin, or a shape this cannot tell apart from
/// that), nothing is reported rather than guessed at.
fn collect_sed_i(rest: &[&str], out: &mut Vec<ShellWrite>) {
    let non_flags: Vec<&str> = rest.iter().copied().filter(|w| !w.starts_with('-')).collect();
    if non_flags.len() < 2 {
        return;
    }
    for w in &non_flags[1..] {
        if is_path_arg(w) {
            out.push(ShellWrite { path: (*w).to_string(), effect: ShellWriteEffect::Rewritten });
        }
    }
}

/// One `>`/`>>` redirection (`shell::Redirect`, recorded by the scanner with
/// whether a real producer runs in its segment). A bare `:` is the shell
/// no-op idiom for "truncate this file, run nothing", so it counts as no
/// producer at all - `: > f` and a bare `> f` both EMPTY the file, while any
/// real command in the segment (`echo x`, `printf ...`) REWRITES it, and `>>`
/// always rewrites regardless, since append can never simply empty a file.
fn collect_redirect(redirect: &crate::shell::Redirect, out: &mut Vec<ShellWrite>) {
    if !is_path_arg(&redirect.target) {
        return;
    }
    let effect =
        if redirect.append || redirect.producer { ShellWriteEffect::Rewritten } else { ShellWriteEffect::Emptied };
    out.push(ShellWrite { path: redirect.target.clone(), effect });
}

/// One segment's own verb-shaped writes (`rm`, `git rm`, `truncate -s 0`,
/// `sed -i`, `tee`, `mv`/`cp`'s own destination), read from its HEAD - so
/// `sudo rm f` and `/bin/rm f` are the same write as `rm f`. Its redirections
/// are read separately (`collect_redirect`), never `else`, so a segment like
/// `tee f > log` reports BOTH targets rather than one silently winning.
fn collect_segment_writes(head: &[String], out: &mut Vec<ShellWrite>) {
    let words: Vec<&str> = head.iter().map(String::as_str).collect();
    match words.as_slice() {
        ["rm", rest @ ..] => {
            for w in rest {
                if is_path_arg(w) {
                    out.push(ShellWrite { path: (*w).to_string(), effect: ShellWriteEffect::Removed });
                }
            }
        }
        ["git", "rm", rest @ ..] => {
            for w in rest {
                if is_path_arg(w) {
                    out.push(ShellWrite { path: (*w).to_string(), effect: ShellWriteEffect::Removed });
                }
            }
        }
        ["truncate", rest @ ..] => collect_truncate(rest, out),
        ["sed", rest @ ..] if rest.iter().any(|w| w.starts_with("-i") || w.starts_with("--in-place")) => {
            collect_sed_i(rest, out)
        }
        ["tee", rest @ ..] => {
            for w in rest {
                if is_path_arg(w) {
                    out.push(ShellWrite { path: (*w).to_string(), effect: ShellWriteEffect::Rewritten });
                }
            }
        }
        ["mv", rest @ ..] | ["cp", rest @ ..] => {
            if let Some(dest) = rest.iter().rev().find(|w| is_path_arg(w)) {
                out.push(ShellWrite { path: (*dest).to_string(), effect: ShellWriteEffect::Rewritten });
            }
        }
        _ => {}
    }
}

#[derive(Debug, Clone, Default)]
pub struct ServeInput {
    pub moments: Vec<Action>,
    pub targets: Vec<(TargetKind, String)>,
    /// Raw text of what actually happened (command, file path, or both) -
    /// never anything an item declared about itself. Source of the closeness
    /// key in `rank::closeness`.
    pub context: String,
    /// Which project this session is standing in, resolved once by
    /// `project::resolve_project` and carried here so `rank::select` can apply
    /// the same scoping session start already applies. `None` means the
    /// project could not be resolved, and then only global items are served -
    /// see `project::applies_to` for why that is the safe direction.
    pub project: Option<String>,
    /// The verbatim string last handed to `add_command` - `targets`/`context`
    /// hold this same text derived and mixed with other signals, which is right
    /// for ranking but not for a caller that needs the command as written.
    pub command: Option<String>,
    /// The verbatim string last handed to `add_file`, for the same reason
    /// `command` above exists.
    pub file: Option<String>,
    /// The project ROOT this session is standing in - the same filesystem
    /// root `serve/src/bin/serve.rs` already resolves once per hook call and
    /// threads into `absent_guard`'s own `root: Option<&Path>` parameters,
    /// carried here too so `rank::select`'s own anchor matching
    /// (`absent_guard::scoped_target_matches`) can tell a touched file
    /// INSIDE this project from one outside it. `None` exactly when no root
    /// was resolved (or none was ever supplied, e.g. a hand-built input) -
    /// `scoped_target_matches` then falls back to the old, root-blind
    /// comparison rather than guessing. See that function's own doc comment
    /// for the defect this exists to close.
    pub root: Option<PathBuf>,
    /// Whether `command` is a real shell command (`add_command`), not the bare
    /// name of a tool offered the same way (`add_tool_name`): only a shell call
    /// can be read-only in the sense of `read_only::is_read_only_call` (a Grep
    /// TOOL call is a search the owner measured to fire the rules anchored at
    /// the place it searches, and still does).
    pub shell_call: bool,
}

impl ServeInput {
    pub fn is_empty(&self) -> bool {
        self.moments.is_empty() && self.targets.is_empty()
    }

    fn push_context(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        if !self.context.is_empty() {
            self.context.push(' ');
        }
        self.context.push_str(text);
    }

    /// A command about to run: derives moments via `intent::from_command`,
    /// treats the command text itself as a possible `Target::Command` doel,
    /// and - see `paths_in_command`/`hosts_in_command` - every file and host
    /// the command NAMES as a doel of its own.
    pub fn add_command(&mut self, command: &str) {
        self.add_command_text(command, true);
    }

    /// The bare name of a tool (`Agent`, `Grep`, `Artifact`), offered the way a
    /// command is so that an anchor naming the tool reaches its call. Not a
    /// shell command: it is never read as a read-only call.
    pub fn add_tool_name(&mut self, name: &str) {
        self.add_command_text(name, false);
    }

    fn add_command_text(&mut self, command: &str, shell: bool) {
        if command.is_empty() {
            return;
        }
        self.command = Some(command.to_string());
        self.shell_call = shell;
        let moments = intent::from_command(command).into_iter().map(|s| s.action);
        if shell && crate::read_only::is_read_only_call(&crate::shell::segments(command, crate::shell::Dialect::Sh)) {
            // The command text only MENTIONS the action (`grep "git push" docs`):
            // nothing is pushed, published or installed by a call that only looks.
            self.moments.extend(moments.filter(|a| !crate::read_only::CHANGES_THINGS.contains(a)));
        } else {
            self.moments.extend(moments);
        }
        self.targets.push((TargetKind::Command, command.to_string()));
        for path in paths_in_command(command) {
            self.targets.push((TargetKind::Path, path));
        }
        for host in hosts_in_command(command) {
            self.targets.push((TargetKind::Host, host));
        }
        self.push_context(command);
    }

    /// A file about to be read or written: derives moments via
    /// `intent::from_path` and treats the path itself as a `Target::Path` doel.
    pub fn add_file(&mut self, path: &str) {
        if path.is_empty() {
            return;
        }
        self.file = Some(path.to_string());
        self.moments.extend(intent::from_path(path).into_iter().map(|s| s.action));
        self.targets.push((TargetKind::Path, path.to_string()));
        self.push_context(path);
    }

    /// An explicit target of any kind (symbol/route/host/project/...), for a
    /// human-driven `check`/`why` call that is not shaped like a real
    /// Claude Code hook payload.
    pub fn add_target(&mut self, kind: TargetKind, value: &str) {
        if value.is_empty() {
            return;
        }
        self.targets.push((kind, value.to_string()));
        self.push_context(value);
    }

    /// An explicit moment, named directly rather than derived from a command
    /// or path - lets a human ask "what fires on `publish`" without having to
    /// spell a command that would trigger it.
    pub fn add_moment(&mut self, action: Action) {
        if !self.moments.contains(&action) {
            self.moments.push(action);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_command_derives_its_own_moments_and_becomes_a_command_target() {
        let mut input = ServeInput::default();
        input.add_command("git push --force origin main");
        assert!(input.moments.contains(&Action::Push));
        assert!(input.targets.contains(&(TargetKind::Command, "git push --force origin main".to_string())));
        assert!(input.context.contains("push"));
        assert_eq!(
            input.command.as_deref(),
            Some("git push --force origin main"),
            "the verbatim command must be kept as written"
        );
    }

    #[test]
    fn a_file_derives_its_own_moments_and_becomes_a_path_target() {
        let mut input = ServeInput::default();
        input.add_file("app/.env");
        assert!(input.moments.contains(&Action::Credentials));
        assert!(input.targets.contains(&(TargetKind::Path, "app/.env".to_string())));
        assert_eq!(
            input.file.as_deref(),
            Some("app/.env"),
            "the verbatim path must be kept as written"
        );
    }

    /// THE READ-ONLY RULE, moment half: a call that only looks never pushes,
    /// installs or deploys, whatever its text mentions - but reading live data
    /// is still touching it.
    #[test]
    fn a_read_only_call_derives_no_moment_that_changes_things() {
        let mut input = ServeInput::default();
        input.add_command("grep -rn \"git push --force\" docs | head");
        assert!(!input.moments.contains(&Action::Push), "{:?}", input.moments);
        let mut input = ServeInput::default();
        input.add_command("git push --force origin main");
        assert!(input.moments.contains(&Action::Push));
        let mut input = ServeInput::default();
        input.add_command("echo x && git push origin main");
        assert!(input.moments.contains(&Action::Push), "a call that pushes is not read-only");
        let mut input = ServeInput::default();
        input.add_command("cat prod-orders.log");
        assert!(input.moments.contains(&Action::ProdData), "reading live data is still touching it: {:?}", input.moments);
    }

    #[test]
    fn an_empty_input_is_empty() {
        assert!(ServeInput::default().is_empty());
    }

    #[test]
    fn adding_the_same_moment_twice_is_not_duplicated() {
        let mut input = ServeInput::default();
        input.add_moment(Action::Publish);
        input.add_moment(Action::Publish);
        assert_eq!(input.moments.len(), 1);
    }

    // ------------------------------------------ files and hosts in a command

    /// The measured defect, in one line: the same file, once as a file and
    /// once inside a command, has to reach the same anchor.
    #[test]
    fn a_command_that_reads_a_file_names_that_file_as_a_target() {
        let paths = paths_in_command("grep -n Refusal thor2/model/src/gate.rs");
        assert_eq!(paths, vec!["thor2/model/src/gate.rs"]);

        let mut input = ServeInput::default();
        input.add_command("grep -n Refusal thor2/model/src/gate.rs");
        assert!(
            input.targets.contains(&(TargetKind::Path, "thor2/model/src/gate.rs".to_string())),
            "the file the command reads must be a Path doel: {:?}",
            input.targets
        );
        assert!(
            input.targets.iter().any(|(k, _)| *k == TargetKind::Command),
            "and the command itself must still be one"
        );
    }

    /// The exact case from the field report: a bare log filename, greped.
    #[test]
    fn a_bare_filename_with_an_extension_counts() {
        assert_eq!(paths_in_command("Select-String UP_DONE rebuild-prod.log"), vec!["rebuild-prod.log"]);
    }

    #[test]
    fn windows_and_unc_paths_are_normalised_not_dropped() {
        assert_eq!(paths_in_command("type C:\\Users\\dev\\thor2\\thor.db"), vec!["C:/Users/dev/thor2/thor.db"]);
        assert_eq!(paths_in_command("cat \\\\server\\share\\deploy.log"), vec!["//server/share/deploy.log"]);
    }

    #[test]
    fn a_flag_hands_over_its_value() {
        assert_eq!(paths_in_command("serve --db=/srv/thor/thor.db hook"), vec!["/srv/thor/thor.db"]);
    }

    #[test]
    fn quotes_and_trailing_punctuation_come_off() {
        assert_eq!(paths_in_command("cat 'docs/1.0/SETUP.md';"), vec!["docs/1.0/SETUP.md"]);
    }

    /// Not a widened trigger: a glob names no one file, and a flag is not a
    /// path. Either one turned into a doel would fire facts on commands that
    /// never touched the file they are about.
    #[test]
    fn globs_and_flags_are_not_paths() {
        assert!(paths_in_command("rm -rf target/*.rs").iter().all(|p| !p.contains('*')));
        assert_eq!(paths_in_command("cargo test --all-targets"), Vec::<String>::new());
    }

    #[test]
    fn a_url_yields_its_host() {
        assert_eq!(hosts_in_command("curl https://quote.example.com/orders?id=3"), vec!["quote.example.com"]);
        // `ssh admin@host` names a host too; the user half is not part of it.
        assert_eq!(hosts_in_command("ssh admin@files.example.com"), vec!["files.example.com"]);
    }

    #[test]
    fn a_bare_hostname_yields_a_host() {
        assert_eq!(hosts_in_command("nslookup shop.example.com"), vec!["shop.example.com"]);
    }

    /// The hard case, and the reason the source-extension list is consulted:
    /// `gate.rs` has exactly the shape of a hostname. Reading it as one would
    /// put facts about a server in front of somebody editing a file.
    #[test]
    fn a_filename_is_never_read_as_a_host() {
        for cmd in ["grep foo model/src/gate.rs", "cat notes.md", "sh deploy.sh", "node build.js"] {
            assert_eq!(hosts_in_command(cmd), Vec::<String>::new(), "{cmd} must name no host");
        }
    }

    #[test]
    fn a_command_naming_a_host_reaches_a_host_anchor() {
        let mut input = ServeInput::default();
        input.add_command("curl -I https://shop.example.com/");
        assert!(
            input.targets.contains(&(TargetKind::Host, "shop.example.com".to_string())),
            "a host anchor must be reachable from a command: {:?}",
            input.targets
        );
    }

    // ------------------------------------------------- shell_write_targets
    //
    // THE MEASURED HOLE: `rm file`, `truncate -s 0 file`, `sed -i ... file`,
    // `echo x > file`, `tee file`, `mv other file` and `cp other file` from
    // Bash walk straight past a rule anchored at that exact file, because
    // only Edit/Write ever counted as a write. Each case below is the one
    // real shape a Bash tool call takes for it.

    fn removed(path: &str) -> ShellWrite {
        ShellWrite { path: path.to_string(), effect: ShellWriteEffect::Removed }
    }
    fn emptied(path: &str) -> ShellWrite {
        ShellWrite { path: path.to_string(), effect: ShellWriteEffect::Emptied }
    }
    fn rewritten(path: &str) -> ShellWrite {
        ShellWrite { path: path.to_string(), effect: ShellWriteEffect::Rewritten }
    }

    #[test]
    fn rm_names_every_argument_as_removed() {
        assert_eq!(shell_write_targets("rm a b"), vec![removed("a"), removed("b")]);
    }

    #[test]
    fn rm_skips_its_own_flags() {
        assert_eq!(shell_write_targets("rm -rf dir"), vec![removed("dir")]);
    }

    #[test]
    fn truncate_s_zero_empties_the_file() {
        assert_eq!(shell_write_targets("truncate -s 0 f"), vec![emptied("f")]);
    }

    #[test]
    fn truncate_a_nonzero_size_rewrites_rather_than_empties() {
        assert_eq!(shell_write_targets("truncate -s 100 f"), vec![rewritten("f")]);
    }

    #[test]
    fn sed_in_place_rewrites_the_file_and_never_its_own_script() {
        assert_eq!(shell_write_targets("sed -i 's/a/b/' f"), vec![rewritten("f")]);
    }

    #[test]
    fn a_bare_redirect_with_a_producer_rewrites_the_file() {
        assert_eq!(shell_write_targets("echo x > f"), vec![rewritten("f")]);
    }

    #[test]
    fn an_append_redirect_always_rewrites_even_with_a_producer() {
        assert_eq!(shell_write_targets("cat a >> f"), vec![rewritten("f")]);
    }

    #[test]
    fn a_redirect_with_no_producer_empties_the_file() {
        assert_eq!(shell_write_targets("> f"), vec![emptied("f")]);
        assert_eq!(shell_write_targets(": > f"), vec![emptied("f")], "the shell no-op idiom names no producer either");
    }

    #[test]
    fn tee_rewrites_the_file_it_names() {
        assert_eq!(shell_write_targets("tee f"), vec![rewritten("f")]);
    }

    #[test]
    fn a_piped_tee_is_still_found_in_its_own_segment() {
        assert_eq!(shell_write_targets("cat notes.md | tee f"), vec![rewritten("f")]);
    }

    #[test]
    fn mv_names_only_the_destination_as_rewritten() {
        assert_eq!(shell_write_targets("mv a b"), vec![rewritten("b")]);
    }

    #[test]
    fn cp_names_only_the_destination_as_rewritten() {
        assert_eq!(shell_write_targets("cp a b"), vec![rewritten("b")]);
    }

    #[test]
    fn git_rm_is_removed_the_same_as_a_bare_rm() {
        assert_eq!(shell_write_targets("git rm f"), vec![removed("f")]);
    }

    #[test]
    fn a_quoted_path_with_a_space_is_read_whole() {
        assert_eq!(shell_write_targets("rm \"my file.txt\""), vec![removed("my file.txt")]);
    }

    #[test]
    fn a_glob_is_ignored_rather_than_guessed_at() {
        assert_eq!(
            shell_write_targets("rm *.log"),
            Vec::<ShellWrite>::new(),
            "a glob names nothing until the shell expands it"
        );
    }

    #[test]
    fn a_sh_c_wrapper_is_unwrapped_to_the_real_command() {
        assert_eq!(shell_write_targets("sh -c \"rm file.txt\""), shell_write_targets("rm file.txt"));
        assert_eq!(shell_write_targets("sh -c \"rm file.txt\""), vec![removed("file.txt")]);
    }

    #[test]
    fn a_bash_lc_wrapper_is_also_unwrapped() {
        assert_eq!(shell_write_targets("bash -lc \"rm file.txt\""), vec![removed("file.txt")]);
    }

    #[test]
    fn a_powershell_command_wrapper_is_unwrapped_even_if_the_inner_shape_is_unrecognised() {
        // Unwrapping succeeds; the inner text just is not one of the POSIX
        // verbs this function knows, which is an acceptable missed case, not
        // a wrong answer - see `shell_write_targets`'s own doc comment.
        assert_eq!(shell_write_targets("powershell -Command \"Remove-Item file.txt\""), Vec::<ShellWrite>::new());
    }

    #[test]
    fn a_plain_read_yields_nothing() {
        assert_eq!(shell_write_targets("cat f"), Vec::<ShellWrite>::new());
    }

    #[test]
    fn a_status_check_yields_nothing() {
        assert_eq!(shell_write_targets("git status"), Vec::<ShellWrite>::new());
    }

    // ------------------------------------- the same segments as every tooth
    //
    // The shapes the old tokenizer missed (measured against it: a newline was
    // a space, `$(...)` was never read, and a wrapper was unwrapped only at
    // the head of the line), and the ones that must stay data.

    /// sh form: a newline ends a command, so `rm` is the head of its own line.
    #[test]
    fn a_write_on_its_own_line_after_a_cd_is_found() {
        assert_eq!(shell_write_targets("cd x\nrm CLAUDE.md"), vec![removed("CLAUDE.md")]);
    }

    /// sh form: command substitution `$(...)` runs its command.
    #[test]
    fn a_write_inside_a_command_substitution_is_found() {
        assert_eq!(shell_write_targets("x=$(rm CLAUDE.md)"), vec![removed("CLAUDE.md")]);
    }

    /// sh form: `bash -lc '...'` after `&&`, not at the head of the line.
    #[test]
    fn a_wrapper_that_is_not_at_the_head_of_the_line_is_read() {
        assert_eq!(shell_write_targets("cd x && bash -lc 'rm CLAUDE.md'"), vec![removed("CLAUDE.md")]);
    }

    /// sh form: a heredoc body fed to `cat` is data, never a command.
    #[test]
    fn a_heredoc_body_is_never_a_write() {
        assert_eq!(shell_write_targets("cat > notes.md <<'EOF'\nrm CLAUDE.md\nEOF"), vec![rewritten("notes.md")]);
    }

    /// sh form: `ssh host 'cmd'` runs on another machine.
    #[test]
    fn a_write_sent_over_ssh_is_never_a_local_write() {
        assert_eq!(shell_write_targets("ssh nas 'rm /srv/CLAUDE.md'"), Vec::<ShellWrite>::new());
        assert_eq!(shell_write_targets("ssh nas bash -s <<'EOF'\nrm CLAUDE.md\nEOF"), Vec::<ShellWrite>::new());
    }

    /// sh form: a mention in an echo or a search pattern writes nothing.
    #[test]
    fn a_mention_of_a_write_is_not_a_write() {
        assert_eq!(shell_write_targets("echo 'rm CLAUDE.md'"), Vec::<ShellWrite>::new());
        assert_eq!(shell_write_targets("grep -n 'rm CLAUDE.md' notes.md"), Vec::<ShellWrite>::new());
    }

    /// sh form: `: > f` on its own line after a cd empties the file.
    #[test]
    fn an_emptying_redirect_on_its_own_line_is_found() {
        assert_eq!(shell_write_targets("cd x\n: > CLAUDE.md"), vec![emptied("CLAUDE.md")]);
    }

    /// sh form: `sudo rm` and `/bin/rm` are the same write as `rm`.
    #[test]
    fn a_write_under_sudo_or_by_full_path_is_found() {
        assert_eq!(shell_write_targets("sudo rm CLAUDE.md"), vec![removed("CLAUDE.md")]);
        assert_eq!(shell_write_targets("/bin/rm CLAUDE.md"), vec![removed("CLAUDE.md")]);
    }

    /// sh form: a file descriptor redirection names no file of its own, and
    /// the redirect target is never a word of the verb it follows.
    #[test]
    fn a_redirect_target_is_not_an_argument_of_the_verb() {
        assert_eq!(shell_write_targets("rm a 2>/dev/null"), vec![removed("a"), rewritten("/dev/null")]);
        assert_eq!(shell_write_targets("rm a 2>&1"), vec![removed("a")]);
    }

    /// Each write carries the folder its segment runs in: the cd chain, or a
    /// git call's own `-C`.
    #[test]
    fn each_write_carries_the_folder_of_its_segment() {
        let at = |c: &str| {
            shell_writes_in(&crate::shell::segments(c, crate::shell::Dialect::Sh))
                .into_iter()
                .map(|w| (w.write.path, w.dir.as_deref().map(str::to_string)))
                .collect::<Vec<_>>()
        };
        assert_eq!(at("cd x && rm other.md"), vec![("other.md".to_string(), Some("x".to_string()))]);
        assert_eq!(at("cd x\ncd y\nrm a"), vec![("a".to_string(), Some("x/y".to_string()))]);
        assert_eq!(at("git -C sub rm a"), vec![("a".to_string(), Some("sub".to_string()))]);
        assert_eq!(at("rm a"), vec![("a".to_string(), None)]);
    }

    /// A `cd` the shell refuses moves nothing (an operand too many, an
    /// option bash does not know, a `chdir` bash does not have): the write
    /// lands where the call already was, and is judged there.
    #[test]
    fn a_write_after_a_cd_the_shell_refuses_stays_where_it_was() {
        let at = |c: &str| {
            shell_writes_in(&crate::shell::segments(c, crate::shell::Dialect::Sh))
                .into_iter()
                .map(|w| (w.write.path, w.dir.as_deref().map(str::to_string)))
                .collect::<Vec<_>>()
        };
        for c in ["cd x extra; rm a", "cd -x x; rm a", "chdir x; rm a", "sudo cd x; rm a"] {
            assert_eq!(at(c), vec![("a".to_string(), None)], "{c}");
        }
        assert_eq!(at("cd -P x; rm a"), vec![("a".to_string(), Some("x".to_string()))]);
    }

    /// sh form: arithmetic is no heredoc. A `<<` in `(( ... ))` or `$[ ... ]`
    /// is a shift, so the write on the next line is read; a command inside
    /// arithmetic runs, so its write is read too.
    #[test]
    fn a_write_after_or_inside_arithmetic_is_read() {
        assert_eq!(shell_write_targets("(( y = 1 << 2 ))
rm CLAUDE.md"), vec![removed("CLAUDE.md")]);
        assert_eq!(shell_write_targets("echo $[1<<2]
rm CLAUDE.md"), vec![removed("CLAUDE.md")]);
        assert_eq!(shell_write_targets("for ((i=0; i<<1; i++)); do :; done
rm CLAUDE.md"), vec![removed("CLAUDE.md")]);
        assert_eq!(shell_write_targets("(( $(rm CLAUDE.md) ))"), vec![removed("CLAUDE.md")]);
    }
}
