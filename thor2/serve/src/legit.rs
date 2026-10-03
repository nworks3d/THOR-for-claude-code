//! The real calls in the log: what a rule's trial measures a command tooth
//! against besides the forms THOR builds itself.
//!
//! WHERE THEY COME FROM. The log only, never the caller: every distinct
//! command an item was served on (`item_served.trigger`), from sessions whose
//! id has the shape of a Claude Code session id. A call the same session had
//! refused is not a legitimate call - it is kept apart, in the refused set.
//! The named limit: only commands that fired some rule are in the log at all.
//!
//! READ THE WAY THE LIVE GUARD READS. Each call is cut into its simple
//! commands ONCE, when the corpus loads, by the one segmenter the hook uses
//! (`shell::segments`), and a literal is a hit only on token boundaries
//! (`shell::literal_in`, the live guard's own matcher): `--force` is not a
//! hit in `git push --force-with-lease`. There is no over-match class: a
//! literal inside a longer token is simply no match (D5).
//!
//! THE CLEAN FORMS ARE BUILT, NEVER TAKEN FROM HERE (K8(b)). A check must let
//! the forms that only list or only ask for help through - `git tag -l`,
//! `<anchor> --help` - and those forms come from `clean_forms`, a fixed list,
//! so nobody can plant one by running a call. A real call is only ever:
//! - listed ("reaches N real calls; would refuse K"), never a reason to drop
//!   a check (K8(a));
//! - a reason a DERIVED tooth does not force, when the check would refuse it
//!   - but only a call from another session than the one writing the rule,
//!   made before the rule's first refused write (K8(c), D6): a call planted
//!   by the writing session, or after THOR refused it once, stops nothing;
//! - counted for the majority question (K8(d)), when it is whole.
//!
//! CUT CALLS (F20). A trigger is stored cut at `deliver::TRIGGER_LIMIT`
//! characters (120 before step 30, `LEGACY_TRIGGER_LIMIT`), or as an excerpt
//! with "..." marks where it is cut. A call of exactly either limit, or with
//! marks, is marked cut and its partial end token(s) dropped. A cut call is never evidence that a literal
//! is ABSENT: it never counts as a call a check lets through, never toward
//! "refuses zero real calls", never in the majority count. It is listed as a
//! hit only when its visible part carries the literal.

use std::collections::HashMap;
use thor_core::event_store::{EventKind, EventStore};

use crate::attest::is_uuid_shaped;
use crate::deliver::{LEGACY_TRIGGER_LIMIT, TRIGGER_LIMIT};
use crate::shell::{self, Segment};

/// How many distinct calls the corpus keeps, newest first.
pub const MAX_CALLS: usize = 20_000;

/// The forms that only list, per command family: a check on one of these
/// anchors must let them through (K8(b)). Applied by word prefix: an anchor
/// gets every form here whose words start with the anchor's words.
pub const LISTING_FORMS: &[&str] = &[
    "git tag",
    "git tag -l",
    "git tag --list",
    "git branch",
    "git branch -a",
    "git branch --list",
    "git stash list",
    "git stash show",
    "git remote",
    "git remote -v",
    "git worktree list",
    "git config --get",
    "git config --list",
];

/// The help and version forms of any anchor.
pub const HELP_SUFFIXES: &[&str] = &["--help", "-h", "--version"];

/// The clean forms THOR builds for a command check on `anchor`: every listing
/// form the anchor reaches by word prefix, then `<anchor> --help`,
/// `<anchor> -h` and `<anchor> --version`. Built here, never read from the
/// log.
pub fn clean_forms(anchor: &str) -> Vec<String> {
    let words: Vec<&str> = anchor.split_whitespace().collect();
    if words.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<String> = LISTING_FORMS
        .iter()
        .filter(|form| {
            let form_words: Vec<&str> = form.split_whitespace().collect();
            form_words.len() >= words.len() && form_words[..words.len()] == words[..]
        })
        .map(|form| form.to_string())
        .collect();
    let anchor = words.join(" ");
    out.extend(HELP_SUFFIXES.iter().map(|suffix| format!("{anchor} {suffix}")));
    out
}

/// Whether `literal` is a hit in `segment`: on token boundaries, in the
/// segment's unit, its joined words or its joined head - the live guard's own
/// matcher (`shell::literal_in`). A hit or no hit; nothing in between.
pub fn classify(literal: &str, segment: &Segment) -> bool {
    shell::literal_in(segment, literal)
}

/// One distinct real call.
#[derive(Debug, Clone)]
pub struct RealCall {
    /// The call as an anchor and a literal are read in: the recorded text, or
    /// its visible part (last token dropped) when it was cut.
    pub text: String,
    /// Recorded cut (at `TRIGGER_LIMIT` characters, or at the 120 an older
    /// build kept, or as an excerpt with "..." marks).
    pub cut: bool,
    /// `text`, cut into its simple commands once, at load.
    pub segments: Vec<Segment>,
    /// Every session that made this call and did not have it refused, with
    /// the seq it first made it at, earliest first.
    pub sessions: Vec<(String, i64)>,
    /// The seq of its newest occurrence.
    pub newest: i64,
}

impl RealCall {
    /// Whether `anchor` reaches one of this call's simple commands.
    pub fn reached_by(&self, anchor: &str) -> bool {
        shell::reaches_any(anchor, &self.segments)
    }

    /// Whether `literal` is a hit in a simple command `anchor` reaches.
    pub fn hit(&self, anchor: &str, literal: &str) -> bool {
        self.segments.iter().any(|s| shell::anchor_reaches(anchor, s) && classify(literal, s))
    }

    /// The occurrence that lets this call stop a DERIVED tooth from forcing
    /// (K8(c)): the earliest one made by a session other than
    /// `writing_session`, before `first_refusal` (the seq of the rule's first
    /// refused write, when there is one). `None` when the call is cut, or
    /// when no occurrence qualifies - it was made only by the writing
    /// session, or only after THOR refused the rule once.
    pub fn counts_against_forcing(&self, writing_session: Option<&str>, first_refusal: Option<i64>) -> Option<(&str, i64)> {
        if self.cut {
            return None;
        }
        self.sessions
            .iter()
            .filter(|(session, _)| Some(session.as_str()) != writing_session)
            .find(|(_, seq)| first_refusal.is_none_or(|limit| *seq < limit))
            .map(|(session, seq)| (session.as_str(), *seq))
    }

    /// The earliest occurrence, for a receipt.
    pub fn first(&self) -> (&str, i64) {
        self.sessions.first().map(|(s, q)| (s.as_str(), *q)).unwrap_or(("", 0))
    }
}

/// A call its own session had refused: never a legitimate call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefusedCall {
    pub session: String,
    pub text: String,
    pub seq: i64,
}

/// The real calls of one log, newest first (see this module's doc comment).
#[derive(Default)]
pub struct Corpus {
    calls: Vec<RealCall>,
    refused: Vec<RefusedCall>,
    last_seq: i64,
    /// text -> session -> (first seq, newest seq).
    seen: HashMap<String, HashMap<String, (i64, i64)>>,
    /// (session, text as a trigger records it) -> seq of the refusal.
    refused_by_session: HashMap<(String, String), i64>,
    /// Each distinct text is segmented once.
    segmented: HashMap<String, Vec<Segment>>,
    /// session -> the project it last worked in, where the store's sidecar
    /// knows it (`usefulness::last_project_of_session`). A session it does not
    /// know is simply absent: its calls count for every project.
    session_projects: HashMap<String, String>,
    /// The anchor words the owner's own prompts fired items on (the place a
    /// prompt firing records: `(prompt) word, word`), lowercase. Not calls:
    /// read by `dead_anchor`, which asks whether a phrase ever fired from the
    /// prompt surface.
    prompted: std::collections::HashSet<String>,
}

/// `text` as a served trigger records it: at most `TRIGGER_LIMIT` characters.
fn as_trigger(text: &str) -> String {
    text.chars().take(TRIGGER_LIMIT).collect()
}

/// The visible part of a recorded trigger, and whether it was cut. A cut
/// trigger is one of exactly `TRIGGER_LIMIT` characters, one of exactly the
/// 120 an older build kept, or an excerpt with "..." marks; the partial token
/// at each cut end is dropped (a mark at the start drops the first token, a
/// mark or a hard cut at the end drops the last).
fn visible(text: &str) -> (String, bool) {
    let count = text.chars().count();
    let marked_start = text.starts_with("...");
    let marked_end = text.ends_with("...");
    let hard_cut = count == LEGACY_TRIGGER_LIMIT || count >= TRIGGER_LIMIT;
    if !marked_start && !marked_end && !hard_cut {
        return (text.to_string(), false);
    }
    let mut body = text;
    if marked_start {
        let rest = body.trim_start_matches("...");
        body = match rest.find(char::is_whitespace) {
            Some(at) => &rest[at..],
            None => "",
        };
    }
    if marked_end {
        body = body.trim_end_matches("...");
    }
    if marked_end || hard_cut {
        let trimmed = body.trim_end();
        let cut_at = trimmed.rfind(char::is_whitespace).unwrap_or(0);
        body = &trimmed[..cut_at];
    }
    (body.trim().to_string(), true)
}

impl Corpus {
    /// The corpus of `store`'s whole log.
    pub fn load(store: &EventStore) -> Corpus {
        let mut corpus = Corpus::default();
        corpus.refresh(store);
        corpus
    }

    /// Read what the log gained since the last seq seen, and rebuild the
    /// list. Fails open to what was already loaded: an unreadable log adds
    /// nothing.
    pub fn refresh(&mut self, store: &EventStore) {
        let Ok(events) = store.events_of_kinds(&[EventKind::ItemServed, EventKind::GateRefused], self.last_seq) else {
            return;
        };
        for e in &events {
            self.last_seq = self.last_seq.max(e.seq);
            if !is_uuid_shaped(&e.session_id) {
                continue;
            }
            let body = serde_json::from_str::<serde_json::Value>(&e.body).unwrap_or(serde_json::Value::Null);
            match e.kind {
                EventKind::ItemServed => {
                    let Some(trigger) = body.get("trigger").and_then(|t| t.as_str()) else { continue };
                    if trigger.trim().is_empty() || trigger == "session start" {
                        continue;
                    }
                    if let Some(words) = trigger.strip_prefix("(prompt)") {
                        self.prompted.extend(words.split(',').map(|w| w.trim().to_lowercase()).filter(|w| !w.is_empty()));
                    }
                    let slot = self.seen.entry(trigger.to_string()).or_default().entry(e.session_id.clone()).or_insert((e.seq, e.seq));
                    slot.0 = slot.0.min(e.seq);
                    slot.1 = slot.1.max(e.seq);
                }
                EventKind::GateRefused if e.actor == model::store::ASSISTANT_DELIVERY_ACTOR => {
                    let Some(target) = body.get("target").and_then(|t| t.as_str()) else { continue };
                    // Both cuts: an event an older build wrote holds the call cut at 120.
                    self.refused_by_session.entry((e.session_id.clone(), as_trigger(target))).or_insert(e.seq);
                    let legacy: String = target.chars().take(LEGACY_TRIGGER_LIMIT).collect();
                    self.refused_by_session.entry((e.session_id.clone(), legacy)).or_insert(e.seq);
                }
                _ => {}
            }
        }
        self.learn_session_projects(store);
        self.rebuild();
    }

    /// Where each session of the log worked, from the sidecar beside the store.
    /// The log keeps no folder for a call, so this is the one answer to "whose
    /// project was it made in" - the project the session LAST worked in
    /// (step 29). Best effort: no sidecar, no answer.
    fn learn_session_projects(&mut self, store: &EventStore) {
        let Some(db) = store.path() else { return };
        let state = crate::usefulness::read_eval_debt_state(&db);
        if state.is_empty() {
            return;
        }
        let sessions: Vec<String> = self.seen.values().flat_map(|m| m.keys().cloned()).collect();
        for session in sessions {
            if self.session_projects.contains_key(&session) {
                continue;
            }
            if let Some(project) = crate::usefulness::newest_project_of_session(&state, &session) {
                self.session_projects.insert(session, project.to_string());
            }
        }
    }

    /// Whether every session that made `call` is known to have last worked in a
    /// project other than `project`: a rule of `project` is then not what the
    /// hook would have applied to it, and the trial must not count it as one
    /// that rule refuses (field 2026-10-02: 57 pushes made in two other
    /// repositories counted against a project's push ban). A session this
    /// corpus knows nothing about keeps its calls counted.
    pub fn made_elsewhere(&self, call: &RealCall, project: &str) -> bool {
        let own = model::normalize::normalize_project(Some(project));
        !call.sessions.is_empty()
            && call.sessions.iter().all(|(session, _)| {
                self.session_projects
                    .get(session)
                    .is_some_and(|p| model::normalize::normalize_project(Some(p.as_str())) != own)
            })
    }

    fn rebuild(&mut self) {
        let mut calls: Vec<RealCall> = Vec::new();
        let mut refused: Vec<RefusedCall> = Vec::new();
        for (text, sessions) in &self.seen {
            let mut kept: Vec<(String, i64)> = Vec::new();
            let mut newest = i64::MIN;
            for (session, (first, last)) in sessions {
                match self.refused_by_session.get(&(session.clone(), text.clone())) {
                    Some(seq) => refused.push(RefusedCall { session: session.clone(), text: text.clone(), seq: *seq }),
                    None => {
                        kept.push((session.clone(), *first));
                        newest = newest.max(*last);
                    }
                }
            }
            if kept.is_empty() {
                continue;
            }
            kept.sort_by(|a, b| a.1.cmp(&b.1).then(a.0.cmp(&b.0)));
            let (shown, cut) = visible(text);
            calls.push(RealCall { text: shown, cut, segments: Vec::new(), sessions: kept, newest });
        }
        calls.sort_by(|a, b| b.newest.cmp(&a.newest).then(a.text.cmp(&b.text)));
        calls.truncate(MAX_CALLS);
        for call in &mut calls {
            call.segments = self
                .segmented
                .entry(call.text.clone())
                .or_insert_with(|| shell::segments(&call.text, shell::Dialect::Sh))
                .clone();
        }
        refused.sort_by(|a, b| a.seq.cmp(&b.seq).then(a.text.cmp(&b.text)));
        self.calls = calls;
        self.refused = refused;
    }

    /// Every distinct legitimate call, newest first, at most `MAX_CALLS`.
    pub fn calls(&self) -> &[RealCall] {
        &self.calls
    }

    /// The calls a session had refused itself - apart, never legitimate.
    pub fn refused(&self) -> &[RefusedCall] {
        &self.refused
    }

    /// Whether an item was ever served by the owner's own prompt on `phrase`
    /// (the anchor as a prompt names it: last path segment, lowercase).
    pub fn prompted(&self, phrase: &str) -> bool {
        self.prompted.contains(&phrase.to_lowercase())
    }

    /// The last log seq this corpus has read.
    pub fn last_seq(&self) -> i64 {
        self.last_seq
    }

    /// The calls `anchor` reaches, newest first.
    pub fn reaching<'a>(&'a self, anchor: &'a str) -> impl Iterator<Item = &'a RealCall> + 'a {
        self.calls.iter().filter(move |call| call.reached_by(anchor))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::absent_guard;
    use crate::live::LiveItem;
    use model::item::{Binding, Check, Item, Kind, TargetKind};

    const S1: &str = "0de7dc52-1b97-4b66-948f-61e4438043b2";
    const S2: &str = "373e8610-8130-490d-ac29-56aa98709fd1";

    fn served(store: &mut EventStore, session: &str, trigger: &str) -> i64 {
        crate::deliver::record_delivery_with_trigger(store, session, session, "hook", "2026-09-28T00:00:00Z", &["r".to_string()], Some(trigger));
        store.get_events_by_entity("r").unwrap().last().unwrap().seq
    }

    /// A served event the way a build before step 30 wrote it: the trigger
    /// cut hard at 120 characters.
    fn served_by_an_older_build(store: &mut EventStore, session: &str, trigger: &str) {
        let body = serde_json::json!({"served_at": "2026-09-28T00:00:00Z", "trigger": trigger}).to_string();
        store.append_event(session, session, "hook", EventKind::ItemServed, "r", None, &body).unwrap();
    }

    fn refused(store: &mut EventStore, session: &str, target: &str) {
        crate::deliver::record_gate_outcome(store, session, session, "hook", "2026-09-28T00:00:01Z", true, "r", target);
    }

    fn texts(corpus: &Corpus) -> Vec<&str> {
        corpus.calls().iter().map(|c| c.text.as_str()).collect()
    }

    /// R7a.3: a non-UUID session's calls are not in the corpus.
    #[test]
    fn only_calls_from_claude_code_sessions_are_real_calls() {
        let mut store = EventStore::in_memory().unwrap();
        served(&mut store, S1, "git push origin main");
        served(&mut store, "probe-1", "git push origin probe");
        served(&mut store, "s", "git push origin fixture");
        let corpus = Corpus::load(&store);
        assert_eq!(texts(&corpus), vec!["git push origin main"]);
    }

    /// R7a.3: a call the same session had refused is not in the clean set;
    /// it is kept apart. The same call from a session that was not refused
    /// stays a real call.
    #[test]
    fn a_call_its_own_session_had_refused_is_kept_apart() {
        let mut store = EventStore::in_memory().unwrap();
        served(&mut store, S1, "git push --force origin main");
        refused(&mut store, S1, "git push --force origin main");
        let corpus = Corpus::load(&store);
        assert!(texts(&corpus).is_empty(), "{:?}", texts(&corpus));
        assert_eq!(corpus.refused().len(), 1);
        assert_eq!(corpus.refused()[0].session, S1);

        served(&mut store, S2, "git push --force origin main");
        let mut corpus = corpus;
        corpus.refresh(&store);
        assert_eq!(texts(&corpus), vec!["git push --force origin main"]);
        assert_eq!(corpus.calls()[0].sessions.iter().map(|(s, _)| s.as_str()).collect::<Vec<_>>(), vec![S2]);
    }

    /// G7a.2: boundaries - `--force` is not a hit in `--force-with-lease`,
    /// and is one in `git push --force origin main`; a compound and an
    /// ssh-wrapped call are split into their commands.
    #[test]
    fn hits_are_whole_tokens_and_compound_calls_are_split() {
        let mut store = EventStore::in_memory().unwrap();
        served(&mut store, S1, "git push --force-with-lease origin main");
        served(&mut store, S1, "git push --force origin main");
        served(&mut store, S1, "cd sub && git push --force origin main");
        served(&mut store, S1, "ssh nas 'git push --force origin main'");
        served(&mut store, S1, "echo git push --force");
        let corpus = Corpus::load(&store);
        let by = |t: &str| corpus.calls().iter().find(|c| c.text == t).unwrap();
        assert!(!by("git push --force-with-lease origin main").hit("git push", "--force"));
        assert!(by("git push --force origin main").hit("git push", "--force"));
        assert!(by("cd sub && git push --force origin main").hit("git push", "--force"));
        assert!(by("cd sub && git push --force origin main").segments.len() >= 2);
        assert!(by("ssh nas 'git push --force origin main'").hit("git push", "--force"));
        assert!(!by("echo git push --force").reached_by("git push"), "a mention is not a call");
        assert_eq!(corpus.reaching("git push").count(), 4);
    }

    /// The listing and help forms are built by word prefix, never read from
    /// the log.
    #[test]
    fn listing_and_help_forms_apply_by_word_prefix() {
        assert_eq!(
            clean_forms("git tag"),
            vec!["git tag", "git tag -l", "git tag --list", "git tag --help", "git tag -h", "git tag --version"]
        );
        assert!(clean_forms("git").contains(&"git stash list".to_string()));
        assert!(clean_forms("git").contains(&"git remote -v".to_string()));
        assert_eq!(clean_forms("git tag -a"), vec!["git tag -a --help", "git tag -a -h", "git tag -a --version"]);
        assert_eq!(clean_forms("pip  install"), vec!["pip install --help", "pip install -h", "pip install --version"]);
        assert!(clean_forms("   ").is_empty());
    }

    /// R7a.5 (F20): a trigger cut at 120 characters loses its last token,
    /// is marked cut, and is listed as a hit only on its visible part - it
    /// never counts as a call a check lets through, never stops forcing.
    #[test]
    fn a_cut_trigger_loses_its_last_token_and_never_proves_absence() {
        let long = format!("git push origin feature/{}", "very-long-name-".repeat(40));
        let mut store = EventStore::in_memory().unwrap();
        served(&mut store, S1, &long);
        served(&mut store, S1, "git push origin main");
        let corpus = Corpus::load(&store);
        let cut = corpus.calls().iter().find(|c| c.cut).expect("the long trigger is cut");
        assert_eq!(long.chars().take(TRIGGER_LIMIT).count(), TRIGGER_LIMIT);
        assert_eq!(cut.text, "git push origin", "the last token (the cut branch name) is dropped");
        assert!(!cut.hit("git push", "--force"));
        assert_eq!(cut.counts_against_forcing(Some(S2), None), None, "a cut call never stops forcing");
        let whole = corpus.calls().iter().find(|c| !c.cut).unwrap();
        assert!(!whole.cut && whole.text == "git push origin main");
    }

    /// Field test 5 (D2): the log used to keep 120 characters of a call, so
    /// most real `ssh ... docker exec ... sh -c "..."` calls were cut and the
    /// trial could not see the literal. A call of a few hundred characters is
    /// now recorded and read whole.
    #[test]
    fn a_call_of_a_few_hundred_characters_is_kept_whole() {
        let call = format!("ssh nas 'sudo docker exec app sh -c \"{}\"'", "echo hello && ".repeat(15));
        assert!(call.chars().count() > 200 && call.chars().count() < TRIGGER_LIMIT);
        let mut store = EventStore::in_memory().unwrap();
        served(&mut store, S1, &call);
        let corpus = Corpus::load(&store);
        let found = &corpus.calls()[0];
        assert!(!found.cut, "a call under the limit is whole");
        assert_eq!(found.text, call);
        assert!(found.hit("ssh", "sh -c"));
    }

    /// An older build cut at exactly 120 characters; those events stay in the
    /// log, and a call of that length is still read as cut.
    #[test]
    fn a_trigger_an_older_build_cut_at_120_characters_is_still_cut() {
        let old: String = format!("ssh nas 'docker exec app {}", "x".repeat(200)).chars().take(120).collect();
        let mut store = EventStore::in_memory().unwrap();
        served_by_an_older_build(&mut store, S1, &old);
        let corpus = Corpus::load(&store);
        assert!(corpus.calls()[0].cut, "a 120-character trigger is a cut one");
    }

    /// An excerpt the hook wrote around an anchor carries "..." where it is
    /// cut; both partial ends are dropped and the call is marked cut.
    #[test]
    fn an_excerpt_with_marks_is_cut_at_both_ends() {
        let mut store = EventStore::in_memory().unwrap();
        served(&mut store, S1, "...ker exec app sh -c ls && echo hi th...");
        let corpus = Corpus::load(&store);
        let call = &corpus.calls()[0];
        assert!(call.cut);
        assert_eq!(call.text, "exec app sh -c ls && echo hi");
    }

    /// R7a.6 and G7a.3 (D6): a call made by the writing session, or after the
    /// rule's first refused write, never stops forcing; the same call by
    /// another session before the first refusal does, and names its session
    /// and seq.
    #[test]
    fn the_writing_session_and_calls_after_the_first_refusal_never_stop_forcing() {
        let mut store = EventStore::in_memory().unwrap();
        let s1_seq = served(&mut store, S1, "git push --force --dry-run");
        let s2_seq = served(&mut store, S2, "git push --force --dry-run");
        let corpus = Corpus::load(&store);
        let call = &corpus.calls()[0];
        assert_eq!(call.counts_against_forcing(Some(S1), None), Some((S2, s2_seq)), "another session, before any refusal");
        assert_eq!(call.counts_against_forcing(Some(S1), Some(s2_seq)), None, "made at or after the first refusal");
        assert_eq!(call.counts_against_forcing(Some(S2), Some(s2_seq)), Some((S1, s1_seq)));

        let mut only_writer = EventStore::in_memory().unwrap();
        served(&mut only_writer, S1, "git push --force --dry-run");
        let corpus = Corpus::load(&only_writer);
        assert_eq!(corpus.calls()[0].counts_against_forcing(Some(S1), None), None, "planted by the writing session");
    }

    /// The corpus reads calls with the live guard's own segmenter and
    /// matcher: for every call here, a hit through `RealCall::hit` is exactly
    /// a refusal by the command guard itself (`absent_guard::
    /// find_command_violation`, what the hook runs per segment), and the
    /// module source names `shell::segments` and `shell::literal_in` and
    /// builds no tokenizer of its own.
    #[test]
    fn the_corpus_segmenter_and_matcher_are_the_live_guards() {
        let item = Item {
            id: "r".to_string(),
            kind: Kind::Rule,
            text: "Never run `git push --force`.".to_string(),
            bindings: vec![Binding::Target { kind: TargetKind::Command, value: "git push".to_string() }],
            severity: None,
            project: None,
            tags: vec![],
            expires: None,
            key: None,
            falsifier: Some("x".to_string()),
            check: Some(Check::Forbidden { literals: vec!["--force".to_string()] }),
        };
        let one = [LiveItem { id: "r".to_string(), item }];
        let calls = [
            "git push --force-with-lease origin main",
            "git push --force origin main",
            "cd sub && git push --force origin main",
            "ssh nas 'git push --force origin main'",
            "echo git push --force",
            "git push origin main",
            "bash -c \"git push --force\"",
            "git -C ../x push --force",
        ];
        let mut store = EventStore::in_memory().unwrap();
        for c in calls {
            served(&mut store, S1, c);
        }
        let corpus = Corpus::load(&store);
        for call in corpus.calls() {
            let live = absent_guard::find_command_violation(&one, &call.text, Some(std::path::Path::new("."))).is_some();
            assert_eq!(call.hit("git push", "--force"), live, "corpus and live guard disagree on {:?}", call.text);
        }
        let source = include_str!("legit.rs");
        let code = source.split("#[cfg(test)]").next().unwrap();
        assert!(code.contains("shell::segments(") && code.contains("shell::literal_in("));
        assert!(!code.contains("split_whitespace().any("), "no matcher of its own");
    }

    /// Step 29: the log keeps no folder for a call, so a session's project comes
    /// from the sidecar beside the store. A call made only by sessions that last
    /// worked in ANOTHER project is not one this project's rule would have met.
    #[test]
    fn a_call_made_in_another_projects_session_is_not_this_projects_call() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        let mut store = EventStore::new(&db).unwrap();
        served(&mut store, S1, "git push origin main");
        served(&mut store, S2, "git push origin dev");
        let sidecar = serde_json::json!({
            "alpha": { "sessions": { S1: { "last_event_unix": 10 } } },
            "beta": { "sessions": { S2: { "last_event_unix": 10 } } },
        });
        std::fs::write(crate::usefulness::eval_debt_state_path(&db), sidecar.to_string()).unwrap();
        let corpus = Corpus::load(&store);
        let by_text = |text: &str| corpus.calls().iter().find(|c| c.text == text).unwrap();
        assert!(!corpus.made_elsewhere(by_text("git push origin main"), "alpha"));
        assert!(corpus.made_elsewhere(by_text("git push origin dev"), "alpha"));
        assert!(!corpus.made_elsewhere(by_text("git push origin dev"), "beta"));
    }

    #[test]
    fn a_session_the_sidecar_does_not_know_keeps_its_calls_counted() {
        let mut store = EventStore::in_memory().unwrap();
        served(&mut store, S1, "git push origin main");
        let corpus = Corpus::load(&store);
        assert!(!corpus.made_elsewhere(&corpus.calls()[0], "alpha"));
    }
}
