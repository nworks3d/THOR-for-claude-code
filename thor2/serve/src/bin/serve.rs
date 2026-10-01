//! The four channels over the one serving path (`serve` crate's `lib.rs`):
//!
//!   hook   Claude Code PreToolUse: JSON on stdin, JSON on stdout, ALWAYS
//!          exit 0, silent on any failure (R5's "read and inject" policy).
//!   check  human-facing: what would fire for a given command/file/target.
//!   why    everything that applies, including what the block would withhold.
//!   audit  declared items vs. their ItemServed history - "declared, never
//!          delivered" as a query, not a thing read off a report by eye.
//!
//! check/why/audit are diagnostic tools for a person and are allowed to
//! report a real error (R5's "write and declare" policy does not apply here
//! either, but "never silent" is still the right instinct for a human-facing
//! tool - only `hook` is required to stay quiet).

use clap::{Parser, Subcommand};
use intent::Action;
use model::item::{Kind, TargetKind};
use serde_json::Value;
use serve::decay::DecayContext;
use serve::input::ServeInput;
use serve::rank::RankedItem;
use serve::{
    absent_guard, capture, deliver, guard_verdict, judge, lookup, mark, project, prompt, render, respond, session_start,
    stale_guard, status, time, usefulness,
};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use thor_core::event_store::EventStore;

#[derive(Parser)]
#[command(name = "serve", about = "Select, rank, cap and render items - one path, four channels")]
struct Cli {
    /// Path to the core event-log database.
    #[arg(long)]
    db: PathBuf,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// The real production delivery boundary for surfaces 1 (session start),
    /// 2 (moment of action) and 3 (per prompt): JSON on stdin, JSON on
    /// stdout, exit 0 always, silent on any failure. Branches on the JSON
    /// payload's `hook_event_name` ("SessionStart" / "UserPromptSubmit" /
    /// anything else, treated as a PreToolUse-shaped tool call). A `Stop`
    /// payload from an agent (`agent_id`) whose answer the owner reads
    /// himself may add `"owner_reads_answer": true`: the rules about how an
    /// answer reads to the owner then apply to it, and it stays an agent's
    /// call for everything else.
    Hook {
        /// Say on stderr whether this run could look at all, and exit 2 when it
        /// could not. For a caller that is NOT an assistant harness: the
        /// default silence cannot tell "I looked and nothing applies" apart
        /// from "I could not look", and a program steering a local model has
        /// to know the difference. stdout and the always-zero exit of a normal
        /// run are untouched by this flag.
        #[arg(long)]
        diagnose: bool,
        /// The actor recorded on every delivery (`ItemServed`) this run
        /// makes, in place of the default `usefulness::ASSISTANT_DELIVERY_
        /// ACTOR` ("hook") - for a caller that is NOT an assistant harness
        /// (see that constant's own doc comment for why only ITS firings
        /// are ever judgement debt: an assistant session can be asked "did
        /// this belong where it fired"; a program driving this boundary on
        /// its own was never in the room to answer that). 1 to 32
        /// characters, lowercase ascii letters/digits/dash/underscore only
        /// (`valid_actor_label`) - anything else is the caller's own
        /// mistake, named on stderr in one line, exit 2: not the silent
        /// fail-open this channel otherwise protects for a real payload.
        #[arg(long)]
        actor: Option<String>,
        /// Produce the exact same stdout as a normal run, but record NO
        /// delivery at all, on every arm - for a measurement run that has
        /// to be able to call this boundary hundreds of times with invented
        /// questions and leave no trace in the log. Nothing in the store's
        /// directory changes either: the store is opened read-only and no
        /// state file beside it (evaluation accrual, watermarks, markers) is
        /// written - see `serve::store_dir`. Combines with `--actor`;
        /// neither flag changes anything about the default behaviour (actor
        /// "hook", delivery recorded) when left off.
        #[arg(long)]
        no_record: bool,
    },
    /// What would fire for a given command, file, target or moment (surface
    /// 2 preview - does not count as a delivery).
    Check(TargetArgs),
    /// Everything that applies, including what the block would withhold
    /// (surface 2 preview - does not count as a delivery).
    Why(TargetArgs),
    /// Declared items vs. their delivery history.
    Audit,
    /// Surface 1 preview: every Always item, global plus (optionally) one
    /// project - full, never capped. Does not count as a delivery; only
    /// `hook`'s SessionStart branch does that.
    SessionStart {
        /// The current project's key. Omitted = resolve it automatically
        /// from the current working directory (`serve::project::
        /// resolve_project` - the one place in the workspace that performs
        /// this resolution: a marker file wins, else the git root's
        /// basename, else global-only). Pass this to preview a different
        /// project without changing directory.
        #[arg(long)]
        project: Option<String>,
    },
    /// Surface 3 preview: what a raw prompt resolves to, or nothing. Does
    /// not count as a delivery; only `hook`'s UserPromptSubmit branch does
    /// that.
    Prompt {
        #[arg(long)]
        text: String,
    },
    /// Surface 4 (explicit lookup): free-text search over every live item,
    /// any project, archive kinds (Report/Chunk) included.
    Search {
        query: String,
        /// Narrow the ranking to one or more kinds: rule, orientation, report
        /// or chunk, several at once separated by a comma, and `fires` as
        /// shorthand for every kind that can fire at a moment - which is the
        /// one to use when the fact has to steer an action
        /// (case-insensitive, an obvious plural such as "rules" accepted).
        /// Without it, Report hits tend to dominate a plain search since
        /// their own text runs long. Asking for `lookup` is refused rather
        /// than answered empty - a register only ever answers to its own key,
        /// so use `lookup-key` for that. Same argument, same names and the
        /// same two refusals as the agent's own `lookup` tool: both doors
        /// call `serve::lookup::parse_searchable_kind`.
        #[arg(long)]
        kind: Option<String>,
        /// Print the normal answer, then a block naming per hit whether it
        /// was found by TEXT (`literal`) or only by MEANING (`meaning`, with
        /// its similarity score) - never changes the ranking or the normal
        /// lines above it. See `serve::lookup::explain_best_effort`'s own doc
        /// comment for why the floor and the cap are the only two ways a hit
        /// can be missing entirely, which `--explain-id` names for one id at
        /// a time.
        #[arg(long)]
        explain: bool,
        /// Name one id and say exactly why it did or did not make this
        /// ranking: its similarity score, and - when it is missing - whether
        /// that is because it never reached the similarity floor or because
        /// too many other candidates outscored it under the cap. Composes
        /// with `--explain`, which only adds the per-hit literal/meaning
        /// lines above this one.
        #[arg(long = "explain-id")]
        explain_id: Option<String>,
    },
    /// Surface 4's other door: an explicit request for one Lookup's key.
    LookupKey {
        key: String,
    },
    /// Every address this memory can be asked for by name: the registers
    /// (`Lookup` items, returned whole and never ranked) and how many
    /// documents each scope holds, plus how many live items carry no scope
    /// at all. Read-only. The same answer the agent's `lookup` gives when it
    /// is called with neither a query nor a key - one definition, two doors.
    Catalog,
    /// One scope, opened: every item filed under it, one line each, complete
    /// and in date order. The step after `catalog` - and the same answer the
    /// agent's `lookup` gives when it is called with a scope and no query.
    Scope {
        /// The scope's name, as `catalog` lists it.
        name: String,
    },
    /// Surface 4's code door: search the code index built by the code-index
    /// binary against a repository. Every hit carries the commit it came
    /// from; the index's own drift against the repository's current state
    /// is printed first. Never an injection surface - see this crate's own
    /// guard test proving so (`serve/tests`, code-index-only-via-lookup).
    SearchCode {
        /// Path to the code index's own sqlite file (built with the
        /// code-index binary's own `<db> <repo> build` subcommand).
        #[arg(long = "index-db")]
        index_db: PathBuf,
        /// Path to (or inside) the repository the index was built from.
        #[arg(long)]
        repo: PathBuf,
        query: String,
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// Who defines and who uses one symbol name - the blast radius of a
    /// change, as file and line, with the index's own provenance first.
    /// Resolution is by bare name only: two unrelated things sharing a name
    /// come back together, so this points at places to open, never at a
    /// conclusion.
    WhereUsed {
        #[arg(long = "index-db")]
        index_db: PathBuf,
        #[arg(long)]
        repo: PathBuf,
        name: String,
        #[arg(long, default_value_t = 30)]
        limit: usize,
    },
    /// What one file defines, in line order. A path the index has never seen
    /// is reported as not indexed, which is a different answer from "this
    /// file defines nothing".
    Outline {
        #[arg(long = "index-db")]
        index_db: PathBuf,
        /// Repository-relative path, exactly as the index stores it.
        path: String,
    },
    /// What this store knows right now: how many live items per kind, how
    /// many are declared but never fired, how many were served repeatedly
    /// without ever being marked useful, and - when a code index is named -
    /// which commit it is at and how far the working copy has drifted.
    Status {
        #[arg(long = "index-db")]
        index_db: Option<PathBuf>,
        #[arg(long)]
        repo: Option<PathBuf>,
    },
    /// THOR's trial of a rule as it stands (`serve::teeth::retest`), run
    /// against the rule's REGISTERED working copy and the real calls in the
    /// log - never a folder named on the command line, so the answer is the
    /// same from any working folder. Prints the registered root, every form
    /// it tried with what the hook's own pipeline did with it, and the
    /// verdict. Writes nothing unless `--apply` is given: then a rule or a
    /// note is written as ONE `fact_revised` by THOR's `retest` actor,
    /// carrying the trial record, and only while the rule is still exactly
    /// the one tried. A blocked rule and one that needs the agent are never
    /// written. Exit codes: 0 a verdict the tool can apply (rule or note), 3
    /// blocked, 4 needs the agent, 1 an error, 2 a usage error. With
    /// `--project` or `--all`, every live rule and orientation without a
    /// current verdict is tried in turn, and the exit code is the worst of
    /// them (1, then 4, then 3, then 0).
    #[command(group(clap::ArgGroup::new("which").required(true).args(["id", "project", "all"])))]
    Retest {
        /// The rule's own id, exactly as `remember`/`revise` filed it.
        #[arg(long)]
        id: Option<String>,
        /// Every live rule and orientation of this project (exactly this
        /// key) that has no current verdict.
        #[arg(long)]
        project: Option<String>,
        /// Every live rule and orientation in the store that has no current
        /// verdict.
        #[arg(long)]
        all: bool,
        /// Write the rule or the note the trial decides. Without it, nothing
        /// is written.
        #[arg(long)]
        apply: bool,
    },
    /// Record that a served item actually helped. Recorded in the log and
    /// reported by `status`; it does NOT decide whether anything fires - see
    /// `serve::decay`'s own doc comment for why silence stopped removing
    /// things on 2026-08-03. Never a ranking nudge either.
    Mark {
        id: String,
        /// Record the OPPOSITE judgement: this item did not belong where it
        /// fired. Two of these, with no mark of usefulness, retire it from
        /// the injection surfaces - it stays fully findable via search.
        #[arg(long)]
        noise: bool,
    },
    /// Rebuild the meaning-search vector sidecar (feature `semantic`) from
    /// every live item `search` itself covers (any project, archive kinds
    /// included, never a Lookup). Overwrites whatever was stored before.
    #[cfg(feature = "semantic")]
    VectorsBuild {
        /// Override the model directory (default:
        /// `serve::semantic_paths::default_model_dir`).
        #[arg(long = "model-dir")]
        model_dir: Option<PathBuf>,
        /// Override the sidecar's own path (default:
        /// `serve::semantic_paths::default_vectors_path`).
        #[arg(long = "vectors-db")]
        vectors_db: Option<PathBuf>,
    },
    /// How many vectors are stored (feature `semantic`) and whether they
    /// still match the current content: missing (no vector yet), stale (the
    /// item's text changed since it was embedded), orphaned (the id is no
    /// longer live), plus whether the stored model_id matches this binary's.
    #[cfg(feature = "semantic")]
    VectorsStatus {
        #[arg(long = "vectors-db")]
        vectors_db: Option<PathBuf>,
    },
    /// Embed only what changed since the last build/refresh (feature
    /// `semantic`): a missing vector, or one whose stored hash no longer
    /// matches its item's current text; also drops the vector of any id that
    /// is no longer live. Never touches an item that is already up to date -
    /// see `vectors-build` above for the full, from-scratch rebuild this
    /// complements, and `serve::vectors::refresh`'s own doc comment for why a
    /// mismatched model_id makes this refuse outright instead of build's
    /// "clear and start over".
    #[cfg(feature = "semantic")]
    VectorsRefresh {
        /// Override the model directory (default:
        /// `serve::semantic_paths::default_model_dir`).
        #[arg(long = "model-dir")]
        model_dir: Option<PathBuf>,
        /// Override the sidecar's own path (default:
        /// `serve::semantic_paths::default_vectors_path`).
        #[arg(long = "vectors-db")]
        vectors_db: Option<PathBuf>,
        /// The most items to embed in this one call, 0 (default) for
        /// unlimited. Deleting an id that is no longer live is never
        /// budgeted - see `serve::vectors::refresh`'s own doc comment.
        #[arg(long, default_value_t = 0)]
        budget: usize,
    },
}

#[derive(clap::Args)]
struct TargetArgs {
    /// A file about to be touched, named directly with no flag at all - the
    /// shortest form of `--file` below, and the one a new user reaches for
    /// first.
    ///
    /// THE DEFECT THIS CLOSES. Every hint this binary prints for a withheld
    /// item (`serve::render::render_text`'s "run `serve why ...`" line) used
    /// to read "run `serve why`" with nothing after it - not a flag this
    /// parser accepted, and not even the file or command that made the
    /// block fire. A session's own report on trying to guess past it: "the
    /// help command for what fires here had a different flag than the hint
    /// said: --file, not a path." The hint is now built from this same
    /// struct (see `render::why_invocation`) and always names `--file`
    /// explicitly, but this positional stays too, so the shortest thing a
    /// new user actually types - the bare path, no flag remembered - also
    /// works. Refused together with `--file` (never silently picking one)
    /// rather than left to guess which wins.
    #[arg(value_name = "PATH", conflicts_with = "file")]
    path: Option<String>,
    /// A command about to run (derives its own moments; also a Command doel).
    #[arg(long)]
    command: Option<String>,
    /// A file about to be touched (derives its own moments; also a Path doel).
    #[arg(long)]
    file: Option<String>,
    /// An explicit target, `<kind>:<value>` (repeatable), kind one of
    /// path/dir/symbol/command/project/route/host.
    #[arg(long = "target", value_parser = parse_target_arg)]
    targets: Vec<(TargetKind, String)>,
    /// An explicit moment, named directly (repeatable).
    #[arg(long = "moment", value_parser = parse_moment_arg)]
    moments: Vec<Action>,
    /// Machine-readable output in place of the prose form: a JSON array on
    /// stdout and nothing else (no "detected: ..." line, no prose), one
    /// object per item, each with exactly these keys - "id" (string),
    /// "kind" (string), "severity" (string, or null when the item carries
    /// none), "verdict" ("prohibition" or "advice" - see this binary's own
    /// classifier, `absent_guard::classify_moment`, for the doctrine), only
    /// ever "matched_literal" (a string) when the verdict is a prohibition,
    /// never for advice, "text" (string), and "shown" (bool: true when the
    /// block would actually include this item, false when it applies but a
    /// heavier item is holding its place). BOTH `check` and `why` emit one
    /// row per item that applies at all, shown or withheld - never only the
    /// ones the block shows, so a rule that would genuinely refuse the thing
    /// being asked about is never invisible here just because it lost its
    /// four slots to heavier items; "shown" is what tells the two apart.
    /// `check`'s own prose form marks a not-shown prohibition the identical
    /// way, underneath the block it never changes; `why`'s prose form always
    /// did.
    #[arg(long)]
    json: bool,
}

fn parse_target_arg(s: &str) -> Result<(TargetKind, String), String> {
    let (kind_str, value) = s
        .split_once(':')
        .ok_or_else(|| format!("expected <kind>:<value> (e.g. path:src/main.rs), got '{s}'"))?;
    let kind = TargetKind::from_str(kind_str).map_err(|e| e.to_string())?;
    Ok((kind, value.to_string()))
}

fn parse_moment_arg(s: &str) -> Result<Action, String> {
    match model::gate::parse_moment(s) {
        Ok(model::item::Binding::Moment(action)) => Ok(action),
        Ok(_) => unreachable!("parse_moment only ever returns Binding::Moment"),
        Err(e) => Err(e.to_string()),
    }
}

/// The preview commands (`check`, `why`) build the same input the hook does,
/// project included. Without that they would answer a question the hook
/// answers differently - the same two-doors defect that let the MCP `lookup`
/// tool and the `search` CLI drift apart earlier the same day. A preview whose
/// answer does not match the real surface is worse than no preview.
fn build_input(args: &TargetArgs) -> ServeInput {
    let cwd = std::env::current_dir().ok();
    let mut input = ServeInput {
        project: cwd.as_deref().and_then(project::resolve_project),
        root: cwd,
        ..Default::default()
    };
    if let Some(command) = &args.command {
        input.add_command(command);
    }
    // The bare positional is `--file`'s own shorthand (see `TargetArgs::path`'s
    // doc comment) - `conflicts_with = "file"` on the parser already refuses
    // both together, so at most one of the two is ever `Some` here.
    if let Some(file) = args.file.as_deref().or(args.path.as_deref()) {
        input.add_file(file);
    }
    for (kind, value) in &args.targets {
        input.add_target(*kind, value);
    }
    for action in &args.moments {
        input.add_moment(*action);
    }
    input
}

fn main() {
    // THE GAP THIS CLOSES: verified 2026-09-08, no binary in the workspace
    // answered `--version` - checked here, before `Cli::parse()`, so this
    // still answers even if some future required top-level arg would
    // otherwise make clap's own `#[command(version)]` path harder to reason
    // about from this one locked call site. See `thor_core::is_version_flag`
    // for why this reads that shared predicate instead of a local literal.
    if std::env::args().any(|a| thor_core::is_version_flag(&a)) {
        println!("serve {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    let cli = Cli::parse();
    match cli.command {
        Command::Hook { diagnose, actor, no_record } => cmd_hook(&cli.db, diagnose, actor, no_record),
        Command::Check(args) => cmd_check(&cli.db, &build_input(&args), args.json),
        Command::Why(args) => cmd_why(&cli.db, &build_input(&args), args.json),
        Command::Audit => cmd_audit(&cli.db),
        Command::SessionStart { project } => {
            let resolved = project.or_else(current_project_from_cwd);
            cmd_session_start(&cli.db, resolved.as_deref())
        }
        Command::Prompt { text } => cmd_prompt(&cli.db, &text),
        Command::Search { query, kind, explain, explain_id } => {
            cmd_search(&cli.db, &query, kind.as_deref(), explain, explain_id.as_deref())
        }
        Command::LookupKey { key } => cmd_lookup_key(&cli.db, &key),
        Command::Catalog => {
            let store = open_store_or_die(&cli.db);
            print!("{}", lookup::render_catalog(&lookup::catalog(&store)));
        }
        Command::Scope { name } => {
            let store = open_store_or_die(&cli.db);
            print!("{}", lookup::render_scope(&name, &lookup::in_scope(&store, &name)));
        }
        Command::SearchCode { index_db, repo, query, limit } => cmd_search_code(&index_db, &repo, &query, limit),
        Command::WhereUsed { index_db, repo, name, limit } => cmd_where_used(&index_db, &repo, &name, limit),
        Command::Outline { index_db, path } => cmd_outline(&index_db, &path),
        Command::Status { index_db, repo } => cmd_status(&cli.db, index_db.as_deref(), repo.as_deref()),
        Command::Mark { id, noise } => cmd_mark(&cli.db, &id, noise),
        Command::Retest { id, project, all, apply } => cmd_retest(&cli.db, id.as_deref(), project.as_deref(), all, apply),
        #[cfg(feature = "semantic")]
        Command::VectorsBuild { model_dir, vectors_db } => cmd_vectors_build(&cli.db, model_dir.as_deref(), vectors_db.as_deref()),
        #[cfg(feature = "semantic")]
        Command::VectorsStatus { vectors_db } => cmd_vectors_status(&cli.db, vectors_db.as_deref()),
        #[cfg(feature = "semantic")]
        Command::VectorsRefresh { model_dir, vectors_db, budget } => {
            cmd_vectors_refresh(&cli.db, model_dir.as_deref(), vectors_db.as_deref(), budget)
        }
    }
}

/// Resolve "the current project" from the process' own working directory,
/// through the one place that does this (`project::resolve_project`). A
/// working directory that cannot even be read degrades to `None` (global
/// only) rather than erroring - this is a human-facing preview command, but
/// "what project am I in" is exactly the kind of read that should never
/// block on an unusual environment.
fn current_project_from_cwd() -> Option<String> {
    std::env::current_dir().ok().and_then(|dir| project::resolve_project(&dir))
}

// --------------------------------------------------------------------- hook

/// Everything the hook channel does that could possibly fail, collapsed to
/// "nothing to say" on any of it: a bad payload, a missing/corrupt store, or
/// nothing matching. Never panics by design, but see `cmd_hook` for the
/// belt-and-braces around a panic anyway.
///
/// This is the ONE real production delivery boundary for surfaces 1, 2 and
/// 3 (CONTRACT.md: "elke levering aan oppervlak 1, 2 of 3 legt vast dat het
/// item gevuurd heeft") - every branch below that returns a block also
/// records delivery for exactly the ids shown, via the same
/// `deliver::record_delivery` call. `check`/`why`/`Prompt`/`SessionStart`
/// (the human-facing preview commands) call the identical selection
/// functions but never this one, so a dry run never inflates "how often did
/// this actually fire".
/// What a hook run produces. THREE shapes, because the Response Guard's own
/// two verdicts each speak a different language from the injection surfaces,
/// and from each other: the surfaces hand the model `additionalContext`, a
/// BLOCK hands it a `decision`, and a WARN (added 2026-08-06) hands the
/// OWNER a top-level `systemMessage` and the model an `additionalContext`,
/// both carrying the same text, never a `decision` - see `Warn`'s own doc
/// comment below for what Claude Code's own hooks documentation confirms
/// about this shape, and the one question it still leaves open.
/// One line for the owner when this project's memory has quietly rotted,
/// or nothing at all.
///
/// Counts two things against the checkout this session started in: anchors
/// that resolve to no file (those facts fire NOWHERE) and checks that now
/// come out FALSE. Both accumulate while nobody is writing, which is
/// exactly when no gate is watching.
///
/// Debounced to once a day through a stamp file beside the store, for the
/// same reason the backup hook debounces: a line that appears every single
/// session stops being read by the third one.
///
/// SILENT ON EVERY FAILURE, per R5's read-and-inject policy: no checkout, no
/// git root, an unreadable stamp, a check that cannot run - all of it means
/// no notice, never a guess and never an error on this channel.
/// The binary named `name` sitting next to this one, with the platform's own
/// executable suffix. Mirrors `ops::install`'s own `sibling` helper: nothing
/// in this deployment is ever on PATH, so a message that names a bare
/// command someone could type is not actually runnable without this.
fn sibling(name: &str) -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    Some(dir.join(format!("{name}{}", std::env::consts::EXE_SUFFIX)))
}

fn decay_notice(store: &EventStore, db: &Path, cwd: Option<&Path>) -> Option<String> {
    let root = project::git_root(cwd?)?;
    let project = project::resolve_project(&root)?;

    let stamp = db.with_file_name("decay-notice.stamp");
    let today = time::now_iso8601().get(..10)?.to_string();
    if std::fs::read_to_string(&stamp).ok().map(|s| s.trim().to_string()) == Some(today.clone()) {
        return None;
    }

    let (mut dead, mut failing) = (0usize, 0usize);
    let mut found: Vec<absent_guard::StaleFinding> = Vec::new();
    // Every item whose check this pass actually RAN. That is the set whose
    // recovery this scan is entitled to record - see `record_stale_text` for
    // the day a healed proof went on being reported as broken.
    let mut scanned_ids: Vec<String> = Vec::new();
    for li in serve::live::live_items(store).iter().filter(|li| li.item.kind.can_fire()) {
        if li.item.project.as_deref() != Some(project.as_str()) {
            continue;
        }
        for binding in &li.item.bindings {
            if let model::item::Binding::Target { kind: model::item::TargetKind::Path, value } = binding {
                if value.contains(':') || value.starts_with('/') || value.starts_with("\\\\") {
                    continue;
                }
                if !root.join(value.replace('\\', "/")).exists() {
                    dead += 1;
                }
            }
        }
        if let Some(check) = li.item.check.as_ref() {
            let outcome = model::check::run(check, &root);
            if outcome != model::check::Outcome::CannotRun {
                scanned_ids.push(li.id.clone());
            }
            if outcome == model::check::Outcome::Fails {
                failing += 1;
                // A proof that RAN and came out false is the strongest thing
                // this system can say about a stored fact: it is provably out
                // of date. Recording it here is what makes the difference
                // between telling somebody and asking them.
                found.push(absent_guard::StaleFinding {
                    id: li.id.clone(),
                    outcome: absent_guard::StaleOutcome::Failed,
                    check: format!("{check:?}"),
                    file: root.display().to_string(),
                });
            }
        }
    }
    // Clear first, and BEFORE the early return below: a scan that finds
    // everything healthy is exactly the scan that has something to retract.
    if let Ok(next_seq) = store.get_next_seq() {
        let stale_path = absent_guard::default_stale_path(db);
        let existing = std::fs::read_to_string(&stale_path).ok();
        if let Some(text) = absent_guard::record_stale_text(
            existing.as_deref(),
            &found,
            &scanned_ids,
            next_seq.saturating_sub(1),
        ) {
            let _ = serve::store_dir::write(&stale_path, text);
        }
    }
    if dead == 0 && failing == 0 {
        return None;
    }

    // WHY THIS WRITES INTO THE SAME SIDECAR THE FILE GUARD USES. Until now
    // this scan was a dead end: it is the only thing that looks at EVERY
    // fact of a project, including the ones whose file nobody touched, and
    // all it did was print a count. The two Stop-time guards that actually
    // make somebody act only ever saw what was written during the turn. So
    // the mechanism that finds the rot could not ask, and the mechanisms
    // that ask could not see it - which is exactly why a session only ever
    // worked on staleness when the owner brought it up himself.
    //
    // Handing the findings to `absent_guard`'s staleness sidecar closes that
    // without inventing a second loop: `serve::stale_guard` already reads it
    // at Stop, already asks about one at a time, and already treats an item
    // as settled once it has been revised or retracted. Only FAILING proofs
    // go in - a dead anchor means a fact fires nowhere, which is bad but is
    // not evidence that it is wrong, and some of them are dead on purpose.

    let _ = serve::store_dir::write(&stamp, &today);
    let doctor = sibling("doctor").unwrap_or_else(|| PathBuf::from("doctor"));
    let checkouts = root.parent().unwrap_or(&root);
    Some(format!(
        "[THOR] {project}: {dead} anchor(s) point at a file that is not there, so those facts fire nowhere, and {failing} proof(s) now come out false. Run `\"{}\" --db \"{}\" --checkouts \"{}\"` to see each one named on the decay line, then repair per id with revise (the file moved) or retract (the fact went with it). That report counts EVERY project under the checkouts directory, so its two numbers are larger than this line's, which counts only {project}.",
        doctor.display(), db.display(), checkouts.display()
    ))
}

// --------------------------------------------- prohibition/advice JSON rows
//
// The one place `absent_guard::Verdict` (data only - "no rendering, no I/O",
// per that module's own doctrine) turns into the documented wire shape:
// `Command::Check`'s own doc comment (read by `--help`) names the exact six
// keys below. `check --json`, `why --json` and `hook`'s own `verdicts`
// sibling field all build their rows through this ONE function, so the
// three can never quietly disagree about what a prohibition looks like on
// the wire.

/// One item's row in the `--json` shape `check`/`why` document in their own
/// `--help` text, and the exact shape `hook`'s own `verdicts` field carries
/// too. `kind`/`severity` are the real `model::item` enums, not hand-rolled
/// strings - both already serialise snake_case (`Kind`/`Severity`'s own
/// `#[serde(rename_all = "snake_case")]`), so "house_style" comes out right
/// without this file re-deciding how to spell it. `matched_literal` is
/// `None` for advice and skipped entirely by serde rather than written as
/// `null` - "present only for a prohibition" means the KEY is absent, not
/// merely empty (see `json_rows_carry_exactly_the_documented_keys` below).
///
/// `shown` (added alongside the fix that makes every row build from `served.
/// all`, never only `served.selection.shown` - see `check_rows`'s own doc
/// comment below) is a PLAIN ADDITION, never a rename or removal: an existing
/// reader that only ever looked at the six keys this row used to carry sees
/// them completely unchanged, plus one more it can ignore. That is what makes
/// this safe to add without a version bump - see this file's own
/// `json_rows_carry_exactly_the_documented_keys` test for the exact key set,
/// now seven.
#[derive(Debug, serde::Serialize)]
struct CheckRow {
    id: String,
    kind: model::item::Kind,
    severity: Option<model::item::Severity>,
    verdict: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    matched_literal: Option<String>,
    text: String,
    shown: bool,
}

/// `absent_guard::classify_moment` run over EVERY item that applies - `all`,
/// never only the ones the context block actually shows - translated into
/// the documented `CheckRow` shape and marked with whether each one is among
/// `shown_ids`. The one function `check --json`, `why --json` and `hook`'s
/// own `verdicts` field all build on, so none of them can drift from what
/// the classifier itself decided, or from each other about which items it
/// ever even asks the question of.
///
/// THE DEFECT THIS CLOSES. Before this, `check --json` and `hook`'s own
/// `verdicts` built their rows from `served.selection.shown` alone - at most
/// `model::item::MAX_ITEMS` (4) rows, the same cap the injected context
/// block itself is held to. A rule that really would refuse the very command
/// being asked about, but that lost the four slots to heavier items, was
/// therefore invisible to anyone reading `--json` or the hook's `verdicts` -
/// including any program that reads exactly those two doors and nothing
/// else. `why --json` already read
/// `served.all` (see this binary's own `--help` text on `TargetArgs::json`,
/// before this change: "`check` emits one row per item the block would
/// actually show; `why` emits one row per item that applies at all, shown or
/// withheld"), so the fix is not a new capability, only bringing `check` and
/// `hook` up to the standard `why` already held itself to - by having every
/// caller build rows from the same full list `why` always used, with `shown`
/// now saying, per row, which half of `why`'s own distinction it falls on.
fn check_rows(all: &[RankedItem], shown_ids: &std::collections::HashSet<&str>, command: &str, content: Option<&str>) -> Vec<CheckRow> {
    let verdicts = absent_guard::classify_moment(all, command, content);
    all.iter()
        .zip(verdicts)
        .map(|(r, (id, verdict))| {
            debug_assert_eq!(r.id, id, "classify_moment must return one verdict per item, in all's own order");
            let (verdict, matched_literal) = match verdict {
                absent_guard::Verdict::Prohibition { literal } => ("prohibition", Some(literal)),
                absent_guard::Verdict::Advice => ("advice", None),
            };
            let shown = shown_ids.contains(id.as_str());
            CheckRow { id, kind: r.item.kind, severity: r.item.severity, verdict, matched_literal, text: r.item.text.clone(), shown }
        })
        .collect()
}

/// `check --json`/`why --json`'s only I/O: a JSON array on stdout and
/// nothing else - no prose, no trailing prompt, never mixed with anything
/// else this process might otherwise have printed on that path (see
/// `cmd_check`/`cmd_why`, which return immediately after calling this).
/// Serialisation over `rows` (plain owned strings/enums, nothing that can
/// fail to serialise in practice) is not expected to ever error, but this
/// still degrades to printing `[]` rather than nothing at all or a partial
/// line, so the one promise this function makes - stdout is always a single
/// valid JSON array - holds even in that should-not-happen case.
fn print_json_rows(rows: &[CheckRow]) {
    match serde_json::to_string(rows) {
        Ok(json) => println!("{json}"),
        Err(_) => println!("[]"),
    }
}

#[cfg(test)]
mod check_rows_json_tests {
    use super::*;
    use model::item::{Binding, Check, Item, Kind, Severity, TargetKind};

    fn ranked(id: &str, check: Option<Check>, severity: Option<Severity>) -> RankedItem {
        RankedItem {
            id: id.to_string(),
            item: Item {
                id: id.to_string(),
                kind: Kind::Rule,
                text: format!("{id}: some fact"),
                bindings: vec![Binding::Target { kind: TargetKind::Command, value: "git push".to_string() }],
                severity,
                project: None,
                tags: vec![],
                expires: None,
                key: None,
                falsifier: Some(format!("{id} turns out not to matter")),
                check,
            },
        }
    }

    /// THE DEFECT THIS PREVENTS: a program reading `--json` is the "door" the
    /// task's own instructions name - its shape has to be boring and stable.
    /// Proves the exact key SET the task documents, both ways: a prohibition
    /// row carries `matched_literal`, an advice row does not carry the key AT
    /// ALL (not `null` - an absent key), `severity` is always present, `null`
    /// only when the item carries none, and `shown` is always present - a
    /// plain bool, `true` when the id is in `shown_ids`, `false` when it is
    /// not, never skipped either way (unlike `matched_literal`, this key is
    /// not optional: a reader must be able to tell "not shown" from "field
    /// omitted").
    #[test]
    fn json_rows_carry_exactly_the_documented_keys() {
        let prohibited = ranked(
            "p1",
            Some(Check::Forbidden { literals: vec!["--force".to_string()] }),
            Some(Severity::Irreversible),
        );
        let advice = ranked("a1", None, None);
        let shown_ids: std::collections::HashSet<&str> = ["p1"].into_iter().collect();
        let rows = check_rows(&[prohibited, advice], &shown_ids, "git push --force", None);
        let json = serde_json::to_value(&rows).unwrap();
        let arr = json.as_array().expect("must be a JSON array");
        assert_eq!(arr.len(), 2);

        let p = arr[0].as_object().expect("row must be a JSON object");
        let mut p_keys: Vec<&str> = p.keys().map(String::as_str).collect();
        p_keys.sort_unstable();
        assert_eq!(p_keys, vec!["id", "kind", "matched_literal", "severity", "shown", "text", "verdict"]);
        assert_eq!(p["id"], "p1");
        assert_eq!(p["kind"], "rule");
        assert_eq!(p["severity"], "irreversible");
        assert_eq!(p["verdict"], "prohibition");
        assert_eq!(p["matched_literal"], "--force");
        assert_eq!(p["shown"], true, "p1 is in shown_ids");

        let a = arr[1].as_object().expect("row must be a JSON object");
        let mut a_keys: Vec<&str> = a.keys().map(String::as_str).collect();
        a_keys.sort_unstable();
        assert_eq!(
            a_keys,
            vec!["id", "kind", "severity", "shown", "text", "verdict"],
            "matched_literal must be ABSENT for advice, not merely null - shown stays present either way"
        );
        assert_eq!(a["severity"], serde_json::Value::Null, "a missing severity serialises as null, never omitted");
        assert_eq!(a["verdict"], "advice");
        assert_eq!(a["shown"], false, "a1 is not in shown_ids");
    }

    /// `check --json` and `why --json` must build rows through the identical
    /// function - proven here by exercising `check_rows` directly against a
    /// SET (`served.all` in production, marked against `served.selection.
    /// shown`), never against a single hand-picked item, so an ordering bug
    /// in the zip-based construction (`check_rows`'s own `.zip(verdicts)`)
    /// would show up as a wrong id/verdict pairing rather than passing by
    /// luck.
    #[test]
    fn rows_stay_paired_with_the_right_item_across_several_at_once() {
        let items = vec![
            ranked("i0", None, None),
            ranked("i1", Some(Check::Forbidden { literals: vec!["bad".to_string()] }), None),
            ranked("i2", None, Some(Severity::HouseStyle)),
        ];
        let shown_ids: std::collections::HashSet<&str> = ["i0", "i2"].into_iter().collect();
        // A command the fixtures' own anchor ("git push") reaches: a
        // Command-bound check is classified in the unit of a segment its
        // anchor reaches, exactly as the command guard reads it.
        let rows = check_rows(&items, &shown_ids, "git push this is bad", None);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].id, "i0");
        assert_eq!(rows[0].verdict, "advice");
        assert!(rows[0].shown, "i0 is in shown_ids");
        assert_eq!(rows[1].id, "i1");
        assert_eq!(rows[1].verdict, "prohibition");
        assert_eq!(rows[1].matched_literal.as_deref(), Some("bad"));
        assert!(!rows[1].shown, "i1 is NOT in shown_ids - a prohibition can be unshown too");
        assert_eq!(rows[2].id, "i2");
        assert_eq!(rows[2].verdict, "advice");
        assert!(rows[2].shown, "i2 is in shown_ids");
    }
}

/// `Debug` only - for a test failure message (`{other:?}`); nothing in
/// production ever formats a `HookOutput`, each variant is rendered by hand
/// in `cmd_hook`.
#[derive(Debug)]
enum HookOutput {
    /// An injection surface's block, wrapped by the caller as
    /// `hookSpecificOutput.additionalContext`.
    ///
    /// `verdicts` (added 2026-09-19) is a SEPARATE, top-level sibling of that
    /// same JSON, never mixed into `block`/`additionalContext` itself - see
    /// `hook_output_json`'s own doc comment for why the two must never touch:
    /// `additionalContext` is what Claude Code hands the model verbatim, and
    /// this field exists for a DIFFERENT reader, a program driving a local
    /// model that already calls this channel per action and wants the same
    /// prohibition/advice verdicts `check --json`/`why --json` document,
    /// without spawning a second process. One row per item in `block` -
    /// `absent_guard::classify_moment` run over the exact items selected for
    /// this call, translated by `check_rows`.
    Context { event_name: String, block: String, verdicts: Vec<CheckRow> },
    /// An injection block PLUS a one-line notice for the OWNER.
    ///
    /// WHY A SECOND AUDIENCE AT SESSION START. A memory decays while nobody
    /// is writing: files move, and the anchors that pointed at them stop
    /// firing. The health check counts that, but a count in a command
    /// somebody has to run is exactly the suggestion this project keeps
    /// refusing to rely on - the owner never once saw it, because he never
    /// ran it. This carries the same block to the model and, only when
    /// something is actually wrong, one line to him.
    ///
    /// `verdicts` - see `Context`'s own doc comment just above; the same
    /// field, for the same reason, on this variant's own `block`.
    ContextWithNotice { event_name: String, block: String, notice: String, verdicts: Vec<CheckRow> },
    /// A verdict, printed verbatim - `{"decision":"block","reason":...}`.
    /// Asks the model to reconsider (Stop) or refuses a tool call
    /// (PreToolUse) rather than adding to its context. Three surfaces share
    /// this shape: the Response Guard (`respond`, Stop), and Lane C's
    /// capture guard C2 (Stop, `capture::decide_stop`) and C3 (PreToolUse,
    /// `capture::sink_verdict`'s `Block` case) - see `serve::capture`'s own
    /// doc comment.
    Decision(Value),
    /// A WARN-tier verdict at Stop (`respond::GuardVerdict::warn_reason`):
    /// visible text that must NEVER stop the turn - the third verdict
    /// SPEC-ENFORCEMENT.md 1.1 names, that before this variant existed had no
    /// way to reach the owner at all (a rulebook rule could already carry
    /// `"tier":"warn"` and `respond::guard_verdict` would already compute the
    /// text, but nothing ever read `GuardVerdict::warn_reason` - see
    /// `respond.rs`'s own "WARN TIER" section for the state this left off
    /// at, and `hook_once`'s `Stop` arm below for where this is now built).
    ///
    /// `cmd_hook` renders this as TWO independent fields carrying the same
    /// `text`, one per audience, instead of the one shape `Context` uses: a
    /// top-level `systemMessage` for the OWNER, and
    /// `hookSpecificOutput.additionalContext` (keyed to this same
    /// `event_name`, always `"Stop"` in practice - `hook_once`'s `Stop` arm
    /// is the only place this variant is ever built) for the MODEL. Neither
    /// field's presence implies the other, and setting them never sets a
    /// `"decision"` key - a WARN must never be readable as a block.
    ///
    /// Why two fields for one string: a Response Guard warning is about the
    /// reply the assistant just finished, and at `Stop` that turn is already
    /// over - the assistant cannot act on anything unless it is blocked, and
    /// a WARN by definition never blocks, so the model-facing half can only
    /// ever land on a LATER turn. The owner is the only audience who can
    /// still do something about it now, which is the reason this variant
    /// exists at all.
    ///
    /// CONFIRMED BY THE DOCS, not merely defensive any more: `Stop` accepts
    /// `additionalContext` for non-blocking feedback in the first place -
    /// "Stop and SubagentStop also accept
    /// `hookSpecificOutput.additionalContext` for non-error feedback that
    /// continues the conversation" - but that feedback is addressed to
    /// Claude, not the owner: "For non-blocking feedback that Claude should
    /// see and act on, use `hookSpecificOutput.additionalContext` instead",
    /// landing "at the end of the turn. The conversation continues so Claude
    /// can act on the feedback" - i.e. next turn, never this one. The field
    /// that actually reaches the owner is the different, top-level one this
    /// variant now also sets: "If you want to show a message to the user
    /// without blocking Claude, return `systemMessage` instead", which the
    /// universal field table every hook event shares (not `Stop`-specific)
    /// lists as "Warning message shown to the user". And the JSON never
    /// carrying a `"decision"` key is still proven the older way too, by
    /// this variant's own tests in `serve/tests/` against the real compiled
    /// binary - so no consumer that keys its blocking behaviour off
    /// `decision`/`reason` can ever read a WARN as a block.
    ///
    /// LEFT OPEN BY THE DOCS: whether `additionalContext` at `Stop` is
    /// actually surfaced to the model when it arrives WITHOUT a `decision`
    /// key - the only way this variant ever sends it, since a WARN by
    /// definition carries no `decision`. The docs say `Stop` takes the field
    /// for non-error feedback, but every worked example in that section
    /// pairs `additionalContext` with a block; none shows it standing alone.
    /// Left unresolved rather than guessed at again: this variant does not
    /// depend on the answer to do its job, because the `systemMessage` half
    /// - the reason it exists - is confirmed on its own and reaches the
    /// owner regardless. If `additionalContext` alone turns out to be
    /// silently ignored at `Stop`, that is the same "the model does not see
    /// it" outcome this whole shape was always chosen to fail into, not a
    /// new failure mode. If either assumption is ever found wrong, only this
    /// one variant's rendering in `cmd_hook` needs to change - nothing about
    /// `hook_once`'s own ordering logic depends on the JSON shape chosen
    /// here.
    Warn { event_name: String, text: String },
}

/// Whether this payload arrives from inside a Task-tool subagent rather than
/// the owner's own main session. `agent_id` is Claude Code's own documented
/// signal for exactly this question - the hooks reference describes it as
/// "present only when the hook fires inside a subagent call... use this to
/// distinguish subagent hook calls from main-thread calls." No other field is
/// used, and none is inferred: a check that misfires on the owner's OWN main
/// session would silently blind it, which is worse than the gap this closes
/// (see INJECTION-FRAMING.md's step 3 for what else was checked and ruled
/// out).
///
/// Confirmed 2026-08-05 against Claude Code's own hooks documentation:
/// `SessionStart` fires ONLY in the main session (it never fires for a
/// subagent at all, so a subagent gate on that arm would be dead code - see
/// `INJECTION-FRAMING.md`'s own addendum); `PreToolUse`, `UserPromptSubmit`
/// and `Stop` DO fire inside a subagent, and `agent_id`/`agent_type` are only
/// ever present in that case - which is exactly why this predicate gates
/// Lane C's capture guard (C1/C2/C3, `capture_flag`/`capture_stop_check`/
/// `capture_sink_check` below): a subagent's own task prompt is written by an
/// orchestrating agent, not the owner, and is often full of the very words
/// ("always", "never", "from now on") this guard watches for - flagging it
/// would deadlock the subagent's own Stop on a "decision" the owner never
/// made.
///
/// Also read, independently, at every other subagent-scoped gate in
/// `hook_once`'s `Stop` arm: `setup_debt` (no owner in the room for a setup
/// conversation either); the Response Guard's two verdict computations
/// (`is_subagent`, local to that arm, passed INTO `respond::guard_verdict`
/// rather than gating the call - see that arm's own `is_subagent` doc
/// comment); and the three memory-upkeep debts `crowding_debt`,
/// `judgement_debt` and `stale_guard_stop_check` (the false-proof debt).
/// Settling any of those three - `mark` for `judgement_debt`,
/// `revise`/`retract` for `crowding_debt` and the false-proof debt - is a
/// write through the tool server, and every such write is stamped with a
/// session identity a subagent shares with its own main session (see
/// `crowding_debt`'s and `judgement_debt`'s own doc comments), so a subagent
/// settling any of the three would write a record that later reads as though
/// the owner dealt with it himself, though he never read the item at all - a
/// false record, worse than the gap silence leaves - and it would spend a
/// whole agent run on upkeep the owner himself will never see asked or
/// answered. Measured 2026-09-09: agents working among themselves are not
/// held by any of this; it applies only to what is addressed to the owner.
/// Each of these gates decides its own scope independently, at its own call
/// site, rather than sharing one early return - see the comments there for
/// the per-gate shape that keeps a later change to any one of them from
/// silencing another.
fn payload_is_from_a_subagent(payload: &Value) -> bool {
    payload.get("agent_id").and_then(|v| v.as_str()).is_some_and(|s| !s.is_empty())
}

/// Whether a Stop payload says the OWNER reads this answer himself:
/// `"owner_reads_answer": true`, a field THOR defines (Claude Code never sends
/// it). Only a literal JSON `true` counts; absent, `false` or anything else
/// reads as "no".
///
/// WHY IT EXISTS (2026-09-26). An agent's answer is normally read by the
/// session that spawned it, so a Stop that carries `agent_id` skips every
/// rulebook rule marked `owner_reading_only` (short, a plain summary first, no
/// reflexive disclaimers). A program that drives a local model for the owner
/// sends `agent_id` for a different reason: so its calls never count toward an
/// assistant session's upkeep or judgement debt. But the owner reads that
/// model's answers directly, in his own chat, and those are exactly the
/// answers the reading rules are for. With this field set, the Response Guard
/// judges the answer as one the owner reads, while every debt in the Stop arm
/// still sees an agent (`payload_is_from_a_subagent`) and leaves it alone.
/// Without `agent_id` the field changes nothing: the owner's own session
/// already gets every rule.
fn payload_says_the_owner_reads_the_answer(payload: &Value) -> bool {
    payload.get("owner_reads_answer").and_then(|v| v.as_bool()).unwrap_or(false)
}

/// Whether a `path:line` citation from an answer names a real line: `cited`
/// resolves (relative to `root`, or as written when absolute) to a file that
/// lies under `root` - the session's own working directory, already
/// canonicalized by the caller - and that file has at least `line` lines.
/// Line 0, a directory, a path that climbs out of `root` (`../`), and anything
/// that cannot be read all answer `false`: a citation has to be checkable to
/// count as evidence (see `respond`'s own "checked citations" section).
fn cited_line_exists(root: &Path, cited: &str, line: u64) -> bool {
    if line == 0 {
        return false;
    }
    let Ok(file) = root.join(cited).canonicalize() else { return false };
    if !file.starts_with(root) || !file.is_file() {
        return false;
    }
    let Ok(bytes) = std::fs::read(&file) else { return false };
    let newlines = bytes.iter().filter(|&&b| b == b'\n').count() as u64;
    let lines = if bytes.last().is_some_and(|&b| b != b'\n') { newlines + 1 } else { newlines };
    line <= lines
}

/// Which of `candidates` (sha-shaped tokens from an answer) name a real commit
/// in the git repository `cwd` stands in - `None` when no repository answers
/// (git missing, `cwd` in no repository, any error), so the Response Guard
/// judges a sha by its shape instead (see `respond`'s own "checked evidence"
/// section). One `git cat-file --batch-check` for the whole list, reading only
/// the local object store: nothing is fetched, nothing is written. An
/// ambiguous short sha names real objects, so it counts as real.
fn real_commits(cwd: &Path, candidates: &[String]) -> Option<std::collections::HashSet<String>> {
    use std::io::Write as _;
    let mut command = std::process::Command::new("git");
    command
        .arg("-C")
        .arg(cwd)
        .args(["cat-file", "--batch-check"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        // CREATE_NO_WINDOW: a hook never opens a console window.
        command.creation_flags(0x0800_0000);
    }
    let mut child = command.spawn().ok()?;
    let input: String = candidates.iter().map(|c| format!("{c}^{{commit}}\n")).collect();
    // A write error (git already gone) shows up in the exit status below.
    let _ = child.stdin.take()?.write_all(input.as_bytes());
    let out = child.wait_with_output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let answers: Vec<&str> = text.lines().collect();
    if answers.len() != candidates.len() {
        return None;
    }
    Some(
        candidates
            .iter()
            .zip(answers)
            .filter(|(_, answer)| !answer.ends_with(" missing"))
            .map(|(c, _)| c.clone())
            .collect(),
    )
}

/// The owner's own last typed prompt, for `respond::guard_verdict`'s
/// `list_request_any_of` exemption (FALSE BLOCK A - see
/// `respond::last_user_prompt`'s own doc comment for the full story).
/// Unlike the reply text (`last_assistant_message`, read
/// straight off the Stop payload), Claude Code does not hand the prompt over
/// the same way - it names the session's transcript file in
/// `transcript_path` instead, so this reads THAT and hands the raw text to
/// `respond::last_user_prompt`, the pure parser. Empty string on anything
/// unreadable (no `transcript_path`, the file is gone or unparseable) -
/// fail-open, same stance as every other optional signal on this path: the
/// exemption then simply does not apply, exactly the behaviour this project
/// had before it existed, never a NEW block.
fn last_user_prompt_from_payload(payload: &Value) -> String {
    let Some(path) = payload.get("transcript_path").and_then(|v| v.as_str()) else {
        return String::new();
    };
    let Ok(text) = std::fs::read_to_string(path) else {
        return String::new();
    };
    respond::last_user_prompt(&text).unwrap_or_default()
}

/// This invocation's own session identity: the payload's own `session_id`
/// when it is a real, non-empty string, or - only when that is missing -
/// a value made up fresh for this one process, never shared with any other
/// call.
///
/// THE DEFECT THIS CLOSES, measured against a copy of the live store on
/// 2026-09-08. The old fallback was the bare literal `"hook"`, shared by
/// EVERY call that ever reached it: a scratch probe over a copy of the real
/// store (never the live one) found 27 distinct items already served under
/// the literal session_id `"hook"` - proof this fallback has already fired
/// for real, more than once, and every one of those firings landed in the
/// SAME bucket. `judgement_debt`'s own session filter
/// (`EventStore::served_ids_in_session`) is only as honest as the identity
/// behind it: if this session's own call and some earlier, unrelated call
/// both fall back to the one shared constant, this session inherits
/// whatever the other one served, which is exactly the "asked to judge
/// something it never saw fire" defect this whole file's `judgement_debt`
/// and `crowding_debt` exist to prevent - just moved one layer down, into
/// the identity the filters trust rather than into the filters themselves.
///
/// A FRESH, ONE-OFF VALUE IS THE HONEST DEGRADATION. This binary is one-shot
/// per hook event (see this file's own module doc comment: JSON in, JSON
/// out, exit), so a missing `session_id` already means nothing here can
/// truthfully be tied to any other call, before or after. Manufacturing a
/// unique identity instead of a shared one cannot make that worse - a call
/// with no real session_id still records its own delivery under SOME
/// identity, and every debt that reads it back (`served_ids_in_session`,
/// the crowding watermark, the teeth-asked sidecar) simply finds nothing
/// under a brand new value and stays quiet, the same safe "owes nothing"
/// answer a session that truly saw nothing already gets. What it no longer
/// does is let two unrelated callers merge into one.
///
/// No new dependency for this: `std::process::id()` plus a nanosecond
/// timestamp is unique enough for a defensive fallback that a correctly
/// behaving Claude Code installation should never actually reach.
fn resolve_session_id(payload: &Value) -> String {
    match payload.get("session_id").and_then(|v| v.as_str()) {
        Some(id) if !id.is_empty() => id.to_string(),
        _ => fresh_session_id(),
    }
}

fn fresh_session_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    format!("no-session-id-{}-{nanos}", std::process::id())
}

#[cfg(test)]
mod resolve_session_id_tests {
    use super::*;

    #[test]
    fn a_real_session_id_is_used_as_is() {
        let payload = serde_json::json!({"session_id": "abc-123"});
        assert_eq!(resolve_session_id(&payload), "abc-123");
    }

    /// THE DEFECT THIS PREVENTS, measured against a copy of the live store on
    /// 2026-09-08: the old fallback was the bare literal `"hook"`, shared by
    /// every call that ever reached it - a scratch probe over a copy of the
    /// real store found 27 distinct items already served under that one
    /// literal session_id, proof this path has fired for real and every
    /// firing landed in the same bucket. Two calls that both lack a
    /// `session_id` must never collide on the value this function hands
    /// back, or `served_ids_in_session` inherits one caller's history for
    /// the other - the exact cross-session leak `judgement_debt`'s own
    /// session filter exists to prevent, just moved into the identity
    /// instead of the filter.
    #[test]
    fn two_payloads_missing_session_id_never_resolve_to_the_same_identity() {
        let missing = serde_json::json!({});
        let a = resolve_session_id(&missing);
        let b = resolve_session_id(&missing);
        assert_ne!(a, b, "two separate invocations must never share one manufactured identity");
    }

    #[test]
    fn a_missing_session_id_is_never_the_old_shared_literal() {
        let payload = serde_json::json!({});
        assert_ne!(
            resolve_session_id(&payload),
            "hook",
            "the fallback must no longer be a constant every caller could collide on"
        );
    }

    #[test]
    fn an_empty_string_session_id_is_treated_the_same_as_a_missing_one() {
        let payload = serde_json::json!({"session_id": ""});
        let a = resolve_session_id(&payload);
        let b = resolve_session_id(&payload);
        assert_ne!(a, b, "an empty string is not a real identity either, and must not become one shared bucket");
    }

    #[test]
    fn a_non_string_session_id_falls_back_the_same_way() {
        let payload = serde_json::json!({"session_id": 12345});
        assert_ne!(resolve_session_id(&payload), "12345");
        assert_ne!(resolve_session_id(&payload), "hook");
    }
}

/// `actor` is the actor stamped on every delivery (`ItemServed`) this call
/// makes - `usefulness::ASSISTANT_DELIVERY_ACTOR` ("hook") for an ordinary
/// run, or whatever `--actor` validated for a caller that is not an
/// assistant harness (see that constant's own doc comment, and `Command::
/// Hook`'s own doc comment on `actor`/`no_record`). `no_record` true skips
/// every one of the three `deliver::record_delivery*` calls below entirely -
/// for a measurement run - while leaving everything else, stdout included,
/// completely unaffected: `ids` is computed exactly as before either way,
/// only the write itself is skipped. It also holds the store's directory
/// read-only for the whole call (`serve::store_dir`), so no state file beside
/// the store is written either and the store is opened read-only.
fn hook_once(db_path: &Path, payload_text: Option<String>, actor: &str, no_record: bool) -> Option<HookOutput> {
    let _hold = no_record.then(|| serve::store_dir::hold(db_path));
    // `payload_text` is `Some` only when the caller already read stdin (see
    // `cmd_hook`'s `--diagnose` path, which has to read it to say anything
    // about it). stdin can only be consumed once, so it is read HERE for a
    // normal run and handed over for that one.
    let raw = match payload_text {
        Some(text) => text,
        None => {
            let mut raw = String::new();
            std::io::stdin().read_to_string(&mut raw).ok()?;
            raw
        }
    };
    let payload: Value = serde_json::from_str(&raw).ok()?;

    let event_name = payload
        .get("hook_event_name")
        .and_then(|v| v.as_str())
        .unwrap_or("PreToolUse")
        .to_string();

    let session_id = resolve_session_id(&payload);

    // Surface 5, the Response Guard - handled FIRST and without opening the
    // store, because it watches the assistant's reply, not the memory. See
    // `serve::respond` for the whole story of why this surface had to be
    // rebuilt. Reads the assistant's last message straight from the payload
    // (Claude Code puts it there), and the owner's own last prompt from the
    // transcript `transcript_path` names (`last_user_prompt_from_payload` -
    // there is no payload field for that the way there is for the reply;
    // see that function's own doc comment), refuses to re-fire when a guard
    // already fired this turn (loop safety), and asks
    // `respond::guard_verdict` for BOTH tiers at once (SPEC-ENFORCEMENT.md
    // 1.1's three verdicts, `respond::GuardVerdict`) instead of the old
    // `respond::block_reason`, which could only ever see BLOCK. A BLOCK
    // reason returns immediately below, exactly as `block_reason` always did
    // - nothing after it in this function runs. A WARN reason (no block) is
    // instead held in `warn_text` rather than returned here: the ordering
    // guarantee this task exists to keep is that a WARN must never suppress
    // or delay Lane C's capture check or the stale-rule guard below, so it
    // only ever becomes visible (as `HookOutput::Warn`) at whichever point
    // below first finds nothing else to say - see `HookOutput::Warn`'s own
    // doc comment for the JSON shape this produces and exactly what is
    // proven versus assumed about it. Every error path (no rulebook,
    // unreadable rulebook, malformed JSON) yields neither tier - `warn_text`
    // stays `None` - and falls through the same way, either to Lane C's
    // capture check below (still on this Stop event) or, for every other
    // event name, past this whole block.
    if event_name == "Stop" {
        // FIELD EVIDENCE, RECORDED ONCE (X3). Every refusal of this session
        // with no recorded verdict yet is checked against the session's own
        // transcript and the answer written to the log, at every Stop,
        // `stop_hook_active` or not: it never holds a turn, and transcripts
        // are deleted after about 30 days, so the answer has to be kept while
        // it can still be given. The main session's Stop also covers its
        // subagents' files (`serve::attest`'s reader walks them).
        // The same for every WRITE of this session through THOR's tool
        // server (step 12): verified against the transcript once every line
        // of the call is on disk, and recorded once - the only thing a
        // judgement-debt reader ever counts a verdict by.
        // AN EVALUATION WHOSE MACHINE PASS WAS CUT OFF (step 14, F10): a start
        // record with no batch means the hook that started the evaluation
        // was killed between its two writes. This Stop finishes the pass and
        // writes the batch record before anything else, and the session is
        // shown the start note with what is now its to do (`resumed_note`
        // below). Never for a subagent's Stop: the evaluation is the main
        // session's.
        let mut resumed: Vec<model::store::EvaluationStart> = Vec::new();
        if !no_record {
            if let Ok(mut store) = serve::store_dir::open_existing(db_path) {
                serve::attest::record_refusal_verifications(&mut store, &session_id);
                serve::attest::record_write_verifications(&mut store, &session_id);
                if !payload_is_from_a_subagent(&payload) {
                    // ANOTHER SESSION'S WRITES on this session's open
                    // evaluation lists (step 24): checked here, so a filing
                    // never waits on a session that went quiet.
                    serve::attest::record_foreign_write_verifications(&mut store, &session_id);
                    // THE READER OF A FILING (step 16): every evaluation
                    // report on file whose filing has no validator record
                    // yet is validated once - after the verifications above,
                    // which it reads - and the answer recorded.
                    serve::eval_filing::validate_new_reports(&mut store, serve::attest::transcripts_root().as_deref(), &session_id);
                    resumed = serve::eval_session::resume(&mut store, &session_id);
                }
            }
            // The evaluation's old handover file is gone for good (step 13):
            // the start lives in the log now, and nothing reads the file.
            serve::eval_session::retire_handover_sidecar(db_path);
        }
        let already_fired = payload.get("stop_hook_active").and_then(|v| v.as_bool()).unwrap_or(false);
        // What the session is told when this Stop finished a cut-off pass.
        // It never replaces the Response Guard below - an early return here
        // would blind it for this Stop, the mistake this arm has made twice
        // before. It RIDES WITH the guard's own verdict instead: appended to
        // its block or its warning, and a block of its own only once the
        // guard had none.
        let resumed_note: Option<String> = (!resumed.is_empty())
            .then(|| resumed.iter().map(serve::eval_session::resumed_note).collect::<Vec<_>>().join("\n\n"));
        let with_resumed = |text: String| match &resumed_note {
            Some(note) => format!("{text}\n\n{note}"),
            None => text,
        };
        let msg = payload.get("last_assistant_message").and_then(|v| v.as_str()).unwrap_or("");
        // The Response Guard's rulebook judges the SHAPE of a reply (length,
        // a summary first, evidence for a claim) for the OWNER's own
        // reading. An agent's reply is read and judged by the main session,
        // not by him, so a rule of that KIND should not reach it - but two of
        // the rulebook's own rules are not about how a reply reads, they are
        // about whether the agent told the truth (a claim of having checked
        // something with no evidence, a claim that something could not be
        // reached without trying), and those must keep catching a subagent
        // exactly as they catch the owner's own main session: a subagent
        // that lies about either is the same lazy-agent behaviour this whole
        // project exists to catch, regardless of who reads the lie. So the
        // call into `respond::guard_verdict` is never skipped for a
        // subagent: `is_subagent` is PASSED INTO it instead, and each rule in
        // the rulebook carries its own optional `owner_reading_only` field
        // deciding whether THAT rule is exempt for a subagent payload - see
        // `respond.rs`'s own "READER SCOPE" doc comment, right above
        // `evaluate_opt_in`, for the field, its default, and the 2026-09-09
        // measurement (15 of 25 real agent reports refused on style rules
        // alone, zero on the honesty rules) that decided it.
        //
        // Computed once, here, and read by both verdict computations below
        // (the `already_fired` retry pass and the ordinary first pass), each
        // passing it into its OWN call into `respond::guard_verdict` rather
        // than sharing one early return - this project has measured twice
        // what a single early return across several gates does to the ones
        // after it (the rule about a loop guard blinding a whole hook). Every
        // debt below (`setup_debt`, Lane C's `capture_stop_check`,
        // `crowding_debt`, `judgement_debt`, `stale_guard_stop_check`) and
        // the backlog burn (`teeth_debt`) reuses this same value but decides
        // its own subagent scope independently, at its own call site.
        let is_subagent = payload_is_from_a_subagent(&payload);
        // Who READS this answer, which is a different question from who
        // wrote it: an agent's answer is normally read by the session that
        // spawned it, but a caller can say the owner reads it himself
        // (`payload_says_the_owner_reads_the_answer`). Only the Response
        // Guard's reader-scoped rules read this; every debt below keeps
        // reading `is_subagent`, so the agent's own session separation holds.
        let reader_is_an_agent = is_subagent && !payload_says_the_owner_reads_the_answer(&payload);
        // A `path:line` citation, a line number next to a file name, or a
        // commit in the answer counts as evidence only when it is real: a line
        // under the session's own working directory, a commit in its git
        // repository - and only when the payload names a working directory
        // that exists; without it the Response Guard judges all of them by
        // their shape, as before. See `respond`'s own "checked evidence"
        // section, `cited_line_exists` and `real_commits`.
        let payload_cwd = payload.get("cwd").and_then(|v| v.as_str()).map(PathBuf::from);
        let evidence_root: Option<PathBuf> = payload_cwd.as_deref().and_then(|cwd| cwd.canonicalize().ok());
        let look_up_citation =
            |cited: &str, line: u64| evidence_root.as_deref().is_some_and(|root| cited_line_exists(root, cited, line));
        // git is handed the working directory as the payload names it, never
        // the canonical form: on Windows that is a `\\?\` path, which is not
        // one every git build reads.
        let look_up_commits = |candidates: &[String]| payload_cwd.as_deref().and_then(|cwd| real_commits(cwd, candidates));
        let evidence_lookup =
            respond::EvidenceLookup { line_exists: &look_up_citation, real_commits: &look_up_commits };
        let citation_lookup: Option<&respond::EvidenceLookup> =
            if evidence_root.is_some() { Some(&evidence_lookup) } else { None };
        // THE DEFECT THIS CLOSES, reported by the owner on 2026-08-09: "my
        // TLDR rules worked an hour ago and now they are gone."
        //
        // `stop_hook_active` means a Stop hook already held this turn once.
        // Returning None on it was loop safety, and it was too wide: it
        // switched off the Response Guard as well as the debts. That went
        // unnoticed while blocks were rare. The moment one debt started
        // firing every turn, EVERY follow-up reply landed in an already-fired
        // turn, and the guard silently stopped watching any of them - exactly
        // the replies most likely to need it, since they come after a nudge.
        //
        // So the second pass still reads the reply and still says what it
        // found, as a WARN. A warning cannot hold the turn, so it cannot
        // loop; the loop safety that matters is keeping BLOCK to one pass,
        // not going blind. Only the EMPTY-message case is an early-out here
        // any more (`is_subagent` is no longer folded into it - see this
        // arm's own `is_subagent` doc comment above for why): a subagent
        // payload still reaches `respond::guard_verdict` below, exactly like
        // the owner's own main session, and it is `guard_verdict` itself
        // that decides, rule by rule, whether `is_subagent` excuses this
        // particular fire.
        if already_fired {
            // A Stop that already held this turn once may not hold it again
            // (loop safety): a resumed note still arrives, as a warning.
            // ONE EXCEPTION (step 24): the evaluation this session owes. Field
            // case 2026-10-01: both sessions did their evaluation work inside
            // the turn the ask held, ended it to "file next turn", and this
            // branch let both go silently - no next turn ever came. So while
            // this session's evaluation is open and unfiled (or was just
            // voided and is still due), this Stop holds again - but only after
            // the session did something since the last hold, and at most
            // `REHOLD_MAX` times in one turn, so it can never loop.
            let guard_text = (!msg.trim().is_empty()).then(|| {
                let rulebook_text = std::fs::read_to_string(respond::default_rulebook_path(db_path)).ok();
                let last_prompt = last_user_prompt_from_payload(&payload);
                let verdict =
                    respond::guard_verdict(rulebook_text.as_deref(), msg, &last_prompt, reader_is_an_agent, citation_lookup);
                verdict.block_reason.or(verdict.warn_reason)
            });
            let guard_text = guard_text.flatten();
            let owed = match is_subagent || no_record {
                true => None,
                false => evaluation_after_a_hold(db_path, &session_id, &payload),
            };
            match owed {
                Some(Rehold::Hold(reason)) => {
                    let parts: Vec<String> = resumed_note.clone().into_iter().chain(guard_text.clone()).chain(std::iter::once(reason)).collect();
                    return Some(HookOutput::Decision(serde_json::json!({ "decision": "block", "reason": parts.join("\n\n") })));
                }
                Some(Rehold::Say(line)) => {
                    let parts: Vec<String> = resumed_note.clone().into_iter().chain(guard_text.clone()).chain(std::iter::once(line)).collect();
                    return Some(HookOutput::Warn { event_name, text: parts.join("\n\n") });
                }
                None => {}
            }
            let text = match guard_text {
                Some(text) => with_resumed(text),
                None => resumed_note?,
            };
            return Some(HookOutput::Warn { event_name, text });
        }
        // A first-pass Stop of a main session: where the log stands, so a
        // later Stop in this same turn can tell whether the session did
        // anything since (`evaluation_after_a_hold`).
        if !is_subagent && !no_record {
            record_turn_mark(db_path, &session_id);
        }
        // Where this Stop stands, resolved once, BEFORE any verdict below can
        // return: the folder, and the project the session is counted in.
        let stop_root: Option<PathBuf> =
            payload.get("cwd").and_then(|v| v.as_str()).map(PathBuf::from).or_else(|| std::env::current_dir().ok());
        // A working directory that belongs to no project falls back to the
        // project this session last worked in (`project::session_project`,
        // the same answer the work clock and the hard guards use) - otherwise
        // a `cd` into a scratch folder silences every debt the session still
        // owes there.
        let stop_project = project::session_project(db_path, &session_id, stop_root.as_deref());
        // EVERY FIRST-PASS STOP IS COUNTED (step 20, NEW-1), whoever holds
        // it: a Stop the Response Guard (or any debt below) holds is still a
        // Stop of this session, and the first-Stop gate (Q10) reads this
        // count. It used to be taken only when nothing earlier had returned,
        // so a session whose replies the guard always held was never counted
        // and never reached its second Stop. A subagent's Stop is work of its
        // session, never a Stop that counts or can be held.
        let stop_now = time::now_unix();
        if is_subagent {
            usefulness::record_hook_event(db_path, stop_project.as_deref(), &session_id, true, stop_now, usefulness::HookEventKind::Other);
        } else {
            usefulness::record_main_stop(db_path, stop_project.as_deref(), &session_id, stop_now);
        }
        // FORMAT DRIFT (D14): this session's transcript is in a shape THOR
        // cannot read. Then no report can be checked or counted, so the
        // evaluation is not asked for - it could not be filed - and every
        // turn says so instead, riding with whatever else this Stop says.
        let drift_note: Option<&str> = (!is_subagent && stop_project.is_some() && !no_record)
            .then(|| serve::eval_filing::session_drifted(db_path, serve::attest::transcripts_root().as_deref(), &session_id))
            .filter(|drifted| *drifted)
            .map(|_| serve::eval_filing::DRIFT_LINE);
        // Same shape for the ordinary first pass: only an empty message
        // skips the call into `respond::guard_verdict` now. A subagent
        // payload still computes a real verdict - `warn_text` (and the BLOCK
        // returned just below, from `verdict.block_reason`) can still carry
        // whichever rules `owner_reading_only` did NOT excuse for this
        // payload, the same as the owner's own main session would see for
        // those same rules.
        let warn_text: Option<String> = if msg.trim().is_empty() {
            None
        } else {
            let rulebook_text = std::fs::read_to_string(respond::default_rulebook_path(db_path)).ok();
            let last_prompt = last_user_prompt_from_payload(&payload);
            let verdict = respond::guard_verdict(rulebook_text.as_deref(), msg, &last_prompt, reader_is_an_agent, citation_lookup);
            if let Some(reason) = verdict.block_reason {
                // THE GUARD NEVER STARVES THE EVALUATION ASK (step 20,
                // NEW-1). The guard's block holds this turn once and the
                // retry cannot hold it again, so an ask that waited for a
                // Stop the guard let through would never come to a session
                // whose replies are always too long or lack a summary. The
                // ask - with the start note of the evaluation it starts -
                // rides with the guard's block instead, the way a resumed
                // machine pass's note does (step 14): the guard is never
                // skipped, and neither is the ask.
                let text = match is_subagent {
                    true => reason,
                    false => with_eval_ask(db_path, &session_id, stop_root.as_deref(), stop_project.as_deref(), drift_note, no_record, reason),
                };
                return Some(HookOutput::Decision(serde_json::json!({
                    "decision": "block",
                    "reason": with_resumed(text),
                })));
            }
            verdict.warn_reason
        };
        // The Response Guard had no block: a finished machine pass holds this
        // Stop once, with the start note (and the guard's warning, if any).
        if let Some(note) = resumed_note {
            let reason = match warn_text {
                Some(warning) => format!("{note}\n\n{warning}"),
                None => note,
            };
            return Some(HookOutput::Decision(serde_json::json!({ "decision": "block", "reason": reason })));
        }

        // Lane C, C2 (SPEC-ENFORCEMENT.md 2.2) - the Response Guard above had
        // no BLOCK to return (a WARN, if any, is only pending in `warn_text`
        // so far, not yet returned), so this is still the one Stop path that
        // may open the store, and only read-only. See `capture_stop_check`'s
        // own doc comment.
        //
        // THE FOURTH DEBT (`setup_debt`) and Lane C's C2 (`capture_stop_check`)
        // share this one `!is_subagent` check: both are about who is on the
        // other end of the conversation, not about the store's own state.
        // Lane C never blocks a subagent's Stop over a "decision" that lives
        // only in its own delegated task prompt
        // (`payload_is_from_a_subagent`'s own doc comment) - the prompt was
        // written by an orchestrating agent, not the owner, so there is no
        // decision of HIS to attribute. `setup_debt` never walks a subagent
        // through AGENTS.md's setup questions - there is no owner in the
        // room to walk through anything (see `setup_debt`'s own doc
        // comment). A per-gate check, not the wider, shared early return
        // this arm once had - the shape that also swallowed the Response
        // Guard's own subagent behaviour (see the rule about a loop guard
        // blinding a whole hook, and `is_subagent`'s own doc comment above).
        // THE EVALUATION ASK RIDES WITH THESE BLOCKS TOO (step 21), as it
        // rides with the Response Guard's: setup, the capture check and the
        // crowding question below each hold the turn once, and an ask that
        // waited for a Stop none of them held could wait for good. The block
        // is never skipped; the ask, when this session owes it, follows its
        // reason in the same block, and the Stop was counted once above.
        let hold = |reason: String| {
            let text =
                with_eval_ask(db_path, &session_id, stop_root.as_deref(), stop_project.as_deref(), drift_note, no_record, reason);
            Some(HookOutput::Decision(serde_json::json!({ "decision": "block", "reason": text })))
        };
        if !is_subagent {
            if let Some(reason) = serve::store_dir::open_existing(db_path).ok().and_then(|s| setup_debt(&s)) {
                return hold(reason);
            }

            if let Some(reason) = capture_stop_check(db_path, &session_id) {
                return hold(reason);
            }
        }

        // THE MESS YOU MADE THIS SESSION, before the older debt below it.
        // Asked first because it is the only one whose answer decays: the
        // person who wrote the fact still knows what it was for, and tomorrow
        // nobody does.
        // The project THIS session is working in, from the payload's own cwd
        // (`stop_project`, resolved at the top of this arm, before the
        // Response Guard, because the shared `session_project` below is
        // computed after this early Stop branch returns). It routes the
        // crowding nag to the session whose project the fact belongs to (see
        // crowding_debt). (`teeth_debt` no longer takes
        // the folder: since step 9 it reads `model::store::settlement`, which
        // judges a rule against its project's REGISTERED working copy, never
        // the folder a Stop ran in.)
        // GATED ON `is_subagent`: a fact landing on an already-full anchor is
        // still true of the STORE no matter who wrote it, but PAYING this
        // debt (fold or re-anchor with `revise`, retract, or the
        // `crowded-on-purpose` tag) is a write through the tool server,
        // which stamps every write it makes - a subagent's included - with
        // the same session identity the owner's own main session would
        // carry (see this function's own watermark note on the shared
        // constant `"mcp"`), so nothing in the store afterward would show
        // that a delegate resolved it rather than him. A false record like
        // that is worse than the gap silence leaves, and the owner will
        // never see this maintenance question either way, so holding a
        // subagent's turn for it spends a whole agent run on nothing he
        // asked for. Agents among themselves are not held by this; it
        // applies only to what is addressed to him.
        if !is_subagent {
            if let Some(reason) = serve::store_dir::open_existing(db_path)
                .ok()
                .and_then(|s| crowding_debt(&s, db_path, &session_id, stop_project.as_deref()))
            {
                return hold(reason);
            }
        }

        // THE EVALUATION DEBT (2026-09-12, moved ahead of the judgement debt
        // below; its own trigger rewritten SEVEN TIMES, three of them on
        // 2026-09-16, three on 2026-09-17 - see `usefulness`'s own
        // "evaluation debt" section for the full story). Everything below
        // this asks about ONE item at a time - the busiest owed one,
        // batched, still one item's own verdict per `mark` call. This asks a
        // different question: does NO evaluation report for this project
        // still COVER it (a report covers the next `usefulness::
        // EVAL_REPORT_COVERS_HOURS` hours from the Stop that first saw it -
        // `usefulness::eval_report_covers`), and has THIS session already
        // accrued enough work here (`usefulness::EVAL_FIRST_WORK_MINUTES`,
        // or `usefulness::EVAL_REPEAT_WORK_MINUTES` once a covering report
        // already exists - `usefulness::eval_debt_owed`) for the honest
        // answer to be the WHOLE end-of-session routine (`ops::install::
        // seed_eval_command`'s file, `/thor-eval`) rather than one more
        // item. `stop_project` (resolved above, shared with `crowding_debt`/
        // `judgement_debt` below) is reused for the identical reason: this
        // checkout's own project, never a backlog that belongs to a
        // different one.
        //
        // THE ACCRUAL ITSELF (`usefulness::record_hook_event`, fifth
        // rewrite, 2026-09-17) RUNS UNCONDITIONALLY FOR EVERY STOP OF A
        // CHECKOUT THAT RESOLVES TO A PROJECT, subagent included - a
        // subagent's own Stop still counts as work of ITS session (see that
        // function's own doc comment), it simply can never be the Stop that
        // BLOCKS, exactly like every other debt here. This is the store-free
        // half: it reads and writes only the small sidecar beside the store,
        // never the store itself, so it costs nothing close to what
        // `update_eval_debt_state` below does.
        //
        // ASKED BEFORE `judgement_debt` BELOW, ON PURPOSE, since 2026-09-16:
        // an evaluation settles the judgement debt anyway - it is the same
        // backlog, judged in bulk instead of one item at a time - so asking
        // for one more single item first would waste the very turn that was
        // about to settle the whole backlog.
        //
        // THE SIDECAR'S REPORT-DETECTION HALF (`usefulness::update_eval_
        // debt_state`, which DOES open the store) RUNS UNCONDITIONALLY HERE
        // TOO, before the gate below, FOR EVERY CHECKOUT THAT RESOLVES TO A
        // PROJECT - but only for a MAIN session (`!is_subagent`, unlike the
        // accrual above): it is how this project's own facts (how long ago
        // THOR started tracking it, and whether an evaluation report has
        // been seen for it) stay true on every main-session Stop inside that
        // project, regardless of whether this particular turn ends up
        // blocked - a turn that is never blocked must still leave the
        // record exactly as accurate as one that was. Run BEFORE the
        // accrual's own call so a report this very Stop first notices can
        // reset THIS session's own accrual immediately, in the same turn,
        // rather than waiting for a later event to notice the sidecar
        // changed underneath it. NEVER RUN AT ALL WHEN `stop_project` IS
        // `None` - see the `stop_project.is_some()` paragraph below for why.
        //
        // NO LONGER GATED ONCE PER SESSION (retired 2026-09-16, fourth
        // rewrite - decision by the owner: he does not want to ever have to
        // run an evaluation himself, and a debt that could be waited out
        // once per session was not enough). `evaluation_debt` below now
        // decides the whole thing on its own, reading the accrual `record_
        // hook_event` just computed rather than measuring anything itself.
        // The debt now BLOCKS THE FIRST STOP OF EVERY TURN for as long as it
        // holds - the "once per turn, not once per session" loop safety this
        // needs is the SAME `stop_hook_active` handling already at the very
        // top of this `Stop` arm (the `already_fired` branch above), not a
        // second copy of it: a retried Stop within the SAME turn never
        // reaches this far down at all. `usefulness::record_eval_debt_asked`
        // below, on the other hand, DOES run again on every fresh turn the
        // debt fires for - it is not a once-only marker, it is a running
        // count of how many times this project has now been asked since its
        // last report.
        //
        // THE BLOCKING CHECK ITSELF STAYS GATED ON `is_subagent`,
        // independently, at its own call site, same as every debt here: a
        // subagent cannot itself type `/thor-eval` - there is no owner in
        // the room for that any more than there is for `setup_debt`'s own
        // conversation - so holding its turn over a routine only the owner
        // can run would spend a whole agent run asking for something it
        // cannot do. The report-detection sidecar update is skipped for a
        // subagent too - see `update_eval_debt_state`'s own doc comment on
        // why "every main-session Stop" excludes it - though the accrual
        // above is NOT: see that paragraph above for why the two differ.
        //
        // ALSO GATED ON `stop_project.is_some()`, at this same call site,
        // since 2026-09-16: silencing this debt for good needs a live Report
        // tagged `evaluation-report`, and `model::gate`'s ground 21
        // (`NO_SCOPE_PROBLEM`) refuses to declare a Report - or any other
        // archive-kind item - with no project at all (see that constant's
        // own doc comment for the refusal text). A checkout that resolves to
        // no project could otherwise be asked forever, with no honest way to
        // ever silence it - exactly the trap this half of the gate exists to
        // close. Both the accrual and the report-detection update are
        // skipped for the identical reason, not only the ask itself: growing
        // either half of the sidecar under the empty-project key
        // (`usefulness::project_key`) for a checkout that can never file the
        // report that would read it back would only grow state nothing can
        // ever act on. A "" entry already sitting in a sidecar written
        // before this gate existed is left exactly as it is - inert from
        // here on, never read for a no-project checkout any more, and never
        // deleted either. `evaluation_debt` below carries the identical
        // guard a second time, on purpose (see its own doc comment), so the
        // rule still holds even if a future caller reaches it some other
        // way.
        // STEP 17 (B8, D11, D12, F14, Q10): the clock is the PROJECT's, and
        // the ask follows the work - every project this session worked in
        // since that project's newest valid evaluation, the folder's own
        // first - but lands only on a session with ten minutes of its own
        // work there, never at its first Stop there, and not while another
        // session runs that project's evaluation (for at most an hour). See
        // `eval_stop_ask` and `usefulness::stop_decision`. A subagent's Stop
        // still counts as work of its session, never as a Stop that can be
        // held. What the paragraphs above say about `stop_project.is_some()`
        // now holds per project: the facts are refreshed, and the ask made,
        // for every project this session worked in - from a folder of no
        // project too - while the no-project key itself is still never
        // written.
        // The Stop itself was counted above, before the Response Guard
        // (step 20).
        let mut elsewhere_note: Option<String> = None;
        if !is_subagent {
            match stop_eval_decision(db_path, &session_id, stop_root.as_deref(), stop_project.as_deref(), drift_note.is_none(), no_record) {
                EvalStop::Ask(reason) => {
                    return Some(HookOutput::Decision(serde_json::json!({ "decision": "block", "reason": reason })));
                }
                EvalStop::Elsewhere(line) => elsewhere_note = Some(line),
                EvalStop::Quiet => {}
            }
        }
        // What rides with whatever this Stop still says: the drift line and
        // the running-evaluation line.
        let riders: Vec<String> = drift_note.map(str::to_string).into_iter().chain(elsewhere_note).collect();
        let with_drift = |reason: String| match riders.is_empty() {
            true => reason,
            false => format!("{}

{reason}", riders.join("

")),
        };

        // The one debt nobody ever pays voluntarily. Independent of the
        // capture guard above it - that one can be switched off and usually
        // is, and this must not go quiet with it. `stop_project` (resolved
        // above for `crowding_debt`) is reused here for the identical reason:
        // this session's own project right now, not just its own session_id.
        //
        // GATED ON `is_subagent`: `mark`'s own verdict is stamped with a
        // session identity a subagent shares with its own main session (see
        // this function's own doc comment, "WHY THIS IS A WATERMARK... AND
        // NOT A FILTER ON THE MARK EVENT'S OWN session_id COLUMN"), so a
        // subagent that pays this debt settles it under a name that reads,
        // from the store's side, exactly like the owner's own verdict -
        // without the owner ever having read the item himself. A false
        // record like that is worse than the gap silence leaves, and the
        // question is one the owner will never see asked or answered
        // regardless, so holding a subagent's turn for it wastes a whole
        // agent run. Agents among themselves are not held by this debt; it
        // applies only to what is addressed to him.
        if !is_subagent {
            if let Some(reason) = serve::store_dir::open_existing(db_path)
                .ok()
                .and_then(|s| judgement_debt(&s, db_path, &session_id, stop_project.as_deref()))
            {
                return Some(HookOutput::Decision(
                    serde_json::json!({ "decision": "block", "reason": with_drift(reason) }),
                ));
            }
        }

        // The backlog burn: one fact per turn that LOOKS armable and has
        // never been asked. Last of the debts on purpose - it is the only one
        // with no urgency, and it must never speak over a mess made this
        // session or a verdict that is owed. Still gated on `!is_subagent`,
        // unchanged by today's fix: not one of the four debts it names, left
        // exactly as it already stood.
        // THE SESSION'S OWN GATES (step 21, C3 B2 L86): like the evaluation
        // ask (F14, Q10), never a session's first Stop in the project, and
        // only with ten minutes of its own work there - a short task session
        // never answers for a backlog it did not touch.
        if !is_subagent && teeth_upkeep_may_hold(db_path, &session_id, stop_project.as_deref()) {
            if let Some(reason) = serve::store_dir::open_existing(db_path)
                .ok()
                .filter(|_| teeth_not_yet_asked_this_session(db_path, &session_id))
                .and_then(|s| teeth_debt(&s, db_path, stop_project.as_deref()))
            {
                record_teeth_asked(db_path, &session_id);
                return Some(HookOutput::Decision(
                    serde_json::json!({ "decision": "block", "reason": with_drift(reason) }),
                ));
            }
        }

        // The stale-rule guard (`serve::stale_guard`) - AFTER both the
        // Response Guard above and Lane C's capture guard just above, on
        // purpose: it never overrides either, only fills the silence when
        // neither had anything to say. See `stale_guard_stop_check`'s own
        // doc comment for the full doctrine.
        //
        // GATED ON `is_subagent`: settling one outstanding entry here
        // (`stale_guard::item_settled`) is itself a `revise` or a `retract`,
        // the same tool-server write `crowding_debt` above already makes
        // under a session identity a subagent shares with the owner's own -
        // the same false-record risk, on top of the same wasted agent run
        // for a maintenance question the owner will never see. A pending
        // WARN still loses to a maintenance BLOCK whenever this DOES run -
        // only once NEITHER has anything to say does the WARN, if any,
        // finally reach the owner, on the very last line below.
        if !is_subagent {
            if let Some(output) = stale_guard_stop_check(db_path, &session_id, actor) {
                return Some(output);
            }
        }
        let warn_text = match (warn_text, riders.is_empty()) {
            (Some(text), false) => Some(format!("{}\n\n{text}", riders.join("\n\n"))),
            (None, false) => Some(riders.join("\n\n")),
            (text, true) => text,
        };
        return warn_text.map(|text| HookOutput::Warn { event_name, text });
    }

    // The session's own working directory, and the two project answers
    // resolved from it - all resolved ONCE for every surface below. Claude
    // Code puts the session's working directory in "cwd" on every hook
    // payload; a payload without one falls back to this process' own
    // directory rather than assuming. `session_cwd` is kept (not just
    // consumed into a project key) because the hard guards
    // (`pre_tool_guard_block` below) also need a real filesystem root to
    // prove a check's own currency against - the top of its checkout
    // (`project::guard_root`), not the folder itself. An unresolvable
    // project means the global layer only (project::applies_to says why that
    // is the safe direction); an unresolvable cwd means that guard can never
    // prove currency for anything, so it never blocks either (see
    // `absent_guard::find_violation`'s own `root: Option<&Path>`).
    //
    // TWO PROJECT ANSWERS, on purpose. `folder_project` is the folder's own
    // answer: what a session is SHOWN (session start, the prompt and
    // pre-tool serving) stays scoped to where it stands. `session_project`
    // (`project::session_project`) falls back to the project this session
    // last worked in when the folder belongs to none - the answer the work
    // clock, the evaluation and the hard guards use, so a `cd` into a scratch
    // folder never switches a project's guards off (D4): its text guards
    // through this answer, its path guards through `project::guard_root`.
    let session_cwd = payload
        .get("cwd")
        .and_then(|v| v.as_str())
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok());
    let folder_project = session_cwd.as_deref().and_then(project::resolve_project);

    // THE EVALUATION DEBT'S OWN ACCRUAL (`usefulness::record_hook_event`,
    // fifth rewrite, 2026-09-17 - see `usefulness`'s own "evaluation debt"
    // section): every hook event of a session inside a project counts
    // toward how long it has worked there, not only `Stop` (which also
    // calls this same function, in its own arm above) - `SessionStart`,
    // `UserPromptSubmit` and `PreToolUse` (this match's own three arms
    // below) all reach here first. Unconditioned by `is_subagent`, on
    // purpose: a subagent's own events still count as work of ITS session
    // (see that function's own doc comment) even though only a main
    // session's `Stop` can ever be blocked for it. Deliberately store-free -
    // it reads and writes only the small sidecar beside `db_path`, never
    // `EventStore::new` below - which is what keeps this affordable on
    // EVERY `PreToolUse` call rather than only at `Stop`.
    //
    // THE RISK CLASSIFICATION (sixth rewrite, 2026-09-17 - see `usefulness`'s
    // own "the repeat's own risk" doc comment) is decided here, straight off
    // the raw payload, before the match below ever parses `tool_name`/
    // `file_path` for its own purposes: `usefulness::classify_hook_event`
    // needs nothing but these borrowed fields, all already sitting in
    // `payload` regardless of which of the three event shapes this actually
    // is - a field that means nothing for a given shape (`tool_name` on a
    // `SessionStart`, `source` on a `PreToolUse`) simply reads back empty/
    // `None` and classifies as `Other`, exactly as it should. `command` is
    // read the same generic way the informational surface below already
    // does (`absent_guard::proposed_command` - "nothing here hard-codes
    // that name"), so a `Bash` or `PowerShell` call alike is recognised.
    // The same fallback the Stop arm uses: work done after a `cd` into a
    // folder that belongs to no project still counts toward the project this
    // session was working in. The evaluation and the hard guards below use
    // this same answer; only serving keeps the folder's own.
    let session_project = project::session_project(db_path, &session_id, session_cwd.as_deref());
    // This session's accrued work in its project, as of this very event: what
    // a skill call opening the evaluation routine is judged by (whether the
    // evaluation is due now, `skill_call_answer`).
    let hook_work = if session_project.is_some() {
        let hook_tool_input = payload.get("tool_input");
        let hook_tool_name = payload.get("tool_name").and_then(|v| v.as_str()).unwrap_or("");
        let hook_file_path = hook_tool_input.and_then(|t| t.get("file_path")).and_then(|v| v.as_str());
        let hook_command = absent_guard::proposed_command(hook_tool_input);
        let hook_source = payload.get("source").and_then(|v| v.as_str());
        let event_kind = usefulness::classify_hook_event(&event_name, hook_tool_name, hook_file_path, hook_command, hook_source);
        usefulness::record_hook_event(db_path, session_project.as_deref(), &session_id, false, time::now_unix(), event_kind)
    } else {
        None
    };

    let mut store = serve::store_dir::open_for_hook(db_path)?;

    match event_name.as_str() {
        "SessionStart" => {
            // Surface 1. Claude Code's own SessionStart payload carries the
            // session's working directory under "cwd"; resolve "the current
            // project" from it through the one place that does this
            // (project::resolve_project - a marker file wins, else the git
            // root's basename, else global-only). A payload with no "cwd" at
            // all (never observed from Claude Code itself, but this channel
            // must degrade rather than assume) falls back to this process'
            // own working directory.
            //
            // No subagent gate on the standing-rules block below: confirmed
            // 2026-08-05 against Claude Code's own hooks documentation that
            // `SessionStart` fires ONLY in the main session and never inside
            // a subagent at all, so a subagent check on that block can never
            // execute - it was removed as dead code that looked like
            // protection (see `INJECTION-FRAMING.md`'s own addendum and
            // `payload_is_from_a_subagent`'s doc comment, which now gates the
            // three surfaces that actually DO fire inside a subagent). The
            // compact reminder below still checks it anyway, defensively -
            // see `session_start::compact_reminder`'s own doc comment for
            // why that is not the same dead code again.
            // Where the log stands right now, so the crowding debt at Stop can
            // tell what THIS session wrote from what was already there. It
            // cannot use the session id for that: every write through the tool
            // server is stamped with a constant, never the caller's own
            // session - see `crowding_debt`.
            record_session_watermark(db_path, &session_id);

            let candidates = serve::live::always_candidates(&store);
            let decay = DecayContext::load(&store);
            let items = serve::decay::retain_live(
                session_start::select(&candidates, folder_project.as_deref()),
                &decay,
            );
            let rules_block = session_start::render(&items);

            // Its own gate, at its own call site, deliberately never chained
            // onto `rules_block`'s own early return above: a session with no
            // live Always item must still get this reminder when it just
            // went through a compaction (see `session_start::compact_
            // reminder`'s own doc comment) - letting the rules block's `?`
            // swallow it would silence the reminder on exactly the sessions
            // that have nothing else to show. `payload_is_from_a_subagent`
            // is passed in rather than checked inside `compact_reminder`
            // itself for the same reason every other gate in this match
            // keeps that check local: see that function's own doc comment.
            let source = payload.get("source").and_then(|v| v.as_str());
            let reminder = session_start::compact_reminder(source, payload_is_from_a_subagent(&payload));

            let block = match (rules_block, reminder) {
                (Some(rules), Some(reminder)) => format!("{rules}\n\n{reminder}"),
                (Some(rules), None) => rules,
                (None, Some(reminder)) => reminder,
                (None, None) => String::new(),
            };
            // One line, for seven days after this project's working copy was
            // registered or moved: the first registration is first-come
            // (N11), so whoever made it is named where the owner looks - see
            // `project::registration_notice`.
            let block = match (block.is_empty(), project::registration_notice(&store, folder_project.as_deref(), time::now_unix())) {
                (_, None) => block,
                (true, Some(line)) => line,
                (false, Some(line)) => format!("{block}\n\n{line}"),
            };
            // Handed over (2026-09-25), capped: this project's own open
            // points, so a fresh session is never held by the evaluation
            // debt later and given no list at all - see `open_points_
            // session_start_note`'s own doc comment. Combined with the
            // block above AFTER it is built, never inside the match above,
            // so a session with nothing else to show still gets this on its
            // own rather than staying silent.
            // Never a subagent's (step 16, B6): the open points - and the
            // owner-queue banner above them - are for the owner's own session.
            let open_points = (!payload_is_from_a_subagent(&payload))
                .then(|| open_points_session_start_note(&store, folder_project.as_deref()))
                .flatten();
            let block = match (block.is_empty(), open_points) {
                (true, None) => return None,
                (true, Some(note)) => note,
                (false, None) => block,
                (false, Some(note)) => format!("{block}\n\n{note}"),
            };

            let ids: Vec<String> = items.iter().map(|r| r.id.clone()).collect();
            // "session start" - the one trigger this surface can ever have:
            // every item here is the pinned `Always` block, never a reaction
            // to a particular file or command. See `ItemServed::trigger`'s
            // own doc comment for the other two shapes.
            if !no_record {
                deliver::record_delivery_with_trigger(
                    &mut store,
                    &session_id,
                    &session_id,
                    actor,
                    &time::now_iso8601(),
                    &ids,
                    Some("session start"),
                );
            }
            // No `verdicts` here - see `HookOutput::Context`'s own doc
            // comment: the classifier answers "is this a prohibition for the
            // thing about to happen", and session start proposes nothing at
            // all (it is the pinned `Always` block, not a reaction to a real
            // command or write) - empty is the honest answer, never a guess
            // dressed up as advice for every item shown.
            match decay_notice(&store, db_path, session_cwd.as_deref()) {
                Some(notice) => Some(HookOutput::ContextWithNotice { event_name, block, notice, verdicts: Vec::new() }),
                None => Some(HookOutput::Context { event_name, block, verdicts: Vec::new() }),
            }
        }
        "UserPromptSubmit" => {
            // Surface 3. Claude Code's UserPromptSubmit payload carries the
            // raw text under a top-level "prompt" field (never under
            // tool_input, which is a PreToolUse-only shape).
            let prompt_text = payload.get("prompt").and_then(|v| v.as_str()).unwrap_or("");
            if prompt_text.trim().is_empty() {
                return None;
            }

            // Lane C, C1 (SPEC-ENFORCEMENT.md 2.1): flag a durable decision
            // for capture at Stop, independent of whether anything below
            // resolves for injection - a decision can be stated even when no
            // existing item matches this prompt's own keywords. See
            // `capture_flag`'s own doc comment; every failure here is silent.
            // NEVER for a subagent (`payload_is_from_a_subagent`'s own doc
            // comment) - a subagent's own task prompt is not the owner
            // stating anything.
            if !payload_is_from_a_subagent(&payload) {
                capture_flag(&store, db_path, &session_id, prompt_text);
            }

            // The owner typed the evaluation command (`/t-eval` or
            // `/thor-eval`): the evaluation starts here, due or not, and the
            // session is told the three lists its report is held to
            // (`serve::eval_session`). Only the command itself counts - a
            // task that merely says to end with it has not started one - and
            // never in a prompt a subagent receives: no owner typed that.
            let typed = !payload_is_from_a_subagent(&payload)
                && serve::eval_session::prompt_starts_evaluation(prompt_text, &serve::eval_session::command_names(db_path));
            let eval_note = if typed && !no_record {
                session_project.as_deref().and_then(|p| {
                    match serve::eval_session::try_start(db_path, &mut store, &session_id, p, "command") {
                        Ok(started) => Some(match started.fresh {
                            true => serve::eval_session::start_note(&started.start),
                            false => serve::eval_session::continue_note(&started.start, true),
                        }),
                        // One project runs one evaluation at a time (step 24).
                        Err(serve::eval_session::StartRefused::RunningElsewhere { start, idle_minutes }) => {
                            Some(serve::eval_session::running_elsewhere_note(&start, idle_minutes))
                        }
                        Err(serve::eval_session::StartRefused::Store) => None,
                    }
                })
            } else {
                None
            };

            let candidates = serve::live::live_items(&store);
            let mut input = prompt::resolve(prompt_text, &candidates);
            input.project = folder_project.clone();
            if input.is_empty() {
                return eval_note.map(|block| HookOutput::Context { event_name, block, verdicts: Vec::new() });
            }
            let decay = DecayContext::load(&store);
            let all = serve::decay::retain_live(serve::rank::select(&candidates, &input), &decay);
            let selection = render::cap(all);
            let rendered = render::render_text(&selection, &input, db_path);
            let block = match (rendered, eval_note) {
                (Some(rendered), Some(note)) => format!("{note}\n\n{rendered}"),
                (Some(rendered), None) => rendered,
                (None, Some(note)) => return Some(HookOutput::Context { event_name, block: note, verdicts: Vec::new() }),
                (None, None) => return None,
            };
            let ids: Vec<String> = selection.shown.iter().map(|r| r.id.clone()).collect();
            // No trigger: a prompt is neither a command nor a file, the two
            // shapes `ItemServed::trigger` names - plain `record_delivery`
            // already writes `None`, exactly what this surface needs.
            if !no_record {
                deliver::record_delivery(&mut store, &session_id, &session_id, actor, &time::now_iso8601(), &ids);
            }
            // No `verdicts` here either, for the identical reason session
            // start carries none: a raw prompt is neither a proposed command
            // nor proposed content (`prompt::resolve` never calls
            // `add_command`/`add_file` - see `render::why_invocation`'s own
            // doc comment), so there is nothing for the classifier to catch
            // anything IN.
            Some(HookOutput::Context { event_name, block, verdicts: Vec::new() })
        }
        _ => {
            // Surface 2 (PreToolUse, and the safe default for any event name
            // this hook does not otherwise recognise).
            let tool_input = payload.get("tool_input");
            let file_path = tool_input.and_then(|t| t.get("file_path")).and_then(|v| v.as_str());
            let tool_name = payload.get("tool_name").and_then(|v| v.as_str()).unwrap_or("");

            // OPENING THE EVALUATION ROUTINE AS A SKILL (`serve::eval_session`),
            // before any guard can return. It starts an evaluation ONLY while
            // the evaluation is due - the Stop hook would ask for it now -
            // and never a second one: an evaluation already open in this
            // session is named again with its start note. Otherwise nothing
            // starts and the call gets one plain answer: which evaluation is
            // already open elsewhere, or that none is due.
            let eval_note = match session_project.as_deref() {
                Some(project)
                    if serve::eval_session::call_starts_evaluation(tool_name, tool_input, &serve::eval_session::command_names(db_path)) =>
                {
                    Some(skill_call_answer(&mut store, db_path, &session_id, project, hook_work.as_ref(), &payload, no_record))
                }
                _ => None,
            };

            // Lane C, C3 (SPEC-ENFORCEMENT.md 2.3): the wrong-sink check,
            // BEFORE anything else in this arm. Deliberately narrow - see
            // `capture_sink_check` / `capture::sink_verdict`'s own doc
            // comments. NEVER for a subagent (`payload_is_from_a_subagent`'s
            // own doc comment). A `Block` (an ACTUAL judge verdict, still
            // unpaid) stops the tool call outright; a `Warn` (C1's own
            // cheap, provisional rulebook match - possibly all that exists
            // yet, before this turn's Stop has run) never blocks, only rides
            // along as visible context below.
            let sink_verdict = if payload_is_from_a_subagent(&payload) {
                capture::SinkVerdict::Allow
            } else {
                capture_sink_check(&store, db_path, &session_id, file_path)
            };
            if let capture::SinkVerdict::Block(reason) = sink_verdict {
                return Some(HookOutput::Decision(serde_json::json!({
                    "decision": "block",
                    "reason": reason,
                })));
            }
            let sink_warning = match sink_verdict {
                capture::SinkVerdict::Warn(reason) => Some(reason),
                _ => None,
            };

            // THE HARD GUARDS, all decided by `serve::guard_verdict::pre_tool`
            // in hook order: the file write, then the command (its shell writes
            // first, then a forbidden literal in the command itself), then the
            // conditional guard - the only arm that catches an OMISSION. See
            // that module's own doc comment for each arm's doctrine and for why
            // the order stands. Independent of Lane C above: they fire on the
            // write or command about to happen, never on a stated decision, so
            // unlike C3 they are never gated on `payload_is_from_a_subagent` -
            // there is no notion of "the owner's own debt" to gate on, and a
            // subagent's write can violate an anchored, still-current rule
            // exactly as easily as the owner's own.
            //
            // THE ROOT IS THE TOP OF THE CHECKOUT (`project::guard_root`),
            // not the working folder: a session standing in a subfolder
            // resolved every check path under that subfolder, so a required
            // line in the checkout's own NOTES.md was never guarded from
            // there. After a drift into a folder of no project it is the
            // registered checkout of the project the session kept, so the
            // path-anchored guards stay on for that project's own files too
            // (D4). A relative path in the call is still read against the
            // working folder itself (`guard_verdict::pre_tool`'s `cwd`).
            let guard_root = project::guard_root(
                &store,
                session_cwd.as_deref(),
                folder_project.as_deref(),
                session_project.as_deref(),
            );
            if let Some(reason) = pre_tool_guard_block(
                &store,
                db_path,
                &session_id,
                tool_name,
                tool_input,
                session_project.as_deref(),
                guard_root.as_deref(),
                session_cwd.as_deref(),
                actor,
                payload.get("tool_use_id").and_then(|v| v.as_str()),
            ) {
                return Some(HookOutput::Decision(serde_json::json!({
                    "decision": "block",
                    "reason": reason,
                })));
            }

            // THE WRITE MARKER (step 12): a THOR write this hook lets through
            // gets one more line of context, naming the call and the digest of
            // its arguments. Claude Code records it in the transcript as a
            // hook attachment of that call, after the tool finished; the Stop
            // then reads it back (`serve::attest::verify_write`) as proof that
            // the write passed this hook. A block returned above never gets it.
            let write_marker = serve::attest::write_marker(
                tool_name,
                payload.get("tool_use_id").and_then(|v| v.as_str()),
                tool_input,
            );
            let marker_event = event_name.clone();
            let served: Option<HookOutput> = 'served: {
                let mut input = ServeInput {
                    project: folder_project.clone(),
                    root: session_cwd.clone(),
                    ..Default::default()
                };
                if let Some(command) = absent_guard::proposed_command(tool_input) {
                    input.add_command(command);
                } else {
                    // THE TOOL-NAME REACH THIS CLOSES. MEASURED: a Rule bound to
                    // Target command "Agent"/"Artifact"/"SendUserFile" was only
                    // ever matched inside the requires guard
                    // (`guard_verdict::requires`, which already reads the bare tool
                    // name as its own reach) - this surface built `input` from a
                    // Bash-style "command" field alone, so such a rule was never
                    // SERVED when the tool it names was actually called, and
                    // nine pinned rules had to stay on Always only to be seen at
                    // all. A tool call with no "command" field (every non-shell
                    // tool: Agent, Read, Write, Artifact, ...) now offers its own
                    // bare NAME the same way a shell command offers its own text,
                    // so a Command anchor naming a tool reaches that tool's own
                    // call - `add_command` gives it the identical moment/target/
                    // context derivation every other command anchor already
                    // gets, and a forbidden-literal check against a bare tool
                    // name stays harmless: this is the informational surface
                    // (`serve::serve`, rendered as context) - the hard guard
                    // (`guard_verdict::command`) still reads only a real "command"
                    // field via `absent_guard::proposed_command`, completely
                    // unaffected by this branch.
                    input.add_command(tool_name);
                }
                if let Some(file_path) = file_path {
                    input.add_file(file_path);
                }
                let searched = searched_path(tool_name, tool_input);
                if let Some(searched) = searched {
                    // Only the PLACE, never the moments a file touch derives: a
                    // search acts on nothing, so it informs about what is anchored
                    // there and nothing more. `file` makes the `why` hint name it.
                    input.add_target(TargetKind::Path, searched);
                    input.file = Some(searched.to_string());
                }
                // The Remember moment (`intent::Action::Remember`): nothing
                // derives it from a command or file path (see `intent`'s own
                // closed vocabulary doc comment), so this is the one place a
                // call to this memory's own remember/revise tool ever turns into
                // that moment. See `is_remember_moment`'s own doc comment for why
                // only the tool-name SUFFIX is compared, never the whole name.
                if is_remember_moment(tool_name) {
                    input.add_moment(Action::Remember);
                }
                // The evaluation's start note leads whatever else this call says.
                let sink_warning = match (eval_note, sink_warning) {
                    (Some(note), Some(warning)) => Some(format!("{note}\n\n{warning}")),
                    (note, warning) => note.or(warning),
                };
                if input.is_empty() {
                    // Nothing else would render, but a provisional sink warning
                    // may still stand entirely on its own. Nothing was ever
                    // selected, so `verdicts` is empty here too - never a guess
                    // at what would have applied.
                    break 'served sink_warning.map(|block| HookOutput::Context { event_name, block, verdicts: Vec::new() });
                }
                let served = serve::serve(&store, &input);
                let rendered = render::render_text(&served.selection, &input, db_path);
                let block = match (sink_warning, rendered) {
                    (Some(warning), Some(rendered)) => Some(format!("{warning}\n\n{rendered}")),
                    (Some(warning), None) => Some(warning),
                    (None, Some(rendered)) => Some(rendered),
                    (None, None) => None,
                };
                let Some(block) = block else { break 'served None };
                let ids: Vec<String> = served.selection.shown.iter().map(|r| r.id.clone()).collect();
                // THE ONE SURFACE WHERE `verdicts` IS EVER REAL: the moment of
                // action, the only one of the three hook events that proposes an
                // actual command or write. `input.command` is whatever was
                // already resolved above (the real command when there was one,
                // the tool-name fallback otherwise - the SAME text this call's
                // own selection already ran on, never a second derivation), and
                // `proposed_content` is the Write/Edit fragment when this tool
                // call carries one. See `check_rows`/`CheckRow` for the shape and
                // `Context`'s own doc comment for why this can never leak into
                // `block` itself.
                //
                // Built from `served.all`, never `served.selection.shown` alone -
                // see `check_rows`'s own doc comment for the defect that left a
                // real prohibition, outranked at its own anchor by heavier items,
                // invisible to any program reading this exact field. `shown_ids`
                // is the same set `ids` above already
                // names, read back out of `served.selection.shown` a second time
                // rather than reused from `ids` directly, because `check_rows`
                // wants `&str` borrows and `ids` is already-owned `String`s.
                let shown_ids: std::collections::HashSet<&str> =
                    served.selection.shown.iter().map(|r| r.id.as_str()).collect();
                let verdicts = check_rows(&served.all, &shown_ids, input.command.as_deref().unwrap_or(""), absent_guard::proposed_content(tool_name, tool_input));
                // The command as typed wins over the file, never the tool-name
                // fallback `input.add_command` used above when there was no real
                // command (`absent_guard::proposed_command` returns `None` for
                // that case too - it is not "the command line as typed" the way
                // `ItemServed::trigger` means it, only a stand-in target). A
                // Bash-style call never carries both at once in a real payload
                // (see the comment above `input.add_command`/`add_file` a few
                // lines up), so the order only matters for a hand-built input.
                let trigger = absent_guard::proposed_command(tool_input).or(file_path).or(searched);
                if !no_record {
                    deliver::record_delivery_with_trigger(
                        &mut store,
                        &session_id,
                        &session_id,
                        actor,
                        &time::now_iso8601(),
                        &ids,
                        trigger,
                    );
                }
                Some(HookOutput::Context { event_name, block, verdicts })
            };
            let Some(marker) = write_marker else { return served };
            Some(match served {
                Some(HookOutput::Context { event_name, block, verdicts }) => {
                    HookOutput::Context { event_name, block: format!("{block}\n{marker}"), verdicts }
                }
                Some(HookOutput::ContextWithNotice { event_name, block, notice, verdicts }) => {
                    HookOutput::ContextWithNotice { event_name, block: format!("{block}\n{marker}"), notice, verdicts }
                }
                Some(other) => other,
                None => HookOutput::Context { event_name: marker_event, block: marker, verdicts: Vec::new() },
            })
        }
    }
}

// ------------------------------------------------------- Lane C: the capture
// guard (SPEC-ENFORCEMENT.md section 2). The wiring here is deliberately
// thin: every real decision lives in `serve::capture`'s pure functions; these
// three do only the I/O (read the rulebook/marker file, read the store,
// write the marker file back) and translate the result into `hook_once`'s
// own `HookOutput` shape.

/// C1 (SPEC-ENFORCEMENT.md 2.1, redesigned 2026-08-05 - see
/// `JUDGE-TRANSPORT.md` and `capture`'s own doc comment): cheap about
/// BLOCKING, deliberately - no judge call happens here, ever (that would pay
/// its multi-second latency in front of every single prompt, before the
/// model has even started answering; see `judge`'s own doc comment). Records
/// the raw prompt text and the store's current max event seq for THIS
/// session, unconditionally, on every non-empty prompt. It DOES also read
/// the fallback rulebook and pass it through - `flag_marker_text` stores that
/// match as a PROVISIONAL signal only (restored 2026-08-05 so C3 has
/// something to warn on within the same turn); it can never reach the
/// AUTHORITATIVE `tier` field, which stays `None` here regardless and is
/// Stop's alone to fill in, via the judge (`capture_stop_check`). Every
/// failure here - store unreadable, rulebook unreadable, marker file
/// unreadable/unwritable - is silent: this must never slow down or fail a
/// prompt (SPEC-ENFORCEMENT.md 1.1). NEVER called for a subagent - see the
/// `UserPromptSubmit` arm above and `payload_is_from_a_subagent`'s own doc
/// comment.
fn capture_flag(store: &thor_core::event_store::EventStore, db_path: &Path, session_id: &str, prompt_text: &str) {
    // seq_at_flag = the store's current max event seq: `get_next_seq` is
    // "the next seq a write would take", i.e. max + 1, so max is that minus
    // one (0 on a genuinely empty store - `saturating_sub` so this can never
    // wrap negative).
    let Ok(next_seq) = store.get_next_seq() else { return };
    let seq_at_flag = next_seq.saturating_sub(1);
    let marker_path = capture::default_marker_path(db_path);
    let existing = std::fs::read_to_string(&marker_path).ok();
    // The NARROW trigger set is the compiled-in default the moment no real
    // `guard-capture-rulebook.json` exists yet - see
    // `capture::resolve_rulebook_text`'s own doc comment for why this matters
    // most for self-adjudicate mode (its entire trigger signal IS this
    // rulebook firing at all).
    let rulebook_text =
        capture::resolve_rulebook_text(std::fs::read_to_string(capture::default_rulebook_path(db_path)).ok());
    let Some(new_text) =
        capture::flag_marker_text(existing.as_deref(), session_id, prompt_text, seq_at_flag, Some(&rulebook_text))
    else {
        return;
    };
    let _ = serve::store_dir::write(&marker_path, new_text);
}

/// C2 (SPEC-ENFORCEMENT.md 2.2): the one Stop path allowed to open the
/// store, and only read-only - `EventStore::open_existing` never creates a
/// store (unlike `EventStore::new`), which matches that claim literally: by
/// the time a marker exists at all, C1 has already run in this same session
/// and the store already exists. Every failure - no marker, marker file
/// unreadable/malformed, store missing/corrupt/unreadable - falls through to
/// `None` (ALLOW), the same fail-open as the Response Guard right above this
/// call in `hook_once`. This is also SPEC-ENFORCEMENT.md's own safety-model
/// test 9 in wiring form: a debt whose payment cannot be proven (the store
/// will not open) never blocks.
///
/// The classification itself (judge, or its fallback) is deliberately
/// skipped whenever the debt is ALREADY paid - `capture::decide_stop` would
/// discard it anyway (a paid debt always wins over any verdict), so checking
/// `paid` first saves a real judge round trip on every turn where the model
/// already did the right thing.
/// Returns the block's reason; the caller holds the Stop with it (and the
/// evaluation ask rides along, step 21).
fn capture_stop_check(db_path: &Path, session_id: &str) -> Option<String> {
    // `Off` is checked before anything else here is even read - the cheapest
    // possible "never blocks anything" (OPTION4-IMPLEMENTATION.md): no marker
    // file, no store, nothing.
    let mode = load_capture_mode(db_path);
    if mode == capture::CaptureMode::Off {
        return None;
    }

    let marker_path = capture::default_marker_path(db_path);
    let marker_text = std::fs::read_to_string(&marker_path).ok()?;
    let mut markers = capture::read_markers(&marker_text);
    let marker = markers.get(session_id)?.clone();

    let store = serve::store_dir::open_existing(db_path).ok()?;
    let kinds: Vec<_> = store.events_since(marker.seq_at_flag).ok()?.into_iter().map(|e| e.kind).collect();
    let paid = capture::debt_paid(&kinds);

    // Mode decides who judges: SelfAdjudicate never reads the judge config
    // and never spawns anything (`capture::decide_self_adjudicate` is pure);
    // Judge is the pre-existing path, unchanged. `Off` was already handled
    // above, so it can never reach this match.
    let (verdict, next) = match mode {
        capture::CaptureMode::Off => unreachable!("returned above"),
        capture::CaptureMode::SelfAdjudicate => capture::decide_self_adjudicate(&marker, paid),
        capture::CaptureMode::Judge => {
            let classification = if paid { None } else { classify_capture(db_path, &marker.prompt) };
            capture::decide_stop(&marker, paid, classification.as_ref())
        }
    };

    // Persist whatever the decision above decided: clear the marker (debt
    // paid, warn tier, no verdict, or already blocked once) or keep it with
    // blocked_once now true. Best-effort - a failed write here must never
    // stop the verdict already decided from being returned.
    match &next {
        Some(updated) => {
            markers.insert(session_id.to_string(), updated.clone());
        }
        None => {
            markers.remove(session_id);
        }
    }
    if let Ok(text) = serde_json::to_string(&markers) {
        let _ = serve::store_dir::write(&marker_path, text);
    }

    match verdict {
        // Nothing else had anything to say, so this is where the one debt
        // that never gets paid voluntarily is collected.
        capture::StopVerdict::Allow => None,
        capture::StopVerdict::Block(reason) => Some(reason),
    }
}

/// THE FOURTH DEBT: setup, held while `install`'s own seeded note
/// (`model::store::SETUP_NOTE_ID`, `ops::install::working_contract`'s
/// `walk-through-the-answer-guard-once`) is still live.
///
/// THE DEFECT THIS CLOSES. That note asks, in its own words, for a first
/// session to raise AGENTS.md's setup questions before other work and then
/// retract it - and until now that request WAS the entire enforcement:
/// prose in a Rule's body, read or skipped exactly like any other sentence a
/// busy session is handed. This project has measured that shape fail
/// before: the crowding advisory just below sat unread for months as a note
/// beside a write, until it grew into the debt that holds the turn instead
/// of a sentence that hopes. This is the identical fix for the identical
/// failure - see `model::gate::retract`'s own doc comment for the
/// retract-time half of it, which refuses letting the note go without
/// answers on record.
///
/// Two conditions clear it, both named in the message below, and they
/// reduce to checking one thing: `model::gate::retract`'s own ground
/// refuses retracting the note without a live `model::store::
/// OWNER_SETUP_ANSWERS_ID` item first, so the note cannot go retracted
/// while unanswered - the note being LIVE is sufficient on its own to know
/// both halves are still owed, and the note being GONE is sufficient to
/// know both are settled.
///
/// FIRST among the debts, ahead of Lane C and the three below it: none of
/// them mean anything to an owner who has not even been told what this
/// memory is yet, and this is the one turn-holding message a brand new
/// install can be certain has nothing else competing with it that turn.
///
/// NEVER for a subagent - inherited for free from the
/// `payload_is_from_a_subagent` check in `hook_once` that already stands
/// between this function and its only call site, the same way the
/// stale-rule guard's own exemption is (see `stale_guard_stop_check`'s doc
/// comment): a subagent's Stop has no owner in the room to walk through
/// anything.
///
/// Fails open like every other guard here: no store, no note, any read
/// error at all reads as nothing to say, never a stuck session.
fn setup_debt(store: &EventStore) -> Option<String> {
    model::store::show(store, model::store::SETUP_NOTE_ID).ok()?;
    Some(format!(
        "[THOR] This memory was just installed and its owner has not been walked through setup \
         yet. Before anything else: raise the questions in AGENTS.md's \"First session with a \
         new owner\" section with him, in plain language, and apply his answers where they \
         belong. Then store what he chose as '{}' (a Report with no scope is enough - this id \
         needs none, so do not ask him to name a collection for it) and retract '{}'. \"He does \
         not want any of this\" is itself a valid answer - write that down and this still \
         clears. Two things settle it: his answers on record, and the note retracted.",
        model::store::OWNER_SETUP_ANSWERS_ID,
        model::store::SETUP_NOTE_ID
    ))
}

/// Where this session's watermark lives: the store's tip as it stood when the
/// session began, keyed by session id, in a sidecar beside the store - the
/// same shape and the same place as every other per-session marker this
/// binary keeps.
fn watermark_path(db: &Path) -> std::path::PathBuf {
    db.parent().unwrap_or_else(|| Path::new(".")).join("session-watermark.json")
}

/// Record where the log stood when this session started, once. Called from the
/// SessionStart arm; best effort, like every sidecar here - a watermark that
/// cannot be written means the crowding debt stays quiet, never that a turn
/// breaks.
fn record_session_watermark(db_path: &Path, session_id: &str) {
    let Ok(store) = serve::store_dir::open_existing(db_path) else { return };
    let Ok((tip, _)) = store.contiguous_tip() else { return };
    let path = watermark_path(db_path);
    let mut marks: std::collections::BTreeMap<String, i64> = std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    // Never move a watermark that already exists: a session start that fires
    // twice (a resume, a compact) must not forgive what was written between.
    marks.entry(session_id.to_string()).or_insert(tip);
    if let Ok(text) = serde_json::to_string(&marks) {
        let _ = serve::store_dir::write(&path, text);
    }
}

/// Where the log stood when this session started, or `None` when no watermark
/// was ever recorded for it. `None` means silence, deliberately: a session
/// whose start was never seen (an older install, a hook that failed to write)
/// must not be handed every crowded fact in the store as though it made them.
fn session_watermark(db_path: &Path, session_id: &str) -> Option<i64> {
    let text = std::fs::read_to_string(watermark_path(db_path)).ok()?;
    let marks: std::collections::BTreeMap<String, i64> = serde_json::from_str(&text).ok()?;
    marks.get(session_id).copied()
}

/// The crowded facts THIS session wrote and has not dealt with.
///
/// WHY THIS IS FORCED AND THE EVALUATION IS NOT. The write response already
/// says "this may well never be shown there" the moment somebody stores a fact
/// onto a full pool. Saying it was not enough: measured across two real
/// sessions, the note was reported and then left alone, and the fact stayed
/// invisible. A maintenance step that depends on somebody remembering is the
/// same shape as the useful-mark was before it was fixed - it works exactly as
/// often as people feel like it.
///
/// So this holds the turn, like the judgement debt does, and for the same
/// reason: it is the only thing that ever produced maintenance. It is asked
/// BEFORE that older debt because this answer decays - the person who wrote
/// the fact still knows what it was for, and tomorrow nobody does.
///
/// WHAT "THIS SESSION" MEANS HERE, AND WHY IT IS NOT THE SESSION ID. A first
/// version filtered events on `session_id` and was UNREACHABLE in real use:
/// every write through the tool server is stamped with the constant "mcp"
/// (`mcp::SESSION_ID`), never the caller's own session, so the Stop hook -
/// which does know the real one - matched nothing and the debt never fired.
/// Its unit tests passed because they handed the same id to both sides. Found
/// by a real session running the test end to end and reporting that step 2
/// simply did not block. (Since step 12 the tool server stamps a write with
/// its `CLAUDE_CODE_SESSION_ID` when it has one - a hint fixed when the server
/// started, still "mcp" without one - so the watermark stays the boundary.)
///
/// So the boundary is a WATERMARK: the store's tip as it stood when this
/// session started (`session_watermark`). Anything written above it was
/// written during this session, whoever stamped it.
///
/// Beyond that watermark it holds no state. `capacity` is a pure function of
/// the store as it stands now, so the debt clears by construction the moment
/// the fact is folded away, re-anchored somewhere with room, or retracted.
///
/// It only ever asks about what THIS session made. A session that stores
/// nothing never sees it, and it never becomes a backlog nag about older mess.
/// THE THING THE OWNER ASKED FOR THREE DAYS RUNNING, and did not have until
/// 2026-08-09: the system finding, by itself, which facts deserve to reach the
/// gate.
///
/// The write gate asks its question of every heavy rule at the door. That
/// covers what is written from now on, and it covered the backlog exactly once
/// - in a sweep somebody had to decide to run. Everything else in the store
/// stayed as it was, and nothing was ever going to look at it again. "Ask when
/// somebody happens to touch it" is not a mechanism; it is a hope.
///
/// So this walks the whole store, every turn, and holds the turn on ONE rule
/// that names something concrete in its own text and has never been asked. It
/// does not decide the answer - that needs judgement, and the answer "there is
/// nothing to catch here" is a real answer. It decides WHO GETS ASKED, which
/// was the part that depended on somebody remembering.
///
/// Deliberately last among the debts and deliberately one at a time: this is a
/// backlog with hundreds in it, and a burn that never stops beats a sweep that
/// happens once.
/// Where the backlog burn remembers whose turn it already took.
fn teeth_asked_path(db: &Path) -> std::path::PathBuf {
    db.parent().unwrap_or_else(|| Path::new(".")).join("teeth-asked.json")
}

/// ONCE per session, never once per turn.
///
/// THE DEFECT THIS PREVENTS, caught the same evening it was built: with 348
/// unanswered rules in the store, a per-turn debt is not a slow burn, it is a
/// wall - every single turn ends held, forever, and the only way to work is to
/// stop believing the debts. A maintenance nudge that makes the tool unusable
/// gets switched off, and then it protects nothing at all.
fn teeth_not_yet_asked_this_session(db: &Path, session_id: &str) -> bool {
    let Ok(text) = std::fs::read_to_string(teeth_asked_path(db)) else { return true };
    !text.lines().any(|line| line.trim() == session_id)
}

/// Best effort, like every sidecar here: a sidecar that cannot be written
/// means the burn asks again next turn, never that a turn breaks.
fn record_teeth_asked(db: &Path, session_id: &str) {
    let path = teeth_asked_path(db);
    let mut text = std::fs::read_to_string(&path).unwrap_or_default();
    text.push_str(session_id);
    text.push('\n');
    // Keep the tail only: this file is a set of recent sessions, not history.
    let kept: Vec<&str> = text.lines().rev().take(200).collect();
    let trimmed: String = kept.into_iter().rev().collect::<Vec<_>>().join("\n");
    let _ = serve::store_dir::write(&path, format!("{trimmed}\n"));
}

/// How many unanswered rules one turn is asked about at once.
///
/// WHY A BATCH, since 2026-08-14. One per session is a burn that never
/// finishes: measured that day at 336 waiting, which is one a day for the
/// better part of a year, while the answer to each is usually one sentence.
/// The judgement debt had exactly this shape and exactly this fix. Five, not
/// all of them: the wall this is deliberately not is a debt that hands over
/// 336 rules and ends every turn until they are gone.
const TEETH_DEBT_BATCH_MAX: usize = 5;

/// SCOPED TO THE CHECKOUT'S OWN PROJECT, the same rule `judgement_debt` and
/// `crowding_debt` already apply: `current_project` is the project THIS
/// checkout resolves to, `None` for "no project" (global-only); an item with
/// no project of its own is global and reaches every checkout regardless
/// (`project::applies_to`, this file's one shared rule for it).
///
/// THE DEFECT THIS CLOSES. Before this parameter existed, this function took
/// only `store` and walked EVERY live candidate in the whole store with
/// nothing anywhere in its filter chain naming a project - confirmed by
/// reading its own signature - and was measured firing exactly that way,
/// unscoped, during a real proof run. That is the identical unanswerable
/// question `judgement_debt` was fixed for on 2026-09-07/08 and
/// `crowding_debt` on 2026-08-14: asking THIS session to arm or dismiss a
/// rule that belongs to a checkout it is not even sitting in.
///
/// TWO THINGS THIS DELIBERATELY DOES NOT ALSO COPY FROM `judgement_debt`'s
/// OWN 2026-09-08 FIX, AND WHY.
///
/// NOT "only what this session actually saw fire" (`served_ids_in_session`).
/// That filter is right for a debt asking "you just saw this happen, judge
/// it" - it is wrong for this one, whose entire reason to exist is the
/// opposite: see `teeth_asked_path`'s own doc comment above ("this walks the
/// whole store, every turn... a backlog with hundreds in it, and a burn that
/// never stops beats a sweep that happens once"). A rule nobody has asked
/// about in months, that no session happens to re-trigger, is exactly what
/// this debt was built to surface BECAUSE no session ever sees it fire on
/// its own; scoping it to "seen this session" would silence it for
/// precisely the backlog it exists to burn down.
///
/// NOT a per-item floor in place of the existing per-session one
/// (`teeth_not_yet_asked_this_session` / `record_teeth_asked`, "ONCE per
/// session, never once per turn"). That gate is already STRICTER than a
/// per-item floor: once this debt has asked at all this session, it asks
/// nothing more this session, about any item, answered or not. Loosening it
/// to per-item would let a fresh batch of newly-eligible ids reopen the
/// block later the same session - reintroducing the exact "every single turn
/// ends held, forever" wall its own 2026-08-14 fix closed.
///
/// SETTLED BY THE ONE READER (step 9). A rule leaves this debt exactly when
/// `model::store::settlement` calls it settled - the same answer the
/// evaluation-filing gate and doctor read: a current verdict from THOR's
/// trial (rule track or note), or a machine blocker while it holds (a file
/// check of a project with no usable registered working copy - never named
/// here, because nothing the agent runs can settle it, F9). A tag, a reason,
/// an open point, a refusal on record or a probe settles nothing. Every
/// live rule and orientation counts, whatever its weight (D8): lowering a
/// severity changes the fingerprint, so it is owed again.
///
/// The demand names the one command that settles it: `serve retest --id
/// <id>` (with `--db`), then the same with `--apply`.
///
/// ROTATES, so a fix-nothing session does not see the same five forever
/// (2026-09-26): measured on a copy of a real store, this named the same five
/// lowest ids at every session start in a project with nearly a hundred
/// waiting, because it sorted by id alone. Now the ones asked about least
/// often come first (`teeth_items_asked_counts`, a sidecar beside the store),
/// then the heaviest, then the ones that fired most since their last verdict.
fn teeth_debt(store: &EventStore, db_path: &Path, current_project: Option<&str>) -> Option<String> {
    let fired = usefulness::served_since_last_verdict(store);
    let asked = teeth_items_asked_counts(db_path);
    // (asked count, severity rank, fired count, id, why it is owed)
    let mut candidates: Vec<(usize, u8, usize, String, String)> = Vec::new();
    for li in serve::live::live_items(store) {
        if !li.item.kind.can_fire() || !project::applies_to(li.item.project.as_deref(), current_project) {
            continue;
        }
        let model::store::Settlement::Owed { why } = model::store::settlement(store, &li.id, None) else { continue };
        let count = fired.get(&li.id).copied().unwrap_or(0);
        candidates.push((
            asked.get(&li.id).copied().unwrap_or(0),
            model::item::severity_rank(li.item.severity),
            count,
            li.id,
            why,
        ));
    }
    if candidates.is_empty() {
        return None;
    }
    candidates.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)).then(b.2.cmp(&a.2)).then(a.3.cmp(&b.3)));
    let batch_len = candidates.len().min(TEETH_DEBT_BATCH_MAX);
    let left = candidates.len() - batch_len;
    let mut items_block = String::new();
    for (_, _, _, id, why) in candidates.iter().take(TEETH_DEBT_BATCH_MAX) {
        items_block.push_str(&format!("\n- '{id}': {why}. Run: {}", model::store::retest_command_for(Some(db_path), id)));
    }
    record_teeth_items_asked(db_path, candidates.iter().take(TEETH_DEBT_BATCH_MAX).map(|c| c.3.as_str()));
    let left_note = if left > 0 {
        format!(" {left} more are waiting behind this batch; the next session is asked about others first.")
    } else {
        String::new()
    };
    Some(format!(
        "[THOR] {batch_len} rule(s) have no current verdict from THOR's trial - listed below, each with why and the \
         command that decides it. For each one: run the command shown. It prints the registered working copy it \
         used, every form it tried, and its verdict - a rule, a note, or the one fix you have to make yourself. When \
         it says rule track or note, run the same command again with --apply to write that verdict. When it names a \
         fix, make it (for example revise the text to name the literal, or hang the tooth it names), then run it \
         again. mcp__thor__revise with only the rule's id runs the same trial too. Retract a rule, saying what \
         changed about it, if it is simply wrong. A tag, a reason, an open point or a refusal on record settles \
         nothing.{left_note}{items_block}"
    ))
}

/// Where `teeth_debt` keeps how often it has named each rule - beside the
/// store, one id per line, newest last. Its own file, never `teeth-asked.json`
/// (that one is the once-per-session gate and holds session ids).
fn teeth_items_asked_path(db: &Path) -> std::path::PathBuf {
    db.parent().unwrap_or_else(|| Path::new(".")).join("teeth-asked-items.txt")
}

/// How often each rule has been named by `teeth_debt`, from its sidecar. An
/// unreadable sidecar reads as "never asked", which only changes the order.
fn teeth_items_asked_counts(db: &Path) -> std::collections::HashMap<String, usize> {
    let mut counts = std::collections::HashMap::new();
    if let Ok(text) = std::fs::read_to_string(teeth_items_asked_path(db)) {
        for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
            *counts.entry(line.to_string()).or_insert(0) += 1;
        }
    }
    counts
}

/// Best effort, like every sidecar here. Keeps the newest 2000 lines: enough
/// history to rotate through a backlog of hundreds, never an unbounded file.
fn record_teeth_items_asked<'a>(db: &Path, ids: impl Iterator<Item = &'a str>) {
    let path = teeth_items_asked_path(db);
    let mut text = std::fs::read_to_string(&path).unwrap_or_default();
    for id in ids {
        text.push_str(id);
        text.push('\n');
    }
    let lines: Vec<&str> = text.lines().collect();
    let kept = &lines[lines.len().saturating_sub(2000)..];
    let _ = serve::store_dir::write(&path, format!("{}\n", kept.join("\n")));
}

/// Does this item ACTUALLY reach a block at one of its own bindings, as the
/// real ranker decides it?
///
/// THE DEFECT THIS CLOSES. `model::store::capacity` counts rivals of the same
/// weight or heavier and calls the pool full at `MAX_ITEMS`. That is right for
/// a WRITE-time note: it is a warning, deliberately pessimistic, and the gate
/// itself refuses only when every rival is strictly HEAVIER - see `capacity`'s
/// own doc comment for why equal weight is a note and not a refusal. It is the
/// wrong test for a debt that holds the turn. Equal-weight rivals outrank
/// nothing; closeness and the promotion prior settle those ties at serve time,
/// so an item can sit third of four in the real block while the estimate still
/// calls its pool full.
///
/// Measured 2026-08-13: a fact was folded out of a 24-claimant pool into the
/// shown four, `why` put it third of four, and the debt went on demanding an
/// answer about it every turn. That is the one shape of message this system is
/// least allowed to have - a report that no longer matches what is true. Worse,
/// the only way offered to silence it was the `crowded-on-purpose` tag, so
/// fixing the problem properly led straight to recording a decision that is
/// false.
///
/// So the estimate stays the TRIGGER and the real ranker becomes the JUDGE:
/// only an item that reaches no block at ANY of its bindings still owes an
/// answer.
fn reaches_a_block(store: &EventStore, id: &str, item: &model::item::Item) -> bool {
    use model::item::Binding;
    for binding in &item.bindings {
        // Pinned: `session_start` serves it whole at every session start, so it
        // never competes for a place and can never be crowded out of one.
        if matches!(binding, Binding::Always) {
            return true;
        }
        let mut input = ServeInput { project: item.project.clone(), ..Default::default() };
        match binding {
            Binding::Moment(action) => input.moments.push(action.clone()),
            // A DIRECTORY binding reaches no automatic surface: a file touch
            // offers the path, never its parent, so `rank::select` drops it
            // before comparing anything (see `model::store::capacity`). Feeding
            // it back as a Dir target here would match it against itself and
            // report an item as reachable that a real session can never see.
            Binding::Target { kind: model::item::TargetKind::Dir, .. } => continue,
            Binding::Target { kind, value } => {
                input.targets.push((*kind, value.clone()));
                // The real surface carries the command or path as context and
                // `rank::closeness` reads it. The binding's own value is the
                // closest honest stand-in for the moment this item was written
                // for; leaving it empty would score every candidate alike and
                // measure a ranking nobody ever sees.
                input.context = value.clone();
            }
            Binding::Always => continue,
        }
        if serve::serve(store, &input).selection.shown.iter().any(|r| r.id == id) {
            return true;
        }
    }
    false
}

/// Whether a write above the watermark can be this session's own.
///
/// The watermark alone cannot tell, so two things are read off the event
/// itself. (1) A write by one of THOR's own actors (`RESERVED_ACTORS`: the
/// retest pass, repair, migration, the machine's notes) is never the
/// session's work, even when it ran from inside the session: measured
/// 2026-10-01, the install ran `serve retest --all --apply` from a session and
/// its 1586 revisions (session id "retest") were then named one per turn as
/// "stored this session". (2) A write stamped with a DIFFERENT real session id
/// is another session's. The two shared constants a write carries when it has
/// no session of its own ("mcp" from the tool server without a session hint,
/// "cli" from the command line) still read as ours, exactly as before: the
/// watermark and the project filter below stay the boundary for those.
fn written_by_a_session_write(event: &thor_core::event_store::Event, session_id: &str) -> bool {
    if model::store::RESERVED_ACTORS.contains(&event.actor.as_str()) {
        return false;
    }
    matches!(event.session_id.as_str(), "mcp" | "cli") || event.session_id == session_id
}

fn crowding_debt(
    store: &EventStore,
    db_path: &Path,
    session_id: &str,
    current_project: Option<&str>,
) -> Option<String> {
    use thor_core::event_store::EventKind;
    let since = session_watermark(db_path, session_id)?;
    let events = store.get_all_events().ok()?;

    // Written during this session: a declare or a revise above the watermark.
    // Retractions are read too, because retracting IS one of the three ways out.
    let mut written: Vec<String> = Vec::new();
    let mut settled: std::collections::HashSet<String> = Default::default();
    for e in &events {
        if e.seq <= since {
            continue;
        }
        match e.kind {
            EventKind::FactCreated | EventKind::FactRevised => {
                if !written_by_a_session_write(e, session_id) {
                    continue;
                }
                if !written.contains(&e.entity_id) {
                    written.push(e.entity_id.clone());
                }
            }
            EventKind::FactRetracted => {
                settled.insert(e.entity_id.clone());
            }
            _ => {}
        }
    }

    for id in written {
        if settled.contains(&id) {
            continue;
        }
        let Ok(item) = model::store::show(store, &id) else { continue };
        // A fact scoped to a project this session is NOT working in was almost
        // certainly not written by this session. It cannot be told apart by the
        // event's own id - every tool-server write is stamped the constant
        // "mcp" (see this function's watermark note), so a second session
        // running concurrently against the SAME store lands its writes above
        // this session's watermark and they read as ours. The item's own scope
        // is the signal the id is not: a session in repo X gets nagged about a
        // crowded fact in repo Y that another session actually wrote. So the
        // nag is routed by project - this session hears only about facts in its
        // own project, or global ones (no project), which belong to everyone.
        // Measured 2026-08-14: a business-repo fact nagged a THOR-dev session
        // twice. When this session's own project cannot be resolved, nothing is
        // filtered - the watermark stands alone, exactly as before.
        if let (Some(cur), Some(p)) = (current_project, item.project.as_deref()) {
            if cur != p {
                continue;
            }
        }
        // Exit 3 taken: the crowd was judged deserved. See the tag's own doc
        // comment for the day this exit existed in the message but nowhere in
        // the code, and the item kept coming back every turn.
        if item.tags.iter().any(|t| t == model::store::CROWDED_ON_PURPOSE_TAG) {
            continue;
        }
        // `full_reliable_anchor` (inside `capacity`) never refuses a binding
        // an item already stands on - see its own doc comment - and by the
        // time this reads `item` back with `show`, it already IS the live
        // occupant of every binding it carries. So `capacity` here always
        // falls through to a note rather than a refusal, exactly as before a
        // Path/Dir/Command anchor could ever be refused outright: a fresh
        // declare/revise cannot land a NEW one on a full anchor any more
        // (refused at the door instead), but a legacy or bypassed write that
        // already holds one is read correctly all the same.
        //
        // ONLY `Crowded` IS DEBT - `Capacity::Broad` (the all-directory case,
        // since 2026-09-17) never matches this pattern and falls straight to
        // `continue` below, exactly like `Capacity::Fine` always did. See
        // `Capacity::Broad`'s own doc comment for the measured case this
        // separated: a directory-only item's note is about BREADTH, not
        // about a rival pool being full, and reading it as "stored onto a
        // place that is already full" (this function's own message below)
        // was simply wrong for that shape - the place was never full.
        let Ok(model::store::Capacity::Crowded(note)) = model::store::capacity(store, &item) else {
            continue;
        };
        // The estimate above is the trigger, never the verdict: ask the real
        // ranker whether this item is actually kept out of every block it could
        // reach. See `reaches_a_block` for the day a fact that had just been
        // folded INTO the shown four kept being asked about anyway.
        if reaches_a_block(store, &id, &item) {
            continue;
        }
        return Some(format!(
            "[THOR] '{id}' was stored this session onto a place that is already full, so it will probably never be read there. The memory said so when you wrote it. Deal with it before ending the turn, in this order: (1) FOLD it - if an existing item almost says the same thing, revise that one to carry your point and retract yours; (2) RE-ANCHOR it - if it is really about a narrower file or command than the one it hangs on, move it there with revise; (3) LEAVE IT - and this exit is narrower than it sounds, so read the condition before taking it. It is honest ONLY when the seats are held by rules that can each REFUSE a write: archiving one of those is refused by the gate anyway, and folding it would spend a check. If the seats are held by DESCRIPTIONS instead, this is the wrong answer - archive a description to free its seat, since it stays fully findable and the rule that can actually block takes its place. When the condition really holds: say in your reply which items hold it, and add '{}' to its tags with revise - BARE, exactly as written here: this tag is matched whole, so appending a reason to it is silently ignored and this will keep asking every turn (that is the opposite of 'no-literal:<why>', which requires one). mark is a verdict on where an item fired and cannot tag anything. Recorded that way it is a decision rather than an oversight. Saying it only in prose settles nothing and this will ask again next turn. What you never do is raise its severity to make it visible: that pushes a heavier warning out to show a lighter one. The note said: {note} The item says: {}",
 model::store::CROWDED_ON_PURPOSE_TAG, item.text
        ));
    }
    None
}

// The threshold itself, and the fold that counts firings-since-last-verdict,
// moved to `serve::usefulness` (2026-09-08, value and behaviour unchanged) so
// `ops::health`'s own doctor line can be built on the exact same numbers this
// hook acts on instead of a second copy silently drifting from them - see
// `usefulness::JUDGEMENT_DEBT_AFTER` and `usefulness::served_since_last_verdict`
// for the full "SINCE its last verdict" reasoning. Imported under its old bare
// name so every reference below (this file's own tests included) is unchanged.
use usefulness::JUDGEMENT_DEBT_AFTER;

/// How many owed items one blocked Stop asks about at once. Unbounded would
/// just move the problem this exists to fix (see `judgement_debt`'s own doc
/// comment) into a single enormous block instead of many small ones; this
/// keeps each block itself bounded while still settling the common case (a
/// handful owed) in one round instead of one Stop per id.
const JUDGEMENT_DEBT_BATCH_MAX: usize = 20;

/// Ask for a verdict on ONE item that has fired over and over and has never
/// once been judged, or nothing.
///
/// WHY THIS ONE IS ALLOWED TO BLOCK WHEN THE CAPTURE GUARD IS NOT. The
/// capture guard has to decide whether a piece of prose stated a durable
/// decision - a judgement, measured on a blind hold-out at 55% catch and
/// 11.4% FALSE blocks, and recorded as not deployable. This decides nothing:
/// the log knows exactly how often an item was served and whether a verdict
/// ever came back. A false block is not merely unlikely here, it is not
/// expressible.
///
/// It terminates by construction. Claude Code sets `stop_hook_active` on the
/// Stop that follows a block, and this whole arm returns early on that, so
/// the ask happens at most once per turn and cannot loop. A verdict buys
/// quiet, not immunity: the count resets, and another `JUDGEMENT_DEBT_AFTER`
/// firings earn another question. This comment used to say a judged item was
/// never asked about again, by anyone, forever - which was true of the
/// lifetime judged-set it once held, and stopped being true the same day the
/// fold below started counting since the last verdict.
///
/// `mark` is the only thing in this system that ever retires noise - a
/// serving count decides nothing, and silence decides nothing either (see
/// `serve::decay`). Without a verdict the loop simply never runs, which is
/// where this store stood for its whole life: four judgements, all on one
/// day, out of 325 items that had fired.
///
/// A PINNED ITEM OWES NO VERDICT - reversed back, 2026-09-12, from the
/// inclusion defended below the line, which held from 2026-09-08 until
/// today. That inclusion was a defensible fix for a real defect (kept below,
/// in full), but it created a sharper one: `serve::decay::DecayContext::
/// is_stale` already ignores every verdict on an `Always`-bound item
/// outright ("an Always-bound item is NEVER stale... it is what Always
/// means"), so a verdict recorded here changes NOTHING anywhere else in the
/// system - asking for one is pure waste, not caution. Measured the day this
/// reversed: the named judgement-debt list carried more than thirty
/// `Always`-bound items, two of them pinned by the owner on purpose, and a
/// whole evaluation was spent judging every one of them for nothing.
///
/// THE DEFECT THE INCLUSION BELOW WAS REALLY FIXING, kept because it is still
/// true and still matters. Before 2026-09-08 this excluded every
/// `Always`-bound item outright, and the blanket exclusion was measured
/// never to let go again: `doctor` (`ops::health::pinned_line`) went on to
/// report "48 pinned facts (14% of all servings) never re-read" and "44 of
/// 48 never reviewed" - every one of them permanently outside the one
/// mechanism (`mark`) that ever puts a verdict on a fact at all. That
/// measurement still stands, and `pinned_line` still reports it. What was
/// wrong was the fix, not the diagnosis: nothing downstream of a verdict
/// recorded here was ever going to read it, because `is_stale` exempts
/// `Always` unconditionally, so manufacturing a question here answered
/// nothing real. The actual fix for a stale or wrong pin is the owner
/// reading `pinned_line`'s own report and revising or unpinning what it
/// names - not this debt asking a question whose answer changes nothing.
///
/// So a pin is excluded again below, through the exact same shared test
/// (`usefulness::is_pinned`) that `usefulness::owed_items` - the fold
/// `judgement_debt_counts`/`judgement_debt_named` build doctor's own lines
/// on - now filters on too: one predicate, both places, so the Stop-time ask
/// and doctor's judgement-debt line can never again silently disagree about
/// what "owed" means the way they did for these four days.
///
/// THE INCLUSION THIS REVERSES, kept in full rather than deleted, since it
/// answers a question a future change here will otherwise ask again: why
/// not simply restore the pre-2026-09-08 blanket exclusion unchanged? A pin
/// still records only a PLACEMENT decision ("this fires at every session
/// start"), never a CONTENT one ("this is still right") - nineteen of the
/// first forty-two owed items were pinned ones, found by trying to pay one,
/// and asking "did it belong where it fired" about something the owner
/// already placed by hand looked unanswerable: "useful" a lie when the rule
/// simply did not come up, "noise" overruling his own decision. That
/// reasoning is exactly why an item pinned but never yet judged did, for
/// those four days, get offered exactly one verdict rather than none at
/// all - and it is why this doc comment keeps that history rather than
/// erasing it: the two defects (a pin nobody ever revisits; a verdict here
/// that changes nothing) are both real, and the fix for the second must not
/// quietly resurrect the first. `pinned_line`'s own report is that fix -
/// visibility without a manufactured, inert verdict.
///
/// ASKS ABOUT THE WHOLE OWED SET IN ONE BLOCK, up to
/// `JUDGEMENT_DEBT_BATCH_MAX`, not one item per Stop. Unpinning a batch of
/// long-pinned items (see `serve unpin`) makes all of them owed at once -
/// each carried a large lifetime serving count from its time as `Always`,
/// which crosses `JUDGEMENT_DEBT_AFTER` the moment the pin comes off - and a
/// long conversation pays the FULL conversation history again on every Stop
/// it takes to answer, one id per turn. Measured 2026-08-13: five separate
/// one-item Stops to judge a backlog that batching would have settled in
/// one. The termination property is unchanged: still one block per Stop
/// (`stop_hook_active` still short-circuits the retry), still one verdict
/// call per id, just several ids asked about in that single block instead of
/// a queue paid out one Stop at a time.
fn judgement_debt(store: &EventStore, db_path: &Path, session_id: &str, current_project: Option<&str>) -> Option<String> {
    // Servings SINCE the last verdict, not servings ever: a judgement resets
    // its item's count to zero rather than removing it from the question
    // forever. Shared with `ops::health`'s own doctor line (2026-09-08) as
    // `usefulness::served_since_last_verdict` - see that function's own doc
    // comment; it is the same fold `usefulness::noise_since_last_useful`
    // performs on the other side of the same doctrine.
    let served = usefulness::served_since_last_verdict(store);
    // Deterministic: the most-served unjudged item, ties broken by id, so a
    // session that ignores the ask is asked the same thing next time rather
    // than being walked through a random tour of the backlog.
    let mut owed: Vec<(String, usize)> = served
        .into_iter()
        .filter(|(_, n)| *n >= JUDGEMENT_DEBT_AFTER)
        .collect();
    if owed.is_empty() {
        return None;
    }
    // Only now, and only when something is actually owed, is the whole live
    // set even fetched - the empty case stays a cheap count over event
    // kinds, which is what every quiet turn pays.
    let live = serve::live::live_items(store);
    // And only what is still LIVE. The served/judged counts are folded from
    // event kinds, which keep every serving an item ever had, including the
    // ones it had before somebody retracted it. So a retracted item stayed
    // owed forever and could never be settled: the question is "did it belong
    // where it fired", and there is no honest answer for something that is
    // gone. Found the moment it bit, on 2026-08-08: merging 57 duplicates
    // turned all 57 into permanent unanswerable asks, and the very next Stop
    // asked about one of them by name.
    //
    // The same pass also builds the id -> project lookup the two filters
    // below share - one fetch of `live`, not two.
    let project_of: std::collections::HashMap<&str, Option<&str>> =
        live.iter().map(|li| (li.id.as_str(), li.item.project.as_deref())).collect();
    owed.retain(|(id, _)| project_of.contains_key(id.as_str()));
    if owed.is_empty() {
        return None;
    }
    // PINNED ITEMS EXCLUDED - see this function's own doc comment above,
    // "A PINNED ITEM OWES NO VERDICT", for why. The same shared test
    // `usefulness::owed_items` (the doctor-side fold) now filters on too, so
    // the two can never again silently disagree about what "owed" means.
    let pinned: std::collections::HashSet<&str> =
        live.iter().filter(|li| usefulness::is_pinned(&li.item.bindings)).map(|li| li.id.as_str()).collect();
    owed.retain(|(id, _)| !pinned.contains(id.as_str()));
    if owed.is_empty() {
        return None;
    }
    // And only what THIS session was actually served. A lifetime count says
    // an item fired a hundred times; it says nothing about whether the
    // reader being asked was ever in the room. A rule scoped to another
    // project fires only there, so asking here produces the same
    // unanswerable question the pinned ones did - found on 2026-08-07 by
    // being asked about an acme-shop commit rule during work in thor-2.
    let seen: std::collections::HashSet<String> =
        store.served_ids_in_session(session_id).unwrap_or_default().into_iter().collect();
    owed.retain(|(id, _)| seen.contains(id));
    if owed.is_empty() {
        return None;
    }
    // THE DEFECT THIS CLOSES, measured 2026-09-07/08: `seen` above proves
    // this literal session_id served the item at SOME point in its own
    // life, never that it did so while working on what this turn is
    // actually about. A session_id is not scoped to one project - the same
    // one can carry a long history across many checkouts (this workspace's
    // own orchestrator pattern, CLAUDE.md's "Orchestrator mode", is exactly
    // this shape on purpose) - so a session that spent part of its life in
    // another project's directory can still show up in that project's own
    // `served_ids_in_session`, and `seen` alone would let one of its items
    // back in here once this session moved on to a different checkout. That
    // is a session honestly being unable to judge something it saw fire
    // somewhere else entirely - a business-repo fact nagging a THOR-dev
    // turn, the exact shape `crowding_debt` (below) was already fixed for on
    // 2026-08-14 ("a business-repo fact nagged a THOR-dev session twice").
    // `judgement_debt` never got that companion filter until now.
    // `project::applies_to` draws the line the same way every real injection
    // surface already does (`rank::select`, `session_start::select`): an
    // item with no project is global and always in scope; one with a
    // project is in scope only for that project's own checkout.
    owed.retain(|(id, _)| project::applies_to(project_of[id.as_str()], current_project));
    if owed.is_empty() {
        return None;
    }
    // THE DEFECT THIS CLOSES, reported by the owner and recorded as
    // `oordeelschuld-komt-binnen-de-sessie-terug` (2026-09-08): measured in a
    // real session, the Stop hook asked for a verdict on the same items
    // three separate times, each only minutes after they had just been
    // judged - four of eight items in one such round had already been
    // marked useful earlier in that very session. Every filter above still
    // reads the count SINCE THE LAST VERDICT, with no floor in time or in
    // turns, so a pinned or often-touched item crosses `JUDGEMENT_DEBT_AFTER`
    // again within minutes in a busy session where nearly every tool call
    // serves it - the verdict is asked, given, and immediately asked again.
    // For a new user that reads as nagging, and the honest answer stops
    // being read.
    //
    // WHY THIS IS A WATERMARK AND NOT A FILTER ON THE MARK EVENT'S OWN
    // `session_id` COLUMN, THE WAY `seen` ABOVE READS `item_served` BACK.
    // Every event carries a `session_id`, but a verdict recorded the way a
    // real session actually records one - calling the `mark` tool - is
    // stamped by the tool server with the shared constant `mcp::SESSION_ID`
    // ("mcp"), never the caller's own Claude Code session id; `serve mark`'s
    // own CLI path stamps a second shared constant, "cli" (`cmd_mark`
    // below). A filter keyed on that column would compare this turn's real
    // session id against a literal no verdict ever actually carries, match
    // nothing, and silently do nothing at all - the identical shape
    // `crowding_debt` (below) was already bitten by and fixed for on
    // 2026-08-14: "every write through the tool server is stamped with the
    // constant 'mcp'... never the caller's own session, so the Stop hook -
    // which does know the real one - matched nothing and the debt never
    // fired." This reuses that exact fix rather than repeating the mistake:
    // a WATERMARK, recorded at SessionStart (`record_session_watermark`,
    // which DOES see the real session id, since Claude Code's own
    // SessionStart payload carries it) - "was this id marked, useful or
    // noise, at a later position in the log than where this session
    // started" - true regardless of which literal a verdict happened to be
    // stamped with, because it never reads that column at all. (Since step
    // 12 the tool server stamps a verdict with its session hint when it has
    // one, still "mcp" without one; and only an ATTESTED verdict counts here,
    // see `judged_since`.)
    //
    // `session_watermark` returning `None` (no SessionStart was ever seen
    // for this session id - an old install, a hook that failed to write, or
    // `resolve_session_id`'s own synthetic fallback) fails OPEN: this filter
    // is simply skipped rather than guessed at, the same stance
    // `crowding_debt` already takes on a missing watermark of its own.
    // Nothing about the threshold or the "since last verdict" fold changes -
    // a session that ends and comes back later, or a different session
    // entirely, is asked about the very same item the moment it is next
    // owed, because `served` above never stopped counting underneath this
    // filter; only asking AGAIN, from the session whose own watermark the
    // verdict already falls after, is what stops.
    if let Some(watermark) = session_watermark(db_path, session_id) {
        owed.retain(|(id, _)| !judged_since(store, id, watermark));
        if owed.is_empty() {
            return None;
        }
    }
    owed.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let total_owed = owed.len();
    let batch_len = total_owed.min(JUDGEMENT_DEBT_BATCH_MAX);
    let mut items_block = String::new();
    // WHERE IT FIRED IN THIS SESSION (step 24): its last distinct places here,
    // the same lines the evaluation's start note shows.
    let batch_ids: Vec<String> = owed.iter().take(JUDGEMENT_DEBT_BATCH_MAX).map(|(id, _)| id.clone()).collect();
    let fired_here = model::store::session_firings(store, session_id, &batch_ids, i64::MAX);
    for (id, count) in owed.iter().take(JUDGEMENT_DEBT_BATCH_MAX) {
        let text = model::store::show(store, id).ok().map(|i| i.text).unwrap_or_default();
        if let Some(fired) = fired_here.get(id).filter(|f| !f.triggers.is_empty()) {
            let places = fired.triggers.iter().map(|t| format!("'{t}'")).collect::<Vec<_>>().join(" | ");
            items_block.push_str(&format!(
                "\n- '{id}' ({count}x since last judged, if ever; {} time(s) in this session, last at {places}): {text}",
                fired.count
            ));
            continue;
        }
        // ", last fired at '<trigger>'" - the same clause `ops::health::
        // judgement_debt_line`'s own named list prints, and trivial to add
        // here once `model::served::last_trigger` existed for that line to
        // call: an evaluation judging "did it belong where it fired" needs
        // the place, not only the binding, and this is the live Stop-time
        // ask, not only the cold `doctor` report.
        let last_fired = match model::served::last_trigger(store, id) {
            Some(trigger) => format!(", last fired at '{trigger}'"),
            None => String::new(),
        };
        items_block.push_str(&format!("\n- '{id}' ({count}x since last judged, if ever){last_fired}: {text}"));
    }
    let held_back = total_owed - batch_len;
    let held_back_note = if held_back > 0 {
        format!(" {held_back} more beyond this batch will follow on a later turn.")
    } else {
        String::new()
    };
    Some(format!(
        "[THOR] {batch_len} item(s) have fired repeatedly since they were last judged, if ever - listed below. Judge ALL {batch_len} before ending the turn: call mark once per id, noise:true if it did not belong where it fired, or plain if it helped.{held_back_note} FIRST look at whether each is still TRUE - if the code or the file says otherwise, revise it against what you can see there; that is a repair, not noise, and marking it noise would leave a wrong fact in place. Two noise judgements since the last mark of usefulness retire an item from every injection surface while leaving it findable; a mark of usefulness clears the noise recorded before it, and a noise mark recorded after it still counts. Nothing else in this system ever retires noise - a serving count decides nothing and silence decides nothing.{items_block}"
    ))
}

/// Has `id` received an ATTESTED verdict (useful or noise, either one) at a
/// strictly later position in the log than `after_seq`? See
/// `judgement_debt`'s own doc comment on its watermark check for why this
/// reads log POSITION rather than a verdict's own `session_id` column.
///
/// ONLY AN ATTESTED VERDICT COUNTS HERE (step 12, D9), exactly as in
/// `usefulness::served_since_last_verdict`: a verdict whose recorded
/// verification (the `attester` record on the same item, written at a Stop)
/// says it was made through an attested THOR call. Measured before this: a
/// `serve mark` typed into a shell after the session's start left the count
/// owed, yet silenced the ask for the rest of the session through this floor.
///
/// An indexed per-id read (`EventStore::get_events_by_entity`,
/// `idx_event_entity`) rather than `crowding_debt`'s own `get_all_events`
/// scan of the whole log: `judgement_debt` only ever reaches this for the
/// handful of ids still standing after every earlier filter, so paying per
/// id here is cheaper than paying for every event on the log on every Stop
/// that owes anything, and the store this was measured against holds well
/// into five figures of events.
fn judged_since(store: &EventStore, id: &str, after_seq: i64) -> bool {
    let Ok(events) = store.get_events_by_entity(id) else { return false };
    let verified = model::store::recorded_write_verdicts(&events);
    events.iter().any(|e| {
        e.seq > after_seq
            && matches!(
                e.kind,
                thor_core::event_store::EventKind::ItemMarkedUseful
                    | thor_core::event_store::EventKind::ItemMarkedNoise
            )
            && verified.get(&e.seq).is_some_and(|v| v.attested)
    })
}

/// What a `Skill` (or `SlashCommand`) call opening the evaluation routine
/// is told, and whether it starts an evaluation (step 13, F16). In order:
/// - an evaluation of `project` already open in THIS session: named again,
///   "continue it", with its start note - nothing new is written;
/// - the evaluation is due now (`evaluation_debt` would ask for it at this
///   session's Stop): it starts here, one start record in the log, and the
///   answer is its start note. Never for a subagent: the Stop never asks one;
/// - an evaluation of `project` open in ANOTHER session: where, and since
///   when - nothing starts;
/// - otherwise: none is due - nothing starts.
///
/// A hook run that records nothing (`no_record`) starts nothing either.
fn skill_call_answer(
    store: &mut EventStore,
    db_path: &Path,
    session_id: &str,
    project: &str,
    work: Option<&usefulness::SessionWorkState>,
    payload: &Value,
    no_record: bool,
) -> String {
    if let Some(open) = serve::eval_session::open_evaluation(store, session_id, project) {
        // An open evaluation whose machine pass was cut off is finished here
        // (`start` keeps the open one and only writes its batch record).
        let open = match (open.frozen.decided(), no_record) {
            (false, false) => {
                serve::eval_session::start(db_path, store, session_id, project, "skill").map_or(open, |started| started.start)
            }
            _ => open,
        };
        return serve::eval_session::continue_note(&open, true);
    }
    // Due as the Stop would ask THIS session (step 17: the project's clock,
    // ten minutes of the session's own work, no evaluation running
    // elsewhere) - except that opening the routine is the session's own
    // choice, so its first Stop not having come yet does not hold it back.
    let due = !payload_is_from_a_subagent(payload)
        && work.is_some()
        && {
            let now = time::now_unix();
            let state = usefulness::update_eval_debt_state(store, db_path, Some(project), now);
            let timeline = serve::attest::transcripts_root()
                .map(|root| serve::attest::session_timeline(&root, session_id))
                .unwrap_or_default();
            matches!(
                usefulness::stop_decision(store, &state, project, session_id, &timeline, false, false, now, usefulness::EVAL_CADENCE),
                usefulness::StopDecision::Ask(_)
            )
        };
    if due && !no_record {
        match serve::eval_session::try_start(db_path, store, session_id, project, "skill") {
            Ok(started) => {
                return match started.fresh {
                    true => serve::eval_session::start_note(&started.start),
                    false => serve::eval_session::continue_note(&started.start, true),
                };
            }
            Err(serve::eval_session::StartRefused::RunningElsewhere { start, idle_minutes }) => {
                return serve::eval_session::running_elsewhere_note(&start, idle_minutes);
            }
            Err(serve::eval_session::StartRefused::Store) => {}
        }
    }
    let now = time::now_unix();
    if let Some((open, idle)) = usefulness::evaluation_running_elsewhere(store, project, session_id, now) {
        return serve::eval_session::running_elsewhere_note(&open, idle);
    }
    let elsewhere = model::store::open_evaluation_starts_of_project(store, project, now);
    match elsewhere.iter().rev().find(|s| s.frozen.session != session_id) {
        Some(open) => serve::eval_session::continue_note(open, false),
        None => serve::eval_session::not_due_note(project),
    }
}

/// Ask for the WHOLE end-of-session evaluation, or nothing - BLOCKS THE
/// FIRST STOP OF EVERY TURN while the obligation holds (2026-09-16, fourth
/// rewrite: the owner does not want to ever have to run an evaluation
/// himself), not merely once per session. The subagent exclusion lives at
/// this function's own call site (above, in `hook_once`'s `Stop` arm); the
/// "once per TURN, not once per session" loop safety this now needs comes
/// from that same `Stop` arm's own `stop_hook_active` handling at its very
/// top, shared with every other debt in this function, never a second copy
/// of it here. This decides only WHETHER the obligation holds right now and
/// what to say, reading the sidecar `update_eval_debt_state` already
/// refreshed moments earlier at that same call site.
///
/// SCOPED TO THIS CHECKOUT AND THIS SESSION, every part of the condition -
/// but, since the fifth rewrite (2026-09-17, `usefulness`'s own "evaluation
/// debt" section), through `work: &usefulness::SessionWorkState` handed in
/// by the call site rather than measured here: `usefulness::record_hook_
/// event` already folded this very Stop's own event into it moments
/// earlier, so this function stays a pure translation of "accrued seconds"
/// into "does the debt hold, and what does it say" - no session id, no
/// store read for "how long has this session worked here" (the retired
/// `session_first_served_in_project` used to answer that here; that
/// question no longer has an answer this function computes at all).
/// `usefulness::project_eval_state` reads this exact project's own sidecar
/// facts, and `usefulness::eval_debt_owed` is the one shared predicate
/// `doctor` (`ops::health::judgement_debt_line`, via its time-only half
/// `eval_report_covers`) evaluates against the identical sidecar field, so
/// the two can never disagree about whether the last evaluation still
/// covers this project. `usefulness::judgement_debt_counts`'s own second
/// number is read too, but only for the message's own context - it is no
/// longer part of the condition.
///
/// TWO DIFFERENT MESSAGES, depending on `usefulness::eval_report_covers`: no
/// evaluation covering this project yet names how many times it has been
/// asked and since when, exactly as before the fifth rewrite (2026-09-17
/// renamed this branch's own trigger from "today" to "the last
/// `usefulness::EVAL_REPORT_COVERS_HOURS` hours" - seventh rewrite,
/// `usefulness`'s own "evaluation debt" section); one that still covers (so
/// this is necessarily a REPEAT ask - `eval_debt_owed` itself would have
/// stayed silent otherwise) instead names the hours accrued since that
/// report and the report's own id, and says the evaluation covers only what
/// happened since it plus the state of the work (`eval-command.example.md`'s
/// own new SCOPE clause). Either way
/// `usefulness::record_eval_debt_asked` still runs at the call site, so the
/// ask count `doctor` reports stays accurate even for a repeat ask whose
/// own message does not quote it.
///
/// READ-ONLY: this never writes the sidecar - see `usefulness::
/// update_eval_debt_state`'s own doc comment for why that write happens
/// once, unconditionally, at the call site, before the gate that decides
/// whether this function is even reached. The other half,
/// `usefulness::record_eval_debt_asked`, is likewise only ever called from
/// the call site, and only once this function has already returned `Some`.
///
/// NEVER SPEAKS FOR `current_project: None`, EVEN THOUGH THE CALL SITE
/// ALREADY GUARDS THIS TOO (`hook_once`'s `Stop` arm, `stop_project.is_
/// some()`) - a second, independent guard on purpose, not a redundant one:
/// a checkout with no project can never file the Report that would silence
/// this (`model::gate`'s ground 21, `NO_SCOPE_PROBLEM`), so asking would
/// have no honest way out regardless of which caller reaches this function.
/// Repeating the check here means that stays true even if some future
/// caller forgets, or bypasses, the call site's own gate.
///
/// STEP 17: the Stop no longer calls this. It decides WHO is asked about
/// WHICH project in `eval_stop_ask` (`usefulness::stop_decision`) and words
/// the ask with `evaluation_ask`. This stays as the unit tests' door to the
/// project half: `work` is the PROJECT's clock, and this session's own
/// minutes are taken to be the same.
#[cfg(test)]
fn evaluation_debt(
    store: &EventStore,
    db_path: &Path,
    current_project: Option<&str>,
    work: &usefulness::SessionWorkState,
) -> Option<String> {
    let project = current_project?;
    let state = usefulness::project_eval_state(db_path, Some(project));
    let by_floor = usefulness::project_due(store, &state, project, work, time::now_unix(), usefulness::EVAL_CADENCE)?;
    let clock = usefulness::EvalClock { work: work.clone(), own_minutes: work.accrued_secs / 60, by_floor };
    Some(evaluation_ask(store, db_path, project, &clock, Some(project)))
}

/// The text of a main session's Stop held by `reason`, with the evaluation
/// ask riding along (steps 20 and 21): the drift line and the "running
/// elsewhere" line first, then `reason`, then the ask with the start note of
/// the evaluation it starts - when this session owes it now. Every block that
/// holds a first-pass Stop before the ask's own turn (the Response Guard,
/// setup, the capture check, the crowding question) goes through here, so
/// none of them starves the ask, and each Stop asks at most once.
fn with_eval_ask(
    db_path: &Path,
    session_id: &str,
    stop_root: Option<&Path>,
    stop_project: Option<&str>,
    drift_note: Option<&str>,
    no_record: bool,
    reason: String,
) -> String {
    let mut parts: Vec<String> = drift_note.map(str::to_string).into_iter().collect();
    let mut ask: Option<String> = None;
    match stop_eval_decision(db_path, session_id, stop_root, stop_project, drift_note.is_none(), no_record) {
        EvalStop::Ask(text) => ask = Some(text),
        EvalStop::Elsewhere(line) => parts.push(line),
        EvalStop::Quiet => {}
    }
    parts.push(reason);
    parts.extend(ask);
    parts.join("\n\n")
}

/// What a Stop inside an already-held turn says about the evaluation this
/// session owes (step 24, `evaluation_after_a_hold`).
enum Rehold {
    /// Hold the turn again, with this reason.
    Hold(String),
    /// Do not hold, but say this - never a silent release while it is owed.
    Say(String),
}

/// How many times one turn may be held again for the evaluation it owes
/// (`evaluation_after_a_hold`) - each time only after the session did
/// something since the last hold, so even this is never reached by a session
/// that does nothing.
const REHOLD_MAX: u32 = 5;

/// Where a session's turn stands for `evaluation_after_a_hold`: the log's
/// tip at its last Stop, and how many times this turn was held again since
/// its first Stop. One entry per session, in a sidecar beside the store.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
struct TurnMark {
    at_seq: i64,
    holds: u32,
}

fn turn_marks_path(db: &Path) -> std::path::PathBuf {
    db.parent().unwrap_or_else(|| Path::new(".")).join("eval-turn-marks.json")
}

fn read_turn_marks(db: &Path) -> std::collections::BTreeMap<String, TurnMark> {
    std::fs::read_to_string(turn_marks_path(db)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

fn write_turn_mark(db: &Path, session_id: &str, mark: TurnMark) {
    let mut all = read_turn_marks(db);
    all.insert(session_id.to_string(), mark);
    if let Ok(text) = serde_json::to_string(&all) {
        let _ = serve::store_dir::write(&turn_marks_path(db), text);
    }
}

/// A first-pass Stop: the log's tip now, and no hold yet in this turn.
fn record_turn_mark(db: &Path, session_id: &str) {
    let Ok(store) = serve::store_dir::open_existing(db) else { return };
    let at_seq = store.get_next_seq().map(|n| n - 1).unwrap_or(0);
    write_turn_mark(db, session_id, TurnMark { at_seq, holds: 0 });
}

/// Whether `session_id` did anything on the log after `at_seq`: a served
/// item, a verdict or a write of its own (THOR's own records never count).
fn session_did_something_since(store: &EventStore, session_id: &str, at_seq: i64) -> bool {
    store.events_since(at_seq).unwrap_or_default().iter().any(|e| {
        let served_here = e.kind == thor_core::event_store::EventKind::ItemServed && e.session_id == session_id;
        let written = (e.session_id == session_id || e.session_id == "mcp") && !model::store::RESERVED_ACTORS.contains(&e.actor.as_str());
        served_here || written
    })
}

/// THE EVALUATION OWED INSIDE AN ALREADY-HELD TURN (step 24, field defect 2).
/// `None`: this session owes no evaluation report now. Otherwise:
/// - its evaluation is open and its report not filed: hold again, saying to
///   file it now (the writes of this turn were just checked at this Stop),
///   with what is still owed;
/// - its newest evaluation of a project was closed as VOID and none is open:
///   the fresh evaluation the void promised starts now when the project is
///   still due here - or one line says which session runs it, or that none is
///   due;
/// - and never a hold when nothing was done since the last one, or after
///   `REHOLD_MAX` holds in this turn: then one line says it is still owed.
fn evaluation_after_a_hold(db_path: &Path, session_id: &str, payload: &Value) -> Option<Rehold> {
    let store = serve::store_dir::open_existing(db_path).ok()?;
    let now = time::now_unix();
    let mut newest: std::collections::BTreeMap<String, model::store::EvaluationStart> = std::collections::BTreeMap::new();
    for start in model::store::evaluation_starts_of_session(&store, session_id) {
        newest.insert(start.frozen.project.clone(), start);
    }
    let open: Vec<&model::store::EvaluationStart> =
        newest.values().filter(|s| model::store::evaluation_still_open(&store, s, now)).collect();
    let void: Vec<&model::store::EvaluationStart> = newest
        .values()
        .filter(|s| s.frozen.is_void() && now - s.frozen.started_unix <= model::store::EVALUATION_OPEN_HOURS * 3600)
        .collect();
    if open.is_empty() && void.is_empty() {
        return None;
    }
    // A transcript THOR cannot read can never carry a filing that counts.
    if serve::eval_filing::session_drifted(db_path, serve::attest::transcripts_root().as_deref(), session_id) {
        return None;
    }
    // No mark: this turn's first Stop was never seen (a session that started
    // before this build). Nothing proves the session did anything since the
    // hold, so this Stop says so and does not hold; the next turn's first
    // Stop records a mark.
    let mark = read_turn_marks(db_path).remove(session_id);
    let tip = store.get_next_seq().map(|n| n - 1).unwrap_or(0);
    let progressed = mark.as_ref().is_some_and(|m| session_did_something_since(&store, session_id, m.at_seq));
    let holds = mark.as_ref().map_or(0, |m| m.holds);
    let may_hold = progressed && holds < REHOLD_MAX;
    let held = |reason: String| {
        write_turn_mark(db_path, session_id, TurnMark { at_seq: tip, holds: holds + 1 });
        Rehold::Hold(reason)
    };
    let quiet = |line: String| {
        write_turn_mark(db_path, session_id, TurnMark { at_seq: tip, holds });
        Rehold::Say(line)
    };
    if let Some(start) = open.first() {
        let project = &start.frozen.project;
        if !may_hold {
            return Some(quiet(format!(
                "[THOR] The evaluation of '{project}' ('{}') is still open and its report is not filed. This Stop does not \
                 hold the turn again ({}), so the next turn's first Stop asks for it again - file the report then, with \
                 the THOR tool itself.",
                start.id,
                match (&mark, progressed) {
                    (None, _) => "this turn's first Stop was not seen by this build",
                    (Some(_), true) => "this turn was held for it often enough",
                    (Some(_), false) => "nothing was done since the last hold",
                }
            )));
        }
        if !start.frozen.decided() {
            return Some(held(serve::eval_session::resumed_note(start)));
        }
        let judgement = model::store::evaluation_judgement_owed(&store, start, &[]);
        let teeth = model::store::evaluation_frozen_batch_owed(&store, start);
        let foreign = model::store::frozen_writes_in_window(&store, start, None).foreign_pending.len();
        let foreign = match foreign {
            0 => String::new(),
            n => format!(", {n} write(s) by another session not checked yet"),
        };
        return Some(held(format!(
            "[THOR] The evaluation of '{project}' ('{}') is still open and its report is not filed, so this turn is held \
             again. Every verdict and write of this turn was checked at this Stop: file the report NOW, in this turn, \
             with the THOR tool itself - mcp__thor__remember with kind report, project '{project}', tags \
             [\"evaluation-report\"], id eval-{project}-<date> and your own plain summary as its text. If anything is still \
             owed, the filing names it, one by one; do that and file again. Owed right now: {} judgement item(s) without \
             a verdict or a not-judgeable tag, {} rule(s) of the batch and the A4 list{foreign}. Do not tell the owner \
             'next turn': file in this one.",
            start.id,
            judgement.owed.len(),
            teeth.len()
        )));
    }
    // The newest evaluation is void: the fresh one its refusal promised.
    let start = void.first()?;
    let project = start.frozen.project.clone();
    let why = start.frozen.void.as_ref().map(|v| format!("'{}' was changed at log seq {} ({})", v.id, v.seq, v.door)).unwrap_or_default();
    if !may_hold {
        return Some(quiet(format!(
            "[THOR] The evaluation of '{project}' ('{}') was closed as void ({why}). This Stop does not hold the turn again, \
             so the next turn's first Stop starts a fresh one when it is still due.",
            start.id
        )));
    }
    let stop_root: Option<PathBuf> =
        payload.get("cwd").and_then(|v| v.as_str()).map(PathBuf::from).or_else(|| std::env::current_dir().ok());
    let stop_project = serve::project::session_project(db_path, session_id, stop_root.as_deref());
    drop(store);
    match stop_eval_decision(db_path, session_id, stop_root.as_deref(), stop_project.as_deref(), true, false) {
        EvalStop::Ask(text) => Some(held(format!(
            "[THOR] The evaluation of '{project}' ('{}') was closed as void ({why}), so its report could not count. The \
             fresh evaluation it promised starts now - its start note is below.\n\n{text}",
            start.id
        ))),
        EvalStop::Elsewhere(line) => Some(quiet(format!(
            "[THOR] The evaluation of '{project}' ('{}') was closed as void ({why}).\n\n{line}",
            start.id
        ))),
        EvalStop::Quiet => Some(quiet(format!(
            "[THOR] The evaluation of '{project}' ('{}') was closed as void ({why}), and no fresh evaluation is due here \
             now - no report is owed by this session for it.",
            start.id
        ))),
    }
}

/// Whether the teeth question may hold this main session's Stop (step 21):
/// the Stop was counted in `stop_project`, and the session passes the
/// evaluation ask's own gates there (`usefulness::upkeep_may_hold` - ten
/// minutes of its own work, not its first Stop). A Stop in no project holds
/// nothing for rule upkeep, as it is never asked for an evaluation. The
/// transcript is read only when the sidecar alone does not already pass:
/// it can only add minutes and earlier turns.
fn teeth_upkeep_may_hold(db_path: &Path, session_id: &str, stop_project: Option<&str>) -> bool {
    let Some(project) = stop_project else { return false };
    let state = usefulness::project_eval_state(db_path, Some(project));
    if usefulness::upkeep_may_hold(&state, project, session_id, &[], true).is_ok() {
        return true;
    }
    let timeline = serve::attest::transcripts_root()
        .map(|root| serve::attest::session_timeline(&root, session_id))
        .unwrap_or_default();
    usefulness::upkeep_may_hold(&state, project, session_id, &timeline, true).is_ok()
}

/// What the Stop decided about the evaluation (step 17, `eval_stop_ask`).
enum EvalStop {
    /// Hold the turn: the ask, with the start note of the evaluation it
    /// starts.
    Ask(String),
    /// Another session runs the evaluation: one line that rides with the
    /// rest of this Stop, no hold (D12).
    Elsewhere(String),
    Quiet,
}

/// THE STOP'S EVALUATION DECISION (step 17). Reads this session's own
/// transcript once (`attest::session_timeline`, bounded to each file's
/// tail), then walks every project the session worked in
/// (`usefulness::projects_of_session`: the folder's own project first),
/// refreshing each project's facts from the log (`update_eval_debt_state`,
/// the one place a valid evaluation resets a clock) and asking
/// `usefulness::stop_decision` whether THIS session is the one to ask. The
/// first project that says so is asked about - one ask per turn - and its
/// evaluation starts in the log; a project whose evaluation runs in another
/// session gives the one line instead. `may_ask` is false while the
/// session's transcript is in a shape THOR cannot read (D14): the facts are
/// still refreshed, nothing is asked.
/// Where a Stop stands: the project it was counted in (the folder's, else
/// the session's last - `project::session_project`) and the folder's own
/// project, if any.
struct EvalStopAt<'a> {
    project: Option<&'a str>,
    folder: Option<&'a str>,
}

/// The Stop's evaluation decision for a main session (`eval_stop_ask`), with
/// the store opened here: `stop_root` is the Stop's folder, `stop_project`
/// the project it was counted in. Asked from two places - the Response
/// Guard's block, which the ask rides with (step 20), and the Stop's own
/// turn when the guard had no block - and never twice in one Stop.
fn stop_eval_decision(
    db_path: &Path,
    session_id: &str,
    stop_root: Option<&Path>,
    stop_project: Option<&str>,
    may_ask: bool,
    no_record: bool,
) -> EvalStop {
    let Ok(mut store) = serve::store_dir::open_existing(db_path) else { return EvalStop::Quiet };
    // The folder's own answer, for the words only: when the ask is about
    // another project, it says where the session stands.
    let folder_own = stop_root.and_then(project::resolve_project);
    let at = EvalStopAt { project: stop_project, folder: folder_own.as_deref() };
    eval_stop_ask(&mut store, db_path, session_id, at, time::now_unix(), may_ask, no_record)
}

fn eval_stop_ask(
    store: &mut EventStore,
    db_path: &Path,
    session_id: &str,
    at: EvalStopAt,
    now: i64,
    may_ask: bool,
    no_record: bool,
) -> EvalStop {
    let folder_project = at.project;
    let timeline = serve::attest::transcripts_root()
        .map(|root| serve::attest::session_timeline(&root, session_id))
        .unwrap_or_default();
    let candidates = usefulness::projects_of_session(
        &usefulness::read_eval_debt_state(db_path),
        session_id,
        folder_project,
        &serve::attest::projects_in(&timeline),
    );
    let mut elsewhere: Option<String> = None;
    for project in candidates {
        let state = usefulness::update_eval_debt_state(store, db_path, Some(&project), now);
        if !may_ask {
            continue;
        }
        let stop_here = folder_project == Some(project.as_str());
        match usefulness::stop_decision(store, &state, &project, session_id, &timeline, stop_here, true, now, usefulness::EVAL_CADENCE) {
            usefulness::StopDecision::Ask(clock) => {
                let reason = evaluation_ask(store, db_path, &project, &clock, at.folder);
                usefulness::record_eval_debt_asked(db_path, Some(&project), now);
                // The ask is where this evaluation starts: one start record
                // in the log freezes the three lists its report is held to,
                // and the note naming them rides with the ask
                // (`serve::eval_session`). A later turn's ask finds it open
                // and keeps it.
                let started = match no_record {
                    true => None,
                    false => match serve::eval_session::try_start(db_path, store, session_id, &project, "stop-ask") {
                        Ok(started) => Some(started),
                        // Another session started it between the decision and
                        // this start (step 24): one line, no hold.
                        Err(serve::eval_session::StartRefused::RunningElsewhere { start, idle_minutes }) => {
                            elsewhere.get_or_insert_with(|| serve::eval_session::running_elsewhere_note(&start, idle_minutes));
                            continue;
                        }
                        Err(serve::eval_session::StartRefused::Store) => None,
                    },
                };
                return EvalStop::Ask(match started {
                    Some(started) => format!("{reason}\n\n{}", serve::eval_session::start_note(&started.start)),
                    None => reason,
                });
            }
            usefulness::StopDecision::Elsewhere(start, idle_minutes) => {
                elsewhere.get_or_insert_with(|| serve::eval_session::running_elsewhere_note(&start, idle_minutes));
            }
            usefulness::StopDecision::Quiet(_) => {}
        }
    }
    elsewhere.map_or(EvalStop::Quiet, EvalStop::Elsewhere)
}

/// The words of the evaluation ask for `project`, from `clock` (the
/// project's work clock and this session's own minutes). `folder_project` is
/// where the session stands now: when the ask is about ANOTHER project (the
/// ask follows the work, D11), it opens by saying so.
fn evaluation_ask(
    store: &EventStore,
    db_path: &Path,
    project: &str,
    clock: &usefulness::EvalClock,
    folder_project: Option<&str>,
) -> String {
    let current_project = Some(project);
    let work = &clock.work;
    let now = time::now_unix();
    let accrued_minutes = work.accrued_secs / 60;
    let own_minutes = clock.own_minutes;
    let state = usefulness::project_eval_state(db_path, current_project);
    let (_, owed_in_project) = usefulness::judgement_debt_counts(store, current_project);
    // THE ASK FOLLOWS THE WORK (D11): named when the folder is elsewhere.
    let elsewhere = match folder_project {
        Some(folder) if folder == project => String::new(),
        Some(folder) => format!(
            "[THOR] This session worked in project '{project}' since its last evaluation; this folder belongs to '{folder}'. \
             The ask below is about '{project}' - moving to another folder does not move it.\n\n"
        ),
        None => format!(
            "[THOR] This session worked in project '{project}' since its last evaluation; this folder belongs to no \
             project. The ask below is about '{project}'.\n\n"
        ),
    };
    // A REPORT ON FILE THAT DOES NOT COUNT (step 16): named first, with the
    // validator's reason, so a session that filed one - through an older
    // binary, a script, or a call its transcript does not hold - learns why
    // it is still asked.
    let not_counted = serve::eval_filing::newest_invalid_report(store, current_project)
        .map(|(id, why)| {
            format!(
                "[THOR] The report '{id}' is on file but does not count as this project's evaluation: {why}. The \
                 evaluation is still owed.\n\n"
            )
        })
        .unwrap_or_default();
    // Named when the file is actually there to read - the same location
    // `ops::install::seed_eval_command` writes to, resolved the same way
    // (`usefulness::default_eval_command_path`, `ops/tests` proves the two
    // agree). Otherwise this never claims a file exists that does not: the
    // owner may keep his own routine at a path this cannot guess, or may
    // simply not have run `install` since this feature shipped.
    let path = usefulness::default_eval_command_path();
    let ask = match path {
        Some(p) if p.exists() => {
            format!("read '{}' and do what it says. The owner can type /thor-eval to run it.", p.display())
        }
        // The command names this very store (`--db`, F7), so running it never
        // sets up a second, default memory beside the one this hook reads.
        _ => format!(
            "no evaluation routine is installed at `~/.claude/commands/thor-eval.md` - `{}` writes one there (usable as /thor-eval), or the owner runs their own routine.",
            model::store::install_command_for(Some(db_path))
        ),
    };
    // The project's own clock, when it says more than this session's (B8):
    // every session of the project together, parallel minutes counted once.
    let together = match accrued_minutes > own_minutes {
        true => format!(" (the project, every session together: {accrued_minutes} minute(s) since its last evaluation)"),
        false => String::new(),
    };
    // THE WEEKLY FLOOR (D13): said plainly when it alone made this due.
    let floor = match clock.by_floor {
        true => format!(
            " It was active in the last {days} days and has had no valid evaluation in {days} days, so it is due whatever the \
             work clock says (the weekly floor).",
            days = usefulness::EVAL_FLOOR_DAYS
        ),
        false => String::new(),
    };

    if usefulness::eval_report_covers(state.last_evaluation_seen, now) {
        // THE REPEAT ASK (fifth rewrite, 2026-09-17): names hours, not
        // minutes (the owner's own wording: "after every THREE HOURS OF
        // WORK"), and the report id this accrual reset against, rather than
        // the ask-count/first-asked clause the first-of-day message below
        // uses.
        //
        // NAMES THE RISK TOO (sixth rewrite, 2026-09-17 - see `usefulness`'s
        // own "the repeat's own risk" doc comment): `eval_debt_owed` already
        // guarantees at least one of the two holds when the clock made this
        // due, so `risk_reasons` is empty only for an ask the weekly floor
        // made.
        let hours = own_minutes as f64 / 60.0;
        let report_id = state.last_evaluation_report_id.as_deref().unwrap_or("an earlier report");
        let mut risk_reasons = Vec::new();
        if work.edits_since_test >= usefulness::EVAL_REPEAT_MIN_UNTESTED_EDITS {
            risk_reasons.push(format!("{} code change(s) since the last test or build run", work.edits_since_test));
        }
        if work.compacted_since_anchor {
            risk_reasons.push("the context was summarized".to_string());
        }
        let risk_text = match risk_reasons.is_empty() {
            true => String::new(),
            false => format!(", and {}", risk_reasons.join(" and ")),
        };
        format!(
            "{elsewhere}{not_counted}[THOR] This session has worked here for {hours:.1} hour(s) since the last evaluation report ('{report_id}'){together}\
             {risk_text}.{floor} \
             A new evaluation is due, covering only what happened since then plus the state of the work; \
             {owed_in_project} item(s) currently owe a verdict here. \
             Run the THOR evaluation before you finish: {ask} \
             A turn cannot end until the evaluation report for this project is filed."
        )
    } else {
        // How many times this project has already been asked since its
        // last report, and since when - the count and clock this very ask
        // is about to add one more to (`usefulness::record_eval_debt_asked`,
        // called at the call site once this function returns). Read here,
        // one ask ahead of that write, so the message can say the true
        // count for THIS ask rather than the one before it;
        // `first_asked_since_report.unwrap_or(now)` covers the case where
        // this is the very first ask since the last report, which is
        // exactly the instant the call site is about to stamp.
        let first_asked = state.first_asked_since_report.unwrap_or(now);
        let asked_count = state.asked_count + 1;
        let covers_hours = usefulness::EVAL_REPORT_COVERS_HOURS;
        format!(
            "{elsewhere}{not_counted}[THOR] This project has had no evaluation in the last {covers_hours} hours (project '{project}').{floor} \
             This session has worked here for {own_minutes} minute(s){together}; \
             {owed_in_project} item(s) currently owe a verdict here. It has been asked {asked_count} time(s) - the first \
             {} day(s) ago - with no report filed yet. \
             Run the THOR evaluation before you finish: {ask} \
             A turn cannot end until the evaluation report for this project is filed. After that it is quiet for {covers_hours} hours.",
            usefulness::days_ago(now, first_asked)
        )
    }
}

/// How many open points `SessionStart`'s own note names in full before
/// falling back to a bare count of the rest - the session-start block is
/// already the largest thing this tool injects (`session_start::render`'s
/// own doc comment), so this stays small on purpose, the same reasoning
/// `TEETH_DEBT_BATCH_MAX` already gives the backlog burn's own batch.
const OPEN_POINTS_SESSION_START_CAP: usize = 5;

/// This project's own open points (`model::store::open_points`), formatted
/// for `SessionStart` - capped at `OPEN_POINTS_SESSION_START_CAP`, id plus
/// one short line each, a bare count for whatever does not fit. THOR's own
/// points come first and say so; a legacy one (filed by an agent before THOR
/// wrote them itself) is marked, because it settles nothing and holds no
/// filing. `None` when the project has none open.
///
/// (The message that asks for an evaluation no longer lists them: the start
/// note that rides with it does, from the start record's frozen lists.)
///
/// OVER THE OWNER'S BUDGET (step 16, B6): when more of THOR's own points of
/// the project wait on the owner (`model::store::owner_queue`) than
/// `model::store::OPEN_POINTS_OWNER_BUDGET`, the block opens with the
/// banner that says so. Nothing is refused over it. The call site never
/// shows this block to a subagent.
fn open_points_session_start_note(store: &EventStore, project: Option<&str>) -> Option<String> {
    let points = model::store::open_points(store, project);
    if points.is_empty() {
        return None;
    }
    let machine = points.iter().filter(|p| p.machine).count();
    let queue = project.map(|p| model::store::owner_queue(store, p)).unwrap_or_default();
    let mut lines: Vec<String> = project.and_then(|p| model::store::owner_queue_banner(p, queue.len())).into_iter().collect();
    lines.push(format!(
        "[THOR] {} open point(s) in this project: {machine} written by THOR itself (each says what clears it, and closes \
         itself), {} legacy (filed by an agent earlier; they settle nothing and hold no filing):",
        points.len(),
        points.len() - machine
    ));
    for p in points.iter().take(OPEN_POINTS_SESSION_START_CAP) {
        let short: String = p.text.chars().take(80).collect();
        let ellipsis = if p.text.chars().count() > 80 { "..." } else { "" };
        let in_queue = queue.iter().any(|(q, _)| q.id == p.id);
        let whose = match (p.machine, in_queue) {
            (true, true) => "THOR's own, the owner decides",
            (true, false) => "THOR's own",
            (false, _) => "legacy",
        };
        lines.push(format!("- '{}' ({whose}): {short}{ellipsis}", p.id));
    }
    if points.len() > OPEN_POINTS_SESSION_START_CAP {
        lines.push(format!(
            "...{} more (`lookup` has the rest).",
            points.len() - OPEN_POINTS_SESSION_START_CAP
        ));
    }
    Some(lines.join("\n"))
}

/// Which capture-guard mode is live right now, for THIS store
/// (OPTION4-IMPLEMENTATION.md): the one place that does the I/O
/// `capture::resolve_capture_mode` itself deliberately stays free of, so that
/// pure decision rule can be unit-tested without touching a filesystem at
/// all. Reads `guard-capture-mode.json` (explicit override, if present and
/// parseable) and whether a real, usable judge config exists
/// (`judge::default_judge_config_path`/`judge::parse_judge_config`) - the
/// signal `resolve_capture_mode` falls back to inferring from when no
/// explicit mode is set.
fn load_capture_mode(db_path: &Path) -> capture::CaptureMode {
    let mode_text = std::fs::read_to_string(capture::default_mode_config_path(db_path)).ok();
    let judge_configured = std::fs::read_to_string(judge::default_judge_config_path(db_path))
        .ok()
        .and_then(|text| judge::parse_judge_config(&text))
        .is_some();
    capture::resolve_capture_mode(mode_text.as_deref(), judge_configured)
}

/// The judge decides; its fallback (the deterministic rulebook, capped to
/// warn-only - `capture::fallback_decision`) never blocks. Every error - no
/// judge config beside the store, an unreadable/malformed config, a command
/// that cannot be spawned, a run that exceeds its configured timeout, or an
/// answer that does not parse into BLOCK/WARN/ALLOW - falls through to the
/// fallback. See `judge`'s own doc comment and `JUDGE-TRANSPORT.md` for the
/// whole fallback ladder. Only ever called from `capture_stop_check` (Stop)
/// - never from `capture_flag` (UserPromptSubmit), which is the whole point
/// of moving classification here (SPEC: "the judge runs at Stop, never at
/// UserPromptSubmit").
fn classify_capture(db_path: &Path, prompt_text: &str) -> Option<capture::FlagDecision> {
    let judge_config_text = std::fs::read_to_string(judge::default_judge_config_path(db_path)).ok();
    if let Some(config) = judge_config_text.as_deref().and_then(judge::parse_judge_config) {
        if let Some(verdict) = judge::run_judge(&config, prompt_text) {
            return capture::from_judge_verdict(verdict);
        }
    }
    let rulebook_text = std::fs::read_to_string(capture::default_rulebook_path(db_path)).ok();
    capture::fallback_decision(rulebook_text.as_deref(), prompt_text)
}

/// C3 (SPEC-ENFORCEMENT.md 2.3, restored 2026-08-05 - see `capture::sink_verdict`'s
/// own doc comment): decide what happens to a write at a mirror sink. Reuses
/// the store `hook_once` already has open for this PreToolUse call (unlike
/// C2, this arm never returns before the store is opened, so there is no
/// separate "read-only, may open its own" concern here). Every failure - no
/// marker, store read failure - falls through to `SinkVerdict::Allow`. NEVER
/// called for a subagent - see the `PreToolUse` arm above and
/// `payload_is_from_a_subagent`'s own doc comment.
fn capture_sink_check(
    store: &thor_core::event_store::EventStore,
    db_path: &Path,
    session_id: &str,
    file_path: Option<&str>,
) -> capture::SinkVerdict {
    // `Off` disables the whole capture guard, C3 included - checked first, no
    // marker file even read (OPTION4-IMPLEMENTATION.md).
    if load_capture_mode(db_path) == capture::CaptureMode::Off {
        return capture::SinkVerdict::Allow;
    }
    let marker_path = capture::default_marker_path(db_path);
    let Ok(marker_text) = std::fs::read_to_string(&marker_path) else { return capture::SinkVerdict::Allow };
    let markers = capture::read_markers(&marker_text);
    let Some(marker) = markers.get(session_id) else { return capture::SinkVerdict::Allow };
    let Ok(events) = store.events_since(marker.seq_at_flag) else { return capture::SinkVerdict::Allow };
    let kinds: Vec<_> = events.into_iter().map(|e| e.kind).collect();
    let paid = capture::debt_paid(&kinds);
    capture::sink_verdict(Some(marker), paid, file_path)
}

/// Write down what the gate just DID.
///
/// This is the measurement that did not exist until 2026-08-08. Every other
/// part of 2.0 was counted - items, checks, dead anchors, crowding - and the
/// one capability the whole version was built for, refusing a wrong write,
/// had no number at all. Nobody could say how often the gate fired, and
/// nobody could say how often it had something to say and said nothing,
/// which is how a once-per-session stand-down survived from the first day
/// until four independent reviews read the code out loud.
///
/// Opens its OWN handle: the guard holds a read-only store and a refusal
/// must never wait on, or be lost to, a writable one. Fail-silent from top
/// to bottom, for the same reason every sidecar here is - a log that cannot
/// take a measurement must never cost a refusal.
/// `subject` is the id the outcome is filed under: a rule id for a real
/// prohibition (read back out of the wording with `absent_guard::rule_id_of`
/// - for the PreToolUse guards by `serve::guard_verdict`, into
/// `Verdict::rule_id`), or the NAME of a
/// guard when what stood aside was the guard itself rather than any one rule.
/// Head-neutral either way, so neither can ever create or move an item.
/// `actor` is the REAL actor this hook run is stamped with (`hook_once`'s own
/// `actor` argument - "hook" for a normal Claude Code run, whatever
/// `--actor` validated otherwise) - never the literal `"hook"` this function
/// used to hardcode regardless of who actually called it.
///
/// THE DEFECT THIS CLOSES (2026-09-27). Piping a hand-built payload into
/// `serve hook --actor probe` produced a real refusal, and the log recorded
/// it with actor `"hook"` anyway - the flag never reached this write. Since a
/// refusal on record newer than a rule's last revision settles that rule
/// (`model::store::teeth_state`), a probe left no trace of being one: any
/// session could settle a heavy rule with a self-built, even one-sided,
/// payload. See `model::store::ASSISTANT_DELIVERY_ACTOR`'s own use in
/// `RefusalRecord` for the other half of the fix - only a refusal actually
/// stamped this way counts as field evidence at all.
///
/// `tool_use_id` is the payload's own id of the tool call the outcome is
/// about. A refusal carries it so the call can be found later in the
/// session's own transcript (`serve::attest::verify_refusal`): without it a
/// refusal can never be checked, and is never counted as field evidence.
fn record_gate(
    db_path: &Path,
    session_id: &str,
    actor: &str,
    refused: bool,
    subject: &str,
    target: &str,
    tool_use_id: Option<&str>,
) {
    let rule_id = subject.to_string();
    let Ok(mut store) = serve::store_dir::open_existing(db_path) else { return };
    deliver::record_gate_outcome_for_call(
        &mut store,
        session_id,
        session_id,
        actor,
        &time::now_iso8601(),
        refused,
        &rule_id,
        target,
        tool_use_id,
    );
}


/// The PreToolUse guards' own I/O wrapper. Every decision is made by
/// `serve::guard_verdict::pre_tool` (see that module's own doc comment for
/// the arms, their doctrine and their order); this keeps only what is not a
/// decision:
///
/// - the staleness sidecar (`record_absent_guard_staleness`,
///   `record_command_guard_staleness`), recorded from the SAME candidate pools
///   the decision was made on - never a second fetch - for each arm that got
///   as far as fetching them, whether or not anything is refused. Observation
///   only: it never influences the refusal;
/// - the once-per-session marker, which changes only the WORDING of a refusal
///   (`with_repeat_wording`), never whether it happens;
/// - the refusal's record in the log (`record_gate`), under the rule the
///   refusal names.
///
/// In that order, as when the hook decided these itself: the staleness
/// sidecar stamps its findings with the store's current last seq, and the
/// refusal record is an append to that same store.
///
/// Every failure is silence (`None`), never a block - the same "any error,
/// anywhere, is silence" doctrine every other guard in this file follows.
#[allow(clippy::too_many_arguments)]
fn pre_tool_guard_block(
    store: &EventStore,
    db_path: &Path,
    session_id: &str,
    tool_name: &str,
    tool_input: Option<&Value>,
    project: Option<&str>,
    root: Option<&Path>,
    cwd: Option<&Path>,
    actor: &str,
    tool_use_id: Option<&str>,
) -> Option<String> {
    let judgement = guard_verdict::pre_tool(store, tool_name, tool_input, project, root, cwd);
    if let Some(file) = &judgement.pools.file {
        record_absent_guard_staleness(store, db_path, &file.location, &file.ranked, &file.file_path, root);
    }
    if let Some(pool) = &judgement.pools.command {
        record_command_guard_staleness(store, db_path, &pool.items, &pool.segments, &pool.command, root);
    }
    let verdict = judgement.verdict?;
    let reason = match &verdict.repeat {
        Some(guard_verdict::RepeatKey::File(attempt)) => {
            with_repeat_wording(&absent_guard::default_marker_path(db_path), session_id, attempt, verdict.reason)
        }
        Some(guard_verdict::RepeatKey::Command(attempt)) => {
            with_repeat_wording(&absent_guard::default_command_marker_path(db_path), session_id, attempt, verdict.reason)
        }
        None => verdict.reason,
    };
    if let Some(id) = &verdict.rule_id {
        record_gate(db_path, session_id, actor, true, id, &verdict.target, tool_use_id);
    }
    Some(reason)
}

/// A refusal's wording once its marker has been read: a repeat of an attempt
/// this session already had refused gets `absent_guard::escalated`'s wording,
/// and a first attempt is marked so that its repeat will. Keyed per ATTEMPT
/// (`absent_guard::attempt_key`), never per file or per rule alone.
///
/// THE MARKER NEVER DECIDES WHETHER, ONLY WHAT IT SAYS. Until 2026-08-08 the
/// marker was read before any rule was, and an attempt it recognised passed
/// unexamined - the file arm keyed on the file and the command arm on the
/// anchor, so ONE refusal disarmed that rule for the whole session. A repeat
/// is now refused like the first; its text differs only so a caller that
/// reads it learns to stop resending the same thing.
fn with_repeat_wording(marker_path: &Path, session_id: &str, attempt: &str, reason: String) -> String {
    let marker_text = std::fs::read_to_string(marker_path).ok();
    let markers = marker_text.as_deref().map(absent_guard::read_blocked).unwrap_or_default();
    let repeat = absent_guard::already_blocked(&markers, session_id, attempt);
    if !repeat {
        if let Some(text) = absent_guard::mark_blocked_text(marker_text.as_deref(), session_id, attempt) {
            let _ = serve::store_dir::write(marker_path, text);
        }
    }
    if repeat {
        absent_guard::escalated(&reason)
    } else {
        reason
    }
}

/// The staleness sidecar's own I/O wrapper (see `absent_guard::StaleRecord`'s
/// own doc comment for the file's shape): read whatever it already held,
/// fold in every finding from THIS call's own content and location scans,
/// write it back - best effort throughout, exactly like every other marker
/// file in this binary (an unreadable or unwritable sidecar is silence,
/// never a panic and never a reason to change what the caller does). Called
/// for its side effect alone, from `pre_tool_guard_block`, on every call
/// whose file arm fetched its pools - whether or not a block is about to
/// fire - and its result is never inspected by anything that decides one.
///
/// Also stamps every finding it folds in with `now_seq` - the store's own
/// current max seq, the exact computation `capture_flag` already uses for
/// its unrelated `seq_at_flag` - onto `StaleRecord::seq_at_record` (via
/// `record_stale_text`'s own parameter of the same name). This is what lets
/// `stale_guard_stop_check` later answer "has this item been revised or
/// retracted SINCE it was recorded here". A store that will not even report
/// its own next seq is the one failure this best-effort function does not
/// fold into a wrong seq - it skips recording entirely instead, same as any
/// other failure here.
fn record_absent_guard_staleness(
    store: &EventStore,
    db_path: &Path,
    location_candidates: &[serve::live::LiveItem],
    ranked: &[serve::rank::RankedItem],
    file_path: &str,
    root: Option<&Path>,
) {
    let mut findings = absent_guard::stale_in_location(location_candidates, file_path, root);
    findings.extend(absent_guard::stale_in_content(ranked, file_path, root));
    findings.extend(absent_guard::stale_in_dir_content(location_candidates, file_path, root));
    // Every id this pass looked at, so one that has recovered is dropped
    // instead of being reported for the rest of its life.
    let scanned: Vec<String> =
        location_candidates.iter().map(|li| li.id.clone()).chain(ranked.iter().map(|r| r.id.clone())).collect();
    let Ok(next_seq) = store.get_next_seq() else { return };
    let now_seq = next_seq.saturating_sub(1);
    let stale_path = absent_guard::default_stale_path(db_path);
    let existing = std::fs::read_to_string(&stale_path).ok();
    if let Some(text) = absent_guard::record_stale_text(existing.as_deref(), &findings, &scanned, now_seq) {
        let _ = serve::store_dir::write(&stale_path, text);
    }
}


/// The command arm's own staleness I/O wrapper - mirrors
/// `record_absent_guard_staleness` exactly (best effort throughout: an
/// unreadable or unwritable sidecar is silence, never a panic and never a
/// reason to change what the caller does), folding into the SAME
/// `absent-guard-stale.json` sidecar that function already writes
/// (`StaleRecord`/`StaleFinding` are keyed by item id, not by "file" vs
/// "command" - see `absent_guard::stale_in_command`'s own doc comment).
fn record_command_guard_staleness(
    store: &EventStore,
    db_path: &Path,
    command_candidates: &[serve::live::LiveItem],
    segments: &[serve::shell::Segment],
    command: &str,
    root: Option<&Path>,
) {
    let findings = absent_guard::stale_in_command_in(command_candidates, segments, command, root);
    let scanned: Vec<String> = command_candidates.iter().map(|li| li.id.clone()).collect();
    let Ok(next_seq) = store.get_next_seq() else { return };
    let now_seq = next_seq.saturating_sub(1);
    let stale_path = absent_guard::default_stale_path(db_path);
    let existing = std::fs::read_to_string(&stale_path).ok();
    if let Some(text) = absent_guard::record_stale_text(existing.as_deref(), &findings, &scanned, now_seq) {
        let _ = serve::store_dir::write(&stale_path, text);
    }
}

/// The stale-rule guard's own I/O wrapper (see `serve::stale_guard`'s module
/// doc comment for the full doctrine): the THIRD Stop-time check, reached
/// only when neither the Response Guard nor Lane C's capture guard (both
/// above, in `hook_once`'s own Stop arm) had anything to say.
///
/// `Off` (`stale_guard::resolve_mode`) is checked first, the cheapest
/// possible "never blocks anything" - the same shape `capture_stop_check`'s
/// own `Off` check already takes: no marker file, no sidecar, no store,
/// nothing read.
///
/// At most once per session (`stale_guard::already_blocked`/
/// `mark_blocked_text` - its OWN marker file, never `absent_guard`'s, which
/// keys on (session, file) for an unrelated prohibition): once this session
/// has already been told, it is not told again until the NEXT session.
///
/// Settlement is decided per item: for every entry in the stale sidecar,
/// this reads that ONE item's own history (`EventStore::get_events_by_entity`
/// - the same call `model::store::show`/`history` already use), filters it
/// to events after that entry's own `seq_at_record`, and asks
/// `stale_guard::item_settled` (built on the same kind-membership approach
/// `capture::debt_paid` already uses for its own, unrelated debt).
///
/// Every failure - no stale sidecar (nothing has ever gone stale), an
/// unreadable/malformed sidecar or marker, a store that will not open, a
/// per-item history lookup that fails - falls through to `None` (ALLOW),
/// fail-open exactly like every other guard in this file: an entry whose
/// settlement cannot be proven either way is never the reason a turn cannot
/// end silently. A single failed per-item lookup aborts the WHOLE check
/// (via `?`, propagating `None` immediately) rather than guessing for just
/// that one entry - the simplest rule that still never blocks on a doubt.
fn stale_guard_stop_check(db_path: &Path, session_id: &str, actor: &str) -> Option<HookOutput> {
    let mode_text = std::fs::read_to_string(stale_guard::default_mode_config_path(db_path)).ok();
    if stale_guard::resolve_mode(mode_text.as_deref()) == stale_guard::Mode::Off {
        return None;
    }

    let marker_path = stale_guard::default_marker_path(db_path);
    let marker_text = std::fs::read_to_string(&marker_path).ok();
    let blocked = marker_text.as_deref().map(stale_guard::read_blocked).unwrap_or_default();
    // The marker is READ here and acted on at the bottom, after the debt is
    // known. It used to return right here, recording a stand-aside before
    // anything had established there was something to withhold - so every
    // Stop after the first nudge logged one, including turns with an empty
    // sidecar and nothing to say, plus a store write per turn. Both reviews
    // caught it: a number that measures silence must not count silence about
    // nothing.
    let already_nudged = stale_guard::already_blocked(&blocked, session_id);

    let stale_text = std::fs::read_to_string(absent_guard::default_stale_path(db_path)).ok()?;
    let stale = absent_guard::read_stale(&stale_text);
    if stale.is_empty() {
        return None;
    }

    let store = serve::store_dir::open_existing(db_path).ok()?;
    let mut settled_ids = std::collections::HashSet::new();
    for (id, record) in &stale {
        let events = store.get_events_by_entity(id).ok()?;
        let kinds: Vec<_> =
            events.into_iter().filter(|e| e.seq > record.seq_at_record).map(|e| e.kind).collect();
        if stale_guard::item_settled(&kinds) {
            settled_ids.insert(id.clone());
        }
    }

    let remaining = stale_guard::outstanding(&stale, &settled_ids);
    if remaining.is_empty() {
        return None;
    }

    // NOW it is true: there is outstanding rot, and this session has already
    // been told once. This is the one place left in the binary that has
    // something to say and chooses not to say it, which is correct for a
    // maintenance nudge - one that blocked every turn would be routed around
    // within a day - and it is the exact shape that hid a broken gate for the
    // whole of 2.0, so it is counted. Filed under the guard's own name rather
    // than a rule id, because what stands aside is the guard, not one rule.
    if already_nudged {
        record_gate(db_path, session_id, actor, false, "stale-guard", "stop", None);
        return None;
    }

    let reason = stale_guard::block_message(&remaining);
    if let Some(text) = stale_guard::mark_blocked_text(marker_text.as_deref(), session_id) {
        let _ = serve::store_dir::write(&marker_path, text);
    }

    Some(HookOutput::Decision(serde_json::json!({
        "decision": "block",
        "reason": reason,
    })))
}

// ---------------------------------------------------- the Remember moment

/// This memory's own "write a fact" tool names - mirror `mcp::Mcp::remember`
/// and `mcp::Mcp::revise` (`mcp/src/lib.rs`), the two `#[tool]` methods that
/// call `model::store::declare`/`model::store::revise`. Named constants,
/// never inline literals, because the crate dependency between the two only
/// runs ONE way (mcp depends on serve, never the reverse - `mcp/Cargo.toml`),
/// so the compiler has no way to catch either string drifting out of sync
/// with the method it names; a rename of either `#[tool]` fn in
/// `mcp/src/lib.rs` must be echoed here by hand.
const MCP_REMEMBER_TOOL: &str = "remember"; // mirrors mcp::Mcp::remember
const MCP_REVISE_TOOL: &str = "revise"; // mirrors mcp::Mcp::revise

/// The file or folder a Grep or Glob call searches, when it names one: those
/// two tools carry it in `path`, never in the `file_path` Read, Write and Edit
/// carry - so until this existed no search ever fired a rule anchored at the
/// place it searched (measured 2026-09-26: no shape of either did). Used ONLY
/// for the informing surface: it is never handed to a guard, so a search is
/// never refused, whatever is anchored there. `None` for every other tool, and
/// for a search that names no path (the session's own directory).
fn searched_path<'a>(tool_name: &str, tool_input: Option<&'a Value>) -> Option<&'a str> {
    if !matches!(tool_name, "Grep" | "Glob") {
        return None;
    }
    tool_input?.get("path")?.as_str().map(str::trim).filter(|p| !p.is_empty())
}

/// The tool-name half of a namespaced MCP tool call. Claude Code calls an MCP
/// tool as `mcp__<server-name>__<tool-name>` - the server-name half is
/// whatever the user's own Claude Code config happens to call the server
/// (already renamed once in this project's own history), so matching the
/// FULL string would silently stop working the next time it is renamed
/// again, and this binary has no way to keep a literal server name in sync
/// with a config file it never reads. Only the part after the LAST double
/// underscore is compared. A tool name with no double underscore at all (an
/// ordinary built-in tool - "Write", "Bash", ...) has nothing to strip, so
/// the whole name comes back unchanged; it will simply never equal
/// `MCP_REMEMBER_TOOL`/`MCP_REVISE_TOOL`.
fn mcp_tool_suffix(tool_name: &str) -> &str {
    tool_name.rsplit_once("__").map_or(tool_name, |(_, suffix)| suffix)
}

/// Whether this tool call is this memory's own `remember` or `revise` - the
/// moment of WRITING or CORRECTING a fact. Nothing else derives
/// `intent::Action::Remember` (see `intent`'s own closed vocabulary doc
/// comment - `from_command`/`from_path`/`from_draft` none of them produce
/// it), so this is the ONLY place that moment can ever fire from.
fn is_remember_moment(tool_name: &str) -> bool {
    let suffix = mcp_tool_suffix(tool_name);
    suffix == MCP_REMEMBER_TOOL || suffix == MCP_REVISE_TOOL
}

#[cfg(test)]
mod decay_notice_tests {
    use super::*;
    use model::item::{Binding, Check, Item, Kind, TargetKind};

    /// A checkout the way `project::resolve_project` recognises one, holding
    /// one file, plus a store on disk beside it.
    fn checkout(name: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(name);
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join("guarded.rs"), "fn kept() {}\n").unwrap();
        (dir, root)
    }

    fn rule_with_check(store: &mut EventStore, id: &str, project: &str, literal: &str) {
        let item = Item {
            id: id.to_string(),
            kind: Kind::Rule,
            text: format!("guarded.rs still says {literal}"),
            bindings: vec![Binding::Target { kind: TargetKind::Path, value: "guarded.rs".to_string() }],
            severity: None,
            project: Some(project.to_string()),
            tags: vec![],
            expires: None,
            key: None,
            falsifier: Some("the file stops saying it".to_string()),
            check: Some(Check::Contains { path: "guarded.rs".to_string(), literal: literal.to_string() }),
        };
        model::store::declare(store, "t", "t", "t", &item).expect("fixture must store");
    }

    /// THE DEFECT THIS CLOSES, and it is a mechanism defect rather than a
    /// bug. This scan is the only thing that looks at EVERY fact of a
    /// project, including ones whose file nobody touched all session. It used
    /// to print a count and stop there, while the two Stop-time guards that
    /// actually make somebody act only ever saw files written during the
    /// turn. So the thing that could find rot could not ask, and the things
    /// that ask could not see it - which is why a session only worked on
    /// staleness when the owner raised it himself. Recording into the same
    /// sidecar `serve::stale_guard` already reads is what turns the finding
    /// into a question.
    #[test]
    fn a_proof_that_comes_out_false_is_handed_to_the_stop_guard() {
        let (dir, root) = checkout("Some-Project");
        let db = dir.path().join("thor.db");
        let mut store = EventStore::new(&db).unwrap();
        rule_with_check(&mut store, "gone-literal", "Some-Project", "fn vanished()");

        let notice = decay_notice(&store, &db, Some(&root));
        assert!(notice.is_some(), "a false proof must still produce the owner's line");

        let sidecar = serve::absent_guard::default_stale_path(&db);
        let text = std::fs::read_to_string(&sidecar).expect("the finding must be recorded, not just counted");
        assert!(text.contains("gone-literal"), "the item has to be nameable at Stop: {text}");
    }

    /// The other half: a proof that HOLDS says nothing at all. A scan that
    /// recorded every fact it looked at would hand the Stop guard a backlog
    /// of non-problems, and a guard that cries wolf gets bypassed.
    #[test]
    fn a_proof_that_holds_is_never_recorded() {
        let (dir, root) = checkout("Some-Project");
        let db = dir.path().join("thor.db");
        let mut store = EventStore::new(&db).unwrap();
        rule_with_check(&mut store, "still-true", "Some-Project", "fn kept()");

        assert!(decay_notice(&store, &db, Some(&root)).is_none(), "nothing is wrong, so nothing is said");
        assert!(
            !serve::absent_guard::default_stale_path(&db).exists(),
            "a healthy store must not grow a staleness sidecar out of nowhere"
        );
    }
}

#[cfg(test)]
mod setup_debt_tests {
    use super::*;
    use model::item::{Binding, Item, Kind};

    /// The same shape `ops::install::working_contract` seeds the real note
    /// as - not imported from there (`serve` sits under `ops` in the
    /// workspace graph, never the other way round), so this is written out
    /// by hand, but it has to be the identical text: it is what proves this
    /// specific text passes the gate with no literal for ground 11 to catch.
    fn setup_note() -> Item {
        Item {
            id: model::store::SETUP_NOTE_ID.to_string(),
            kind: Kind::Rule,
            text: "walk the owner through setup once, then retract this note".to_string(),
            bindings: vec![Binding::Always],
            severity: None,
            project: None,
            tags: vec!["working-contract".to_string()],
            expires: None,
            key: None,
            falsifier: Some("this note is still served after the owner already answered".to_string()),
            check: None,
        }
    }

    /// No project on purpose, not just a leftover default: a real first
    /// session writes this on a store that has never named one yet, and
    /// `model::gate::declare`'s own ground 21 carries an exemption for this
    /// exact id so that write still lands - see that ground's doc comment.
    /// A `Some("test-project")` here would prove nothing about the defect
    /// this fixture exists to close.
    fn owner_setup_answers() -> Item {
        Item {
            id: model::store::OWNER_SETUP_ANSWERS_ID.to_string(),
            kind: Kind::Report,
            text: "reply length: short. language: Dutch. lanes: work only.".to_string(),
            bindings: vec![],
            severity: None,
            project: None,
            tags: vec![],
            expires: None,
            key: None,
            falsifier: None,
            check: None,
        }
    }

    /// THE DEFECT THIS CLOSES: until now the only thing asking a first
    /// session to walk the owner through setup was the note's own prose.
    #[test]
    fn the_debt_appears_while_the_note_is_live() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let mut store = EventStore::new(&db).unwrap();
        model::store::declare(&mut store, "s", "l", "a", &setup_note()).unwrap();

        let reason = setup_debt(&store).expect("the note is live, so this must hold the turn");
        assert!(reason.contains("First session with a new owner"), "{reason}");
        assert!(reason.contains(model::store::OWNER_SETUP_ANSWERS_ID), "{reason}");
        assert!(reason.contains(model::store::SETUP_NOTE_ID), "{reason}");
        assert!(reason.to_lowercase().contains("does not want"), "{reason}");
    }

    /// Once the answers are on record and the note is retracted through the
    /// real gate (`model::store::retract`, refused otherwise - see
    /// `model::gate::retract`), this falls silent on its own: it only ever
    /// asks whether the note is still live.
    #[test]
    fn the_debt_is_silent_once_the_note_is_retracted() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let mut store = EventStore::new(&db).unwrap();
        model::store::declare(&mut store, "s", "l", "a", &setup_note()).unwrap();
        model::store::declare(&mut store, "s", "l", "a", &owner_setup_answers()).unwrap();
        model::store::retract(&mut store, "s", "l", "a", model::store::SETUP_NOTE_ID, "owner walked through setup", None)
            .expect("the gate must accept this retract once the answers are on record");

        assert!(setup_debt(&store).is_none(), "a retracted note must never hold the turn again");
    }

    /// An old store, or one where setup was long ago finished and the note
    /// was never even seeded into this particular install: nothing to ask.
    #[test]
    fn a_store_that_never_had_the_note_is_unaffected() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let store = EventStore::new(&db).unwrap();

        assert!(setup_debt(&store).is_none());
    }
}

#[cfg(test)]
mod judgement_debt_tests {
    use super::*;
    use model::item::{Binding, Item, Kind};

    /// A crowded pool to write into: MAX_ITEMS items of equal weight already
    /// claiming one moment, so the next arrival gets the note rather than a
    /// refusal (a refusal needs HEAVIER rivals - see `model::store::capacity`).
    fn crowd_a_moment(store: &mut EventStore, project: &str) {
        const DISTINCT: [&str; 5] = [
            "a webhook retry backs off before it gives up entirely",
            "the estimator rounds a quote up to whole cents",
            "a spool label carries the batch it came from",
            "the scheduler skips a printer that is on hold",
            "an invoice number never restarts inside a year",
        ];
        for i in 0..model::item::MAX_ITEMS {
            let item = Item {
                id: format!("holder-{i}"),
                kind: Kind::Rule,
                text: DISTINCT[i % DISTINCT.len()].to_string(),
                bindings: vec![Binding::Moment(intent::Action::Deploy)],
                severity: None,
                project: Some(project.to_string()),
                tags: vec![],
                expires: None,
                key: None,
                falsifier: Some(format!("holder {i} turns out not to matter")),
                check: None,
            };
            model::store::declare(store, "earlier", "earlier", "t", &item).expect("fixture must store");
        }
    }

    fn crowded_newcomer(id: &str, project: &str) -> Item {
        Item {
            id: id.to_string(),
            kind: Kind::Rule,
            text: "a shipment label is printed once and never reprinted silently".to_string(),
            bindings: vec![Binding::Moment(intent::Action::Deploy)],
            severity: None,
            project: Some(project.to_string()),
            tags: vec![],
            expires: None,
            key: None,
            falsifier: Some("a label is reprinted without anyone noticing".to_string()),
            check: None,
        }
    }

    /// A crowded PATH pool: `MAX_ITEMS` items of equal weight claiming one
    /// file, whose texts share no words with the path, so closeness separates
    /// none of them from each other.
    fn crowd_a_path(store: &mut EventStore, project: &str) {
        const DISTINCT: [&str; 5] = [
            "a webhook retry backs off before it gives up entirely",
            "the estimator rounds a quote up to whole cents",
            "a spool label carries the batch it came from",
            "the scheduler skips a printer that is on hold",
            "an invoice number never restarts inside a year",
        ];
        for i in 0..model::item::MAX_ITEMS {
            let item = Item {
                id: format!("pathholder-{i}"),
                kind: Kind::Rule,
                text: DISTINCT[i % DISTINCT.len()].to_string(),
                bindings: vec![Binding::Target {
                    kind: model::item::TargetKind::Path,
                    value: "server/app.js".to_string(),
                }],
                severity: None,
                project: Some(project.to_string()),
                tags: vec![],
                expires: None,
                key: None,
                falsifier: Some(format!("path holder {i} turns out not to matter")),
                check: None,
            };
            model::store::declare(store, "earlier", "earlier", "t", &item).expect("fixture must store");
        }
    }

    fn path_newcomer(id: &str, project: &str, text: &str) -> Item {
        Item {
            id: id.to_string(),
            kind: Kind::Rule,
            text: text.to_string(),
            bindings: vec![Binding::Target {
                kind: model::item::TargetKind::Path,
                value: "server/app.js".to_string(),
            }],
            severity: None,
            project: Some(project.to_string()),
            tags: vec![],
            expires: None,
            key: None,
            falsifier: Some("this newcomer turns out not to matter".to_string()),
            check: None,
        }
    }

    /// THE DEFECT THIS PREVENTS. `capacity` used to count only rivals of the
    /// same weight or heavier and call the pool full - correct as a
    /// deliberately pessimistic WRITE-time warning, wrong as the debt's
    /// verdict, because equal weight outranks nothing and closeness settles
    /// those ties at serve time. Measured 2026-08-13 on a fact that had just
    /// been folded INTO the shown four and was still asked about every turn,
    /// with the `crowded-on-purpose` tag as the only offered exit - so
    /// answering it honestly meant recording a decision that was false.
    ///
    /// WRITTEN DIRECTLY TO THE LOG, NOT THROUGH `declare`, since a crowding
    /// tightening made 2026-09: a fresh declare onto a Path anchor already
    /// holding `MAX_ITEMS` rivals is now refused outright regardless of
    /// weight (`model::store::full_reliable_anchor`), so this exact shape -
    /// a fifth equal-weight rival sharing one Path - can no longer arise from
    /// a normal write; `model::store::tests::
    /// a_fifth_equal_weight_fact_on_a_full_path_anchor_is_refused` is that
    /// proof. It can still arise in a store written before that rule existed,
    /// or synced from a client that predates it, and `crowding_debt` has to
    /// read whatever such a store actually holds correctly regardless of how
    /// it got there - which is the one thing this test is still about.
    #[test]
    fn a_crowded_estimate_settles_itself_when_the_item_really_reaches_the_block() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let mut store = EventStore::new(&db).unwrap();
        crowd_a_path(&mut store, "p");
        record_session_watermark(&db, "now");
        // Same weight and same pool, so the estimate still calls it full - but
        // its text shares words with the path, which is what wins the tie.
        let mine = path_newcomer("mine", "p", "the server app entry point stays free of route handlers");
        // Checked BEFORE `mine` exists anywhere in the store: `capacity` never
        // refuses a binding an item already stood on (see `full_reliable_
        // anchor`'s own doc comment), so asking after the direct append below
        // would find `mine`'s own live entry already holding this exact
        // binding and wrongly read as a harmless revise-in-place.
        assert!(
            matches!(model::store::capacity(&store, &mine), Ok(model::store::Capacity::Full(_))),
            "fixture sanity: the write-time estimate must still call this pool full"
        );
        let body = serde_json::to_string(&mine).unwrap();
        store
            .append_event(
                "legacy",
                "mine",
                "migration",
                thor_core::event_store::EventKind::FactCreated,
                "mine",
                None,
                &body,
            )
            .unwrap();

        assert!(
            crowding_debt(&store, &db, "now", None).is_none(),
            "an item the real ranker does show must not be asked about as though it were invisible"
        );
    }

    /// The new half: going forward, this exact shape is refused at the door
    /// instead of quietly landing and waiting for the Stop-hook to notice.
    #[test]
    fn a_fifth_equal_weight_path_write_is_now_refused_before_it_ever_reaches_the_debt() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let mut store = EventStore::new(&db).unwrap();
        crowd_a_path(&mut store, "p");
        record_session_watermark(&db, "now");
        let mine = path_newcomer("mine", "p", "the server app entry point stays free of route handlers");

        let err = model::store::declare(&mut store, "mcp", "mcp", "t", &mine)
            .expect_err("a fifth rival on a full Path anchor is refused, not merely noted");
        assert!(format!("{err}").contains("pathholder-0"), "{err}");
        assert!(
            crowding_debt(&store, &db, "now", None).is_none(),
            "the refused write never landed, so this session owes no debt for it"
        );
    }

    /// RETIRED 2026-09-17, AND HERE IS WHY THAT IS SAFE RATHER THAN A QUIET
    /// LOSS OF COVERAGE. This test used to stand for "an item that really
    /// cannot appear still holds the turn", proven with a DIRECTORY binding
    /// as the one fixture its own author could find for "the estimate calls
    /// it crowded and the real ranker agrees it is unreachable" - a plain
    /// crowded PATH pool does not do it (recency lets an equal-weight
    /// newcomer win, per `a_crowded_estimate_settles_itself_when_the_item_
    /// really_reaches_the_block` above), and the test's own doc comment said
    /// so: "a Dir binding reaches no automatic surface at all... so here the
    /// estimate and the ranker agree, and the debt is owed".
    ///
    /// That premise is the exact misconception Fix 1 (`Capacity::Broad`,
    /// 2026-09-17) closes: a directory anchor has reached the files inside
    /// it since 2026-08-19 (`normalize::target_matches`), so an all-directory
    /// item is not the unreachable case this test needed - it is BROAD, and
    /// `crowding_debt` now never asks `reaches_a_block` about one at all (see
    /// `Capacity::Broad`'s own doc comment for the measured case: `serve why
    /// --file` on a file under the exact same kind of anchor answered "1
    /// item(s) apply"). Keeping this test would mean asserting the bug.
    ///
    /// `a_directory_only_item_written_this_session_produces_no_crowding_debt`
    /// above proves the corrected behaviour for the identical fixture shape.
    /// No other construction for "estimate says Crowded, ranker says never
    /// reachable" is known to this suite; `reaches_a_block` stays exercised
    /// by every test around it that proves the estimate and the ranker CAN
    /// disagree the other way (a crowded estimate that the ranker does show).

    /// THE LAZINESS THIS REMOVES. The write response already said "this may
    /// well never be shown there". Across two real sessions that note was
    /// reported and then left alone, and the fact stayed invisible. A
    /// maintenance step that depends on remembering works exactly as often as
    /// people feel like it, which is the same defect the useful-mark had.
    #[test]
    fn a_fact_written_onto_a_full_place_holds_the_turn() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let mut store = EventStore::new(&db).unwrap();
        crowd_a_moment(&mut store, "p");
        // What SessionStart does: mark where the log stood when we began.
        record_session_watermark(&db, "now");
        model::store::declare(&mut store, "mcp", "mcp", "t", &crowded_newcomer("mine", "p")).unwrap();

        let asked = crowding_debt(&store, &db, "now", None).expect("a crowded write must hold the turn");
        assert!(asked.contains("mine"), "{asked}");
        assert!(asked.contains("FOLD"), "it must say what to do, not just that something is wrong: {asked}");
        assert!(asked.contains("never do is raise its severity"), "{asked}");
    }

    /// Measured 2026-10-01: the install ran `serve retest --all --apply` from
    /// inside a Claude session. Its revisions (actor and session id "retest")
    /// landed above that session's watermark, so every Stop named one of them
    /// as "stored this session". A write by THOR's own machine is never the
    /// session's debt, whoever ran the command.
    #[test]
    fn a_retest_write_onto_a_full_place_is_never_this_sessions_debt() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let mut store = EventStore::new(&db).unwrap();
        crowd_a_moment(&mut store, "p");
        record_session_watermark(&db, "now");
        for actor in model::store::RESERVED_ACTORS {
            let id = format!("retested-by-{actor}");
            let body = serde_json::to_string(&crowded_newcomer(&id, "p")).unwrap();
            store
                .append_event(
                    "retest",
                    "retest",
                    actor,
                    thor_core::event_store::EventKind::FactRevised,
                    &id,
                    None,
                    &body,
                )
                .unwrap();
        }
        assert!(
            crowding_debt(&store, &db, "now", None).is_none(),
            "a write by one of THOR's own actors is not this session's"
        );
        // Fixture sanity: the same item written by the session itself holds.
        let mut mine = crowded_newcomer("mine", "p");
        mine.text = "a quote never ships before the spool is counted twice".to_string();
        model::store::declare(&mut store, "now", "now", "t", &mine).unwrap();
        let asked = crowding_debt(&store, &db, "now", None).expect("a real session write must still hold");
        assert!(asked.contains("mine"), "{asked}");
        assert!(!asked.contains("retested"), "{asked}");
    }

    /// A write stamped with another real session's id is that session's, even
    /// though it landed above this session's watermark.
    #[test]
    fn another_sessions_stamped_write_is_not_this_sessions_debt() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let mut store = EventStore::new(&db).unwrap();
        crowd_a_moment(&mut store, "p");
        record_session_watermark(&db, "now");
        model::store::declare(&mut store, "other-session", "other-session", "mcp", &crowded_newcomer("theirs", "p")).unwrap();
        assert!(crowding_debt(&store, &db, "now", None).is_none(), "another session's write is not ours");
    }

    /// A write stamped with this session's own id still holds the turn.
    #[test]
    fn a_write_stamped_with_this_sessions_own_id_holds_the_turn() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let mut store = EventStore::new(&db).unwrap();
        crowd_a_moment(&mut store, "p");
        record_session_watermark(&db, "now");
        model::store::declare(&mut store, "now", "now", "mcp", &crowded_newcomer("mine", "p")).unwrap();
        assert!(crowding_debt(&store, &db, "now", None).is_some());
    }

    /// It asks about what YOU made, never about somebody else's backlog. A
    /// session that wrote nothing crowded ends silently.
    #[test]
    fn an_earlier_sessions_crowded_fact_is_never_this_sessions_debt() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let mut store = EventStore::new(&db).unwrap();
        crowd_a_moment(&mut store, "p");
        // Written BEFORE this session began - the watermark is taken after it.
        model::store::declare(&mut store, "mcp", "mcp", "t", &crowded_newcomer("theirs", "p")).unwrap();
        record_session_watermark(&db, "today");

        assert!(crowding_debt(&store, &db, "today", None).is_none(), "a fresh session inherits no older mess");
        assert!(
            crowding_debt(&store, &db, "never-started", None).is_none(),
            "a session whose start was never seen must stay silent, never inherit everything"
        );
    }

    /// IT CARRIES NO STATE, so it clears by construction. Retracting the fact
    /// is one of the three ways out, and nothing has to be told about it.
    #[test]
    fn retracting_the_crowded_fact_clears_the_debt_with_nothing_to_update() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let mut store = EventStore::new(&db).unwrap();
        crowd_a_moment(&mut store, "p");
        record_session_watermark(&db, "now");
        model::store::declare(&mut store, "mcp", "mcp", "t", &crowded_newcomer("mine", "p")).unwrap();
        assert!(crowding_debt(&store, &db, "now", None).is_some(), "fixture sanity");

        model::store::retract(&mut store, "mcp", "mcp", "t", "mine", "folded into the item that already said it", None)
            .unwrap();
        assert!(crowding_debt(&store, &db, "now", None).is_none(), "folding it away settles it, with no marker to update");
    }

    /// The other way out: move it somewhere with room. Same story - the debt
    /// is derived from the store as it stands, so re-anchoring settles it.
    #[test]
    fn re_anchoring_it_somewhere_with_room_clears_the_debt() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let mut store = EventStore::new(&db).unwrap();
        crowd_a_moment(&mut store, "p");
        record_session_watermark(&db, "now");
        let mine = crowded_newcomer("mine", "p");
        model::store::declare(&mut store, "mcp", "mcp", "t", &mine).unwrap();
        assert!(crowding_debt(&store, &db, "now", None).is_some(), "fixture sanity");

        let mut moved = mine.clone();
        moved.bindings = vec![Binding::Target {
            kind: model::item::TargetKind::Path,
            value: "server/lib/labels.js".to_string(),
        }];
        model::store::revise(&mut store, "mcp", "mcp", "t", &mine, &moved).unwrap();
        assert!(crowding_debt(&store, &db, "now", None).is_none(), "a place with room settles it");
    }

    /// The cross-session leak this closes, measured 2026-08-14: two Claude
    /// sessions share one store, every tool-server write is stamped the
    /// constant "mcp", so the second session's writes land above the first's
    /// watermark and read as the first's own. The item's PROJECT is the signal
    /// the id is not - a session working in one repo is nagged only about
    /// crowded facts in that repo, or global ones, never another repo's.
    #[test]
    fn a_crowded_fact_in_another_project_does_not_nag_this_session() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let mut store = EventStore::new(&db).unwrap();
        crowd_a_moment(&mut store, "p");
        record_session_watermark(&db, "now");
        model::store::declare(&mut store, "mcp", "mcp", "t", &crowded_newcomer("theirs", "p")).unwrap();

        // A session working in a DIFFERENT project hears nothing about it.
        assert!(
            crowding_debt(&store, &db, "now", Some("other-repo")).is_none(),
            "a fact scoped to another project was another session's write, not this one's"
        );
        // The session whose project it belongs to still gets nagged.
        assert!(
            crowding_debt(&store, &db, "now", Some("p")).is_some(),
            "the session actually in that project must still hear it"
        );
        // With no project to route by, nothing is filtered - the watermark stands alone.
        assert!(
            crowding_debt(&store, &db, "now", None).is_some(),
            "an unresolved project falls back to the old behaviour"
        );
    }

    /// A rule as it exists in a store written BEFORE the gate started asking:
    /// it names something concrete and carries no check. The gate refuses to
    /// create one now, which is the point of the gate - so the only honest way
    /// to test the backlog burn is to put a legacy row in directly, exactly as
    /// the migration did.
    fn legacy_unanswered(store: &mut EventStore, id: &str, text: &str) {
        let mut item = crowded_newcomer(id, "p");
        item.text = text.to_string();
        // Heavy: since the 2026-09-25 rebuild, the backlog burn only ever
        // owes a proof to the same population `serve::prove::teeth_box`
        // does (heavy, or a refusing-kind check) - see `teeth_debt`'s own
        // doc comment. `crowded_newcomer`'s own default (`severity: None`)
        // would make every fixture below `NotApplicable` and silently stop
        // testing anything.
        item.severity = Some(model::item::Severity::Irreversible);
        let body = serde_json::to_string(&item).unwrap();
        store
            .append_event("legacy", id, "migration", thor_core::event_store::EventKind::FactCreated, id, None, &body)
            .unwrap();
    }

    /// Same legacy bypass as `legacy_unanswered` above, with an explicit
    /// project (`None` for a global rule) instead of the fixed "p" - needed
    /// to prove the backlog burn's own project scoping without touching the
    /// write gate, which still refuses a NEW item shaped this way regardless
    /// of project (see `legacy_unanswered`'s own doc comment).
    fn legacy_unanswered_in_project(store: &mut EventStore, id: &str, text: &str, project: Option<&str>) {
        let mut item = crowded_newcomer(id, "unused");
        item.project = project.map(str::to_string);
        item.text = text.to_string();
        // See `legacy_unanswered`'s own comment just above: heavy, so Change
        // 2's applicability gate actually reaches this fixture.
        item.severity = Some(model::item::Severity::Irreversible);
        let body = serde_json::to_string(&item).unwrap();
        store
            .append_event("legacy", id, "migration", thor_core::event_store::EventKind::FactCreated, id, None, &body)
            .unwrap();
    }

    /// THE DEFECT THIS CLOSES: until `teeth_debt` took a project at all, a
    /// rule scoped to one checkout was named to every OTHER checkout's
    /// session too - the identical unanswerable question `judgement_debt`
    /// and `crowding_debt` were already fixed for on their own debts.
    #[test]
    fn the_burn_never_names_another_checkouts_own_project() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let mut store = EventStore::new(&db).unwrap();
        legacy_unanswered_in_project(
            &mut store,
            "acme-unarmed",
            "Draai npm audit fix --force nooit op deze repo",
            Some("acme"),
        );
        assert!(
            teeth_debt(&store, &db, Some("acme")).is_some(),
            "fixture sanity: it applies inside its own checkout"
        );
        assert!(
            teeth_debt(&store, &db, Some("thor")).is_none(),
            "a different checkout's project must never be asked about acme's own rule"
        );
        assert!(
            teeth_debt(&store, &db, None).is_none(),
            "a checkout that resolves to no project at all gets the global layer only"
        );
    }

    /// A global rule (no project of its own) still reaches every checkout,
    /// exactly as `project::applies_to` already promises every other real
    /// injection surface.
    #[test]
    fn the_burn_still_names_a_global_rule_from_any_checkout() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let mut store = EventStore::new(&db).unwrap();
        legacy_unanswered_in_project(
            &mut store,
            "global-unarmed",
            "Draai npm audit fix --force nooit op deze repo",
            None,
        );
        assert!(teeth_debt(&store, &db, Some("acme")).is_some(), "a global rule reaches acme's checkout");
        assert!(teeth_debt(&store, &db, Some("thor")).is_some(), "and thor's checkout too");
        assert!(teeth_debt(&store, &db, None).is_some(), "and a checkout with no project at all");
    }

    #[test]
    fn the_burn_takes_one_turn_per_session_not_every_turn() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        assert!(teeth_not_yet_asked_this_session(&db, "s1"), "a fresh session has not been asked");
        record_teeth_asked(&db, "s1");
        assert!(!teeth_not_yet_asked_this_session(&db, "s1"), "the same session must not be held again");
        assert!(teeth_not_yet_asked_this_session(&db, "s2"), "a different session gets its own turn");
    }

    /// A rule with no verdict is found by the sweep itself, with nobody
    /// pointing at it, and named with why it is owed (`settlement`'s own
    /// reason) and the one command that decides it (step 9).
    #[test]
    fn a_heavy_checkless_rule_is_found_without_anybody_pointing_at_it() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let mut store = EventStore::new(&db).unwrap();
        legacy_unanswered(&mut store, "names-a-flag", "Draai npm audit fix --force nooit op deze repo");

        let asked =
            teeth_debt(&store, &db, Some("p")).expect("a heavy checkless rule must be found by the sweep itself");
        assert!(asked.contains("names-a-flag"), "it must name which rule: {asked}");
        assert!(asked.contains("it has never been tried"), "and say why it is owed: {asked}");
        let shown = db.display().to_string().replace('\\', "/");
        let shown = if shown.contains(' ') { format!("\"{shown}\"") } else { shown };
        assert!(
            asked.contains(&format!("--db {shown} retest --id 'names-a-flag'")),
            "and the command that decides it, on this very store: {asked}"
        );
    }

    /// One rule per session is a burn that never finishes: 336 were waiting on
    /// 2026-08-14, and the answer to each is a single sentence. The batch is
    /// what turns a year into weeks. It must also say what it held back - five
    /// names with nothing after them read as the whole backlog.
    #[test]
    fn the_burn_asks_a_batch_and_says_what_it_held_back() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let mut store = EventStore::new(&db).unwrap();
        for i in 0..TEETH_DEBT_BATCH_MAX + 2 {
            legacy_unanswered(&mut store, &format!("unarmed-{i}"), &format!("Draai npm audit fix --force-{i} nooit"));
        }

        let asked = teeth_debt(&store, &db, Some("p")).expect("several rules are unanswered");
        for i in 0..TEETH_DEBT_BATCH_MAX {
            assert!(asked.contains(&format!("unarmed-{i}")), "the whole batch is named: {asked}");
        }
        assert!(
            !asked.contains(&format!("unarmed-{TEETH_DEBT_BATCH_MAX}")),
            "and it stops at the cap: {asked}"
        );
        assert!(asked.contains("2 more are waiting"), "silence about the rest would read as done: {asked}");
    }

    /// Step 8 of the two tracks: a reason tag settles NOTHING any more -
    /// thin or checkable, before or after a check was tried - because THOR's
    /// trial decides whether a rule can have a tooth. A thin reason is no
    /// longer refused at the write either (it is just a tag). The rule leaves
    /// the debt once the trial puts it on the note track.
    #[test]
    fn a_reason_settles_nothing_and_a_note_verdict_does() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let mut store = EventStore::new(&db).unwrap();
        legacy_unanswered(&mut store, "answer-me", "Draai npm audit fix --force nooit op deze repo");
        assert!(teeth_debt(&store, &db, Some("p")).is_some(), "fixture sanity");

        let item = model::store::show(&store, "answer-me").unwrap();
        let mut real = item.clone();
        real.tags.push(format!(
            "{}the same --force also appears in the legitimate lockfile repair run",
            model::store::NO_LITERAL_REASON_PREFIX
        ));
        model::store::revise(&mut store, "s", "l", "a", &item, &real).expect("a reason tag is just a tag now");
        let now = model::store::show(&store, "answer-me").unwrap();
        let mut tried = now.clone();
        tried.check = Some(model::item::Check::Forbidden { literals: vec!["npm audit fix --force".to_string()] });
        for body in [serde_json::to_string(&tried).unwrap(), serde_json::to_string(&now).unwrap()] {
            store
                .append_mutate_checked("legacy", "answer-me", "migration", thor_core::event_store::EventKind::FactRevised, "answer-me", None, &body)
                .unwrap();
        }
        let asked = teeth_debt(&store, &db, Some("p")).unwrap();
        assert!(asked.contains("answer-me"), "a reason, even after a try, settles nothing: {asked}");
        assert!(asked.contains("retest --id 'answer-me'"), "the way on is the trial: {asked}");

        let corpus = serve::legit::Corpus::load(&store);
        let retest = serve::teeth::retest(&store, Some(db.as_path()), "answer-me", &corpus).unwrap();
        assert!(matches!(retest.outcome, serve::teeth::Maintenance::Note { .. }), "{:?}", retest.outcome);
        serve::teeth::apply_as_retest(&mut store, &retest).unwrap();
        assert!(teeth_debt(&store, &db, Some("p")).is_none(), "a note verdict settles it");
    }
    /// Step 9: a refusal on record and an open point naming the rule settle
    /// NOTHING - not even a field refusal, verified from the session's own
    /// transcript (it only sets the "proven" label of a trial). The one
    /// thing that settles it is a verdict from THOR's trial.
    #[test]
    fn a_refusal_or_an_open_point_settles_nothing_and_a_verdict_does() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let mut store = EventStore::new(&db).unwrap();
        legacy_unanswered(&mut store, "arm-me", "Draai npm audit fix --force nooit op deze repo");
        let current = model::store::show(&store, "arm-me").unwrap();
        let mut armed = current.clone();
        armed.bindings = vec![model::item::Binding::Target {
            kind: model::item::TargetKind::Command,
            value: "npm audit fix".to_string(),
        }];
        armed.check = Some(model::item::Check::Forbidden { literals: vec!["--force".to_string()] });
        model::store::revise(&mut store, "s", "l", "a", &current, &armed).unwrap();
        let asked = teeth_debt(&store, &db, Some("p")).expect("a check with no verdict is owed");
        assert!(asked.contains("arm-me"), "{asked}");

        let transcripts = tempfile::tempdir().unwrap();
        serve::attest::fixture::field_refusal(
            &mut store,
            transcripts.path(),
            "0de7dc52-1b97-4b66-948f-61e4438043b2",
            "arm-me",
            "toolu_arm",
            "Bash",
            serde_json::json!({"command": "npm audit fix --force"}),
            "npm audit fix --force",
        );
        assert!(teeth_debt(&store, &db, Some("p")).is_some_and(|a| a.contains("arm-me")), "a field refusal settles nothing");

        let point = model::item::Item {
            id: "open-arm-me".to_string(),
            kind: model::item::Kind::Report,
            text: "arm-me: its --force check is wired and still has to refuse a real call".to_string(),
            bindings: vec![],
            severity: None,
            project: Some("p".to_string()),
            tags: vec![model::store::OPEN_POINT_TAG.to_string()],
            expires: None,
            key: None,
            falsifier: None,
            check: None,
        };
        model::store::declare(&mut store, "s", "l", "a", &point).unwrap();
        assert!(teeth_debt(&store, &db, Some("p")).is_some_and(|a| a.contains("arm-me")), "an open point settles nothing");

        let corpus = serve::legit::Corpus::load(&store);
        let retest = serve::teeth::retest(&store, Some(db.as_path()), "arm-me", &corpus).unwrap();
        assert!(matches!(retest.outcome, serve::teeth::Maintenance::Rule { proven: true, .. }), "{:?}", retest.outcome);
        serve::teeth::apply_as_retest(&mut store, &retest).unwrap();
        assert!(teeth_debt(&store, &db, Some("p")).is_none(), "the trial's verdict - proven by that refusal - settles it");
    }

    /// F9: a file check of a project with no registered working copy is
    /// held by that blocker - never named, because nothing the agent runs
    /// settles it - and owed again the moment the project is registered.
    #[test]
    fn a_blocked_rule_is_named_only_once_its_blocker_lifts() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let mut store = EventStore::new(&db).unwrap();
        let checkout = dir.path().join("gamma");
        std::fs::create_dir_all(checkout.join(".git")).unwrap();
        std::fs::write(checkout.join("NOTES.md"), "keep me\nplain line\n").unwrap();
        let mut item = crowded_newcomer("gamma-keeps", "gamma");
        item.text = "NOTES.md must keep the line `keep me`.".to_string();
        item.severity = Some(model::item::Severity::Irreversible);
        item.bindings = vec![model::item::Binding::Target { kind: model::item::TargetKind::Path, value: "NOTES.md".to_string() }];
        item.check = Some(model::item::Check::Contains { path: "NOTES.md".to_string(), literal: "keep me".to_string() });
        let body = serde_json::to_string(&item).unwrap();
        store
            .append_event("legacy", "gamma-keeps", "migration", thor_core::event_store::EventKind::FactCreated, "gamma-keeps", None, &body)
            .unwrap();
        assert!(teeth_debt(&store, &db, Some("gamma")).is_none(), "blocked: never named");
        // The installer's registration, as its own event lands on the log (a
        // raw append: THOR's actor token is built in the installer only).
        let record = model::item::Item {
            id: model::store::project_opened_id("gamma"),
            kind: model::item::Kind::Report,
            text: "Project gamma: a fixture record.".to_string(),
            bindings: Vec::new(),
            severity: None,
            project: Some("gamma".to_string()),
            tags: vec!["project-record".to_string()],
            expires: None,
            key: None,
            falsifier: None,
            check: None,
        };
        let body = model::store::canonical_body(&record).unwrap();
        let body = format!(
            "{},\"root\":{},\"root_at\":\"2026-09-29T00:00:00Z\"}}",
            &body[..body.len() - 1],
            serde_json::to_string(checkout.to_str().unwrap()).unwrap()
        );
        store
            .append_event("install", "install", model::store::INSTALLER_ACTOR, thor_core::event_store::EventKind::FactCreated, &record.id, None, &body)
            .unwrap();
        let asked = teeth_debt(&store, &db, Some("gamma")).expect("registered: owed again");
        assert!(asked.contains("gamma-keeps"), "{asked}");
    }

    /// THE DEFECT THIS CLOSES (H): sorted by id alone, every session was asked
    /// about the same five rules. The ones asked least often come first now.
    #[test]
    fn the_burn_rotates_so_the_next_session_is_asked_about_others() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let mut store = EventStore::new(&db).unwrap();
        for i in 0..TEETH_DEBT_BATCH_MAX * 2 {
            legacy_unanswered(&mut store, &format!("unarmed-{i:02}"), &format!("Draai npm audit fix --force-{i} nooit"));
        }
        let first = teeth_debt(&store, &db, Some("p")).unwrap();
        let second = teeth_debt(&store, &db, Some("p")).unwrap();
        for i in 0..TEETH_DEBT_BATCH_MAX {
            assert!(first.contains(&format!("'unarmed-{i:02}'")), "{first}");
            assert!(!second.contains(&format!("'unarmed-{i:02}'")), "the second ask must move on: {second}");
        }
        for i in TEETH_DEBT_BATCH_MAX..TEETH_DEBT_BATCH_MAX * 2 {
            assert!(second.contains(&format!("'unarmed-{i:02}'")), "{second}");
        }
    }

    /// D8 (step 9): a light rule with no check and no verdict is owed like
    /// any other - weight plays no part, so lowering a severity never takes
    /// a rule out of this debt. The trial's note settles it.
    #[test]
    fn a_light_item_without_a_verdict_is_asked_until_the_trial_decides_it() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let mut store = EventStore::new(&db).unwrap();
        let mut item = crowded_newcomer("pure-judgement", "p");
        item.text = "Vraag eerst toestemming voor je iets onomkeerbaars doet".to_string();
        model::store::declare(&mut store, "s", "l", "a", &item).unwrap();
        assert!(teeth_debt(&store, &db, Some("p")).is_some_and(|a| a.contains("pure-judgement")), "light and owed");
        let corpus = serve::legit::Corpus::load(&store);
        let retest = serve::teeth::retest(&store, Some(db.as_path()), "pure-judgement", &corpus).unwrap();
        assert!(matches!(retest.outcome, serve::teeth::Maintenance::Note { .. }), "{:?}", retest.outcome);
        serve::teeth::apply_as_retest(&mut store, &retest).unwrap();
        assert!(teeth_debt(&store, &db, Some("p")).is_none(), "a note verdict settles it");
    }

    #[test]
    fn prose_that_merely_contains_a_slash_or_a_dot_is_not_a_path() {
        assert_eq!(model::gate::candidate_literal("werk dev->prod bij en/of herstart"), None);
        assert_eq!(model::gate::candidate_literal("dat kost 15-20 min. in totaal"), None);
        assert_eq!(
            model::gate::candidate_literal("wijzig files/deploy-watcher.sh en push"),
            Some("files/deploy-watcher.sh".to_string())
        );
    }

    /// REPLACES `a_rule_that_already_has_teeth_is_left_alone` (2026-09-25):
    /// that test's own fixture LOOKED armed (a `Forbidden` check) but its
    /// binding is `crowded_newcomer`'s own `Moment(Deploy)` - never a
    /// `Command` or `Always`, the only two shapes a `Forbidden` check can
    /// ever reach (`serve::prove::forbidden_reach`). A check that cannot be
    /// tried without a real checkout is asked again forever until anchored;
    /// this one can never be tried AT ALL, so leaving it alone was exactly
    /// the defect this rebuild closes - proven here by inverting the
    /// original assertion. `model::store::declare` still accepts this
    /// shape (the unreachable-check refusal lives at the MCP/`serve::prove`
    /// layer - see `mcp::teeth_test` - never inside the pure write gate),
    /// which is exactly how a rule like this can exist live in the store at
    /// all.
    #[test]
    fn a_check_that_cannot_reach_anything_is_named_again_not_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let mut store = EventStore::new(&db).unwrap();
        let mut item = crowded_newcomer("looks-armed-fires-nothing", "p");
        item.text = "Draai npm audit fix --force nooit op deze repo".to_string();
        item.check = Some(model::item::Check::Forbidden { literals: vec!["--force".to_string()] });
        model::store::declare(&mut store, "s", "l", "a", &item).unwrap();
        let asked = teeth_debt(&store, &db, Some("p"));
        assert!(asked.is_some(), "a check that can never reach anything must still be named");
        assert!(asked.unwrap().contains("looks-armed-fires-nothing"));
    }

    /// The THIRD way out, and until 2026-08-09 the only one the message
    /// promised without the code honouring it. Saying "it belongs here" in a
    /// reply settled nothing, so the debt re-fired every turn and the only
    /// escape left was deleting a true fact.
    #[test]
    fn judging_the_crowd_deserved_settles_the_debt_without_moving_anything() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let mut store = EventStore::new(&db).unwrap();
        crowd_a_moment(&mut store, "p");
        record_session_watermark(&db, "now");
        let mine = crowded_newcomer("mine", "p");
        model::store::declare(&mut store, "mcp", "mcp", "t", &mine).unwrap();
        assert!(crowding_debt(&store, &db, "now", None).is_some(), "fixture sanity");

        let mut judged = mine.clone();
        judged.tags.push(model::store::CROWDED_ON_PURPOSE_TAG.to_string());
        model::store::revise(&mut store, "mcp", "mcp", "t", &mine, &judged).unwrap();
        assert!(
            crowding_debt(&store, &db, "now", None).is_none(),
            "a recorded decision settles it, with the item exactly as crowded as before"
        );
    }

    /// THE DEFECT THIS PREVENTS, measured 2026-08-15 in a session that had just
    /// thinned 39 crowded facts: the tag is matched WHOLE, so writing it with a
    /// reason attached settles nothing and the debt asks again every turn. The
    /// worker had every reason to attach one - the neighbouring `no-literal`
    /// exit REQUIRES a reason - so the two exits read as one convention and are
    /// opposites. This is the trap; the next test is the message that names it.
    #[test]
    fn a_reason_appended_to_the_tag_does_not_settle_the_debt() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let mut store = EventStore::new(&db).unwrap();
        crowd_a_moment(&mut store, "p");
        record_session_watermark(&db, "now");
        let mine = crowded_newcomer("mine", "p");
        model::store::declare(&mut store, "mcp", "mcp", "t", &mine).unwrap();

        let mut judged = mine.clone();
        judged.tags.push(format!(
            "{}:de plek is eerlijk vol met zwaardere regels",
            model::store::CROWDED_ON_PURPOSE_TAG
        ));
        model::store::revise(&mut store, "mcp", "mcp", "t", &mine, &judged).unwrap();
        assert!(
            crowding_debt(&store, &db, "now", None).is_some(),
            "a reason attached to the tag is not the tag - the debt must keep asking, \
             which is exactly why the message has to say so"
        );
    }

    /// THE CONTRADICTION THIS CLOSES, reported by a session evaluation on
    /// 2026-08-16: the owner's standing requirement is zero crowding, while this
    /// message offered "sometimes the place is honestly full of heavier things"
    /// as an unconditional third exit. Both cannot be true, and a worker who
    /// follows the message ends up in breach of the requirement.
    ///
    /// The condition that makes exit 3 honest is narrow and now stated: seats
    /// held by rules that can each REFUSE a write. Those cannot be archived (the
    /// gate turns that down) and folding one would spend a check. A seat held by
    /// a description is the opposite case - it should be archived so the rule
    /// that can block takes the seat.
    #[test]
    fn leaving_it_is_offered_only_for_seats_held_by_rules_that_can_refuse() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let mut store = EventStore::new(&db).unwrap();
        crowd_a_moment(&mut store, "p");
        record_session_watermark(&db, "now");
        let mine = crowded_newcomer("mine", "p");
        model::store::declare(&mut store, "mcp", "mcp", "t", &mine).unwrap();

        let message = crowding_debt(&store, &db, "now", None).expect("fixture sanity");
        assert!(
            message.contains("REFUSE a write"),
            "exit 3 must name the condition that makes it honest"
        );
        assert!(
            message.contains("archive a description"),
            "and must say what to do when the seats are held by descriptions instead"
        );
        assert!(
            !message.contains("full of heavier things"),
            "the unconditional wording contradicted the owner's zero-crowding requirement"
        );
    }

    /// A trap the code cannot forgive has to be named where the worker reads it.
    /// The `no-literal` exit already states its own form; this one did not, and
    /// that asymmetry is what cost a session an hour.
    #[test]
    fn the_message_says_the_tag_counts_only_bare() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let mut store = EventStore::new(&db).unwrap();
        crowd_a_moment(&mut store, "p");
        record_session_watermark(&db, "now");
        let mine = crowded_newcomer("mine", "p");
        model::store::declare(&mut store, "mcp", "mcp", "t", &mine).unwrap();

        let message = crowding_debt(&store, &db, "now", None).expect("fixture sanity");
        assert!(message.contains("BARE"), "the message must say the tag takes no reason");
        assert!(
            message.contains("no-literal"),
            "and must name the neighbouring exit it is the opposite of"
        );
    }

    /// The tag settles the DEBT, not the crowding. An item that took exit 3 is
    /// still crowded out, and every count that measures reach must still say so
    /// - otherwise the escape hatch would quietly improve the numbers.
    #[test]
    fn taking_the_third_exit_does_not_pretend_the_place_has_room() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let mut store = EventStore::new(&db).unwrap();
        crowd_a_moment(&mut store, "p");
        let mut mine = crowded_newcomer("mine", "p");
        mine.tags.push(model::store::CROWDED_ON_PURPOSE_TAG.to_string());
        model::store::declare(&mut store, "mcp", "mcp", "t", &mine).unwrap();
        assert!(
            matches!(model::store::capacity(&store, &mine), Ok(model::store::Capacity::Crowded(_))),
            "the tag is a decision about the debt, never a claim that the place is free"
        );
    }

    /// THE DEFECT THIS PREVENTS (measured 2026-09-17, Printer-stuff session):
    /// `claude-app-sandbox-echte-schijf-via-wmi` and `nas-naar-prullenbak-
    /// via-rename-naar-recycle`, both bound to a directory only, were nagged
    /// at Stop although `serve why --file` on a file under the anchor
    /// answered "1 item(s) apply; the block would show 1" - the place was
    /// never full, the advisory was read as a displacement. A directory-only
    /// item's note is `Capacity::Broad`, not `Capacity::Crowded` (see that
    /// variant's own doc comment), and this debt only ever reacts to
    /// `Crowded` - so it must stay silent for one, however many files the
    /// directory reaches.
    #[test]
    fn a_directory_only_item_written_this_session_produces_no_crowding_debt() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let mut store = EventStore::new(&db).unwrap();
        record_session_watermark(&db, "now");
        let mut mine = crowded_newcomer("dir-only-mine", "p");
        mine.bindings = vec![Binding::Target { kind: TargetKind::Dir, value: "src/deep".to_string() }];
        model::store::declare(&mut store, "mcp", "mcp", "t", &mine).unwrap();

        assert!(
            crowding_debt(&store, &db, "now", None).is_none(),
            "a directory-only item's note is Broad, never Crowded, and must never be read as debt"
        );
    }


    fn declare(store: &mut EventStore, id: &str, pinned: bool) {
        let item = Item {
            id: id.to_string(),
            kind: Kind::Rule,
            text: format!("something worth knowing about {id}"),
            bindings: if pinned {
                vec![Binding::Always]
            } else {
                vec![Binding::Moment(intent::Action::Commit)]
            },
            severity: None,
            project: None,
            tags: vec![],
            expires: None,
            key: None,
            falsifier: Some("it stops being true".to_string()),
            check: None,
        };
        model::store::declare(store, "t", "t", "t", &item).expect("fixture must store");
    }

    fn serve_it(store: &mut EventStore, id: &str, times: usize) {
        serve_as(store, "s", id, times);
    }

    /// Served under `usefulness::ASSISTANT_DELIVERY_ACTOR` (2026-09-19):
    /// `judgement_debt` (below, via `usefulness::served_since_last_verdict`)
    /// now counts only that actor's own firings, so a fixture that claims to
    /// build real judgement debt has to serve under it.
    fn serve_as(store: &mut EventStore, session: &str, id: &str, times: usize) {
        for _ in 0..times {
            deliver::record_delivery(store, session, session, usefulness::ASSISTANT_DELIVERY_ACTOR, "2026-08-07T00:00:00Z", &[id.to_string()]);
        }
    }

    /// `declare`'s own project-scoped sibling: trigger-bound (never pinned -
    /// existing pin coverage lives on `declare` above), anchored to `project`
    /// exactly the way a real project-scoped rule is.
    fn declare_in_project(store: &mut EventStore, id: &str, project: &str) {
        let item = Item {
            id: id.to_string(),
            kind: Kind::Rule,
            text: format!("something worth knowing about {id}, only in {project}"),
            bindings: vec![Binding::Moment(intent::Action::Commit)],
            severity: None,
            project: Some(project.to_string()),
            tags: vec![],
            expires: None,
            key: None,
            falsifier: Some("it stops being true".to_string()),
            check: None,
        };
        model::store::declare(store, "t", "t", "t", &item).expect("fixture must store");
    }

    /// THE DEFECT THIS PREVENTS, and it bit within minutes of being possible.
    /// The served and judged counts are folded from event kinds, which keep
    /// every serving an item ever had - including the ones from before
    /// somebody retracted it. So a retracted item stayed owed forever, and
    /// the ask had no honest answer: "did it belong where it fired" cannot be
    /// answered about something that is gone. On 2026-08-08 a de-duplication
    /// retracted 57 items and the very next Stop asked about one of them by
    /// name, quoting an empty text because `show` refuses a tombstone.
    #[test]
    fn a_retracted_item_is_never_asked_about() {
        let mut store = EventStore::in_memory().unwrap();
        declare(&mut store, "gone", false);
        serve_it(&mut store, "gone", JUDGEMENT_DEBT_AFTER);
        assert!(judgement_debt(&store, Path::new("no-watermark-for-this-test"), "s", None).is_some(), "fixture sanity: it is owed while it is live");

        model::store::retract(&mut store, "t", "t", "t", "gone", "merged into another item", None).unwrap();
        assert!(
            judgement_debt(&store, Path::new("no-watermark-for-this-test"), "s", None).is_none(),
            "a retracted item must drop out of the debt, or it is owed forever with no way to settle it"
        );
    }

    /// THE DEFECT THIS PREVENTS: one answer settling an item for life. The
    /// debt used to hold a lifetime judged-set, so the first verdict about an
    /// item was also the last, however far the item drifted afterwards. That
    /// is the same shape `decay::is_stale` carried until 2026-08-08, and it
    /// bit hardest here, because the debt asks first about the items that
    /// fire most - the ones whose bindings are most worth a second look.
    ///
    /// A verdict now buys quiet, not immunity: the count resets, and another
    /// `JUDGEMENT_DEBT_AFTER` firings earn another question - but see the
    /// NEXT test for the part that is new since 2026-09-08: that second
    /// question is never put to the SAME session that just answered the
    /// first one, no matter how many more times it fires. This test proves
    /// the count-reset half from a session that did not itself hand down the
    /// earlier verdict, which is exactly the case the floor is not meant to
    /// touch.
    #[test]
    fn a_judged_item_is_asked_about_again_in_a_new_session_once_it_fires_that_many_times_more() {
        let mut store = EventStore::in_memory().unwrap();
        declare(&mut store, "drifter", false);
        serve_it(&mut store, "drifter", JUDGEMENT_DEBT_AFTER);
        assert!(judgement_debt(&store, Path::new("no-watermark-for-this-test"), "s", None).is_some(), "fixture sanity: it is owed");

        let verdicts = tempfile::tempdir().unwrap();
        // Through THOR's mark tool and verified at a Stop (step 12, D9): the
        // only kind of verdict that clears judgement debt.
        serve::attest::fixture::attested_mark(&mut store, verdicts.path(), serve::attest::fixture::SESSION, "drifter", false, "toolu_drifter");
        assert!(judgement_debt(&store, Path::new("no-watermark-for-this-test"), "s", None).is_none(), "a verdict must settle it for now");

        serve_as(&mut store, "s2", "drifter", JUDGEMENT_DEBT_AFTER - 1);
        assert!(judgement_debt(&store, Path::new("no-watermark-for-this-test"), "s2", None).is_none(), "one short of the threshold is not owed yet");

        serve_as(&mut store, "s2", "drifter", 1);
        let asked = judgement_debt(&store, Path::new("no-watermark-for-this-test"), "s2", None)
            .expect("forty more firings must earn a second question, from a session that did not itself just judge it");
        assert!(asked.contains("drifter"), "{asked}");
    }

    /// THE DEFECT THIS PREVENTS, reported by the owner and recorded as
    /// `oordeelschuld-komt-binnen-de-sessie-terug` (2026-09-08): measured in
    /// a real session, the Stop hook asked for a verdict on the same items
    /// three separate times, each only minutes after they had just been
    /// judged - four of eight items in one such round had already been
    /// marked useful earlier in that very session. The count-since-last-
    /// verdict fold (proven by the previous test) had no floor in time or in
    /// turns, so a busy session that keeps serving the same item crosses the
    /// threshold again within minutes of its own verdict.
    #[test]
    fn a_verdict_settles_an_item_for_the_rest_of_this_session_no_matter_how_many_more_times_it_fires() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let mut store = EventStore::new(&db).unwrap();
        // The SessionStart this session actually had, before anything else -
        // see `judgement_debt`'s own doc comment for why the floor reads
        // this watermark rather than a verdict's own `session_id` column.
        record_session_watermark(&db, "s");
        declare(&mut store, "settled-in-session", false);
        serve_it(&mut store, "settled-in-session", JUDGEMENT_DEBT_AFTER);
        assert!(judgement_debt(&store, &db, "s", None).is_some(), "fixture sanity: it is owed");

        let verdicts = tempfile::tempdir().unwrap();
        // Through THOR's mark tool and verified at a Stop (step 12, D9): the
        // only kind of verdict that clears judgement debt.
        serve::attest::fixture::attested_mark(&mut store, verdicts.path(), serve::attest::fixture::SESSION, "settled-in-session", false, "toolu_settled_in_session");
        assert!(judgement_debt(&store, &db, "s", None).is_none(), "a verdict must settle it for now");

        serve_it(&mut store, "settled-in-session", JUDGEMENT_DEBT_AFTER * 2);
        assert!(
            judgement_debt(&store, &db, "s", None).is_none(),
            "the SAME session must never be asked about it again this session, however many more times it fires"
        );
    }

    /// D9 (step 12): a verdict typed into a shell (`serve mark`, no stamp)
    /// after the session started is written, but neither resets the count nor
    /// silences this session's ask through the in-session floor.
    #[test]
    fn a_verdict_from_the_command_line_does_not_silence_the_session() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let mut store = EventStore::new(&db).unwrap();
        record_session_watermark(&db, "s");
        declare(&mut store, "shell-marked", false);
        serve_it(&mut store, "shell-marked", JUDGEMENT_DEBT_AFTER);
        serve::mark::record_useful(&mut store, "cli", "cli", "cli", "2026-08-08T00:00:00Z", "shell-marked", None).unwrap();
        let asked = judgement_debt(&store, &db, "s", None).expect("an unattested verdict settles nothing");
        assert!(asked.contains("shell-marked"), "{asked}");
    }

    /// The other verdict shape settles a session the same way - `judged_since`
    /// matches BOTH `ItemMarkedUseful` and `ItemMarkedNoise`, not just one of
    /// the two.
    #[test]
    fn a_noise_verdict_also_settles_an_item_for_the_rest_of_this_session() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let mut store = EventStore::new(&db).unwrap();
        record_session_watermark(&db, "s");
        declare(&mut store, "noised-in-session", false);
        serve_it(&mut store, "noised-in-session", JUDGEMENT_DEBT_AFTER);
        assert!(judgement_debt(&store, &db, "s", None).is_some(), "fixture sanity: it is owed");

        let verdicts = tempfile::tempdir().unwrap();
        // Through THOR's mark tool and verified at a Stop (step 12, D9): the
        // only kind of verdict that clears judgement debt.
        serve::attest::fixture::attested_mark(&mut store, verdicts.path(), serve::attest::fixture::SESSION, "noised-in-session", true, "toolu_noised_in_session");
        assert!(judgement_debt(&store, &db, "s", None).is_none(), "a noise verdict must also settle it for now");

        serve_it(&mut store, "noised-in-session", JUDGEMENT_DEBT_AFTER * 2);
        assert!(
            judgement_debt(&store, &db, "s", None).is_none(),
            "the SAME session must never be asked about it again this session, however many more times it fires"
        );
    }

    /// THE DEFECT THIS PREVENTS, and the direct cause of writing this fix:
    /// measured 2026-09-12, the named list under doctor's own judgement-debt
    /// line carried more than thirty `Always`-bound items, two of them
    /// pinned by the owner on purpose, and a whole evaluation was spent
    /// judging every one of them for nothing - `decay::DecayContext::
    /// is_stale` ignores a verdict on a pinned item outright, so nothing
    /// downstream of that verdict was ever going to change. See this
    /// function's own doc comment, "A PINNED ITEM OWES NO VERDICT", for the
    /// fuller history, including the earlier defect (`pinned_line`'s "44 of
    /// 48 never reviewed") the brief 2026-09-08 inclusion was itself fixing.
    #[test]
    fn a_never_judged_pinned_item_is_never_asked_about() {
        let mut store = EventStore::in_memory().unwrap();
        declare(&mut store, "pinned-one", true);
        serve_it(&mut store, "pinned-one", JUDGEMENT_DEBT_AFTER + 5);
        assert!(
            judgement_debt(&store, Path::new("no-watermark-for-this-test"), "s", None).is_none(),
            "a pinned item's own verdict changes nothing downstream, so asking for one is pure waste"
        );
    }

    /// THE OTHER HALF: a pin already served past the threshold stays silent
    /// however many more times it fires, and regardless of which session is
    /// asking - there is no session-scoped floor to prove here any more (a
    /// pin never enters `owed` at all to be floored), only that the
    /// exclusion holds no matter who is asking.
    #[test]
    fn a_heavily_served_pinned_item_stays_silent_across_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let mut store = EventStore::new(&db).unwrap();
        record_session_watermark(&db, "s");
        declare(&mut store, "pinned-two", true);
        serve_it(&mut store, "pinned-two", JUDGEMENT_DEBT_AFTER * 3);
        assert!(judgement_debt(&store, &db, "s", None).is_none(), "the session that saw it fire must not be asked");

        serve_as(&mut store, "s2", "pinned-two", JUDGEMENT_DEBT_AFTER);
        assert!(
            judgement_debt(&store, &db, "s2", None).is_none(),
            "a different session must not be asked either - pinned or not, a pin owes no verdict"
        );
    }

    /// A large pinned backlog produces NO ask at all - not merely a capped
    /// one. Before this fix, unpinning a batch of long-pinned items was the
    /// one way a backlog could arrive all at once (see
    /// `the_batch_is_capped_and_names_how_many_it_held_back` below, still
    /// true for a TRIGGER-bound backlog); a store with many long-served pins
    /// is no longer that shape at all, because none of them ever reach
    /// `owed` in the first place.
    #[test]
    fn a_large_pinned_backlog_produces_no_ask_at_all() {
        let mut store = EventStore::in_memory().unwrap();
        let n = JUDGEMENT_DEBT_BATCH_MAX + 3;
        for i in 0..n {
            let id = format!("pinned-owed-{i}");
            declare(&mut store, &id, true);
            serve_it(&mut store, &id, JUDGEMENT_DEBT_AFTER);
        }

        assert!(
            judgement_debt(&store, Path::new("no-watermark-for-this-test"), "s", None).is_none(),
            "an all-pinned backlog, however large, must never hold the turn"
        );
    }

    /// And the mechanism is still armed for what fires because a TRIGGER
    /// matched, which is the case where the question is real.
    #[test]
    fn a_trigger_bound_item_is_still_asked_about() {
        let mut store = EventStore::in_memory().unwrap();
        declare(&mut store, "on-a-moment", false);
        serve_it(&mut store, "on-a-moment", JUDGEMENT_DEBT_AFTER + 5);
        let reason = judgement_debt(&store, Path::new("no-watermark-for-this-test"), "s", None).expect("a trigger-bound item must still be asked about");
        assert!(reason.contains("on-a-moment"), "{reason}");
    }

    /// THE DEFECT THIS PREVENTS: asking this reader about something only
    /// SOMEBODY ELSE was ever served. A lifetime count says an item fired a
    /// hundred times; a rule scoped to another project fired all hundred of
    /// them there, and "did it belong where it fired" then has no form this
    /// session can answer. Found by being asked about an acme-shop commit
    /// rule during work in thor-2.
    #[test]
    fn an_item_this_session_never_saw_is_not_asked_about() {
        let mut store = EventStore::in_memory().unwrap();
        declare(&mut store, "elsewhere", false);
        serve_as(&mut store, "another-session", "elsewhere", JUDGEMENT_DEBT_AFTER + 5);
        assert!(
            judgement_debt(&store, Path::new("no-watermark-for-this-test"), "mine", None).is_none(),
            "this session never saw it fire, so it has nothing truthful to say about it"
        );
        assert!(
            judgement_debt(&store, Path::new("no-watermark-for-this-test"), "another-session", None).is_some(),
            "the session that DID see it is still asked"
        );
    }

    /// THE DEFECT THIS PREVENTS, measured against a copy of the live store on
    /// 2026-09-07/08: `seen` (above) proves this literal session_id served
    /// the item at SOME point in its own life, never that it did so while
    /// this turn's own project was the one in front of it. A session_id can
    /// legitimately carry a long history across many checkouts - this
    /// workspace's own "Orchestrator mode" is exactly that shape on purpose -
    /// so the SAME session that served this item while working in "acme"
    /// earlier must not be handed it back once it has moved on to "thor",
    /// mirroring the exact fix `crowding_debt` already got for its own
    /// project leak on 2026-08-14 ("a business-repo fact nagged a THOR-dev
    /// session twice").
    #[test]
    fn an_item_served_this_session_in_a_different_project_is_not_asked_about() {
        let mut store = EventStore::in_memory().unwrap();
        declare_in_project(&mut store, "acme-only", "acme");
        serve_it(&mut store, "acme-only", JUDGEMENT_DEBT_AFTER + 5);
        assert!(
            judgement_debt(&store, Path::new("no-watermark-for-this-test"), "s", Some("acme")).is_some(),
            "fixture sanity: the same session, still in the item's own project, is asked"
        );
        assert!(
            judgement_debt(&store, Path::new("no-watermark-for-this-test"), "s", Some("thor")).is_none(),
            "this session DID see it fire, but only while working on a different project - it \
             cannot honestly judge it now that it has moved on"
        );
    }

    /// The carve-out `project::applies_to` already gives every real injection
    /// surface: an item with NO project is global, and stays in scope
    /// regardless of which project this session currently resolves to.
    #[test]
    fn a_global_item_is_still_asked_about_regardless_of_the_current_project() {
        let mut store = EventStore::in_memory().unwrap();
        declare(&mut store, "global-rule", false);
        serve_it(&mut store, "global-rule", JUDGEMENT_DEBT_AFTER + 5);
        let asked = judgement_debt(&store, Path::new("no-watermark-for-this-test"), "s", Some("thor")).expect("a global item applies everywhere");
        assert!(asked.contains("global-rule"), "{asked}");
    }

    /// Sanity opposite of the mismatch test above: the item's own project and
    /// the session's CURRENT project agreeing must never itself be treated as
    /// suspicious - only a genuine mismatch withholds the ask.
    #[test]
    fn an_item_in_the_sessions_current_project_is_still_asked_about() {
        let mut store = EventStore::in_memory().unwrap();
        declare_in_project(&mut store, "thor-only", "thor");
        serve_it(&mut store, "thor-only", JUDGEMENT_DEBT_AFTER + 5);
        let asked = judgement_debt(&store, Path::new("no-watermark-for-this-test"), "s", Some("thor")).expect("same project as right now - still owed");
        assert!(asked.contains("thor-only"), "{asked}");
    }

    /// Below the threshold nothing is owed: this is about the handful of
    /// rules constantly in front of a reader, not about everything served.
    #[test]
    fn firing_a_few_times_owes_nothing() {
        let mut store = EventStore::in_memory().unwrap();
        declare(&mut store, "rarely", false);
        serve_it(&mut store, "rarely", JUDGEMENT_DEBT_AFTER - 1);
        assert!(judgement_debt(&store, Path::new("no-watermark-for-this-test"), "s", None).is_none());
    }

    /// THE OTHER HALF of "a session that saw nothing is not held": not merely
    /// under the threshold (`firing_a_few_times_owes_nothing`, above), but a
    /// session_id that never appears in the log AT ALL, against a store that
    /// genuinely owes plenty to OTHER sessions. `served_ids_in_session`
    /// returns an empty set for an id it has never seen, so `seen.contains`
    /// can never be true for anything - the same "owes nothing" answer, from
    /// the opposite direction (never having served anything at all, rather
    /// than not having served enough of any one thing).
    #[test]
    fn a_session_that_saw_nothing_at_all_is_not_held() {
        let mut store = EventStore::in_memory().unwrap();
        declare(&mut store, "owed-elsewhere-a", false);
        declare(&mut store, "owed-elsewhere-b", false);
        serve_as(&mut store, "busy-session", "owed-elsewhere-a", JUDGEMENT_DEBT_AFTER);
        serve_as(&mut store, "busy-session", "owed-elsewhere-b", JUDGEMENT_DEBT_AFTER + 20);
        assert!(
            judgement_debt(&store, Path::new("no-watermark-for-this-test"), "busy-session", None).is_some(),
            "fixture sanity: the store really does owe something, to the session that saw it"
        );
        assert!(
            judgement_debt(&store, Path::new("no-watermark-for-this-test"), "brand-new-session-saw-nothing", None).is_none(),
            "a session absent from the log entirely owes nothing, no matter how large the backlog"
        );
    }

    /// THE DEFECT THIS PREVENTS: unpinning a batch of long-pinned items (each
    /// carrying a large lifetime serving count from its time as `Always`)
    /// makes all of them owed in the same instant, and asking about them one
    /// per Stop turns a single backlog into that many separate blocked turns
    /// - each one repaying a long conversation's full history for a single
    /// id. One block must be able to carry more than one id.
    #[test]
    fn multiple_owed_items_are_all_asked_about_in_one_block() {
        let mut store = EventStore::in_memory().unwrap();
        declare(&mut store, "first", false);
        declare(&mut store, "second", false);
        serve_it(&mut store, "first", JUDGEMENT_DEBT_AFTER);
        serve_it(&mut store, "second", JUDGEMENT_DEBT_AFTER);

        let asked = judgement_debt(&store, Path::new("no-watermark-for-this-test"), "s", None).expect("two owed items must still produce one block");
        assert!(asked.contains("first"), "{asked}");
        assert!(asked.contains("second"), "{asked}");
    }

    /// THE DEFECT THIS PREVENTS (measured 2026-09-17): an evaluation judging
    /// "did it belong where it fired" could see an owed item's own text but
    /// not where it had actually been firing. This Stop-time ask names it
    /// too now, the same way `ops::health::judgement_debt_line`'s own named
    /// list does - trivial to add once `model::served::last_trigger` existed
    /// for that line to call.
    #[test]
    fn the_ask_names_where_the_owed_item_last_fired() {
        let mut store = EventStore::in_memory().unwrap();
        declare(&mut store, "triggered", false);
        serve_it(&mut store, "triggered", JUDGEMENT_DEBT_AFTER - 1);
        deliver::record_delivery_with_trigger(
            &mut store, "s", "s", usefulness::ASSISTANT_DELIVERY_ACTOR,
            "2026-08-07T00:00:00Z", &["triggered".to_string()], Some("npm run deploy"),
        );

        let asked = judgement_debt(&store, Path::new("no-watermark-for-this-test"), "s", None).expect("fixture sanity: it is owed");
        // Since step 24: this session's own places, newest first.
        assert!(asked.contains("time(s) in this session, last at 'npm run deploy'"), "{asked}");
    }

    /// A verdict on one of several owed items settles only that one - the
    /// others must still be owed and still asked about together, not
    /// silently cleared as a side effect of somebody else's answer.
    #[test]
    fn judging_one_of_several_leaves_the_rest_owed_together() {
        let mut store = EventStore::in_memory().unwrap();
        declare(&mut store, "answered", false);
        declare(&mut store, "still-owed-a", false);
        declare(&mut store, "still-owed-b", false);
        serve_it(&mut store, "answered", JUDGEMENT_DEBT_AFTER);
        serve_it(&mut store, "still-owed-a", JUDGEMENT_DEBT_AFTER);
        serve_it(&mut store, "still-owed-b", JUDGEMENT_DEBT_AFTER);

        let verdicts = tempfile::tempdir().unwrap();
        // Through THOR's mark tool and verified at a Stop (step 12, D9): the
        // only kind of verdict that clears judgement debt.
        serve::attest::fixture::attested_mark(&mut store, verdicts.path(), serve::attest::fixture::SESSION, "answered", false, "toolu_answered");

        let asked = judgement_debt(&store, Path::new("no-watermark-for-this-test"), "s", None).expect("two items are still owed");
        assert!(!asked.contains("answered"), "a settled item must not reappear: {asked}");
        assert!(asked.contains("still-owed-a"), "{asked}");
        assert!(asked.contains("still-owed-b"), "{asked}");
    }

    /// THE CAP MUST STAY A CAP, not become a new unbounded block under a
    /// different name - the whole point of batching was to stop paying a
    /// long conversation's full history once per owed id, not to risk one
    /// enormous block instead. Past the cap, it must say how many it held
    /// back rather than silently dropping them.
    #[test]
    fn the_batch_is_capped_and_names_how_many_it_held_back() {
        let mut store = EventStore::in_memory().unwrap();
        let n = JUDGEMENT_DEBT_BATCH_MAX + 3;
        for i in 0..n {
            let id = format!("owed-{i}");
            declare(&mut store, &id, false);
            serve_it(&mut store, &id, JUDGEMENT_DEBT_AFTER);
        }

        let asked = judgement_debt(&store, Path::new("no-watermark-for-this-test"), "s", None).expect("a large backlog is still owed");
        let shown = (0..n).filter(|i| asked.contains(&format!("owed-{i}"))).count();
        assert_eq!(shown, JUDGEMENT_DEBT_BATCH_MAX, "the block must show exactly the cap, not more: {asked}");
        assert!(asked.contains(&format!("{} more", n - JUDGEMENT_DEBT_BATCH_MAX)), "{asked}");
    }
}

#[cfg(test)]
mod evaluation_debt_tests {
    use super::*;
    use model::item::{Binding, Item, Kind};

    fn declare(store: &mut EventStore, id: &str, project: Option<&str>) {
        let item = Item {
            id: id.to_string(),
            kind: Kind::Rule,
            text: format!("something worth knowing about {id}"),
            bindings: vec![Binding::Moment(intent::Action::Commit)],
            severity: None,
            project: project.map(str::to_string),
            tags: vec![],
            expires: None,
            key: None,
            falsifier: Some("it stops being true".to_string()),
            check: None,
        };
        model::store::declare(store, "t", "t", "t", &item).expect("fixture must store");
    }

    fn declare_report(store: &mut EventStore, id: &str, project: &str) {
        let item = Item {
            id: id.to_string(),
            kind: Kind::Report,
            text: "fixture evaluation report".to_string(),
            bindings: vec![],
            severity: None,
            project: Some(project.to_string()),
            tags: vec!["evaluation-report".to_string()],
            expires: None,
            key: None,
            falsifier: None,
            check: None,
        };
        // Filed through THOR's remember tool, the way Claude Code files one
        // (step 15), and validated the way the next Stop validates it (step
        // 16) - by the shared fixture.
        let transcripts = tempfile::tempdir().unwrap();
        let session = serve::attest::fixture::SESSION;
        serve::eval_filing::fixture::valid_filing(store, transcripts.path(), session, &item, "toolu_fixture_report");
    }

    /// Served under `usefulness::ASSISTANT_DELIVERY_ACTOR` (2026-09-19), same
    /// reason as `judgement_debt_tests`'s own `serve_as`: `owe` below claims
    /// to build real judgement debt for the context line's own count, and
    /// only an assistant-attributed firing counts as one any more.
    fn serve_it(store: &mut EventStore, id: &str, times: usize) {
        for _ in 0..times {
            deliver::record_delivery(store, "s", "s", usefulness::ASSISTANT_DELIVERY_ACTOR, "2026-08-07T00:00:00Z", &[id.to_string()]);
        }
    }

    /// Declares `n` never-judged global items and serves each one enough
    /// times to sit in this checkout's own judgement debt, under the fixed
    /// fixture session "s" - never the real session id a test under this
    /// module uses (`serve_marker_minutes_ago` below), so these never also
    /// answer "has THIS session worked here". Only feeds the message's own
    /// "N item(s) currently owe a verdict here" context line now; no longer
    /// a condition for `evaluation_debt` to speak at all.
    fn owe(store: &mut EventStore, n: usize) {
        owe_project(store, n, None);
    }

    fn owe_project(store: &mut EventStore, n: usize, project: Option<&str>) {
        for i in 0..n {
            let id = format!("owed-{}-{i}", project.unwrap_or("global"));
            declare(store, &id, project);
            serve_it(store, &id, JUDGEMENT_DEBT_AFTER);
        }
    }

    /// A fresh store beside a real, throwaway db path - `evaluation_debt`
    /// reads its sidecar from disk (`usefulness::project_eval_state`), so
    /// `EventStore::in_memory()` (no path at all) cannot stand in for the
    /// fixture here.
    fn new_store() -> (tempfile::TempDir, std::path::PathBuf, EventStore) {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let store = EventStore::new(&db).unwrap();
        (dir, db, store)
    }

    /// Write the evaluation-debt sidecar directly, the same JSON shape
    /// `usefulness::update_eval_debt_state` itself writes - the only way to
    /// seed `last_evaluation_seen` (or `asked_count`/`first_asked_since_
    /// report`) at an exact, controllable instant, since `evaluation_debt`
    /// itself always reads the real wall clock (`time::now_unix`), never a
    /// fixture instant, and `update_eval_debt_state` only ever stamps `now`
    /// on a genuinely new report rather than an arbitrary past one. Mirrors
    /// `ops::health`'s own test-only `seed_eval_debt_state` helper.
    fn seed_eval_state(db: &Path, project: Option<&str>, state: usefulness::ProjectEvalState) {
        let mut all: usefulness::EvalDebtState = Default::default();
        all.insert(project.unwrap_or("").to_string(), state);
        std::fs::write(usefulness::eval_debt_state_path(db), serde_json::to_string(&all).unwrap()).unwrap();
    }

    /// An instant `usefulness::eval_report_covers` will always agree still
    /// covers, relative to the real "now" - replaces the retired
    /// `start_of_today_utc` (seventh rewrite, 2026-09-17: the UTC-calendar-
    /// day rule it measured against is gone, and reusing it under the new
    /// rolling window would be wrong at least once a day, whenever the test
    /// suite happened to run within `usefulness::EVAL_REPORT_COVERS_HOURS`
    /// of real UTC midnight). One hour ago is comfortably inside the window
    /// regardless of wall-clock time.
    fn recently_covered() -> i64 {
        time::now_unix() - 3600
    }

    /// The mirror of `recently_covered` above: an instant safely OUTSIDE
    /// `usefulness::EVAL_REPORT_COVERS_HOURS`, so a fixture can prove the
    /// "does not cover any more" branch regardless of wall-clock time.
    fn no_longer_covered() -> i64 {
        time::now_unix() - (usefulness::EVAL_REPORT_COVERS_HOURS + 1) * 3600
    }

    /// A `SessionWorkState` fixture carrying `minutes` worth of accrued work
    /// - `evaluation_debt` reads accrued seconds directly from this struct
    /// since the fifth rewrite (2026-09-17, `usefulness`'s own "evaluation
    /// debt" section), rather than measuring wall-clock elapsed time from a
    /// served item the way the retired `session_first_served_in_project`
    /// did, so a test only needs to build one of these directly instead of
    /// making real wall-clock time pass.
    fn work_minutes(minutes: i64) -> usefulness::SessionWorkState {
        usefulness::SessionWorkState {
            anchor_unix: Some(time::now_unix()),
            accrued_secs: minutes * 60,
            last_event_unix: Some(time::now_unix()),
            ..Default::default()
        }
    }

    /// The identical fixture as `work_minutes` above, plus the two risk
    /// counters the sixth rewrite (2026-09-17) added - see `usefulness`'s
    /// own "the repeat's own risk" doc comment. Needed by every "repeat
    /// fires" test below: accrued time alone is no longer enough.
    fn work_minutes_with_risk(minutes: i64, edits_since_test: u32, compacted_since_anchor: bool) -> usefulness::SessionWorkState {
        usefulness::SessionWorkState { edits_since_test, compacted_since_anchor, ..work_minutes(minutes) }
    }

    // --------------------------------------------------------- evaluation_debt
    //
    // REWRITTEN 2026-09-16 (fourth rewrite, `usefulness`'s own "evaluation
    // debt" section) from a rolling 24-hour window measured against
    // `tracking_since`/`last_evaluation_seen` to a UTC calendar day measured
    // against `last_evaluation_seen` alone - `seed_eval_state` writes that
    // field directly. REWRITTEN AGAIN 2026-09-17 (seventh rewrite): the UTC
    // calendar day gave way to a rolling `usefulness::EVAL_REPORT_COVERS_
    // HOURS`-hour window (`usefulness::eval_report_covers`); `recently_
    // covered`/`no_longer_covered` build a fixture instant safely inside or
    // outside that window, relative to the REAL wall clock this function
    // reads - simpler than the retired `start_of_today_utc`, which had to
    // dodge a real calendar-day boundary this rule no longer has.
    // REWRITTEN AGAIN 2026-09-17 (fifth rewrite): "how long has this session
    // worked here" moved from a store-derived timestamp
    // (`session_first_served_in_project`, retired) to `work_minutes` above,
    // a plain `usefulness::SessionWorkState` fixture handed straight to
    // `evaluation_debt` - the accrual mechanism itself (`accrue_session_
    // work`/`record_hook_event`) has its own, separate unit tests in
    // `usefulness::session_work_tests`; this module only proves what
    // `evaluation_debt` does once handed a particular accrued total.
    //
    // EVERY "FIRES" CASE BELOW IS SCOPED TO A REAL PROJECT (`Some("thor")`):
    // a checkout with no project can never file the Report that silences
    // this debt (`model::gate`'s ground 21, `NO_SCOPE_PROBLEM`), so it must
    // never be asked regardless of report history or time worked - see
    // `is_silent_for_a_checkout_with_no_project_regardless_of_report_
    // history_or_time_worked` and `the_identical_case_with_a_project_still_
    // fires` below for the corrected pair: the exact same fixture, once
    // proving `None` is always silent, once proving `Some("thor")` fires.

    #[test]
    fn fires_when_no_report_has_ever_been_seen_and_the_session_has_worked_here_long_enough() {
        let (_dir, db, mut store) = new_store();
        owe_project(&mut store, 12, Some("thor"));
        let asked =
            evaluation_debt(&store, &db, Some("thor"), &work_minutes(90)).expect("no report ever seen, enough time worked");
        assert!(asked.starts_with("[THOR]"), "{asked}");
        assert!(asked.contains("This project has had no evaluation in the last 16 hours"), "{asked}");
        assert!(asked.contains("This session has worked here for"), "{asked}");
        assert!(asked.contains("minute(s)"), "{asked}");
        assert!(asked.contains("12 item(s) currently owe a verdict here"), "{asked}");
        assert!(asked.contains("It has been asked 1 time(s)"), "{asked}");
        assert!(asked.contains("A turn cannot end until the evaluation report for this project is filed"), "{asked}");
        assert!(asked.contains("After that it is quiet for 16 hours"), "{asked}");
    }

    /// Step 14: the ask itself no longer carries a batch computed live - a
    /// list that could name a rule the evaluation is not held to. The start
    /// note that rides with it says what THOR decided itself at the start
    /// and what is left for the session, from the start record alone.
    #[test]
    fn the_ask_carries_no_live_batch_and_the_start_note_says_what_the_machine_decided() {
        let (_dir, db, mut store) = new_store();
        let owed = model::store::EVALUATION_TEETH_BATCH + 2;
        for i in 0..owed {
            let id = format!("heavy-{i:02}");
            let item = Item {
                id: id.clone(),
                kind: Kind::Rule,
                text: format!("a heavy fixture rule {id}"),
                bindings: vec![Binding::Moment(intent::Action::Commit)],
                severity: Some(model::item::Severity::Irreversible),
                project: Some("thor".to_string()),
                tags: vec![],
                expires: None,
                key: None,
                falsifier: Some("it stops being true".to_string()),
                check: None,
            };
            // Straight into the log: the write gate would ask this heavy rule
            // for a tooth or a reason, which is exactly what it still lacks.
            let body = serde_json::to_string(&item).unwrap();
            store.append_event("legacy", &id, "migration", thor_core::event_store::EventKind::FactCreated, &id, None, &body).unwrap();
            serve_it(&mut store, &id, 1);
        }
        let asked = evaluation_debt(&store, &db, Some("thor"), &work_minutes(90)).expect("the evaluation is owed");
        assert!(!asked.contains("This report's batch of rules") && !asked.contains("heavy-00"), "no live batch in the ask: {asked}");

        let started = serve::eval_session::start(&db, &mut store, "0de7dc52-1b97-4b66-948f-61e4438043b2", "thor", "stop-ask").unwrap();
        let note = serve::eval_session::start_note(&started.start);
        assert!(note.contains(&format!("THOR tried {owed} old rule(s) of this project itself")), "{note}");
        assert!(note.contains(&format!("{owed} became a note")) && note.contains("not reached this time: 0"), "{note}");
        assert!(note.contains("a. THE BATCH - no rule needs a fix from you"), "{note}");
        assert_eq!(teeth_debt(&store, &db, Some("thor")), None, "and the backlog burn has nothing left to ask");
    }

    /// Case named in the build brief: zero items owed must still fire once
    /// enough time has been worked - the item count is context in the
    /// message now, never a condition. Deliberately no `owe_project` call
    /// at all.
    #[test]
    fn fires_with_zero_items_owed_once_worked_long_enough_with_no_report() {
        let (_dir, db, store) = new_store();
        let asked =
            evaluation_debt(&store, &db, Some("thor"), &work_minutes(90)).expect("a zero-item backlog must still owe an evaluation");
        assert!(asked.contains("0 item(s) currently owe a verdict here"), "{asked}");
    }

    /// THE NO-PROJECT GUARD (`no evaluation is asked outside a project,
    /// where no report can be filed`), proven at the pure-function level: a
    /// real backlog, a report seen yesterday (never today), 90 minutes
    /// accrued this session - stays silent for `None`. A checkout with no
    /// project can never file the Report that would silence this (ground
    /// 21, `NO_SCOPE_PROBLEM`, in `model::gate`), so it must never be asked
    /// regardless of report history or how well-worked it looks.
    #[test]
    fn is_silent_for_a_checkout_with_no_project_regardless_of_report_history_or_time_worked() {
        let (_dir, db, mut store) = new_store();
        owe(&mut store, 12);
        seed_eval_state(&db, None, usefulness::ProjectEvalState {
            last_evaluation_seen: Some(no_longer_covered()),
            ..Default::default()
        });
        assert_eq!(
            evaluation_debt(&store, &db, None, &work_minutes(90)),
            None,
            "a checkout with no project must never be asked for the evaluation"
        );
    }

    /// THE CONTRAST: the identical fixture as the test just above, except
    /// scoped to a real project - and this one must still fire, proving the
    /// silence above is about the missing project specifically, not some
    /// other accidental difference between the two fixtures.
    #[test]
    fn the_identical_case_with_a_project_still_fires() {
        let (_dir, db, mut store) = new_store();
        owe_project(&mut store, 12, Some("thor"));
        seed_eval_state(&db, Some("thor"), usefulness::ProjectEvalState {
            last_evaluation_seen: Some(no_longer_covered()),
            ..Default::default()
        });
        assert!(
            evaluation_debt(&store, &db, Some("thor"), &work_minutes(90)).is_some(),
            "the identical case, scoped to a real project, must still fire"
        );
    }

    /// Case named in the build brief: less than an hour accrued -> silent,
    /// however long this project has gone without a report.
    #[test]
    fn silent_when_the_session_has_worked_here_less_than_an_hour() {
        let (_dir, db, mut store) = new_store();
        owe_project(&mut store, 12, Some("thor"));
        seed_eval_state(&db, Some("thor"), usefulness::ProjectEvalState {
            last_evaluation_seen: Some(no_longer_covered()),
            ..Default::default()
        });
        assert_eq!(evaluation_debt(&store, &db, Some("thor"), &work_minutes(30)), None);
    }

    /// Case named in the build brief: a report that still covers silences the
    /// FIRST obligation, however long the session has worked - as long as
    /// accrued work has not yet also crossed the REPEAT threshold (see the
    /// repeat-ask tests further below for that boundary).
    #[test]
    fn a_recently_seen_report_silences_it() {
        let (_dir, db, store) = new_store();
        seed_eval_state(&db, Some("thor"), usefulness::ProjectEvalState {
            last_evaluation_seen: Some(recently_covered()),
            ..Default::default()
        });
        assert_eq!(evaluation_debt(&store, &db, Some("thor"), &work_minutes(90)), None);
    }

    /// Case named in the build brief: a report seen more than
    /// `usefulness::EVAL_REPORT_COVERS_HOURS` ago no longer covers, and does
    /// not silence the obligation.
    #[test]
    fn a_report_outside_the_coverage_window_does_not_silence_it() {
        let (_dir, db, store) = new_store();
        seed_eval_state(&db, Some("thor"), usefulness::ProjectEvalState {
            last_evaluation_seen: Some(no_longer_covered()),
            ..Default::default()
        });
        assert!(
            evaluation_debt(&store, &db, Some("thor"), &work_minutes(90)).is_some(),
            "a report outside the coverage window must not buy silence"
        );
    }

    /// THE DELIBERATE BEHAVIOUR CHANGE from the retired `tracking_since`
    /// grace period: a project THOR has NEVER tracked before (no sidecar at
    /// all) now fires the very first time a session has worked here long
    /// enough, exactly like any other project with no report ever seen -
    /// `tracking_since` no longer gates anything (this file's own
    /// "evaluation debt" section, fourth rewrite), so there is no first-day
    /// grace period left to grant. Before this rewrite this fixture was
    /// silent (`tracking_since` of `None` read as "not yet stale").
    #[test]
    fn fires_with_no_sidecar_at_all_once_enough_time_is_worked() {
        let (_dir, db, store) = new_store();
        // Deliberately no `seed_eval_state` call: this is the very first
        // Stop this project has ever been seen at.
        assert!(
            evaluation_debt(&store, &db, Some("thor"), &work_minutes(90)).is_some(),
            "no report ever seen fires immediately once enough time is worked, with no grace period"
        );
    }

    /// Case named in the build brief: a session that has accrued no work at
    /// all yet is silent - `eval_debt_owed` reads zero minutes the same as
    /// "not yet an hour", never as a crash or a guess. Mirrors what a
    /// session's very first hook event in a project would actually hand
    /// `evaluation_debt` (`usefulness::accrue_session_work`'s own "the very
    /// first event is a reset to zero" behaviour).
    #[test]
    fn is_silent_for_a_session_with_no_accrued_work_yet() {
        let (_dir, db, store) = new_store();
        assert_eq!(evaluation_debt(&store, &db, Some("thor"), &work_minutes(0)), None);
    }

    /// The production detection path, not merely a hand-seeded sidecar: a
    /// report freshly declared in the store, then read through the REAL
    /// `update_eval_debt_state` (stamping `last_evaluation_seen` at the real
    /// "now") - the exact call the Stop hook itself makes before ever
    /// reaching `evaluation_debt` - must silence the very same turn.
    #[test]
    fn a_freshly_filed_report_silences_it_the_same_turn_via_a_real_stop() {
        let (_dir, db, mut store) = new_store();
        declare_report(&mut store, "eval-thor-2026-09-17", "thor");
        usefulness::update_eval_debt_state(&store, &db, Some("thor"), time::now_unix());
        assert_eq!(
            evaluation_debt(&store, &db, Some("thor"), &work_minutes(90)),
            None,
            "a report the real detection path just saw must silence this the same turn"
        );
    }

    #[test]
    fn a_different_projects_report_history_never_counts_toward_this_one() {
        let (_dir, db, store) = new_store();
        assert!(
            evaluation_debt(&store, &db, Some("acme"), &work_minutes(90)).is_some(),
            "fixture sanity: acme's own checkout sees it"
        );
        seed_eval_state(&db, Some("thor"), usefulness::ProjectEvalState {
            last_evaluation_seen: Some(recently_covered()),
            ..Default::default()
        });
        assert_eq!(
            evaluation_debt(&store, &db, Some("thor"), &work_minutes(90)),
            None,
            "thor's own report today must silence thor, regardless of what acme's own history looks like"
        );
    }

    /// Case named in the build brief: "the ask counter increments per
    /// blocked turn... and doctor shows it" - the message-building half:
    /// with two asks already on record, the THIRD ask (this one) must name
    /// itself as the third, not repeat "1".
    #[test]
    fn the_message_names_the_ask_count_including_this_ask_and_when_first_asked() {
        let (_dir, db, mut store) = new_store();
        owe_project(&mut store, 3, Some("thor"));
        seed_eval_state(&db, Some("thor"), usefulness::ProjectEvalState {
            asked_count: 2,
            first_asked_since_report: Some(recently_covered()),
            ..Default::default()
        });
        let asked = evaluation_debt(&store, &db, Some("thor"), &work_minutes(90)).expect("still owed, must speak");
        assert!(asked.contains("It has been asked 3 time(s)"), "{asked}");
    }

    // ------------------------------------------------- evaluation_debt: repeat
    //
    // Added 2026-09-17 (fifth rewrite): once a report already exists for
    // today, `evaluation_debt` asks a DIFFERENT question (accrued work since
    // that report reaching `EVAL_REPEAT_WORK_MINUTES`) and says something
    // different too (hours since the report's own id, never an ask count).

    #[test]
    fn a_report_seen_today_with_90_minutes_accrued_since_stays_silent() {
        // Case named in the build brief: well past the FIRST threshold, but
        // nowhere near the REPEAT one - a report already seen today must
        // not read the first threshold at all any more.
        let (_dir, db, store) = new_store();
        seed_eval_state(&db, Some("thor"), usefulness::ProjectEvalState {
            last_evaluation_seen: Some(recently_covered()),
            last_evaluation_report_id: Some("eval-thor-2026-09-17".to_string()),
            ..Default::default()
        });
        assert_eq!(evaluation_debt(&store, &db, Some("thor"), &work_minutes(90)), None);
    }

    #[test]
    fn a_report_seen_today_with_three_hours_accrued_since_fires_the_repeat_message() {
        let (_dir, db, mut store) = new_store();
        owe_project(&mut store, 5, Some("thor"));
        seed_eval_state(&db, Some("thor"), usefulness::ProjectEvalState {
            last_evaluation_seen: Some(recently_covered()),
            last_evaluation_report_id: Some("eval-thor-2026-09-17".to_string()),
            ..Default::default()
        });
        let work = work_minutes_with_risk(usefulness::EVAL_REPEAT_WORK_MINUTES, usefulness::EVAL_REPEAT_MIN_UNTESTED_EDITS, false);
        let asked = evaluation_debt(&store, &db, Some("thor"), &work).expect("three hours plus a risk must fire a repeat ask");
        assert!(asked.starts_with("[THOR]"), "{asked}");
        assert!(asked.contains("This session has worked here for 3.0 hour(s) since the last evaluation report"), "{asked}");
        assert!(asked.contains("eval-thor-2026-09-17"), "must name the report this accrual is measured since: {asked}");
        assert!(
            asked.contains(&format!("{} code change(s) since the last test or build run", usefulness::EVAL_REPEAT_MIN_UNTESTED_EDITS)),
            "must name the risk that triggered it: {asked}"
        );
        assert!(
            asked.contains("A new evaluation is due, covering only what happened since then plus the state of the work"),
            "{asked}"
        );
        assert!(asked.contains("5 item(s) currently owe a verdict here"), "{asked}");
        assert!(asked.contains("A turn cannot end until the evaluation report for this project is filed"), "{asked}");
        assert!(
            !asked.contains("It has been asked"),
            "the repeat message names hours and the report id, never an ask count: {asked}"
        );
    }

    /// THE CENTRAL CASE THE SIXTH REWRITE EXISTS FOR (2026-09-17, owner's
    /// decision): the identical fixture as the test just above, MINUS any
    /// risk - three hours accrued, a report already seen today, a real
    /// backlog - must now stay silent. Time alone is no longer enough.
    #[test]
    fn three_hours_accrued_with_no_risk_at_all_stays_silent() {
        let (_dir, db, mut store) = new_store();
        owe_project(&mut store, 5, Some("thor"));
        seed_eval_state(&db, Some("thor"), usefulness::ProjectEvalState {
            last_evaluation_seen: Some(recently_covered()),
            last_evaluation_report_id: Some("eval-thor-2026-09-17".to_string()),
            ..Default::default()
        });
        let work = work_minutes_with_risk(usefulness::EVAL_REPEAT_WORK_MINUTES, 0, false);
        assert_eq!(
            evaluation_debt(&store, &db, Some("thor"), &work),
            None,
            "three hours of accrued time with no code change and no summary must never fire a repeat"
        );
    }

    /// The other risk on its own: a context summary, with zero untested
    /// edits, must still fire - and the message must name the summary, not
    /// the edit count, as the reason.
    #[test]
    fn a_context_summary_with_zero_edits_fires_the_repeat_message_naming_the_summary() {
        let (_dir, db, mut store) = new_store();
        owe_project(&mut store, 5, Some("thor"));
        seed_eval_state(&db, Some("thor"), usefulness::ProjectEvalState {
            last_evaluation_seen: Some(recently_covered()),
            last_evaluation_report_id: Some("eval-thor-2026-09-17".to_string()),
            ..Default::default()
        });
        let work = work_minutes_with_risk(usefulness::EVAL_REPEAT_WORK_MINUTES, 0, true);
        let asked = evaluation_debt(&store, &db, Some("thor"), &work).expect("a context summary alone must fire a repeat ask");
        assert!(asked.contains("the context was summarized"), "{asked}");
        assert!(
            !asked.contains("code change(s) since the last test or build run"),
            "with zero edits, the edit-count reason must not appear: {asked}"
        );
    }

    /// Two untested edits - one short of the floor - with no summary either,
    /// must stay silent. Case named in the build brief.
    #[test]
    fn two_untested_edits_with_no_summary_stays_silent() {
        let (_dir, db, mut store) = new_store();
        owe_project(&mut store, 5, Some("thor"));
        seed_eval_state(&db, Some("thor"), usefulness::ProjectEvalState {
            last_evaluation_seen: Some(recently_covered()),
            last_evaluation_report_id: Some("eval-thor-2026-09-17".to_string()),
            ..Default::default()
        });
        let work = work_minutes_with_risk(usefulness::EVAL_REPEAT_WORK_MINUTES, usefulness::EVAL_REPEAT_MIN_UNTESTED_EDITS - 1, false);
        assert_eq!(evaluation_debt(&store, &db, Some("thor"), &work), None, "two untested edits is under the risk floor");
    }
}

#[cfg(test)]
mod remember_moment_tests {
    use super::*;

    /// THE DEFECT THIS PREVENTS: a namespaced call to this memory's own
    /// `remember` tool is not recognised at all, so `intent::Action::Remember`
    /// - which nothing else can ever derive - never fires on the one call it
    /// exists for.
    #[test]
    fn a_namespaced_remember_call_is_detected() {
        assert!(is_remember_moment("mcp__47d4a877-5022-48ae-b05f-dfbe635261fc__remember"));
    }

    /// The other tool name this same detection must cover.
    #[test]
    fn a_namespaced_revise_call_is_detected() {
        assert!(is_remember_moment("mcp__47d4a877-5022-48ae-b05f-dfbe635261fc__revise"));
    }

    /// THE DEFECT THIS PREVENTS: matching the WHOLE "mcp__<server>__<tool>"
    /// string, or hardcoding one particular server name, so a rename of the
    /// server in the user's own Claude Code config (already happened once in
    /// this project's own history) would silently stop detection working.
    /// Proven with two different server names, neither the one used above -
    /// this pins the SUFFIX match, not a literal.
    #[test]
    fn a_differently_named_server_is_still_detected_because_only_the_suffix_is_matched() {
        assert!(is_remember_moment("mcp__some-completely-different-server-name__remember"));
        assert!(is_remember_moment("mcp__thor2__revise"));
    }

    /// THE DEFECT THIS PREVENTS: the suffix match is so loose it fires on any
    /// tool at all - a built-in tool with no namespace, and a namespaced call
    /// to a DIFFERENT tool on this very server, must both come back false.
    #[test]
    fn an_unrelated_tool_produces_no_moment() {
        assert!(!is_remember_moment("Write"));
        assert!(!is_remember_moment("Bash"));
        assert!(!is_remember_moment("mcp__47d4a877-5022-48ae-b05f-dfbe635261fc__lookup"));
        assert!(!is_remember_moment("mcp__47d4a877-5022-48ae-b05f-dfbe635261fc__get"));
    }
}

/// The channel boundary itself: whatever `hook_once` returns or panics with,
/// this prints at most one JSON object and always exits 0. A silent panic
/// hook is installed first so a bug in the selection/render path cannot even
/// leak a Rust panic message - R5's "never speak on failure" applied at the
/// one place it has to hold regardless of what changes underneath it.
///
/// THE JUDGE REENTRANCY GUARD (see `serve::reentry`'s own doc comment and
/// `JUDGE-TRANSPORT.md`'s "Judge reentrancy" section for the whole hazard):
/// this is the FIRST thing `cmd_hook` does, before the panic hook, before
/// `hook_once`, before the store is ever opened, before ANY surface runs.
/// `serve::reentry::is_reentrant()` is true exactly when this process is a
/// descendant of a judge invocation (the child `claude` CLI process the
/// judge transport spawned, or anything THAT process in turn started - a
/// hook it fires, an MCP server it registers) - marked by
/// `judge::run_judge_command` via an inherited environment variable, never
/// by anything this process has to notice about its own call site. When
/// marked, this function does NOTHING AT ALL: no injection, no block, no
/// delivery event, no store write - it exits immediately, exactly as
/// successfully as a hook that simply had nothing to say. This is not an
/// error path and must never look like one; a marked invocation is the
/// EXPECTED shape for a hook firing inside the judge's own child session,
/// not a failure.
/// Everything a caller can check BEFORE the hook runs, as a pure answer
/// instead of silence: `Ok(())` means this run can look, `Err(line)` says why
/// it cannot in one sentence.
///
/// WHY THIS EXISTS (asked for 2026-09-19 by the first program that drives this
/// boundary itself). `hook` prints nothing and exits 0 on every failure,
/// deliberately: an assistant must never be broken by its own memory. But from
/// outside, that silence is identical to "no rule applies here", and a program
/// whose whole purpose is to stop a wrong action cannot tell those two apart.
/// Both cases this closes were found in real use: a payload that is not valid
/// JSON, and a store path that does not exist.
fn hook_preflight(raw: &str, db_path: &Path) -> Result<(), String> {
    if let Err(e) = serde_json::from_str::<Value>(raw) {
        return Err(format!(
            "could not look: the payload on stdin is not valid JSON ({e}). A Windows path written with single backslashes is the usual cause - use forward slashes in the payload."
 ));
 }
 if !db_path.exists() {
 return Err(format!("could not look: there is no memory at {}", db_path.display()));
 }
 if let Err(e) = serve::store_dir::open_existing(db_path) {
 return Err(format!("could not look: the memory at {} would not open ({e})", db_path.display()));
 }
 Ok(())
}

/// Whether `s` is an acceptable `--actor` label for `Command::Hook`: 1 to 32
/// characters, every one a lowercase ascii letter, digit, dash or underscore.
/// A pure function of a borrowed `&str` - no clap, no process exit - so the
/// exact rule can be unit tested directly (see `valid_actor_label_tests`
/// below) rather than only indirectly, by spawning the binary and reading
/// its exit code.
fn valid_actor_label(s: &str) -> bool {
    (1..=32).contains(&s.len())
        && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
}

/// The actor `serve hook` stamps its recorded events with: `--actor` when
/// given and acceptable, else `usefulness::ASSISTANT_DELIVERY_ACTOR`
/// ("hook"). `Err` is the one line printed before exiting 2.
///
/// A name reserved for THOR itself is refused with the store's own sentence
/// (`model::store::refuse_reserved_actor`): the events this hook records are
/// written raw, below the store's fence, so this is where that fence has to
/// stand for them. Its own default, `hook`, stays accepted - this IS the hook.
fn hook_actor(actor: Option<String>) -> Result<String, String> {
    match actor {
        None => Ok(usefulness::ASSISTANT_DELIVERY_ACTOR.to_string()),
        Some(a) if !valid_actor_label(&a) => Err(format!(
            "--actor must be 1 to 32 characters of lowercase ascii letters, digits, dash or underscore, got '{a}'"
        )),
        Some(a) if a == usefulness::ASSISTANT_DELIVERY_ACTOR => Ok(a),
        Some(a) => match model::store::refuse_reserved_actor(&a) {
            Ok(()) => Ok(a),
            Err(refusal) => Err(refusal.to_string()),
        },
    }
}

#[cfg(test)]
mod valid_actor_label_tests {
    use super::*;

    /// THE LEAK THIS CLOSES (D1): `serve hook --actor teeth-eval` stamped
    /// every event it recorded with a name only THOR may use. Refused now
    /// with the store's sentence; the hook's own default and an ordinary
    /// probe name still pass.
    #[test]
    fn a_reserved_actor_is_refused_before_the_hook_runs() {
        for name in ["teeth-eval", "eval-machine", "installer", "attester", "teeth-write"] {
            assert_eq!(
                hook_actor(Some(name.to_string())),
                Err(format!("the actor name {name} is reserved for THOR itself - write without --actor, or with your own name"))
            );
        }
        assert_eq!(hook_actor(Some("hook".to_string())), Ok("hook".to_string()));
        assert_eq!(hook_actor(None), Ok("hook".to_string()));
        assert_eq!(hook_actor(Some("probe".to_string())), Ok("probe".to_string()));
        assert!(hook_actor(Some("Hook".to_string())).unwrap_err().starts_with("--actor must be"));
    }

    #[test]
    fn an_empty_label_is_rejected() {
        assert!(!valid_actor_label(""));
    }

    #[test]
    fn a_33_character_label_is_rejected() {
        assert!(!valid_actor_label(&"a".repeat(33)));
    }

    #[test]
    fn a_32_character_label_is_accepted() {
        assert!(valid_actor_label(&"a".repeat(32)));
    }

    #[test]
    fn an_uppercase_letter_is_rejected() {
        assert!(!valid_actor_label("Hook"));
    }

    #[test]
    fn a_space_is_rejected() {
        assert!(!valid_actor_label("local model"));
    }

    #[test]
    fn a_slash_is_rejected() {
        assert!(!valid_actor_label("local/model"));
    }

    #[test]
    fn a_good_label_is_accepted() {
        assert!(valid_actor_label("local-model_v2"));
    }
}

#[cfg(test)]
mod hook_actor_and_no_record_tests {
    use super::*;
    use model::item::{Binding, Item, Kind};
    use thor_core::event_store::EventKind;

    /// One live, global Always-bound item, so `hook_once`'s `SessionStart`
    /// arm has something to show regardless of project resolution.
    fn seed_always_item(db_path: &Path, id: &str) {
        let mut store = EventStore::new(db_path).unwrap();
        let item = Item {
            id: id.to_string(),
            kind: Kind::Rule,
            text: "an always-bound rule for the hook actor tests".to_string(),
            bindings: vec![Binding::Always],
            severity: None,
            project: None,
            tags: vec![],
            expires: None,
            key: None,
            falsifier: Some("it stops being true".to_string()),
            check: None,
        };
        model::store::declare(&mut store, "seed", "seed", "seed", &item).unwrap();
    }

    /// A `cwd` inside `dir` and no `.git` above it anywhere resolves to no
    /// project at all (`project::resolve_project`), which keeps `decay_
    /// notice` a guaranteed no-op (`project::git_root` fails at its very
    /// first `?`) - so the only thing either run below can ever produce is
    /// the plain `HookOutput::Context` the Always item's own block renders
    /// to, never `ContextWithNotice`.
    fn session_start_payload(cwd: &Path, session_id: &str) -> String {
        serde_json::json!({
            "hook_event_name": "SessionStart",
            "session_id": session_id,
            "cwd": cwd.display().to_string(),
        })
        .to_string()
    }

    fn context_block(out: Option<HookOutput>) -> (String, String) {
        match out {
            Some(HookOutput::Context { event_name, block, .. }) => (event_name, block),
            other => panic!("expected a plain Context block, got {other:?}"),
        }
    }

    /// THE DEFECT THIS PREVENTS: the actor recorded on a delivery was the
    /// bare literal "hook" at every one of the three `deliver::record_
    /// delivery*` call sites - `--actor` has to actually reach the one this
    /// run's SessionStart delivery is stamped with, not just validate a flag
    /// nobody reads.
    #[test]
    fn the_actor_flag_is_stamped_on_the_delivery_instead_of_the_default() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        seed_always_item(&db, "always-1");

        let out = hook_once(&db, Some(session_start_payload(dir.path(), "s1")), "probe-actor", false);
        let (event_name, block) = context_block(out);
        assert_eq!(event_name, "SessionStart");
        assert!(block.contains("always-1") || !block.is_empty(), "fixture sanity: a block was rendered: {block}");

        let store = EventStore::open_existing(&db).unwrap();
        let recorded = store.event_kinds_with_actor().unwrap();
        let served: Vec<&str> = recorded
            .iter()
            .filter(|(kind, _, _)| *kind == EventKind::ItemServed)
            .map(|(_, _, actor)| actor.as_str())
            .collect();
        assert_eq!(served, vec!["probe-actor"], "the delivery must be stamped with the given actor, not the default");
    }

    /// THE DEFECT THIS PREVENTS: a measurement run has to be able to call
    /// this boundary hundreds of times with invented questions and leave NO
    /// trace - `--no-record` must write no `item_served` event at all, while
    /// producing byte-for-byte the same stdout as the identical payload
    /// without the flag.
    #[test]
    fn no_record_writes_no_delivery_but_produces_identical_output() {
        let dir = tempfile::tempdir().unwrap();

        let recorded_db = dir.path().join("recorded.db");
        seed_always_item(&recorded_db, "always-1");
        let recorded_out = hook_once(&recorded_db, Some(session_start_payload(dir.path(), "s1")), "hook", false);

        let unrecorded_db = dir.path().join("unrecorded.db");
        seed_always_item(&unrecorded_db, "always-1");
        let unrecorded_out = hook_once(&unrecorded_db, Some(session_start_payload(dir.path(), "s1")), "hook", true);

        assert_eq!(context_block(recorded_out), context_block(unrecorded_out), "output must be identical either way");

        let recorded_store = EventStore::open_existing(&recorded_db).unwrap();
        let recorded_served = recorded_store.event_kinds().unwrap().into_iter().filter(|(k, _)| *k == EventKind::ItemServed).count();
        assert_eq!(recorded_served, 1, "fixture sanity: a normal run does record one delivery");

        let unrecorded_store = EventStore::open_existing(&unrecorded_db).unwrap();
        let unrecorded_served =
            unrecorded_store.event_kinds().unwrap().into_iter().filter(|(k, _)| *k == EventKind::ItemServed).count();
        assert_eq!(unrecorded_served, 0, "--no-record must write no item_served event at all");
    }
}

/// The JSON `cmd_hook` writes to stdout for one `HookOutput` - split out of
/// `cmd_hook` itself so a test can inspect the exact value a payload
/// produces without going through the real process, which calls
/// `std::process::exit` unconditionally and so can never be driven from a
/// unit test directly. The match is exhaustive over every variant
/// `HookOutput` has today, so a future variant fails to compile here rather
/// than silently printing nothing.
///
/// `verdicts` (`Context`/`ContextWithNotice` only) is written as a TOP-LEVEL
/// sibling of `hookSpecificOutput`, never folded inside it and never mixed
/// into `additionalContext`/`block` itself - seeing both keys is how a
/// program that already calls this channel per action gets the same
/// prohibition/advice rows `check --json`/`why --json` document without a
/// second process, while `additionalContext` keeps carrying exactly the text
/// it always did for Claude Code, byte for byte (see `hook_verdicts_never_
/// change_the_additional_context_text` and `a_path_bound_forbidden_item_
/// reaches_the_hook_as_a_prohibition_without_touching_additional_context`
/// below for the proof). Every variant produces a value - unlike
/// `HookOutput` itself, which `hook_once` may not even return at all
/// (`Option<HookOutput>`), this function only ever runs once a caller
/// already has one in hand, so there is nothing left for it to say "nothing"
/// about; it returns `Value`, never `Option<Value>`.
fn hook_output_json(output: HookOutput) -> Value {
    match output {
        HookOutput::Context { event_name, block, verdicts } => serde_json::json!({
            "hookSpecificOutput": {
                "hookEventName": event_name,
                "additionalContext": block,
            },
            "verdicts": verdicts,
        }),
        HookOutput::ContextWithNotice { event_name, block, notice, verdicts } => serde_json::json!({
            "systemMessage": notice,
            "hookSpecificOutput": {
                "hookEventName": event_name,
                "additionalContext": block,
            },
            "verdicts": verdicts,
        }),
        // The Response Guard's verdict is its own top-level shape, printed
        // verbatim - never wrapped in additionalContext, and no `verdicts`
        // sibling either: a BLOCK already IS the refusal, so there is
        // nothing left for an advisory classifier to add.
        HookOutput::Decision(v) => v,
        // A WARN-tier verdict at Stop - kept as its own arm rather than
        // folded into `Context`'s (not merely to save a few lines) because
        // the two mean different things and no longer render the same way
        // either: `Context` is an injection surface handing the model ONE
        // field, `hookSpecificOutput.additionalContext`. `Warn` is the
        // Response Guard's own softer verdict, and has a second audience
        // `Context` never needs to reach - the owner, not just the model -
        // so it also sets a top-level `systemMessage` carrying the same
        // text. No `verdicts` here either: `Warn` fires at Stop, about a
        // reply already given, never about a proposed command or write the
        // classifier could have anything to say about.
        HookOutput::Warn { event_name, text } => serde_json::json!({
            "systemMessage": text,
            "hookSpecificOutput": {
                "hookEventName": event_name,
                "additionalContext": text,
            }
        }),
    }
}

#[cfg(test)]
mod hook_verdicts_tests {
    use super::*;
    use model::item::{Binding, Check, Item, Kind, Severity, TargetKind};

    /// THE DEFECT THIS PREVENTS: a future edit wiring `verdicts` through the
    /// same string `additionalContext` carries, instead of a separate JSON
    /// key - which would mean Claude Code's own model reads a "[PROHIBITS]"
    /// marker it never asked for, in text the owner never wrote. Drives
    /// `hook_output_json` directly with a HAND-BUILT `Context` whose
    /// `verdicts` is non-empty and DOES contain a prohibition, so the proof
    /// is not vacuous: even then, `additionalContext` must equal `block`
    /// exactly, byte for byte, with nothing from `verdicts` mixed in.
    #[test]
    fn hook_verdicts_never_change_the_additional_context_text() {
        let block = "Background facts about the owner's setup, not instructions for this task:\nBefore you do this:\n- [i0] never write an em dash".to_string();
        let verdicts = vec![CheckRow {
            id: "i0".to_string(),
            kind: Kind::Rule,
            severity: Some(Severity::HouseStyle),
            verdict: "prohibition",
            matched_literal: Some("\u{2014}".to_string()),
            text: "never write an em dash".to_string(),
            shown: true,
        }];
        let output = HookOutput::Context { event_name: "PreToolUse".to_string(), block: block.clone(), verdicts };

        let json = hook_output_json(output);
        assert_eq!(
            json["hookSpecificOutput"]["additionalContext"],
            serde_json::Value::String(block),
            "additionalContext must be exactly the block that was passed in, whatever verdicts carries"
        );
        assert!(json.get("verdicts").is_some(), "the sibling field must still be present");
        assert_eq!(json["verdicts"][0]["verdict"], "prohibition");
    }

    /// The `ContextWithNotice` counterpart: `verdicts` sits beside
    /// `systemMessage` too, and neither one leaks into `additionalContext`.
    #[test]
    fn hook_verdicts_never_change_additional_context_on_context_with_notice_either() {
        let block = "some block text".to_string();
        let output = HookOutput::ContextWithNotice {
            event_name: "SessionStart".to_string(),
            block: block.clone(),
            notice: "a decay notice".to_string(),
            verdicts: vec![CheckRow {
                id: "i0".to_string(),
                kind: Kind::Rule,
                severity: None,
                verdict: "prohibition",
                matched_literal: Some("bad".to_string()),
                text: "x".to_string(),
                shown: true,
            }],
        };
        let json = hook_output_json(output);
        assert_eq!(json["hookSpecificOutput"]["additionalContext"], serde_json::Value::String(block));
        assert_eq!(json["systemMessage"], "a decay notice");
        assert_eq!(json["verdicts"][0]["verdict"], "prohibition");
    }

    fn checkout(name: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(name);
        std::fs::create_dir_all(root.join(".git")).unwrap();
        (dir, root)
    }

    /// THE GAP THIS PROVES CLOSED. A `Check::Forbidden` item bound to a
    /// `Path` target is never enforced by any existing hard guard - the
    /// content guard's own `absent_literals` names only `Absent`/`AbsentAll`
    /// (see that function's own doc comment in `absent_guard.rs`), and the
    /// self-contained/command arms require an `Always`/`Command` binding
    /// respectively, neither of which this item carries. So this exact call
    /// reaches the informational Context branch untouched by any block -
    /// yet the new classifier must still call it a prohibition for a Write
    /// whose own content carries the literal, and the hook's own `verdicts`
    /// field must say so, all while `additionalContext` - the actual text
    /// the model reads - stays exactly what `render::render_text` alone
    /// already produces (proven directly above at the JSON-assembly level;
    /// this proves the same thing end to end, through the real store and
    /// the real `hook_once`, including the verdict computation that reads
    /// the SAME `served.selection.shown`/`input` the block was built from).
    #[test]
    fn a_path_bound_forbidden_item_reaches_the_hook_as_a_prohibition_without_touching_additional_context() {
        let (dir, root) = checkout("Some-Project");
        let db = dir.path().join("t.db");
        let mut store = EventStore::new(&db).unwrap();
        let item = Item {
            id: "no-todo-marker".to_string(),
            kind: Kind::Rule,
            text: "guarded.rs never carries a TODO-MARKER-XYZ".to_string(),
            bindings: vec![Binding::Target { kind: TargetKind::Path, value: "guarded.rs".to_string() }],
            severity: Some(Severity::HouseStyle),
            project: Some("Some-Project".to_string()),
            tags: vec![],
            expires: None,
            key: None,
            falsifier: Some("a TODO-MARKER-XYZ lands in guarded.rs and stays a week".to_string()),
            check: Some(Check::Forbidden { literals: vec!["TODO-MARKER-XYZ".to_string()] }),
        };
        model::store::declare(&mut store, "t", "t", "t", &item).expect("fixture must store");
        drop(store);

        let payload = serde_json::json!({
            "hook_event_name": "PreToolUse",
            "session_id": "s1",
            "cwd": root.display().to_string(),
            "tool_name": "Write",
            "tool_input": {"file_path": "guarded.rs", "content": "fn x() { /* TODO-MARKER-XYZ */ }"},
        })
        .to_string();

        let out = hook_once(&db, Some(payload), "hook", true);
        let (event_name, block, verdicts) = match out {
            Some(HookOutput::Context { event_name, block, verdicts }) => (event_name, block, verdicts),
            other => panic!("expected a plain Context block carrying the item above, got {other:?}"),
        };
        assert_eq!(event_name, "PreToolUse");

        // additionalContext must be exactly what render_text alone produces -
        // never anything the new verdicts computation added to it.
        assert!(block.contains("no-todo-marker"), "sanity: the item must actually be shown: {block}");
        assert!(!block.contains("PROHIBITS"), "additionalContext must carry no marker of its own: {block}");

        let row = verdicts.iter().find(|r| r.id == "no-todo-marker").expect("the item must appear in verdicts");
        assert_eq!(row.verdict, "prohibition");
        assert_eq!(row.matched_literal.as_deref(), Some("TODO-MARKER-XYZ"));
    }
}

fn cmd_hook(db_path: &Path, diagnose: bool, actor: Option<String>, no_record: bool) {
 // Validated HERE, before the reentry check below: a malformed `--actor`
 // is the caller's own mistake (a typo in whatever is invoking this),
 // never the silent fail-open R5 protects for a real hook payload, so it
 // is named on stderr in exactly one line and exits 2 regardless of
 // reentry or `--diagnose`. `None` (the flag was left off) is the default,
 // `usefulness::ASSISTANT_DELIVERY_ACTOR` ("hook"), byte for byte what
 // every call site below already wrote before this flag existed.
 let actor = match hook_actor(actor) {
 Ok(actor) => actor,
 Err(line) => {
 eprintln!("{line}");
 std::process::exit(2);
 }
 };
 if serve::reentry::is_reentrant() {
 if diagnose {
 eprintln!("did not look: this process is a hook inside a hook, which never runs the surfaces again");
 }
 std::process::exit(0);
 }
 // `--no-record` holds the store's directory for the whole call, the
 // diagnosing open below included - see `serve::store_dir`.
 let _hold = no_record.then(|| serve::store_dir::hold(db_path));
 // Read stdin HERE only for the diagnosing path, which has to inspect the
 // payload before the surfaces run; a normal run lets `hook_once` read it,
 // exactly as before.
 let payload_text = if diagnose {
 let mut raw = String::new();
 if std::io::stdin().read_to_string(&mut raw).is_err() {
 eprintln!("could not look: the payload on stdin could not be read");
 std::process::exit(2);
 }
 if let Err(line) = hook_preflight(&raw, db_path) {
 eprintln!("{line}");
 std::process::exit(2);
 }
 Some(raw)
 } else {
 None
 };
 std::panic::set_hook(Box::new(|_| {}));
 let db_path = db_path.to_path_buf();
 let result = std::panic::catch_unwind(move || hook_once(&db_path, payload_text, &actor, no_record));
 // Taken before the match below consumes `result`.
 let diagnosis: Option<(&str, i32)> = if diagnose {
 Some(match &result {
 Ok(Some(_)) => ("looked, and produced a block", 0),
 Ok(None) => ("looked, and nothing applies here", 0),
 Err(_) => ("could not look: the hook failed internally after starting, and wrote nothing", 2),
 })
 } else {
 None
 };
 // The per-variant shape (including the new `verdicts` sibling on
 // `Context`/`ContextWithNotice` - see that function's own doc comment)
 // now lives in `hook_output_json` alone, so a test can drive it
 // directly without going through this process-exiting function.
 if let Ok(Some(output)) = result {
 let out = hook_output_json(output);
 let _ = serde_json::to_writer(std::io::stdout(), &out);
 }
 if let Some((line, code)) = diagnosis {
 eprintln!("{line}");
 std::process::exit(code);
 }
 std::process::exit(0);
}

// -------------------------------------------------------- human-facing CLI

fn open_store_or_die(db_path: &Path) -> EventStore {
 match EventStore::new(db_path) {
 Ok(store) => store,
 Err(e) => {
 eprintln!("could not open store at {}: {e}", db_path.display());
 std::process::exit(1);
 }
 }
}

/// `render::render_text`'s own block, with one extra marker line spliced in
/// right after each shown item's bullet whose check classifies as a
/// prohibition for `command`/`content` - never touching `render_text`
/// itself, whose output the hook channel hands the model BYTE FOR BYTE (see
/// `hook_output_json`'s own doc comment and tests). `render_text` is the one
/// shared renderer for `check`, `hook` and `prompt` alike; marking inside it
/// would leak this work's own marker into the hook's `additionalContext`,
/// which must never change. So this walks the ALREADY-RENDERED block's own
/// lines instead and inserts a short, separate line - never rewriting or
/// removing one that was already there - for every id `classify_moment`
/// calls a prohibition. `check` and `hook` therefore agree on which items
/// are prohibitions only because both ultimately call the SAME classifier;
/// only `check`'s own prose is ever marked with it.
///
/// THEN, UNDERNEATH THE EXISTING BLOCK, UNCHANGED: one more line per
/// prohibition among `all` that is NOT in `shown` - the gap where a rule,
/// outranked at its own anchor by heavier items, never appeared in the
/// block, so anything reading only the block concluded nothing was wrong.
/// The block itself - every line the loop above
/// produces - is never touched by this second pass; it only ever appends
/// after it, one line per outranked prohibition, naming the rule, the
/// matched literal, and why it is missing from the block above.
fn mark_prohibitions(block: &str, all: &[RankedItem], shown: &[RankedItem], command: &str, content: Option<&str>) -> String {
    let verdicts = absent_guard::classify_moment(all, command, content);
    let mut lines: Vec<String> = Vec::new();
    for line in block.lines() {
        // `render_text`'s own bullet shape is `"- [{id}] {text}"` - the id
        // sits between the first `[` and the first `]` after it, whatever it
        // contains (a project-prefixed id carries its own `:`).
        let bullet_id = line.strip_prefix("- [").and_then(|rest| rest.split(']').next());
        lines.push(line.to_string());
        let Some(id) = bullet_id else { continue };
        let verdict = verdicts.iter().find(|(vid, _)| vid == id).map(|(_, v)| v);
        if let Some(absent_guard::Verdict::Prohibition { literal }) = verdict {
            lines.push(format!("  [PROHIBITS] matched \"{literal}\""));
        }
    }
    let shown_ids: std::collections::HashSet<&str> = shown.iter().map(|r| r.id.as_str()).collect();
    for (id, verdict) in &verdicts {
        if shown_ids.contains(id.as_str()) {
            continue; // already marked above, inside the block itself
        }
        let absent_guard::Verdict::Prohibition { literal } = verdict else { continue };
        let text = all.iter().find(|r| &r.id == id).map(|r| r.item.text.as_str()).unwrap_or("");
        lines.push(format!(
            "[NOT SHOWN] rule {id} (\"{text}\") also forbids this - matched \"{literal}\" - but is not \
             shown above because heavier items hold the block"
        ));
    }
    lines.join("\n")
}

fn cmd_check(db_path: &Path, input: &ServeInput, json: bool) {
    if input.is_empty() {
        if json {
            println!("[]");
        } else {
            println!("no intent detected - nothing would fire");
        }
        return;
    }
    let store = open_store_or_die(db_path);
    let served = serve::serve(&store, input);
    let command = input.command.as_deref().unwrap_or("");
    if json {
        let shown_ids: std::collections::HashSet<&str> =
            served.selection.shown.iter().map(|r| r.id.as_str()).collect();
        print_json_rows(&check_rows(&served.all, &shown_ids, command, None));
        return;
    }
    if input.moments.is_empty() {
        println!("detected: (no action; matching on target only)");
    } else {
        println!("detected: {}", input.moments.iter().map(Action::as_str).collect::<Vec<_>>().join(", "));
    }
    match render::render_text(&served.selection, input, db_path) {
        Some(block) => println!("\n{}", mark_prohibitions(&block, &served.all, &served.selection.shown, command, None)),
        None => println!("\n(no item governs this)"),
    }
}

/// One item's two-line description in `why`'s own prose form, plus - only
/// when `classify_item` calls it a prohibition for `command` - a short
/// third line marking it. Split out of `cmd_why` so a test can inspect the
/// exact lines it produces without capturing real stdout; every original
/// line stays exactly as it always read (see this file's own `mark_
/// prohibitions` for the identical guarantee on `check`'s side).
fn why_lines(all: &[RankedItem], shown_ids: &std::collections::HashSet<&str>, command: &str) -> Vec<String> {
    let mut lines = Vec::new();
    for ranked in all {
        let mark = if shown_ids.contains(ranked.id.as_str()) { " " } else { "-" };
        let severity = ranked.item.severity.map(|s| format!("{s:?}")).unwrap_or_else(|| "none".to_string());
        lines.push(format!("{mark} [{severity}] {}", ranked.item.text));
        lines.push(format!("    id {}   kind {:?}", ranked.id, ranked.item.kind));
        if let absent_guard::Verdict::Prohibition { literal } = absent_guard::classify_item(&ranked.item, command, None) {
            lines.push(format!("    [PROHIBITS] matched \"{literal}\""));
        }
    }
    lines
}

fn cmd_why(db_path: &Path, input: &ServeInput, json: bool) {
    let store = open_store_or_die(db_path);
    let served = serve::serve(&store, input);
    let command = input.command.as_deref().unwrap_or("");
    let shown_ids: std::collections::HashSet<&str> =
        served.selection.shown.iter().map(|r| r.id.as_str()).collect();
    if json {
        print_json_rows(&check_rows(&served.all, &shown_ids, command, None));
        return;
    }
    println!("{} item(s) apply; the block would show {}.\n", served.all.len(), served.selection.shown.len());
    for line in why_lines(&served.all, &shown_ids, command) {
        println!("{line}");
    }
}

#[cfg(test)]
mod prose_marking_tests {
    use super::*;
    use model::item::{Binding, Check, Item, Kind, Severity, TargetKind};

    fn ranked_command_item(id: &str, check: Option<Check>) -> RankedItem {
        RankedItem {
            id: id.to_string(),
            item: Item {
                id: id.to_string(),
                kind: Kind::Rule,
                text: format!("{id}: never carry the forbidden text"),
                bindings: vec![Binding::Target { kind: TargetKind::Command, value: "git push".to_string() }],
                severity: Some(Severity::HouseStyle),
                project: None,
                tags: vec![],
                expires: None,
                key: None,
                falsifier: Some(format!("{id} turns out fine anyway")),
                check,
            },
        }
    }

    // ------------------------------------------------- check's own marking

    /// THE DEFECT THIS PREVENTS: `render_text` is the one shared renderer
    /// for `check`, `hook` and `prompt` alike (see `mark_prohibitions`'s own
    /// doc comment) - marking INSIDE it would leak straight into the hook's
    /// own `additionalContext`, which must never change. `mark_prohibitions`
    /// must therefore only ever ADD a line, never touch one that was already
    /// there: every line `render_text` produced must still be found, whole,
    /// in the marked output, plus the new marker.
    #[test]
    fn marking_a_prohibited_item_keeps_every_original_line_and_adds_the_marker() {
        let shown = vec![
            ranked_command_item("p1", Some(Check::Forbidden { literals: vec!["--force".to_string()] })),
            ranked_command_item("a1", None),
        ];
        let selection = render::Selection { shown: shown.clone(), withheld: 0 };
        let input = ServeInput { command: Some("git push --force origin main".to_string()), ..Default::default() };
        let before = render::render_text(&selection, &input, Path::new("does-not-need-to-exist.db"))
            .expect("fixture must render a block");

        let after = mark_prohibitions(&before, &shown, &shown, "git push --force origin main", None);

        for line in before.lines() {
            assert!(after.contains(line), "an original line went missing: {line:?}\nafter:\n{after}");
        }
        assert!(after.contains("[PROHIBITS]"), "the prohibited item must be marked: {after}");
        assert!(after.contains("--force"), "the marker must name the matched literal: {after}");
        assert!(
            !after.lines().any(|l| l.contains("a1") && l.contains("PROHIBITS")),
            "the advice item must never be marked: {after}"
        );
    }

    /// An item whose check never matches this command stays unmarked -
    /// `mark_prohibitions` must add nothing at all when nothing classifies
    /// as a prohibition.
    #[test]
    fn a_block_with_no_prohibition_is_returned_unchanged() {
        let shown = vec![ranked_command_item("a1", None)];
        let selection = render::Selection { shown: shown.clone(), withheld: 0 };
        let input = ServeInput { command: Some("git status".to_string()), ..Default::default() };
        let before = render::render_text(&selection, &input, Path::new("does-not-need-to-exist.db")).unwrap();

        let after = mark_prohibitions(&before, &shown, &shown, "git status", None);
        assert_eq!(before, after, "nothing to mark means nothing added");
    }

    /// THE EXACT DEFECT THIS CLOSES: a rule that really would refuse the
    /// command being asked about, but lost its place in the shown block to
    /// heavier items, used to be invisible to `check`'s own prose - a
    /// reader of that prose was told nothing was wrong. Four
    /// heavier, unrelated items fill `MAX_ITEMS`; a fifth, lighter one
    /// carries a real `Forbidden` check matching the command but never makes
    /// the block. The existing block (the four heavier items' own bullets)
    /// must stay byte-for-byte what it always was; the fifth is named
    /// UNDERNEATH it, never inside it.
    #[test]
    fn a_prohibition_outranked_at_its_own_anchor_is_named_underneath_the_block_instead_of_staying_invisible() {
        let heavier: Vec<RankedItem> = (0..model::item::MAX_ITEMS)
            .map(|i| ranked_command_item(&format!("heavy{i}"), None))
            .collect();
        let outranked = ranked_command_item(
            "skip-hooks-rule",
            Some(Check::Forbidden { literals: vec!["--no-verify".to_string()] }),
        );
        let mut all = heavier.clone();
        all.push(outranked.clone());
        let selection = render::Selection { shown: heavier.clone(), withheld: 1 };
        // A command the fixtures' own anchor ("git push") reaches - see
        // `rows_stay_paired_with_the_right_item_across_several_at_once`.
        let command = "git push --no-verify origin main";
        let input = ServeInput { command: Some(command.to_string()), ..Default::default() };
        let before = render::render_text(&selection, &input, Path::new("does-not-need-to-exist.db"))
            .expect("fixture must render a block");
        assert!(
            !before.contains("skip-hooks-rule"),
            "sanity: the outranked rule must not already be in the shown block: {before}"
        );

        let after = mark_prohibitions(&before, &all, &heavier, command, None);

        for line in before.lines() {
            assert!(after.contains(line), "an original line went missing: {line:?}\nafter:\n{after}");
        }
        assert!(
            after.contains("NOT SHOWN") && after.contains("skip-hooks-rule"),
            "the outranked prohibition must be named, marked as not shown: {after}"
        );
        assert!(after.contains("--no-verify"), "the marker must name the matched literal: {after}");
        assert!(
            after.contains("heavier items hold the block"),
            "the marker must say WHY it is missing from the block above: {after}"
        );
    }

    // --------------------------------------------------- why's own marking

    /// The identical guarantee as `check`'s own marker, proven at the
    /// `why_lines` level instead: every original two-line description stays
    /// exactly as it read, and the marker is a THIRD, separate line, never a
    /// rewrite of either.
    #[test]
    fn why_lines_keep_the_original_two_lines_and_add_a_third_for_a_prohibition() {
        let items = vec![ranked_command_item("p1", Some(Check::Forbidden { literals: vec!["--force".to_string()] }))];
        let shown_ids: std::collections::HashSet<&str> = ["p1"].into_iter().collect();

        let lines = why_lines(&items, &shown_ids, "git push --force origin main");
        assert_eq!(lines.len(), 3, "two original lines plus one marker: {lines:?}");
        // "shown" (the fixture's own `shown_ids`) prints as a one-char mark
        // (a plain space) THEN the format string's own literal space before
        // "[" - two spaces total before the bracket, not one.
        assert_eq!(lines[0], "  [HouseStyle] p1: never carry the forbidden text");
        assert_eq!(lines[1], "    id p1   kind Rule");
        assert!(lines[2].contains("[PROHIBITS]") && lines[2].contains("--force"), "{}", lines[2]);
    }

    /// An item that is not a prohibition for this command produces exactly
    /// its two original lines, no third one.
    #[test]
    fn why_lines_add_nothing_for_advice() {
        let items = vec![ranked_command_item("a1", None)];
        let shown_ids: std::collections::HashSet<&str> = ["a1"].into_iter().collect();

        let lines = why_lines(&items, &shown_ids, "git push --force origin main");
        assert_eq!(lines.len(), 2, "{lines:?}");
    }
}

fn cmd_audit(db_path: &Path) {
    let store = open_store_or_die(db_path);
    let rows = serve::audit::audit_rows(&store);
    println!("{:<28} {:<12} {:<13} {:>5}  last served", "id", "kind", "severity", "count");
    for row in &rows {
        let severity = row.item.severity.map(|s| format!("{s:?}")).unwrap_or_else(|| "none".to_string());
        let last = row.stats.last_served_at.clone().unwrap_or_else(|| "never".to_string());
        println!(
            "{:<28} {:<12} {:<13} {:>5}  {}",
            row.id,
            format!("{:?}", row.item.kind),
            severity,
            row.stats.times_served,
            last
        );
    }
    let never = rows.iter().filter(|r| r.stats.times_served == 0).count();
    println!("\ndeclared, never delivered: {never} of {}", rows.len());
}

// --------------------------------------------------- surface 1: session start

/// Preview of surface 1: prints every Always item (global plus `project`),
/// full and never capped. Does not record a delivery - only `hook`'s
/// SessionStart branch does that (see `hook_once`'s own doc comment). Prints
/// the resolved project first, so a person can see what `project::
/// resolve_project` decided (marker file, git root, or neither) without
/// reading source.
fn cmd_session_start(db_path: &Path, project: Option<&str>) {
    let store = open_store_or_die(db_path);
    println!("project: {}", project.unwrap_or("(none - global only)"));
    let candidates = serve::live::always_candidates(&store);
    let decay = DecayContext::load(&store);
    let items = serve::decay::retain_live(session_start::select(&candidates, project), &decay);
    match session_start::render(&items) {
        Some(block) => println!("{block}"),
        None => println!("(no standing rules apply)"),
    }
}

// -------------------------------------------------------- surface 3: prompt

/// Preview of surface 3: prints what a raw prompt resolves to, or says so
/// plainly when it resolves to nothing. Does not record a delivery - only
/// `hook`'s UserPromptSubmit branch does that.
fn cmd_prompt(db_path: &Path, text: &str) {
    let store = open_store_or_die(db_path);
    let candidates = serve::live::live_items(&store);
    let input = prompt::resolve(text, &candidates);
    if input.is_empty() {
        println!("nothing resolves - no moment and no declared target was named");
        return;
    }
    let decay = DecayContext::load(&store);
    let all = serve::decay::retain_live(serve::rank::select(&candidates, &input), &decay);
    let selection = render::cap(all);
    match render::render_text(&selection, &input, db_path) {
        Some(block) => println!("{block}"),
        None => println!("(no item governs this)"),
    }
}

// ------------------------------------------------------- surface 4: lookup

/// Surface 4: free-text search over every live item, any project, archive
/// kinds (Report/Chunk) included - never an injection surface, so there is
/// no delivery to record here at all. Calls `search_best_effort`, which
/// silently degrades to plain text match without the `semantic` feature, or
/// with it but no usable model/sidecar - so this one call site is correct in
/// every build without an `#[cfg]` of its own.
/// `cmd_search`'s reply body, kept separate from the `println!` calls so a
/// unit test can check it directly. This workspace's usual way to prove a
/// CLI end to end is spawning the real compiled binary (`serve/tests/*.rs`,
/// using `env!("CARGO_BIN_EXE_serve")`), but that mechanism only works from
/// a DIFFERENT target in the same package (an integration test, an example,
/// another bin) - never from the `serve` bin's own unit tests, which is all
/// this file may add to (see this file's own top-of-file doc comment on the
/// four channels this binary owns). So this returns a plain `String`, the
/// same shape the agent's own `lookup` tool already builds for exactly this
/// reason.
///
/// Never mistaken for a second reply engine: it is the ONLY place `cmd_search`
/// builds its normal-answer text, called by `cmd_search` itself and by this
/// file's own tests.
fn render_search_reply(
    hits: &[lookup::LookupHit],
    query: &str,
    kinds: &[Kind],
    unfiltered_count: usize,
    literal_found_any: bool,
    withheld: usize,
) -> String {
    let mut out = String::new();
    if hits.is_empty() {
        // Names the kinds asked for, so a thin answer reads as "nothing of
        // that kind" rather than as an empty memory - and, when the
        // unfiltered ranking DID find something, how many hits of other
        // kinds there were (see `other_kind_hint`'s own doc comment: a bare
        // "no matches" there would be a lie by omission).
        if kinds.is_empty() {
            out.push_str(&format!("no matches for '{query}'
"));
        } else {
            out.push_str(&format!(
                "no matches for '{query}' of kind {}{}
",
                lookup::kind_names(kinds),
                lookup::other_kind_hint(unfiltered_count)
            ));
        }
    } else {
        for hit in hits {
            out.push_str(&format!("{:<28} {:<12} {}\n", hit.id, format!("{:?}", hit.item.kind), hit.item.text));
        }
        // Every hit above came from the meaning leg alone - say so, since an
        // answer built entirely from semantic extras otherwise looks exactly
        // like an ordinary literal result (see `meaning_only_note`'s own doc
        // comment).
        if let Some(note) = lookup::meaning_only_note(literal_found_any) {
            out.push_str(note);
            out.push('\n');
        }
    }
    if withheld > 0 {
        out.push_str(&format!(
            "({withheld} match(es) held back: their own expiry date has passed. `get <id>` still shows them whole.)\n"
        ));
    }
    out
}

/// The `--explain`/`--explain-id` block: strictly ADDITIONAL text meant to be
/// appended after `render_search_reply`'s own output, never altering it -
/// see `explain_only_appends_never_alters_the_normal_reply_lines`, the test
/// this guarantees. `hits` is the exact (already kind-filtered) list
/// `render_search_reply` just rendered, so the per-hit lines below name
/// exactly what was shown, never a wider or narrower set.
fn render_explain_block(
    explanation: &lookup::ExplainAnswer,
    hits: &[lookup::LookupHit],
    show_per_hit: bool,
    explain_id: Option<&str>,
) -> String {
    let mut out = String::new();
    out.push_str("explain:");
    match (explanation.floor, explanation.cap) {
        // Printed ONCE here, since a number a caller cannot see is not a
        // diagnosis - never re-typed per line below.
        (Some(floor), Some(cap)) => out.push_str(&format!(" floor {floor:.3}, cap {cap}\n")),
        _ => out.push_str(" no meaning door ran this call - every hit here is literal\n"),
    }
    if show_per_hit {
        let by_id: std::collections::HashMap<&str, &lookup::ExplainedHit> =
            explanation.hits.iter().map(|h| (h.id.as_str(), h)).collect();
        for hit in hits {
            match by_id.get(hit.id.as_str()) {
                Some(eh) if eh.literal => out.push_str(&format!("  {} literal\n", hit.id)),
                Some(eh) => out.push_str(&format!("  {} meaning {:.3}\n", hit.id, eh.similarity.unwrap_or(0.0))),
                // Cannot happen while explain_best_effort and search_best_effort
                // agree on ids (see this file's own test module and lookup.rs's
                // explain_path_and_plain_search_best_effort_agree_on_ids_and_order)
                // - skipped rather than panicking if that ever stops holding.
                None => {}
            }
        }
    }
    if let (Some(id), Some(target)) = (explain_id, &explanation.target) {
        out.push_str(&render_explain_target(id, target));
    }
    out
}

/// One line naming exactly why `--explain-id`'s named `id` did or did not
/// make the ranking - see `lookup::ExplainedTarget`'s own doc comment for
/// what each case means. Always names `id` itself: this line often follows
/// several per-hit lines above it (when `--explain` was also given), so a
/// reader must never have to guess which id this one is about.
fn render_explain_target(id: &str, target: &lookup::ExplainedTarget) -> String {
    match target {
        lookup::ExplainedTarget::Literal => format!("explain {id}: a literal hit - the floor and the cap never apply to it\n"),
        lookup::ExplainedTarget::Meaning { similarity } => format!("explain {id}: a meaning hit, similarity {similarity:.3}\n"),
        lookup::ExplainedTarget::BelowFloor { similarity, floor } => {
            format!("explain {id}: scored {similarity:.3} - below the floor {floor:.3}, so it never became a candidate\n")
        }
        lookup::ExplainedTarget::CutByCap { similarity, cap, outscored_by } => format!(
            "explain {id}: scored {similarity:.3}, above the floor - but {outscored_by} other candidate(s) outscored it, and only the top {cap} are ever shown\n"
        ),
        lookup::ExplainedTarget::NoVector => format!("explain {id}: no vector is on record for it, so no similarity could be computed\n"),
        lookup::ExplainedTarget::NotACandidate => {
            format!("explain {id}: not a live searchable candidate right now (unknown, a Lookup register, or expired)\n")
        }
    }
}

/// Everything `cmd_search` prints, in one string: the normal answer
/// (`render_search_reply`), then, only when asked, the explain block
/// strictly appended after it. Split out from `cmd_search` so a test can
/// call it directly against an in-memory store instead of spawning the
/// compiled binary - see `render_search_reply`'s own doc comment for why.
fn build_search_reply(
    store: &EventStore,
    vectors_path: &Path,
    query: &str,
    kinds: &[Kind],
    explain: bool,
    explain_id: Option<&str>,
) -> String {
    let (literal_hits, withheld) = lookup::search_with_expired(store, query);
    let hits = lookup::search_best_effort(store, vectors_path, None, query);
    let unfiltered_count = hits.len();
    let hits = lookup::only_kinds(hits, kinds);
    let mut out = render_search_reply(&hits, query, kinds, unfiltered_count, !literal_hits.is_empty(), withheld);
    if explain || explain_id.is_some() {
        let explanation = lookup::explain_best_effort(store, vectors_path, None, query, explain_id);
        out.push_str(&render_explain_block(&explanation, &hits, explain, explain_id));
    }
    out
}

/// Surface 4: free-text search over every live item, any project, archive
/// kinds (Report/Chunk) included - never an injection surface, so there is
/// no delivery to record here at all. Calls `search_best_effort`, which
/// silently degrades to plain text match without the `semantic` feature, or
/// with it but no usable model/sidecar - so this one call site is correct in
/// every build without an `#[cfg]` of its own.
fn cmd_search(db_path: &Path, query: &str, kind: Option<&str>, explain: bool, explain_id: Option<&str>) {
    // Parsed BEFORE the store is opened and before anything is ranked: an
    // unusable kind is a mistake in the question, and answering an unfiltered
    // search instead would look like a result. Exit 2, not 1: a refused
    // argument is not the same outcome as a search that ran and found nothing.
    let kinds: Vec<Kind> = match kind.map(str::trim).filter(|k| !k.is_empty()) {
        None => Vec::new(),
        Some(raw) => match lookup::parse_searchable_kinds(raw) {
            Ok(k) => k,
            Err(refusal) => {
                eprintln!("{refusal}");
                std::process::exit(2);
            }
        },
    };
    let store = open_store_or_die(db_path);
    let vectors_path = serve::semantic_paths::default_vectors_path(db_path);
    print!("{}", build_search_reply(&store, &vectors_path, query, &kinds, explain, explain_id));
}

/// Surface 4's other door: an explicit request for exactly one Lookup's key.
fn cmd_lookup_key(db_path: &Path, key: &str) {
    let store = open_store_or_die(db_path);
    match lookup::by_key(&store, key) {
        Some(hit) => println!("{}", hit.item.text),
        None => println!("no lookup found for key '{key}'"),
    }
}

/// A human line for a code answer's provenance - shared by `cmd_search_code`
/// and `cmd_status` so the two never phrase "how stale is this" differently.
fn format_code_provenance(p: &lookup::CodeProvenance) -> String {
    let current = match &p.current_commit {
        Ok(c) => c.clone(),
        Err(e) => format!("unknown ({e})"),
    };
    let differ = p.files_differ.map(|n| n.to_string()).unwrap_or_else(|| "an unknown number of".to_string());
    let uncommitted =
        p.uncommitted_changed.map(|n| n.to_string()).unwrap_or_else(|| "an unknown number of".to_string());
    format!(
        "code index at commit {}, current commit {current}, {differ} file(s) differ, {uncommitted} file(s) uncommitted",
        p.indexed_commit
    )
}

/// Surface 4's code door: search the code index against a repository,
/// printing its own drift line first, then every hit with the commit it
/// came from. A human-facing lookup, so a real error (a missing index, an
/// unreachable repository) is reported plainly and exits non-zero, never
/// swallowed.
fn cmd_search_code(index_db: &Path, repo: &Path, query: &str, limit: usize) {
    match lookup::search_code(index_db, repo, query, limit) {
        Ok(answer) => {
            println!("{}", format_code_provenance(&answer.provenance));
            if answer.hits.is_empty() {
                println!("no matches for '{query}'");
                return;
            }
            for hit in &answer.hits {
                println!(
                    "{}:{}-{} (commit {})\n{}\n",
                    hit.path, hit.start_line, hit.end_line, hit.commit_id, hit.text
                );
            }
        }
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}

/// Who defines and who uses one name. Provenance first, deliberately: a line
/// number from an index three commits behind is a wrong line number, and the
/// reader has to see that before the list, not after it.
fn cmd_where_used(index_db: &Path, repo: &Path, name: &str, limit: usize) {
    match lookup::where_used(index_db, repo, name, limit) {
        Ok(usage) => {
            println!("{}", format_code_provenance(&usage.provenance));
            if usage.defined_at.is_empty() && usage.referenced_at.is_empty() {
                println!("the index knows no symbol named '{name}'");
                return;
            }
            println!("defined at ({}):", usage.defined_at.len());
            for s in &usage.defined_at {
                println!("  {}:{}", s.path, s.line);
            }
            println!("used at ({}):", usage.referenced_at.len());
            for s in &usage.referenced_at {
                println!("  {}:{}", s.path, s.line);
            }
        }
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}

/// A file's shape. "Never indexed" and "indexed, defines nothing" are printed
/// as different sentences, because they call for different next steps.
fn cmd_outline(index_db: &Path, path: &str) {
    match lookup::outline(index_db, path) {
        Ok(None) => {
            println!(
                "'{path}' is not in the code index - check the path is repository-relative, or \
                 the file was added after the index was last built"
            );
        }
        Ok(Some(names)) if names.is_empty() => {
            println!("'{path}' is indexed but defines no named symbols");
        }
        Ok(Some(names)) => {
            println!("{path} ({} definitions):", names.len());
            for (name, line) in &names {
                println!("  {line}: {name}");
            }
        }
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}

// ------------------------------------------------------------------ status

/// What this store knows right now (CONTRACT gap 3): the fact-store half
/// (item counts per kind, how many are declared but never fired) always
/// runs; the code-index half only when both `--index-db` and `--repo` are
/// given - printed as a plain sentence either way, so "not configured" is
/// never confused with "configured and silent".
fn cmd_status(db_path: &Path, index_db: Option<&Path>, repo: Option<&Path>) {
    let store = open_store_or_die(db_path);
    let s = status::store_status(&store);
    println!("{:<12} {:>6}", "kind", "count");
    for row in &s.counts {
        println!("{:<12} {:>6}", format!("{:?}", row.kind), row.count);
    }
    println!("\ndeclared, never fired: {} of {} fireable item(s)", s.declared_never_fired, s.fireable_total);
    println!(
        "served repeatedly, never marked useful: {} of {} fireable item(s) - a count to look at, not a filter",
        s.served_never_marked, s.fireable_total
    );
    println!(
        "retired as noise (called noise {}+ times since the last mark of usefulness): {} of {} fireable item(s) - still fully findable via search",
        serve::decay::NOISE_MARKS_BEFORE_STALE, s.decayed, s.fireable_total
    );
    println!("missing falsifier: {} of {} fireable item(s)", s.missing_falsifier, s.fireable_total);
    println!("\n{}", s.semantic_search);
    #[cfg(feature = "semantic")]
    print_vectors_report(&store, &serve::semantic_paths::default_vectors_path(db_path));

    match (index_db, repo) {
        (Some(index_db), Some(repo)) => match lookup::code_index_status(index_db, repo) {
            Ok(p) => println!("\n{}", format_code_provenance(&p)),
            Err(e) => println!("\ncode index: {e}"),
        },
        (None, None) => println!("\ncode index: not configured (pass --index-db and --repo to check one)"),
        _ => println!("\ncode index: both --index-db and --repo are required to check one"),
    }
}

// ---------------------------------------------- surface 4: vectors (semantic)

/// Print how many vectors are stored and whether they still match the
/// current content - shared by `cmd_status`'s own semantic block and the
/// dedicated `vectors-status` command, so the two can never phrase this
/// differently.
#[cfg(feature = "semantic")]
fn print_vectors_report(store: &EventStore, vectors_path: &Path) {
    match serve::vectors::report(store, vectors_path) {
        Ok(r) => {
            println!("vectors sidecar : {}", vectors_path.display());
            println!("model_id        : {}", r.model_id_stored.as_deref().unwrap_or("(none - never built)"));
            println!("expected        : {}", r.model_id_expected);
            println!(
                "model_id        : {}",
                if r.model_id_matches() { "matches" } else { "MISMATCH - vectors_build must run again" }
            );
            println!("stored vectors  : {} (of {} live item(s))", r.stored_count, r.live_count);
            println!("missing (no vector yet)                : {}", r.missing);
            println!("stale (content changed since embedding) : {}", r.stale);
            println!("orphaned (no longer a live item)        : {}", r.orphaned);
        }
        Err(e) => println!("semantic vectors: could not be read ({e})"),
    }
}

/// Rebuild the sidecar from every live, non-Lookup item (feature `semantic`).
#[cfg(feature = "semantic")]
fn cmd_vectors_build(db_path: &Path, model_dir: Option<&Path>, vectors_db: Option<&Path>) {
    let store = open_store_or_die(db_path);
    let model_dir = model_dir.map(PathBuf::from).or_else(serve::semantic_paths::default_model_dir);
    let Some(model_dir) = model_dir else {
        eprintln!("no per-user data directory could be resolved for the model - pass --model-dir");
        std::process::exit(1);
    };
    let vectors_db = vectors_db.map(PathBuf::from).unwrap_or_else(|| serve::semantic_paths::default_vectors_path(db_path));
    match serve::vectors::build(&store, &model_dir, &vectors_db) {
        Ok(n) => println!("built {n} vector(s) at {}", vectors_db.display()),
        Err(e) => {
            eprintln!("could not build vectors: {e}");
            std::process::exit(1);
        }
    }
}

/// How many vectors are stored and whether they still match the current
/// content (feature `semantic`).
#[cfg(feature = "semantic")]
fn cmd_vectors_status(db_path: &Path, vectors_db: Option<&Path>) {
    let store = open_store_or_die(db_path);
    let vectors_db = vectors_db.map(PathBuf::from).unwrap_or_else(|| serve::semantic_paths::default_vectors_path(db_path));
    print_vectors_report(&store, &vectors_db);
}

/// Embed only what changed, drop what is no longer live, and print one
/// honest line (feature `semantic`) - see `serve::vectors::refresh`'s own
/// doc comment for exactly what counts as changed.
#[cfg(feature = "semantic")]
fn cmd_vectors_refresh(db_path: &Path, model_dir: Option<&Path>, vectors_db: Option<&Path>, budget: usize) {
    let store = open_store_or_die(db_path);
    let model_dir = model_dir.map(PathBuf::from).or_else(serve::semantic_paths::default_model_dir);
    let Some(model_dir) = model_dir else {
        eprintln!("no per-user data directory could be resolved for the model - pass --model-dir");
        std::process::exit(1);
    };
    let vectors_db = vectors_db.map(PathBuf::from).unwrap_or_else(|| serve::semantic_paths::default_vectors_path(db_path));
    match serve::vectors::refresh(&store, &model_dir, &vectors_db, budget) {
        Ok(outcome) if outcome.model_id_mismatch => {
            println!("refused: stored model_id does not match this binary's - run vectors-build first");
        }
        Ok(outcome) if outcome.embedded == 0 && outcome.deleted == 0 => {
            println!("nothing needed doing at {}", vectors_db.display());
        }
        Ok(outcome) => {
            println!(
                "embedded {}, deleted {}, {} remaining at {}",
                outcome.embedded, outcome.deleted, outcome.remaining, vectors_db.display()
            );
        }
        Err(e) => {
            eprintln!("could not refresh vectors: {e}");
            std::process::exit(1);
        }
    }
}

// -------------------------------------------------------------------- mark

/// Record that `id` actually helped. A human-facing, deliberate write (R5's
/// "write and declare" side): a real failure is reported plainly, never
/// swallowed the way `hook`'s delivery telemetry is.
fn cmd_mark(db_path: &Path, id: &str, noise: bool) {
    let mut store = open_store_or_die(db_path);
    let now = time::now_iso8601();
    let written = if noise {
        mark::record_noise(&mut store, "cli", "cli", "cli", &now, id, None)
    } else {
        mark::record_useful(&mut store, "cli", "cli", "cli", &now, id, None)
    };
    // A verdict typed here is written, and counts for decay, but it clears no
    // judgement debt: only a verdict made through THOR's own `mark` tool in a
    // Claude Code session - one the session's Stop can verify - does (D9).
    let debt = "written from the command line, so it clears no judgement debt - to clear that, judge it with THOR's \
                mark tool inside the Claude Code session it fired in";
    match written {
        Ok(_) if noise => println!(
            "marked noise: {id} ({}+ of these, with no mark of usefulness, retires it from the injection surfaces; it stays findable via search) - {debt}",
            serve::decay::NOISE_MARKS_BEFORE_STALE
        ),
        Ok(_) => println!("marked useful: {id} (clears the noise recorded before it; a later noise mark still counts) - {debt}"),
        Err(e) => {
            eprintln!("could not record the mark for {id}: {e}");
            std::process::exit(1);
        }
    }
}

/// `Command::Retest` - see that variant's own doc comment. Opens an existing
/// store only (never creates one), tries each rule, prints what it found, and
/// exits with the code the variant's doc comment names.
fn cmd_retest(db_path: &Path, id: Option<&str>, project: Option<&str>, all: bool, apply: bool) {
    let mut store = match EventStore::open_existing(db_path) {
        Ok(store) => store,
        Err(e) => {
            eprintln!("could not open the store at {}: {e}", db_path.display());
            std::process::exit(1);
        }
    };
    let corpus = serve::legit::Corpus::load(&store);
    if let Some(id) = id {
        std::process::exit(retest_one(&mut store, db_path, id, &corpus, apply));
    }
    let wanted = project.and_then(|p| model::normalize::normalize_project(Some(p)));
    let ids: Vec<String> = serve::live::live_items(&store)
        .into_iter()
        .filter(|li| li.item.kind.can_fire())
        .filter(|li| all || model::normalize::normalize_project(li.item.project.as_deref()) == wanted)
        .map(|li| li.id)
        .collect();
    let (mut decided, mut tried) = (0usize, 0usize);
    let mut worst = 0;
    for id in &ids {
        if !matches!(model::store::current_verdict(&store, id), model::store::CurrentVerdict::Undecided { .. }) {
            decided += 1;
            continue;
        }
        tried += 1;
        let code = retest_one(&mut store, db_path, id, &corpus, apply);
        worst = worse_exit(worst, code);
        println!();
    }
    println!("tried {tried} rule(s) without a current verdict; {decided} already have one and were left as they are");
    std::process::exit(worst);
}

/// The worse of two `retest` exit codes: an error, then needs the agent, then
/// blocked, then a verdict.
fn worse_exit(a: i32, b: i32) -> i32 {
    let rank = |c: i32| match c {
        1 => 3,
        4 => 2,
        3 => 1,
        _ => 0,
    };
    if rank(b) > rank(a) {
        b
    } else {
        a
    }
}

/// Retest one rule, print it, write it with `apply`; the exit code.
fn retest_one(store: &mut EventStore, db_path: &Path, id: &str, corpus: &serve::legit::Corpus, apply: bool) -> i32 {
    let retest = match serve::teeth::retest(store, Some(db_path), id, corpus) {
        Ok(retest) => retest,
        Err(why) => {
            println!("{id}: cannot be tried - {why}");
            return 1;
        }
    };
    println!("{id}:");
    match (&retest.root, retest.project.as_deref()) {
        (Ok(root), Some(project)) => println!("  root: {} (registered for '{project}')", root.display()),
        (Ok(root), None) => println!("  root: {}", root.display()),
        (Err(problem), Some(project)) => println!("  root: none - {problem} for '{project}'"),
        (Err(problem), None) => println!("  root: none - {problem}"),
    }
    for attempt in &retest.attempts {
        let outcome = match attempt.outcome {
            model::store::AttemptOutcome::Refused => "refused",
            model::store::AttemptOutcome::LetThrough => "let through",
        };
        println!("  tried {} (as a {} call): {outcome}", attempt.form, attempt.via);
    }
    for line in &retest.lines {
        println!("  {line}");
    }
    let (verdict, code) = match &retest.outcome {
        serve::teeth::Maintenance::Rule { proven: true, .. } => ("rule track, proven".to_string(), 0),
        serve::teeth::Maintenance::Rule { proven: false, .. } => ("rule track, wired".to_string(), 0),
        serve::teeth::Maintenance::Note { because } => (format!("note - {because}"), 0),
        serve::teeth::Maintenance::Blocked(blocker) => (format!("blocked: {blocker}; nothing written"), 3),
        serve::teeth::Maintenance::NeedsAgent(fix) => (format!("needs the agent - {fix}; nothing written"), 4),
    };
    println!("  verdict: {verdict}");
    if code != 0 {
        return code;
    }
    if retest.write.is_none() {
        println!("  nothing to write: it stays the note it is");
        return 0;
    }
    if !apply {
        println!("  nothing written - run this again with --apply to write this verdict");
        return 0;
    }
    match serve::teeth::apply_as_retest(store, &retest) {
        Ok(event) => {
            println!("  written: fact_revised at log seq {} by '{}'", event.seq, event.actor);
            0
        }
        Err(serve::teeth::ApplyError::NeedsAgent(fix)) => {
            println!("  needs the agent - {fix}; nothing written");
            4
        }
        Err(e) => {
            println!("  {e}");
            1
        }
    }
}

#[cfg(test)]
mod subagent_detection_tests {
    use super::payload_is_from_a_subagent;
    use serde_json::json;

    /// The documented shape (Claude Code hooks reference): `agent_id` is a
    /// non-empty string present only inside a Task-tool subagent call.
    #[test]
    fn a_payload_carrying_a_real_agent_id_is_a_subagent() {
        let payload = json!({"hook_event_name": "PreToolUse", "session_id": "s1", "agent_id": "a1dca2c0feb7f44fb"});
        assert!(payload_is_from_a_subagent(&payload));
    }

    /// The ordinary shape for the owner's own main session: no `agent_id` key
    /// at all. This is the case that must never misfire - see this
    /// function's own doc comment.
    #[test]
    fn a_payload_with_no_agent_id_is_not_a_subagent() {
        let payload = json!({"hook_event_name": "PreToolUse", "session_id": "s1"});
        assert!(!payload_is_from_a_subagent(&payload));
    }

    /// Defensive: an empty string or a non-string value must not be read as
    /// "present" - only a real, non-empty id counts.
    #[test]
    fn an_empty_or_non_string_agent_id_is_not_a_subagent() {
        assert!(!payload_is_from_a_subagent(&json!({"agent_id": ""})));
        assert!(!payload_is_from_a_subagent(&json!({"agent_id": null})));
        assert!(!payload_is_from_a_subagent(&json!({"agent_id": 42})));
    }
}

#[cfg(test)]
mod search_explain_tests {
    /// THE DEFECT THIS CLOSES, reported 2026-09-19 by the first program that
    /// drives this boundary itself: `hook` prints nothing and exits 0 whether
    /// it looked and found nothing or could not look at all, and a program whose
    /// job is to stop a wrong action cannot tell those apart. Both real cases
    /// are pinned here.
    #[test]
    fn preflight_names_a_payload_that_is_not_json_and_blames_the_usual_cause() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        EventStore::new(&db).unwrap();
        let refusal = hook_preflight(r#"{"cwd": "C:\Users\dev\repo"}"#, &db)
            .expect_err("single backslashes are not valid JSON");
        assert!(refusal.starts_with("could not look:"), "{refusal}");
        assert!(refusal.contains("not valid JSON"), "{refusal}");
        assert!(refusal.contains("forward slashes"), "must name the fix: {refusal}");
    }

    /// A store path that does not exist reads, from outside, exactly like a
    /// memory with nothing to say. It has to be named instead.
    #[test]
    fn preflight_names_a_memory_that_is_not_there() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nowhere").join("thor.db");
        let refusal = hook_preflight(r#"{"hook_event_name":"SessionStart"}"#, &missing)
            .expect_err("a missing store must be named");
        assert!(refusal.starts_with("could not look:"), "{refusal}");
        assert!(refusal.contains(&missing.display().to_string()), "must name the path: {refusal}");
    }

    /// And the honest pass: a real payload against a real store says nothing at
    /// all, so the diagnosing path only ever speaks about real trouble.
    #[test]
    fn preflight_passes_a_real_payload_against_a_real_store() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        EventStore::new(&db).unwrap();
        assert_eq!(hook_preflight(r#"{"hook_event_name":"SessionStart"}"#, &db), Ok(()));
    }

    use super::*;
    use model::item::{Item, Kind};
    use model::store;
    use thor_core::event_store::EventStore;

    fn report(id: &str, text: &str) -> Item {
        Item {
            id: id.to_string(),
            kind: Kind::Report,
            text: text.to_string(),
            bindings: vec![],
            severity: None,
            project: Some("test-project".to_string()),
            tags: vec![],
            expires: Some("2027-01-01".to_string()),
            key: None,
            falsifier: None,
            check: None,
        }
    }

    /// THE DEFECT THIS PREVENTS: an `--explain`/`--explain-id` flag that
    /// altered the normal answer (reordered a hit, dropped one, changed a
    /// line) would make the diagnostic surface itself untrustworthy - a
    /// caller could never be sure what they saw was the real ranking, only
    /// what it looked like with a flag attached.
    #[test]
    fn explain_only_appends_never_alters_the_normal_reply_lines() {
        let mut db = EventStore::in_memory().unwrap();
        store::declare(&mut db, "s", "l", "a", &report("r1", "the bbq recipe lives here")).unwrap();
        store::declare(&mut db, "s", "l", "a", &report("r2", "a totally unrelated fact about zebras")).unwrap();
        let vectors_dir = tempfile::tempdir().unwrap();
        let vectors_path = vectors_dir.path().join("v.db");

        let plain = build_search_reply(&db, &vectors_path, "bbq", &[], false, None);
        let explained = build_search_reply(&db, &vectors_path, "bbq", &[], true, None);
        assert!(explained.starts_with(&plain), "explain must only ever append, never alter, the normal reply: {explained}");
        assert_ne!(explained, plain, "explain must actually add something when there are hits to explain");
    }

    /// THE DEFECT THIS PREVENTS: a kind-filtered search that finds nothing
    /// reads exactly like an empty memory unless it also says how many hits
    /// of OTHER kinds the same ranking found - a bare "no matches" there is
    /// a lie by omission.
    #[test]
    fn kind_filtered_empty_reply_names_how_many_other_kind_hits_there_were() {
        let mut db = EventStore::in_memory().unwrap();
        store::declare(&mut db, "s", "l", "a", &report("gizmo-1", "a gizmo has nothing to do with rules")).unwrap();
        let vectors_dir = tempfile::tempdir().unwrap();
        let vectors_path = vectors_dir.path().join("v.db");

        let reply = build_search_reply(&db, &vectors_path, "gizmo", &[Kind::Rule], false, None);
        assert!(reply.contains("no matches for 'gizmo' of kind Rule"), "{reply}");
        assert!(reply.contains("1 hit(s) of other kind(s)"), "must name how many other-kind hits there were: {reply}");
    }
}
