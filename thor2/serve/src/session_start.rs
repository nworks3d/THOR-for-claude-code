//! Surface 1: SESSION START. Serves exactly the items bound `Always`, scoped
//! to global (no project) plus the current project - full, never capped,
//! never ranked: CONTRACT.md's promise here is "the standing rules, complete,
//! or nothing", not "the four most relevant ones".
//!
//! `Always` reaches an injection surface through exactly this door.
//! `rank::binding_matches` refuses `Always` outright (see its own doc
//! comment) precisely so the moment/prompt surfaces can never also serve it -
//! the five surfaces never take each other's places (CONTRACT.md).

use crate::live::LiveItem;
use crate::rank::RankedItem;
use model::item::Binding;

/// Every live item whose kind can fire, is bound `Always`, and whose own
/// project is either unset (global) or exactly `project` - in the same
/// deterministic id order `live::live_items` already provides (this is a
/// pure filter over that order, so no further sort is applied: "no ranking,
/// no threshold" is not just a policy here, it is the absence of a sort
/// call). A different project's `Always` item is excluded, never shown and
/// never counted.
pub fn select(candidates: &[LiveItem], project: Option<&str>) -> Vec<RankedItem> {
    candidates
        .iter()
        .filter(|c| c.item.kind.can_fire())
        .filter(|c| c.item.bindings.iter().any(|b| matches!(b, Binding::Always)))
        // An overflow pin is not in this block: the first prompt delivers it (`select_overflow`).
        .filter(|c| !model::store::is_overflow_pin(&c.item))
        .filter(|c| crate::project::applies_to(c.item.project.as_deref(), project))
        .map(|c| RankedItem { id: c.id.clone(), item: c.item.clone(), via: None })
        .collect()
}

/// The overflow pins that apply in `project` (its own and the global ones): pinned notes written on
/// the owner's own go because the session start block was full. Delivered whole at the first
/// prompt of each context, within `model::item::HOOK_OUTPUT_LIMIT_BYTES`.
pub fn select_overflow(candidates: &[LiveItem], project: Option<&str>) -> Vec<RankedItem> {
    candidates
        .iter()
        .filter(|c| c.item.kind.can_fire())
        .filter(|c| c.item.bindings.iter().any(|b| matches!(b, Binding::Always)))
        .filter(|c| model::store::is_overflow_pin(&c.item))
        .filter(|c| crate::project::applies_to(c.item.project.as_deref(), project))
        .map(|c| RankedItem { id: c.id.clone(), item: c.item.clone(), via: None })
        .collect()
}

/// The block text, or `None` when nothing applies - never a truncated list:
/// every item `select` returns is rendered whole, with no `render::cap` in
/// this surface's path at all.
///
/// Each shown item's line opens with `[id]`, straight from `RankedItem.id`
/// (the event log's own entity id, not the copy carried inside the item
/// body - see `live::LiveItem`) - the same bracket form and the same field
/// `render::render_text` uses for the moment/prompt surfaces. See that
/// function's own doc comment for the defect this closes: a served fact
/// with no id forces an assistant that notices it is wrong to guess search
/// terms to find the item again before it can call `revise`. There is no
/// budget to protect the id from here, unlike that surface: this one never
/// truncates anything to begin with (see the paragraph above), so the id is
/// always additive, never a trade against how much of the body is delivered
/// (see `adding_the_id_never_shrinks_how_much_of_the_body_is_delivered`).
///
/// The `falsifier` is deliberately NOT printed here, and this is the one
/// place worth arguing about, so the argument is written down.
///
/// Its value is at WRITE time: it forces the author to name the observation
/// that would make the fact false, and refusing a rule without one is what
/// keeps the store honest. That value is already banked by the time anything
/// reaches this surface. Printing it back on every session start roughly
/// DOUBLED this block on the real store (1900 to 3700 characters) for a
/// benefit nothing has measured - and this whole rebuild exists because the
/// blocks were too noisy, so paying that with no evidence is the exact trade
/// 2.0 is against.
///
/// The line 2.0 draws instead: surfaces that PUSH at a reader stay minimal;
/// surfaces a reader ASKS carry everything. So the falsifier travels on `get`
/// (one item, on request, whole) and is counted in `status`/`doctor`, and
/// never rides along on session start, the moment of action, or the prompt.
pub fn render(items: &[RankedItem]) -> Option<String> {
    render_within(items, usize::MAX).map(|r| r.text)
}

/// The most bytes a hook output may carry. Claude Code keeps a hook output
/// longer than 10,000 characters in a file and shows the agent only its first
/// 2 KB (measured on this machine's own transcripts: the largest output that
/// arrived whole was 9,972 characters, the smallest that was cut was 9.8 KB,
/// i.e. 10,035 bytes). The budget counts BYTES, never fewer than characters, so
/// a multi-byte letter only makes it safer. 9,800 keeps 200 bytes under the
/// 10,000-character line and 172 under the largest output seen whole; it was
/// 9,500, which left about one pinned rule of slack on a full block.
///
/// The number lives in `model::item`, because the write gate refuses a pin that
/// would not fit in it; this is the same constant.
pub const HOOK_OUTPUT_LIMIT_BYTES: usize = model::item::HOOK_OUTPUT_LIMIT_BYTES;

/// The most bytes of rule ids the "did not fit" line names. The rest is
/// counted ("and N more"). Nothing may be crowded out silently, and the line
/// comes out of the same budget as the rules.
pub const HIDDEN_IDS_ALLOWANCE: usize = 400;

/// A rule that did not fit: its id, and whether `Always` is its only binding.
/// Such a rule is reachable nowhere else (no command, file or moment brings it
/// back), so the line names those first.
struct Hidden<'a> {
    id: &'a str,
    only_always: bool,
}

/// The one line that says which standing rules were left out: the ones bound
/// to nothing but `Always` first, then the others, at most
/// `HIDDEN_IDS_ALLOWANCE` bytes of ids, then "and N more". Printed whenever
/// anything is cut, never otherwise.
fn not_fit_note(hidden: &[Hidden]) -> String {
    let mut named_only_always: Vec<&str> = Vec::new();
    let mut named_others: Vec<&str> = Vec::new();
    let mut used = 0;
    let in_order = hidden.iter().filter(|h| h.only_always).chain(hidden.iter().filter(|h| !h.only_always));
    for h in in_order {
        let cost = h.id.len() + if used == 0 { 0 } else { 2 };
        if used + cost > HIDDEN_IDS_ALLOWANCE {
            break;
        }
        used += cost;
        match h.only_always {
            true => named_only_always.push(h.id),
            false => named_others.push(h.id),
        }
    }
    let rest = hidden.len() - named_only_always.len() - named_others.len();
    let mut note = format!("[THOR] {} more standing rule(s) did not fit in this block and are not shown.", hidden.len());
    if !named_only_always.is_empty() {
        note.push_str(&format!(
            " These are bound to nothing but 'always', so nothing else brings them back: {}.",
            named_only_always.join(", ")
        ));
    }
    if !named_others.is_empty() {
        note.push_str(&format!(" Also not shown: {}.", named_others.join(", ")));
    }
    if rest > 0 {
        note.push_str(&format!(" And {rest} more."));
    }
    note.push_str(
        " Open one with the get tool (give it the id), or use the lookup tool when something you are about to do may be covered.",
    );
    note
}

/// What `render_within` put in the block.
pub struct Rendered {
    pub text: String,
    /// The ids that are really in `text`, in order. Only these were shown.
    pub shown: Vec<String>,
    /// How many standing rules did not fit and are not in `text`.
    pub hidden: usize,
    /// The ids of those rules: the ones bound to nothing but `Always` first,
    /// then the others, each in block order.
    pub hidden_ids: Vec<String>,
}

/// The standing-rules block, kept within `budget` bytes. Nothing is cut in the
/// middle: every rule is in the block whole or not in it at all. This project's
/// own facts come first, then the heavy global rules, then house style, then the
/// global facts with no weight (see [`render_within_ranked`]); when not
/// everything fits, the rest is replaced by ONE line saying how many were left
/// out, and the caller records as shown only `Rendered::shown`. `None` when
/// there is nothing to render.
pub fn render_within(items: &[RankedItem], budget: usize) -> Option<Rendered> {
    render_within_ranked(items, budget, &std::collections::BTreeSet::new())
}

/// [`render_within`], ranked for relevance (steps 31 and 32). A project's first
/// prompt is not known at session start, so relevance comes from whose fact it
/// is and from what the facts did here lately. Order, one place after the other:
///
/// 1. this project's own facts;
/// 2. the global heavy rules (irreversible, then costly);
/// 3. the global house-style rules;
/// 4. the global facts with no weight at all (no severity): reply habits, other
///    areas, experiments - last, so they only fill what is left.
///
/// Inside a place, a fact that fired in this project's own sessions lately
/// (`recent`: shown at a prompt or before a tool call, never counting a session
/// start) comes first, then the heavier, then id order. Field test 7: about 60
/// percent of a code task's block was noise while the project's own facts shared
/// slots with global style rules.
///
/// The cap and the "N did not fit" line are exactly [`render_within`]'s.
pub fn render_within_ranked(
    items: &[RankedItem],
    budget: usize,
    recent: &std::collections::BTreeSet<String>,
) -> Option<Rendered> {
    if items.is_empty() {
        return None;
    }
    let head: Vec<String> = vec![crate::render::FRAMING_LINE.to_string(), model::item::STANDING_RULES_HEADER.to_string()];
    // Claude Code keeps a long hook output in a file and shows the agent only
    // its first 2 KB (measured 2026-10-01: 53 items, 16 KB, 6 visible), so the
    // order decides what is seen. The sort is stable: ids order what is left.
    let place = |r: &RankedItem| -> u8 {
        use model::item::Severity::{Costly, HouseStyle, Irreversible};
        match (r.item.project.is_some(), r.item.severity) {
            (true, _) => 0,
            (false, Some(Irreversible | Costly)) => 1,
            (false, Some(HouseStyle)) => 2,
            (false, None) => 3,
        }
    };
    let mut ordered: Vec<&RankedItem> = items.iter().collect();
    ordered.sort_by_key(|r| (place(r), !recent.contains(&r.id), r.item.severity.map_or(3u8, |s| s as u8)));
    let lines: Vec<String> = ordered.iter().map(|r| model::item::standing_line(&r.id, &r.item.text)).collect();
    let head_len: usize = head.iter().map(|l| l.len() + 1).sum();
    let all_len: usize = head_len + lines.iter().map(|l| l.len() + 1).sum::<usize>();
    // The note names the rules it hides, so its length depends on which rules
    // those are, and which are hidden depends on the room the note leaves.
    // Place the rules in `room`, write the note for what did not fit, and if
    // rules plus note are over the budget take the excess off the room and
    // place again. The room only ever shrinks, so this ends; the block is never
    // over `budget`, and `hidden` is what really is not in it.
    let mut room = if all_len <= budget { usize::MAX } else { budget };
    let (shown_at, note) = loop {
        let mut used = head_len;
        let mut shown_at: Vec<usize> = Vec::new();
        for (at, line) in lines.iter().enumerate() {
            if used.saturating_add(line.len() + 1) > room {
                continue;
            }
            used += line.len() + 1;
            shown_at.push(at);
        }
        if shown_at.len() == ordered.len() {
            break (shown_at, None);
        }
        let hidden: Vec<Hidden> = (0..ordered.len())
            .filter(|at| !shown_at.contains(at))
            .map(|at| Hidden {
                id: &ordered[at].id,
                only_always: ordered[at].item.bindings.iter().all(|b| matches!(b, Binding::Always)),
            })
            .collect();
        let note = not_fit_note(&hidden);
        let total = used + note.len() + 1;
        if total <= budget || shown_at.is_empty() {
            break (shown_at, Some((note, hidden)));
        }
        room = used - (total - budget);
    };
    let mut out = head;
    let mut shown = Vec::new();
    for at in &shown_at {
        shown.push(ordered[*at].id.clone());
        out.push(lines[*at].clone());
    }
    let (hidden, hidden_ids) = match note {
        Some((note, hidden)) => {
            out.push(note);
            let first = hidden.iter().filter(|h| h.only_always).chain(hidden.iter().filter(|h| !h.only_always));
            (hidden.len(), first.map(|h| h.id.to_string()).collect())
        }
        None => (0, Vec::new()),
    };
    Some(Rendered { text: out.join("\n"), shown, hidden, hidden_ids })
}

/// How many days back "fired in this project lately" looks, for the ranking of
/// the session start block.
pub const RECENT_FACTS_DAYS: i64 = 14;

/// The ids shown in this project's own sessions in the last `RECENT_FACTS_DAYS`
/// days (`render_within_ranked` puts them first). Empty without a project.
pub fn recent_in_project(db: &std::path::Path, project: Option<&str>, now_unix: i64) -> std::collections::BTreeSet<String> {
    let Some(project) = project else { return Default::default() };
    let from = now_unix - RECENT_FACTS_DAYS * 86_400;
    let sessions = crate::usefulness::sessions_of_project_since(db, project, from);
    crate::volume::shown_in_sessions_since(db, &sessions, from)
}

/// The standing-rules block an interactive session start in `project` would
/// build: the same selection, the same ranking and the same fitting as the
/// hook, so doctor and the session start can never disagree about which rules
/// do not fit. The rules take their room first (`STANDING_BLOCK_LIMIT_BYTES`),
/// so the other lines of a start never change which rules are shown. `None`
/// when no standing rule applies.
pub fn render_for_project(ctx: &DoctorStart, db: &std::path::Path, project: Option<&str>, now_unix: i64) -> Option<Rendered> {
    let (items, _) = split_for_session_start(crate::decay::retain_live(select(&ctx.candidates, project), &ctx.decay), false);
    let recent = recent_in_project(db, project, now_unix);
    render_within_ranked(&items, model::item::standing_limit_for(project), &recent)
}

/// The extra lines of a start that fit next to `rules_len` bytes of standing
/// rules, in their order. The rules took their room first
/// (`STANDING_BLOCK_LIMIT_BYTES`), so the compaction reminder, which must always be shown, always fits in the
/// reserve; in a folder of no project the rules get `NO_PROJECT_STANDING_LIMIT_BYTES`, which also keeps the no-project line's room. THE ORDER WHEN
/// ROOM IS SHORT: the reminder, the no-project line, then every short line that someone must act on (the copy line, the
/// registration notice, open points, a missing program, a copy that does not
/// answer, the folder notices, the no-project line), in the order they come,
/// and last the tools overview, which takes what is left and shrinks to fit
/// (`work_folders::shrink_overview`: fewer tool lines, at the smallest one line).
/// A line that does not fit is left out, and then ONE line says so
/// (`EXTRAS_DROPPED_NOTE`, also in the reserve): nothing is left out silently.
pub fn fit_extras(rules_len: usize, extras: Vec<String>) -> Vec<String> {
    let cost = |line: &String| line.len() + 2;
    let is_overview = |line: &String| line.starts_with(crate::work_folders::OVERVIEW_PREFIX);
    // The two lines that are never left out: the compaction reminder and the
    // no-project line (it has its own reserve, `NO_PROJECT_LINE_MAX_BYTES`).
    let must_show = |line: &String| line == COMPACT_REMINDER || line.starts_with(crate::other_project::NO_PROJECT_PREFIX);
    let room = HOOK_OUTPUT_LIMIT_BYTES.saturating_sub(rules_len);
    let note_cost = model::item::EXTRAS_DROPPED_NOTE.len() + 2;
    for with_note in [false, true] {
        let mut left = room.saturating_sub(if with_note { note_cost } else { 0 });
        left = left.saturating_sub(extras.iter().filter(|e| must_show(e)).map(cost).sum());
        let mut kept: Vec<Option<String>> = vec![None; extras.len()];
        let mut dropped = false;
        for (at, line) in extras.iter().enumerate() {
            if must_show(line) {
                kept[at] = Some(line.clone());
            } else if !is_overview(line) {
                if cost(line) <= left {
                    left -= cost(line);
                    kept[at] = Some(line.clone());
                } else {
                    dropped = true;
                }
            }
        }
        for (at, line) in extras.iter().enumerate().filter(|(_, line)| is_overview(line)) {
            match crate::work_folders::shrink_overview(line, left.saturating_sub(2)) {
                Some(text) => {
                    left -= cost(&text);
                    kept[at] = Some(text);
                }
                None => dropped = true,
            }
        }
        if dropped && !with_note {
            continue;
        }
        let mut out: Vec<String> = kept.into_iter().flatten().collect();
        if dropped {
            out.push(model::item::EXTRAS_DROPPED_NOTE.to_string());
        }
        return out;
    }
    extras
}

/// What doctor reads ONCE per run for every project it measures: the standing
/// rules' candidates and the decay state. Nothing is recorded or started.
pub struct DoctorStart {
    candidates: Vec<LiveItem>,
    decay: crate::decay::DecayContext,
}

impl DoctorStart {
    pub fn load(store: &thor_core::event_store::EventStore, _db: &std::path::Path) -> Self {
        DoctorStart { candidates: crate::live::always_candidates(store), decay: crate::decay::DecayContext::load(store) }
    }
}

/// The one plain line a session start shows when the folder it runs in is a git
/// repository THOR has no memory for yet (no registered working copy for its
/// project), naming the one command that gives it one. `None` for a folder that
/// is not a git repository, a topic folder, and a project whose working copy is
/// registered.
pub fn unregistered_repo_line(
    store: &thor_core::event_store::EventStore,
    cwd: &std::path::Path,
    project: Option<&str>,
    install: &str,
    db: &std::path::Path,
) -> Option<String> {
    let project = project?;
    let top = crate::project::git_root(cwd)?;
    if crate::other_project::is_topic_folder(cwd) || model::store::project_root_for(store, Some(project)).is_some() {
        return None;
    }
    let db = db.display().to_string().replace('\\', "/");
    Some(format!(
        "[THOR] This folder ({}) is a git project that THOR has no memory for yet, so notes about it cannot come back here and its code cannot be searched. Tell the person in one plain sentence and offer to set it up; they only answer yes or no. On yes, run from inside this folder:  \"{install}\" --db \"{db}\"",
        top.display().to_string().replace('\\', "/")
    ))
}

/// What decides which extra lines a session start carries.
pub struct ExtrasFor<'a> {
    /// The payload's `source` ("startup", "resume", "compact", ...).
    pub source: Option<&'a str>,
    pub headless: bool,
    pub subagent: bool,
    /// Whether a notice shown now is recorded as shown (`serve::notices`).
    pub record: bool,
    /// The store is a read-only copy of another machine's memory.
    pub copy: bool,
    /// Whether a due refresh of a copy is started in the background (the
    /// hook does; doctor only measures).
    pub refresh_copy: bool,
}

/// The extra lines of a session start that are the same for every project on
/// this machine: the copy line, the compaction reminder, a missing program, the
/// work folders and a copy that does not answer. Reading them can touch a
/// network share and the other machine, so doctor builds them ONCE per run.
#[derive(Clone)]
pub struct SharedExtras {
    copy_line: Option<String>,
    reminder: Option<String>,
    missing_program: Option<String>,
    work_folders: Vec<String>,
    replica_down: Option<String>,
    /// The setting files edited by hand that differ from the last session start (`serve::mode_files`).
    mode_changes: Vec<String>,
}

/// Build the lines that do not depend on the project.
pub fn shared_extras(db: &std::path::Path, f: &ExtrasFor) -> SharedExtras {
    let now = crate::time::now_unix();
    let copy_line = (f.copy && !f.subagent).then(|| {
        let marker = thor_core::copy_mode::read_marker(db);
        if f.refresh_copy && thor_core::copy_mode::pull_due(&marker, now) {
            thor_core::copy_mode::spawn_pull(db);
        }
        thor_core::copy_mode::session_line_with(&marker, crate::copy_eval::connector_configured(db))
    });
    let work_folders = match f.subagent {
        true => Vec::new(),
        false => crate::work_folders::session_start_lines(
            db,
            now,
            !f.headless,
            f.record,
            crate::work_folders::HOOK_READ_LIMIT,
            &crate::work_folders::READERS,
        )
        .into_lines(),
    };
    SharedExtras {
        copy_line,
        reminder: compact_reminder(f.source, f.subagent),
        missing_program: (!f.subagent).then(|| crate::missing_programs::session_line(db)).flatten(),
        work_folders,
        replica_down: (!f.subagent).then(|| crate::replica_reach::down_line(db)).flatten(),
        // Only a session the owner reads moves the record (`f.record`, never headless).
        mode_changes: match f.subagent {
            true => Vec::new(),
            false => crate::mode_files::session_start_lines(db, f.record && !f.headless),
        },
    }
}

/// The two extra lines that depend on the project: the registration notice
/// (for seven days after its working copy was registered or moved, at most once
/// a day per project; `project::registration_notice`) and the open points.
/// Neither reaches a headless session.
fn project_extras(
    store: &thor_core::event_store::EventStore,
    db: &std::path::Path,
    project: Option<&str>,
    f: &ExtrasFor,
) -> (Option<String>, Option<String>) {
    let now = crate::time::now_unix();
    let registration = crate::project::registration_notice(store, project, now)
        .filter(|_| !f.headless && crate::notices::due(db, project.unwrap_or(""), "registration", now, f.record));
    let open_points = (!f.subagent && !f.headless)
        .then(|| open_points_session_start_note(store, project, &|p| crate::notices::due(db, p, "open-points", now, f.record)))
        .flatten();
    (registration, open_points)
}

/// Every line a session start carries besides the standing rules, in the order
/// they follow them: the copy line, the compaction reminder, the registration
/// notice, the open points, a missing program, the work folders and a copy that
/// does not answer. The hook subtracts their size from the rules' room, and
/// doctor does the same through the same two builders, so doctor reports what
/// a real start hides.
pub fn start_extras(
    store: &thor_core::event_store::EventStore,
    db: &std::path::Path,
    project: Option<&str>,
    f: &ExtrasFor,
) -> Vec<String> {
    let (registration, open_points) = project_extras(store, db, project, f);
    assemble_extras(shared_extras(db, f), registration, open_points)
}

fn assemble_extras(shared: SharedExtras, registration: Option<String>, open_points: Option<String>) -> Vec<String> {
    [shared.copy_line, shared.reminder, registration, open_points, shared.missing_program, shared.replica_down]
        .into_iter()
        .flatten()
        .chain(shared.work_folders)
        .chain(shared.mode_changes)
        .collect()
}

/// This project's own open points (`model::store::open_points`), as ONE line
/// at `SessionStart`: how many there are, and that they are the owner's
/// (step 27). Field test 3 (2026-10-01): the list of them - THOR's own, legacy
/// ones that "settle nothing", and a stale text that still said "the owner
/// decides" - was put in front of every agent at every start, and an agent can
/// do nothing about any of them. Their texts are no longer injected; `doctor`
/// and `lookup` have them. `None` when the project has none open.
///
/// OVER THE OWNER'S BUDGET (step 16, B6): when more of THOR's own points of
/// the project wait on the owner (`model::store::owner_queue`) than
/// `model::store::OPEN_POINTS_OWNER_BUDGET`, the line is preceded by the
/// banner that says so. Nothing is refused over it. The call site never
/// shows this to a subagent.
fn open_points_session_start_note(store: &thor_core::event_store::EventStore, project: Option<&str>, line_is_due: &dyn Fn(&str) -> bool) -> Option<String> {
    let points = model::store::open_points(store, project);
    if points.is_empty() {
        return None;
    }
    let queue = project.map(|p| model::store::owner_queue(store, p)).unwrap_or_default();
    let mut lines: Vec<String> = project.and_then(|p| model::store::owner_queue_banner(p, queue.len())).into_iter().collect();
    // The count line is shown once a day per project (`serve::notices`); the
    // banner above it, when the owner's budget is over, is shown every time.
    if line_is_due(project.unwrap_or("")) {
        lines.push(format!(
            "[THOR] {} open point(s) in this project are the owner's to read, not a task for this session (doctor lists them).",
            points.len()
        ));
    }
    (!lines.is_empty()).then(|| lines.join("\n"))
}

/// A fact tagged this way is written for the owner's interactive chat (he
/// orchestrates through background agents, wants an answer before any agent,
/// pastes prompts) and is kept out of the session start and the prompt
/// injection of a headless session, such as `claude -p` (step 31). Field test
/// 6 (2026-10-02): a headless run was served such facts, delegated everything
/// to a background agent that could get no shell permission, and delivered
/// nothing. Facts that fire at a tool call or a file are not affected.
pub const INTERACTIVE_ONLY_TAG: &str = "interactive-only";

/// Whether a Claude Code entrypoint is a headless one. Claude Code names how
/// it was started in `CLAUDE_CODE_ENTRYPOINT` (and in every transcript line):
/// the owner's chats read `claude-desktop` or `cli`, `claude -p` reads
/// `sdk-cli`, the SDKs `sdk-ts` and `sdk-py`. Every `sdk...` one is headless.
pub fn entrypoint_is_headless(entrypoint: &str) -> bool {
    entrypoint.trim().to_ascii_lowercase().starts_with("sdk")
}

/// The entrypoint a transcript names, read from the head of its text.
pub fn entrypoint_in_transcript_head(head: &str) -> Option<String> {
    let at = head.find("\"entrypoint\":\"")? + "\"entrypoint\":\"".len();
    let value: String = head[at..].chars().take_while(|c| *c != '"').collect();
    (!value.is_empty()).then_some(value)
}

/// `items` without the interactive-only facts when the session is headless;
/// untouched otherwise.
pub fn for_session_kind(items: Vec<RankedItem>, headless: bool) -> Vec<RankedItem> {
    if !headless {
        return items;
    }
    items.into_iter().filter(|r| !r.item.tags.iter().any(|t| t == INTERACTIVE_ONLY_TAG)).collect()
}

/// What a headless session start left out, by ids.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct LeftOut {
    pub interactive_only: Vec<String>,
    pub answer_only: Vec<String>,
    /// Global facts a headless start cut to `HEADLESS_GLOBAL_CAP` (step 33).
    pub global_capped: Vec<String>,
}

/// The most global facts (no project) a headless session's start block carries.
/// This project's own facts are never counted against it. Field test 8: a small
/// project has few facts of its own, and the global "always" facts about
/// previews, browser panes and other areas filled the rest of a block for a
/// task they had nothing to do with. Chosen, not measured: eight is room for
/// the irreversible and costly rules of the whole house.
pub const HEADLESS_GLOBAL_CAP: usize = 8;

/// Whether the only thing besides `Always` that a fact is bound to is the
/// moment of answering or of claiming something done. Those moments fire
/// nowhere on their own, so such a fact reaches a session only through its
/// `Always`, and what it says is about the reply, which the Stop guard watches.
fn bound_only_to_the_reply(item: &model::item::Item) -> bool {
    use intent::Action::{Answer, ClaimDone};
    let others: Vec<&Binding> = item.bindings.iter().filter(|b| !matches!(b, Binding::Always)).collect();
    !others.is_empty() && others.iter().all(|b| matches!(b, Binding::Moment(Answer | ClaimDone)))
}

/// The start block's own filter (step 32): [`for_session_kind`], plus - for a
/// headless session - the facts bound only to the reply and (step 33) the global
/// facts past `HEADLESS_GLOBAL_CAP`, and the ids of what was left out, so the volume record can say what a headless start did not
/// carry. An interactive session is served everything.
pub fn split_for_session_start(items: Vec<RankedItem>, headless: bool) -> (Vec<RankedItem>, LeftOut) {
    let mut left_out = LeftOut::default();
    if !headless {
        return (items, left_out);
    }
    let mut kept = Vec::with_capacity(items.len());
    for r in items {
        if r.item.tags.iter().any(|t| t == INTERACTIVE_ONLY_TAG) {
            left_out.interactive_only.push(r.id);
        } else if bound_only_to_the_reply(&r.item) {
            left_out.answer_only.push(r.id);
        } else {
            kept.push(r);
        }
    }
    // Of the global facts the heaviest stay (irreversible, costly, house style,
    // then the unweighted), in the order they came; this project's own all stay.
    let mut global: Vec<usize> = (0..kept.len()).filter(|&i| kept[i].item.project.is_none()).collect();
    if global.len() > HEADLESS_GLOBAL_CAP {
        global.sort_by_key(|&i| (kept[i].item.severity.map_or(3u8, |s| s as u8), i));
        let dropped: std::collections::BTreeSet<usize> = global[HEADLESS_GLOBAL_CAP..].iter().copied().collect();
        let mut index = 0;
        kept.retain(|r| {
            let keep = !dropped.contains(&index);
            index += 1;
            if !keep {
                left_out.global_capped.push(r.id.clone());
            }
            keep
        });
    }
    (kept, left_out)
}

/// Appended after the standing-rules block exactly once: right after Claude
/// Code has just summarized the conversation. A replay on two real sessions
/// found the one real miss was not a "tested" claim made carelessly, but one
/// made right after a `/compact` with nothing re-checked - the summary reads
/// like a fact but is a paraphrase, and paraphrases lose the difference
/// between "I ran this" and "the summary says I ran this". This never
/// blocks; it is additive text on the one surface that already fires at the
/// top of a session, addressed to the assistant rather than framed as one of
/// the owner's own stored rules (see `render::FRAMING_LINE`'s doc comment
/// for that other, narrower framing problem - this text is THOR's own,
/// not the owner's, so it does not need it).
pub use model::item::COMPACT_REMINDER;

/// `Some(COMPACT_REMINDER)` exactly when this SessionStart payload is the
/// main session's own (`is_subagent` false) and its `source` field reads
/// `"compact"`; `None` for every other source ("startup", "resume",
/// "clear", absent, or anything else) and `None` for a subagent payload
/// regardless of source.
///
/// The subagent check is defense in depth, not a live gate: Claude Code's
/// `SessionStart` never actually fires inside a subagent at all (confirmed
/// 2026-08-05, see `payload_is_from_a_subagent`'s own doc comment in
/// `bin/serve.rs`, and `serve/tests/subagent_hook_behavior.rs` for the dead
/// code this same fact already retired once). Kept here anyway because this
/// function is a cheap, pure predicate that has no other way to prove its
/// own "never for a subagent" half except by being handed one - the same
/// reasoning `hook_once`'s other gates already apply at each of their own
/// call sites (see the module-level rule against a shared early return).
pub fn compact_reminder(source: Option<&str>, is_subagent: bool) -> Option<String> {
    if is_subagent || source != Some("compact") {
        return None;
    }
    Some(COMPACT_REMINDER.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use intent::Action;
    use model::item::{Item, Kind, TargetKind};

    fn item(id: &str, kind: Kind, bindings: Vec<Binding>, project: Option<&str>) -> LiveItem {
        LiveItem {
            id: id.to_string(),
            item: Item {
                id: id.to_string(),
                kind,
                text: format!("rule {id}"),
                bindings,
                severity: None,
                project: project.map(str::to_string),
                tags: vec![],
                expires: None,
                key: None,
                falsifier: None,
                check: None,
            },
        }
    }

    #[test]
    fn a_global_always_item_is_served_regardless_of_the_current_project() {
        let c = item("g1", Kind::Rule, vec![Binding::Always], None);
        let hits = select(&[c], Some("thor2"));
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn an_always_item_for_the_current_project_is_served() {
        let c = item("p1", Kind::Rule, vec![Binding::Always], Some("thor2"));
        let hits = select(&[c], Some("thor2"));
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn an_always_item_scoped_to_a_different_project_is_never_served() {
        // Named after the defect it prevents: session start must never leak
        // another project's standing rules into this one's session.
        let c = item("other1", Kind::Rule, vec![Binding::Always], Some("some-other-project"));
        let hits = select(&[c], Some("thor2"));
        assert!(hits.is_empty(), "a different project's Always item must never appear here");
    }

    #[test]
    fn with_no_current_project_only_global_always_items_are_served() {
        let global = item("g2", Kind::Rule, vec![Binding::Always], None);
        let scoped = item("p2", Kind::Rule, vec![Binding::Always], Some("thor2"));
        let hits = select(&[global, scoped], None);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, "g2");
    }

    #[test]
    fn an_item_bound_only_to_a_moment_is_never_served_here() {
        // The five surfaces never take each other's places: a Moment/Target
        // binding is surface 2/3's door, never surface 1's.
        let c = item("m1", Kind::Rule, vec![Binding::Moment(Action::Push)], None);
        let hits = select(&[c], None);
        assert!(hits.is_empty(), "a Moment-only binding must never be served by session start");
    }

    #[test]
    fn an_item_bound_only_to_a_target_is_never_served_here() {
        let c = item(
            "t1",
            Kind::Rule,
            vec![Binding::Target { kind: TargetKind::Path, value: "src/main.rs".to_string() }],
            None,
        );
        let hits = select(&[c], None);
        assert!(hits.is_empty(), "a Target-only binding must never be served by session start");
    }

    #[test]
    fn a_report_bound_always_never_reaches_session_start_even_bypassing_the_write_gate() {
        // The write gate already refuses a Report with a binding; this
        // constructs one directly (as if that check had never run) to prove
        // this surface is a second, independent lock on the door by KIND -
        // mirroring rank::rs's own `a_report_can_never_reach_a_gate_even_bypassing_the_write_gate`.
        let c = item("r1", Kind::Report, vec![Binding::Always], None);
        let hits = select(&[c], None);
        assert!(hits.is_empty(), "a Report must never be selected, gate or no gate");
    }

    #[test]
    fn a_chunk_bound_always_never_reaches_session_start_even_bypassing_the_write_gate() {
        let c = item("k1", Kind::Chunk, vec![Binding::Always], None);
        let hits = select(&[c], None);
        assert!(hits.is_empty(), "a Chunk must never be selected, gate or no gate");
    }

    #[test]
    fn session_start_never_caps_even_past_the_moment_surfaces_four_item_limit() {
        // "volledig, niet afgekapt" (CONTRACT.md): render::MAX_ITEMS (4) must
        // not apply here at all - ten items must all come back.
        let items: Vec<LiveItem> =
            (0..10).map(|i| item(&format!("i{i}"), Kind::Rule, vec![Binding::Always], None)).collect();
        let hits = select(&items, None);
        assert_eq!(hits.len(), 10, "session start must never cap, unlike the moment/prompt surfaces");
        let block = render(&hits).unwrap();
        for i in 0..10 {
            assert!(block.contains(&format!("rule i{i}")), "block must contain item i{i} verbatim: {block}");
        }
    }

    #[test]
    fn no_always_items_renders_no_block() {
        assert!(render(&[]).is_none());
    }

    // ------------------------------------------------------------- framing

    /// The defect this guards against: a standing rule is written as a
    /// constraint on the OWNER's own behaviour ("never do X"), because that
    /// is what a rule is FOR - a blank subagent (no project, no prior turns
    /// to place it in) has no way to tell that apart from an actual task
    /// instruction. Same requirement as `render::render_text`'s own guard
    /// test; this surface must carry it too. See INJECTION-FRAMING.md for the
    /// incident and `render::FRAMING_LINE`'s own doc comment for the argument.
    #[test]
    fn the_pushed_block_always_opens_with_the_framing_line() {
        let c = item("g1", Kind::Rule, vec![Binding::Always], None);
        let block = render(&select(&[c], None)).unwrap();
        assert!(
            block.starts_with(crate::render::FRAMING_LINE),
            "session start must open with the same framing line as every other surface: {block}"
        );
        let second_line = block.lines().nth(1).unwrap_or("");
        assert_eq!(second_line, "Standing rules for this session:", "{block}");
    }

    // ---------------------------------------------------------- falsifier

    /// The defect this guards against: a second line per item on the surface
    /// that fires at EVERY session start. Measured on the real store it took
    /// the standing-rules block from about 1900 to 3700 characters, for a
    /// benefit nothing has measured - in a system rebuilt because its blocks
    /// were too noisy. The falsifier's value is at write time and on `get`;
    /// see `render`'s own doc comment for the whole argument.
    #[test]
    fn a_falsifier_never_rides_along_on_the_pushed_block() {
        let mut c = item("f1", Kind::Rule, vec![Binding::Always], None);
        c.item.falsifier = Some("this stops holding once the store is retired".to_string());
        let hits = select(&[c], None);
        let block = render(&hits).unwrap();
        assert!(
            !block.contains("falsified by"),
            "an injection surface must not carry the falsifier: {block}"
        );
        assert!(
            !block.contains("this stops holding once the store is retired"),
            "and not its text under any other wording either: {block}"
        );
    }

    /// The item itself still renders whole, falsifier or not - the field is
    /// invisible here, never a reason to drop or shorten a rule.
    #[test]
    fn the_rule_itself_is_rendered_whole_either_way() {
        let mut with = item("f1", Kind::Rule, vec![Binding::Always], None);
        with.item.falsifier = Some("some observation".to_string());
        let without = item("f2", Kind::Rule, vec![Binding::Always], None);
        assert_eq!(without.item.falsifier, None, "fixture sanity");

        let with_text = with.item.text.clone();
        let without_text = without.item.text.clone();
        let a = render(&select(&[with], None)).unwrap();
        let b = render(&select(&[without], None)).unwrap();
        assert!(a.contains(&with_text), "the rule text must survive: {a}");
        assert!(b.contains(&without_text), "the rule text must survive: {b}");
        assert_eq!(a.lines().count(), b.lines().count(), "same shape either way");
    }

    // -------------------------------------------------------------------- id

    /// THE DEFECT THIS PREVENTS: a served fact carried no id at all, so an
    /// assistant that noticed the fact was wrong had no way to correct it
    /// directly - it had to guess search terms to find the item again first,
    /// turning a one-call `revise` into three steps. Every shown item now
    /// carries its id. Same defect `render::render_text` already closed for
    /// the moment/prompt surfaces; this closes it here too.
    #[test]
    fn a_session_start_item_shows_its_id() {
        let c = item("g1", Kind::Rule, vec![Binding::Always], None);
        let block = render(&select(&[c], None)).unwrap();
        assert!(block.contains("g1"), "the block must carry the item's id: {block}");
    }

    /// Showing the id somewhere is not enough - it has to come out whole and
    /// cleanly delimited, so an assistant can lift it out and pass it
    /// straight to `revise`'s own `id` argument with no editing. Uses an id
    /// shaped like a real one (a project prefix plus a colon, matching
    /// `render.rs`'s own test of the same name) to prove the bracket, not
    /// the id's own content, is what an assistant would split on.
    #[test]
    fn the_shown_id_is_in_a_form_that_can_be_passed_straight_to_revise() {
        let c = item("thor2:01ARZ3NDEKTSV4RRFFQ69G5FAV", Kind::Rule, vec![Binding::Always], None);
        let block = render(&select(&[c], None)).unwrap();
        assert!(
            block.contains("[thor2:01ARZ3NDEKTSV4RRFFQ69G5FAV]"),
            "the id must appear whole, inside brackets, exactly as `revise` expects it: {block}"
        );
    }

    /// THE CRITICAL DEFECT THIS PREVENTS: the id riding in on the item's own
    /// text, so adding it silently shrinks how much of the fact's body is
    /// delivered compared to before. This surface never truncates to begin
    /// with (see `render`'s own doc comment: no `render::cap` in this path
    /// at all, unlike the moment/prompt surfaces), so the id can only ever
    /// be additive here - proven directly with a long body, distinct from
    /// the id's own characters so a dropped body character could never hide
    /// inside it.
    #[test]
    fn adding_the_id_never_shrinks_how_much_of_the_body_is_delivered() {
        let mut c = item("i0", Kind::Rule, vec![Binding::Always], None);
        c.item.text = "y".repeat(300);
        let long_text = c.item.text.clone();
        let block = render(&select(&[c], None)).unwrap();
        assert!(block.contains(&long_text), "the full 300-char body must still appear unmodified: {block}");
        assert!(block.contains("[i0]"), "and the id must appear alongside it, not instead of any of it: {block}");
    }

    // --------------------------------------------------- compact reminder

    #[test]
    fn a_compact_source_in_the_main_session_returns_the_reminder() {
        assert_eq!(compact_reminder(Some("compact"), false).as_deref(), Some(COMPACT_REMINDER));
    }

    #[test]
    fn startup_resume_and_clear_sources_return_nothing() {
        for source in ["startup", "resume", "clear"] {
            assert!(
                compact_reminder(Some(source), false).is_none(),
                "source {source} must never return the reminder"
            );
        }
    }

    #[test]
    fn a_missing_source_returns_nothing() {
        assert!(compact_reminder(None, false).is_none());
    }

    #[test]
    fn an_unrecognised_source_returns_nothing() {
        assert!(compact_reminder(Some("something-future-claude-code-adds"), false).is_none());
    }

    #[test]
    fn a_subagent_payload_never_returns_the_reminder_even_with_source_compact() {
        assert!(compact_reminder(Some("compact"), true).is_none());
    }

    #[test]
    fn the_heaviest_standing_rules_come_first_and_none_is_left_out() {
        use model::item::Severity;
        let mut light = item("a-light", Kind::Orientation, vec![Binding::Always], None);
        let mut costly = item("b-costly", Kind::Rule, vec![Binding::Always], None);
        let mut hard = item("c-hard", Kind::Rule, vec![Binding::Always], None);
        let plain = item("d-plain", Kind::Orientation, vec![Binding::Always], None);
        costly.item.severity = Some(Severity::Costly);
        hard.item.severity = Some(Severity::Irreversible);
        light.item.severity = Some(Severity::HouseStyle);
        let items = select(&[light, costly, hard, plain], None);
        let block = render(&items).unwrap();
        let at = |id: &str| block.find(&format!("[{id}]")).unwrap_or_else(|| panic!("{id} missing: {block}"));
        assert!(at("c-hard") < at("b-costly") && at("b-costly") < at("a-light") && at("a-light") < at("d-plain"), "{block}");
    }

    // ------------------------------------------------ the size limit (step 29)

    fn many(n: usize, len: usize) -> Vec<LiveItem> {
        (0..n)
            .map(|i| {
                let mut c = item(&format!("id-{i:03}"), Kind::Rule, vec![Binding::Always], None);
                c.item.text = format!("{i:03} {}", "w".repeat(len));
                c
            })
            .collect()
    }

    /// THE FIELD CASE (2026-10-01): 53 rules, 15.6 KB. Claude Code showed the
    /// agent 2 KB of it while THOR counted all 53 as shown.
    #[test]
    fn a_block_over_the_limit_keeps_whole_rules_only_and_says_how_many_were_left_out() {
        let items = select(&many(53, 280), None);
        let r = render_within(&items, HOOK_OUTPUT_LIMIT_BYTES).unwrap();
        assert!(r.text.len() <= HOOK_OUTPUT_LIMIT_BYTES, "{} bytes", r.text.len());
        assert_eq!(r.shown.len() + r.hidden, 53);
        assert!(r.hidden > 0 && !r.shown.is_empty());
        assert!(r.text.contains(&format!("{} more standing rule(s) did not fit", r.hidden)), "{}", r.text);
        for id in &r.shown {
            assert!(r.text.contains(&format!("[{id}]")), "a shown id must be in the text: {id}");
        }
        let in_text = r.text.lines().filter(|l| l.starts_with("- [")).count();
        assert_eq!(in_text, r.shown.len(), "what is reported shown is exactly what is in the block");
    }

    /// One rule more than fits: the cut is never silent and never over budget,
    /// at every size around the edge.
    #[test]
    fn one_rule_more_than_fits_is_cut_with_the_note_and_stays_within_the_budget() {
        for len in 150..=330 {
            let items = select(&many(40, len), None);
            let r = render_within(&items, HOOK_OUTPUT_LIMIT_BYTES).unwrap();
            assert!(r.text.len() <= HOOK_OUTPUT_LIMIT_BYTES, "len {len}: {} bytes", r.text.len());
            assert_eq!(r.text.contains("did not fit"), r.hidden > 0, "len {len}: a cut always says so, a full block never does");
            assert_eq!(r.shown.len() + r.hidden, 40);
        }
    }

    /// The reserve for the note is the note's real size, not a guess: a block
    /// that must be cut leaves no more than about one rule of the budget empty.
    #[test]
    fn the_cut_block_keeps_no_more_spare_room_than_the_note_needs() {
        let items = select(&many(120, 100), None);
        let r = render_within(&items, HOOK_OUTPUT_LIMIT_BYTES).unwrap();
        assert!(r.hidden > 0);
        let spare = HOOK_OUTPUT_LIMIT_BYTES - r.text.len();
        assert!(spare < 110, "{spare} bytes of the budget were left empty");
    }

    #[test]
    fn the_hook_budget_stays_under_the_limit_claude_code_shows_whole() {
        assert!(HOOK_OUTPUT_LIMIT_BYTES > 9_500);
        assert!(HOOK_OUTPUT_LIMIT_BYTES <= 9_800, "10,000 characters is the line; 9,972 is the largest seen whole");
    }

    /// The write gate (`model::store`) counts the block's bytes from numbers in
    /// `model::item`; they must equal what this module really builds, or a pin
    /// would be refused or let through on a wrong count.
    #[test]
    fn the_write_gates_byte_count_is_the_blocks_real_size() {
        assert_eq!(model::item::FRAMING_LINE_BYTES, crate::render::FRAMING_LINE.len());
        let items = select(&many(7, 123), None);
        let r = render(&items).unwrap();
        let counted = model::item::standing_head_bytes()
            + items.iter().map(|i| model::item::standing_line(&i.id, &i.item.text).len() + 1).sum::<usize>();
        assert_eq!(counted, r.len() + 1, "one newline per line, the last one included");
    }

    #[test]
    fn a_block_that_fits_is_unchanged_and_has_no_note() {
        let items = select(&many(5, 100), None);
        let r = render_within(&items, HOOK_OUTPUT_LIMIT_BYTES).unwrap();
        assert_eq!(r.hidden, 0);
        assert_eq!(r.shown.len(), 5);
        assert_eq!(r.text, render(&items).unwrap());
        assert!(!r.text.contains("did not fit"));
    }

    #[test]
    fn the_heaviest_rules_are_the_ones_that_survive_the_limit() {
        use model::item::Severity;
        let mut all = many(40, 280);
        all[39].item.severity = Some(Severity::Irreversible);
        let items = select(&all, None);
        let r = render_within(&items, 3_000).unwrap();
        assert_eq!(r.shown.first().map(String::as_str), Some("id-039"), "the irreversible rule must come first and fit");
    }

    #[test]
    fn this_projects_own_rules_come_before_global_ones_of_the_same_weight() {
        let global = item("a-global", Kind::Rule, vec![Binding::Always], None);
        let own = item("z-own", Kind::Rule, vec![Binding::Always], Some("thor2"));
        let items = select(&[global, own], Some("thor2"));
        let r = render_within(&items, HOOK_OUTPUT_LIMIT_BYTES).unwrap();
        assert_eq!(r.shown, vec!["z-own".to_string(), "a-global".to_string()]);
    }

    // ------------------------------------------------ the ranking (step 32)

    fn standing(id: &str, project: Option<&str>, severity: Option<model::item::Severity>) -> LiveItem {
        let mut c = item(id, Kind::Rule, vec![Binding::Always], project);
        c.item.severity = severity;
        c
    }

    fn order_of(items: &[LiveItem], project: Option<&str>, recent: &[&str]) -> Vec<String> {
        use std::collections::BTreeSet;
        let recent: BTreeSet<String> = recent.iter().map(|s| s.to_string()).collect();
        render_within_ranked(&select(items, project), HOOK_OUTPUT_LIMIT_BYTES, &recent).unwrap().shown
    }

    /// THE FIELD CASE (field test 7): about 60 percent of a code task's start
    /// block was global style rules and other areas. This project's own facts
    /// come first, then heavy rules, then house style, then facts with no project
    /// and no weight at all.
    #[test]
    fn this_projects_facts_come_first_then_heavy_then_house_style_then_the_unweighted_global_ones() {
        use model::item::Severity::{Costly, HouseStyle, Irreversible};
        let items = [
            standing("a-global-none", None, None),
            standing("b-global-house", None, Some(HouseStyle)),
            standing("c-global-costly", None, Some(Costly)),
            standing("d-own-none", Some("alpha"), None),
            standing("e-global-irreversible", None, Some(Irreversible)),
            standing("f-own-costly", Some("alpha"), Some(Costly)),
        ];
        assert_eq!(
            order_of(&items, Some("alpha"), &[]),
            ["f-own-costly", "d-own-none", "e-global-irreversible", "c-global-costly", "b-global-house", "a-global-none"]
        );
    }

    /// What this project's sessions used lately goes first inside its own place
    /// in that order, and never jumps a place.
    #[test]
    fn a_recent_showing_lifts_a_fact_only_inside_its_own_place() {
        use model::item::Severity::HouseStyle;
        let items = [
            standing("a-own", Some("alpha"), None),
            standing("b-own-used", Some("alpha"), None),
            standing("c-global-house", None, Some(HouseStyle)),
            standing("d-global-none-used", None, None),
        ];
        assert_eq!(
            order_of(&items, Some("alpha"), &["b-own-used", "d-global-none-used"]),
            ["b-own-used", "a-own", "c-global-house", "d-global-none-used"]
        );
    }

    /// A headless run has the Stop guard and no chat: a fact whose only binding
    /// besides Always is the moment of answering or of claiming done is left out
    /// of its start block, and the volume record says how many and which.
    #[test]
    fn a_headless_start_leaves_out_interactive_only_and_answer_only_facts_and_names_them() {
        use intent::Action;
        let plain = item("plain", Kind::Rule, vec![Binding::Always], None);
        let mut chat = item("chat", Kind::Rule, vec![Binding::Always], None);
        chat.item.tags = vec![INTERACTIVE_ONLY_TAG.to_string()];
        let answer = item("answer", Kind::Rule, vec![Binding::Moment(Action::Answer), Binding::Always], None);
        let done = item("done", Kind::Rule, vec![Binding::Always, Binding::Moment(Action::ClaimDone)], None);
        let mixed = item("mixed", Kind::Rule, vec![Binding::Always, Binding::Moment(Action::Answer), Binding::Moment(Action::Push)], None);
        let all = select(&[plain, chat, answer, done, mixed], None);

        let (kept, left_out) = split_for_session_start(all.clone(), true);
        let kept_ids: Vec<&str> = kept.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(kept_ids, ["plain", "mixed"]);
        assert_eq!(left_out.interactive_only, ["chat"]);
        assert_eq!(left_out.answer_only, ["answer", "done"]);

        let (kept, left_out) = split_for_session_start(all, false);
        assert_eq!(kept.len(), 5, "an interactive session is served everything");
        assert!(left_out.interactive_only.is_empty() && left_out.answer_only.is_empty());
    }

    /// Field test 8 (2026-10-02): a headless start for a small project was about
    /// 70 percent global "always" facts that had nothing to do with the task.
    /// A headless start now carries at most `HEADLESS_GLOBAL_CAP` global facts,
    /// the irreversible and costly ones first, and every one of this project's.
    #[test]
    fn a_headless_start_caps_the_global_facts_and_keeps_every_project_fact() {
        use model::item::Severity::{Costly, HouseStyle, Irreversible};
        let mut items = vec![standing("own-1", Some("alpha"), None), standing("own-2", Some("alpha"), Some(HouseStyle))];
        for n in 0..6 {
            items.push(standing(&format!("g-style-{n}"), None, Some(HouseStyle)));
        }
        for n in 0..6 {
            items.push(standing(&format!("g-none-{n}"), None, None));
        }
        items.push(standing("g-costly", None, Some(Costly)));
        items.push(standing("g-irreversible", None, Some(Irreversible)));
        let all = select(&items, Some("alpha"));
        assert_eq!(all.len(), 16);

        let (kept, left_out) = split_for_session_start(all.clone(), true);
        let ids: Vec<&str> = kept.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(kept.len(), 2 + HEADLESS_GLOBAL_CAP);
        assert!(ids.contains(&"own-1") && ids.contains(&"own-2"), "{ids:?}");
        assert!(ids.contains(&"g-irreversible") && ids.contains(&"g-costly"), "heavy ones survive the cap: {ids:?}");
        assert_eq!(left_out.global_capped.len(), 16 - 2 - HEADLESS_GLOBAL_CAP);
        assert!(left_out.global_capped.iter().all(|id| id.starts_with("g-none-") || id.starts_with("g-style-")));

        let (kept, left_out) = split_for_session_start(all, false);
        assert_eq!(kept.len(), 16, "an interactive session is not capped");
        assert!(left_out.global_capped.is_empty());
    }

    // ------------------------------------------ naming the rules that did not fit

    /// The count the "did not fit" line states, or `None` without the line.
    fn stated_hidden(text: &str) -> Option<usize> {
        let line = text.lines().find(|l| l.contains("did not fit"))?;
        line.trim_start_matches("[THOR] ").split(' ').next()?.parse().ok()
    }

    /// A few rules hidden: every id is named, the ones bound to nothing but
    /// 'always' first, and the reader is told how to open one.
    #[test]
    fn a_few_hidden_rules_are_named_with_the_always_only_ones_first() {
        let mut all = many(40, 280);
        // The lightest ids come last in block order; give two of the hidden
        // ones a second binding so they can be told apart.
        for c in all.iter_mut().filter(|c| c.id == "id-037" || c.id == "id-038") {
            c.item.bindings.push(Binding::Target { kind: TargetKind::Command, value: "git".to_string() });
        }
        let items = select(&all, None);
        let r = render_within(&items, HOOK_OUTPUT_LIMIT_BYTES).unwrap();
        assert!(r.hidden > 0 && r.hidden < 10, "fixture: a few hidden, got {}", r.hidden);
        let note = r.text.lines().last().unwrap();
        assert!(note.starts_with(&format!("[THOR] {} more standing rule(s) did not fit", r.hidden)), "{note}");
        for id in &r.hidden_ids {
            assert!(note.contains(id.as_str()), "{id} must be named: {note}");
        }
        assert!(note.contains("get tool"), "{note}");
        assert!(!note.contains("more."), "all are named, so no 'and N more': {note}");
        let first_other = r.hidden_ids.iter().position(|id| id == "id-037" || id == "id-038");
        if let Some(at) = first_other {
            let only_always_before = r.hidden_ids[..at].iter().all(|id| id != "id-037" && id != "id-038");
            assert!(only_always_before, "the always-only ones come first: {:?}", r.hidden_ids);
            assert!(note.find("bound to nothing but 'always'").unwrap() < note.find("Also not shown").unwrap(), "{note}");
        }
    }

    /// Many hidden: the ids stay inside the allowance, the rest is counted, and
    /// the block still fits.
    #[test]
    fn many_hidden_rules_are_named_up_to_the_allowance_and_the_rest_counted() {
        let items = select(&many(150, 100), None);
        let r = render_within(&items, HOOK_OUTPUT_LIMIT_BYTES).unwrap();
        assert!(r.hidden > 60, "fixture: many hidden, got {}", r.hidden);
        assert!(r.text.len() <= HOOK_OUTPUT_LIMIT_BYTES, "{} bytes", r.text.len());
        let note = r.text.lines().last().unwrap();
        let named = r.hidden_ids.iter().filter(|id| note.contains(id.as_str())).count();
        assert!(named < r.hidden, "not all fit in the allowance");
        assert!(named * "id-000".len() + named.saturating_sub(1) * 2 <= HIDDEN_IDS_ALLOWANCE, "{named} ids named");
        assert!(note.contains(&format!(" And {} more.", r.hidden - named)), "{note}");
        assert_eq!(stated_hidden(&r.text), Some(r.hidden));
        assert_eq!(r.shown.len() + r.hidden, 150);
    }

    /// A store where everything fits prints no such line.
    #[test]
    fn nothing_hidden_means_no_line_and_no_ids() {
        let items = select(&many(10, 100), None);
        let r = render_within(&items, HOOK_OUTPUT_LIMIT_BYTES).unwrap();
        assert_eq!((r.hidden, r.hidden_ids.len()), (0, 0));
        assert!(!r.text.contains("did not fit"));
    }

    /// The limit holds for every number of rules from 0 to 200, at several
    /// rule sizes, and the count the line states is the count really hidden.
    #[test]
    fn the_byte_limit_holds_for_every_number_of_rules_from_0_to_200() {
        for len in [20, 100, 280] {
            for n in 0..=200 {
                let items = select(&many(n, len), None);
                let Some(r) = render_within(&items, HOOK_OUTPUT_LIMIT_BYTES) else {
                    assert_eq!(n, 0);
                    continue;
                };
                assert!(r.text.len() <= HOOK_OUTPUT_LIMIT_BYTES, "n {n} len {len}: {} bytes", r.text.len());
                assert_eq!(r.shown.len() + r.hidden, n, "n {n} len {len}");
                assert_eq!(r.hidden_ids.len(), r.hidden);
                assert_eq!(stated_hidden(&r.text), (r.hidden > 0).then_some(r.hidden), "n {n} len {len}");
                let in_text = r.text.lines().filter(|l| l.starts_with("- [")).count();
                assert_eq!(in_text, r.shown.len(), "n {n} len {len}: shown is what is in the block");
            }
        }
    }
}
