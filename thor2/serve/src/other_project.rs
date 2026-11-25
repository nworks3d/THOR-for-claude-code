//! Work that belongs elsewhere gets its own session, prepared by the assistant.
//!
//! Two plain lines THOR hands the assistant, never the person:
//!
//! - `named_in_prompt` / `prompt_note`: the person's prompt names ANOTHER
//!   project or topic folder THOR knows (case, spaces and hyphens ignored, the
//!   same name resolution `model::checkout::same_project_name` uses), and that
//!   project has a registered folder. The assistant is told to propose
//!   continuing in a session there.
//! - `no_project_line`: a session opened in a folder that belongs to no project
//!   is told to propose a folder for this work and to move the session there.
//!
//! WHY WORDS, NOT SUBSTRINGS. The prompt is cut into words and a project only
//! matches when whole consecutive words (up to five) spell its name; a project
//! called `shop` would otherwise fire on "workshop". Names shorter than four
//! letters never match, for the same reason. Both lines only ever add text to
//! what the assistant reads: they never block anything and fail silent.

use std::path::PathBuf;
use thor_core::event_store::EventStore;

/// Another project named in a prompt, with the folder it is kept in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OtherProject {
    pub project: String,
    pub folder: PathBuf,
}

/// A name with case, spaces, hyphens and underscores folded away.
fn fold(text: &str) -> String {
    text.chars().filter(|c| !c.is_whitespace() && *c != '-' && *c != '_').flat_map(char::to_lowercase).collect()
}

/// The longest run of consecutive prompt words a project name can span.
const MAX_NAME_WORDS: usize = 5;

/// A known project (not this session's own) that the prompt names, whose
/// registered folder still exists. `None` when there is none.
pub fn named_in_prompt(store: &EventStore, prompt: &str, session_project: Option<&str>) -> Option<OtherProject> {
    let words: Vec<String> = prompt.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(fold).collect();
    if words.is_empty() {
        return None;
    }
    let mut spans: Vec<String> = Vec::new();
    for start in 0..words.len() {
        let mut joined = String::new();
        for word in words.iter().skip(start).take(MAX_NAME_WORDS) {
            joined.push_str(word);
            spans.push(joined.clone());
        }
    }
    let mut best: Option<OtherProject> = None;
    for (name, _) in model::store::resolvable_scope_counts(store) {
        let key = fold(&name);
        if key.chars().count() < 4 || !spans.contains(&key) {
            continue;
        }
        if session_project.is_some_and(|own| model::checkout::same_project_name(own, &name)) {
            continue;
        }
        let Ok(folder) = model::checkout::registered_checkout(store, Some(&name)) else { continue };
        // The longest name wins: "kitchen renovation" over "kitchen".
        if best.as_ref().is_none_or(|b| fold(&b.project).len() < key.len()) {
            best = Some(OtherProject { project: name, folder });
        }
    }
    best
}

/// The one plain line for the assistant about such a prompt.
pub fn prompt_note(other: &OtherProject) -> String {
    format!(
        "[THOR] This message is about {}, which has its own folder: {}. It is not what this session works on. Say so in one plain sentence and propose continuing in a session in that folder; prepare it so the person only clicks, and let them type nothing.",
        other.project,
        other.folder.display()
    )
}

/// Is `item` a rule or note of `project` whose only binding is the project itself (a
/// target of kind project naming it)? This is how a subject's folder brings its notes
/// without pinning them: they come up at the prompts of that project that share a word
/// with them (`narrow_to_prompt`).
pub fn is_project_bound(item: &model::item::Item, project: &str) -> bool {
    use model::item::{Binding, TargetKind};
    item.kind.can_fire()
        && item.project.as_deref().is_some_and(|p| model::checkout::same_project_name(p, project))
        && !item.bindings.is_empty()
        && item.bindings.iter().all(|b| {
            matches!(b, Binding::Target { kind: TargetKind::Project, value } if model::checkout::same_project_name(value, project))
        })
}

/// Is the folder a session runs in a topic folder (a plain folder with its own marker, no git
/// repository)? Measured on a copy of a real store (2026-11-25): with every unpinned rule and
/// note of a big code project bound to it, 72% to 88% of its prompts shared a word with at
/// least one, so the project binding is read at a prompt only in topic folders.
pub fn is_topic_folder(cwd: &std::path::Path) -> bool {
    model::checkout::plain_root(cwd).is_some()
}

/// The project-bound notes of `project` that this prompt reaches: a candidate stays only when
/// the prompt shares at least one distinctive word with it (`lookup::shares_a_word`), and when
/// one stays the project target is added to `input`, so the ordinary ranking shows it (four at
/// most, once per conversation like every other note). When the prompt already typed the
/// project's name, `resolve` has added that target itself and nothing is narrowed.
pub fn narrow_to_prompt(
    candidates: Vec<crate::live::LiveItem>,
    project: &str,
    prompt: &str,
    input: &mut crate::input::ServeInput,
) -> Vec<crate::live::LiveItem> {
    use model::item::{Binding, TargetKind};
    let named = input.targets.iter().any(|(kind, value)| *kind == TargetKind::Project && model::checkout::same_project_name(value, project));
    if named || !candidates.iter().any(|c| is_project_bound(&c.item, project)) {
        return candidates;
    }
    let words = crate::lookup::distinctive_words(prompt);
    let mut kept = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        if is_project_bound(&candidate.item, project) {
            if !crate::lookup::shares_a_word(&candidate.item, &words) {
                continue;
            }
            for binding in &candidate.item.bindings {
                if let Binding::Target { kind: TargetKind::Project, value } = binding {
                    let entry = (TargetKind::Project, value.clone());
                    if !input.targets.contains(&entry) {
                        input.targets.push(entry);
                    }
                }
            }
        }
        kept.push(candidate);
    }
    kept
}

/// A project that holds live rules or notes but has no registered folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Folderless {
    pub project: String,
    pub notes: usize,
}

/// Every project with at least one live rule or note (a kind that can fire) and no
/// registered folder that still resolves to it, most notes first, then by name.
/// The spellings of one name (case, spaces, hyphens) are counted as one project.
pub fn folderless_projects(store: &EventStore) -> Vec<Folderless> {
    let mut counts: Vec<(String, usize)> = Vec::new();
    for live in crate::live::live_items(store) {
        let (true, Some(project)) = (live.item.kind.can_fire(), live.item.project.as_deref()) else { continue };
        match counts.iter_mut().find(|(name, _)| model::checkout::same_project_name(name, project)) {
            Some((_, n)) => *n += 1,
            None => counts.push((project.to_string(), 1)),
        }
    }
    let mut out: Vec<Folderless> = counts
        .into_iter()
        .filter(|(name, _)| model::checkout::registered_checkout(store, Some(name)).is_err())
        .map(|(project, notes)| Folderless { project, notes })
        .collect();
    out.sort_by(|a, b| b.notes.cmp(&a.notes).then_with(|| a.project.cmp(&b.project)));
    out
}

/// The folderless project the prompt names (the same whole-word matching as
/// `named_in_prompt`; never this session's own project; the longest name wins).
pub fn folderless_named_in_prompt(store: &EventStore, prompt: &str, session_project: Option<&str>) -> Option<Folderless> {
    let words: Vec<String> = prompt.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(fold).collect();
    let mut spans: Vec<String> = Vec::new();
    for start in 0..words.len() {
        let mut joined = String::new();
        for word in words.iter().skip(start).take(MAX_NAME_WORDS) {
            joined.push_str(word);
            spans.push(joined.clone());
        }
    }
    folderless_projects(store)
        .into_iter()
        .filter(|f| {
            let key = fold(&f.project);
            key.chars().count() >= 4
                && spans.contains(&key)
                && !session_project.is_some_and(|own| model::checkout::same_project_name(own, &f.project))
        })
        .max_by_key(|f| fold(&f.project).len())
}

/// The one plain line for the assistant about a subject that has notes and no folder.
pub fn folderless_line(project: &str, notes: usize) -> String {
    let (count, verb) = match notes {
        1 => ("1 note".to_string(), "it never comes"),
        n => (format!("{n} notes"), "they never come"),
    };
    format!(
        "[THOR] '{project}' has {count} but no folder of its own, so {verb} up by themselves. Tell the person so in one plain sentence and offer to give it a folder; they only answer yes or no. On yes: make a folder named after it and run the installer with --topic from inside it."
    )
}

/// How every no-project line begins: `session_start::fit_extras` never drops a
/// line that starts with it.
pub const NO_PROJECT_PREFIX: &str = "[THOR] This session runs in a folder that belongs to no project";

/// The words that start the hold at the end of a turn (`stop_hold_text`); the
/// plain sentence for the person is chosen by them.
pub const HOLD_MARKER: &str = "[THOR] This conversation runs in a temporary place of the app";

/// The most known project folders either text names.
pub const KNOWN_FOLDERS_MAX: usize = 10;

/// A project THOR knows on this machine, with the folder it is kept in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnownFolder {
    pub project: String,
    pub folder: PathBuf,
}

/// THE OFF SWITCH of the two asks (the line at the first prompt and the hold at
/// Stop): `{"mode": "off"}` in `no-project-ask-mode.json` beside the store, in
/// the style of `stale-guard-mode.json`. Absent, unreadable or anything else
/// means on. The session start line is never switched off by it.
pub fn ask_is_off(db: &std::path::Path) -> bool {
    let file = db.parent().unwrap_or_else(|| std::path::Path::new(".")).join("no-project-ask-mode.json");
    let Ok(text) = std::fs::read_to_string(file) else { return false };
    serde_json::from_str::<serde_json::Value>(&text)
        .ok()
        .and_then(|v| v.get("mode").and_then(|m| m.as_str()).map(|m| m.trim().eq_ignore_ascii_case("off")))
        .unwrap_or(false)
}

/// The registered working copies and topic folders (a registered folder that
/// still exists and still resolves to its project, the very check doctor makes),
/// most recently registered first, at most `max`. THOR does not guess the
/// subject: the assistant picks from this short list.
pub fn known_folders(store: &EventStore, max: usize) -> Vec<KnownFolder> {
    let machine = model::machine::current_machine_id();
    let mut found: Vec<(Option<String>, KnownFolder)> = Vec::new();
    for (name, _) in model::store::resolvable_scope_counts(store) {
        let Ok(folder) = model::checkout::registered_checkout(store, Some(&name)) else { continue };
        let at = model::store::registrations_for_machine(store, &name, &machine).pop().and_then(|r| r.at);
        found.push((at, KnownFolder { project: name, folder }));
    }
    // Newest registration first; one without a date, and then the name, after.
    found.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.project.cmp(&b.1.project)));
    found.into_iter().map(|(_, known)| known).take(max).collect()
}

fn entry(known: &KnownFolder) -> String {
    format!("{} = {}", known.project, known.folder.display().to_string().replace('\\', "/"))
}

/// The one plain line for a session that sits in a folder of no project, without
/// any known folder.
pub fn no_project_line() -> String {
    no_project_line_with(&[])
}

/// The same line with the known project folders it still has room for: never
/// longer than `model::item::NO_PROJECT_LINE_MAX_BYTES` (a session start keeps
/// that room for it before the standing rules are fitted), at most
/// `KNOWN_FOLDERS_MAX` folders, whole entries only.
pub fn no_project_line_with(folders: &[KnownFolder]) -> String {
    let base = format!("{NO_PROJECT_PREFIX}, so what is said here is not kept under a project. Pick the folder this work belongs to, or propose a new subject folder inside Documents, and ask the person one yes or no question. On yes, move this session there with request_directory, change_directory and set_session_title, or prepare a new one with spawn_task so the person only clicks.");
    let mut entries: Vec<String> = Vec::new();
    for known in folders.iter().take(KNOWN_FOLDERS_MAX) {
        entries.push(entry(known));
        if with_list(&base, &entries).len() > model::item::NO_PROJECT_LINE_MAX_BYTES {
            entries.pop();
            break;
        }
    }
    with_list(&base, &entries)
}

fn with_list(base: &str, entries: &[String]) -> String {
    if entries.is_empty() {
        return base.to_string();
    }
    format!("{base} Known folders: {}.", entries.join("; "))
}

/// The hold at the end of the first turn of a conversation in a folder of no
/// project (the app's temporary place) that was never moved.
pub fn stop_hold_text(folders: &[KnownFolder]) -> String {
    let list = if folders.is_empty() {
        "No project folder is known yet.".to_string()
    } else {
        let entries: Vec<String> = folders.iter().take(KNOWN_FOLDERS_MAX).map(entry).collect();
        format!("Known project folders, most recent first: {}.", entries.join("; "))
    };
    format!(
        "{HOLD_MARKER} and belongs to no project, so what is said here is not kept under a project and the weekly check cannot run for it. Before you finish: in one plain sentence propose ONE folder for this work and ask the person only yes or no. Pick the folder this conversation's work belongs to from the list, or, when none fits, a new subject folder inside Documents named after the subject (give a new folder its own memory as the setup text describes). {list} On yes, move this conversation there: call mcp__ccd_directory__request_directory and then mcp__ccd_directory__change_directory with the folder path, and rename it with mcp__ccd_session_mgmt__set_session_title; if the move does not happen, prepare a new conversation with mcp__ccd_session__spawn_task (cwd and title) and tell the person to click the card. On no, carry on. This is asked once; nothing is needed from the person but yes or no."
    )
}

/// What the transcript of a session says about moving it.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct TranscriptFacts {
    /// The session already called `change_directory`, `request_directory` or
    /// `spawn_task` (any tool whose name ends in one of them), in any turn.
    pub moved_or_prepared: bool,
    /// The session itself retracted the first-session setup note: it is the
    /// welcome conversation, whose proposal 3 already covers the folder.
    pub retracted_setup_note: bool,
}

/// Whether the first-session setup (the setup note, then the tidy step's note)
/// is still open: the welcome proposes the folder itself then, so no second
/// question about it is asked.
pub fn first_session_open(store: &EventStore) -> bool {
    model::store::show(store, model::store::SETUP_NOTE_ID).is_ok() || model::store::show(store, model::store::ORDER_NOTE_ID).is_ok()
}

/// Reads a transcript (Claude Code's JSONL) for `TranscriptFacts`. Lines that
/// are not JSON are skipped; nothing here does I/O.
pub fn transcript_facts(transcript_jsonl: &str) -> TranscriptFacts {
    let mut facts = TranscriptFacts::default();
    for line in transcript_jsonl.lines() {
        let Ok(entry) = serde_json::from_str::<serde_json::Value>(line.trim()) else { continue };
        if entry.get("type").and_then(|t| t.as_str()) != Some("assistant") {
            continue;
        }
        let Some(blocks) = entry.get("message").and_then(|m| m.get("content")).and_then(|c| c.as_array()) else { continue };
        for block in blocks.iter().filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("tool_use")) {
            let name = block.get("name").and_then(|n| n.as_str()).unwrap_or("");
            if name.ends_with("retract") && block.get("input").is_some_and(|i| i.to_string().contains(model::store::SETUP_NOTE_ID)) {
                facts.retracted_setup_note = true;
            }
            if ["change_directory", "request_directory", "spawn_task"].iter().any(|moving| name.ends_with(moving)) {
                facts.moved_or_prepared = true;
            }
        }
    }
    facts
}
