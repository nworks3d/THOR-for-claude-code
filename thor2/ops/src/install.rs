//! Installs THOR's hooks into an agent's `settings.json`: SessionStart,
//! PreToolUse and UserPromptSubmit, each pointing at the same `serve hook`
//! command (see `serve/src/bin/serve.rs`'s `cmd_hook`, which branches on the
//! payload's own `hook_event_name`).
//!
//! Non-negotiables (all enforced structurally, not by convention - CONTRACT
//! R1/R7): a back-up is written before anything else touches the file; the
//! file must already be valid JSON or the whole run refuses (never silently
//! starts a fresh settings file over a file the agent could not parse); a
//! second run adds nothing that is already there; and any hook this tool did
//! not put there is never touched, moved, or removed - only appended past.

use serde_json::{json, Value};
use serve::respond;
use std::fs;
use std::path::{Path, PathBuf};
use model::store::{SystemActor, INSTALLER_ACTOR};
use thor_core::event_store::EventStore;

/// The command a reader runs to register a checkout, always naming the store
/// (`--db`) - the one definition, in `model::store` so `serve` and `mcp` can
/// print it too (neither can depend on this crate).
pub use model::store::register_command_for;

/// THOR's installer, as the actor its own writes carry - the only writer the
/// store lets touch a project record (`model::store::fence`).
fn installer() -> SystemActor {
    SystemActor::new(INSTALLER_ACTOR)
}

/// The per-user directory this installer puts a store in when it was not told
/// where to put one: `LOCALAPPDATA` / `XDG_DATA_HOME` / `HOME/.local/share`,
/// then `thor2`. Same resolution order as `serve::semantic_paths`, so a
/// machine ends up with one THOR directory rather than one per component.
///
/// Deliberately NOT next to the binaries. A store beside a `cargo build`
/// output lives in `target/`, and `cargo clean` is a command people run
/// without thinking twice about it - which would take the memory with it.
pub fn default_data_dir() -> Option<PathBuf> {
    let base = std::env::var("LOCALAPPDATA")
        .or_else(|_| std::env::var("XDG_DATA_HOME"))
        .map(PathBuf::from)
        .ok()
        .or_else(|| std::env::var("HOME").ok().map(|h| Path::new(&h).join(".local").join("share")))?;
    Some(base.join("thor2"))
}

/// The user's home directory: `USERPROFILE` on Windows, then `HOME`. The one
/// thing every default path below is relative to.
fn home_dir() -> Option<PathBuf> {
    std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME")).map(PathBuf::from).ok()
}

/// Claude Code's per-user settings file, where its `hooks` and `permissions`
/// live: `~/.claude/settings.json`.
///
/// WHY DEFAULTING THIS IS NOT THE "never guess a file I write to" violation it
/// looks like. That rule exists so this tool never rewrites the WRONG file.
/// This is not a guess: it is the single documented per-user location, the
/// same on every machine Claude Code runs on, and `install_hooks` only ever
/// appends to it and writes a `.bak` first. Requiring a newcomer to type a
/// path they have never needed to know was the actual reported blocker; a
/// well-known default that is printed before it is used removes it without
/// giving up the safety, which is the backup and the append-only writer, not
/// the absence of a default.
pub fn default_settings_path() -> Option<PathBuf> {
    home_dir().map(|h| h.join(".claude").join("settings.json"))
}

/// Claude Code's per-user MCP config: `~/.claude.json`. A tool server written
/// under its top-level `mcpServers` object is available in EVERY project, which
/// is what a global THOR install wants. `install_tool_server` preserves every
/// other key in that (large, stateful) file and backs it up first.
///
/// Defaulted for the same reason as `default_settings_path`, and it matters
/// more here: leaving the MCP target unset gave a newcomer a memory the agent
/// could READ but never WRITE to - a THOR that quietly does half its job. The
/// working default makes writing-back the normal outcome instead of the one
/// you had to know to ask for.
pub fn default_user_mcp_path() -> Option<PathBuf> {
    home_dir().map(|h| h.join(".claude.json"))
}

/// What happened to the store itself on this run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreOutcome {
    Created,
    AlreadyThere,
}

/// Make sure a store exists at `db`, creating an empty one if it does not.
///
/// WHY THE INSTALLER HAS TO DO THIS. `EventStore::open_existing` refuses a
/// missing file on purpose, and `doctor` uses it - so on a brand new machine
/// the documented "check it works before you install anything" step failed
/// with "no THOR store at ...", which reads like a broken program rather than
/// like a first run. Everything that WRITES creates the store as a side
/// effect, so the store used to appear halfway through the first session,
/// after the health check had already said no. Creating it here puts the
/// steps back in an order a person can follow.
///
/// An existing store is opened by nobody and touched by nothing: this returns
/// early, so a re-run can never disturb a real memory.
pub fn ensure_store(db: &Path) -> anyhow::Result<StoreOutcome> {
    if db.exists() {
        return Ok(StoreOutcome::AlreadyThere);
    }
    if let Some(parent) = db.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    // `new` is the constructor that creates the file and lays down the
    // schema; `open_existing` is the one that never creates. See both doc
    // comments in `core::event_store`.
    EventStore::new(db)?;
    Ok(StoreOutcome::Created)
}

/// What happened to the response-guard rulebook on this run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RulebookOutcome {
    Written,
    AlreadyThere,
}

#[derive(Debug, Clone)]
pub struct RulebookReport {
    pub outcome: RulebookOutcome,
    /// Resolved once here, via `serve::respond::default_rulebook_path`, so a
    /// caller never has to know that function exists just to print where the
    /// file went.
    pub path: PathBuf,
}

/// The rulebook this installer seeds when none exists yet: byte-identical to
/// the rules shipped at the repository root as
/// `guard-response-rulebook.example.json` (a plain-language TLDR rule, a
/// no-disclaimers rule, an ask-before-checking rule, a length rule, an
/// evidence rule, and an absolute-verification-claim rule) - baked in at compile time so a copied-out binary carries
/// it too, with no example file it has to find sitting next to it.
const RESPONSE_RULEBOOK_TEMPLATE: &str = include_str!("../../guard-response-rulebook.example.json");

/// How many reply rules the seeded rulebook carries, read from the file
/// itself so no text has to repeat the number.
pub fn response_rulebook_rule_count() -> usize {
    serde_json::from_str::<Value>(RESPONSE_RULEBOOK_TEMPLATE).ok().and_then(|v| v.as_array().map(Vec::len)).unwrap_or(0)
}

/// Give a store a working Response Guard from its first session, without
/// ever touching a rulebook that is already there.
///
/// THE GAP THIS CLOSES. `respond::default_rulebook_path` always resolves to
/// one fixed place beside the store, and `install` is the one place that
/// already knows `db` before anything else runs - so where to put this is
/// never in question here. Without this function, the `Stop` hook
/// `standard_hooks` wires in is live from the very first run, but reads a
/// file that is not there: `respond::block_reason` and `respond::guard_verdict`
/// both fail open on a missing rulebook BY DESIGN (a guard that watches
/// replies must never itself become the reason a reply cannot be given - see
/// respond.rs's own doc comment), so it runs on every reply and blocks
/// nothing, silently, until someone finds the example file in the
/// repository and copies it over by hand. Measured gap: that is exactly what
/// a new install did, with nothing anywhere saying so.
///
/// Checked on EVERY run, the same as `ensure_store` - never only on a store
/// this run just created. A store from before this function existed has
/// exactly the same missing file and deserves the same fix the next time
/// `install` runs against it, not only on a brand new one.
///
/// An existing file - the owner's own tuned rulebook, or one an earlier
/// install already seeded - is never opened, merged or backed up: there is
/// nothing to back up FROM when nothing is being overwritten. Same stance as
/// `ensure_store` toward an existing store: only ever write into an absence.
/// This is also deliberately a NEUTRAL default, not the owner's own live
/// rulebook: a new user is meant to end up with working rules, never with
/// somebody else's wording imposed without being asked - see the seeded note
/// `walk-through-the-answer-guard-once` in `working_contract` for the other
/// half of that.
pub fn seed_response_rulebook(db: &Path) -> anyhow::Result<RulebookReport> {
    let path = respond::default_rulebook_path(db);
    if path.exists() {
        return Ok(RulebookReport { outcome: RulebookOutcome::AlreadyThere, path });
    }
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    fs::write(&path, RESPONSE_RULEBOOK_TEMPLATE)?;
    Ok(RulebookReport { outcome: RulebookOutcome::Written, path })
}

/// What happened to the end-of-session evaluation command on this run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvalCommandOutcome {
    Written,
    AlreadyThere,
}

#[derive(Debug, Clone)]
pub struct EvalCommandReport {
    pub outcome: EvalCommandOutcome,
    pub path: PathBuf,
}

/// The generic end-of-session evaluation routine this installer ships, its
/// one placeholder still in it: baked in at compile time so a copied-out
/// binary carries it too, the same way `RESPONSE_RULEBOOK_TEMPLATE` does.
const EVAL_COMMAND_TEMPLATE: &str = include_str!("../../eval-command.example.md");

/// The command line the eval routine's own report tells a reader to run for
/// THOR's own health check: `doctor` is a separate program, not on the PATH,
/// so it is always invoked by its full path next to the other binaries.
///
/// Windows gets PowerShell's own call-operator form, because a quoted path
/// alone is a call EXPRESSION there, not a command. Every other platform gets a plain quoted path, which a
/// POSIX shell already runs as a command with no operator needed, and no
/// `.exe` suffix, which is a Windows-only convention.
///
/// `windows` is a parameter rather than this function reading `cfg!(windows)`
/// itself: `cfg!` bakes into whichever platform compiled the test binary, so a suite
/// built on one platform could otherwise never prove the other form was even
/// reachable. `render_eval_command` passes the real `cfg!(windows)` at its
/// own call site, so production behaviour is unchanged; only the test's
/// ability to ask for either branch on demand is new.
pub fn doctor_invocation(bin_dir: &Path, db: &Path, windows: bool) -> String {
    if windows {
        format!("& \"{}\\doctor.exe\" --db \"{}\"", bin_dir.display(), db.display())
    } else {
        format!("\"{}/doctor\" --db \"{}\"", bin_dir.display(), db.display())
    }
}

/// The `allowed-tools` entry that pre-approves running `doctor` from the eval
/// command's own frontmatter (`{{THOR_DOCTOR_ALLOW}}` in `eval-command.
/// example.md`) - the owner's decision, 2026-09-17: an evaluation command
/// pre-approves Bash only per command, never a bare `Bash` that would let any
/// shell command through unasked during an evaluation, including a wrong or
/// whispered one (see this file's own `EVAL_COMMAND_TEMPLATE` doc comment).
///
/// FORWARD SLASHES EVEN ON WINDOWS, unlike `doctor_invocation` above:
/// Claude Code reads a `Bash(...)` entry as a glob pattern matched against
/// the command text, not as a path handed to a shell, so a Windows backslash
/// there is an escape character, not a separator - `bin_dir.display()` on
/// Windows would otherwise put one directly into the pattern.
///
/// `windows` is a parameter for the identical reason `doctor_invocation`
/// takes one rather than reading `cfg!(windows)` itself - see that
/// function's own doc comment.
pub fn doctor_allow_invocation(bin_dir: &Path, windows: bool) -> String {
    let dir = bin_dir.display().to_string().replace('\\', "/");
    if windows {
        format!("Bash({dir}/doctor.exe:*)")
    } else {
        format!("Bash({dir}/doctor:*)")
    }
}

/// The same doctor call for a Bash tool (`{{THOR_DOCTOR_BASH}}` in
/// `eval-command.example.md`): the form `doctor_allow_invocation`'s
/// `Bash(...)` entry pre-approves, so it runs with no permission prompt.
/// The form of `doctor_invocation` is not pre-approved there (PowerShell's
/// call operator on Windows, a quoted path elsewhere), so this one has
/// forward slashes, no call operator and no quotes - the entry is a glob over
/// the command text and a leading quote would never match it. A path with
/// whitespace in it is quoted anyway: unquoted it would not run at all.
/// `windows` is a parameter for the same reason as in `doctor_invocation`.
pub fn doctor_bash_invocation(bin_dir: &Path, db: &Path, windows: bool) -> String {
    let suffix = if windows { ".exe" } else { "" };
    let program = format!("{}/doctor{suffix}", bin_dir.display().to_string().replace('\\', "/"));
    let store = db.display().to_string().replace('\\', "/");
    if program.contains(char::is_whitespace) || store.contains(char::is_whitespace) {
        format!("\"{program}\" --db \"{store}\"")
    } else {
        format!("{program} --db {store}")
    }
}

/// The fast form of `doctor_invocation` (`{{THOR_DOCTOR_EVAL}}` in
/// `eval-command.example.md`): `doctor --eval` prints only the lines the
/// evaluation routine asks for, in seconds instead of the full check's much
/// longer run, so the routine can have its reader wait for it.
pub fn doctor_eval_invocation(bin_dir: &Path, db: &Path, windows: bool) -> String {
    format!("{} --eval", doctor_invocation(bin_dir, db, windows))
}

/// The fast form of `doctor_bash_invocation` (`{{THOR_DOCTOR_EVAL_BASH}}`).
/// `doctor_allow_invocation`'s `Bash(<program>:*)` entry is a prefix match on
/// the command text, so it pre-approves this form with no entry of its own
/// (a test holds that).
pub fn doctor_eval_bash_invocation(bin_dir: &Path, db: &Path, windows: bool) -> String {
    format!("{} --eval", doctor_bash_invocation(bin_dir, db, windows))
}

/// The `serve` program the Stop hook's retest command names, next to the
/// other binaries.
fn serve_program(bin_dir: &Path) -> PathBuf {
    bin_dir.join(format!("serve{}", std::env::consts::EXE_SUFFIX))
}

/// Fill in the template's placeholders - `{{THOR_DOCTOR}}` (the doctor
/// invocation the report tells a reader to run), `{{THOR_DOCTOR_BASH}}` (the
/// same call as a Bash tool runs it), `{{THOR_DOCTOR_EVAL}}` and
/// `{{THOR_DOCTOR_EVAL_BASH}}` (the same two with `--eval`, the fast form the
/// routine tells a reader to wait for), `{{THOR_DOCTOR_ALLOW}}` (the
/// matching `allowed-tools` entry that pre-approves it) and
/// `{{THOR_RETEST_ALLOW}}` (the entry that pre-approves THOR's own retest
/// command, in the exact form `model::store::retest_command_with` prints) -
/// with this install run's own resolution for the current platform (see
/// `doctor_invocation`/`doctor_allow_invocation`).
fn render_eval_command(bin_dir: &Path, db: &Path) -> String {
    EVAL_COMMAND_TEMPLATE
        .replace("{{THOR_DOCTOR}}", &doctor_invocation(bin_dir, db, cfg!(windows)))
        .replace("{{THOR_DOCTOR_BASH}}", &doctor_bash_invocation(bin_dir, db, cfg!(windows)))
        .replace("{{THOR_DOCTOR_EVAL}}", &doctor_eval_invocation(bin_dir, db, cfg!(windows)))
        .replace("{{THOR_DOCTOR_EVAL_BASH}}", &doctor_eval_bash_invocation(bin_dir, db, cfg!(windows)))
        .replace("{{THOR_DOCTOR_ALLOW}}", &doctor_allow_invocation(bin_dir, cfg!(windows)))
        .replace("{{THOR_RETEST_ALLOW}}", &model::store::retest_allow_entry(&serve_program(bin_dir), Some(db)))
}

/// Claude Code's per-user commands folder: `~/.claude/commands`. Same base
/// directory as `default_settings_path` - one documented per-user location,
/// not a guess, so a file placed here is exactly where typing `/thor-eval`
/// looks.
///
/// Routed through `eval_command_path_from` rather than `home_dir` above
/// directly, so that pure rule can be called on its own, with no environment
/// to read - see that function's own doc comment for why this crate carries
/// the rule twice.
pub fn default_eval_command_path() -> Option<PathBuf> {
    eval_command_path_from(std::env::var("USERPROFILE").ok().as_deref(), std::env::var("HOME").ok().as_deref())
}

/// The pure resolution rule behind `default_eval_command_path` above: given
/// Claude Code's per-user home candidates exactly as `std::env::var` would
/// hand them over (`USERPROFILE` tried first, then `HOME` - `home_dir`'s own
/// order), the path `install` writes the evaluation routine to.
///
/// SPLIT OUT, PURE, AND DUPLICATED IN `serve::usefulness` (the identical
/// function, same name, same body) - the Stop hook's own evaluation debt
/// (`serve/src/bin/serve.rs`) has to name this exact file, but `ops` depends
/// on `serve`, never the other way round (a dependency the other direction
/// would be a cycle Cargo refuses outright), so `serve` cannot call into
/// `ops` to resolve it. Kept pure rather than reading the environment
/// itself for the same reason `serve::reentry`'s own `depth_from_env_value`
/// is split from its I/O wrapper: `std::env::set_var` is process-wide and
/// races across parallel test threads, so proving the two copies agree
/// (`ops/tests`) has to drive both with plain fixture strings, never by
/// mutating this test binary's own environment.
pub fn eval_command_path_from(userprofile: Option<&str>, home: Option<&str>) -> Option<PathBuf> {
    userprofile.or(home).map(|h| PathBuf::from(h).join(".claude").join("commands").join("thor-eval.md"))
}

/// How `settle_mode` came to its answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModeHow {
    /// The owner said so (`install --set-mode`).
    Told,
    /// A mode was written by an earlier run; this run leaves it alone.
    Kept,
    /// Nobody said anything: the default, `release`, on every build.
    Default,
    /// The user switched bug reporting off. The mode returned beside this is a
    /// placeholder and means nothing; nothing is offered and nothing is sent.
    Off,
}

/// Settle which mode the bug nudge runs in (`serve::thor_bugs`), and write it
/// beside the store at `db`. `told`: the owner's own choice, always written.
/// Otherwise a mode already on disk is kept (a second run never flips what
/// the owner set), else the default is `release` on EVERY build, a source
/// build included: only an explicit `install --set-mode test` makes the test
/// mode, so a friend who builds from source is a co-tester like anyone else.
pub fn settle_mode(db: &Path, told: Option<serve::bug_report::Mode>) -> anyhow::Result<(serve::bug_report::Mode, ModeHow)> {
    use serve::bug_report::Mode;
    if told.is_none() && serve::thor_bugs::bug_reports_off(db) {
        return Ok((Mode::Release, ModeHow::Off));
    }
    let (mode, how) = match (told, serve::thor_bugs::read_mode(db)) {
        (Some(mode), _) => (mode, ModeHow::Told),
        (None, Some(mode)) => return Ok((mode, ModeHow::Kept)),
        (None, None) => (Mode::Release, ModeHow::Default),
    };
    serve::thor_bugs::write_mode(db, mode)?;
    Ok((mode, how))
}

/// Write THOR's end-of-session evaluation routine to `path`, with its two
/// placeholders filled in from `bin_dir` and `db`.
///
/// THE GAP THIS CLOSES. THOR holds a turn open for a verdict on one served
/// note at a time (see the working-contract notes on judgement, and the
/// `Stop` hook this whole install wires up), but that debt still needs a way
/// to be settled in bulk, deliberately, at the end of a session - not one
/// item at a time forever. That routine used to exist only as the
/// maintainer's own private slash command: written for one person, in one
/// language, naming one machine's paths. A fresh install had no copy of it
/// and no way to write one, so every debt THOR ever accrues for a new user
/// had nowhere generic to be settled. This ships the same method, generic,
/// with this run's own paths filled in.
///
/// Written even with `--no-mcp`. A read-only memory cannot itself act on
/// anything the routine would ask it to judge, so there is nothing yet for
/// it to settle - but the file still belongs on disk from the first run, so
/// it is already there the day writes are turned on, instead of a second
/// install being needed just to fetch it.
///
/// An existing file - the owner's own edited copy, or one an earlier install
/// already wrote - is never opened, merged or overwritten: the same "only
/// ever write into an absence" stance as `seed_response_rulebook`.
pub fn seed_eval_command(path: &Path, bin_dir: &Path, db: &Path) -> anyhow::Result<EvalCommandReport> {
    if path.exists() {
        return Ok(EvalCommandReport { outcome: EvalCommandOutcome::AlreadyThere, path: path.to_path_buf() });
    }
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    fs::write(path, render_eval_command(bin_dir, db))?;
    Ok(EvalCommandReport { outcome: EvalCommandOutcome::Written, path: path.to_path_buf() })
}

/// One hook this installer knows how to place: which event fires it, an
/// optional matcher (Claude Code's PreToolUse groups carry one; SessionStart
/// and UserPromptSubmit do not), and the exact command line to run.
#[derive(Debug, Clone)]
pub struct HookSpec {
    pub event: &'static str,
    pub matcher: Option<&'static str>,
    pub command: String,
}

/// Start `serve --version` once, the way a hook would start it, and say in
/// plain words why not when it cannot be started. A program that is on disk but
/// blocked (Windows Smart App Control, an antivirus) makes every hook fail open:
/// Claude Code records an error and carries on, and THOR never speaks. `None`
/// when it ran and exited cleanly. Writes nothing.
pub fn launch_problem(serve_exe: &Path) -> Option<String> {
    let mut command = std::process::Command::new(serve_exe);
    command.arg("--version");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    match command.output() {
        Err(e) => Some(format!(
            "{} could not be started ({e}). Windows or your antivirus is probably blocking it (Smart App Control and \
             Code Integrity do this to programs they do not know), or the file is damaged. THOR does not change that \
             setting: allow the program in Windows Security, or build it again, then run this again.",
            serve_exe.display()
        )),
        Ok(out) if !out.status.success() => Some(format!(
            "{} started but exited with {} when asked for its version, so a hook that calls it would fail too. Build it \
             again, then run this again.",
            serve_exe.display(),
            out.status
        )),
        Ok(_) => None,
    }
}

/// The four hooks the CONTRACT asks for: session start, before a tool call,
/// per prompt, and at the end of a turn (Stop) - all four calling the same
/// `serve hook --db <db>` command,
/// exactly like `serve/src/bin/serve.rs`'s own `hook` subcommand expects
/// (it tells the events apart by the JSON payload's `hook_event_name`, not by
/// which command line ran it).
///
/// The `--db` goes BEFORE the subcommand, because that is where `serve`'s CLI
/// puts it (`serve --db <DB> hook`, a global option on the parser, not on the
/// subcommand). This file had it the other way round until 2026-08-03 and was
/// caught the first time anyone actually RAN the command it writes: every hook
/// would have exited with "unexpected argument '--db'". Hooks fail open by
/// design, so nothing would have complained - the memory would simply never
/// have spoken again, which is this project's own worst failure class.
pub fn standard_hooks(serve_exe: &str, db: &str) -> Vec<HookSpec> {
    let command = format!("\"{serve_exe}\" --db \"{db}\" hook");
    vec![
        HookSpec { event: "SessionStart", matcher: None, command: command.clone() },
        HookSpec { event: "PreToolUse", matcher: Some("*"), command: command.clone() },
        HookSpec { event: "UserPromptSubmit", matcher: None, command: command.clone() },
        // Surface 5, the Response Guard. The `hook` command branches on the
        // payload's own `hook_event_name`, so the Stop hook runs the SAME
        // command - it is the payload, not the command line, that tells them
        // apart. Leaving this one out is exactly the regression that let a
        // whole session of untidy replies through (see serve::respond).
        HookSpec { event: "Stop", matcher: None, command },
    ]
}

/// What happened to one hook on this run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookOutcome {
    Added,
    AlreadyPresent,
    /// An older hook of ours was pointing at a binary that is no longer on
    /// disk, and this run repointed it. See `stale_thor_command`.
    Replaced,
}

/// The executable a hook command calls, when the command has the shape this
/// installer writes: `"<exe>" --db "<db>" hook`.
fn exe_in_command(command: &str) -> Option<&str> {
    let rest = command.strip_prefix('"')?;
    let end = rest.find('"')?;
    Some(&rest[..end])
}

/// True iff `group` holds a command that calls a binary with the same FILE
/// NAME as `exe`, at a path that no longer exists.
///
/// THE DEFECT THIS CLOSES. Move the binaries and run this again, and the old
/// hooks stayed: the skip check compares the whole command line, so a
/// different path read as a different hook and the installer simply appended
/// a second one. The stale entry then calls a binary that is not there, and a
/// hook that cannot run FAILS OPEN - the agent carries on and the memory
/// never speaks again, with nothing reporting it. There is no uninstall
/// either, so nothing else would ever have taken it out.
///
/// Deliberately narrow on two counts. Only a binary with the SAME file name
/// is considered, so nobody else's hook is ever touched. And only one that is
/// GONE: a hook pointing at a binary that still exists is left exactly where
/// it is, because somebody running two stores on purpose must not silently
/// lose one of them.
fn stale_thor_command(group: &Value, exe: &str) -> Option<String> {
    let ours = Path::new(exe).file_name()?;
    for hook in group.get("hooks")?.as_array()? {
        if hook.get("type").and_then(Value::as_str) != Some("command") {
            continue;
        }
        let command = hook.get("command").and_then(Value::as_str)?;
        let Some(their_exe) = exe_in_command(command) else { continue };
        if Path::new(their_exe).file_name() == Some(ours) && !Path::new(their_exe).exists() {
            return Some(command.to_string());
        }
    }
    None
}

#[derive(Debug, Clone)]
pub struct InstallReport {
    pub results: Vec<(String, HookOutcome)>,
    pub backup_path: Option<std::path::PathBuf>,
    /// The commands this run repointed, each one an older hook of ours that
    /// was calling a binary no longer on disk. Reported so a move never
    /// happens silently.
    pub replaced: Vec<String>,
}

/// True iff `group` (one element of `hooks.<event>`) already contains a
/// `{"type":"command","command": command}` entry, regardless of its matcher.
/// Matching on the command line is deliberate: it is the one field that
/// identifies "this is THOR's hook" without also depending on how a matcher
/// happened to be phrased.
///
/// "The same command" means the same program and the same arguments in ANY
/// spelling of the paths: see `same_spelling`. Until 2026-10-08 this was a
/// byte comparison, so running the installer with `C:/x/thor.db` over hooks
/// written as `C:\x\thor.db` appended a duplicate of every hook.
fn group_has_command(group: &Value, command: &str) -> bool {
    group
        .get("hooks")
        .and_then(Value::as_array)
        .map(|hooks| {
            hooks.iter().any(|h| {
                h.get("type").and_then(Value::as_str) == Some("command")
                    && h.get("command").and_then(Value::as_str).is_some_and(|c| same_spelling(c, command))
            })
        })
        .unwrap_or(false)
}

/// One word of a command line (or one argument) with its path spelling
/// flattened: `\` becomes `/`, a Windows `\\?\` prefix goes, and on Windows
/// the case goes too (its file system ignores case; other systems do not).
pub(crate) fn flatten_word(word: &str) -> String {
    let flat = word.replace('\\', "/");
    let flat = flat.strip_prefix("//?/").unwrap_or(&flat);
    if cfg!(windows) {
        flat.to_lowercase()
    } else {
        flat.to_string()
    }
}

/// Split a command line into words, honouring double quotes only, so a quoted
/// and an unquoted spelling of the same path reduce to the same word. A
/// backslash is NOT an escape here: on Windows it is the path separator.
pub(crate) fn command_words(command: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut in_word = false;
    let mut quoted = false;
    for ch in command.chars() {
        match ch {
            '"' => {
                quoted = !quoted;
                in_word = true;
            }
            c if c.is_whitespace() && !quoted => {
                if in_word {
                    words.push(flatten_word(&current));
                    current.clear();
                    in_word = false;
                }
            }
            c => {
                current.push(c);
                in_word = true;
            }
        }
    }
    if in_word {
        words.push(flatten_word(&current));
    }
    words
}

/// True iff two command lines call the same program with the same arguments,
/// ignoring only path spelling: separators, case on Windows, and quoting. A
/// different program, a different store path or a different flag is still a
/// different command; nothing here compares more loosely than that.
pub fn same_spelling(a: &str, b: &str) -> bool {
    a == b || command_words(a) == command_words(b)
}

fn new_group(spec: &HookSpec) -> Value {
    let mut group = json!({
        "hooks": [ { "type": "command", "command": spec.command } ]
    });
    if let Some(matcher) = spec.matcher {
        group["matcher"] = json!(matcher);
    }
    group
}

/// Save `raw` (the file as it is NOW) to `<path>.bak`, but only when no backup
/// exists yet. The first backup is the file as the user had it before THOR, and
/// the "Remove THOR completely" steps rely on it: a later write (a second
/// install, `--set-backup`, a replaced tool-server entry) must never replace
/// it with a file THOR has already touched. Returns the path of the backup.
pub fn backup_once(path: &Path, raw: &str) -> std::io::Result<std::path::PathBuf> {
    let backup = path.with_extension(match path.extension().and_then(|e| e.to_str()) {
        Some(ext) => format!("{ext}.bak"),
        None => "bak".to_string(),
    });
    if !backup.exists() {
        fs::write(&backup, raw)?;
    }
    Ok(backup)
}

/// Install `specs` into the settings JSON at `path`.
///
/// - Missing file: starts from `{}` (a fresh install has nothing to preserve).
/// - Existing file that is not valid JSON, or not a JSON object, or whose
///   `hooks` key (or one `hooks.<event>` key) is not the shape this tool
///   expects: refused with a reason. Nothing is written.
/// - Otherwise: the file is backed up to `<path>.bak` first (copied verbatim,
///   before any parse result is acted on; an existing `.bak` is kept, see
///   `backup_once`), then for each spec, its event's
///   array gets exactly one new group appended IF no existing group in that
///   array already carries the same command - every other group (anyone
///   else's hook, or THOR's own from an earlier install) is left byte-for-
///   byte as it was.
pub fn install_hooks(path: &Path, specs: &[HookSpec]) -> anyhow::Result<InstallReport> {
    let existed = path.exists();
    let raw = if existed { fs::read_to_string(path)? } else { "{}".to_string() };

    let mut root: Value = serde_json::from_str(&raw).map_err(|e| {
        anyhow::anyhow!(
            "{} is not valid JSON ({e}) - refusing to touch it; fix the JSON first, or point --settings at a different file",
            path.display()
        )
    })?;
    anyhow::ensure!(
        root.is_object(),
        "{} does not contain a JSON object at the top level - refusing to touch it",
        path.display()
    );

    if root.get("hooks").is_some() {
        anyhow::ensure!(
            root["hooks"].is_object(),
            "{}'s \"hooks\" key is not a JSON object - refusing to touch a shape this tool does not recognise",
            path.display()
        );
    } else {
        root["hooks"] = json!({});
    }

    for spec in specs {
        if let Some(existing) = root["hooks"].get(spec.event) {
            anyhow::ensure!(
                existing.is_array(),
                "{}'s \"hooks.{}\" key is not a JSON array - refusing to touch a shape this tool does not recognise",
                path.display(),
                spec.event
            );
        }
    }

    let backup_path = if existed {
        Some(backup_once(path, &raw)?)
    } else {
        None
    };

    let mut results = Vec::new();
    let mut replaced = Vec::new();
    for spec in specs {
        let array = root["hooks"]
            .as_object_mut()
            .unwrap()
            .entry(spec.event)
            .or_insert_with(|| json!([]));
        let array = array.as_array_mut().expect("checked above: this key is an array");

        if array.iter().any(|group| group_has_command(group, &spec.command)) {
            results.push((spec.event.to_string(), HookOutcome::AlreadyPresent));
            continue;
        }

        // Before appending, look for one of OURS that has gone stale: same
        // binary name, path that no longer exists. Repointing it beats
        // appending a second hook beside a dead one, which is what used to
        // happen after a move (see `stale_thor_command`).
        let ours = exe_in_command(&spec.command).unwrap_or_default().to_string();
        let mut repointed = None;
        for group in array.iter_mut() {
            let Some(old) = stale_thor_command(group, &ours) else { continue };
            if let Some(hooks) = group.get_mut("hooks").and_then(Value::as_array_mut) {
                for hook in hooks.iter_mut() {
                    if hook.get("command").and_then(Value::as_str) == Some(old.as_str()) {
                        hook["command"] = json!(spec.command);
                    }
                }
            }
            repointed = Some(old);
            break;
        }

        match repointed {
            Some(old) => {
                replaced.push(old);
                results.push((spec.event.to_string(), HookOutcome::Replaced));
            }
            None => {
                array.push(new_group(spec));
                results.push((spec.event.to_string(), HookOutcome::Added));
            }
        }
    }

    let pretty = serde_json::to_string_pretty(&root)?;
    fs::write(path, pretty + "\n")?;

    Ok(InstallReport { results, backup_path, replaced })
}

/// The notes a brand new memory starts with, and what happened to each.
#[derive(Debug, Clone)]
pub struct SeededItem {
    pub id: String,
    pub stored: bool,
    /// The gate's own words, when it refused this one.
    pub refusal: Option<String>,
}

/// The starting notes: how to write something down so it comes back.
///
/// WHY THESE LIVE IN THE MEMORY AND NOT IN A PAGE. An agent that stores notes
/// in a shape that never fires produces a memory which fails silently, and
/// neither side notices for weeks. A documentation page fixes that only for
/// someone who reads it, and a page can be skipped; what the memory hands over
/// at the start of every conversation cannot.
///
/// WHY THESE AND NOT MORE. The set started as four notes; it is 22 now (the
/// test `working_contract().len() == 22` below pins the count). The original
/// reasoning still holds for what was added: two of the four things a new writer
/// gets wrong are already caught by the gate, which refuses a rule with no
/// binding and a rule with no falsifier and says exactly what to fix. Seeding
/// those would be telling someone what they are about to be told anyway. The
/// four first notes are the ones nothing can catch: a wrong anchor, a second
/// copy, routing around a refusal, and expecting words alone to forbid
/// something. Each fails quietly.
///
/// They are ordinary notes. Unpin one and it stops arriving; rewrite it in your
/// own words; throw it out. Nothing here treats them as special afterwards.
pub fn working_contract() -> Vec<model::item::Item> {
    let rule = |id: &str, text: &str, falsifier: &str| model::item::Item {
        id: id.to_string(),
        kind: model::item::Kind::Rule,
        text: text.to_string(),
        bindings: vec![model::item::Binding::Always],
        severity: None,
        project: None,
        tags: vec!["working-contract".to_string()],
        expires: None,
        key: None,
        falsifier: Some(falsifier.to_string()),
        check: None,
    };

    vec![
        rule(
            "anchor-what-the-fact-is-about",
            "Anchor a fact to the file or command it is really about, never to a path that merely \
             appears in the sentence. An anchor matching nothing fires nowhere, and nothing says \
             so: the fact is silent from the day it was written.",
            "A fact whose anchor names a path that is not there turns out to be served anyway.",
        ),
        rule(
            "correct-instead-of-duplicating",
            "Search before storing. When something changed, revise the item that already says it \
             rather than storing a second copy. Two items saying one thing take two of the few \
             places a block has, and the older one goes on being served.",
            "Storing a second copy of an existing fact turns out to serve a reader better than \
             revising the original.",
        ),
        rule(
            "a-new-project-is-one-command",
            "Giving a folder its own memory is ONE command, run FROM that folder: `install --db \
             <your store> --project <name>` for code, or `install --db <your store> --topic` for \
             a plain folder of notes and files (no git). It marks the folder and registers it. \
             The owner names a project - never you.",
            "A folder set up with that one command turns out to be missing its marker, its \
             registration, its code reading or its refresh on commit.",
        ),
        rule(
            "two-lanes-work-and-life",
            "Two memories live here. Anything about the owner's own life - a recipe, a book, \
             a training log, an expense - goes to the library with `shelve`, never \
             `remember`. The work memory is for code and projects.",
            "A recipe stored with remember turns out to be found as easily as one filed \
             with shelve.",
        ),
        rule(
            "never-open-a-place-the-owner-did-not-name",
            "Never invent a place to file something - not a scope, not a shelf, not a register \
             key. Put it where something already lives, and if honestly nothing fits, ASK the \
             owner what the new one should be called and use his word. A name you thought of \
             is a name he will never look under.",
            "An agent opens a scope or a shelf under a name of its own and the owner finds it \
             again later without being told where it went.",
        ),
        rule(
            "one-entry-is-one-thing",
            "One entry is ONE thing. Given a schema AND an injury AND the equipment AND a \
             measurement, file them as separate entries with labels, not as one block: an \
             entry holding five things is found by one of them and corrected by none.",
            "A single entry holding several subjects turns out to be as findable and as \
             correctable as the same content split into one entry per subject.",
        ),
        rule(
            "a-refusal-is-the-gate-working",
            "A refusal names the exact reason and what to do instead, and nothing is written when \
             it fires. Fix what it names. Never reword a fact just to get past it and never report \
             it as a bug: it is the one moment a bad entry can still be stopped.",
            "A refusal is found that names no reason and no fix, or that fires on an entry with \
             nothing wrong with it.",
        ),
        rule(
            "words-inform-a-proof-forbids",
            "A rule backed by words alone can inform, never forbid. Only a check that runs and \
             holds may refuse, and THOR tries every check when its rule is written: one that bites \
             is kept, one that cannot is dropped and the rule stays a note. Most rules carry none, \
             by design.",
            "A rule carrying no runnable check is found blocking a write.",
        ),
        // The fifth, and the only one here that is not about WRITING a fact.
        // It earned its place from the field, 2026-08-09: a session told to
        // report literally what it got, including when nothing happened, did
        // markedly better work than one left to summarise. Everything else in
        // this contract keeps the memory honest; this one keeps the report
        // honest, and a memory read through a flattering report is no better
        // than a wrong one.
        rule(
            "say-what-actually-happened",
            "Say what actually happened, including when nothing did. A step that fired nothing, a \
             check that found nothing, a number you could not get: that is a result and it belongs \
             in the report. Leaving it out reads as success, and the reader cannot tell the \
             difference.",
            "A report omits a step that produced nothing and the reader turns out not to be misled \
             by the omission.",
        ),
        // The sixth, added 2026-08-09 and rewritten for the two tracks (step 8):
        // the one a newcomer meets as a REFUSAL before they meet it as advice.
        // Every rule is tried when it is written, so the first serious rule
        // someone writes may come back refused with the check to hang - and
        // this says, before that happens, that it is the trial working.
        rule(
            "answer-whether-a-rule-can-refuse",
            "Every rule is tried when written. A check that bites keeps it a RULE; with no \
             working tooth it is a NOTE; REFUSED names the check to hang. Name a check's literal \
             in the rule's own text (U+2014 for a glyph). A no-literal tag answers nothing.",
            "A rule is stored on the rule track with a check that THOR never tried.",
        ),
        // Eleven more below, all bound Always like the ten above. Not about
        // WRITING a fact this time - these are the generic honesty,
        // agent-spawning and memory-hygiene lessons an agent needs on ANY
        // project, picked from the owner's own standing rules and rewritten
        // clean of his language, his business and his machines: nothing
        // here holds only because of who is running it or what it is
        // running on.
        rule(
            "never-fabricate-a-measured-value",
            "Never invent or fabricate a measured or tuned value - a benchmark, a timing, a \
             count. Flag it as unknown instead of writing a plausible guess down as if it were \
             real.",
            "A fabricated value sits in a report indistinguishable from a real one, and nothing \
             in the text marks it as guessed.",
        ),
        rule(
            "count-coverage-never-claim-it",
            "Never claim something is covered everywhere on the strength of memory. Count it \
             with something that walks the real source, and state the number. A remembered \
             impression is not a count.",
            "A coverage claim made from memory matches an exact count of the real source \
             closely enough that counting stops being worth the trouble.",
        ),
        rule(
            "verify-against-the-real-source",
            "Verify a requested check against the real file and line, the commit, or a freshly \
             fetched source, never a summary, a memory, or an assumption. Source unreadable: \
             say 'not checked', no verdict.",
            "A claim verified only against a summary or a memory turns out to match the real \
             source closely enough that the distinction stops mattering.",
        ),
        rule(
            "never-verify-with-the-changed-mechanism",
            "Never verify a change using the very mechanism the change itself touched - the \
             measurement would confirm itself. Verify on an independent path, or have someone \
             else read it against the source.",
            "A measurement taken through the changed mechanism reproduces independently often \
             enough that taking a second, independent path stops being worth it.",
        ),
        rule(
            "name-a-model-on-every-agent",
            "Split independent work across parallel agents, and name an explicit, \
             cheapest-fitting model on each one rather than letting it inherit the session's \
             model. Escalate only after a cheap model fails a few tries.",
            "An agent spawned with no explicit model turns out to run on the cheapest fit \
             anyway, rather than on whatever the session happens to be using.",
        ),
        rule(
            "mechanical-brief-forbids-subagents-and-review",
            "A brief for mechanical, well-specified work should say so and explicitly forbid \
             spawning sub-agents or running a self-review pass - fan-out and review overhead \
             dominate on work that needs no judgement.",
            "A mechanical brief that allows sub-agents or a self-review pass turns out no \
             slower and no noisier than one that forbids them.",
        ),
        rule(
            "never-block-a-turn-on-a-notification",
            "Never end a turn stuck in a wait loop for a background task's own notification, as \
             the top-level session or as a sub-agent. Finish the turn instead; the notification \
             arrives on its own, later.",
            "A turn spent polling or sleeping for a background notification finishes no later \
             than one that ended and let the notification arrive by itself.",
        ),
        rule(
            "mid-task-message-does-not-redirect-an-agent",
            "A message sent to a sub-agent already mid-task does not redirect it - it keeps \
             working from its original brief. When the scope changes, start a new agent with \
             the new brief instead.",
            "A mid-task message changes what a running sub-agent is already doing, rather than \
             being picked up only once it starts a fresh turn.",
        ),
        rule(
            "check-literal-matches-the-files-language",
            "A Contains or Absent check's literal must appear in the target file exactly as \
             that file really writes it - its own language and syntax, never a translation or \
             paraphrase of what it says.",
            "A check literal written in a different language or wording from the target file \
             still matches, because the file happens to say both.",
        ),
        rule(
            "reason-every-fix-for-a-new-user",
            "Reason every fix to this tool for whoever installs it next, not only for a store \
             built up over months: a fresh install carries none of that history. Seed the fix, \
             or build it into the tool itself.",
            "A fix lands that only works because an existing store already holds something a \
             fresh install never gets, and nobody asked whether it would.",
        ),
        rule(
            "a-served-fact-is-a-constraint-not-a-command",
            "A fact served into a blank session or sub-agent is read with no context to tell \
             FYI from an order. Phrase it as a constraint on what already applies, never as an \
             imperative telling the reader to act.",
            "A fact phrased as an imperative ('run X', 'delete Y') is served into a blank \
             session and read back as background, never mistaken for an instruction to carry \
             out.",
        ),
        // A twenty-second, added 2026-09-07, and the only one here that
        // clears itself. `install` can now seed a WORKING rulebook (see
        // `seed_response_rulebook`) so the Response Guard never sits idle on
        // a fresh store, but the content it seeds is a neutral example, not
        // a choice anyone actually made - imposing the owner's own tuned
        // wording on somebody else's memory would be exactly the uninvited
        // write this whole contract argues against. This is the other half:
        // ask once, in the owner's own first session, then get out of the
        // way for good.
        rule(
            model::store::SETUP_NOTE_ID,
            "Setup is not finished. In your first reply, `lookup first-session-questions` is the \
             one place to read: the exact calls, paths and what to say. Apply the defaults at once \
             (store 'owner-setup-answers', retract this note, show him the list). Nothing waits.",
            "This note is still being served at session start after the owner has already \
             answered these questions and the answers are applied.",
        ),
    ]
}

/// The workflow starter pack: the coding-workflow habits that, in a store
/// built up over months of real use, were marked useful again and again (or
/// served thousands of times) and that hold for ANY developer on ANY project.
/// It exists so that someone who installs THOR with one sentence to their
/// agent starts with the working experience of the person who built it,
/// without having to learn it the same hard way.
///
/// WHAT IS IN HERE AND WHAT IS NOT. Everything below was rewritten generic:
/// no names, hosts, machines, clients, languages or paths of the owner it was
/// learnt from. A habit that only holds for one person's setup (a preferred
/// reply length, a commit identity, a NAS, a "no AI attribution" policy) is
/// NOT here, and neither is a habit THOR already ships by another route (the
/// post-compaction reminder, the honesty notes in `working_contract`).
///
/// BINDINGS. None of these is pinned: a pinned note is read at every session
/// start, and the session-start block is where THOR's token cost lives. Each
/// is bound to the moment or the command it is about instead (commit, push,
/// publish, delete, deploy, install), so it arrives right before the step it
/// is about, once per context.
///
/// HELPER-AGENT MODE IS NOT IN THE PACK. Its two notes (`helper_mode_notes`)
/// are off in a new memory (the owner's decision, 2026-10-09: a helper costs a
/// fixed start-up context and the user cannot steer it halfway, so it is not
/// for everyone) and `install --set-helpers on` stores them.
///
/// SEVERITY. None carries one, like the notes in `working_contract`. A note
/// called costly or irreversible with no check is listed by doctor as
/// toothless and is later asked about, one at a time, with the turn held: a
/// new user would meet that nag for rules they never chose. Without a weight
/// the four notes at one moment are equals, and all of them fit one block
/// (see `a_moments_starter_notes_fit_one_block`).
///
/// TEETH. Two notes carry a check, and only on a Windows install (the owner's
/// goal: an agent never flashes a console window, for any user):
/// `windowless-processes-on-windows` (`requires` on `Start-Process`, answered by
/// `-WindowStyle`, `-NoNewWindow`, `ms-settings:`, `windowsdefender:`,
/// `control.exe`, `.cpl`, `http://`, `https://` or `file:///`) and
/// `scheduled-task-not-interactive` (`forbidden` `/it` on `schtasks /create`).
/// Replayed through the real guard over the same 145,976 recorded calls
/// (2026-10-09): the first refused 4 calls once `file:///` was an answer - three
/// were probes of the rule itself (two with a program that does not exist, one
/// with a stand-in function), none a browser or control panel the user wanted -
/// and the second refused none. The match is on words, so `/it` also refuses a
/// forward-slash path segment `/it/`, and a `$p = Start-Process ...` assignment
/// is not seen by the guard at all (a miss, never a false block). Every other
/// note carries no check, on evidence. Seven candidate checks were
/// replayed through the real guard over 145,976 recorded tool calls
/// (2026-10-09): a ban on --no-verify refused 11 legitimate commits in
/// scratch trees; "names a model" refused 9 agent calls, "says subagent"
/// refused 1,052 and "names a working folder or says read-only" refused 982
/// (a brief can be sound in its own words); "start a process hidden" refused
/// 9 (opening a browser, a control panel); a ban on staging .env refused 3
/// exclusion pathspecs and
/// is a deliberate practice in some private repositories; a force-push ban
/// refused nothing, but it would also refuse the one legitimate history
/// rewrite a solo project makes. A new user must never get a false block
/// from a note they did not write, so every one of these stays a note.
///
/// They are ordinary notes. Retract one, or rewrite it, and nothing else
/// notices.
pub fn workflow_starter_pack() -> Vec<model::item::Item> {
    workflow_starter_pack_for(cfg!(windows))
}

/// The starter pack for a platform: `windows` decides whether the two console-flash
/// notes carry their refusing check.
pub fn workflow_starter_pack_for(windows: bool) -> Vec<model::item::Item> {
    use intent::Action;
    use model::item::{Binding, Check, TargetKind};

    // The check rides only on a Windows install; elsewhere the note stays a note.
    let with_check = |mut item: model::item::Item, check: Check| {
        item.check = windows.then_some(check);
        item
    };

    let moment = |a: Action| Binding::Moment(a);
    let command = |c: &str| Binding::Target { kind: TargetKind::Command, value: c.to_string() };
    let note = starter_note;

    vec![
        // COMMIT (four), then PUSH (four): a place holds at most four notes
        // (`model::item::MAX_ITEMS`) and the gate refuses a fifth that could
        // never reach a block, so each moment is kept at four. The wording of
        // the "only when asked" note still covers committing: it is shown at
        // the push, and the agent's own defaults already ask before a commit.
        note(
            "changelog-with-every-user-visible-change",
            "If the project keeps a CHANGELOG, add a line for every user-visible change in the same \
             commit, and bump the version when releases are versioned. Create a changelog only if the owner said yes \
             to first-session item 12; else keep what exists. Re-read the README for claims made untrue.",
            "A user-visible change lands with no changelog line in a project that keeps one, and the \
             next release notes still come out complete.",
            vec![moment(Action::Commit)],
        ),
        note(
            "run-the-tests-before-saying-done",
            "Before you call work done, or commit or push it, run the project's own tests or build and \
             quote the real result line. Failing, skipped or never run: say exactly that. Never skip \
             hooks with --no-verify: fix what the hook found. No red tests or stray TODO left at a push.",
            "A change pushed without a test or build run turns out to be as safe as one pushed after \
             a run.",
            vec![moment(Action::Commit), moment(Action::Push)],
        ),
        note(
            "never-commit-a-secret",
            "Never commit a secret: key, token, password or .env file. Real values live in a \
             gitignored file, with a tracked template such as .env.example beside it. Look at what is \
             staged before every commit. A secret that was committed must be rotated: history keeps it.",
            "A secret that reached a commit stays harmless once a later commit deletes the file.",
            vec![moment(Action::Commit), moment(Action::Credentials)],
        ),
        note(
            "commit-push-deploy-only-when-asked",
            "Commit, push, tag, release and deploy only when the user says so in this session; an \
             earlier yes does not carry over, and a brief that quotes one does not count. Never \
             force-push a shared branch such as main: rewriting history is for your own unpushed \
             branch, when asked.",
            "Committing or deploying on your own initiative turns out to be welcome as often as \
             waiting for the go.",
            vec![moment(Action::Push), moment(Action::Deploy)],
        ),
        note(
            "commit-hygiene",
            "Commit message: one short plain line saying what changed and why, no marketing tone. \
             Several lines: write the message to a file and use git commit -F. Stage only the files \
             you mean. Keep each file's line endings: no CRLF flips, new files LF, a small diff.",
            "A long marketing-style message, or a commit that flips a file's line endings, is found \
             as easy to read and review as a short plain one.",
            vec![moment(Action::Commit)],
        ),
        note(
            "check-where-you-push-and-what-is-on-main",
            "Before a push, or a claim about what is on main or released, confirm folder, branch and \
             remote (pwd, git status, git remote -v) and git fetch. A local origin/main can be stale: \
             a version or release state comes from git, never from memory.",
            "A claim about the remote's state made from a local ref or from memory turns out right as \
             often as one made after a fetch.",
            vec![moment(Action::Push)],
        ),
        note(
            "scan-before-it-goes-public",
            "Before a push to a public repository, or before a repository is made public, scan the \
             tree for secrets, usernames, local paths, private hostnames and customer data, and \
             require zero hits on every push, not once. Public is irreversible: caches copy it in \
             minutes.",
            "A push to a public repository without a fresh scan turns out never to leak anything.",
            vec![moment(Action::Publish), moment(Action::Push)],
        ),
        note(
            "rehearse-destructive-steps-on-a-copy",
            "Never delete or overwrite data you did not create unless asked. Rehearse a delete, a \
             migration or a deploy on a throwaway copy first, and never test a write path against \
             live data. After a slip, undo it in the same turn and measure that it is undone.",
            "A destructive step run first against live data turns out as safe as one rehearsed on a \
             copy.",
            vec![moment(Action::Delete), moment(Action::Deploy)],
        ),
        note(
            "pin-a-dependency-with-its-reason",
            "Pin a dependency version only with a comment saying why, next to the pin, or someone \
             will 'improve' it back to latest. Add a dependency only when the standard library or \
             one already installed cannot do the job.",
            "A pin with no reason next to it is found to survive later cleanups as often as one that \
             has a reason.",
            vec![moment(Action::Install)],
        ),
        // Bound to the command they are about. A command place holds four
        // notes at most, so these stay two or one apiece and leave room for
        // the owner's own.
        note(
            "absolute-paths-not-the-shell-working-directory",
            "Do not trust the shell's working directory after a cd: it can stick into the next call \
             or reset to the repo root. Use absolute paths in every command and every agent brief, \
             and write back to the same absolute path you read from.",
            "A relative path used after a cd is found to land in the same folder every time, in the \
             shell and in agents.",
            vec![command("cd")],
        ),
        note(
            "script-file-not-shell-quoting",
            "Text with quotes, backticks, $ or backslashes goes into a script file written with the \
             Write tool, then run that file. A bash -c, node -e or python -c one-liner, or a \
             heredoc, can silently mangle it and the error points at the wrong line.",
            "Nested quotes and backslashes pass intact through a shell one-liner or heredoc on this \
             machine.",
            vec![command("bash -c"), command("node -e"), command("python -c")],
        ),
        with_check(
            note(
                "windowless-processes-on-windows",
                "On Windows Start-Process must run hidden: add -WindowStyle Hidden or -NoNewWindow. A window the \
                 user explicitly asked to see: -WindowStyle Normal. Also fine: \
                 ms-settings:, windowsdefender:, control.exe, a .cpl file, http://, https:// or file:///. \
                 Python: CREATE_NO_WINDOW.",
                "A visible console window started by an agent is found to leave the user's focus alone.",
                vec![command("Start-Process")],
            ),
            Check::Requires {
                when: "Start-Process".to_string(),
                required: [
                    "-WindowStyle",
                    "-NoNewWindow",
                    "ms-settings:",
                    "windowsdefender:",
                    "control.exe",
                    ".cpl",
                    "http://",
                    "https://",
                    "file:///",
                ]
                .map(String::from)
                .to_vec(),
            },
        ),
        with_check(
            note(
                "scheduled-task-not-interactive",
                "Never create a scheduled task with schtasks /create and /it or /IT: an interactive task opens a \
                 console window at every run and steals focus. Leave it out, or let the task start a \
                 hidden launcher (wscript.exe, or powershell -WindowStyle Hidden).",
                "An interactive scheduled task (/it) is found to run without any console window.",
                vec![command("schtasks /create")],
            ),
            Check::Forbidden { literals: vec!["/it".to_string(), "/IT".to_string()] },
        ),
    ]
}

/// One starter note, as every note of the pack and of helper-agent mode is
/// built: a global rule with the `workflow-starter` tag, no severity, no check.
fn starter_note(id: &str, text: &str, falsifier: &str, bindings: Vec<model::item::Binding>) -> model::item::Item {
    model::item::Item {
        id: id.to_string(),
        kind: model::item::Kind::Rule,
        text: text.to_string(),
        bindings,
        severity: None,
        project: None,
        tags: vec!["workflow-starter".to_string()],
        expires: None,
        key: None,
        falsifier: Some(falsifier.to_string()),
        check: None,
    }
}

/// The two notes that make up helper-agent mode (first-session item 6). They
/// are NOT in `workflow_starter_pack`: the mode is off in a new memory and the
/// user switches it on with `install --set-helpers on`, which stores exactly
/// these (`set_helpers`). Bound to the Agent tool, so they arrive right before
/// a helper is spawned. A command place holds four notes at most, so these stay
/// two and leave room for the owner's own. The ids are listed in
/// `questionnaire::HELPER_MODE_NOTES` (a test holds the two together).
pub fn helper_mode_notes() -> Vec<model::item::Item> {
    use model::item::{Binding, TargetKind};
    let agent = || vec![Binding::Target { kind: TargetKind::Command, value: "Agent".to_string() }];
    vec![
        starter_note(
            "orchestrator-mode-saves-quota",
            "Strongest model answers, judges, checks; cheaper models work in helpers, ONE per task, never a bundle. Chat stays free. No helper for: questions, trivial one-step tasks, work needing this chat or quick back-and-forth, checking helper results. Targeted tests while working, one full run at the end.",
            "Doing all the work in the strongest model uses no more quota than letting cheaper helper agents do it.",
            agent(),
        ),
        starter_note(
            "an-agent-brief-stands-alone",
            "A helper starts blank, so its brief stands alone: goal, ABSOLUTE folder, files it may touch, a SUMMARY of what is known (not a read-first list), one narrow task, a SHORT report. Check every 20-30 minutes, at most 3 times, then judge it dead by its last real activity, not its status.",
            "An agent given a brief with no working folder or limits is found to behave the same as one given a complete brief.",
            agent(),
        ),
    ]
}

/// What `set_helpers` did to one of the two helper-mode notes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HelperChange {
    /// Stored for the first time.
    Stored,
    /// Stored again after an earlier `off` retracted it (see `set_helpers`).
    Restored,
    /// Retracted.
    Retracted,
    /// `on`, and the note was live already: nothing written.
    AlreadyOn,
    /// `off`, and the note was not live (never stored, or retracted): nothing written.
    AlreadyOff,
    /// The gate or the store said no; nothing was changed for this note.
    Refused(String),
}

/// The retraction reason `set_helpers(off)` records.
pub const HELPERS_OFF_REASON: &str = "The owner switched helper-agent mode off (install --set-helpers off); install --set-helpers on stores it again.";

/// `install --set-helpers on|off`: switch helper-agent mode (first-session
/// item 6) by storing or retracting its two notes, `helper_mode_notes`, and
/// nothing else. Both directions are idempotent: a second `on` or `off` writes
/// nothing.
///
/// `on` goes through the same gate as any other write (`model::store::declare`),
/// so a refusal (for example a command place already full of the owner's own
/// notes) is reported per note, never worked around.
///
/// RETRACTED IDS CANNOT BE CREATED AGAIN. A retraction is a tombstone on the id
/// for good: a create is never an upsert, and a plain second create would leave
/// the id with two heads, which `show` and `get` refuse to read until a resolve.
/// So `on` after an `off` does exactly those two steps in the installer's own
/// name: create the note again (the gate judges it as new), then resolve the id
/// keeping the new head and discarding the tombstone. The log keeps the whole
/// story (created, retracted, created, resolved) and `history` walks it; the id
/// is live again with ONE head, and a later `off` retracts it as before.
pub fn set_helpers(db: &Path, on: bool) -> anyhow::Result<Vec<(String, HelperChange)>> {
    use model::store::{declare, resolve, retract, show, ReadError};
    let mut store = EventStore::open_existing(db)?;
    let mut out = Vec::new();
    for item in helper_mode_notes() {
        let id = item.id.clone();
        let state = show(&store, &id);
        let change = match (on, state) {
            (true, Ok(_)) => HelperChange::AlreadyOn,
            (true, Err(ReadError::NotFound(_))) => match declare(&mut store, "install", "install", installer(), &item) {
                Ok(_) => HelperChange::Stored,
                Err(e) => HelperChange::Refused(e.to_string()),
            },
            (true, Err(ReadError::Retracted(_))) => match declare(&mut store, "install", "install", installer(), &item) {
                Ok(created) => {
                    let events = store.get_events_by_entity(&id)?;
                    let heads = thor_core::cas::compute_head_sets(&events);
                    let discard: Vec<String> = heads
                        .get(&id)
                        .map(|set| set.heads.iter().filter(|h| **h != created.this_hash).cloned().collect())
                        .unwrap_or_default();
                    match resolve(&mut store, "install", "install", installer(), &id, &created.this_hash, &discard, None) {
                        Ok(_) => HelperChange::Restored,
                        Err(e) => HelperChange::Refused(format!("stored the note again but could not settle its old retraction: {e}")),
                    }
                }
                Err(e) => HelperChange::Refused(e.to_string()),
            },
            (true, Err(e)) => HelperChange::Refused(e.to_string()),
            (false, Ok(_)) => match retract(&mut store, "install", "install", installer(), &id, HELPERS_OFF_REASON, None) {
                Ok(_) => HelperChange::Retracted,
                Err(e) => HelperChange::Refused(e.to_string()),
            },
            (false, Err(ReadError::NotFound(_) | ReadError::Retracted(_))) => HelperChange::AlreadyOff,
            (false, Err(e)) => HelperChange::Refused(e.to_string()),
        };
        out.push((id, change));
    }
    Ok(out)
}

/// Write the starting notes into a store that has just been created.
///
/// Only ever called for a store this run created. Seeding an existing memory
/// would push the whole contract into someone's real notes on an upgrade, which is
/// exactly the kind of uninvited write this whole project argues against.
///
/// Every note goes through `model::store::declare` - the same gate the agent's
/// own writes go through, refusals included. A refusal here is reported rather
/// than worked around: if the gate will not accept its own starting notes, that
/// is worth seeing, not hiding.
pub fn seed_working_contract(db: &Path) -> anyhow::Result<Vec<SeededItem>> {
    let claude_json = serve::plan::default_claude_json();
    seed_working_contract_with(db, claude_json.as_deref())
}

/// `seed_working_contract` with Claude Code's state file named, so a test can
/// say what the first-session list shows for the plan.
pub fn seed_working_contract_with(db: &Path, claude_json: Option<&Path>) -> anyhow::Result<Vec<SeededItem>> {
    let paths = crate::questionnaire::Paths { db: Some(db.to_path_buf()), ..Default::default() };
    seed_working_contract_at(db, claude_json, &paths)
}

/// `seed_working_contract_with` and the real paths of this install (the
/// installer program, the store, Claude Code's settings file and the file of
/// the tool server), which the first-session lookup carries so an assistant
/// that never saw AGENTS.md has every path it needs.
pub fn seed_working_contract_at(
    db: &Path,
    claude_json: Option<&Path>,
    paths: &crate::questionnaire::Paths,
) -> anyhow::Result<Vec<SeededItem>> {
    let mut store = EventStore::new(db)?;
    let mut out = Vec::new();
    // The numbered first-session list the setup note points at: a lookup item,
    // rendered from the same catalogue as the page in AGENTS.md.
    let detected = claude_json.map(serve::plan::detect_in);
    let questions = crate::questionnaire::lookup_item_with(detected, paths);
    for item in working_contract().into_iter().chain(workflow_starter_pack()).chain(std::iter::once(questions)) {
        let id = item.id.clone();
        match model::store::declare(&mut store, "install", "install", installer(), &item) {
            Ok(_) => out.push(SeededItem { id, stored: true, refusal: None }),
            Err(e) => out.push(SeededItem { id, stored: false, refusal: Some(e.to_string()) }),
        }
    }
    Ok(out)
}

/// The combined outcome of `ensure_and_seed_store`: what happened to the
/// store itself, the starting notes (empty when the store already existed),
/// and the response-guard rulebook beside it.
#[derive(Debug)]
pub struct BootstrapReport {
    pub store: StoreOutcome,
    pub seeded: Vec<SeededItem>,
    pub rulebook: RulebookReport,
}

/// Bring a store from nothing to one that answers - the exact sequence
/// `install`'s own CLI runs by hand (`ensure_store`, then only on a store
/// this call created `seed_working_contract`, then `seed_response_rulebook`
/// on every call regardless) - collapsed into one call for a caller with no
/// settings.json to write into and no business going near one.
///
/// WHY THIS IS SEPARATE FROM `install`. `install`'s CLI always resolves a
/// settings.json (defaulting under HOME/USERPROFILE when none is named) and
/// refuses outright when the `serve` binary it would wire hooks to is not
/// there - both wrong for a container entry point, which ships neither
/// `serve` nor an agent's configuration to touch. This function is the store
/// half alone: no hooks, no tool-server registration, no project marker.
/// See `ops/src/bin/initstore.rs`, the binary built on exactly this.
pub fn ensure_and_seed_store(db: &Path) -> anyhow::Result<BootstrapReport> {
    let store = ensure_store(db)?;
    let seeded = match store {
        StoreOutcome::Created => seed_working_contract(db)?,
        StoreOutcome::AlreadyThere => Vec::new(),
    };
    let rulebook = seed_response_rulebook(db)?;
    Ok(BootstrapReport { store, seeded, rulebook })
}

/// What happened to the tool-server registration on this run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerOutcome {
    Added,
    AlreadyPresent,
    Replaced,
}

#[derive(Debug, Clone)]
pub struct ServerReport {
    pub outcome: ServerOutcome,
    /// The command the previous entry under this name pointed at, when this
    /// run replaced one. Reported so an upgrade never happens silently.
    pub replaced: Option<String>,
    pub backup_path: Option<std::path::PathBuf>,
}

/// Register the tool server an agent calls to WRITE to its memory, under
/// `mcpServers.<name>` in `path`.
///
/// The hooks alone are a one-way street: they hand facts to the agent and can
/// stop a wrong write, but nothing the agent decides ever gets back into the
/// memory. That is why this is part of installing rather than a follow-up
/// step someone reads about later and skips.
///
/// Same non-negotiables as `install_hooks`, for the same reasons: back up
/// first, refuse a file that is not valid JSON rather than starting a fresh
/// one over it, and never touch an entry this tool did not put there.
///
/// One deliberate difference. Hooks live in a LIST, so a foreign hook is
/// simply appended past. A server lives under a NAME, and there can only be
/// one `thor`. An existing entry that differs is therefore REPLACED, and the
/// command it used to point at comes back in the report. Refusing instead
/// would make an upgrade impossible - the 1.0 entry would stay registered and
/// the person would believe 2.0 was installed - and replacing it quietly
/// would be worse still.
pub fn install_tool_server(
    path: &Path,
    name: &str,
    exe: &str,
    args: &[String],
) -> anyhow::Result<ServerReport> {
    let existed = path.exists();
    let raw = if existed { fs::read_to_string(path)? } else { "{}".to_string() };

    let mut root: Value = serde_json::from_str(&raw).map_err(|e| {
        anyhow::anyhow!(
            "{} is not valid JSON ({e}) - refusing to touch it; fix the JSON first, or point --mcp-json at a different file",
            path.display()
        )
    })?;
    anyhow::ensure!(
        root.is_object(),
        "{} does not contain a JSON object at the top level - refusing to touch it",
        path.display()
    );

    if root.get("mcpServers").is_some() {
        anyhow::ensure!(
            root["mcpServers"].is_object(),
            "{}'s \"mcpServers\" key is not a JSON object - refusing to touch a shape this tool does not recognise",
            path.display()
        );
    } else {
        root["mcpServers"] = json!({});
    }

    let entry = json!({
        "type": "stdio",
        "command": exe,
        "args": args,
    });

    let previous = root["mcpServers"].get(name).cloned();
    let outcome = match &previous {
        Some(old) if same_server_entry(old, &entry) => ServerOutcome::AlreadyPresent,
        Some(_) => ServerOutcome::Replaced,
        None => ServerOutcome::Added,
    };

    // Nothing to write, so nothing to back up either: a re-run on an already
    // correct file leaves it byte-for-byte alone, including its formatting.
    if outcome == ServerOutcome::AlreadyPresent {
        return Ok(ServerReport { outcome, replaced: None, backup_path: None });
    }

    let backup_path = if existed {
        Some(backup_once(path, &raw)?)
    } else {
        None
    };

    root["mcpServers"]
        .as_object_mut()
        .expect("checked above: this key is an object")
        .insert(name.to_string(), entry);

    let pretty = serde_json::to_string_pretty(&root)?;
    fs::write(path, pretty + "\n")?;

    let replaced = previous
        .as_ref()
        .and_then(|p| p.get("command"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|_| outcome == ServerOutcome::Replaced);

    Ok(ServerReport { outcome, replaced, backup_path })
}

/// True iff `old` is the entry `new` would write, up to path spelling: the
/// same keys, `type` and any other key exact, and `command` and every `args`
/// word equal after `flatten_word` (separators, case on Windows, quoting). An
/// extra key, a different program or a different store path is a real
/// difference and still gets the entry replaced.
fn same_server_entry(old: &Value, new: &Value) -> bool {
    if old == new {
        return true;
    }
    let (Some(o), Some(n)) = (old.as_object(), new.as_object()) else { return false };
    if o.len() != n.len() || o.keys().any(|k| !n.contains_key(k)) {
        return false;
    }
    let words = |v: &Value| v.as_str().map(command_words);
    o.iter().all(|(key, theirs)| {
        let ours = &n[key];
        match key.as_str() {
            "command" => words(theirs).is_some() && words(theirs) == words(ours),
            "args" => match (theirs.as_array(), ours.as_array()) {
                (Some(t), Some(u)) => {
                    t.len() == u.len() && t.iter().zip(u).all(|(x, y)| words(x).is_some() && words(x) == words(y))
                }
                _ => false,
            },
            _ => theirs == ours,
        }
    })
}

/// What happened to the project marker on this run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkerOutcome {
    Written,
    AlreadyThere,
}

/// Write the marker that gives a folder its own memory scope.
///
/// Without one, everything a person stores while working in that folder lands
/// in the shared memory every project sees, and search there competes with
/// projects they were not asking about. THOR gets worse the more it is used,
/// which is the opposite of the promise.
///
/// A marker that already names a DIFFERENT key is refused. Re-scoping a
/// project silently would strand every fact already filed under the old key:
/// they stay in the store, they simply stop being served here, and nothing
/// says so.
pub fn write_project_marker(dir: &Path, key: &str) -> anyhow::Result<MarkerOutcome> {
    let key = key.trim();
    anyhow::ensure!(!key.is_empty(), "a project key cannot be blank");
    anyhow::ensure!(
        !key.contains('\n') && !key.contains('\r'),
        "a project key is one line, and {key:?} is not"
    );

    let marker = dir.join(serve::project::MARKER_FILE_NAME);
    if marker.exists() {
        let current = fs::read_to_string(&marker)?;
        let current = current.trim();
        anyhow::ensure!(
            current == key,
            "{} already scopes this folder to {current:?} - refusing to re-scope it to {key:?}, \
             which would strand every fact already filed under {current:?}",
            marker.display()
        );
        return Ok(MarkerOutcome::AlreadyThere);
    }

    fs::write(&marker, format!("{key}\n"))?;
    Ok(MarkerOutcome::Written)
}

/// The id `record_project_opened` writes under for project `key` - one
/// record per project, forever. Only the installer may write any event on it
/// (`model::store::reserved_id_owner`), so no retract or revise from anywhere
/// else can tombstone it or move its working copy.
///
/// Ids in this store carry no character restriction of their own to derive
/// around: `model::gate::declare` validates an item's SHAPE (its bindings,
/// falsifier, text length, scope) but never its id, and `entity_id` is a
/// free-form SQLite TEXT column with no format constraint
/// (`core::event_store`'s schema). `key` itself is already constrained to a
/// single, non-blank line by every path that can produce it
/// (`serve::project::resolve_project`'s marker read and git-root basename;
/// `write_project_marker`'s own refusal of a `--project` value containing a
/// newline), so it is used exactly as given, with no separate normalisation
/// step to invent.
pub fn project_opened_record_id(key: &str) -> String {
    model::store::project_opened_id(key)
}

/// The record itself, pure: its exact id/kind/project/tags/text are
/// testable with no store at all. `today` is `YYYY-MM-DD`. `files` is how
/// many files the code read found; `None` for a registration that read no
/// code at all (`install --register-root`), whose text then says only that.
fn project_record(id: String, key: &str, files: Option<usize>, today: &str) -> model::item::Item {
    let text = match files {
        Some(files) => format!(
            "Project {key}: install read its code on {today} ({files} file(s)) so it can be \
             searched with search_code and kept fresh on every commit. Notes filed under '{key}' \
             belong to this repository."
        ),
        None => format!(
            "Project {key}: install registered its working copy on {today}. Notes filed under \
             '{key}' belong to this project (a code repository, or a plain topic folder that has no code to search)."
        ),
    };
    model::item::Item {
        id,
        kind: model::item::Kind::Report,
        text,
        bindings: Vec::new(),
        severity: None,
        project: Some(key.to_string()),
        tags: vec!["working-contract".to_string(), "project-record".to_string()],
        expires: None,
        key: None,
        falsifier: None,
        check: None,
    }
}

// ------------------------------------------------ the registered working copy
//
// WHY A PROJECT HAS ONE REGISTERED CHECKOUT (A2, Z3). A rule's file check is
// only worth trusting when it is tried against the project's real checkout -
// and "real" was whatever folder the session happened to run in, so a second
// copy with a planted line could prove anything. The installer now records,
// once, where the project's checkout lives (`model::store`'s trailing
// `"root"` key on the project record, installer events only), and a second
// copy can never take that over: a registration moves only when the folder it
// names is gone.

/// What `register_root` did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegisterOutcome {
    /// The project had no record yet: it now has one, carrying this checkout.
    Created { root: String },
    /// The record existed without a working copy: it now carries this one.
    Registered { root: String },
    /// The project's record was retracted before this build, so the working
    /// copy is registered on a record of its own
    /// (`model::store::project_root_id`).
    CreatedUnderRootRecord { root: String },
    /// This checkout is already the registered one: nothing was written.
    AlreadyHere { root: String },
    /// The registered folder no longer exists, so the registration moved here.
    Moved { from: String, to: String },
}

/// Why `register_root` wrote nothing. `Display` is the exact line printed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegisterRefusal {
    /// The folder is neither inside a git checkout nor a marked topic folder.
    NotACheckout { cwd: String },
    /// The folder is far too broad to be one topic: the home folder or a
    /// whole drive. Everything a person ever does there would land in one
    /// memory.
    TooBroad { cwd: String },
    /// The registered folder still exists and is still a checkout of this
    /// project: a second copy.
    StillACheckout { key: String, old: String },
    /// The registered folder still exists but no longer resolves to this
    /// project - editing its marker is not a move (D3).
    StillExists { key: String, old: String },
    /// The top of this checkout resolves to another project, or to none.
    ResolvesElsewhere { key: String, root: String, resolved: Option<String> },
    /// The store could not be read or written.
    Store(String),
}

impl std::fmt::Display for RegisterRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RegisterRefusal::NotACheckout { cwd } => write!(
                f,
                "REFUSED: {cwd} is not set up as a topic folder yet. Open a terminal INSIDE the folder for your topic \
                 and run the installer there with --topic added (no git or code needed). Nothing was changed."
            ),
            RegisterRefusal::TooBroad { cwd } => write!(
                f,
                "REFUSED: {cwd} is your whole home folder or a whole drive, which is too broad to be one topic. Make a \
                 folder for just one subject (for example 'Kitchen renovation'), open that folder, and run this again \
                 from inside it. Nothing was changed."
            ),
            RegisterRefusal::StillACheckout { key, old } => write!(
                f,
                "REFUSED: '{key}' is registered at {old}, which is still a checkout of it - a second copy cannot take \
                 over its registration. Nothing was changed."
            ),
            RegisterRefusal::StillExists { key, old } => write!(
                f,
                "REFUSED: '{key}' is registered at {old}, which still exists. Moving a registration is the owner's \
                 call: if the project really moved, the owner removes or renames the old folder, then runs this \
                 again. Nothing was changed."
            ),
            RegisterRefusal::ResolvesElsewhere { key, root, resolved } => {
                let resolved = match resolved {
                    Some(other) => format!("project '{other}'"),
                    None => "no project".to_string(),
                };
                write!(
                    f,
                    "REFUSED: the checkout at {root} resolves to {resolved}, not '{key}' - run this from inside a \
                     checkout of '{key}' (its .thor-project marker, or else its folder name, is what names it). \
                     Nothing was changed."
                )
            }
            RegisterRefusal::Store(e) => write!(f, "REFUSED: the memory could not be read or written: {e}. Nothing was changed."),
        }
    }
}

impl std::error::Error for RegisterRefusal {}

/// A folder path the way a registration stores it: forward slashes, no
/// trailing slash. Never through `std::fs::canonicalize`, which on Windows
/// prefixes `\\?\` and would make the same folder compare unequal.
fn root_string(path: &Path) -> String {
    let s = path.display().to_string().replace('\\', "/");
    match s.strip_suffix('/') {
        Some(stripped) if !stripped.is_empty() && !stripped.ends_with(':') => stripped.to_string(),
        _ => s,
    }
}

/// Whether two registered folder paths name the same folder: separators
/// already equal (`root_string`), and on Windows compared without regard to
/// case, the way the filesystem itself compares them.
fn same_root(a: &str, b: &str) -> bool {
    if cfg!(windows) {
        a.eq_ignore_ascii_case(b)
    } else {
        a == b
    }
}

/// The folder a registration records for `cwd`: the top of the git checkout it
/// sits in, else the top of the marked topic folder it sits in (a plain folder
/// with a `.thor-project` file and no git above it). `None` for any other
/// folder.
fn registration_top(cwd: &Path) -> Option<PathBuf> {
    serve::project::git_root(cwd).or_else(|| serve::project::plain_root(cwd))
}

/// The name a plain folder gets as a topic: its own folder name. Refused for
/// the home folder and for a drive root, which are far too broad to be one
/// topic (everything a person ever does there would share one memory).
pub fn topic_folder_name(dir: &Path) -> Result<String, RegisterRefusal> {
    let too_broad = || RegisterRefusal::TooBroad { cwd: root_string(dir) };
    if home_dir().is_some_and(|home| same_root(&root_string(&home), &root_string(dir))) {
        return Err(too_broad());
    }
    dir.file_name().and_then(|n| n.to_str()).map(str::trim).filter(|n| !n.is_empty()).map(str::to_string).ok_or_else(too_broad)
}

/// What a registration of a checkout would do, decided before anything is
/// written.
enum Plan {
    /// Nothing is registered for the project yet: first come.
    First,
    /// This checkout is already the registered one.
    AlreadyHere,
    /// The registered folder is gone: the registration moves here.
    MoveFrom(String),
}

/// The plan for registering `root` as `key`'s working copy, or the refusal.
/// A registered folder that still exists is never taken over (D3), whether
/// it still resolves to `key` or not: editing a marker is not a move.
fn registration_plan(store: &EventStore, root: &str, key: &str) -> Result<Plan, RegisterRefusal> {
    let Some((old, _seq)) = model::store::registered_root_raw(store, key) else { return Ok(Plan::First) };
    let old_text = root_string(&old);
    if same_root(&old_text, root) {
        return Ok(Plan::AlreadyHere);
    }
    if old.exists() {
        return Err(if serve::project::resolve_project(&old).as_deref() == Some(key) {
            RegisterRefusal::StillACheckout { key: key.to_string(), old: old_text }
        } else {
            RegisterRefusal::StillExists { key: key.to_string(), old: old_text }
        });
    }
    Ok(Plan::MoveFrom(old_text))
}

/// Refuse, BEFORE anything is written anywhere (settings, marker, code index,
/// store), a per-project install from a folder whose project is already
/// registered at another folder that still exists. `Ok` when there is nothing
/// to protect: no store yet, no git checkout here, or no registration that
/// this folder would take over.
pub fn check_registration(db: &Path, cwd: &Path, key: &str) -> Result<(), RegisterRefusal> {
    if !db.exists() {
        return Ok(());
    }
    let Some(top) = registration_top(cwd) else { return Ok(()) };
    check_registration_at(db, &top, key)
}

/// `check_registration` for a folder that is already known to be the top of
/// its project - a plain folder about to become a topic has no marker yet, so
/// `registration_top` cannot find it from the folder alone.
pub fn check_registration_at(db: &Path, top: &Path, key: &str) -> Result<(), RegisterRefusal> {
    if !db.exists() {
        return Ok(());
    }
    let store = EventStore::open_existing(db).map_err(|e| RegisterRefusal::Store(e.to_string()))?;
    registration_plan(&store, &root_string(top), key).map(|_| ())
}

/// Register the checkout `cwd` sits in as project `key`'s working copy - the
/// one function both entry points use: the per-project install
/// (`record_project_opened`) and `install --register-root`.
///
/// The registered folder is the top of the checkout (`git_root(cwd)`), and it
/// must resolve to `key`. Then:
/// - no record yet: the record is created carrying the root (`files` names
///   how much code was read, `None` when none was);
/// - a record without a root: one `fact_revised` by the installer adds it;
/// - a record retracted before this build: `project-<key>-root-by-install`
///   carries it instead;
/// - the same root: nothing is written;
/// - another root that no longer exists: moved;
/// - another root that still exists: refused (`registration_plan`).
pub fn register_root(
    store: &mut EventStore,
    cwd: &Path,
    key: &str,
    files: Option<usize>,
) -> Result<RegisterOutcome, RegisterRefusal> {
    let Some(top) = registration_top(cwd) else {
        return Err(RegisterRefusal::NotACheckout { cwd: root_string(cwd) });
    };
    let root = root_string(&top);
    let resolved = serve::project::resolve_project(&top);
    if resolved.as_deref() != Some(key) {
        return Err(RegisterRefusal::ResolvesElsewhere { key: key.to_string(), root, resolved });
    }
    let plan = registration_plan(store, &root, key)?;
    let at = serve::time::now_iso8601();
    let store_err = |e: model::store::WriteError| RegisterRefusal::Store(e.to_string());
    match plan {
        Plan::AlreadyHere => return Ok(RegisterOutcome::AlreadyHere { root }),
        Plan::MoveFrom(from) => {
            let id = model::store::root_registrations(store, key)
                .pop()
                .map(|r| r.id)
                .unwrap_or_else(|| project_opened_record_id(key));
            model::store::revise_project_root(store, &id, &root, &at, installer()).map_err(store_err)?;
            return Ok(RegisterOutcome::Moved { from, to: root });
        }
        Plan::First => {}
    }
    let today: String = at.chars().take(10).collect();
    let opened = project_opened_record_id(key);
    match model::store::show(store, &opened) {
        Err(model::store::ReadError::NotFound(_)) => {
            let item = project_record(opened, key, files, &today);
            model::store::declare_project_record(store, &item, &root, &at, installer()).map_err(store_err)?;
            Ok(RegisterOutcome::Created { root })
        }
        Ok(_) => {
            model::store::revise_project_root(store, &opened, &root, &at, installer()).map_err(store_err)?;
            Ok(RegisterOutcome::Registered { root })
        }
        Err(model::store::ReadError::Retracted(_)) => {
            let fallback = model::store::project_root_id(key);
            match model::store::show(store, &fallback) {
                Err(model::store::ReadError::NotFound(_)) => {
                    let item = project_record(fallback, key, files, &today);
                    model::store::declare_project_record(store, &item, &root, &at, installer()).map_err(store_err)?;
                    Ok(RegisterOutcome::CreatedUnderRootRecord { root })
                }
                Ok(_) => {
                    model::store::revise_project_root(store, &fallback, &root, &at, installer()).map_err(store_err)?;
                    Ok(RegisterOutcome::Registered { root })
                }
                Err(e) => Err(RegisterRefusal::Store(e.to_string())),
            }
        }
        Err(e) => Err(RegisterRefusal::Store(e.to_string())),
    }
}

/// Record, in the memory itself, that this project's code was read - and
/// register the checkout it was read from as the project's working copy
/// (`register_root`). This closes the gap `mcp::refuse_a_new_collection`
/// otherwise leaves open.
///
/// THE DEFECT THIS CLOSES. `build_project_index` (above) reads a project's
/// code into its own sidecar, under project name `key`, but never told the
/// MAIN store that name was open. The write gate's own collection check
/// (`mcp::refuse_a_new_collection`, `mcp/src/lib.rs`) passes a project only
/// when a `.thor-project` marker names it or the scope catalogue
/// (`serve::lookup::catalog`, built from LIVE items in the main store)
/// already lists it - and the code index is a sidecar that catalogue never
/// reads. So the very first well-formed note filed under a freshly-installed
/// project (plain `install`, no `--project`, so no marker either) was
/// refused as an unknown collection, pointed at `install --project <name>` -
/// advice that is stale now that plain `install` already reads the code
/// under the folder's own name.
///
/// One live `Report` scoped to `key` is enough to close it: `catalog()`
/// already counts a `Report` toward its own `project` field (see
/// `serve::lookup::catalog`'s `Kind::Report | Kind::Chunk` arm), so nothing
/// about the gate itself needs to change once this exists.
///
/// IDEMPOTENT. A second run from the same checkout writes nothing
/// (`RegisterOutcome::AlreadyHere`); a record that already exists keeps its
/// text, and only gains the working copy it did not carry yet.
pub fn record_project_opened(
    db: &Path,
    cwd: &Path,
    key: &str,
    files: usize,
) -> Result<RegisterOutcome, RegisterRefusal> {
    let mut store = EventStore::new(db).map_err(|e| RegisterRefusal::Store(e.to_string()))?;
    register_root(&mut store, cwd, key, Some(files))
}

/// `record_project_opened` for a plain topic folder: no code was read, so the
/// record says only that the folder was registered.
pub fn record_topic_opened(db: &Path, cwd: &Path, key: &str) -> Result<RegisterOutcome, RegisterRefusal> {
    let mut store = EventStore::new(db).map_err(|e| RegisterRefusal::Store(e.to_string()))?;
    register_root(&mut store, cwd, key, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Today, as `YYYY-MM-DD` - the same derivation `register_root` makes
    /// from the one clock this workspace shares (`serve::time::now_iso8601`).
    fn today_ymd() -> String {
        serve::time::now_iso8601().chars().take(10).collect()
    }

    fn specs() -> Vec<HookSpec> {
        standard_hooks("C:\\fake\\serve.exe", "C:\\fake\\thor.db")
    }

    /// The defect this guards against, and it is the worst kind this project
    /// has: a hook command that is written correctly-looking but does not
    /// parse. `serve` takes `--db` as a GLOBAL option, before the subcommand.
    /// With it after, every hook exits 1 with "unexpected argument '--db'" -
    /// and hooks fail open, so nothing complains and the memory simply stops
    /// speaking. Caught only by running the command this function writes.
    #[test]
    fn the_command_puts_db_before_the_subcommand() {
        let specs = standard_hooks("C:\\thor2\\bin\\serve.exe", "C:\\thor2\\thor.db");
        for spec in &specs {
            assert_eq!(
                spec.command, "\"C:\\thor2\\bin\\serve.exe\" --db \"C:\\thor2\\thor.db\" hook",
                "the {} hook must call serve the way serve's own CLI parses it",
                spec.event
            );
            let db_at = spec.command.find("--db").expect("the command must name --db");
            let sub_at = spec.command.find(" hook").expect("the command must call the hook subcommand");
            assert!(db_at < sub_at, "--db is a global option, so it comes first: {}", spec.command);
        }
    }

    #[test]
    fn a_fresh_settings_file_gets_all_four_hooks_added() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let report = install_hooks(&path, &specs()).unwrap();
        assert_eq!(report.results.len(), 4);
        assert!(report.results.iter().all(|(_, o)| *o == HookOutcome::Added));
        assert!(report.backup_path.is_none(), "nothing existed yet, so there is nothing to back up");

        let written: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(written["hooks"]["SessionStart"].as_array().unwrap().len(), 1);
        assert_eq!(written["hooks"]["PreToolUse"].as_array().unwrap().len(), 1);
        assert_eq!(written["hooks"]["UserPromptSubmit"].as_array().unwrap().len(), 1);
        assert_eq!(written["hooks"]["PreToolUse"][0]["matcher"], json!("*"));
        assert!(written["hooks"]["SessionStart"][0].get("matcher").is_none());
        // Surface 5: the Response Guard on the Stop hook - the one that was
        // missing and let a whole session of untidy replies through.
        assert_eq!(written["hooks"]["Stop"].as_array().unwrap().len(), 1);
    }

    /// The defect this guards against: a naive installer appends its group
    /// unconditionally, so running `install` twice (a re-run, a second agent
    /// setup pass) leaves TWO identical hook entries firing the same command
    /// on every session start.
    #[test]
    fn a_second_install_adds_nothing_new() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        install_hooks(&path, &specs()).unwrap();
        let second = install_hooks(&path, &specs()).unwrap();

        assert!(
            second.results.iter().all(|(_, o)| *o == HookOutcome::AlreadyPresent),
            "the second run must recognise every hook as already installed: {:?}",
            second.results
        );
        let written: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        for event in ["SessionStart", "PreToolUse", "UserPromptSubmit", "Stop"] {
            assert_eq!(
                written["hooks"][event].as_array().unwrap().len(),
                1,
                "event {event} must still carry exactly one group after a second install"
            );
        }
    }

    /// 2026-10-08: running the installer with `C:/x/thor.db` over hooks written
    /// with backslashes added a duplicate of every hook. The same program and
    /// store in another spelling (separators, quoting, case on Windows) is the
    /// same hook.
    #[test]
    fn the_same_hooks_in_another_path_spelling_are_already_there() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        install_hooks(&path, &standard_hooks("C:\\thor2\\bin\\serve.exe", "C:\\thor2\\thor.db")).unwrap();
        let before = fs::read(&path).unwrap();

        let slashes = install_hooks(&path, &standard_hooks("C:/thor2/bin/serve.exe", "C:/thor2/thor.db")).unwrap();
        assert!(
            slashes.results.iter().all(|(_, o)| *o == HookOutcome::AlreadyPresent),
            "forward slashes over backslashes must add nothing: {:?}",
            slashes.results
        );
        assert!(slashes.replaced.is_empty());
        let written: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        for event in ["SessionStart", "PreToolUse", "UserPromptSubmit", "Stop"] {
            assert_eq!(written["hooks"][event].as_array().unwrap().len(), 1, "{event} must not gain a duplicate");
        }
        assert_eq!(fs::read(&path).unwrap(), before, "the existing hooks must stay as they were");

        // An unquoted spelling of the same command is the same command too.
        let unquoted = vec![HookSpec {
            event: "Stop",
            matcher: None,
            command: "C:/thor2/bin/serve.exe --db C:/thor2/thor.db hook".to_string(),
        }];
        let third = install_hooks(&path, &unquoted).unwrap();
        assert_eq!(third.results[0].1, HookOutcome::AlreadyPresent);

        if cfg!(windows) {
            let shouting = install_hooks(&path, &standard_hooks("c:\\THOR2\\bin\\SERVE.EXE", "C:\\Thor2\\THOR.db")).unwrap();
            assert!(shouting.results.iter().all(|(_, o)| *o == HookOutcome::AlreadyPresent), "case is ignored on Windows");
        }
    }

    /// The other side of the same rule: the comparison flattens spelling and
    /// nothing else. Another store, another program, another flag is a
    /// different hook and still gets its own entry.
    #[test]
    fn a_genuinely_different_store_or_program_is_still_a_different_hook() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        // A real serve.exe on disk, so the installer's "binary is gone" repair
        // (stale_thor_command) stays out of this question.
        let exe = dir.path().join("serve.exe");
        fs::write(&exe, b"x").unwrap();
        let exe = exe.display().to_string();
        let exe_slashes = exe.replace('\\', "/");
        install_hooks(&path, &standard_hooks(&exe, "C:\\thor2\\thor.db")).unwrap();

        let other_store = install_hooks(&path, &standard_hooks(&exe_slashes, "C:/other/thor.db")).unwrap();
        assert!(other_store.results.iter().all(|(_, o)| *o == HookOutcome::Added), "{:?}", other_store.results);

        let other_exe = install_hooks(&path, &standard_hooks("C:/thor2/bin/other.exe", "C:/thor2/thor.db")).unwrap();
        assert!(other_exe.results.iter().all(|(_, o)| *o == HookOutcome::Added), "{:?}", other_exe.results);

        assert!(!same_spelling("\"a.exe\" --db \"b.db\" hook", "\"a.exe\" --db \"b.db\" hook --extra"));
        assert!(!same_spelling("\"C:\\a b\\x.exe\" hook", "\"C:\\a\" \"b\\x.exe\" hook"));
    }

    /// The tool server entry in the user's `.claude.json` gets the same
    /// treatment: another spelling of the same program and store is left alone
    /// (no rewrite, no backup), a different store is still replaced.
    #[test]
    fn the_tool_server_in_another_path_spelling_is_already_registered() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".claude.json");
        install_tool_server(&path, "thor", "C:\\thor2\\bin\\mcp.exe", &server_args("C:\\thor2\\thor.db")).unwrap();
        let before = fs::read(&path).unwrap();

        let same = install_tool_server(&path, "thor", "C:/thor2/bin/mcp.exe", &server_args("C:/thor2/thor.db")).unwrap();
        assert_eq!(same.outcome, ServerOutcome::AlreadyPresent);
        assert!(same.backup_path.is_none());
        assert_eq!(fs::read(&path).unwrap(), before, "the file must not be rewritten");

        let other = install_tool_server(&path, "thor", "C:/thor2/bin/mcp.exe", &server_args("C:/other/thor.db")).unwrap();
        assert_eq!(other.outcome, ServerOutcome::Replaced, "a different store is a real change");
    }

    /// The other defect this guards against: an installer that "cleans up"
    /// or rewrites the hooks array wholesale would silently delete another
    /// tool's hook (or an earlier hand-written one) the moment it runs.
    #[test]
    fn install_leaves_a_foreign_hook_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(
            &path,
            serde_json::to_string_pretty(&json!({
                "hooks": {
                    "SessionStart": [
                        { "hooks": [ { "type": "command", "command": "\"C:\\\\some\\\\other-tool.exe\" backup" } ] }
                    ]
                },
                "permissions": { "allow": ["Bash(ls:*)"] }
            }))
            .unwrap(),
        )
        .unwrap();

        let report = install_hooks(&path, &specs()).unwrap();
        assert_eq!(
            report.results.iter().find(|(e, _)| e == "SessionStart").unwrap().1,
            HookOutcome::Added,
            "our own SessionStart hook must still be added alongside the foreign one"
        );

        let written: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        let session_start = written["hooks"]["SessionStart"].as_array().unwrap();
        assert_eq!(session_start.len(), 2, "the foreign hook must survive, plus our own");
        assert!(
            session_start.iter().any(|g| group_has_command(g, "\"C:\\\\some\\\\other-tool.exe\" backup")),
            "the pre-existing foreign hook must be byte-identical, not merged or rewritten"
        );
        // Unrelated top-level keys must survive completely untouched too.
        assert_eq!(written["permissions"]["allow"][0], json!("Bash(ls:*)"));
    }

    /// Refusal test: invalid JSON must never be silently replaced with a
    /// fresh `{}` (that would look like a successful install while quietly
    /// discarding every setting the file used to hold).
    #[test]
    fn install_refuses_invalid_json_and_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let original = "{ this is not valid json ,,, ";
        fs::write(&path, original).unwrap();

        let err = install_hooks(&path, &specs());
        assert!(err.is_err(), "invalid JSON must be refused, not repaired or replaced");
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            original,
            "a refused install must leave the file exactly as it was"
        );
    }

    #[test]
    fn install_refuses_a_non_object_hooks_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(&path, serde_json::to_string(&json!({ "hooks": "not-an-object" })).unwrap()).unwrap();
        let err = install_hooks(&path, &specs());
        assert!(err.is_err(), "an unrecognised \"hooks\" shape must be refused, not overwritten");
    }

    #[test]
    fn install_backs_up_the_original_before_writing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let original = serde_json::to_string_pretty(&json!({ "hooks": {}, "marker": "original" })).unwrap();
        fs::write(&path, &original).unwrap();

        let report = install_hooks(&path, &specs()).unwrap();
        let backup = report.backup_path.expect("an existing file must be backed up");
        assert_eq!(fs::read_to_string(&backup).unwrap(), original, "the backup must hold the pre-install content");
    }

    /// A second install must not replace the user's pristine backup with the
    /// already-wired file: the "Remove THOR completely" steps rely on it.
    #[test]
    fn a_second_install_keeps_the_first_backup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(&path, "{}").unwrap();

        let first = install_hooks(&path, &specs()).unwrap();
        let backup = first.backup_path.expect("an existing file must be backed up");
        assert_eq!(fs::read_to_string(&backup).unwrap(), "{}");

        let second = install_hooks(&path, &specs()).unwrap();
        assert_eq!(second.backup_path.as_ref(), Some(&backup), "the report still names the one backup");
        assert_eq!(fs::read_to_string(&backup).unwrap(), "{}", "the backup must still hold the pre-THOR file");
    }

    /// The same rule for the tool-server file, when a later write does change it.
    #[test]
    fn a_replaced_tool_server_entry_keeps_the_first_backup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".claude.json");
        fs::write(&path, "{}").unwrap();
        install_tool_server(&path, "thor", "mcp.exe", &server_args("a.db")).unwrap();
        let second = install_tool_server(&path, "thor", "mcp.exe", &server_args("b.db")).unwrap();
        let backup = second.backup_path.expect("a changed file has a backup");
        assert_eq!(fs::read_to_string(&backup).unwrap(), "{}");
    }

    /// The defect this guards against, and it is this project's worst class:
    /// move the binaries, run install again, and the old hooks stayed. They
    /// call a binary that is gone, and a hook that cannot run fails OPEN - so
    /// the agent carries on and the memory simply never speaks again, with
    /// nothing reporting it and no uninstall to clean it up.
    #[test]
    fn a_hook_whose_binary_is_gone_is_repointed_not_duplicated() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        // A real binary at the new location, so only the OLD one is missing.
        let new_exe = dir.path().join("serve.exe");
        fs::write(&new_exe, b"not really a binary, but it exists").unwrap();
        // The GONE binary's path uses THIS platform's own separators, never a
        // Windows literal. `stale_thor_command` decides staleness with
        // `Path::file_name`, and on Linux a backslash is an ordinary character:
        // "C:\gone\serve.exe" is then ONE filename component, not
        // ".../serve.exe", so it never matched our file name and the repoint
        // silently did not fire. That made this test pass on Windows and fail on
        // the Linux CI runner - caught the first time a v2 tag actually built,
        // on 2026-08-14.
        let gone = dir.path().join("gone");
        let old_command =
            format!("\"{}\" --db \"{}\" hook", gone.join("serve.exe").display(), gone.join("thor.db").display());
        fs::write(
            &path,
            serde_json::to_string_pretty(&json!({
                "hooks": { "SessionStart": [ { "hooks": [ { "type": "command", "command": old_command } ] } ] }
            }))
            .unwrap(),
        )
        .unwrap();

        let specs =
            standard_hooks(&new_exe.display().to_string(), &dir.path().join("thor.db").display().to_string());
        let report = install_hooks(&path, &specs).unwrap();

        assert_eq!(
            report.results.iter().find(|(e, _)| e == "SessionStart").unwrap().1,
            HookOutcome::Replaced
        );
        assert!(
            report.replaced.iter().any(|c| c == &old_command),
            "the replacement must be reported, or the move happened silently: {:?}",
            report.replaced
        );
        let written: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        let groups = written["hooks"]["SessionStart"].as_array().unwrap();
        assert_eq!(groups.len(), 1, "it must be repointed in place, never appended beside the dead one");
        assert!(!format!("{groups:?}").contains("gone"), "the dead path must be gone: {groups:?}");
    }

    /// The other half, and the reason this is narrow: somebody running two
    /// stores on purpose must not silently lose one. A hook whose binary is
    /// still THERE is left exactly as it is.
    #[test]
    fn a_hook_whose_binary_still_exists_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let other_exe = dir.path().join("serve.exe");
        fs::write(&other_exe, b"exists").unwrap();
        let their_command = format!("\"{}\" --db \"C:\\\\other\\\\thor.db\" hook", other_exe.display());
        fs::write(
            &path,
            serde_json::to_string_pretty(&json!({
                "hooks": { "SessionStart": [ { "hooks": [ { "type": "command", "command": their_command } ] } ] }
            }))
            .unwrap(),
        )
        .unwrap();

        let specs = standard_hooks(&other_exe.display().to_string(), "C:\\mine\\thor.db");
        let report = install_hooks(&path, &specs).unwrap();

        assert_eq!(report.results.iter().find(|(e, _)| e == "SessionStart").unwrap().1, HookOutcome::Added);
        assert!(report.replaced.is_empty(), "nothing was dead, so nothing may be replaced");
        let written: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            written["hooks"]["SessionStart"].as_array().unwrap().len(),
            2,
            "both stores keep their own hook"
        );
    }

    /// A foreign tool's hook is never touched, dead or alive: the file name
    /// has to match ours before anything is considered.
    #[test]
    fn a_foreign_hook_with_a_dead_path_is_still_not_ours_to_repoint() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let new_exe = dir.path().join("serve.exe");
        fs::write(&new_exe, b"exists").unwrap();
        let foreign = "\"C:\\gone\\other-tool.exe\" backup";
        fs::write(
            &path,
            serde_json::to_string_pretty(&json!({
                "hooks": { "SessionStart": [ { "hooks": [ { "type": "command", "command": foreign } ] } ] }
            }))
            .unwrap(),
        )
        .unwrap();

        let report = install_hooks(&path, &standard_hooks(&new_exe.display().to_string(), "db")).unwrap();
        assert!(report.replaced.is_empty());
        let written: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert!(
            format!("{:?}", written["hooks"]["SessionStart"]).contains("other-tool.exe"),
            "somebody else's hook must survive untouched"
        );
    }

    fn server_args(db: &str) -> Vec<String> {
        vec!["--db".to_string(), db.to_string()]
    }

    /// The defect this guards against: `doctor` refuses a missing store on
    /// purpose, so the documented "check it before you install anything" step
    /// used to fail on every brand new machine. The installer creates one, and
    /// the store it creates has to be a real openable store, not a blank file.
    #[test]
    fn ensure_store_creates_one_that_actually_opens() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("fresh").join("thor.db");
        assert_eq!(ensure_store(&db).unwrap(), StoreOutcome::Created);
        assert!(db.exists(), "the store file must be there afterwards");
        thor_core::event_store::EventStore::open_existing(&db)
            .expect("the store the installer creates must open with the constructor that never creates");
    }

    /// The defect this guards against: a re-run that "makes sure" the store is
    /// there and quietly replaces a real memory with an empty one.
    #[test]
    fn ensure_store_never_touches_an_existing_one() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        ensure_store(&db).unwrap();
        let before = fs::read(&db).unwrap();

        assert_eq!(ensure_store(&db).unwrap(), StoreOutcome::AlreadyThere);
        assert_eq!(fs::read(&db).unwrap(), before, "a second run must not write a single byte");
    }

    #[test]
    fn a_fresh_file_gets_the_tool_server() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".mcp.json");
        let report =
            install_tool_server(&path, "thor", "C:\\thor2\\bin\\mcp.exe", &server_args("C:\\thor2\\thor.db")).unwrap();

        assert_eq!(report.outcome, ServerOutcome::Added);
        let written: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(written["mcpServers"]["thor"]["command"], json!("C:\\thor2\\bin\\mcp.exe"));
        assert_eq!(written["mcpServers"]["thor"]["type"], json!("stdio"));
        assert_eq!(written["mcpServers"]["thor"]["args"][1], json!("C:\\thor2\\thor.db"));
    }

    /// The defect this guards against: a re-run that rewrites an already
    /// correct file, which churns its formatting and writes a `.bak` that says
    /// something changed when nothing did.
    #[test]
    fn a_second_registration_leaves_the_file_byte_for_byte() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".mcp.json");
        install_tool_server(&path, "thor", "mcp.exe", &server_args("thor.db")).unwrap();
        let after_first = fs::read(&path).unwrap();

        let second = install_tool_server(&path, "thor", "mcp.exe", &server_args("thor.db")).unwrap();
        assert_eq!(second.outcome, ServerOutcome::AlreadyPresent);
        assert!(second.backup_path.is_none(), "nothing changed, so nothing may be backed up");
        assert_eq!(fs::read(&path).unwrap(), after_first, "an unchanged run must not rewrite the file");
    }

    /// The upgrade case, and the reason this one replaces rather than refuses:
    /// a 1.0 entry left in place would keep answering while the person believes
    /// 2.0 is installed. Replacing is right; replacing SILENTLY is not, so the
    /// old command has to come back in the report.
    #[test]
    fn an_older_registration_is_replaced_and_the_old_command_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".mcp.json");
        fs::write(
            &path,
            serde_json::to_string_pretty(&json!({
                "mcpServers": { "thor": { "command": "C:\\old\\thor.exe", "args": ["mcp"] } }
            }))
            .unwrap(),
        )
        .unwrap();

        let report = install_tool_server(&path, "thor", "C:\\thor2\\bin\\mcp.exe", &server_args("thor.db")).unwrap();
        assert_eq!(report.outcome, ServerOutcome::Replaced);
        assert_eq!(
            report.replaced.as_deref(),
            Some("C:\\old\\thor.exe"),
            "the report must name what it replaced, or the upgrade happened silently"
        );
        assert!(report.backup_path.is_some(), "replacing an entry must back the file up first");
    }

    /// The defect this guards against: an installer that writes its own
    /// `mcpServers` object wholesale deletes every other tool the person had
    /// registered, and they find out one at a time over the following days.
    #[test]
    fn registration_leaves_a_foreign_server_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".mcp.json");
        fs::write(
            &path,
            serde_json::to_string_pretty(&json!({
                "mcpServers": { "something-else": { "command": "other.exe", "args": ["run"] } },
                "unrelated": { "kept": true }
            }))
            .unwrap(),
        )
        .unwrap();

        install_tool_server(&path, "thor", "mcp.exe", &server_args("thor.db")).unwrap();
        let written: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(written["mcpServers"]["something-else"]["command"], json!("other.exe"));
        assert_eq!(written["mcpServers"]["something-else"]["args"][0], json!("run"));
        assert_eq!(written["unrelated"]["kept"], json!(true));
        assert_eq!(written["mcpServers"]["thor"]["command"], json!("mcp.exe"));
    }

    #[test]
    fn registration_refuses_invalid_json_and_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".mcp.json");
        let original = "{ not json at all ,,,";
        fs::write(&path, original).unwrap();

        assert!(install_tool_server(&path, "thor", "mcp.exe", &server_args("thor.db")).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), original, "a refused run must leave the file exactly as it was");
    }

    #[test]
    fn the_marker_writes_the_key_and_a_rerun_is_a_no_op() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(write_project_marker(dir.path(), "my-project").unwrap(), MarkerOutcome::Written);
        let marker = dir.path().join(serve::project::MARKER_FILE_NAME);
        assert_eq!(fs::read_to_string(&marker).unwrap().trim(), "my-project");
        assert_eq!(write_project_marker(dir.path(), "my-project").unwrap(), MarkerOutcome::AlreadyThere);
    }

    /// The defect this guards against: re-scoping a folder strands every fact
    /// already filed under the old key. They stay in the store and simply stop
    /// being served here, which is invisible from every surface.
    #[test]
    fn the_marker_refuses_to_rescope_a_folder() {
        let dir = tempfile::tempdir().unwrap();
        write_project_marker(dir.path(), "first-key").unwrap();

        let err = write_project_marker(dir.path(), "second-key").expect_err("re-scoping must be refused");
        let msg = err.to_string();
        assert!(msg.contains("first-key"), "the refusal must name the key already there: {msg}");
        assert!(msg.contains("second-key"), "and the one that was asked for: {msg}");
        assert_eq!(
            fs::read_to_string(dir.path().join(serve::project::MARKER_FILE_NAME)).unwrap().trim(),
            "first-key",
            "a refused re-scope must leave the marker alone"
        );
    }

    #[test]
    fn the_marker_refuses_a_blank_key() {
        let dir = tempfile::tempdir().unwrap();
        assert!(write_project_marker(dir.path(), "   ").is_err());
        assert!(!dir.path().join(serve::project::MARKER_FILE_NAME).exists());
    }

    /// The contradiction this guards against, and it would be an embarrassing
    /// one: a memory whose own write gate refuses the starting notes it ships
    /// with. Every refusal here is a real refusal, so this test is the gate
    /// judging its own contract.
    #[test]
    fn the_gate_accepts_every_starting_note() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        ensure_store(&db).unwrap();

        let seeded = seed_working_contract(&db).unwrap();
        // The notes, the starter pack and the first-session list (a lookup item).
        assert_eq!(seeded.len(), working_contract().len() + workflow_starter_pack().len() + 1);
        for s in &seeded {
            assert!(s.stored, "the gate refused its own starting note {}: {:?}", s.id, s.refusal);
        }
    }

    /// The defect this guards against: a starting note that is stored but
    /// bound to nothing fires nowhere, so a new user gets an empty session
    /// start and no sign that anything was seeded at all.
    #[test]
    fn every_starting_note_arrives_at_session_start() {
        for item in working_contract() {
            assert!(
                item.bindings.contains(&model::item::Binding::Always),
                "{} must be pinned, or it is seeded and then never seen",
                item.id
            );
            assert!(item.falsifier.is_some(), "{} must name what would prove it wrong", item.id);
            assert!(
                item.text.chars().count() <= 300,
                "{} is {} characters, over the limit the gate enforces",
                item.id,
                item.text.chars().count()
            );
        }
    }

    /// The defect this guards against: seeding on every run, which would push
    /// the starting notes into a real memory on an upgrade. The CLI only calls this
    /// for a store it just created; this proves the second call is refused
    /// rather than quietly doubling everything up.
    #[test]
    fn seeding_twice_stores_nothing_the_second_time() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        ensure_store(&db).unwrap();
        seed_working_contract(&db).unwrap();

        let again = seed_working_contract(&db).unwrap();
        for s in &again {
            assert!(!s.stored, "{} was stored a second time - the memory now holds it twice", s.id);
        }
    }

    /// The defect `ensure_and_seed_store` exists to fix: a container's entry
    /// point calling `mcp` straight against an empty volume got "no THOR
    /// store at ... this command never creates one" and nothing that speaks
    /// the tool protocol. This is the whole bootstrap in one call, proving it
    /// leaves a store with the full working contract (22 notes, matching
    /// `working_contract().len()`), the workflow starter pack
    /// (`workflow_starter_pack().len()` more) and the response-guard
    /// rulebook, not the bare file `ensure_store` alone would leave.
    #[test]
    fn ensure_and_seed_store_creates_the_notes_and_the_rulebook_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");

        let report = ensure_and_seed_store(&db).unwrap();

        assert_eq!(report.store, StoreOutcome::Created);
        assert_eq!(working_contract().len(), 22, "this test's own claim of 22 starting notes is now stale");
        // 22 working-contract notes, the starter pack, and the first-session list.
        let want = 22 + workflow_starter_pack().len() + 1;
        let stored = report.seeded.iter().filter(|s| s.stored).count();
        assert_eq!(stored, want, "expected all {want} starting notes to be stored, got {stored}");
        assert_eq!(report.rulebook.outcome, RulebookOutcome::Written);
        assert!(report.rulebook.path.exists(), "the rulebook path reported back must be the one actually written");
    }

    /// The other half of the same defect: a second start against a volume
    /// that already holds a real memory (this container's own second run, or
    /// a volume someone pointed at their existing store) must never reseed it
    /// or touch the rulebook again - see `ensure_store` and
    /// `seed_response_rulebook`'s own "only ever write into an absence"
    /// stance, which this call inherits.
    #[test]
    fn ensure_and_seed_store_leaves_an_existing_store_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        ensure_and_seed_store(&db).unwrap();
        let before = fs::read(&db).unwrap();
        let rulebook_path = respond::default_rulebook_path(&db);
        let rulebook_before = fs::read(&rulebook_path).unwrap();

        let second = ensure_and_seed_store(&db).unwrap();

        assert_eq!(second.store, StoreOutcome::AlreadyThere);
        assert!(second.seeded.is_empty(), "a store that already existed must not be seeded again");
        assert_eq!(second.rulebook.outcome, RulebookOutcome::AlreadyThere);
        assert_eq!(fs::read(&db).unwrap(), before, "a second call must not write a single byte to the store");
        assert_eq!(fs::read(&rulebook_path).unwrap(), rulebook_before, "nor to the rulebook beside it");
    }

    /// The new starter note rides the same three tests above (they iterate
    /// `working_contract()` generically), plus this one, targeted: its tags
    /// are exactly `working-contract` - since step 8 no seed carries a
    /// `no-literal:` reason, which answers nothing any more.
    #[test]
    fn the_answer_guard_note_is_present_and_carries_no_literal_tag() {
        let items = working_contract();
        let note = items
            .iter()
            .find(|i| i.id == model::store::SETUP_NOTE_ID)
            .expect("the first-session answer-guard note must be part of the working contract");
        assert_eq!(note.kind, model::item::Kind::Rule);
        assert!(note.bindings.contains(&model::item::Binding::Always));
        assert!(note.falsifier.is_some());
        assert_eq!(
            note.tags,
            vec!["working-contract".to_string()],
            "a seed carries no reason tag"
        );
    }

    /// The first-session note is the one thing a fresh agent reads before it
    /// has seen AGENTS.md, so it has to carry the way out by itself: say the
    /// defaults, and that nothing waits for an answer.
    #[test]
    fn the_first_session_note_carries_the_defaults_in_its_own_words() {
        let note = working_contract().into_iter().find(|i| i.id == model::store::SETUP_NOTE_ID).unwrap();
        let lowered = note.text.to_lowercase();
        assert!(lowered.contains("default"), "{}", note.text);
        assert!(lowered.contains("owner-setup-answers"), "{}", note.text);
        assert!(note.text.chars().count() <= 300, "{} characters", note.text.chars().count());
    }

    /// Step 8: every seed is written with only its own tag - no `no-literal:`
    /// reason (it answers nothing now) - and the one install command a seed
    /// spells out names the store it registers into (`--db`, F7).
    #[test]
    fn no_seed_carries_a_reason_tag_and_the_install_seed_names_its_store() {
        for item in working_contract() {
            assert_eq!(item.tags, vec!["working-contract".to_string()], "{}", item.id);
            assert!(model::gate::unnamed_literals(&item).is_empty(), "{}", item.id);
        }
        let install = working_contract().into_iter().find(|i| i.id == "a-new-project-is-one-command").unwrap();
        assert!(install.text.contains("`install --db <your store> --project <name>`"), "{}", install.text);
    }

    /// THE DEFECT THIS GUARDS AGAINST: a starter pack that is stored and then
    /// never speaks (bound to a moment nothing produces), or that carries the
    /// owner's own life into someone else's memory. A friend who installs
    /// THOR on one sentence gets exactly this set; it has to fire where it
    /// says and say nothing that is only true for the person who wrote it.
    #[test]
    fn the_workflow_starter_pack_is_generic_bound_and_within_the_limit() {
        let pack = workflow_starter_pack();
        assert!(pack.len() >= 12, "the starter pack shrank to {} notes", pack.len());
        let mut ids = std::collections::HashSet::new();
        let mut pinned = 0;
        for item in &pack {
            assert!(ids.insert(item.id.clone()), "{} is in the pack twice", item.id);
            assert_eq!(item.kind, model::item::Kind::Rule, "{}", item.id);
            assert!(item.project.is_none(), "{} is scoped to a project, a new user has none", item.id);
            assert_eq!(item.tags, vec!["workflow-starter".to_string()], "{}", item.id);
            assert!(item.falsifier.is_some(), "{} names nothing that would prove it wrong", item.id);
            assert!(
                item.text.chars().count() <= 300,
                "{} is {} characters, over the limit the gate enforces",
                item.id,
                item.text.chars().count()
            );
            assert!(item.text.is_ascii(), "{} holds a non-ASCII character", item.id);
            assert!(!item.bindings.is_empty(), "{} fires nowhere", item.id);
            for b in &item.bindings {
                if let model::item::Binding::Moment(a) = b {
                    assert!(!model::gate::is_dead_moment(*a), "{} binds to {a:?}, which nothing produces", item.id);
                }
            }
            if item.bindings.contains(&model::item::Binding::Always) {
                pinned += 1;
            }
            let lowered = format!("{} {}", item.text, item.falsifier.as_deref().unwrap_or("")).to_lowercase();
            for personal in [
                "c:\\users", "/home/", "10.0.", "@", "www.", "nas ", "printer", "klipper", "filament", "dutch",
            ] {
                assert!(!lowered.contains(personal), "{} mentions '{personal}', which is not generic", item.id);
            }
        }
        assert!(pinned <= 3, "{pinned} starter notes are pinned: every one of them is read at every session start");
    }

    /// The efficiency rules measured on 2026-10-09 (a main session at $166 and
    /// its helpers at $492: 65k of start context per helper, bundled tasks of
    /// 150 to 350 steps, repeated full suites, polling, re-read reports) live in
    /// the two notes bound to the Agent tool, with no model names (they go
    /// stale) and each under the 300 characters the gate allows.
    #[test]
    fn the_helper_notes_state_the_efficiency_rules_without_model_names() {
        let pack = helper_mode_notes();
        let note = |id: &str| pack.iter().find(|n| n.id == id).unwrap_or_else(|| panic!("{id} left helper mode")).text.to_lowercase();
        let orchestrator = note("orchestrator-mode-saves-quota");
        let brief = note("an-agent-brief-stands-alone");
        for (text, must) in [
            (&orchestrator, "one per task"),
            (&orchestrator, "never a bundle"),
            (&orchestrator, "no helper for: questions, trivial one-step tasks, work needing this chat or quick back-and-forth, checking helper results"),
            (&orchestrator, "chat stays free"),
            (&orchestrator, "targeted tests while working, one full run at the end"),
            (&brief, "check every 20-30 minutes, at most 3 times"),
            (&brief, "last real activity, not its status"),
            (&brief, "summary of what is known (not a read-first list)"),
        ] {
            assert!(text.contains(must), "the helper notes lost '{must}': {text}");
        }
        for text in [&orchestrator, &brief] {
            assert!(text.chars().count() <= 300, "{} characters: {text}", text.chars().count());
            for model_name in ["opus", "sonnet", "haiku", "gpt", "claude", "fable"] {
                assert!(!text.contains(model_name), "a model name goes stale: {model_name} in {text}");
            }
        }
    }

    /// Helper-agent mode is off in a new memory (owner decision, 2026-10-09):
    /// its two notes are in no seeded set, and they are as generic and as small
    /// as every other starter note.
    #[test]
    fn the_helper_notes_are_outside_every_seeded_set_and_as_clean_as_the_pack() {
        let helpers = helper_mode_notes();
        let ids: Vec<&str> = helpers.iter().map(|n| n.id.as_str()).collect();
        assert_eq!(ids, crate::questionnaire::HELPER_MODE_NOTES);
        for seeded in working_contract().into_iter().chain(workflow_starter_pack()) {
            assert!(!ids.contains(&seeded.id.as_str()), "{} is seeded into a new memory", seeded.id);
        }
        for item in &helpers {
            assert_eq!(item.kind, model::item::Kind::Rule);
            assert_eq!(item.tags, vec!["workflow-starter".to_string()]);
            assert!(item.project.is_none() && item.check.is_none() && item.severity.is_none());
            assert!(item.falsifier.is_some() && item.text.is_ascii() && item.text.chars().count() <= 300, "{}", item.id);
        }
    }

    /// A changelog and version rule depends on the project's own convention:
    /// it has to say "if the project keeps one", and it may never refuse
    /// anything, because a project without a changelog would be blocked on a
    /// file it never had.
    #[test]
    fn the_changelog_note_is_adaptive_and_has_no_tooth() {
        let note = workflow_starter_pack()
            .into_iter()
            .find(|i| i.id == "changelog-with-every-user-visible-change")
            .expect("the changelog note is part of the starter pack");
        assert!(note.text.to_lowercase().contains("if the project keeps a changelog"), "{}", note.text);
        assert!(note.check.is_none(), "a project convention is never a refusal");
        assert!(note.bindings.contains(&model::item::Binding::Moment(intent::Action::Commit)));
    }

    /// Only a check proven against real calls may ride in the pack, and the
    /// replay (see `workflow_starter_pack`'s TEETH paragraph) found none that
    /// never refuses a legitimate call. Adding one means replaying it over the
    /// recorded corpus first and listing it here by id.
    #[test]
    fn only_audited_starter_notes_carry_a_check() {
        let audited: &[&str] = &["windowless-processes-on-windows", "scheduled-task-not-interactive"];
        for item in workflow_starter_pack() {
            if item.check.is_some() {
                assert!(audited.contains(&item.id.as_str()), "{} carries a check nobody replayed", item.id);
            }
        }
    }

    /// "Geen pop cmd": on Windows an agent must never flash a console window,
    /// for any user. A new memory gets two starter rules that REFUSE, and
    /// only on a Windows install: a `Start-Process` with no hidden form and
    /// no target the user asked to see, and an interactive `schtasks /create`.
    /// Every call the user wanted visible, and every harmless one, passes.
    #[cfg(windows)]
    #[test]
    fn a_windows_install_refuses_a_console_flash_and_lets_the_wanted_calls_through() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        ensure_and_seed_store(&db).unwrap();
        let store = EventStore::open_read_only(&db).unwrap();
        let cwd = dir.path();
        let owner_said_nothing: &dyn Fn() -> Result<Vec<String>, String> = &|| Ok(Vec::new());
        let verdict = |tool: &str, command: &str| {
            let input = serde_json::json!({ "command": command });
            serve::guard_verdict::pre_tool(&store, tool, Some(&input), None, Some(cwd), Some(cwd), Some(owner_said_nothing))
                .verdict
                .map(|v| v.rule_id.unwrap_or_default())
        };
        for refused in [
            "Start-Process cmd.exe -ArgumentList '/c x'",
            "Start-Process -FilePath python -ArgumentList 'job.py'",
            "Start-Process notepad.exe",
        ] {
            assert_eq!(verdict("PowerShell", refused).as_deref(), Some("windowless-processes-on-windows"), "{refused}");
        }
        assert_eq!(
            verdict("Bash", "powershell -Command \"Start-Process cmd.exe -ArgumentList '/c x'\"").as_deref(),
            Some("windowless-processes-on-windows"),
            "the same call through the Bash tool"
        );
        for passes in [
            "Start-Process cmd.exe -ArgumentList '/c x' -WindowStyle Hidden",
            "Start-Process cmd.exe -ArgumentList '/c x' -NoNewWindow -Wait",
            "Start-Process ms-settings:windowsdefender",
            "Start-Process windowsdefender:",
            "Start-Process control.exe mmsys.cpl",
            "Start-Process https://example.com/docs",
            "Start-Process http://localhost:3000",
            "Start-Process msedge \"file:///C:/work/demo.html\"",
            "Start-Process cmd.exe -ArgumentList '/c x' -WhatIf",
            "Get-Process | Where-Object Name -eq cmd",
        ] {
            assert_eq!(verdict("PowerShell", passes), None, "{passes}");
        }
        let task = "schtasks /create /tn x /tr \"wscript.exe C:\\x.vbs\" /sc once /st 23:59";
        assert_eq!(
            verdict("PowerShell", &format!("{task} /it /ru me /f")).as_deref(),
            Some("scheduled-task-not-interactive"),
            "an interactive task flashes a window"
        );
        for refused in [format!("{task} /IT"), format!("{task} /it"), "schtasks.exe /create /tn x /tr y /sc daily /it".to_string()] {
            assert_eq!(verdict("PowerShell", &refused).as_deref(), Some("scheduled-task-not-interactive"), "{refused}");
        }
        for passes in [task, "schtasks /query /tn x /v /fo LIST", "schtasks /run /tn x", "schtasks /create /tn x /tr \"C:\\it\\run.vbs\" /sc daily"] {
            assert_eq!(verdict("PowerShell", passes), None, "{passes}");
        }
    }

    /// Off Windows the same two notes stay, as plain notes: nothing there
    /// can flash a console window, and a rule that can refuse must never
    /// ride into an install it does not describe.
    #[test]
    fn off_windows_the_console_flash_notes_carry_no_check() {
        for id in ["windowless-processes-on-windows", "scheduled-task-not-interactive"] {
            let on = workflow_starter_pack_for(true);
            let off = workflow_starter_pack_for(false);
            let on = on.iter().find(|n| n.id == id).unwrap_or_else(|| panic!("{id} left the pack"));
            let off = off.iter().find(|n| n.id == id).unwrap_or_else(|| panic!("{id} left the off-Windows pack"));
            assert!(on.check.is_some(), "{id} lost its check on Windows");
            assert!(off.check.is_none(), "{id} refuses on a platform with no console window to flash");
            assert_eq!(on.text, off.text, "{id}: the note says the same on every platform");
        }
    }

    /// A block shows at most four notes (`model::item::MAX_ITEMS`) and a
    /// command place holds at most four, so the pack must not crowd one
    /// moment or command past what a block can carry in one showing, and must
    /// leave the owner's own notes room on every command it binds.
    #[test]
    fn the_starter_pack_does_not_crowd_a_place() {
        use std::collections::HashMap;
        let mut per_place: HashMap<String, usize> = HashMap::new();
        for item in workflow_starter_pack() {
            for b in &item.bindings {
                *per_place.entry(format!("{b:?}")).or_default() += 1;
            }
        }
        for (place, n) in &per_place {
            let cap = if place.starts_with("Target") { 2 } else { model::item::MAX_ITEMS };
            assert!(*n <= cap, "{place} holds {n} starter notes, over {cap}");
        }
    }

    /// What one moment shows is capped at four notes AND 1200 characters
    /// (`serve::render::cap`): the starter notes at one moment have to fit
    /// together, or the last one waits for a later showing and may never
    /// come. No starter note carries a weight either, so none can crowd out
    /// the owner's own heavier ones, and doctor has nothing to call toothless.
    #[test]
    fn a_moments_starter_notes_fit_one_block() {
        use std::collections::HashMap;
        let mut per_moment: HashMap<String, usize> = HashMap::new();
        for item in workflow_starter_pack() {
            assert!(item.severity.is_none(), "{} carries a weight, so doctor calls it toothless", item.id);
            for b in &item.bindings {
                if let model::item::Binding::Moment(a) = b {
                    *per_moment.entry(format!("{a:?}")).or_default() += item.text.chars().count();
                }
            }
        }
        for (moment, chars) in &per_moment {
            assert!(*chars <= serve::render::MAX_BLOCK_CHARS, "{moment}: {chars} characters, over one block");
        }
    }

    /// Every starter note is accepted by the gate into a store that already
    /// holds the working contract, and is then served at the place it names:
    /// a rule bound to a moment nothing fires there would be stored and mute.
    #[test]
    fn a_starter_note_is_served_at_its_own_moment() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        ensure_and_seed_store(&db).unwrap();
        let store = EventStore::open_read_only(&db).unwrap();
        let live = serve::live::live_items(&store);
        for item in workflow_starter_pack() {
            let mut input = serve::input::ServeInput::default();
            for b in &item.bindings {
                match b {
                    model::item::Binding::Moment(a) => input.add_moment(*a),
                    model::item::Binding::Target { kind: model::item::TargetKind::Command, value } => {
                        input.add_command(value)
                    }
                    _ => {}
                }
            }
            let ranked = serve::rank::select(&live, &input);
            assert!(ranked.iter().any(|r| r.id == item.id), "{} is not served at the place it names", item.id);
        }
    }

    /// THE GAP THIS CLOSES: a fresh store got the `Stop` hook wired in from
    /// the very first run, but no rulebook for it to read - `respond::block_reason`
    /// fails open on a missing file BY DESIGN, so the guard ran on every
    /// reply and blocked nothing, silently, until someone found the example
    /// file in the repository and copied it over by hand.
    #[test]
    fn seed_response_rulebook_writes_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        ensure_store(&db).unwrap();

        let report = seed_response_rulebook(&db).unwrap();
        assert_eq!(report.outcome, RulebookOutcome::Written);
        assert_eq!(report.path, respond::default_rulebook_path(&db));
        assert!(report.path.exists());
        assert_eq!(fs::read_to_string(&report.path).unwrap(), RESPONSE_RULEBOOK_TEMPLATE);
    }

    /// The defect this guards against: an installer that "helpfully"
    /// refreshes the rulebook on every run would silently overwrite an
    /// owner's own tuned rules the next time a binary gets rebuilt.
    #[test]
    fn seed_response_rulebook_leaves_an_existing_file_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        ensure_store(&db).unwrap();
        let path = respond::default_rulebook_path(&db);
        let owners_own = r#"[{"id":"owners-own-rule","any_of":["x"],"reminder":"mine"}]"#;
        fs::write(&path, owners_own).unwrap();

        let report = seed_response_rulebook(&db).unwrap();
        assert_eq!(report.outcome, RulebookOutcome::AlreadyThere);
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            owners_own,
            "an existing rulebook must never be overwritten, seeded or not"
        );
    }

    /// `doctor_invocation`'s own two platform forms, proven directly with
    /// fixed inputs rather than through `cfg!(windows)` (which bakes into
    /// whichever platform compiled this test binary and so could never let a
    /// single suite prove the other form was even reachable) - see the
    /// function's own doc comment for why `windows` is a parameter rather
    /// than read from `cfg!` internally. Mirrors `serve::render`'s own
    /// `the_windows_form_leads_with_the_call_operator_and_quotes_both_paths`,
    /// the same pattern for the same reason.
    #[test]
    fn doctor_invocation_windows_form_uses_the_call_operator_backslash_and_exe_suffix() {
        assert_eq!(
            doctor_invocation(Path::new("C:\\thor2\\bin"), Path::new("C:\\thor2\\thor.db"), true),
            "& \"C:\\thor2\\bin\\doctor.exe\" --db \"C:\\thor2\\thor.db\""
        );
    }

    #[test]
    fn doctor_invocation_non_windows_form_uses_a_forward_slash_and_no_suffix() {
        assert_eq!(
            doctor_invocation(Path::new("/home/user/thor2/bin"), Path::new("/home/user/thor2/thor.db"), false),
            "\"/home/user/thor2/bin/doctor\" --db \"/home/user/thor2/thor.db\""
        );
    }

    /// `doctor_allow_invocation`'s own Windows form: forward slashes even
    /// though `bin_dir` itself carries backslashes, plus the `.exe` suffix -
    /// see that function's own doc comment for why a `Bash(...)` allow-list
    /// entry can never carry a literal backslash.
    /// The Bash form must be one the `Bash(...)` allow entry pre-approves:
    /// the command starts with the entry's prefix (the entry minus its
    /// `Bash(` and `:*)`), on both platform forms.
    #[test]
    fn doctor_bash_invocation_is_covered_by_the_allow_entry_on_both_platforms() {
        for (bin, db, windows) in [
            ("C:\\thor2\\bin", "C:\\thor2\\thor.db", true),
            ("/home/user/thor2/bin", "/home/user/thor2/thor.db", false),
        ] {
            let command = doctor_bash_invocation(Path::new(bin), Path::new(db), windows);
            let entry = doctor_allow_invocation(Path::new(bin), windows);
            let prefix = entry.strip_prefix("Bash(").and_then(|e| e.strip_suffix(":*)")).expect("a Bash entry");
            assert!(command.starts_with(prefix), "{command} is not covered by {entry}");
            assert!(!command.starts_with('&') && !command.contains('\\'), "no PowerShell form: {command}");
        }
        assert_eq!(
            doctor_bash_invocation(Path::new("C:\\thor2\\bin"), Path::new("C:\\thor2\\thor.db"), true),
            "C:/thor2/bin/doctor.exe --db C:/thor2/thor.db"
        );
    }

    /// The fast form is the same call with `--eval` after it, and the Bash
    /// form is still covered by the one `Bash(<doctor>:*)` entry the routine
    /// pre-approves - no permission prompt may stand between the reader and
    /// the numbers it has to wait for.
    #[test]
    fn the_fast_doctor_forms_end_in_eval_and_the_bash_one_is_pre_approved() {
        for (bin, db, windows) in [
            ("C:\\thor2\\bin", "C:\\thor2\\thor.db", true),
            ("/home/user/thor2/bin", "/home/user/thor2/thor.db", false),
        ] {
            let (bin, db) = (Path::new(bin), Path::new(db));
            assert_eq!(doctor_eval_invocation(bin, db, windows), format!("{} --eval", doctor_invocation(bin, db, windows)));
            let command = doctor_eval_bash_invocation(bin, db, windows);
            assert_eq!(command, format!("{} --eval", doctor_bash_invocation(bin, db, windows)));
            let entry = doctor_allow_invocation(bin, windows);
            let prefix = entry.strip_prefix("Bash(").and_then(|e| e.strip_suffix(":*)")).expect("a Bash entry");
            assert!(command.starts_with(prefix), "{command} is not covered by {entry}");
        }
        assert_eq!(
            doctor_eval_bash_invocation(Path::new("C:\\thor2\\bin"), Path::new("C:\\thor2\\thor.db"), true),
            "C:/thor2/bin/doctor.exe --db C:/thor2/thor.db --eval"
        );
    }

    /// The rendered routine tells a reader to wait for the fast form, names
    /// both of its commands, and still names the full check beside it.
    #[test]
    fn the_rendered_routine_carries_the_fast_doctor_form_and_the_full_one() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("thor-eval.md");
        let (bin_dir, db) = (Path::new("/fake/thor2/bin"), Path::new("/fake/thor2/thor.db"));
        seed_eval_command(&path, bin_dir, db).unwrap();
        let written = fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = written.lines().map(str::trim).collect();
        let eval = doctor_eval_invocation(bin_dir, db, cfg!(windows));
        let eval_bash = doctor_eval_bash_invocation(bin_dir, db, cfg!(windows));
        assert!(lines.contains(&eval.as_str()), "the fast PowerShell form must stand on a line of its own: {eval}");
        assert!(lines.contains(&eval_bash.as_str()), "the fast Bash form must stand on a line of its own: {eval_bash}");
        assert!(lines.contains(&doctor_invocation(bin_dir, db, cfg!(windows)).as_str()), "the full check stays in the routine");
        assert!(lines.contains(&doctor_bash_invocation(bin_dir, db, cfg!(windows)).as_str()), "the full Bash form stays in the routine");
        assert!(written.contains("RUN IT IN THE FOREGROUND"), "the routine must tell the reader to wait for the fast form");
    }

    #[test]
    fn doctor_bash_invocation_quotes_a_path_with_whitespace() {
        assert_eq!(
            doctor_bash_invocation(Path::new("C:\\John Smith\\bin"), Path::new("C:\\John Smith\\thor.db"), true),
            "\"C:/John Smith/bin/doctor.exe\" --db \"C:/John Smith/thor.db\""
        );
    }

    #[test]
    fn doctor_allow_invocation_windows_form_uses_forward_slashes_and_exe_suffix() {
        assert_eq!(doctor_allow_invocation(Path::new("C:\\thor2\\bin"), true), "Bash(C:/thor2/bin/doctor.exe:*)");
    }

    #[test]
    fn doctor_allow_invocation_non_windows_form_uses_no_suffix() {
        assert_eq!(doctor_allow_invocation(Path::new("/home/user/thor2/bin"), false), "Bash(/home/user/thor2/bin/doctor:*)");
    }

    /// Proves the real wiring on THIS host: `render_eval_command` passes the
    /// real `cfg!(windows)` (not a caller-chosen value) into
    /// `doctor_invocation`, so a Windows build's own rendered eval command
    /// actually carries the `&`-prefixed, `.exe`-suffixed PowerShell form -
    /// not merely that the pure formatter can produce it when asked (see the
    /// two form tests above). Mirrors `serve::render`'s own
    /// `on_windows_the_real_hint_leads_with_the_ampersand_call_operator`.
    #[cfg(windows)]
    #[test]
    fn on_windows_the_real_doctor_line_leads_with_the_call_operator_and_exe_suffix() {
        let rendered = render_eval_command(Path::new("C:\\thor2\\bin"), Path::new("C:\\thor2\\thor.db"));
        // `l.contains("doctor")` alone would also match the frontmatter's own
        // `allowed-tools` line since `{{THOR_DOCTOR_ALLOW}}` was added there
        // too (section 2 of this task), and the retest entry carries `--db` -
        // so the frontmatter line is left out, which narrows this to the
        // actual invocation line the report tells a reader to run.
        let line = rendered
            .lines()
            .find(|l| l.contains("doctor") && l.contains("--db") && !l.starts_with("allowed-tools:") && !l.ends_with("--eval"))
            .expect("the rendered eval command must carry a line naming doctor");
        let trimmed = line.trim();
        assert!(trimmed.starts_with("& \""), "the windows doctor line must start with the call operator: {trimmed}");
        assert!(
            trimmed.ends_with(".exe\" --db \"C:\\thor2\\thor.db\""),
            "the windows doctor line must end with the exe suffix and --db: {trimmed}"
        );
    }

    /// The identical real-wiring proof as the test above, for the
    /// `allowed-tools` entry: `render_eval_command` passes the real
    /// `cfg!(windows)` into `doctor_allow_invocation` too.
    #[cfg(windows)]
    #[test]
    fn on_windows_the_real_doctor_allow_entry_uses_forward_slashes_and_exe_suffix() {
        let rendered = render_eval_command(Path::new("C:\\thor2\\bin"), Path::new("C:\\thor2\\thor.db"));
        assert!(rendered.contains("Bash(C:/thor2/bin/doctor.exe:*)"), "{rendered}");
    }

    /// THE GAP THIS GUARDS AGAINST: a template shipped with `{{THOR_DOCTOR}}`
    /// still sitting in it is worse than no file at all - it reads as
    /// finished and silently tells a new user to run a command that can
    /// never resolve. The placeholder must be gone, replaced by the real
    /// doctor invocation this run resolved, which itself names both the bin
    /// directory and the store path.
    #[test]
    fn seed_eval_command_writes_with_the_doctor_placeholder_substituted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("commands").join("thor-eval.md");
        let bin_dir = Path::new("C:\\fake\\thor2\\bin");
        let db = Path::new("C:\\fake\\thor2\\thor.db");

        let report = seed_eval_command(&path, bin_dir, db).unwrap();
        assert_eq!(report.outcome, EvalCommandOutcome::Written);
        assert_eq!(report.path, path);
        let written = fs::read_to_string(&path).unwrap();
        assert!(written.contains("C:\\fake\\thor2\\bin"), "the programs folder must be substituted in: {written}");
        assert!(written.contains("C:\\fake\\thor2\\thor.db"), "the store path must be substituted in: {written}");
        assert!(!written.contains("{{"), "no placeholder may survive substitution: {written}");
    }

    /// THE GAP SECTION 2 OF THIS TASK CLOSES (owner's decision, 2026-09-17):
    /// the shipped evaluation command must pre-approve Bash only per
    /// command, never a bare `Bash` that would let ANY shell command through
    /// unasked during an evaluation, including a wrong or whispered one.
    /// Proven on the rendered file's own `allowed-tools` line, entry by
    /// entry (never a substring search over the whole file, which a bare
    /// `Bash` sitting right next to a scoped `Bash(...)` entry could pass by
    /// accident), so a future edit that reintroduces one anywhere in the
    /// line is still caught regardless of how the rest of it changes.
    #[test]
    fn rendered_eval_command_never_pre_approves_a_bare_bash() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("thor-eval.md");
        seed_eval_command(&path, Path::new("C:\\fake\\thor2\\bin"), Path::new("C:\\fake\\thor2\\thor.db")).unwrap();
        let written = fs::read_to_string(&path).unwrap();

        let allowed_tools_line =
            written.lines().find(|l| l.starts_with("allowed-tools:")).expect("the frontmatter must carry an allowed-tools line");
        let entries: Vec<&str> = allowed_tools_line.trim_start_matches("allowed-tools:").split(',').map(str::trim).collect();

        assert!(!entries.contains(&"Bash"), "a bare Bash entry pre-approves every shell command unasked: {allowed_tools_line}");
        assert!(
            entries.iter().any(|e| e.starts_with("Bash(")),
            "at least one scoped Bash(...) entry must still be present: {allowed_tools_line}"
        );
        assert!(entries.iter().any(|e| e.contains("doctor")), "the doctor entry must be present: {allowed_tools_line}");
        assert!(!allowed_tools_line.contains("{{"), "no placeholder may survive substitution: {allowed_tools_line}");
    }

    /// C3b E1: the retest command the Stop hook prints for a rot or teeth
    /// hold must be one the seeded routine's own `allowed-tools` allows, or a
    /// headless session is refused again and again and cannot run it. Proven
    /// on the rendered routine: the printed command (with `--apply` too)
    /// starts with the text of one of its `Bash(...)` entries.
    #[test]
    fn the_printed_retest_command_matches_an_allowed_tools_entry_of_the_seeded_routine() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("thor-eval.md");
        let bin_dir = Path::new("C:\\fake\\thor2\\bin");
        let db = Path::new("C:\\fake\\thor2\\thor.db");
        seed_eval_command(&path, bin_dir, db).unwrap();
        let written = fs::read_to_string(&path).unwrap();
        let line = written.lines().find(|l| l.starts_with("allowed-tools:")).expect("an allowed-tools line");
        let entries: Vec<&str> = line.trim_start_matches("allowed-tools:").split(", ").map(str::trim).collect();
        let allows = |command: &str| {
            entries.iter().any(|e| {
                e.strip_prefix("Bash(").and_then(|e| e.strip_suffix(":*)")).is_some_and(|prefix| command.starts_with(prefix))
            })
        };

        let printed = model::store::retest_command_with(&serve_program(bin_dir), Some(db), "unanswered-rule");
        assert!(allows(&printed), "the printed command {printed} matches no entry of: {line}");
        assert!(allows(&format!("{printed} --apply")), "the same command with --apply: {printed}");
        assert!(!printed.starts_with('&') && !printed.contains('\\'), "no PowerShell form: {printed}");
        assert!(!allows("C:/fake/thor2/bin/serve.exe --db C:/other/thor.db retest --id 'x'"), "another store is not allowed");
    }

    /// The defect this guards against: an installer that "refreshes" the
    /// eval command on every run would silently overwrite an owner's own
    /// edited copy the next time a binary gets rebuilt - the same failure
    /// `seed_response_rulebook_leaves_an_existing_file_untouched` guards
    /// against for the rulebook.
    #[test]
    fn seed_eval_command_leaves_an_edited_copy_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("thor-eval.md");
        let owners_own = "the owner's own edited routine\n";
        fs::write(&path, owners_own).unwrap();

        let report = seed_eval_command(&path, Path::new("bin"), Path::new("thor.db")).unwrap();
        assert_eq!(report.outcome, EvalCommandOutcome::AlreadyThere);
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            owners_own,
            "an edited copy must never be overwritten, seeded or not"
        );
    }

    /// Required to actually prove the seeded file is usable, not merely
    /// present: parsed with the guard's own type (`respond::parse_opt_in_rules`),
    /// never hand-checked as JSON, so a shape only respond.rs itself would
    /// reject is caught here too.
    #[test]
    fn the_seeded_rulebook_parses_with_the_guards_own_rule_type() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        ensure_store(&db).unwrap();
        let report = seed_response_rulebook(&db).unwrap();

        let text = fs::read_to_string(&report.path).unwrap();
        let rules = respond::parse_opt_in_rules(&text);
        assert_eq!(rules.len(), 6, "the six shipped rules must all parse");

        let ids: Vec<&str> = rules.iter().map(|r| r.base.id.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                "no-plain-language-tldr",
                "ask-user-to-check-or-fetch",
                "no-reflexive-disclaimers",
                "answer-is-too-long",
                "checked-claim-needs-evidence",
                "absolute-verification-claim",
            ]
        );

        let tldr = rules.iter().find(|r| r.base.id == "no-plain-language-tldr").unwrap();
        assert_eq!(tldr.base.min_chars, 600);
        assert!(tldr.base.any_of.contains(&"gepusht".to_string()));
        assert!(tldr.base.none_of.contains(&"tldr".to_string()));

        let length_rule = rules.iter().find(|r| r.base.id == "answer-is-too-long").unwrap();
        assert_eq!(length_rule.base.min_chars, 1000);
        assert!(
            !length_rule.list_request_any_of.is_empty(),
            "the length rule must keep its list exemption"
        );

        let evidence = rules.iter().find(|r| r.base.id == "checked-claim-needs-evidence").unwrap();
        assert!(evidence.none_of_patterns.contains(&"commit_sha".to_string()));
        assert!(evidence.none_of_patterns.contains(&"path_line".to_string()));
        assert!(!evidence.base.reminder.is_empty());

        let absolute = rules.iter().find(|r| r.base.id == "absolute-verification-claim").unwrap();
        assert!(absolute.base.any_of.contains(&"alles nagekeken".to_string()));
        assert!(!absolute.backed_by_turn_tools, "it must fire even when the turn looked at something");
    }

    /// A real checkout at `dir` with one commit, named `key` by its own
    /// marker. Its hooks point at an empty folder right after `git init`, so
    /// the machine's shared git hooks never act on a throwaway repository.
    fn checkout(dir: &Path, key: &str) {
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git").arg("-C").arg(dir).args(args).output().expect("git on PATH");
            assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        };
        let hooks = dir.parent().unwrap().join("no-hooks");
        fs::create_dir_all(dir).unwrap();
        fs::create_dir_all(&hooks).unwrap();
        git(&["init", "--quiet"]);
        git(&["config", "core.hooksPath", &hooks.display().to_string()]);
        git(&["config", "user.email", "fixture@example.invalid"]);
        git(&["config", "user.name", "fixture"]);
        fs::write(dir.join(serve::project::MARKER_FILE_NAME), format!("{key}\n")).unwrap();
        git(&["add", "-A"]);
        git(&["commit", "--quiet", "-m", "initial"]);
    }

    /// THE DEFECT `record_project_opened` CLOSES: a successful code read
    /// opened a code-search sidecar under the project's name but never told
    /// the main store, so `mcp::refuse_a_new_collection` never saw that name
    /// as open. Proven directly here (id, kind, project, tags, text, and the
    /// registered working copy spliced onto the body); the gate-level
    /// consequence (a `remember` under this project now passes the
    /// collection check) is proven in `ops/tests/install_records_project_opened.rs`.
    #[test]
    fn a_successful_read_writes_the_record_with_the_exact_shape() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        ensure_store(&db).unwrap();
        let repo = dir.path().join("demo-shop");
        checkout(&repo, "demo-shop");

        let outcome = record_project_opened(&db, &repo, "demo-shop", 7).unwrap();
        assert_eq!(outcome, RegisterOutcome::Created { root: root_string(&repo) });

        let store = thor_core::event_store::EventStore::open_existing(&db).unwrap();
        let item = model::store::show(&store, "project-demo-shop-opened-by-install").unwrap();
        assert_eq!(item.kind, model::item::Kind::Report);
        assert_eq!(item.project.as_deref(), Some("demo-shop"));
        assert_eq!(item.tags, vec!["working-contract".to_string(), "project-record".to_string()]);
        assert!(item.bindings.is_empty(), "a Report may carry no binding");
        assert!(item.falsifier.is_none(), "no falsifier was asked for");
        let today = today_ymd();
        assert_eq!(
            item.text,
            format!(
                "Project demo-shop: install read its code on {today} (7 file(s)) so it can be \
                 searched with search_code and kept fresh on every commit. Notes filed under \
                 'demo-shop' belong to this repository."
            )
        );
        let events = store.get_events_by_entity("project-demo-shop-opened-by-install").unwrap();
        assert_eq!(events.len(), 1, "created carrying its root in one event");
        assert_eq!(events[0].actor, "installer");
        let body: serde_json::Value = serde_json::from_str(&events[0].body).unwrap();
        assert_eq!(body["root"], serde_json::Value::String(root_string(&repo)));
        assert!(body["root_at"].as_str().is_some_and(|t| t.starts_with(&today)));
        assert!(!root_string(&repo).contains('\\'), "a registered root uses forward slashes");
    }

    /// A second call for the same project from the same checkout must write
    /// nothing more: no revision, no duplicate. Checked by event count, not
    /// merely by the returned outcome, so a bug that revised the item instead
    /// of skipping it would still be caught.
    #[test]
    fn a_second_call_for_the_same_project_writes_nothing_more() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        ensure_store(&db).unwrap();
        let repo = dir.path().join("demo-shop");
        checkout(&repo, "demo-shop");

        record_project_opened(&db, &repo, "demo-shop", 7).unwrap();
        let store = thor_core::event_store::EventStore::open_existing(&db).unwrap();
        let before = store.get_all_events().unwrap().len();

        let again = record_project_opened(&db, &repo, "demo-shop", 9999).unwrap();
        assert_eq!(again, RegisterOutcome::AlreadyHere { root: root_string(&repo) });

        let store = thor_core::event_store::EventStore::open_existing(&db).unwrap();
        assert_eq!(store.get_all_events().unwrap().len(), before, "a second call appended an event");
        let item = model::store::show(&store, "project-demo-shop-opened-by-install").unwrap();
        assert!(item.text.contains("(7 file(s))"), "the original record must be untouched: {}", item.text);
    }

    /// The other half of idempotency: two DIFFERENT projects each get their
    /// own record, because the id carries the project's own name.
    #[test]
    fn two_different_projects_each_get_their_own_record() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        ensure_store(&db).unwrap();
        let shop = dir.path().join("demo-shop");
        checkout(&shop, "demo-shop");
        let other = dir.path().join("other-repo");
        checkout(&other, "other-repo");

        assert!(matches!(record_project_opened(&db, &shop, "demo-shop", 3), Ok(RegisterOutcome::Created { .. })));
        assert!(matches!(record_project_opened(&db, &other, "other-repo", 5), Ok(RegisterOutcome::Created { .. })));

        let store = thor_core::event_store::EventStore::open_existing(&db).unwrap();
        assert!(model::store::show(&store, "project-demo-shop-opened-by-install").is_ok());
        assert!(model::store::show(&store, "project-other-repo-opened-by-install").is_ok());
    }

    /// A record written before registration existed (no root) gains its
    /// working copy through ONE revise by the installer, text untouched.
    #[test]
    fn a_record_without_a_root_gains_one_revise_by_the_installer() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        ensure_store(&db).unwrap();
        let repo = dir.path().join("demo-shop");
        checkout(&repo, "demo-shop");
        {
            let mut store = EventStore::new(&db).unwrap();
            let old = project_record(project_opened_record_id("demo-shop"), "demo-shop", Some(4), "2026-08-01");
            model::store::declare(&mut store, "install", "install", installer(), &old).unwrap();
        }

        let outcome = record_project_opened(&db, &repo, "demo-shop", 9).unwrap();
        assert_eq!(outcome, RegisterOutcome::Registered { root: root_string(&repo) });
        let store = EventStore::open_existing(&db).unwrap();
        let history = model::store::history(&store, "project-demo-shop-opened-by-install").unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(history[1].kind, "fact_revised");
        assert_eq!(history[1].actor, "installer");
        assert!(history[1].item.as_ref().unwrap().text.contains("(4 file(s))"), "the text is kept");
        assert_eq!(
            model::store::registered_root_raw(&store, "demo-shop").map(|(p, _)| root_string(&p)),
            Some(root_string(&repo))
        );
    }
}
