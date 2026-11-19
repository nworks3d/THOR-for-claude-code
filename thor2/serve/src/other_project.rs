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

/// The one plain line for a session that sits in a folder of no project.
pub fn no_project_line() -> String {
    "[THOR] This session runs in a folder that belongs to no project, so what you are told here is kept together with everything else. Propose in one plain sentence a folder for this work (inside Documents, named after the subject), give it its own memory, and move this session there; prepare it so the person only clicks, and let them type nothing.".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fold_ignores_case_spaces_hyphens_and_underscores() {
        assert_eq!(fold("Kitchen Renovation"), "kitchenrenovation");
        assert_eq!(fold("kitchen-renovation_2"), "kitchenrenovation2");
    }

    #[test]
    fn the_lines_are_plain_and_name_what_to_do() {
        let other = OtherProject { project: "Kitchen".to_string(), folder: PathBuf::from("/x/Kitchen") };
        let note = prompt_note(&other);
        assert!(note.contains("Kitchen") && note.contains("/x/Kitchen") && note.contains("only clicks"), "{note}");
        let line = no_project_line();
        for jargon in ["scope", "marker", "hook", "store", "collection", "falsifier"] {
            assert!(!line.contains(jargon) && !note.contains(jargon), "{jargon}");
        }
    }
}
