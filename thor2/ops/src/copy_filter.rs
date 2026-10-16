//! SELECTIVE COPY: which part of the log a copy of the memory gets, and how
//! that part is turned into a log of its own.
//!
//! A copy on a company laptop must not hold what the owner did not choose to
//! put there. So the excluded events are never written to the copy at all, and
//! the copy is a FILTERED DERIVATIVE with a hash chain of its own: the chosen
//! events are appended, in their original order, to a fresh log. Every event
//! gets the next sequence number and a hash that continues the copy's own
//! chain, which is why `doctor` can verify the copy exactly like any log.
//!
//! WHAT IS KEPT. An entity (one item and its whole history) is kept when every
//! project it was ever filed under is selected (`Selection::scopes`), and, for
//! an item that was ever general (no project), only when the selection also
//! includes the general rules. "Ever" is on purpose: an old revision of an item
//! that once belonged to an unselected project still holds that project's
//! words, so the whole item stays out. An entity with no readable item body
//! cannot be classified and stays out.
//!
//! WHAT IS CLEANED. Two kinds of events carry text from the sessions they
//! happened in, not from the item: a refusal or a stood-aside record holds the
//! command or path that was judged, and a "served" record holds the command it
//! fired at. Refusals and stood-aside records are not copied; a served record
//! is copied without its `trigger`.
//!
//! WHAT STILL REFERS TO WHAT IS GONE. An item is a plain value: it names no
//! other item by anything but words. A kept item whose text or tags mention an
//! item that was left out simply mentions it; nothing looks it up, so serving
//! never breaks. Where a hash is cited (a revision's parent, a resolve) it is
//! translated to the copy's own hash.

use anyhow::{bail, Result};
use std::collections::{BTreeSet, HashMap, HashSet};
use thor_core::cas::{parse_reproject_body, parse_resolve_body};
use thor_core::copy_mode::Selection;
use thor_core::event_store::{canonical_repr_parts, canonicalize_body, hash_event, EventKind, ShippedEvent};

/// What the first pass learns about one entity.
#[derive(Default)]
struct EntityClass {
    /// Every project the entity was ever filed under (`None` = general).
    projects: BTreeSet<Option<String>>,
    /// An item body was read for it.
    readable: bool,
    /// A creating or revising event had a body that is not an item.
    unreadable: bool,
}

/// The first pass: feed it every source event, then ask which entities to keep.
#[derive(Default)]
pub struct Classes {
    entities: HashMap<String, EntityClass>,
}

fn project_of(value: Option<&str>) -> Option<String> {
    model::normalize::normalize_project(value)
}

impl Classes {
    pub fn observe(&mut self, event: &ShippedEvent) {
        let Some(kind) = EventKind::from_str(&event.kind) else { return };
        let class = self.entities.entry(event.entity_id.clone()).or_default();
        match kind {
            EventKind::FactCreated | EventKind::FactRevised | EventKind::FactSuperseded => match model::store::parse_body(&event.body) {
                Ok(item) => {
                    class.readable = true;
                    class.projects.insert(project_of(item.project.as_deref()));
                    // The project a bare id names (`<project>:<rest>`) counts too,
                    // exactly as the log's own project fold reads it.
                    if let Some(prefix) = thor_core::repo::owner_project(&event.entity_id) {
                        class.projects.insert(project_of(Some(prefix)));
                    }
                }
                Err(_) => class.unreadable = true,
            },
            EventKind::FactReprojected => {
                if let Some(project) = parse_reproject_body(&event.body) {
                    class.projects.insert(project_of(project.as_deref()));
                }
            }
            _ => {}
        }
    }

    /// The entities the selection keeps.
    pub fn keep(&self, selection: &Selection) -> HashSet<String> {
        let wanted: HashSet<String> = selection.scopes.iter().filter_map(|s| project_of(Some(s))).collect();
        self.entities
            .iter()
            .filter(|(_, class)| class.readable && !class.unreadable)
            .filter(|(_, class)| {
                class.projects.iter().all(|project| match project {
                    None => selection.include_global,
                    Some(name) => wanted.contains(name),
                })
            })
            .map(|(id, _)| id.clone())
            .collect()
    }
}

/// The second pass: turns the kept source events into the copy's own chain.
pub struct Rewriter {
    keep: HashSet<String>,
    /// Source revision hash -> the copy's hash for the same event.
    translated: HashMap<String, String>,
    prev_hash: String,
    next_seq: i64,
}

fn replace_hashes(body: &str, translated: &HashMap<String, String>) -> String {
    let Ok(mut value) = serde_json::from_str::<serde_json::Value>(body) else { return body.to_string() };
    let swap = |v: &mut serde_json::Value| {
        if let Some(new) = v.as_str().and_then(|s| translated.get(s)) {
            *v = serde_json::Value::String(new.clone());
        }
    };
    if let Some(keep) = value.get_mut("keep_rev") {
        swap(keep);
    }
    if let Some(list) = value.get_mut("discarded").and_then(|d| d.as_array_mut()) {
        list.iter_mut().for_each(swap);
    }
    serde_json::to_string(&value).unwrap_or_else(|_| body.to_string())
}

fn without_trigger(body: &str) -> String {
    let Ok(mut value) = serde_json::from_str::<serde_json::Value>(body) else { return body.to_string() };
    if let Some(object) = value.as_object_mut() {
        object.remove("trigger");
    }
    serde_json::to_string(&value).unwrap_or_else(|_| body.to_string())
}

impl Rewriter {
    pub fn new(keep: HashSet<String>) -> Self {
        Rewriter { keep, translated: HashMap::new(), prev_hash: String::new(), next_seq: 1 }
    }

    /// The event as the copy holds it, or `None` when the copy does not.
    pub fn rewrite(&mut self, event: &ShippedEvent) -> Result<Option<ShippedEvent>> {
        if !self.keep.contains(&event.entity_id) {
            return Ok(None);
        }
        let Some(kind) = EventKind::from_str(&event.kind) else {
            bail!("unknown event kind '{}' at seq {} (this program is older than the source)", event.kind, event.seq);
        };
        let body = match kind {
            EventKind::GateRefused | EventKind::GateStoodAside => return Ok(None),
            EventKind::ItemServed => without_trigger(&event.body),
            EventKind::FactResolved if parse_resolve_body(&event.body).is_some() => replace_hashes(&event.body, &self.translated),
            _ => event.body.clone(),
        };
        let parent_rev = match event.parent_rev.as_deref().filter(|p| !p.is_empty()) {
            None => None,
            Some(parent) => match self.translated.get(parent) {
                Some(new) => Some(new.clone()),
                None if matches!(kind, EventKind::FactRevised | EventKind::FactSuperseded | EventKind::FactRetracted) => {
                    bail!("seq {} revises a revision the copy does not hold; the source is not consistent", event.seq)
                }
                None => None,
            },
        };
        let canonical = canonical_repr_parts(kind, &event.entity_id, parent_rev.as_deref(), &canonicalize_body(&body));
        let this_hash = hash_event(&self.prev_hash, &canonical);
        self.translated.insert(event.this_hash.clone(), this_hash.clone());
        self.prev_hash = this_hash.clone();
        let seq = self.next_seq;
        self.next_seq += 1;
        Ok(Some(ShippedEvent {
            seq,
            session_id: event.session_id.clone(),
            lineage_id: event.lineage_id.clone(),
            actor: event.actor.clone(),
            kind: event.kind.clone(),
            entity_id: event.entity_id.clone(),
            parent_rev,
            body,
            this_hash,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(seq: i64, kind: &str, entity: &str, body: &str) -> ShippedEvent {
        ShippedEvent {
            seq,
            session_id: "s".into(),
            lineage_id: "l".into(),
            actor: "a".into(),
            kind: kind.into(),
            entity_id: entity.into(),
            parent_rev: None,
            body: body.into(),
            this_hash: format!("src-{seq}"),
        }
    }

    fn item_body(id: &str, project: Option<&str>) -> String {
        let item = model::item::Item {
            id: id.into(),
            kind: model::item::Kind::Report,
            text: format!("text of {id}"),
            bindings: vec![],
            severity: None,
            project: project.map(str::to_string),
            tags: vec![],
            expires: None,
            key: None,
            falsifier: None,
            check: None,
        };
        serde_json::to_string(&item).unwrap()
    }

    fn classes(events: &[ShippedEvent]) -> Classes {
        let mut c = Classes::default();
        events.iter().for_each(|e| c.observe(e));
        c
    }

    fn pick(scopes: &[&str], global: bool) -> Selection {
        Selection { scopes: scopes.iter().map(|s| s.to_string()).collect(), include_global: global }
    }

    #[test]
    fn scopes_and_the_general_switch_decide_independently() {
        let events = vec![
            event(1, "fact_created", "a", &item_body("a", Some("alpha"))),
            event(2, "fact_created", "b", &item_body("b", Some("beta"))),
            event(3, "fact_created", "g", &item_body("g", None)),
        ];
        let c = classes(&events);
        let names = |s: &Selection| {
            let mut v: Vec<String> = c.keep(s).into_iter().collect();
            v.sort();
            v
        };
        assert_eq!(names(&pick(&["alpha"], false)), vec!["a"]);
        assert_eq!(names(&pick(&["alpha"], true)), vec!["a", "g"]);
        assert_eq!(names(&pick(&[], true)), vec!["g"]);
        assert_eq!(names(&pick(&["alpha", "beta"], false)), vec!["a", "b"]);
    }

    #[test]
    fn an_item_that_was_ever_in_an_unselected_project_stays_out() {
        let events = vec![
            event(1, "fact_created", "m", &item_body("m", Some("secret"))),
            event(2, "fact_revised", "m", &item_body("m", None)),
        ];
        let c = classes(&events);
        assert!(c.keep(&pick(&[], true)).is_empty(), "its first revision belonged to 'secret'");
        assert!(c.keep(&pick(&["secret"], true)).contains("m"));
    }

    #[test]
    fn a_reproject_counts_and_a_global_spelling_is_global() {
        let events = vec![
            event(1, "fact_created", "r", &item_body("r", Some("global"))),
            event(2, "fact_reprojected", "r", r#"{"project":"alpha"}"#),
        ];
        let c = classes(&events);
        assert!(c.keep(&pick(&["alpha"], true)).contains("r"));
        assert!(c.keep(&pick(&["alpha"], false)).is_empty(), "'global' is the general tier, not a project");
    }

    #[test]
    fn an_entity_with_no_readable_item_is_left_out() {
        let events = vec![
            event(1, "fact_created", "x", "not json at all"),
            event(2, "item_served", "ghost", r#"{"served_at":"t"}"#),
        ];
        let c = classes(&events);
        assert!(c.keep(&pick(&["alpha"], true)).is_empty());
    }

    #[test]
    fn the_rewritten_chain_continues_itself_and_translates_cited_hashes() {
        let created = event(1, "fact_created", "a", &item_body("a", Some("alpha")));
        let dropped = event(2, "fact_created", "b", &item_body("b", Some("beta")));
        let mut revised = event(3, "fact_revised", "a", &item_body("a", Some("alpha")));
        revised.parent_rev = Some(created.this_hash.clone());
        let c = classes(&[created.clone(), dropped.clone(), revised.clone()]);
        let mut w = Rewriter::new(c.keep(&pick(&["alpha"], false)));
        let first = w.rewrite(&created).unwrap().unwrap();
        assert!(w.rewrite(&dropped).unwrap().is_none());
        let second = w.rewrite(&revised).unwrap().unwrap();
        assert_eq!((first.seq, second.seq), (1, 2), "the copy's own numbering has no holes");
        assert_eq!(second.parent_rev.as_deref(), Some(first.this_hash.as_str()));
        assert_ne!(first.this_hash, created.this_hash, "the copy's hash is its own");
    }

    #[test]
    fn refusals_are_not_copied_and_served_records_lose_their_trigger() {
        let created = event(1, "fact_created", "a", &item_body("a", Some("alpha")));
        let refused = event(2, "gate_refused", "a", r#"{"target":"CANARY command"}"#);
        let served = event(3, "item_served", "a", r#"{"served_at":"2026-01-01T00:00:00Z","trigger":"CANARY command"}"#);
        let c = classes(std::slice::from_ref(&created));
        let mut w = Rewriter::new(c.keep(&pick(&["alpha"], false)));
        w.rewrite(&created).unwrap();
        assert!(w.rewrite(&refused).unwrap().is_none());
        let kept = w.rewrite(&served).unwrap().unwrap();
        assert!(!kept.body.contains("CANARY"), "{}", kept.body);
        assert!(kept.body.contains("served_at"));
    }

    #[test]
    fn a_revision_of_a_revision_the_copy_lacks_is_refused() {
        let mut revised = event(2, "fact_revised", "a", &item_body("a", Some("alpha")));
        revised.parent_rev = Some("never-seen".into());
        let created = event(1, "fact_created", "a", &item_body("a", Some("alpha")));
        let c = classes(&[created, revised.clone()]);
        let mut w = Rewriter::new(c.keep(&pick(&["alpha"], false)));
        assert!(w.rewrite(&revised).is_err());
    }
}
