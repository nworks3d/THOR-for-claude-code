//! The decision half of every PreToolUse guard, in one place: what the hook
//! refuses, and why, for one tool call against one store.
//!
//! WHY THIS IS ITS OWN MODULE. The three guards used to be decided inside the
//! hook binary (`bin/serve.rs`), where nothing else could reach them. A rule's
//! trial at the moment it is written has to try exactly what the hook would do
//! with a call, and a second copy of the pipeline for the trial would drift
//! from the hook the first time either changed. So the pipeline lives here,
//! and the hook is one caller of it: `pre_tool` chains the arms in hook order,
//! and the hook itself keeps only what is not a decision - the once-per-session
//! markers that change a repeat's wording, the refusal record in the log
//! (`record_gate`), the staleness sidecar, and the JSON it prints.
//!
//! NOTHING HERE WRITES. Every function reads the store and the files a check
//! names, and returns. A refusal comes back as a [`Verdict`] worded exactly as
//! a first attempt reads; the hook turns it into a repeat's wording when its
//! marker says so. The candidate pools the arms fetched come back beside the
//! verdict ([`Pools`]) - the hook records staleness from the SAME pools the
//! decision was made on, never from a second fetch - and they come back
//! whether or not anything was refused, because staleness is recorded on
//! every call that reaches an arm, refused or not.
//!
//! HOOK ORDER (`pre_tool`). The file write first (`file_write`: an Edit/Write
//! carrying content), then the command (`command`: shell writes, then a
//! forbidden literal in the command itself), then `requires` (the one arm that
//! catches an omission). The first arm with a refusal wins and the rest do not
//! run. In every real payload the first two never overlap (a Write/Edit
//! carries a file path and no command, a Bash call the opposite), so the order
//! between them only matters for a hand-built payload, where the older,
//! file-based guard keeps its precedence. `requires` runs last so an actual
//! forbidden fragment - the more specific complaint - still wins.
//!
//! None of these arms is gated on whether the call comes from a subagent: they
//! judge the write or command about to happen, never a stated decision, and a
//! subagent's write can break an anchored rule exactly as easily as the main
//! session's.

use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use thor_core::event_store::EventStore;

use crate::absent_guard;
use crate::input::{self, ServeInput, ShellWriteEffect};
use crate::live::{self, LiveItem};
use crate::project;
use crate::rank::{self, RankedItem};
use crate::shell::{self, Segment};
use model::item::TargetKind;

/// One refusal, as it reads on a first attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    /// The rule the refusal names, read back out of its own wording
    /// (`absent_guard::rule_id_of`). A repeat's wording keeps the first line,
    /// so this is the same id either way.
    pub rule_id: Option<String>,
    /// The refusal as the guard words it on a first attempt.
    pub reason: String,
    /// What the refusal is filed under in the log: the file for a file write,
    /// the command for a shell call, and for `requires` the command, or the
    /// bare tool name when the call carries no command.
    pub target: String,
    /// Which once-per-session marker decides whether this is a repeat, and
    /// under which key. `None`: the refusal reads the same every time.
    pub repeat: Option<RepeatKey>,
}

/// The marker a refusal is remembered in, with the attempt's key
/// (`absent_guard::attempt_key`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepeatKey {
    /// The file guard's marker (`absent_guard::default_marker_path`): the file
    /// plus the text the call carries.
    File(String),
    /// The command guard's marker (`absent_guard::default_command_marker_path`):
    /// the matched anchor plus the command.
    Command(String),
}

/// The candidate pools the arms fetched, handed back for the staleness
/// sidecar. An arm that never got as far as fetching leaves its pool `None`,
/// and the hook then records nothing for it - exactly as when the hook
/// decided these itself.
#[derive(Default)]
pub struct Pools {
    pub file: Option<FilePools>,
    pub command: Option<CommandPool>,
}

/// The file arm's two pools: the kind-narrowed location pool (also the
/// directory-content and must-remain arms' pool) and the ranked content pool.
pub struct FilePools {
    pub file_path: String,
    pub location: Vec<LiveItem>,
    pub ranked: Vec<RankedItem>,
}

/// The command arm's pool, with no project scope applied (each segment is
/// scoped on its own, inside the decision): whether an anchor has rotted is
/// true of the store, not of which checkout happens to be asking. The call's
/// segments come back with it, so the staleness sidecar reads the same reach.
pub struct CommandPool {
    pub command: String,
    pub items: Vec<LiveItem>,
    pub segments: Vec<Segment>,
}

/// A call that carries a command, cut ONCE into the simple commands it runs
/// (`shell::segments`, in the dialect of its tool), with the project each of
/// them runs in (`segment_projects`). Every arm reads this one cut.
pub struct Call {
    pub segments: Vec<Segment>,
    pub projects: Vec<Option<String>>,
}

impl Call {
    /// The call `tool_input` carries, or `None` when it carries no command.
    pub fn of(
        live: &Live,
        tool_name: &str,
        tool_input: Option<&Value>,
        project: Option<&str>,
        cwd: Option<&Path>,
    ) -> Option<Call> {
        let command = absent_guard::proposed_command(tool_input)?;
        let segments = shell::call_segments(tool_name, command);
        let projects = segment_projects(live, &segments, cwd, project);
        Some(Call { segments, projects })
    }
}

/// What came of one call: the refusal, if any, and the pools behind it.
#[derive(Default)]
pub struct Judgement {
    pub verdict: Option<Verdict>,
    pub pools: Pools,
}

/// Every PreToolUse guard, in hook order, for one tool call - the three
/// values the hook resolves from the payload's working directory:
/// - `project`: the session's project (`project::session_project` - the
///   folder's own, else the one the session last worked in);
/// - `root`: the folder every check path resolves against, the top of the
///   checkout the session works in (`project::guard_root` - after a drift
///   into a folder of no project, the registered checkout of the project the
///   session kept);
/// - `cwd`: the session's working folder itself, which a RELATIVE path in a
///   call is relative to (`judged_path`).
pub fn pre_tool(
    store: &EventStore,
    tool_name: &str,
    tool_input: Option<&Value>,
    project: Option<&str>,
    root: Option<&Path>,
    cwd: Option<&Path>,
    owner: Option<absent_guard::OwnerWords>,
) -> Judgement {
    pre_tool_in(&Live::of(store), tool_name, tool_input, project, root, cwd, owner)
}

/// `pre_tool` for a caller that judges many calls against one store (a replay
/// of real calls, `sweep`): the store's live items are read once into `live`
/// and shared, instead of once per call. The decision is the same function.
pub fn pre_tool_in(
    live: &Live,
    tool_name: &str,
    tool_input: Option<&Value>,
    project: Option<&str>,
    root: Option<&Path>,
    cwd: Option<&Path>,
    owner: Option<absent_guard::OwnerWords>,
) -> Judgement {
    let file_path = tool_input.and_then(|t| t.get("file_path")).and_then(|v| v.as_str());
    let mut judged = file_write_in(live, tool_name, file_path, tool_input, project, root, cwd);
    if judged.verdict.is_some() {
        return judged;
    }
    let call = Call::of(live, tool_name, tool_input, project, cwd);
    let by_command = command_in(live, tool_input, call.as_ref(), root, cwd);
    judged.pools.command = by_command.pools.command;
    if by_command.verdict.is_some() {
        judged.verdict = by_command.verdict;
        return judged;
    }
    judged.verdict = requires_in(live, tool_name, tool_input, call.as_ref(), project, owner);
    judged
}

/// The file guard (see `absent_guard`'s module doc comment for the full
/// doctrine): a Write or Edit that carries content, judged LOCATION first,
/// then the content.
///
/// LOCATION FIRST, deliberately: it is the broader refusal (a file need not
/// carry any particular text to be refused, only to sit in the wrong place),
/// so when a write would trip both, the reason reported is the one that would
/// have refused it regardless of what it said. It is also the arm with the
/// least excuse for being silenced: the way out of it is not to satisfy the
/// rule, it is to write somewhere else.
///
/// The location pool is fetched separately from the content one:
/// `live::candidates_for` narrows by target KIND only, so a `Path`-only fetch
/// (what `ServeInput::add_file` builds) would never surface a `Dir`-bound
/// item. A `Path` AND a `Dir` placeholder target are added instead - the
/// value never matters for this narrowing, only the kind, and
/// `absent_guard::find_location_violation` alone decides real containment.
/// That pool never goes through `rank::select` (see `absent_guard`'s module
/// doc comment for why approximate matching is the wrong tool for a location
/// prohibition), and so carries no project scoping: a rule prohibiting writes
/// to a place is true or not whichever project the session resolves to.
///
/// The five arms, in order:
/// 1. location (`find_location_violation`);
/// 2. the ranked content pool (`find_violation`) - a path-anchored content
///    rule, the most specific statement about this file;
/// 3. content for a rule bound to a DIRECTORY (`find_dir_content_violation`):
///    `rank::select` drops such an item on a kind mismatch before it compares
///    a path, so it has to come from the kind-narrowed location pool;
/// 4. the self-contained form (`find_forbidden_violation`): a `Forbidden`
///    check has no anchor, so its Always binding carries its reach - every
///    write to every file. It needs no root but does need the session's
///    project, like the command arms: the literal carries no location, so
///    without it one project's forbidden text would refuse a write in every
///    other checkout (see `in_session_project`);
/// 5. the mirror image of all four (`find_missing_required`): not a literal
///    that must not appear, but one that must not disappear. Fed the file's
///    content AFTER the call (`absent_guard::content_after_write`), never the
///    fragment the call carries - a replacement fragment almost never
///    contains the required literal. A content it cannot resolve means this
///    arm simply does not run. Last, because an offending fragment someone
///    can see beats an absence they cannot.
///
/// A relative `file_path` names a file under the session's working folder
/// `cwd`, not under `root`; every arm judges the file it really names
/// (`judged_path`), and the refusal is filed under that same path.
///
/// Nothing to decide - no file path, a tool with no content, no matching
/// item, a stale anchor - is `verdict: None`, never a refusal: any error,
/// anywhere, is silence.
pub fn file_write(
    store: &EventStore,
    tool_name: &str,
    file_path: Option<&str>,
    tool_input: Option<&Value>,
    project: Option<&str>,
    root: Option<&Path>,
    cwd: Option<&Path>,
) -> Judgement {
    file_write_in(&Live::of(store), tool_name, file_path, tool_input, project, root, cwd)
}

fn file_write_in(
    live: &Live,
    tool_name: &str,
    file_path: Option<&str>,
    tool_input: Option<&Value>,
    project: Option<&str>,
    root: Option<&Path>,
    cwd: Option<&Path>,
) -> Judgement {
    let Some(file_path) = file_path else { return Judgement::default() };
    let Some(content) = absent_guard::proposed_content(tool_name, tool_input) else { return Judgement::default() };
    let judged = judged_path(file_path, root, cwd);
    let file_path = judged.as_str();

    let mut location_input = ServeInput::default();
    location_input.add_target(TargetKind::Path, file_path);
    location_input.add_target(TargetKind::Dir, file_path);
    let location = live.candidates(&location_input);

    let mut input =
        ServeInput { project: project.map(str::to_string), root: root.map(Path::to_path_buf), ..Default::default() };
    input.add_file(file_path);
    let candidates = live.candidates(&input);
    let ranked = rank::select(&candidates, &input);

    let always = live.always_in(project);
    let reason = absent_guard::find_location_violation(&location, file_path, root)
        .or_else(|| absent_guard::find_violation(&ranked, file_path, content, root))
        .or_else(|| absent_guard::find_dir_content_violation(&location, file_path, content, root))
        .or_else(|| absent_guard::find_forbidden_violation(&always, content))
        .or_else(|| {
            absent_guard::content_after_write(tool_name, tool_input, root, file_path)
                .and_then(|after| absent_guard::find_missing_required(&location, file_path, &after, root))
        });

    // Per ATTEMPT, not per file (`absent_guard::attempt_key`): keying on the
    // file alone disarmed the guard for the rest of the session after one
    // refusal, so a second, different, genuinely forbidden write passed.
    let verdict = reason.map(|reason| {
        let attempt_text = absent_guard::attempt_text(tool_name, tool_input);
        Verdict {
            rule_id: absent_guard::rule_id_of(&reason).map(str::to_string),
            target: file_path.to_string(),
            repeat: Some(RepeatKey::File(absent_guard::attempt_key(file_path, attempt_text.as_deref()))),
            reason,
        }
    });
    // A replay feeds no staleness sidecar, so it copies no pool.
    let pools = match live.keeps_pools() {
        true => Pools {
            file: Some(FilePools { file_path: file_path.to_string(), location: owned(location), ranked }),
            command: None,
        },
        false => Pools::default(),
    };
    Judgement { verdict, pools }
}

/// The command guard (see `absent_guard`'s module doc comment, "THE COMMAND
/// guard"): a call carrying a "command" field (read the one way,
/// `absent_guard::proposed_command`, whatever the tool is called), cut into
/// its segments in the dialect of `tool_name` (see [`Call`]).
///
/// THE SHELL WRITE HOLE, checked FIRST. A rule anchored at a file with a
/// `contains` or `absent` check refuses an Edit/Write of that file, but `rm`,
/// `truncate -s 0`, `sed -i`, a `>`/`>>` redirect, `tee`, and a `mv`/`cp`
/// destination walked straight past it - a shell write is a write.
/// `input::shell_writes_in` reads those paths out of the SAME segments every
/// command tooth reads (a write on its own line, inside `$(...)` or inside a
/// `bash -lc` is found; one sent over `ssh` is not local), and each goes
/// through the location check and, only for a write that removes or empties
/// the file (where what remains is KNOWN to be nothing), the must-remain
/// check. A rewrite is never fed to the must-remain check: what a
/// `sed`/`tee`/redirect leaves behind cannot be known without running it, and
/// guessing risks a false refusal. A place out of bounds or a lost required
/// literal is a more specific complaint than a forbidden literal in the
/// command string, so it wins. A relative write target is relative to the
/// folder its segment runs in (`effective_dir`: the `cd` chain before it, or
/// its own `git -C`), and to the session's working folder `cwd` when the
/// segment names none or one no reader can resolve (`judged_path`): `rm
/// NOTES.md` from a subfolder removes the subfolder's file, never the one at
/// the top of the checkout. The refusal still quotes the path as the command
/// wrote it.
///
/// Then the command-anchored rules: a live Rule/Orientation bound to a
/// `Command` target whose check forbids a literal in the unit of a segment
/// its anchor reaches (`absent_guard::find_command_violation_in`). The files
/// and hosts the command NAMES are added as targets of their own, so a fact
/// anchored at a file reaches a shell command that reads it just as it
/// reaches a file tool - only the targets, never the moments a command
/// implies. EACH SEGMENT IS JUDGED UNDER ITS OWN PROJECT (`segment_projects`):
/// `cd ../other-checkout && git push` is judged by the other checkout's
/// rules when the store knows that project, a `cd` into a folder of no known
/// project - or one that did not take effect - keeps the session's, and a
/// global rule applies everywhere. The pool itself goes back to the hook
/// unscoped for the staleness sidecar.
///
/// The repeat key carries the command's own fingerprint beside the matched
/// anchor: the same command again is told it is a repeat, a different one
/// tripping the same rule gets its own plain wording, and neither gets
/// through.
pub fn command(
    store: &EventStore,
    tool_name: &str,
    tool_input: Option<&Value>,
    root: Option<&Path>,
    cwd: Option<&Path>,
    project: Option<&str>,
) -> Judgement {
    let live = Live::of(store);
    let call = Call::of(&live, tool_name, tool_input, project, cwd);
    command_in(&live, tool_input, call.as_ref(), root, cwd)
}

fn command_in(
    live: &Live,
    tool_input: Option<&Value>,
    call: Option<&Call>,
    root: Option<&Path>,
    cwd: Option<&Path>,
) -> Judgement {
    let (Some(command), Some(call)) = (absent_guard::proposed_command(tool_input), call) else {
        return Judgement::default();
    };

    for at in input::shell_writes_in(&call.segments) {
        let write = at.write;
        // A folder that is not there: the `cd` into it fails, and the write
        // lands where the command already was.
        let base = effective_dir(at.dir.as_deref(), cwd).filter(|dir| dir.is_dir());
        let path = judged_path(&write.path, root, base.as_deref().or(cwd));
        let mut location_input = ServeInput::default();
        location_input.add_target(TargetKind::Path, &path);
        location_input.add_target(TargetKind::Dir, &path);
        let location = live.candidates(&location_input);

        let shell_reason = absent_guard::find_location_violation(&location, &path, root).or_else(|| {
            matches!(write.effect, ShellWriteEffect::Removed | ShellWriteEffect::Emptied)
                .then(|| absent_guard::find_missing_required(&location, &path, "", root))
                .flatten()
        });
        let Some(shell_reason) = shell_reason else { continue };

        let verb = match write.effect {
            ShellWriteEffect::Removed => "removed",
            ShellWriteEffect::Emptied => "emptied",
            ShellWriteEffect::Rewritten => "rewritten",
        };
        return Judgement {
            verdict: Some(Verdict {
                rule_id: absent_guard::rule_id_of(&shell_reason).map(str::to_string),
                reason: format!(
                    "{shell_reason} The command `{command}` would have {verb} {}. Nothing was done.",
                    write.path
                ),
                target: command.to_string(),
                repeat: None,
            }),
            pools: Pools::default(),
        };
    }

    let mut targets = ServeInput::default();
    targets.add_target(TargetKind::Command, command);
    for path in input::paths_in_command(command) {
        targets.add_target(TargetKind::Path, &path);
    }
    for host in input::hosts_in_command(command) {
        targets.add_target(TargetKind::Host, &host);
    }
    let items = live.candidates(&targets);

    let verdict = absent_guard::find_command_violation_in(&items, &call.segments, Some(&call.projects), root).map(
        |(reason, anchor)| Verdict {
            rule_id: absent_guard::rule_id_of(&reason).map(str::to_string),
            target: command.to_string(),
            repeat: Some(RepeatKey::Command(absent_guard::attempt_key(&anchor, Some(command)))),
            reason,
        },
    );
    if !live.keeps_pools() {
        return Judgement { verdict, pools: Pools::default() };
    }
    let pool = CommandPool { command: command.to_string(), items: owned(items), segments: call.segments.clone() };
    Judgement { verdict, pools: Pools { file: None, command: Some(pool) } }
}

/// The conditional guard, and the only arm that catches an OMISSION: a live
/// Rule/Orientation reached through a `Command` target, carrying a
/// `Check::Requires`, refuses a call whose own input trips the trigger and
/// says none of the requirements.
///
/// WHAT IT READS. Every VALUE in `tool_input`, flattened to one string
/// (`tool_input_values`) - never the field names, so a rule cannot match the
/// shape of the payload instead of what the caller wrote. That makes it work
/// for any tool: a script, a prompt, a command, an argument list. The reach
/// is decided per segment of the real command when the call carries one
/// (see [`Call`]), and against the bare tool name otherwise - one
/// pseudo-segment whose unit is the flattened input (`shell::tool_segment`)
/// (`absent_guard::find_requires_violation_in`).
///
/// Every live item, not just the Always pool: a conditional rule about one
/// tool hangs on a Command target, and narrowing the pool first would drop
/// exactly those. EACH SEGMENT IS JUDGED UNDER ITS OWN PROJECT (2026-09-26,
/// per segment since the door): a rule written for one checkout ("pip must
/// carry --no-deps" for one repository's own fork) refused an ordinary `pip
/// install` in an unrelated repository - a false block, the most expensive
/// thing this system can do. A call with no command runs in the session's
/// own project.
pub fn requires(
    store: &EventStore,
    tool: &str,
    tool_input: Option<&Value>,
    project: Option<&str>,
    cwd: Option<&Path>,
) -> Option<Verdict> {
    let live = Live::of(store);
    let call = Call::of(&live, tool, tool_input, project, cwd);
    requires_in(&live, tool, tool_input, call.as_ref(), project, None)
}

/// `owner`: where the live hook reads the owner's own words for a rule marked
/// owner-approval (step 26, `absent_guard::find_requires_violation_owned`);
/// `None` in THOR's own trial, which builds the calls it tries.
fn requires_in(
    live: &Live,
    tool: &str,
    tool_input: Option<&Value>,
    call: Option<&Call>,
    project: Option<&str>,
    owner: Option<absent_guard::OwnerWords>,
) -> Option<Verdict> {
    let content = tool_input_values(tool_input?);
    if content.is_empty() {
        return None;
    }
    let candidates = live.items();
    let command = absent_guard::proposed_command(tool_input);
    let reason = match call {
        Some(call) => absent_guard::find_requires_violation_owned(&candidates, &call.segments, Some(&call.projects), &content, owner),
        None => {
            let segments = [shell::tool_segment(tool, &content)];
            let projects = [project.map(str::to_string)];
            absent_guard::find_requires_violation_owned(&candidates, &segments, Some(&projects), &content, owner)
        }
    }?;
    Some(Verdict {
        rule_id: absent_guard::rule_id_of(&reason).map(str::to_string),
        target: command.unwrap_or(tool).to_string(),
        repeat: None,
        reason,
    })
}

/// The project each segment of a call runs in (5c): a segment whose folder
/// (`shell::Segment::dir` - the `cd` chain before it that certainly took
/// effect, a `cd` the shell refuses moving nothing, or the repository its
/// own `git -C`, `--git-dir` or `GIT_DIR=` names; a `--work-tree` names no
/// repository and chooses nothing) is a folder on disk that resolves to
/// ANOTHER KNOWN project - one this store holds a live item of - runs in
/// THAT project;
/// every other segment runs in `session_project`
/// (`project::session_project`: the folder's own, else the one the session
/// last worked in).
///
/// "Every other" is deliberate, and it is what keeps a `cd` from being an
/// exit: a folder of no project (`cd /tmp`), a folder that is not there (the
/// `cd` fails, and the command runs where it was), one no reader can resolve
/// (it holds a `$`, starts with `~`, or is `-`), a network path
/// (`//host/share`, never walked by a hook), a command sent over `ssh`, and a
/// checkout this store knows nothing of all keep the session's project, so
/// its rules still apply. Without "known", `git init` in any scratch folder
/// made a project of its basename, and one `cd` into it silenced every rule
/// of the session's project. The stated price: a command whose effect
/// depends on its folder, run in a folder of no known project, is judged
/// under the session's project. Two things soften that price, for `git`
/// alone - it acts on the repository that holds its folder, and a repository
/// of no project this store knows is not the session's project (a `git push` in it is
/// judged by the global rules only): a folder that resolves to such a project, and a
/// repository the same call makes itself (`scratch_made_by`: `git init <dir>`,
/// `git clone <local source> [dir]` of a folder that is not there yet).
/// The exception that keeps a push ban from being walked around: such a
/// repository that shares a remote with a known project's registered working
/// copy (a second clone of the same repository, in a folder of another name)
/// is that project's (`Live::project_of_remote`).
///
/// A folder written with a variable (`git -C $b push`, `cd $b`) is read with
/// the variables the segments before it certainly set (`shell::env_before`);
/// a name nothing set leaves the folder one no reader resolves.
///
/// The store's projects are read only when some segment's folder resolves to
/// a project other than the session's (`Live::knows_project`), and each
/// folder is resolved once per call.
pub fn segment_projects(
    live: &Live,
    segments: &[Segment],
    cwd: Option<&Path>,
    session_project: Option<&str>,
) -> Vec<Option<String>> {
    let mut folders: HashMap<PathBuf, Option<String>> = HashMap::new();
    let mut scratch: Vec<PathBuf> = Vec::new();
    let mut projects = Vec::with_capacity(segments.len());
    for (i, segment) in segments.iter().enumerate() {
        let session = session_project.map(str::to_string);
        if segment.remote {
            projects.push(session);
            continue;
        }
        let resolve = |typed: Option<&str>| {
            let text = typed.map(|dir| {
                if dir.contains('$') {
                    shell::expand_vars(dir, &shell::env_before(segments, i))
                } else {
                    home_expanded(dir)
                }
            });
            effective_dir(text.as_deref(), cwd)
        };
        let dir = resolve(segment.dir.as_deref());
        // Where the segment runs if every `cd` before it took effect: how a folder the call
        // makes is named, the way the call itself names it (`cd hk` after `mkdir hk &&`).
        let intended = resolve(segment.any_cd_dir.as_deref());
        let git = segment.head.first().is_some_and(|w| w.eq_ignore_ascii_case("git"));
        let project = match &dir {
            // A repository this very call made is nobody's project (`scratch_made_by`).
            _ if git && intended.as_ref().is_some_and(|dir| scratch.iter().any(|root| dir.starts_with(root))) => None,
            Some(dir) => {
                let folder = folders
                    .entry(dir.clone())
                    .or_insert_with_key(|dir| if dir.is_dir() { project::resolve_project(&canonical(dir)) } else { None })
                    .clone();
                match folder {
                    Some(other) if session_project != Some(other.as_str()) && live.knows_project(&other) => Some(other),
                    // git acts on the repository that holds its folder: one of no project this
                    // store knows is not the session's project - unless it is a clone of a known
                    // project's repository (a remote shared with that project's working copy), in
                    // a folder of another name: it is still that project's, and its pushes are.
                    Some(other) if git && session_project != Some(other.as_str()) => live.project_of_remote(dir),
                    // A bare repository has no `.git` entry to find, yet git acts on it and on
                    // nothing else: it is not the session's repository.
                    None if git && is_bare_repository(dir) => live.project_of_remote(dir),
                    _ => session,
                }
            }
            None => session,
        };
        projects.push(project);
        if let Some(root) = scratch_made_by(segment, intended.as_deref(), cwd) {
            scratch.push(root);
        }
    }
    projects
}

/// The repository a `git init <dir>` or a `git clone <local source> [dir]`
/// makes, as a path read from `base` (the folder the segment runs in, else the
/// session's `cwd`). It is a repository this call MAKES only when nothing is
/// there yet: a `git init` of a folder that exists is a re-init of what it
/// was. A clone of a remote source (`https://`, `git@host:`, `host:path`) is
/// not scratch: its pushes go to that remote. A `git init` naming no folder
/// makes the folder the segment is in, which is not a path this can place.
fn scratch_made_by(segment: &Segment, dir: Option<&Path>, cwd: Option<&Path>) -> Option<PathBuf> {
    let head = &segment.head;
    if !head.first().is_some_and(|w| w.eq_ignore_ascii_case("git")) {
        return None;
    }
    let takes_value = |w: &str| {
        matches!(
            w,
            "-b" | "--branch" | "--initial-branch" | "--template" | "--separate-git-dir" | "--object-format" | "-o" | "--origin"
                | "--depth" | "--reference" | "-c" | "--config" | "-j" | "--jobs" | "--filter"
        )
    };
    let mut operands: Vec<&str> = Vec::new();
    let mut skip = false;
    for w in head.iter().skip(2).take_while(|w| *w != "--") {
        if std::mem::take(&mut skip) {
            continue;
        }
        if takes_value(w) {
            skip = true;
        } else if !w.starts_with('-') {
            operands.push(w);
        }
    }
    let named = match head.get(1).map(String::as_str)? {
        "init" => operands.last()?.to_string(),
        "clone" => {
            let source = *operands.first()?;
            let remote = source.contains("://") || source.contains('@') || source.get(2..).is_some_and(|rest| rest.contains(':'));
            if remote {
                return None;
            }
            match operands.get(1) {
                Some(dir) => dir.to_string(),
                None => source.trim_end_matches(['/', '\\']).rsplit(['/', '\\']).next()?.trim_end_matches(".git").to_string(),
            }
        }
        _ => return None,
    };
    let native = drive_form(&named);
    let made = without_dots(&dir.or(cwd)?.join(native));
    (!made.exists()).then_some(made)
}

/// The items of a pool as an owned list: moved out when nothing else holds the
/// pool (the hook's own call, which reads each pool afresh), copied when a
/// replay shares it.
fn owned(pool: std::rc::Rc<Vec<LiveItem>>) -> Vec<LiveItem> {
    std::rc::Rc::try_unwrap(pool).unwrap_or_else(|shared| (*shared).clone())
}

/// The store's live items for one decision, and the projects they are filed
/// under - each read from the store the first time it is asked, and never
/// again: the project scope of a call and its `requires` arm share one read
/// (one per hook call, or one for a whole replay of many calls).
pub struct Live<'a> {
    store: &'a EventStore,
    items: std::cell::OnceCell<Vec<LiveItem>>,
    projects: std::cell::OnceCell<HashSet<String>>,
    /// Each known project's registered remotes, normalised, to the project.
    remote_owners: std::cell::OnceCell<HashMap<String, String>>,
    /// A replay of many calls against one store that does not change: each
    /// candidate pool (`live::candidates_for`) is read and parsed once, not
    /// once per call, and no staleness pool is copied out.
    replay: bool,
    pools: std::cell::RefCell<HashMap<(Vec<String>, Vec<String>), std::rc::Rc<Vec<LiveItem>>>>,
    always: std::cell::RefCell<HashMap<Option<String>, std::rc::Rc<Vec<LiveItem>>>>,
}

impl<'a> Live<'a> {
    pub fn of(store: &'a EventStore) -> Self {
        Live {
            store,
            items: std::cell::OnceCell::new(),
            projects: std::cell::OnceCell::new(),
            remote_owners: std::cell::OnceCell::new(),
            replay: false,
            pools: Default::default(),
            always: Default::default(),
        }
    }

    /// `of`, for a replay of many calls against a store that does not change
    /// while it runs (`sweep`). The decisions are the same; only the reading of
    /// the store is shared between calls.
    pub fn for_replay(store: &'a EventStore) -> Self {
        Live { replay: true, ..Live::of(store) }
    }

    fn keeps_pools(&self) -> bool {
        !self.replay
    }

    /// `live::candidates_for` for `input` - read once per pool in a replay.
    fn candidates(&self, input: &ServeInput) -> std::rc::Rc<Vec<LiveItem>> {
        if !self.replay {
            return std::rc::Rc::new(live::candidates_for(self.store, input));
        }
        let key = live::candidate_pool_key(input);
        if let Some(known) = self.pools.borrow().get(&key) {
            return known.clone();
        }
        let pool = std::rc::Rc::new(live::candidates_for(self.store, input));
        self.pools.borrow_mut().insert(key, pool.clone());
        pool
    }

    /// The Always pool as `session_project` sees it (`in_session_project`).
    fn always_in(&self, session_project: Option<&str>) -> std::rc::Rc<Vec<LiveItem>> {
        let key = session_project.map(str::to_string);
        if self.replay {
            if let Some(known) = self.always.borrow().get(&key) {
                return known.clone();
            }
        }
        let pool = std::rc::Rc::new(in_session_project(live::always_candidates(self.store), session_project));
        if self.replay {
            self.always.borrow_mut().insert(key, pool.clone());
        }
        pool
    }

    /// The known project whose registered working copy shares a remote with
    /// the repository that holds `dir`: a second clone of a project's repository
    /// in a folder of another name is still that project. Remotes are read from
    /// the git config files alone (`checkout::remote_urls`), spelled alike
    /// whatever the scheme, `.git` suffix or case (`checkout::normalise_remote`),
    /// and the registry is read once per `Live`. `None` when `dir` is no
    /// repository, has no remote, shares none, or anything is unreadable - the
    /// answer a repository of no known project already gets.
    pub fn project_of_remote(&self, dir: &Path) -> Option<String> {
        let owners = self.remote_owners.get_or_init(|| {
            let mut known: Vec<&String> = self.projects.get_or_init(|| self.known_projects()).iter().collect();
            known.sort();
            let mut owners = HashMap::new();
            for project in known {
                let Some((root, _)) = model::store::registered_root_raw(self.store, project) else { continue };
                for url in model::checkout::remote_urls(&root) {
                    if let Some(remote) = model::checkout::normalise_remote(&url) {
                        owners.entry(remote).or_insert_with(|| project.clone());
                    }
                }
            }
            owners
        });
        if owners.is_empty() {
            return None;
        }
        let top = model::checkout::git_root(dir)?;
        model::checkout::remote_urls(&top)
            .iter()
            .filter_map(|url| model::checkout::normalise_remote(url))
            .find_map(|remote| owners.get(&remote).cloned())
    }

    fn known_projects(&self) -> HashSet<String> {
        self.items().iter().filter_map(|li| li.item.project.clone()).collect()
    }

    /// Every live item of the store (`live::live_items`).
    pub fn items(&self) -> &[LiveItem] {
        self.items.get_or_init(|| live::live_items(self.store))
    }

    /// Whether a live item of the store is filed under `project` - a project
    /// this memory knows (a project record written by install counts, being
    /// one).
    pub fn knows_project(&self, project: &str) -> bool {
        self.projects.get_or_init(|| self.known_projects()).contains(project)
    }
}

/// `dir` with a leading `~` (the home folder, as a shell reads it) replaced by the
/// home folder of the process that judges it; `dir` unchanged when it does not start
/// with one, or no home folder is known (`effective_dir` then leaves it unresolved).
fn home_expanded(dir: &str) -> String {
    let rest = match dir.strip_prefix('~') {
        Some("") => "",
        Some(rest) if rest.starts_with('/') || rest.starts_with('\\') => rest,
        _ => return dir.to_string(),
    };
    match std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME")) {
        Ok(home) if !home.is_empty() => format!("{}{rest}", home.trim_end_matches(['/', '\\'])),
        _ => dir.to_string(),
    }
}

/// Whether `dir` is a bare git repository itself: a `HEAD` file beside an `objects` and
/// a `refs` folder, with no `.git` above it to tell so.
fn is_bare_repository(dir: &Path) -> bool {
    dir.join("HEAD").is_file() && dir.join("objects").is_dir() && dir.join("refs").is_dir()
}

/// The folder a segment's own directory text names, made absolute against
/// the session's working folder `cwd` and with its `.`/`..` taken by text -
/// `None` when the segment names no folder, or one no reader can resolve: a
/// `$` in it, a `~` or `-` (the home folder, the previous folder), or a
/// network path (`//host/share`, `\\host\share`), which a hook never walks.
/// On Windows a Git Bash drive path (`/c/work`) reads as `C:/work`.
fn effective_dir(dir: Option<&str>, cwd: Option<&Path>) -> Option<PathBuf> {
    let dir = dir?;
    let unresolvable = dir.contains('$')
        || dir.starts_with('~')
        || dir.starts_with("//")
        || dir.starts_with("\\\\")
        || dir.split(['/', '\\']).any(|part| part == "-");
    if unresolvable {
        return None;
    }
    let native = drive_form(dir);
    let path = Path::new(&native);
    let joined = if path.has_root() { path.to_path_buf() } else { cwd?.join(path) };
    Some(without_dots(&joined))
}

/// A Git Bash drive path (`/c/x`) in the form Windows reads (`C:/x`). Only on
/// Windows: elsewhere `/c/x` is a real path of its own.
fn drive_form(dir: &str) -> String {
    let b = dir.as_bytes();
    let drive = cfg!(windows) && b.len() >= 2 && b[0] == b'/' && b[1].is_ascii_alphabetic() && (b.len() == 2 || b[2] == b'/');
    if drive {
        format!("{}:/{}", (b[1] as char).to_ascii_uppercase(), &dir[2..].trim_start_matches('/'))
    } else {
        dir.to_string()
    }
}

/// `path` resolved on disk (links followed), without the `\\?\` prefix
/// Windows puts in front of a resolved path; `path` itself when it cannot be
/// resolved.
fn canonical(path: &Path) -> PathBuf {
    match std::fs::canonicalize(path) {
        Ok(resolved) => {
            let text = resolved.to_string_lossy().to_string();
            match text.strip_prefix(r"\\?\") {
                Some(rest) if !rest.starts_with("UNC") => PathBuf::from(rest),
                _ => resolved,
            }
        }
        Err(_) => path.to_path_buf(),
    }
}

/// Only the items that apply to a session in `session_project` - its own
/// project's, and the global ones (`project::applies_to`). Used by the one
/// hard guard whose match reads only the TEXT of content with no path of its
/// own (the always-forbidden arm of `file_write`): nothing about that text
/// says which checkout it belongs to, so without this a rule from one project
/// refuses an ordinary write in another. The command and requires arms scope
/// each segment of a call on its own instead (`segment_projects`). A
/// path-anchored arm needs no such filter - its own anchor is resolved
/// against the session's root already (see
/// `absent_guard::scoped_target_matches`), and that root follows the
/// session's project too: after a drift into a folder of no project it is the
/// project's registered checkout (`project::guard_root`), so the project's
/// own files keep their path guards there.
fn in_session_project(items: Vec<LiveItem>, session_project: Option<&str>) -> Vec<LiveItem> {
    items.into_iter().filter(|li| project::applies_to(li.item.project.as_deref(), session_project)).collect()
}

/// The file a path in a call really names, in the form the file guards read.
///
/// WHY. Check paths resolve against `root`, the top of the checkout, so a
/// session in a subfolder still has its rules (a required line in the
/// checkout's NOTES.md is guarded from `<checkout>/sub` too). A relative path
/// in a call, though, is relative to where the session stands: `rm NOTES.md`
/// from `<checkout>/sub` names `sub/NOTES.md`, never the top-level file. So a
/// relative path is joined onto `cwd` first, and a `.` or `..` in it is taken
/// by its text (`rm ../NOTES.md` from `sub` names the top-level file; no
/// comparison further on ever resolves a `..`, so leaving one in would let
/// that file go unguarded).
///
/// THE FORM. The joined path goes on relative to `root` when it lies inside
/// it - the form every arm resolves with `root.join`, and the only form the
/// directory-anchored must-remain arm can try: it builds a check from the path
/// itself, and `model::check::run` refuses an absolute check path. Outside
/// `root` it goes on absolute. An absolute path, or one with no `cwd` or no
/// `root` to place it by, is returned exactly as it came, so a session
/// standing at the top of its checkout judges every plain relative path as it
/// always did.
fn judged_path(path: &str, root: Option<&Path>, cwd: Option<&Path>) -> String {
    let (Some(root), Some(cwd)) = (root, cwd) else { return path.to_string() };
    let relative = Path::new(path);
    if relative.has_root() {
        return path.to_string();
    }
    let dotted = relative.components().any(|c| matches!(c, Component::CurDir | Component::ParentDir));
    let joined = if dotted { without_dots(&cwd.join(relative)) } else { cwd.join(relative) };
    match joined.strip_prefix(root) {
        Ok(inside) => inside.to_string_lossy().to_string(),
        Err(_) => joined.to_string_lossy().to_string(),
    }
}

/// `path` with every `.` dropped and every `..` taking back the folder before
/// it, by its text alone (a `..` at the top of a drive stays at the top).
fn without_dots(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// How deep `tool_input_values` will recurse into a nested array/object
/// before it gives up and stops descending. `input` is the model's own tool
/// call, not something this project controls the shape of, so an unbounded
/// walk over one nested deep enough (deliberately or not) is a stack
/// overflow waiting to happen - the same "never trust a payload's own shape
/// to stay reasonable" stance `model::check::run`'s own file-size limit
/// takes for an unbounded read.
const MAX_TOOL_INPUT_WALK_DEPTH: usize = 32;

/// Every string VALUE inside a tool input, joined with newlines. Field names
/// are deliberately left out: a rule must match what the caller wrote, not
/// the shape of the payload it travelled in. Recursion is capped at
/// `MAX_TOOL_INPUT_WALK_DEPTH`: anything nested deeper is silently left out
/// of the flattened text rather than walked - the same "a missed case is
/// acceptable, a crash is not" stance this whole guard takes throughout.
fn tool_input_values(input: &Value) -> String {
    fn walk(v: &Value, out: &mut Vec<String>, depth: usize) {
        if depth > MAX_TOOL_INPUT_WALK_DEPTH {
            return;
        }
        match v {
            Value::String(s) => out.push(s.clone()),
            Value::Number(n) => out.push(n.to_string()),
            Value::Bool(b) => out.push(b.to_string()),
            Value::Array(items) => items.iter().for_each(|i| walk(i, out, depth + 1)),
            Value::Object(map) => map.values().for_each(|i| walk(i, out, depth + 1)),
            Value::Null => {}
        }
    }
    let mut out = Vec::new();
    walk(input, &mut out, 0);
    out.join("\n")
}

#[cfg(test)]
mod judged_path_tests {
    use super::*;

    fn slashes(s: &str) -> String {
        s.replace('\\', "/")
    }

    #[test]
    fn a_relative_path_from_a_subfolder_is_the_subfolders_file_relative_to_the_top() {
        let top = tempfile::tempdir().unwrap();
        let sub = top.path().join("sub");
        assert_eq!(slashes(&judged_path("NOTES.md", Some(top.path()), Some(&sub))), "sub/NOTES.md");
        assert_eq!(slashes(&judged_path("docs/a.md", Some(top.path()), Some(&sub))), "sub/docs/a.md");
    }

    #[test]
    fn at_the_top_a_relative_path_is_unchanged() {
        let top = tempfile::tempdir().unwrap();
        assert_eq!(judged_path("NOTES.md", Some(top.path()), Some(top.path())), "NOTES.md");
        assert_eq!(judged_path("docs/a.md", Some(top.path()), Some(top.path())), "docs/a.md");
    }

    #[test]
    fn an_absolute_path_or_a_missing_folder_is_returned_as_it_came() {
        let top = tempfile::tempdir().unwrap();
        let sub = top.path().join("sub");
        let absolute = top.path().join("NOTES.md").to_string_lossy().to_string();
        assert_eq!(judged_path(&absolute, Some(top.path()), Some(&sub)), absolute);
        assert_eq!(judged_path("NOTES.md", None, Some(&sub)), "NOTES.md");
        assert_eq!(judged_path("NOTES.md", Some(top.path()), None), "NOTES.md");
    }

    #[test]
    fn a_relative_path_leaving_the_top_stays_absolute() {
        let top = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        let judged = judged_path("NOTES.md", Some(top.path()), Some(elsewhere.path()));
        assert_eq!(judged, elsewhere.path().join("NOTES.md").to_string_lossy());
    }

    /// No comparison after this ever resolves a `..`, so it is taken here:
    /// `rm ../NOTES.md` from `sub` names the top-level file.
    #[test]
    fn dots_in_a_relative_path_are_taken_by_their_text() {
        let top = tempfile::tempdir().unwrap();
        let sub = top.path().join("sub");
        assert_eq!(judged_path("../NOTES.md", Some(top.path()), Some(&sub)), "NOTES.md");
        assert_eq!(judged_path("./NOTES.md", Some(top.path()), Some(top.path())), "NOTES.md");
        assert_eq!(slashes(&judged_path("./x/../docs/a.md", Some(top.path()), Some(&sub))), "sub/docs/a.md");
        let parent = top.path().parent().unwrap();
        assert_eq!(
            judged_path("../../NOTES.md", Some(top.path()), Some(&sub)),
            parent.join("NOTES.md").to_string_lossy(),
            "leaving the top by dots ends outside it, absolute"
        );
    }
}

#[cfg(test)]
mod tool_input_values_tests {
    use super::*;

    /// A `serde_json::Value` nested `depth` arrays deep, with `leaf` as the
    /// innermost value - built so `MAX_TOOL_INPUT_WALK_DEPTH`'s own tests
    /// below can pick an exact depth relative to the cap, rather than
    /// guessing at a real payload's shape.
    fn nested(depth: usize, leaf: &str) -> Value {
        let mut v = serde_json::json!(leaf);
        for _ in 0..depth {
            v = serde_json::json!([v]);
        }
        v
    }

    #[test]
    fn nested_values_are_flattened_regardless_of_shape() {
        let input = serde_json::json!({
            "a": "one",
            "b": {"c": "two", "d": ["three", "four"]},
            "e": 5,
            "f": true,
            "g": null,
        });
        let flat = tool_input_values(&input);
        for expected in ["one", "two", "three", "four", "5", "true"] {
            assert!(flat.contains(expected), "missing {expected:?} in {flat:?}");
        }
        assert!(!flat.contains("null"), "Null must contribute nothing: {flat:?}");
    }

    #[test]
    fn a_value_exactly_at_the_depth_cap_is_still_reached() {
        let input = nested(MAX_TOOL_INPUT_WALK_DEPTH, "reachable");
        assert!(tool_input_values(&input).contains("reachable"));
    }

    /// THE CRASH THIS PREVENTS: `input` is the model's own tool call, not a
    /// shape this project controls, so an unbounded recursive walk over one
    /// nested deep enough is a stack overflow waiting to happen. Proven at
    /// the exact boundary rather than at some very large depth: constructing
    /// (and later dropping) a `serde_json::Value` nested tens of thousands of
    /// levels deep is itself a known way to overflow the stack, in the
    /// ordinary recursive `Drop` every such tree type gets - a risk that has
    /// nothing to do with `tool_input_values` and would make a "very deep"
    /// version of this test dangerous in exactly the way this fix exists to
    /// avoid. One level past the cap is enough to prove the walk stops
    /// there, without going anywhere near that unrelated risk.
    #[test]
    fn a_value_one_level_past_the_depth_cap_is_left_out_rather_than_reached() {
        let input = nested(MAX_TOOL_INPUT_WALK_DEPTH + 1, "too-deep");
        assert!(
            !tool_input_values(&input).contains("too-deep"),
            "a value past the cap must be left out, not walked into"
        );
    }
}
