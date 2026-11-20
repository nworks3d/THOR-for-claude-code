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
            if ["change_directory", "request_directory", "spawn_task"].iter().any(|moving| name.ends_with(moving)) {
                facts.moved_or_prepared = true;
            }
        }
    }
    facts
}
