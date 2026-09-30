//! THOR's trial of a rule: is it a rule with a tooth, or a note?
//!
//! THE TWO TRACKS (the owner's decision this serves): a rule whose tooth
//! provably works goes on the RULE track and carries its check; a rule that
//! provably cannot have one becomes a NOTE and carries none; and anything in
//! between is refused with the one fix the writer can run. Nothing here
//! guesses: every answer comes from trying the hook's own pipeline
//! (`guard_verdict::pre_tool`) on a store that holds only the rule
//! (`model::store::one_item_store`), with calls THOR builds itself, and from
//! the real calls in the log (`legit`).
//!
//! WHAT IS TRIED (`trial`, in `decide`'s order):
//! 1. UNTRYABLE - a file check with no registered working copy (K20): refused
//!    when the session stands in that project (the one fix, registering it,
//!    can run right there), else stored as a note that keeps the check
//!    PENDING in its record, for the machine to try once the project is
//!    registered. A global rule's file check, an unreadable file and a
//!    malformed dispute are refused naming their fix.
//! 2. A4 - a check whose literal the rule's own text does not name is refused
//!    when this write attaches or changes it; a write that leaves an old
//!    failure untouched (pin, unpin, tags) lands, undecided (K23). At a
//!    retest only, OLD contains stock (`model::store::old_contains_stock`,
//!    Q24) is tried on as if named: when it bites in the registered working
//!    copy it is a rule, and its record carries the fragment it guards;
//!    otherwise A4 answers exactly as before.
//! 3. FALSE NOW - a file check that does not hold in the registered checkout:
//!    a note with the check pending when it holds in the session's own
//!    checkout (a worktree or another branch), else refused.
//! 4. A COLLAPSED CHECK (its anchor carries its own literal) is a
//!    whole-command ban and is tried as one (K19).
//! 5. THE WRITER'S CHECK BITES - its forbidden form is refused and every
//!    clean form THOR builds is let through - so it is a rule, unless it
//!    would refuse more than half of at least 5 real calls it reaches
//!    (K8(d)): then the writer narrows it, or names one listed call in
//!    `accept_refusing`.
//! 6. It does not bite (no reach, or it refuses a clean form): it is not
//!    stored, and a tooth THOR builds from the rule's own text (`derive`)
//!    that bites FORCES the write to be refused naming it (8); else a note.
//! 7. No check and nothing to build one from: a note.
//!
//! A DERIVED TOOTH FORCES (`forces`) only when it bites, refuses no real call
//! of another session made before the rule's first refused write (K8(c)),
//! and the rule with it (and any binding it proposes) passes the gate and
//! has room. The writer can dispute one (K22): `legitimate_calls` for a
//! command tooth, `may_change` for a derived contains - a note by dispute,
//! listed for the owner. An earlier check on the rule's own log forces only
//! when no revise with a because removed it and the current text still names
//! its literal (D7).
//!
//! AT A RETEST (`retest`, no write) the same trial runs with no session, and
//! a refusal becomes "needs the agent", a missing working copy "blocked"
//! (`Blocker`). `apply` writes a rule or a note with its record, only while
//! the head is still the one tried.
//!
//! NEVER WRITES a file, never appends to a real store, except `apply`.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use thor_core::event_store::{Event, EventKind, EventStore};

use crate::guard_verdict;
use crate::legit::{self, Corpus, RealCall};
use crate::project::{self, RootProblem};
use crate::shell;
use model::gate;
use model::item::{Binding, Check, Item, TargetKind};
use model::store::{
    AttemptOutcome, OldContainsStock, PendingCheck, PendingWhy, RecordedVerdict, StoppedForcing, SystemActor, TrialAttempt,
    TrialCorpus, TrialDisputes, TrialEvidence, TrialFields, TrialHistory, TrialRecord, WriteError,
};

// ------------------------------------------------------------ the pipeline

/// One call THOR builds: what a reader is shown, the tool it is made as, and
/// the tool input.
#[derive(Debug, Clone, PartialEq)]
struct Call {
    shown: String,
    tool: String,
    input: Value,
}

fn bash(command: &str) -> Call {
    Call { shown: command.to_string(), tool: "Bash".to_string(), input: json!({ "command": command }) }
}

/// A rule alone in a store of its own, tried through the hook's pipeline.
struct Alone {
    store: EventStore,
    id: String,
    project: Option<String>,
}

impl Alone {
    fn of(item: &Item) -> Option<Alone> {
        let store = model::store::one_item_store(item).ok()?;
        Some(Alone { store, id: item.id.clone(), project: item.project.clone() })
    }

    /// Whether the hook's pipeline refuses `call` naming this rule - the
    /// session's project is the rule's own, and `root` is both the checkout
    /// and the working folder.
    fn refuses(&self, call: &Call, root: &Path) -> bool {
        let judged = guard_verdict::pre_tool(&self.store, &call.tool, Some(&call.input), self.project.as_deref(), Some(root), Some(root));
        judged.verdict.and_then(|v| v.rule_id).as_deref() == Some(self.id.as_str())
    }
}

/// The folder a command check is tried in: the rule's registered checkout
/// when it has one, else the store's own folder - never the process's
/// working folder, so a trial never depends on where it was run from.
fn command_root(store: &EventStore, project: Option<&str>) -> PathBuf {
    if let Ok(root) = project::registered_checkout(store, project) {
        return root;
    }
    store.path().and_then(|p| p.parent().map(Path::to_path_buf)).unwrap_or_else(std::env::temp_dir)
}

/// Whether `item`'s current check refuses the real shell call `command`,
/// tried through the hook's own pipeline - the one question field evidence
/// and the real-call replay ask of a command check. `root` is where it is
/// tried (`command_root`).
pub fn refuses_call(id: &str, item: &Item, command: &str, root: &Path) -> bool {
    let mut item = item.clone();
    item.id = id.to_string();
    Alone::of(&item).is_some_and(|alone| alone.refuses(&bash(command), root))
}

/// Whether `item`'s check refuses the call `tool`/`input`, tried in `root`
/// through the hook's own pipeline - for a reader that builds its own forms
/// (`prove`), so it never calls a guard arm directly.
pub(crate) fn pipeline_refuses(item: &Item, tool: &str, input: &Value, root: &Path) -> bool {
    let call = Call { shown: String::new(), tool: tool.to_string(), input: input.clone() };
    Alone::of(item).is_some_and(|alone| alone.refuses(&call, root))
}

/// `refuses_call`, in the folder `command_root` picks for `store`.
pub fn refuses_call_in(store: &EventStore, id: &str, item: &Item, command: &str) -> bool {
    refuses_call(id, item, command, &command_root(store, item.project.as_deref()))
}

/// The item's first Command anchor.
fn command_anchor(bindings: &[Binding]) -> Option<&str> {
    bindings.iter().find_map(|b| match b {
        Binding::Target { kind: TargetKind::Command, value } => Some(value.as_str()),
        _ => None,
    })
}

/// Whether a Command anchor names a tool rather than a shell command: one
/// word, a capital first letter (`Agent`, `WebFetch`) or an MCP tool name.
/// Such a call carries no command; the hook reads its tool name.
fn is_tool_name(anchor: &str) -> bool {
    let one_word = !anchor.trim().contains(char::is_whitespace);
    one_word && (anchor.starts_with("mcp__") || anchor.chars().next().is_some_and(|c| c.is_ascii_uppercase()))
}

/// The call a `Requires` check is tried with: its binding, carrying the
/// trigger when the trigger is a literal INSIDE the bound command rather than
/// the binding itself. A star trigger (`/thor*`) is tried with its stem and a
/// made-up continuation, `{anchor} {stem}-probe`, or the binding alone when
/// it already carries the stem.
pub(crate) fn requires_attempt(item: &Item, anchor: &str) -> String {
    match &item.check {
        Some(Check::Requires { when, .. }) if !gate::requires_trigger_names_a_binding(when, &item.bindings) => {
            let when = when.trim();
            match shell::star_stem(when) {
                Some(stem) if anchor.contains(stem) => anchor.to_string(),
                Some(stem) => format!("{anchor} {stem}-probe"),
                None if anchor.contains(when) => anchor.to_string(),
                None => format!("{anchor} {when}"),
            }
        }
        _ => anchor.to_string(),
    }
}

// ------------------------------------------------------------------ forms

/// The file a file check is tried on: its path relative to the checkout, the
/// absolute path, and its content as it stands.
#[derive(Debug, Clone)]
struct FileTried {
    rel: String,
    abs: PathBuf,
    content: String,
}

/// The forms of one check: every forbidden form must be refused and every
/// clean one let through for it to bite.
struct Forms {
    forbidden: Vec<Call>,
    clean: Vec<Call>,
    collapsed: bool,
    file: Option<FileTried>,
}

/// What trying one check showed.
#[derive(Debug, Clone)]
struct Tried {
    forbidden: Vec<(Call, bool)>,
    clean: Vec<(Call, bool)>,
    collapsed: bool,
    file: Option<FileTried>,
    /// Why no forms could be built, when none could.
    none: Option<String>,
}

impl Tried {
    fn bites(&self) -> bool {
        !self.forbidden.is_empty() && self.forbidden.iter().all(|(_, r)| *r) && self.clean.iter().all(|(_, r)| !*r)
    }

    fn reaches(&self) -> bool {
        self.forbidden.iter().any(|(_, r)| *r)
    }

    fn clean_failed(&self) -> Vec<String> {
        self.clean.iter().filter(|(_, r)| *r).map(|(c, _)| c.shown.clone()).collect()
    }

    /// "'git push --force' (refused), 'git push' (let through)".
    fn listed(&self) -> String {
        self.forbidden
            .iter()
            .chain(self.clean.iter())
            .map(|(c, r)| format!("{} ({})", c.shown, if *r { "refused" } else { "let through" }))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// The text of a file, or why it cannot be read.
fn read_text(path: &Path) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|e| e.to_string())
}

/// The file a check path names in `root`: the file itself, or - for a
/// folder - its first direct child (sorted) for which `pick` holds.
fn target_file(root: &Path, path: &str, pick: impl Fn(&str) -> bool) -> Result<FileTried, String> {
    let abs = root.join(path);
    if abs.is_file() {
        let content = read_text(&abs)?;
        return Ok(FileTried { rel: path.to_string(), abs, content });
    }
    if abs.is_dir() {
        let mut children: Vec<PathBuf> = std::fs::read_dir(&abs)
            .map_err(|e| e.to_string())?
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_file())
            .collect();
        children.sort();
        for child in children {
            let Ok(content) = read_text(&child) else { continue };
            if pick(&content) {
                let name = child.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                let rel = format!("{}/{name}", path.trim_end_matches(['/', '\\']));
                return Ok(FileTried { rel, abs: child, content });
            }
        }
        return Err(format!("no file directly inside {path} fits this check"));
    }
    Err(format!("{path} does not exist"))
}

fn write_call(file: &FileTried, content: &str, shown: String) -> Call {
    Call {
        shown,
        tool: "Write".to_string(),
        input: json!({ "file_path": file.abs.to_string_lossy(), "content": content }),
    }
}

/// The file minus its first line that overlaps no occurrence of `literal`,
/// and that line's number (1-based) - `None` when every line overlaps one
/// (F17: a literal spanning several lines keeps all of them).
fn minus_unrelated_line(content: &str, literal: &str) -> Option<(String, usize)> {
    let mut occurrences: Vec<(usize, usize)> = Vec::new();
    let mut from = 0;
    while let Some(at) = content.get(from..).and_then(|c| c.find(literal)) {
        let start = from + at;
        occurrences.push((start, start + literal.len()));
        from = start + literal.chars().next().map_or(1, char::len_utf8);
    }
    let mut start = 0;
    for (n, line) in content.split_inclusive('\n').enumerate() {
        let end = start + line.len();
        let overlaps = occurrences.iter().any(|(a, b)| *a < end && start < *b);
        if !overlaps {
            let mut out = String::with_capacity(content.len());
            out.push_str(&content[..start]);
            out.push_str(&content[end..]);
            return Some((out, n + 1));
        }
        start = end;
    }
    None
}

/// The forms of `item`'s own check (see this module's doc comment), built in
/// `root` (the registered checkout, or `command_root` for a command check).
/// `Err` says why none can be built: nothing this check can reach.
fn forms(item: &Item, root: &Path) -> Result<Forms, String> {
    let none = |why: &str| Err(why.to_string());
    match &item.check {
        None => none("this rule carries no check"),
        Some(Check::Forbidden { literals }) => {
            if let Some(anchor) = command_anchor(&item.bindings) {
                let anchor = anchor.split_whitespace().collect::<Vec<_>>().join(" ");
                let words: Vec<&str> = anchor.split(' ').collect();
                let collapsed = literals.iter().any(|l| !l.is_empty() && shell::literal_at_boundary(&anchor, l));
                if collapsed {
                    let mut clean = vec![bash(&format!("echo {anchor}"))];
                    if words.len() >= 2 {
                        clean.push(bash(&format!("{} --version", words[0])));
                    }
                    if words.len() >= 3 {
                        clean.push(bash(&words[..words.len() - 1].join(" ")));
                    }
                    return Ok(Forms { forbidden: vec![bash(&format!("{anchor} x"))], clean, collapsed: true, file: None });
                }
                let forbidden = literals.iter().map(|l| bash(&format!("{anchor} {l}"))).collect();
                let mut clean = vec![bash(&anchor)];
                clean.extend(legit::clean_forms(&anchor).iter().filter(|f| **f != anchor).map(|f| bash(f)));
                return Ok(Forms { forbidden, clean, collapsed: false, file: None });
            }
            if item.bindings.iter().any(|b| matches!(b, Binding::Always)) {
                let file = FileTried { rel: "thor-trial.txt".to_string(), abs: root.join("thor-trial.txt"), content: String::new() };
                let forbidden = literals
                    .iter()
                    .map(|l| write_call(&file, l, format!("writing '{}' into a file", gate::shown_literal(l))))
                    .collect();
                let clean = vec![write_call(&file, "", "writing an empty file".to_string())];
                return Ok(Forms { forbidden, clean, collapsed: false, file: None });
            }
            none("a forbidden check reaches a call only through a command anchor, and a written file only through an always binding - this rule has neither")
        }
        Some(Check::Requires { when, required }) => {
            let Some(anchor) = command_anchor(&item.bindings) else {
                return none("a requires check reaches a call only through a command anchor, and this rule has none");
            };
            let Some(answer) = required.first() else { return none("the required set is empty") };
            let inner = !gate::requires_trigger_names_a_binding(when, &item.bindings);
            if is_tool_name(anchor) {
                let trigger = if inner { shell::star_stem(when.trim()).map_or(when.trim().to_string(), |s| format!("{s}-probe")) } else { "trial".to_string() };
                let tool_call = |prompt: String| Call {
                    shown: format!("{anchor} with '{prompt}'"),
                    tool: anchor.to_string(),
                    input: json!({ "prompt": prompt }),
                };
                let mut clean = vec![tool_call(format!("{trigger} {answer}"))];
                if inner {
                    clean.push(tool_call("trial".to_string()));
                }
                return Ok(Forms { forbidden: vec![tool_call(trigger)], clean, collapsed: false, file: None });
            }
            let attempt = requires_attempt(item, anchor);
            let mut clean = vec![bash(&format!("{attempt} {answer}"))];
            if attempt != anchor {
                clean.push(bash(anchor));
            }
            clean.extend(legit::clean_forms(anchor).iter().map(|f| bash(f)));
            Ok(Forms { forbidden: vec![bash(&attempt)], clean, collapsed: false, file: None })
        }
        Some(Check::Absent { path, literal }) => absent_forms(root, path, std::slice::from_ref(literal)),
        Some(Check::AbsentAll { path, literals }) => absent_forms(root, path, literals),
        Some(Check::PathExists { path }) => {
            let abs = root.join(path);
            let inside = if abs.is_dir() { abs.join("thor-trial.txt") } else { abs };
            let file = FileTried { rel: path.clone(), abs: inside, content: String::new() };
            let elsewhere = FileTried { rel: String::new(), abs: root.join("thor-trial-elsewhere.txt"), content: String::new() };
            Ok(Forms {
                forbidden: vec![write_call(&file, "x", format!("a write to {path}"))],
                clean: vec![write_call(&elsewhere, "x", "a write outside it".to_string())],
                collapsed: false,
                file: None,
            })
        }
        Some(Check::Contains { path, literal }) => {
            let file = target_file(root, path, |c| c.contains(literal.as_str()))?;
            let lit = gate::shown_literal(literal);
            let forbidden = vec![Call {
                shown: format!("an Edit removing '{lit}'"),
                tool: "Edit".to_string(),
                input: json!({ "file_path": file.abs.to_string_lossy(), "old_string": literal, "new_string": "", "replace_all": true }),
            }];
            let mut clean = vec![
                write_call(&file, &file.content, "the file as it stands".to_string()),
                Call {
                    shown: format!("an Edit keeping '{lit}'"),
                    tool: "Edit".to_string(),
                    input: json!({ "file_path": file.abs.to_string_lossy(), "old_string": literal, "new_string": format!("{literal}\n") }),
                },
            ];
            if let Some((without, line)) = minus_unrelated_line(&file.content, literal) {
                clean.push(write_call(&file, &without, format!("the file without line {line}")));
            }
            Ok(Forms { forbidden, clean, collapsed: false, file: Some(file) })
        }
    }
}

fn absent_forms(root: &Path, path: &str, literals: &[String]) -> Result<Forms, String> {
    let file = target_file(root, path, |_| true)?;
    let forbidden = literals
        .iter()
        .map(|l| write_call(&file, &format!("{}\n{l}", file.content), format!("writing '{}' into {}", gate::shown_literal(l), file.rel)))
        .collect();
    let clean = vec![write_call(&file, &file.content, format!("{} as it stands", file.rel))];
    Ok(Forms { forbidden, clean, collapsed: false, file: Some(file) })
}

/// Try `item`'s check through the pipeline, every form.
fn try_check(item: &Item, root: &Path) -> Tried {
    let built = match forms(item, root) {
        Ok(f) => f,
        Err(why) => return Tried { forbidden: vec![], clean: vec![], collapsed: false, file: None, none: Some(why) },
    };
    let Some(alone) = Alone::of(item) else {
        return Tried { forbidden: vec![], clean: vec![], collapsed: false, file: None, none: Some("the rule could not be built into a trial store".to_string()) };
    };
    let forbidden = built.forbidden.into_iter().map(|c| {
        let refused = alone.refuses(&c, root);
        (c, refused)
    });
    let forbidden: Vec<(Call, bool)> = forbidden.collect();
    let clean: Vec<(Call, bool)> = built.clean.into_iter().map(|c| {
        let refused = alone.refuses(&c, root);
        (c, refused)
    }).collect();
    Tried { forbidden, clean, collapsed: built.collapsed, file: built.file, none: None }
}

// ------------------------------------------------------- the real calls

/// What the real calls in the log say about one command check.
#[derive(Debug, Default, Clone)]
struct Replay {
    /// Whole calls the anchor reaches.
    reached: usize,
    /// Cut calls the anchor reaches.
    cut: usize,
    /// Whole calls the check would refuse.
    refused: Vec<RealCall>,
    /// Cut calls whose visible part carries a literal as a whole token.
    refused_cut: Vec<String>,
    /// Whole reached calls carrying a literal as a whole token.
    whole_hits: usize,
    /// For an inner requires trigger: reached calls whose raw text holds it,
    /// and those the matcher finds it in.
    trigger_raw: usize,
    trigger_found: usize,
}

impl Replay {
    fn line(&self) -> String {
        let examples: Vec<String> = self.refused.iter().take(3).map(|c| format!("'{}'", c.text)).collect();
        let mut line = format!(
            "reaches {} real call(s) in the log ({} more cut at {} characters); would refuse {}",
            self.reached,
            self.cut,
            crate::deliver::TRIGGER_LIMIT,
            self.refused.len()
        );
        if !examples.is_empty() {
            line.push_str(&format!(", e.g. {}", examples.join(", ")));
        }
        if !self.refused_cut.is_empty() {
            line.push_str(&format!("; {} cut call(s) carry it too", self.refused_cut.len()));
        }
        line
    }

    /// K8(d): refuses more than half of at least 5 whole calls it reaches.
    fn majority(&self) -> bool {
        self.reached >= 5 && self.refused.len() * 2 > self.reached
    }
}

/// Replay a command check over the real calls its anchor reaches, through the
/// hook's own pipeline. A forbidden check can only refuse a call carrying one
/// of its literals, so a call without any is let through without running.
fn replay(item: &Item, corpus: &Corpus, root: &Path) -> Option<Replay> {
    let anchor = command_anchor(&item.bindings)?;
    let check = item.check.as_ref()?;
    let literals: Vec<String> = match check {
        Check::Forbidden { literals } => literals.clone(),
        Check::Requires { .. } => Vec::new(),
        _ => return None,
    };
    let alone = Alone::of(item)?;
    let mut out = Replay::default();
    for call in corpus.reaching(anchor) {
        if call.cut {
            out.cut += 1;
            if literals.iter().any(|l| call.hit(anchor, l)) {
                out.refused_cut.push(call.text.clone());
            }
            continue;
        }
        out.reached += 1;
        if literals.iter().any(|l| call.hit(anchor, l)) {
            out.whole_hits += 1;
        }
        if let Check::Requires { when, .. } = check {
            if !gate::requires_trigger_names_a_binding(when, &item.bindings) {
                let stem = shell::star_stem(when.trim()).unwrap_or(when.trim());
                if call.text.contains(stem) {
                    out.trigger_raw += 1;
                }
                if call.segments.iter().any(|s| shell::anchor_reaches(anchor, s) && shell::trigger_in_segment(s, when)) {
                    out.trigger_found += 1;
                }
            }
        }
        let may_refuse = matches!(check, Check::Requires { .. }) || literals.iter().any(|l| call.text.contains(l.as_str()));
        if may_refuse && alone.refuses(&bash(&call.text), root) {
            out.refused.push(call.clone());
        }
    }
    Some(out)
}

// ------------------------------------------------------------ derivation

/// Where a derived tooth came from.
#[derive(Debug, Clone, PartialEq)]
enum Origin {
    /// Built from the current text (A5 (a)-(c), (e)).
    Text(String),
    /// An earlier check on the rule's own log (D7). `may_force`: no revise
    /// with a because removed it, and the current text names its literal.
    History { seq: i64, may_force: bool },
    /// Built from an earlier text of the rule (the one written at `seq`):
    /// tried and recorded, never forcing - the current text no longer
    /// composes it (D7).
    EarlierText { seq: i64 },
}

/// A tooth THOR builds, with the binding it would add.
#[derive(Debug, Clone, PartialEq)]
struct Derived {
    check: Check,
    proposed: Option<Binding>,
    origin: Origin,
}

fn command(value: &str) -> Binding {
    Binding::Target { kind: TargetKind::Command, value: value.to_string() }
}

/// A flag at the start of `token`: `^--?[A-Za-z][A-Za-z0-9-]*`, two
/// characters or more.
fn leading_flag(token: &str) -> Option<&str> {
    let rest = token.strip_prefix("--").or_else(|| token.strip_prefix('-'))?;
    if !rest.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) {
        return None;
    }
    let end = rest.find(|c: char| !(c.is_ascii_alphanumeric() || c == '-')).unwrap_or(rest.len());
    let flag = &token[..token.len() - rest.len() + end];
    (flag.len() >= 2).then_some(flag)
}

/// The teeth a span composing a call yields once `anchor_words` of it are the
/// anchor: forbidden F, forbidden "F next" when the next token is no flag,
/// and requires F when F is the first flag after the anchor (A5 (a)/(b)).
fn teeth_of_span(words: &[&str], anchor_len: usize, anchor: &str, proposed: Option<Binding>, from: &str) -> Vec<Derived> {
    let mut out = Vec::new();
    let Some(first) = words.get(anchor_len) else { return out };
    let origin = Origin::Text(from.to_string());
    out.push(Derived { check: Check::Forbidden { literals: vec![first.to_string()] }, proposed: proposed.clone(), origin: origin.clone() });
    if let Some(next) = words.get(anchor_len + 1).filter(|n| !n.starts_with('-')) {
        out.push(Derived {
            check: Check::Forbidden { literals: vec![format!("{first} {next}")] },
            proposed: proposed.clone(),
            origin: origin.clone(),
        });
    }
    if let Some(flag) = words[anchor_len..].iter().find_map(|w| leading_flag(w)) {
        out.push(Derived {
            check: Check::Requires { when: anchor.to_string(), required: vec![flag.to_string()] },
            proposed,
            origin,
        });
    }
    out
}

/// A5: every tooth the rule's own text composes (see this module's doc
/// comment), plus (e) - a span or flag of the text present in the file of a
/// Path/Dir anchor or a text path that resolves in `root`. Only where the
/// TEXT composes a call with a fragment: a flag alone in prose, far from any
/// anchor, derives nothing.
fn derive(item: &Item, root: Option<&Path>) -> Vec<Derived> {
    let candidates = gate::teeth_candidates(item);
    let spans: Vec<&str> = candidates
        .iter()
        .filter(|c| matches!(c.source, gate::CandidateSource::Backtick | gate::CandidateSource::Quoted))
        .map(|c| c.value.as_str())
        .collect();
    let anchors: Vec<String> = item
        .bindings
        .iter()
        .filter_map(|b| match b {
            Binding::Target { kind: TargetKind::Command, value } => Some(value.split_whitespace().collect::<Vec<_>>().join(" ")),
            _ => None,
        })
        .collect();
    let mut out: Vec<Derived> = Vec::new();
    for span in &spans {
        let words: Vec<&str> = span.split_whitespace().collect();
        let mut matched = false;
        for anchor in &anchors {
            let aw: Vec<&str> = anchor.split(' ').collect();
            if words.len() > aw.len() && words[..aw.len()] == aw[..] {
                matched = true;
                out.extend(teeth_of_span(&words, aw.len(), anchor, None, &format!("the span `{span}`")));
            }
        }
        if matched {
            continue;
        }
        // (b) a span starting with a command the rule is not bound to: 1 to 3
        // words before its first flag.
        if let Some(k) = words.iter().position(|w| leading_flag(w).is_some()).filter(|k| (1..=3).contains(k)) {
            let proposed_anchor = words[..k].join(" ");
            let plain = words[..k].iter().all(|w| w.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')));
            if plain {
                out.extend(teeth_of_span(&words, k, &proposed_anchor, Some(command(&proposed_anchor)), &format!("the span `{span}`")));
            }
        }
    }
    // (c) a prose flag within 3 tokens after a Command anchor's words.
    let tokens: Vec<String> = item
        .text
        .split_whitespace()
        .map(|t| t.trim_matches(|c: char| matches!(c, '`' | '\'' | '"' | '(' | ')' | ',' | ';' | ':' | '.' | '!' | '?')).to_string())
        .collect();
    for anchor in &anchors {
        let aw: Vec<&str> = anchor.split(' ').collect();
        for i in 0..tokens.len() {
            if i + aw.len() > tokens.len() || !tokens[i..i + aw.len()].iter().zip(&aw).all(|(t, a)| t.eq_ignore_ascii_case(a)) {
                continue;
            }
            let window = &tokens[i + aw.len()..(i + aw.len() + 3).min(tokens.len())];
            if let Some(flag) = window.iter().find_map(|t| leading_flag(t)) {
                let origin = Origin::Text(format!("the flag '{flag}' after '{anchor}' in the text"));
                out.push(Derived { check: Check::Forbidden { literals: vec![flag.to_string()] }, proposed: None, origin: origin.clone() });
                out.push(Derived { check: Check::Requires { when: anchor.clone(), required: vec![flag.to_string()] }, proposed: None, origin });
            }
        }
    }
    // (e) a derived contains.
    if let Some(root) = root {
        let fragments: Vec<&str> = candidates
            .iter()
            .filter(|c| matches!(c.source, gate::CandidateSource::Backtick | gate::CandidateSource::Quoted | gate::CandidateSource::Flag))
            .map(|c| c.value.as_str())
            .collect();
        let mut places: Vec<String> = item
            .bindings
            .iter()
            .filter_map(|b| match b {
                Binding::Target { kind: TargetKind::Path | TargetKind::Dir, value } => Some(value.clone()),
                _ => None,
            })
            .collect();
        places.extend(candidates.iter().filter(|c| c.source == gate::CandidateSource::Path).map(|c| c.value.clone()));
        places.dedup();
        for place in places {
            if Path::new(&place).has_root() {
                continue;
            }
            for fragment in &fragments {
                if target_file(root, &place, |c| c.contains(fragment)).is_ok() {
                    out.push(Derived {
                        check: Check::Contains { path: place.clone(), literal: fragment.to_string() },
                        proposed: None,
                        origin: Origin::Text(format!("'{fragment}' in {place}")),
                    });
                }
            }
        }
    }
    let mut unique: Vec<Derived> = Vec::new();
    for d in out {
        if !unique.iter().any(|u| u.check == d.check && u.proposed == d.proposed) {
            unique.push(d);
        }
    }
    unique
}

/// Every distinct earlier check on `id`'s own log that is not `current`,
/// with the seq it first appeared at and - when a revise that carried a
/// because removed or replaced it - that because (D7).
pub fn history(store: &EventStore, id: &str, current: Option<&Check>) -> Vec<TrialHistory> {
    let Ok(events) = store.get_events_by_entity(id) else { return Vec::new() };
    let mut out: Vec<TrialHistory> = Vec::new();
    let mut live: Option<(Check, i64)> = None;
    for e in events.iter().filter(|e| matches!(e.kind, EventKind::FactCreated | EventKind::FactRevised)) {
        let Ok(item) = model::store::parse_body(&e.body) else { continue };
        if live.as_ref().map(|(c, _)| Some(c)) == Some(item.check.as_ref()) {
            continue;
        }
        if let Some((old, seq)) = live.take() {
            let because = serde_json::from_str::<Value>(&e.body)
                .ok()
                .and_then(|b| b.get("because").and_then(|v| v.as_str()).map(str::to_string));
            if !out.iter().any(|h| h.check == old) {
                out.push(TrialHistory { seq, check: old, removed_with_because: because });
            }
        }
        live = item.check.map(|c| (c, e.seq));
    }
    out.retain(|h| Some(&h.check) != current);
    out
}

/// `history`, for a write: plus the head's own check when this write
/// changes it, with this write's because (D7) - a check removed right now
/// with a because forces nothing, and one removed without a because is tried
/// like any earlier check.
pub fn history_for_write(store: &EventStore, before: Option<&Item>, updated: &Item, because: Option<&str>) -> Vec<TrialHistory> {
    let mut out = history(store, &updated.id, updated.check.as_ref());
    let Some(old) = before.and_then(|b| b.check.as_ref()) else { return out };
    if Some(old) == updated.check.as_ref() || out.iter().any(|h| &h.check == old) {
        return out;
    }
    let events = store.get_events_by_entity(&updated.id).unwrap_or_default();
    let seq = crate::attest::check_since(&events, Some(old));
    let because = because.map(str::trim).filter(|b| !b.is_empty()).map(str::to_string);
    out.push(TrialHistory { seq, check: old.clone(), removed_with_because: because });
    out
}

/// Every distinct earlier text of `id` other than `current`'s, as the item
/// written with it and the seq it was written at, oldest first.
fn earlier_texts(store: &EventStore, id: &str, current: &Item) -> Vec<(i64, Item)> {
    let Ok(events) = store.get_events_by_entity(id) else { return Vec::new() };
    let mut out: Vec<(i64, Item)> = Vec::new();
    for e in events.iter().filter(|e| matches!(e.kind, EventKind::FactCreated | EventKind::FactRevised)) {
        let Ok(item) = model::store::parse_body(&e.body) else { continue };
        if item.text != current.text && !out.iter().any(|(_, o)| o.text == item.text) {
            out.push((e.seq, item));
        }
    }
    out
}

/// The seq of `id`'s first write THOR's trial refused (`GateRefused` by
/// actor `teeth-write`): a real call made after it never stops a derived
/// tooth from forcing (K8(c)).
fn first_refused_write(store: &EventStore, id: &str) -> Option<i64> {
    store
        .get_events_by_entity(id)
        .ok()?
        .iter()
        .find(|e| e.kind == EventKind::GateRefused && e.actor == model::store::TEETH_WRITE_ACTOR)
        .map(|e| e.seq)
}

// ----------------------------------------------------------- the outcome

/// Why a write is refused, one code per kind of fix (the refusal table, G1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefuseCode {
    /// A file check THOR cannot try: register the checkout, give the rule a
    /// project, make the file readable, or remove an empty dispute entry.
    Untryable,
    /// A literal the text does not name: name it, or drop the check.
    A4,
    /// A file check that is false right now: fix the check or the file.
    FalseNow,
    /// A tooth THOR built bites: hang it, or dispute it.
    Forcing,
    /// The check refuses most real calls it reaches: narrow it, or name one
    /// listed call in `accept_refusing`.
    Majority,
}

impl RefuseCode {
    pub fn as_str(self) -> &'static str {
        match self {
            RefuseCode::Untryable => "untryable",
            RefuseCode::A4 => "a4",
            RefuseCode::FalseNow => "false_now",
            RefuseCode::Forcing => "forcing",
            RefuseCode::Majority => "majority",
        }
    }
}

/// THE REFUSAL TABLE of this module (G1): every code a trial or a retest can
/// answer with, and the words its message always carries that name the way
/// on - an action the agent runs itself. Step 8's table test
/// (`mcp/tests/every_refusal_names_a_way_on.rs`) reads this list.
pub const REFUSAL_TABLE: &[(&str, &str)] = &[
    ("untryable", "then write this again"),
    ("a4", "or drop the check"),
    ("false_now", "then write this again"),
    ("forcing", "check_kind"),
    ("majority", "accept_refusing"),
    ("needs_agent:a4", "revise the text to name"),
    ("needs_agent:forcing", "check_kind"),
    ("needs_agent:majority", "accept_refusing"),
    ("needs_agent:no_reach", "bind it to"),
    ("needs_agent:false_now", "revise the check"),
    ("needs_agent:global", "give it the project"),
    ("needs_agent:crowded", "fold this rule into a neighbour"),
];

/// Where a write stands after its trial.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// On the rule track: stored with its check.
    Rule { proven: bool },
    /// On the note track: stored with no check. `pending` keeps a check THOR
    /// could not try or hang yet (K20); `by_dispute` names a writer's dispute
    /// that turned a forcing tooth into this note (K22).
    Note { pending: Option<PendingCheck>, by_dispute: bool },
    /// A pre-existing A4 failure on a write that leaves the check alone
    /// (K23): the write lands, the check stays, no verdict counts.
    Undecided { why: String },
    /// Refused: nothing is written on the item.
    Refuse { code: RefuseCode, message: String },
}

/// A trial's result.
#[derive(Debug, Clone)]
pub struct Trial {
    pub outcome: Outcome,
    /// The check the item is stored with: its own for a rule or an undecided
    /// write, none for a note or a refusal.
    pub check: Option<Check>,
    /// What the writer is told, one line each, in plain words.
    pub receipt: Vec<String>,
    /// The record to splice into the write; `None` for a refusal.
    pub record: Option<TrialRecord>,
    /// For a refusal with code `forcing`: each forcing tooth in words.
    pub forcing: Vec<String>,
}

/// The writing session, for K20 and K8(c). Empty for a retest.
#[derive(Debug, Clone, Default)]
pub struct SessionCtx {
    /// The top of the checkout the session works in.
    pub own_checkout: Option<PathBuf>,
    /// The project that checkout resolves to.
    pub own_project: Option<String>,
    /// The writing session's id.
    pub session_id: Option<String>,
}

/// The writer's disputes and accepted refusals (K22, K8(d)).
#[derive(Debug, Clone, Default)]
pub struct Disputes {
    pub legitimate_calls: Vec<String>,
    pub may_change: Vec<String>,
    pub accept_refusing: Vec<String>,
}

/// The write being tried.
#[derive(Debug, Clone)]
pub struct Write<'a> {
    /// The item exactly as it would be written, its check included.
    pub item: &'a Item,
    /// The head it replaces, for a revise, pin or unpin.
    pub before: Option<&'a Item>,
    /// The because this write carries, if any.
    pub because: Option<&'a str>,
    /// The head the item was read from.
    pub parent_rev: Option<String>,
}

/// What the trial reads besides the write: the real store (read only) and
/// the file it lives in, for the commands a message prints.
pub struct Env<'a> {
    pub store: &'a EventStore,
    pub db: Option<&'a Path>,
}

/// Why a retest cannot decide right now, on the machine's side - the same
/// type `model::store::settlement` holds a rule settled by while it lasts.
pub use model::store::Blocker;

/// Whether a check reads a file (and so needs a working copy to be tried).
use model::checkout::is_file_check;

fn check_path(check: &Check) -> Option<&str> {
    match check {
        Check::Contains { path, .. } | Check::Absent { path, .. } | Check::AbsentAll { path, .. } | Check::PathExists { path } => Some(path),
        _ => None,
    }
}

/// A check in words: "forbidden '--force' on 'git push'".
fn describe(check: &Check, bindings: &[Binding]) -> String {
    let anchor = command_anchor(bindings).map(|a| format!(" on '{a}'")).unwrap_or_default();
    let list = |ls: &[String]| ls.iter().map(|l| format!("'{}'", gate::shown_literal(l))).collect::<Vec<_>>().join(", ");
    match check {
        Check::Forbidden { literals } => format!("forbidden {}{anchor}", list(literals)),
        Check::Requires { when, required } => format!("requires {} after '{}'{anchor}", list(required), gate::shown_literal(when)),
        Check::Contains { path, literal } => format!("contains '{}' in {path}", gate::shown_literal(literal)),
        Check::Absent { path, literal } => format!("absent '{}' in {path}", gate::shown_literal(literal)),
        Check::AbsentAll { path, literals } => format!("absent {} in {path}", list(literals)),
        Check::PathExists { path } => format!("path_exists {path}"),
    }
}

/// The arguments that hang `check`, as the writer would pass them.
fn hang_it(check: &Check) -> String {
    let list = |ls: &[String]| format!("[{}]", ls.iter().map(|l| format!("'{}'", gate::shown_literal(l))).collect::<Vec<_>>().join(", "));
    match check {
        Check::Forbidden { literals } => format!("check_kind forbidden, check_literals {}", list(literals)),
        // The remember/revise door takes a requires check as check_literals
        // with the trigger first, never a check_path.
        Check::Requires { when, required } => {
            let mut literals = vec![when.clone()];
            literals.extend(required.iter().cloned());
            format!("check_kind requires, check_literals {}", list(&literals))
        }
        Check::Contains { path, literal } => format!("check_kind contains, check_path '{path}', check_literal '{}'", gate::shown_literal(literal)),
        Check::Absent { path, literal } => format!("check_kind absent, check_path '{path}', check_literal '{}'", gate::shown_literal(literal)),
        Check::AbsentAll { path, literals } => format!("check_kind absent_all, check_path '{path}', check_literals {}", list(literals)),
        Check::PathExists { path } => format!("check_kind path_exists, check_path '{path}'"),
    }
}

/// The literal a derived tooth turns on, for disputes and naming.
fn tooth_literal(check: &Check) -> Option<&str> {
    match check {
        Check::Forbidden { literals } | Check::AbsentAll { literals, .. } => literals.first().map(String::as_str),
        Check::Requires { required, .. } => required.first().map(String::as_str),
        Check::Contains { literal, .. } | Check::Absent { literal, .. } => Some(literal),
        Check::PathExists { path } => Some(path),
    }
}

/// `bindings` with a derived tooth's proposed binding FIRST: the tooth was
/// built for that command (a span starting with a command the rule is not
/// bound to, derive (b)), so it is tried, described and hung on it - never
/// on whichever command anchor the rule happened to list first.
fn proposed_first(bindings: &[Binding], proposed: Option<&Binding>) -> Vec<Binding> {
    let Some(p) = proposed else { return bindings.to_vec() };
    let mut out = vec![p.clone()];
    out.extend(bindings.iter().filter(|b| *b != p).cloned());
    out
}

/// `item` without the labels that answer nothing about its tooth: THOR's own
/// `teeth:` label and a `no-literal:`/`no-catch-on-command:` reason tag.
fn without_answer_labels(item: &Item) -> Item {
    let mut out = item.clone();
    out.tags.retain(|t| {
        !t.starts_with(model::store::TEETH_TAG_PREFIX)
            && model::store::teeth_answer(t).is_none()
            && gate::command_teeth_answer(t).is_none()
    });
    out
}

/// How one derived tooth fared.
#[derive(Debug, Clone)]
struct DerivedTrial {
    d: Derived,
    tried: Tried,
    /// Real calls of another session, before the first refused write, that it
    /// refuses: each stops it from forcing (K8(c)).
    stopped: Vec<StoppedForcing>,
    /// Real calls it refuses that do not count, and why.
    not_counting: Vec<String>,
    /// Why the rule with this tooth would not pass the gate or has no room.
    blocked: Option<String>,
    replay: Option<Replay>,
}

impl DerivedTrial {
    fn forces(&self) -> bool {
        let may_force = !matches!(self.d.origin, Origin::History { may_force: false, .. } | Origin::EarlierText { .. });
        may_force && self.tried.bites() && self.stopped.is_empty() && self.blocked.is_none()
    }

    fn bindings(&self, item: &Item) -> Vec<Binding> {
        proposed_first(&item.bindings, self.d.proposed.as_ref())
    }

    /// "forbidden '--force' on 'git push' refuses 'git push --force' and lets
    /// 'git push' and 3 real calls through".
    fn words(&self, item: &Item) -> String {
        let bindings = self.bindings(item);
        let refused: Vec<String> = self.tried.forbidden.iter().map(|(c, _)| format!("'{}'", c.shown)).collect();
        let passed: Vec<String> = self.tried.clean.iter().take(1).map(|(c, _)| format!("'{}'", c.shown)).collect();
        let mut s = format!("{} refuses {}", describe(&self.d.check, &bindings), refused.join(", "));
        if !passed.is_empty() {
            s.push_str(&format!(" and lets {}", passed.join(", ")));
            if let Some(r) = &self.replay {
                let through = r.reached.saturating_sub(r.refused.len());
                if through > 0 {
                    s.push_str(&format!(" and {through} real call(s)"));
                }
            }
            s.push_str(" through");
        }
        if let Origin::History { seq, .. } = &self.d.origin {
            s = format!("the check it carried at log seq {seq} ({}) still bites", describe(&self.d.check, &bindings));
        }
        s
    }
}

// ------------------------------------------------------------- the trial

enum Mode {
    Write,
    Retest,
}

/// The decision a trial reached, before it is shaped into a `Trial` or a
/// retest outcome.
enum Decision {
    Rule { proven: bool },
    Note { pending: Option<PendingCheck>, by_dispute: bool, dropped: Option<String> },
    Undecided { why: String },
    Refuse { code: RefuseCode, message: String, retest: RetestAs },
}

/// What a refusal is at a retest.
enum RetestAs {
    NeedsAgent,
    Blocked(Blocker),
}

struct Work<'a> {
    env: &'a Env<'a>,
    write: &'a Write<'a>,
    root: &'a Result<PathBuf, RootProblem>,
    session: &'a SessionCtx,
    corpus: &'a Corpus,
    disputes: &'a Disputes,
    history: &'a [TrialHistory],
    mode: Mode,
    // what the trial learned, for the record
    attempts: Vec<TrialAttempt>,
    corpus_summary: TrialCorpus,
    clean_failed: Vec<String>,
    evidence: Vec<TrialEvidence>,
    uncommitted: bool,
    file_blob: Option<String>,
    used_disputes: TrialDisputes,
    accepted: Vec<String>,
    receipt: Vec<String>,
    forcing_words: Vec<String>,
    /// At a retest: the rule's check is old contains stock (Q24), so A4 waits
    /// for the trial - `Some` only then.
    old_stock: Option<OldContainsStock>,
    /// The old contains check bit: the record carries `old_stock`.
    old_stock_granted: bool,
    /// At a retest that A4 refuses: the check itself bites - it holds and the
    /// hook's pipeline refuses its forbidden form and lets every clean form
    /// through - so ONLY the naming is missing (K23).
    a4_only: bool,
}

fn attempts_of(tried: &Tried, what: &str) -> Vec<TrialAttempt> {
    let via = |c: &Call| c.tool.clone();
    let mut out: Vec<TrialAttempt> = tried
        .forbidden
        .iter()
        .chain(tried.clean.iter())
        .map(|(c, r)| TrialAttempt {
            form: format!("{what}: {}", c.shown),
            via: via(c),
            outcome: if *r { AttemptOutcome::Refused } else { AttemptOutcome::LetThrough },
        })
        .collect();
    if let Some(why) = &tried.none {
        out.push(TrialAttempt { form: format!("{what}: nothing could be built - {why}"), via: "none".to_string(), outcome: AttemptOutcome::LetThrough });
    }
    out
}

fn same_project(a: Option<&str>, b: Option<&str>) -> bool {
    let a = model::normalize::normalize_project(a);
    a.is_some() && a == model::normalize::normalize_project(b)
}

use model::checkout::{default_branch, git, git_path, head_commit, in_blob};

/// Whether a file check holds in `root`: `Ok(())`, `Err(Ok(what))` when it is
/// false (with what is missing or present), `Err(Err(why))` when the file
/// cannot be read.
fn truth(check: &Check, root: &Path) -> Result<(), Result<String, String>> {
    let Some(path) = check_path(check) else { return Ok(()) };
    if !root.join(path).exists() {
        return Err(Ok(format!("{path} does not exist")));
    }
    match model::check::run(check, root) {
        model::check::Outcome::Holds => Ok(()),
        model::check::Outcome::Fails => Err(Ok(match check {
            Check::Contains { literal, .. } => format!("{path} does not contain '{}'", gate::shown_literal(literal)),
            Check::Absent { literal, .. } => format!("{path} already contains '{}'", gate::shown_literal(literal)),
            Check::AbsentAll { literals, .. } => format!(
                "{path} already contains one of {}",
                literals.iter().map(|l| format!("'{}'", gate::shown_literal(l))).collect::<Vec<_>>().join(", ")
            ),
            _ => format!("{path} does not exist"),
        })),
        model::check::Outcome::CannotRun => Err(Err(format!("{path} cannot be read as text"))),
    }
}

impl<'a> Work<'a> {
    fn item(&self) -> &'a Item {
        self.write.item
    }

    fn register_command(&self) -> String {
        model::store::register_command_for(self.env.db)
    }

    fn refuse(&self, code: RefuseCode, message: String) -> Decision {
        Decision::Refuse { code, message, retest: RetestAs::NeedsAgent }
    }

    /// (1) the file check cannot be tried here.
    fn untryable(&mut self, check: &Check, problem: &RootProblem) -> Option<Decision> {
        let item = self.item();
        let path = check_path(check).unwrap_or_default();
        if matches!(problem, RootProblem::GlobalRule) {
            return Some(self.refuse(
                RefuseCode::Untryable,
                format!(
                    "THOR cannot try this file check: a rule with no project has no working copy to try a check on {path} in - \
                     give it the project whose checkout holds {path}, then write this again"
                ),
            ));
        }
        let project = model::normalize::normalize_project(item.project.as_deref()).unwrap_or_default();
        let registered = match problem {
            RootProblem::NotRegistered => format!("'{project}' has no registered working copy"),
            other => format!("the working copy registered for '{project}' cannot be used ({other})"),
        };
        if matches!(self.mode, Mode::Retest) {
            return Some(Decision::Refuse {
                code: RefuseCode::Untryable,
                message: format!("blocked: {registered}"),
                retest: RetestAs::Blocked(Blocker::NoWorkingCopy { project }),
            });
        }
        if same_project(self.session.own_project.as_deref(), item.project.as_deref()) {
            return Some(self.refuse(
                RefuseCode::Untryable,
                format!(
                    "THOR cannot try this file check: {registered}. Run this from inside the checkout: {}, then write this again",
                    self.register_command()
                ),
            ));
        }
        None
    }

    /// Try every derived and historical tooth; the ones that force, after the
    /// writer's disputes. `command_only`: only teeth that need no working
    /// copy (K20).
    fn forcing(&mut self, base: &Item, root: Option<&Path>, command_only: bool) -> Result<Vec<DerivedTrial>, Decision> {
        let text_root = if command_only { None } else { root };
        let mut teeth = derive(base, text_root);
        for h in self.history {
            if command_only && is_file_check(&h.check) {
                continue;
            }
            let named = gate::unnamed_literals(&Item { check: Some(h.check.clone()), ..base.clone() }).is_empty();
            let may_force = h.removed_with_because.is_none() && named;
            if !may_force {
                let why = match &h.removed_with_because {
                    Some(b) => format!("because: '{b}'"),
                    None => "and the text no longer names its literal".to_string(),
                };
                self.receipt.push(format!(
                    "{} dropped at seq {}, {why} - listed, it forces nothing",
                    describe(&h.check, &base.bindings),
                    h.seq
                ));
            }
            let d = Derived { check: h.check.clone(), proposed: None, origin: Origin::History { seq: h.seq, may_force } };
            if !teeth.iter().any(|t| t.check == d.check && t.proposed.is_none()) {
                teeth.push(d);
            }
        }
        // The teeth of every earlier text: tried and recorded, forcing
        // nothing (D7).
        let mut from_texts = 0;
        for (seq, earlier) in earlier_texts(self.env.store, &base.id, base) {
            for mut d in derive(&earlier, text_root) {
                if !teeth.iter().any(|t| t.check == d.check && t.proposed == d.proposed) {
                    d.origin = Origin::EarlierText { seq };
                    teeth.push(d);
                    from_texts += 1;
                }
            }
        }
        if from_texts > 0 {
            self.receipt.push(format!(
                "{from_texts} tooth/teeth built from the rule's earlier texts were tried and recorded - the current text no \
                 longer composes them, so they force nothing"
            ));
        }
        let from_history = teeth.iter().filter(|t| matches!(t.origin, Origin::History { .. } | Origin::EarlierText { .. })).count();
        self.attempts.push(TrialAttempt {
            form: format!(
                "derive: {} tooth/teeth built from the text and anchors, {from_history} from the rule's own history \
                 (earlier checks and texts)",
                teeth.len() - from_history
            ),
            via: "derive".to_string(),
            outcome: AttemptOutcome::LetThrough,
        });
        let cmd_root = command_root(self.env.store, base.project.as_deref());
        let writer = self.session.session_id.clone();
        let first_refusal = first_refused_write(self.env.store, &base.id);
        let mut trials: Vec<DerivedTrial> = Vec::new();
        for d in teeth {
            let mut with = base.clone();
            with.check = Some(d.check.clone());
            with.bindings = proposed_first(&base.bindings, d.proposed.as_ref());
            let file = is_file_check(&d.check);
            let try_root = if file { root.map(Path::to_path_buf) } else { Some(cmd_root.clone()) };
            let Some(try_root) = try_root else { continue };
            let tried = try_check(&with, &try_root);
            let what = format!("derived {}", describe(&d.check, &with.bindings));
            self.attempts.extend(attempts_of(&tried, &what));
            let mut dt = DerivedTrial { d, tried, stopped: vec![], not_counting: vec![], blocked: None, replay: None };
            let never_forces = matches!(dt.d.origin, Origin::EarlierText { .. });
            if dt.tried.bites() && !file && !never_forces {
                if let Some(r) = replay(&with, self.corpus, &try_root) {
                    for call in &r.refused {
                        match call.counts_against_forcing(writer.as_deref(), first_refusal) {
                            Some((session, seq)) => {
                                dt.stopped.push(StoppedForcing { call: call.text.clone(), session: session.to_string(), seq })
                            }
                            None => {
                                let (session, seq) = call.first();
                                let why = if writer.as_deref() == Some(session) && call.sessions.len() == 1 {
                                    "was made by the writing session and does not count".to_string()
                                } else {
                                    "was made after THOR first refused this rule and does not count".to_string()
                                };
                                dt.not_counting.push(format!("the real call '{}' (session {session}, log seq {seq}) {why}", call.text));
                            }
                        }
                    }
                    dt.replay = Some(r);
                }
            }
            if dt.tried.bites() && dt.stopped.is_empty() && !never_forces {
                // Judged without the labels a writer's refusal tells them to
                // drop or that THOR itself replaces - a `teeth:` label and a
                // `no-literal:` reason - so neither can keep a tooth from
                // forcing: a reason tag on the rule, or the note label of the
                // head it revises, would otherwise make the gate refuse the
                // rule WITH the tooth, and the tooth would never force.
                let judged = without_answer_labels(&with);
                let gate_result = match self.write.before {
                    Some(before) => gate::revise(&without_answer_labels(before), &judged),
                    None => gate::declare(&judged),
                };
                if let Err(refusal) = gate_result {
                    dt.blocked = Some(refusal.problem);
                } else {
                    match model::store::capacity(self.env.store, &with) {
                        Ok(model::store::Capacity::DeadOnArrival(r)) | Ok(model::store::Capacity::Full(r)) => dt.blocked = Some(r.problem),
                        Err(e) => dt.blocked = Some(e.to_string()),
                        Ok(_) => {}
                    }
                }
            }
            for s in &dt.stopped {
                self.corpus_summary.stopped_forcing.push(s.clone());
            }
            trials.push(dt);
        }
        for dt in &trials {
            // One line per tooth, never one per call: measured on a copy of a
            // real store, a rule about `git push` got 58 such lines, one per
            // ordinary push in the log. The record keeps every call.
            if let Some(s) = dt.stopped.first() {
                let more = dt.stopped.len() - 1;
                let tail = if more > 0 { format!(", and {more} more real call(s) like it") } else { String::new() };
                self.receipt.push(format!(
                    "{} does not force: it would refuse the real call '{}' (session {}, log seq {}){tail}",
                    describe(&dt.d.check, &dt.bindings(base)),
                    s.call,
                    s.session,
                    s.seq
                ));
            }
            for n in &dt.not_counting {
                self.receipt.push(n.clone());
            }
            if let (true, Some(why)) = (dt.tried.bites(), &dt.blocked) {
                self.receipt.push(format!("{} does not force: {why}", describe(&dt.d.check, &dt.bindings(base))));
            }
        }
        let mut forcing: Vec<DerivedTrial> = trials.into_iter().filter(DerivedTrial::forces).collect();
        if forcing.is_empty() {
            return Ok(forcing);
        }
        // Disputes (K22).
        let mut failed: Vec<String> = Vec::new();
        let base_root = cmd_root.clone();
        for call in &self.disputes.legitimate_calls {
            let segments = shell::segments(call, shell::Dialect::Sh);
            let mut valid_for_any = false;
            let mut why_not = String::new();
            for dt in forcing.iter_mut().filter(|d| !is_file_check(&d.d.check)) {
                let bindings = dt.bindings(base);
                let Some(anchor) = command_anchor(&bindings) else { continue };
                let literal = tooth_literal(&dt.d.check).unwrap_or_default();
                let mut with = base.clone();
                with.check = Some(dt.d.check.clone());
                with.bindings = bindings.clone();
                let own_form = dt.tried.forbidden.iter().any(|(c, _)| c.shown.trim() == call.trim());
                let reaches = shell::reaches_any(anchor, &segments);
                let beyond = call.split_whitespace().count() > anchor.split_whitespace().count() + literal.split_whitespace().count();
                if own_form {
                    why_not = format!("'{call}' is the forbidden form THOR built itself, not a legitimate call");
                    continue;
                }
                if !reaches {
                    why_not = format!("'{call}' is not a call on '{anchor}'");
                    continue;
                }
                if !refuses_call(&with.id, &with, call, &base_root) {
                    why_not = format!("'{call}' is not refused by {} - nothing to dispute there", describe(&dt.d.check, &bindings));
                    continue;
                }
                if !beyond {
                    why_not = format!("'{call}' carries nothing beyond '{anchor}' and '{literal}' - name the whole legitimate call");
                    continue;
                }
                valid_for_any = true;
                dt.blocked = Some(format!("disputed: the writer named '{call}' as legitimate"));
            }
            if valid_for_any {
                self.used_disputes.legitimate_calls.push(call.clone());
            } else {
                failed.push(if why_not.is_empty() { format!("'{call}' does not fit any tooth that forces") } else { why_not });
            }
        }
        for span in &self.disputes.may_change {
            let mut valid = false;
            for dt in forcing.iter_mut() {
                if let Check::Contains { path, literal } = &dt.d.check {
                    let present = root.is_some_and(|r| target_file(r, path, |c| c.contains(literal.as_str())).is_ok());
                    if literal == span && present && matches!(dt.d.origin, Origin::Text(_)) {
                        valid = true;
                        dt.blocked = Some(format!("disputed: the writer named '{span}' as a span that may change"));
                    }
                }
            }
            if valid {
                self.used_disputes.may_change.push(span.clone());
            } else {
                failed.push(format!("may_change '{span}' is not exactly the literal of a contains THOR derived and found in its file"));
            }
        }
        let still: Vec<DerivedTrial> = forcing.iter().filter(|d| d.blocked.is_none()).cloned().collect();
        if !failed.is_empty() {
            let mut message = self.forcing_message(base, &still);
            message.push_str(&format!(" Your dispute does not hold: {}.", failed.join("; ")));
            return Err(self.refuse(RefuseCode::Forcing, message));
        }
        forcing.retain(|d| d.blocked.is_none());
        Ok(forcing)
    }

    /// The refusal for forcing teeth (8): each one, its forms, both
    /// polarities when both bite, and both exits with a built example.
    fn forcing_message(&mut self, base: &Item, forcing: &[DerivedTrial]) -> String {
        let mut parts: Vec<String> = Vec::new();
        for dt in forcing {
            let words = dt.words(base);
            self.forcing_words.push(words.clone());
            let bindings = dt.bindings(base);
            let verb = if matches!(dt.d.origin, Origin::History { .. }) { "restore it" } else { "hang it" };
            let mut hang = format!("{verb}: {}", hang_it(&dt.d.check));
            if let Some(Binding::Target { value, .. }) = &dt.d.proposed {
                hang.push_str(&format!(", and bind it to the command '{value}'"));
            }
            let exit = match &dt.d.check {
                Check::Contains { literal, .. } => format!("Or, if that span may change: may_change: ['{}']", gate::shown_literal(literal)),
                _ => {
                    let anchor = command_anchor(&bindings).unwrap_or_default();
                    let literal = tooth_literal(&dt.d.check).unwrap_or_default();
                    format!("Or, if a real call legitimately carries it: legitimate_calls: ['{anchor} {literal} <the rest of the real call>']")
                }
            };
            parts.push(format!("{words}. {}. {exit}.", capitalise(&hang)));
        }
        // Both polarities of one fragment, side by side (F4).
        let fragments: Vec<(&str, bool)> = forcing
            .iter()
            .filter_map(|d| tooth_literal(&d.d.check).map(|l| (l, matches!(d.d.check, Check::Requires { .. }))))
            .collect();
        let both = fragments.iter().any(|(l, req)| *req && fragments.iter().any(|(m, r2)| m == l && !r2));
        let mut message = format!("THOR built a tooth from your own text and it bites: {}", parts.join(" Also: "));
        if both {
            message.push_str(
                " Both polarities bite on the same fragment: a text that forbids it wants the forbidden one, a text that \
                 recommends it wants the requires one.",
            );
        }
        if base.check.is_none() && gate::teeth_reason_tag(base).is_some() {
            message.push_str(" A no-literal reason no longer stands in for a tooth that works.");
        }
        message
    }

    /// The whole decision (see this module's doc comment for the order).
    fn decide(&mut self) -> Decision {
        let item = self.item();
        for entry in self.disputes.legitimate_calls.iter().chain(&self.disputes.may_change).chain(&self.disputes.accept_refusing) {
            if entry.trim().is_empty() {
                return self.refuse(
                    RefuseCode::Untryable,
                    "legitimate_calls, may_change and accept_refusing each take a whole call or span, and one entry is empty - \
                     remove it, then write this again"
                        .to_string(),
                );
            }
        }
        let root_ok: Option<PathBuf> = self.root.as_ref().ok().cloned();
        let w = item.check.clone();
        let unnamed = gate::unnamed_literals(item);
        let touches_check = match self.write.before {
            None => true,
            Some(before) => {
                let was = gate::unnamed_literals(before);
                before.check != item.check || unnamed.iter().any(|l| !was.contains(l))
            }
        };
        // (1) untryable.
        if let (Some(check), Err(problem)) = (&w, self.root) {
            if is_file_check(check) {
                if let Some(refused) = self.untryable(check, problem) {
                    return refused;
                }
                if !unnamed.is_empty() {
                    return self.a4_refusal();
                }
                let mut base = item.clone();
                base.check = None;
                let forcing = match self.forcing(&base, None, true) {
                    Ok(f) => f,
                    Err(refused) => return refused,
                };
                if !forcing.is_empty() {
                    let message = self.forcing_message(&base, &forcing);
                    return self.refuse(RefuseCode::Forcing, message);
                }
                let project = model::normalize::normalize_project(item.project.as_deref()).unwrap_or_default();
                self.receipt.push(format!(
                    "teeth: note - this file check could not be tried here because '{project}' has no registered working copy \
                     on this machine; THOR tries it again at the first evaluation after that project is registered, and \
                     hangs it if it bites"
                ));
                return Decision::Note {
                    pending: Some(PendingCheck { check: check.clone(), why: PendingWhy::NoWorkingCopy }),
                    by_dispute: false,
                    dropped: None,
                };
            }
        }

        // (2) A4 - or, at a retest, old contains stock (Q24): its trial goes on
        // as if the literal were named, and only a check that bites both ways
        // in the registered working copy becomes a rule; anything else gets
        // A4's own answer (`not_old_stock`).
        let a4_pre_existing = !unnamed.is_empty();
        let old_stock = a4_pre_existing && matches!(self.mode, Mode::Retest) && self.old_stock.is_some();
        if a4_pre_existing && !old_stock && (touches_check || matches!(self.mode, Mode::Retest)) {
            if matches!(self.mode, Mode::Retest) {
                self.a4_only = self.bites_but_for_the_naming(root_ok.as_deref());
            }
            return self.a4_refusal();
        }

        // (3) false in the registered checkout.
        if let (Some(check), Some(root)) = (&w, &root_ok) {
            if is_file_check(check) {
                match truth(check, root) {
                    Ok(()) => {}
                    Err(Err(why)) if old_stock => return self.not_old_stock(format!("{why} in {}", root.display())),
                    Err(Ok(what)) if old_stock => return self.not_old_stock(format!("{what} in {}", root.display())),
                    Err(Err(why)) => {
                        return self.refuse(
                            RefuseCode::Untryable,
                            format!(
                                "THOR cannot try this check: {why} in {} - make it a readable text file, or drop the check, then \
                                 write this again",
                                root.display()
                            ),
                        )
                    }
                    Err(Ok(what)) => {
                        let own = self.session.own_checkout.as_ref().filter(|own| *own != root);
                        let holds_in_own = own.is_some_and(|own| truth(check, own).is_ok())
                            && same_project(self.session.own_project.as_deref(), item.project.as_deref());
                        if holds_in_own && matches!(self.mode, Mode::Write) {
                            self.receipt.push(format!(
                                "teeth: note - your check holds in your own checkout but not in the working copy registered for \
                                 this project ({}): {what}. THOR keeps it pending and hangs it once it holds and bites there - \
                                 hanging it now would refuse every edit of that file in the registered checkout",
                                root.display()
                            ));
                            return Decision::Note {
                                pending: Some(PendingCheck { check: check.clone(), why: PendingWhy::NotTrueInRegisteredCheckout }),
                                by_dispute: false,
                                dropped: None,
                            };
                        }
                        if matches!(self.mode, Mode::Retest) {
                            if let Check::Contains { path, literal } = check {
                                if root.join(path).is_file() && in_blob(root, &default_branch(root), path, literal) {
                                    return Decision::Refuse {
                                        code: RefuseCode::FalseNow,
                                        message: format!("blocked: {what} in {}, but the default branch still has it", root.display()),
                                        retest: RetestAs::Blocked(Blocker::WorkingTreeDiffers { path: path.clone() }),
                                    };
                                }
                            }
                            return self.refuse(
                                RefuseCode::FalseNow,
                                format!("its check is false in {} right now: {what} - revise the check, or restore the file", root.display()),
                            );
                        }
                        return self.refuse(
                            RefuseCode::FalseNow,
                            format!(
                                "your check is false in {} right now: {what}. Fix the check (or the file), then write this again",
                                root.display()
                            ),
                        );
                    }
                }
            }
        }

        // (4)/(5) the writer's own check.
        let cmd_root = command_root(self.env.store, item.project.as_deref());
        let mut dropped: Option<String> = None;
        if let Some(check) = &w {
            let try_root = if is_file_check(check) { root_ok.clone().unwrap_or_else(|| cmd_root.clone()) } else { cmd_root.clone() };
            let tried = try_check(item, &try_root);
            self.attempts.extend(attempts_of(&tried, &format!("your {}", describe(check, &item.bindings))));
            self.clean_failed = tried.clean_failed();
            if let Some(file) = &tried.file {
                self.file_blob = git(&try_root, &["hash-object", "--", &git_path(&file.rel)]).map(|s| s.trim().to_string());
            }
            let replayed = replay(item, self.corpus, &try_root);
            if let Some(r) = &replayed {
                self.corpus_summary.reached = r.reached;
                self.corpus_summary.whole_hits = r.whole_hits;
                self.corpus_summary.cut = r.cut;
                self.receipt.push(r.line());
                if r.trigger_raw > 0 && r.trigger_found == 0 {
                    self.receipt.push(format!(
                        "the trigger is blind to how it is really typed: it occurs in {} reached call(s) and the matcher finds \
                         it in {}; if it names a family of names, end it with *",
                        r.trigger_raw, r.trigger_found
                    ));
                }
            }
            if tried.bites() {
                if let Some(r) = replayed.as_ref().filter(|r| r.majority()) {
                    let listed: Vec<String> = r.refused.iter().map(|c| c.text.clone()).collect();
                    let accepted: Vec<String> =
                        self.disputes.accept_refusing.iter().filter(|a| listed.contains(a)).cloned().collect();
                    let bad: Vec<&String> = self.disputes.accept_refusing.iter().filter(|a| !listed.contains(a)).collect();
                    if !bad.is_empty() {
                        return self.refuse(
                            RefuseCode::Majority,
                            format!(
                                "accept_refusing names {}, which is not one of the listed calls. Name one of: {}",
                                bad.iter().map(|b| format!("'{b}'")).collect::<Vec<_>>().join(", "),
                                listed.iter().map(|c| format!("'{c}'")).collect::<Vec<_>>().join(", ")
                            ),
                        );
                    }
                    if accepted.is_empty() {
                        let mut message = format!(
                            "your check would refuse {} of the {} real calls it reaches in the log: {}. Two ways on: narrow \
                             the check (a longer literal, a narrower anchor), or - when refusing them is the intent - name one \
                             of these calls in accept_refusing",
                            r.refused.len(),
                            r.reached,
                            listed.iter().map(|c| format!("'{c}'")).collect::<Vec<_>>().join(", ")
                        );
                        // F4 (R8.4): when a tooth THOR builds from the rule's own
                        // text bites, name it too - it is the narrower check.
                        let mut base = item.clone();
                        base.check = None;
                        if let Ok(forcing) = self.forcing(&base, root_ok.as_deref(), false) {
                            if !forcing.is_empty() {
                                message.push_str(&format!(". {}", self.forcing_message(&base, &forcing)));
                            }
                        }
                        return self.refuse(RefuseCode::Majority, message);
                    }
                    self.accepted = accepted;
                    self.receipt.push(format!("refusing these real calls is accepted: {}", self.accepted.join(", ")));
                }
                // Evidence (the proven label; never settlement).
                let mut refusals = Vec::new();
                if let Some(before) = self.write.before {
                    if before.check.as_ref() == Some(check) {
                        refusals = crate::attest::field_refusals(self.env.store, &item.id, item);
                    }
                }
                if let Some((seq, target)) = refusals.first() {
                    self.evidence.push(TrialEvidence::FieldRefusal { seq: *seq });
                    self.receipt.push(format!("the real hook refused '{target}' (log seq {seq}) in a Claude Code session"));
                }
                if let (Check::Contains { literal, .. }, Some(file)) = (check, &tried.file) {
                    match head_commit(&try_root).filter(|_| in_blob(&try_root, "HEAD", &file.rel, literal)) {
                        Some(commit) => self.evidence.push(TrialEvidence::InHead { commit }),
                        None => self.uncommitted = true,
                    }
                }
                let absent = match check {
                    Check::Absent { path, literal } => Some((path, literal.clone())),
                    Check::AbsentAll { path, literals } => literals.first().map(|l| (path, l.clone())),
                    _ => None,
                };
                if let (Some((path, literal)), Mode::Write) = (absent, &self.mode) {
                    if let Some(commit) = crate::prove::lived_in_history(&try_root, path, &literal) {
                        self.evidence.push(TrialEvidence::LivedInHistory { commit });
                    }
                }
                let proven = !self.evidence.is_empty();
                self.receipt.insert(0, self.rule_headline(check, &tried, proven, &try_root));
                if old_stock {
                    // Q24: a tooth, with the fragment it guards in the record
                    // and on the receipt's second line, for a reader to judge.
                    self.old_stock_granted = true;
                    let mark = self.old_stock.as_ref().expect("old_stock is only true with a mark");
                    self.receipt.insert(1, old_stock_line(mark));
                    return Decision::Rule { proven };
                }
                if a4_pre_existing {
                    let why = format!(
                        "A4: {} not named in the text - revise the text to name it, or clear the check with a because",
                        unnamed.iter().map(|l| format!("'{}'", gate::shown_literal(l))).collect::<Vec<_>>().join(", ")
                    );
                    // The verdict first: the check bites, and still no verdict
                    // counts while its literal is unnamed (K23).
                    self.receipt.insert(0, format!("teeth: undecided - {why}"));
                    return Decision::Undecided { why };
                }
                return Decision::Rule { proven };
            }
            // (6) the writer's check does not bite.
            if old_stock {
                let why = if tried.reaches() {
                    format!(
                        "it refuses {}, which a check must let through",
                        self.clean_failed.iter().map(|c| format!("'{c}'")).collect::<Vec<_>>().join(" and ")
                    )
                } else {
                    format!("an Edit removing it is let through ({})", no_reach_why(check))
                };
                return self.not_old_stock(why);
            }
            if a4_pre_existing {
                let why = format!(
                    "A4: {} not named in the text - revise the text to name it, or clear the check with a because",
                    unnamed.iter().map(|l| format!("'{}'", gate::shown_literal(l))).collect::<Vec<_>>().join(", ")
                );
                self.receipt.insert(0, format!("teeth: undecided - {why}"));
                return Decision::Undecided { why };
            }
            let failed_form = if !tried.reaches() {
                match (&tried.none, tried.forbidden.first()) {
                    (Some(why), _) => format!("nothing could be tried: {why}"),
                    (None, Some((c, _))) => format!("tried {} through the hook; let through ({})", c.shown, no_reach_why(check)),
                    _ => "nothing reached it".to_string(),
                }
            } else {
                format!("it refuses {}, which a check must let through", self.clean_failed.iter().map(|c| format!("'{c}'")).collect::<Vec<_>>().join(" and "))
            };
            // At a retest, a file check that only lacks its anchor needs the
            // agent: THOR can name the one binding that makes it bite.
            if matches!(self.mode, Mode::Retest) && !tried.reaches() {
                if let Some(path) = check_path(check).filter(|_| is_file_check(check)) {
                    let mut bound = item.clone();
                    bound.bindings.push(Binding::Target { kind: TargetKind::Path, value: path.to_string() });
                    if try_check(&bound, &try_root).bites() {
                        return Decision::Refuse {
                            code: RefuseCode::Forcing,
                            message: format!(
                                "forbidden form: {} -> let through (this rule's bindings never reach a write to {path}); bind it \
                                 to path:{path}",
                                tried.forbidden.first().map(|(c, _)| c.shown.clone()).unwrap_or_default()
                            ),
                            retest: RetestAs::NeedsAgent,
                        };
                    }
                }
            }
            dropped = Some(format!("your check ({}) was not stored: {failed_form}", describe(check, &item.bindings)));
        }

        // (6)-(8) derived teeth.
        let mut base = item.clone();
        base.check = None;
        let forcing = match self.forcing(&base, root_ok.as_deref(), false) {
            Ok(f) => f,
            Err(refused) => return refused,
        };
        if !forcing.is_empty() {
            let mut message = self.forcing_message(&base, &forcing);
            // The writer's own check was dropped on the way here: say so, and
            // why, so the refusal does not read as if it was never looked at.
            if let Some(dropped) = &dropped {
                message = format!("{}. {message}", capitalise(dropped));
            }
            return self.refuse(RefuseCode::Forcing, message);
        }
        let by_dispute = !self.used_disputes.legitimate_calls.is_empty() || !self.used_disputes.may_change.is_empty();
        let headline = if by_dispute {
            format!(
                "teeth: note by dispute - a tooth THOR built bites, and you named {} as legitimate; doctor lists this for the \
                 owner",
                self.used_disputes
                    .legitimate_calls
                    .iter()
                    .chain(&self.used_disputes.may_change)
                    .map(|c| format!("'{c}'"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        } else if let Some(dropped) = &dropped {
            format!("teeth: note - {dropped}")
        } else if gate::teeth_candidates(item).is_empty() {
            "teeth: note - nothing in the text or anchors to build a tooth from (no flag, quoted call, backtick span or path)"
                .to_string()
        } else {
            "teeth: note - THOR tried every tooth your text and anchors compose, and none of them bites".to_string()
        };
        self.receipt.insert(0, headline);
        Decision::Note { pending: None, by_dispute, dropped }
    }

    /// The receipt's first line for a rule.
    fn rule_headline(&self, check: &Check, tried: &Tried, proven: bool, root: &Path) -> String {
        let label = if proven { "teeth: proven guard" } else { "teeth: wired, not proven" };
        if tried.collapsed {
            let refused: Vec<String> = tried.forbidden.iter().map(|(c, _)| format!("'{}'", c.shown)).collect();
            let passed: Vec<String> = tried.clean.iter().map(|(c, _)| format!("'{}'", c.shown)).collect();
            let mut line = format!(
                "{label} - a whole-command ban: refuses {} (refused), lets {} through",
                refused.join(", "),
                and_join(&passed)
            );
            if let Some(advice) = self.cleaner_anchor(check, root) {
                line.push_str(&format!(". Advice: {advice}"));
            }
            return line;
        }
        if let (Check::Contains { literal, .. }, Some(file)) = (check, &tried.file) {
            let project = self.item().project.clone().unwrap_or_default();
            let place = format!("{} in the working copy registered for {project} ({})", file.rel, root.display());
            let _ = literal;
            return format!("{label} - {place} holds it; tried {}", tried.listed());
        }
        format!("{label} - tried {}", tried.listed())
    }

    /// For a collapsed forbidden check: the anchor without its literal, when
    /// the literal on that anchor bites (K19, advice only).
    fn cleaner_anchor(&self, check: &Check, root: &Path) -> Option<String> {
        let Check::Forbidden { literals } = check else { return None };
        let anchor = command_anchor(&self.item().bindings)?;
        let literal = literals.iter().find(|l| shell::literal_at_boundary(anchor, l))?;
        let stripped = anchor.replace(literal.as_str(), " ").split_whitespace().collect::<Vec<_>>().join(" ");
        if stripped.is_empty() {
            return None;
        }
        let mut cleaner = self.item().clone();
        cleaner.bindings = vec![command(&stripped)];
        cleaner.check = Some(Check::Forbidden { literals: vec![literal.clone()] });
        try_check(&cleaner, root).bites().then(|| {
            format!("bind to '{stripped}' with the literal '{}' - THOR tried exactly that and it bites", gate::shown_literal(literal))
        })
    }

    fn record(&self, stored: &Item, verdict: RecordedVerdict, pending: Option<PendingCheck>) -> TrialRecord {
        let root = self.root.as_ref().ok();
        TrialRecord::seal(TrialFields {
            v: model::store::TRIAL_RECORD_VERSION,
            fingerprint: model::store::fingerprint(stored),
            root: root.map(|r| r.display().to_string().replace('\\', "/")),
            project: model::normalize::normalize_project(stored.project.as_deref()),
            head_commit: root.and_then(|r| head_commit(r)),
            candidates: gate::teeth_candidates(self.item()),
            attempts: self.attempts.clone(),
            corpus: self.corpus_summary.clone(),
            clean_failed: self.clean_failed.clone(),
            disputes: self.used_disputes.clone(),
            accept_refusing: self.accepted.clone(),
            history: self.history.to_vec(),
            pending,
            a4: gate::unnamed_literals(stored),
            verdict,
            evidence: self.evidence.clone(),
            uncommitted: self.uncommitted,
            parent_rev: self.write.parent_rev.clone(),
            file_blob: self.file_blob.clone(),
            a4_old_stock: if self.old_stock_granted { self.old_stock.clone() } else { None },
        })
    }

    /// A4's own answer (step 2): refused at a write, needs the agent at a
    /// retest - in the ground's own words (`gate::a4_ground`: "no longer
    /// named" for a text change that unnamed a literal), else the plain ones.
    fn a4_refusal(&self) -> Decision {
        let item = self.item();
        let file = match (&item.check, self.root.as_ref().ok()) {
            (Some(Check::Contains { path, .. }), Some(root)) => std::fs::read_to_string(root.join(path)).ok(),
            _ => None,
        };
        let refusal = gate::a4_ground(self.write.before, item, file.as_deref())
            .err()
            .or_else(|| gate::check_names_its_literals(item, file.as_deref()).err());
        let text = match refusal {
            Some(r) => format!("{}. Fix: {}", r.problem, r.fix),
            None => "A4".to_string(),
        };
        match self.mode {
            Mode::Retest => Decision::Refuse {
                code: RefuseCode::A4,
                message: format!("A4: {text} - revise the text to name it (revise --id {} with the literal in the text)", item.id),
                retest: RetestAs::NeedsAgent,
            },
            Mode::Write => self.refuse(RefuseCode::A4, text),
        }
    }

    /// Whether the rule's check works as a tooth when the A4 naming is left
    /// aside: a file check holds in the registered checkout, and the hook's
    /// pipeline refuses the forbidden form and lets every clean form through.
    /// What tells "only the words are missing" (K23: the A4 list) from a
    /// check that needs more than a rewording.
    fn bites_but_for_the_naming(&self, root: Option<&Path>) -> bool {
        let item = self.item();
        let Some(check) = &item.check else { return false };
        let try_root = if is_file_check(check) {
            match root {
                Some(root) if truth(check, root).is_ok() => root.to_path_buf(),
                _ => return false,
            }
        } else {
            command_root(self.env.store, item.project.as_deref())
        };
        try_check(item, &try_root).bites()
    }

    /// Old contains stock that does not bite (Q24): no exemption, so A4
    /// answers exactly as it would have - with one line saying why the old
    /// check was not counted.
    fn not_old_stock(&mut self, why: String) -> Decision {
        self.receipt.push(format!("old contains check (Q24): not counted as a tooth despite A4 - {why}"));
        self.a4_refusal()
    }
}

/// The receipt's line for old contains stock counted as a tooth (Q24): the
/// guarded fragment, and what the reader does with it.
fn old_stock_line(mark: &OldContainsStock) -> String {
    format!(
        "old contains check (Q24): counted as a tooth although the text does not name its literal - {}. Judge whether \
         that fragment guards this rule's subject: if it does, name it in the text; if it does not, clear the check \
         with a because. A revise of the check, text or anchors ends this exemption, and A4 applies from then on",
        mark.describe()
    )
}

fn capitalise(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

/// `'a'`, `'a' and 'b'`, `'a', 'b' and 'c'`.
fn and_join(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// Why a check THOR tried did not reach the forbidden form, in the words
/// that name the fix.
fn no_reach_why(check: &Check) -> String {
    match check {
        Check::Contains { path, .. } => format!(
            "a contains check only reaches a write when the rule is anchored to that file or its directory - bind it to {path}"
        ),
        Check::Absent { path, .. } | Check::AbsentAll { path, .. } => {
            format!("this rule's bindings never reach a write to {path} - bind it to {path}")
        }
        Check::PathExists { path } => format!(
            "a path_exists check refuses a write only on an irreversible rule anchored to exactly {path}"
        ),
        _ => "this rule's anchor never reaches the call".to_string(),
    }
}

/// THOR's trial of one write (see this module's doc comment). `root` is the
/// item's registered checkout (`project::registered_checkout`); `reality`
/// is not a parameter of its own - field evidence is read from `env.store`
/// (`attest::field_refusals`) for a check that is not changing.
#[allow(clippy::too_many_arguments)]
pub fn trial(
    env: &Env<'_>,
    write: &Write<'_>,
    root: &Result<PathBuf, RootProblem>,
    session: &SessionCtx,
    corpus: &Corpus,
    disputes: &Disputes,
    history: &[TrialHistory],
) -> Trial {
    let mut work = new_work(env, write, root, session, corpus, disputes, history, Mode::Write);
    let decision = work.decide();
    finish(work, decision)
}

#[allow(clippy::too_many_arguments)]
fn new_work<'a>(
    env: &'a Env<'a>,
    write: &'a Write<'a>,
    root: &'a Result<PathBuf, RootProblem>,
    session: &'a SessionCtx,
    corpus: &'a Corpus,
    disputes: &'a Disputes,
    history: &'a [TrialHistory],
    mode: Mode,
) -> Work<'a> {
    Work {
        env,
        write,
        root,
        session,
        corpus,
        disputes,
        history,
        mode,
        attempts: Vec::new(),
        corpus_summary: TrialCorpus { reached: 0, whole_hits: 0, cut: 0, stopped_forcing: Vec::new() },
        clean_failed: Vec::new(),
        evidence: Vec::new(),
        uncommitted: false,
        file_blob: None,
        used_disputes: TrialDisputes { legitimate_calls: Vec::new(), may_change: Vec::new() },
        accepted: Vec::new(),
        receipt: Vec::new(),
        forcing_words: Vec::new(),
        old_stock: None,
        old_stock_granted: false,
        a4_only: false,
    }
}

fn finish(work: Work<'_>, decision: Decision) -> Trial {
    let item = work.item();
    match decision {
        Decision::Rule { proven } => {
            let verdict = if proven { RecordedVerdict::RuleProven } else { RecordedVerdict::RuleWired };
            let record = work.record(item, verdict, None);
            Trial { outcome: Outcome::Rule { proven }, check: item.check.clone(), receipt: work.receipt, record: Some(record), forcing: vec![] }
        }
        Decision::Undecided { why } => {
            let record = work.record(item, RecordedVerdict::RuleWired, None);
            Trial { outcome: Outcome::Undecided { why }, check: item.check.clone(), receipt: work.receipt, record: Some(record), forcing: vec![] }
        }
        Decision::Note { pending, by_dispute, .. } => {
            let mut stored = item.clone();
            stored.check = None;
            let record = work.record(&stored, RecordedVerdict::Note, pending.clone());
            Trial { outcome: Outcome::Note { pending, by_dispute }, check: None, receipt: work.receipt, record: Some(record), forcing: vec![] }
        }
        Decision::Refuse { code, message, .. } => Trial {
            outcome: Outcome::Refuse { code, message },
            check: None,
            receipt: work.receipt,
            record: None,
            forcing: work.forcing_words,
        },
    }
}

/// The item as a trial stores it: its check as the trial decided, and its
/// `teeth:` tags replaced by the one the verdict implies. `None` for a
/// refusal.
pub fn stored_item(item: &Item, trial: &Trial) -> Option<Item> {
    let tag = match &trial.outcome {
        Outcome::Rule { proven: true } => Some(model::store::TEETH_PROVEN_TAG),
        Outcome::Rule { proven: false } => Some(model::store::TEETH_WIRED_TAG),
        Outcome::Note { .. } => Some(model::store::TEETH_NOTE_TAG),
        Outcome::Undecided { .. } => None,
        Outcome::Refuse { .. } => return None,
    };
    let mut out = item.clone();
    out.check = trial.check.clone();
    out.tags.retain(|t| !t.starts_with(model::store::TEETH_TAG_PREFIX));
    if let Some(tag) = tag {
        out.tags.push(tag.to_string());
    }
    Some(out)
}

// ---------------------------------------------------------------- retest

/// What a retest decided (no write).
#[derive(Debug, Clone)]
pub enum Maintenance {
    /// On the rule track, with this check.
    Rule { check: Check, proven: bool },
    /// On the note track; `because` says why the check (if any) is cleared.
    Note { because: String },
    Blocked(Blocker),
    /// The agent has to act: the one fix, in words.
    NeedsAgent(String),
}

/// A retest's full result: the outcome, and what `apply` needs.
#[derive(Debug, Clone)]
pub struct Retest {
    pub id: String,
    pub outcome: Maintenance,
    /// The rule's project, and the registered working copy the trial used
    /// (or why there is none) - `serve retest` prints it first.
    pub project: Option<String>,
    pub root: Result<PathBuf, RootProblem>,
    /// Every form the trial tried, in order, with what the hook's pipeline
    /// did with it.
    pub attempts: Vec<TrialAttempt>,
    /// Every line of the trial, for a reader.
    pub lines: Vec<String>,
    /// The fingerprint and head of the item that was tried.
    pub fingerprint: String,
    pub head_rev: String,
    /// The item to write and its record, for a rule or a note.
    pub write: Option<(Item, TrialRecord)>,
    /// For a rule that needs the agent: its check bites and only the A4
    /// naming is missing (K23) - the evaluation's machine pass lists those
    /// apart from every other fix.
    pub a4_only: bool,
}

/// The head of `id`: its item and its rev hash, when it has one live head.
fn head(store: &EventStore, id: &str) -> Result<(Item, String), String> {
    let events = store.get_events_by_entity(id).map_err(|e| e.to_string())?;
    let heads = thor_core::cas::compute_head_sets(&events);
    let set = heads.get(id).ok_or_else(|| format!("there is no item '{id}'"))?;
    if set.heads.len() != 1 {
        return Err(format!("'{id}' has {} heads - resolve it first", set.heads.len()));
    }
    let hash = set.heads.iter().next().expect("one head").clone();
    let event: &Event = events.iter().find(|e| e.this_hash == hash).ok_or_else(|| format!("there is no item '{id}'"))?;
    if event.kind == EventKind::FactRetracted {
        return Err(format!("'{id}' was retracted"));
    }
    let item = model::store::parse_body(&event.body).map_err(|e| e.to_string())?;
    Ok((item, hash))
}

/// The disputes and accepted refusals the head's last matching trial
/// recorded - a retest keeps what the writer decided.
fn recorded_disputes(store: &EventStore, id: &str, fp: &str) -> (Disputes, Option<PendingCheck>) {
    let Ok(events) = store.get_events_by_entity(id) else { return (Disputes::default(), None) };
    for e in events.iter().rev() {
        if let Ok(Some(record)) = model::store::trial_of(e) {
            let f = record.fields();
            if f.fingerprint != fp {
                return (Disputes::default(), None);
            }
            return (
                Disputes {
                    legitimate_calls: f.disputes.legitimate_calls.clone(),
                    may_change: f.disputes.may_change.clone(),
                    accept_refusing: f.accept_refusing.clone(),
                },
                f.pending.clone(),
            );
        }
    }
    (Disputes::default(), None)
}

/// Whether `head` - a rule on the rule track, never a note's pending check -
/// is old contains stock whose literal its text does not name (Q24), judged
/// against the recorded A4 cutoff, or, while none is recorded, against the
/// one `apply` would record now (`model::store::prospective_a4_cutoff`).
fn old_stock_candidate(store: &EventStore, id: &str, head: &Item) -> Option<OldContainsStock> {
    if !matches!(head.check, Some(Check::Contains { .. })) || gate::unnamed_literals(head).is_empty() {
        return None;
    }
    let cutoff = model::store::a4_cutoff(store).unwrap_or_else(|| model::store::prospective_a4_cutoff(store));
    let events = store.get_events_by_entity(id).ok()?;
    model::store::old_contains_stock(&events, cutoff)
}

/// Retest rule `id` against its registered checkout and the real calls in
/// `corpus`, with no session (nobody is writing). Writes nothing. Never runs
/// `git log -S` (that label is set at the write and by doctor only).
pub fn retest(store: &EventStore, db: Option<&Path>, id: &str, corpus: &Corpus) -> Result<Retest, String> {
    let (head_item, head_rev) = head(store, id)?;
    let fingerprint = model::store::fingerprint(&head_item);
    let (disputes, pending) = recorded_disputes(store, id, &fingerprint);
    // A note that keeps a pending check is retried with that check (K20).
    let mut item = head_item.clone();
    if item.check.is_none() {
        if let Some(p) = &pending {
            item.check = Some(p.check.clone());
        }
    }
    let root = project::registered_checkout(store, item.project.as_deref());
    let env = Env { store, db };
    let hist = history(store, id, item.check.as_ref());
    let write = Write { item: &item, before: Some(&head_item), because: None, parent_rev: Some(head_rev.clone()) };
    let session = SessionCtx::default();
    let mut work = new_work(&env, &write, &root, &session, corpus, &disputes, &hist, Mode::Retest);
    work.old_stock = old_stock_candidate(store, id, &head_item);
    let decision = work.decide();
    if work.old_stock_granted && model::store::a4_cutoff(store).is_none() {
        let cutoff = work.old_stock.as_ref().map_or(0, |m| m.cutoff);
        work.receipt.push(format!(
            "the A4 cutoff is not recorded yet: --apply records log seq {cutoff} as the cutoff, once, before it writes \
             this verdict, and it never moves after that"
        ));
    }
    let lines = work.receipt.clone();
    let attempts = work.attempts.clone();
    let outcome = match &decision {
        Decision::Rule { proven } => Maintenance::Rule { check: item.check.clone().expect("a rule has a check"), proven: *proven },
        Decision::Undecided { why } => Maintenance::NeedsAgent(why.clone()),
        Decision::Note { dropped, .. } => {
            let because = dropped.clone().unwrap_or_else(|| lines.first().cloned().unwrap_or_else(|| "no tooth bites".to_string()));
            Maintenance::Note { because }
        }
        Decision::Refuse { retest: RetestAs::Blocked(b), .. } => Maintenance::Blocked(b.clone()),
        Decision::Refuse { message, .. } => Maintenance::NeedsAgent(message.clone()),
    };
    // A note keeping a pending check stays that note unless its check now
    // holds and bites (a rule) or is blocked: nothing to write for it.
    let pending_note = head_item.check.is_none() && pending.is_some();
    let outcome = match outcome {
        Maintenance::NeedsAgent(why) | Maintenance::Note { because: why } if pending_note => {
            Maintenance::Note { because: format!("its pending check is still not a working tooth: {why}") }
        }
        other => other,
    };
    let write_out = match &outcome {
        Maintenance::Note { .. } if pending_note => None,
        Maintenance::Rule { proven, .. } => {
            let verdict = if *proven { RecordedVerdict::RuleProven } else { RecordedVerdict::RuleWired };
            let mut stored = item.clone();
            stored.tags.retain(|t| !t.starts_with(model::store::TEETH_TAG_PREFIX));
            stored.tags.push(if *proven { model::store::TEETH_PROVEN_TAG } else { model::store::TEETH_WIRED_TAG }.to_string());
            let record = work.record(&stored, verdict, None);
            Some((stored, record))
        }
        Maintenance::Note { .. } => {
            let mut stored = head_item.clone();
            stored.check = None;
            stored.tags.retain(|t| !t.starts_with(model::store::TEETH_TAG_PREFIX));
            stored.tags.push(model::store::TEETH_NOTE_TAG.to_string());
            let record = work.record(&stored, RecordedVerdict::Note, pending.filter(|_| head_item.check.is_none()));
            Some((stored, record))
        }
        _ => None,
    };
    let project = model::normalize::normalize_project(item.project.as_deref());
    let a4_only = work.a4_only && matches!(outcome, Maintenance::NeedsAgent(_));
    Ok(Retest { id: id.to_string(), outcome, project, root: root.clone(), attempts, lines, fingerprint, head_rev, write: write_out, a4_only })
}

/// Why `apply` wrote nothing.
#[derive(Debug)]
pub enum ApplyError {
    /// The rule changed while it was being tried.
    Moved(String),
    /// A capacity refusal: the agent folds or moves the rule (X9).
    NeedsAgent(String),
    /// Nothing to write (blocked, or needs the agent).
    Nothing(String),
    Store(String),
}

impl std::fmt::Display for ApplyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApplyError::Moved(s) | ApplyError::NeedsAgent(s) | ApplyError::Nothing(s) | ApplyError::Store(s) => f.write_str(s),
        }
    }
}

/// Write a retest's rule or note as one `fact_revised` carrying its record,
/// only while the head is still the one tried (compare-and-swap, X4). A
/// capacity refusal answers "needs the agent" (X9) and writes nothing.
pub fn apply(store: &mut EventStore, retest: &Retest, actor: SystemActor) -> Result<Event, ApplyError> {
    let Some((stored, record)) = &retest.write else {
        return Err(ApplyError::Nothing(format!("nothing to write for '{}': {:?}", retest.id, retest.outcome)));
    };
    let (now, now_rev) = head(store, &retest.id).map_err(ApplyError::Store)?;
    if model::store::fingerprint(&now) != retest.fingerprint || now_rev != retest.head_rev {
        return Err(ApplyError::Moved(format!(
            "REFUSED: {} changed while it was being tried (fingerprint moved) - run retest again",
            retest.id
        )));
    }
    if let Ok(model::store::Capacity::DeadOnArrival(r) | model::store::Capacity::Full(r)) = model::store::capacity(store, stored) {
        let at = stored
            .bindings
            .iter()
            .find_map(|b| match b {
                Binding::Target { value, .. } => Some(value.clone()),
                _ => None,
            })
            .unwrap_or_else(|| "its binding".to_string());
        return Err(ApplyError::NeedsAgent(format!(
            "crowded at {at}: fold this rule into a neighbour or move it to a narrower anchor ({})",
            r.problem
        )));
    }
    // Q24: the first old-stock verdict records the A4 cutoff it was judged
    // by; one recorded by another run in between means the retest judged by
    // a point that no longer stands - it is tried again.
    if let Some(mark) = record.fields().a4_old_stock.as_ref() {
        let recorded = model::store::record_a4_cutoff(store, actor, mark.cutoff).map_err(|e| ApplyError::Store(e.to_string()))?;
        if recorded != mark.cutoff {
            return Err(ApplyError::Moved(format!(
                "REFUSED: {} was tried against an A4 cutoff at log seq {}, but the cutoff recorded is log seq {recorded} - \
                 run retest again",
                retest.id, mark.cutoff
            )));
        }
    }
    let because = match &retest.outcome {
        Maintenance::Note { because } => Some(because.clone()),
        _ => None,
    };
    // A note clears the check on purpose: the comparison baseline says so,
    // or the gate would read the missing check as a silent loss.
    let existing = if stored.check.is_none() && now.check.is_some() {
        gate::ClearedFields { check: true, ..Default::default() }.baseline(&now)
    } else {
        now.clone()
    };
    let written = model::store::revise_tried_scoped(
        store,
        "retest",
        &retest.id,
        actor,
        &existing,
        stored,
        &now,
        because.as_deref(),
        None,
        Some(record),
        Some(&retest.head_rev),
        None,
    );
    match written {
        Ok(event) => Ok(event),
        Err(WriteError::Store(e)) if e.to_string().contains("parent_rev") => {
            Err(ApplyError::Moved(format!("REFUSED: {} changed while it was being tried (fingerprint moved) - run retest again", retest.id)))
        }
        Err(WriteError::Refused(r)) => Err(ApplyError::Store(format!("the write gate refused it: {}. Fix: {}", r.problem, r.fix))),
        Err(e) => Err(ApplyError::Store(e.to_string())),
    }
}

/// `apply`, as the `retest` actor - for a caller outside this module (the
/// backfill loop), which may not build THOR's own actor token itself.
pub fn apply_as_retest(store: &mut EventStore, retest: &Retest) -> Result<Event, ApplyError> {
    apply(store, retest, SystemActor::new("retest"))
}

/// Whether `retest`'s verdict rests on old contains stock (Q24): its record
/// carries THOR's old-stock mark, which only the `retest` actor may write
/// (`model::store`'s Q24 fence) - so a caller that writes verdicts under
/// another of THOR's names writes this one through `apply_as_retest`.
pub fn is_old_stock_verdict(retest: &Retest) -> bool {
    retest.write.as_ref().is_some_and(|(_, record)| record.fields().a4_old_stock.is_some())
}

/// Whether a rule-track `contains` rule has to be tried again at an
/// evaluation start (K9), and why: its record says the literal was not
/// committed yet (`uncommitted`), or the file changed since the trial
/// (`file_blob` is no longer the blob `git hash-object` gives the file now,
/// or the file is gone). `None` for every other item - nothing to try again.
///
/// `root` is the registered checkout; `blobs` holds the current blob hash of
/// every check path that is a readable file (`blob_hashes`). A check on a
/// DIRECTORY is tried on one of its children, so it has no one blob to
/// compare: it is tried again only while its record says `uncommitted`.
pub fn contains_retry_due(
    store: &EventStore,
    id: &str,
    item: &Item,
    root: &Path,
    blobs: &std::collections::HashMap<String, String>,
) -> Option<&'static str> {
    let Some(Check::Contains { path, .. }) = &item.check else { return None };
    let record = model::store::current_record(store, id)?;
    let fields = record.fields();
    if !matches!(fields.verdict, RecordedVerdict::RuleProven | RecordedVerdict::RuleWired) {
        return None;
    }
    if fields.uncommitted {
        return Some("its literal was not committed when it was last tried");
    }
    if root.join(path).is_dir() {
        return None;
    }
    match (fields.file_blob.as_deref(), blobs.get(path).map(String::as_str)) {
        (None, _) => None,
        (Some(then), Some(now)) if then == now => None,
        (Some(_), Some(_)) => Some("its file changed since it was last tried"),
        (Some(_), None) => Some("its file is gone or unreadable since it was last tried"),
    }
}

/// Whether writing `retest`'s verdict would change nothing a reader cares
/// about: the rule's current record already says the same verdict, the same
/// `uncommitted` and the same file blob. A rule tried again at an evaluation
/// start and found exactly as before is left alone, not rewritten.
pub fn same_as_current_record(store: &EventStore, retest: &Retest) -> bool {
    let (Some((_, new)), Some(old)) = (&retest.write, model::store::current_record(store, &retest.id)) else { return false };
    let (new, old) = (new.fields(), old.fields());
    new.verdict == old.verdict && new.uncommitted == old.uncommitted && new.file_blob == old.file_blob
}

/// The current blob hash of each of `paths` (relative to `root`), from ONE
/// `git hash-object` call; a path that is not a readable file has none.
pub fn blob_hashes(root: &Path, paths: &[String]) -> std::collections::HashMap<String, String> {
    let existing: Vec<&String> = paths.iter().filter(|p| root.join(p.as_str()).is_file()).collect();
    if existing.is_empty() {
        return Default::default();
    }
    let mut args: Vec<String> = vec!["hash-object".to_string(), "--".to_string()];
    args.extend(existing.iter().map(|p| git_path(p)));
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let Some(out) = git(root, &arg_refs) else { return Default::default() };
    let hashes: Vec<&str> = out.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    if hashes.len() != existing.len() {
        return Default::default();
    }
    existing.into_iter().cloned().zip(hashes.into_iter().map(str::to_string)).collect()
}

// ------------------------------------------------------------ at the write
//
// Every Rule/Orientation write through a write tool (remember, revise, pin,
// unpin, and a replica's queued write at the drain) goes through here (step
// 8): its trial decides the check it is stored with, its `teeth:` label and
// its record, or refuses it. A write that changes nothing a trial tries
// (tags, falsifier, expires, key) runs no trial and keeps the last matching
// record.

/// What THOR's trial decided for one write.
#[derive(Debug, Clone)]
pub enum WriteTrial {
    /// No trial ran: the kind is never tried, or the write changes nothing a
    /// trial tries. `item` keeps the head's `teeth:` label.
    Unchanged { item: Item, receipt: Vec<String> },
    /// Tried: write `item` (its check and label as the verdict decided) with
    /// `record`. `because` is the trial's own reason when it cleared a check
    /// the item carried before and the writer gave none - it goes into the
    /// item's history.
    Tried { item: Item, record: TrialRecord, outcome: Outcome, receipt: Vec<String>, because: Option<String> },
    /// Refused: nothing may be written on the item. Recorded once as a
    /// `GateRefused` by `teeth-write` (K5).
    Refused { code: RefuseCode, message: String, receipt: Vec<String> },
}

/// The log seq of the trial record `id`'s head is judged by, when one counts.
fn record_seq(store: &EventStore, id: &str) -> Option<i64> {
    let events = store.get_events_by_entity(id).ok()?;
    events.iter().rev().find_map(|e| matches!(model::store::trial_of(e), Ok(Some(_))).then_some(e.seq))
}

/// Record a write the trial refused (`model::store::record_refused_write`),
/// as THOR's `teeth-write` actor.
pub fn record_refused_write(
    store: &mut EventStore,
    session_id: &str,
    id: &str,
    code: RefuseCode,
    forcing: &[String],
    text: &str,
) -> Result<Event, WriteError> {
    model::store::record_refused_write(
        store,
        session_id,
        SystemActor::new(model::store::TEETH_WRITE_ACTOR),
        id,
        code.as_str(),
        forcing,
        text,
    )
}

/// THOR's trial of one write (see this section's banner). `item` is the item
/// exactly as the writer asked for it, `before` the head it replaces (a
/// revise, pin or unpin), `because` the writer's own. `retry_undecided`: a
/// revise that names no field at all still runs the trial when no trial
/// record counts for the head - the one way an old rule is tried without
/// inventing a change to it.
#[allow(clippy::too_many_arguments)]
pub fn trial_for_write(
    store: &mut EventStore,
    db: Option<&Path>,
    session: &SessionCtx,
    corpus: &mut Corpus,
    item: &Item,
    before: Option<&Item>,
    because: Option<&str>,
    disputes: &Disputes,
    retry_undecided: bool,
) -> WriteTrial {
    if !item.kind.can_fire() {
        return WriteTrial::Unchanged { item: item.clone(), receipt: Vec::new() };
    }
    if let Some(before) = before {
        let verdict = model::store::current_verdict(store, &item.id);
        let retry = retry_undecided && matches!(verdict, model::store::CurrentVerdict::Undecided { .. });
        if !retry && model::store::fingerprint(before) == model::store::fingerprint(item) {
            // The label is the head's own: a writer never sets one, and a
            // write that changes nothing tried keeps the one the head has.
            let mut kept = item.clone();
            kept.tags.retain(|t| !t.starts_with(model::store::TEETH_TAG_PREFIX));
            kept.tags.extend(before.tags.iter().filter(|t| t.starts_with(model::store::TEETH_TAG_PREFIX)).cloned());
            let line = match (&verdict, record_seq(store, &item.id)) {
                (model::store::CurrentVerdict::Undecided { why }, _) => format!(
                    "teeth: undecided - {why}. This write changes nothing a trial tries (its text, anchors, check, \
                     project and severity), so none ran"
                ),
                (_, Some(seq)) => format!(
                    "teeth unchanged: the trial at log seq {seq} still matches its text, anchors, check, project and \
                     severity"
                ),
                (_, None) => "teeth unchanged".to_string(),
            };
            return WriteTrial::Unchanged { item: kept, receipt: vec![line] };
        }
    }
    corpus.refresh(store);
    let root = project::registered_checkout(store, item.project.as_deref());
    let hist = history_for_write(store, before, item, because);
    let parent_rev = before.and_then(|_| head(store, &item.id).ok()).map(|(_, rev)| rev);
    let trial_result = {
        let env = Env { store, db };
        let write = Write { item, before, because, parent_rev };
        trial(&env, &write, &root, session, corpus, disputes, &hist)
    };
    match trial_result.outcome.clone() {
        Outcome::Refuse { code, message } => {
            let session_id = session.session_id.as_deref().unwrap_or("mcp");
            let _ = record_refused_write(store, session_id, &item.id, code, &trial_result.forcing, &item.text);
            WriteTrial::Refused { code, message, receipt: trial_result.receipt }
        }
        outcome => {
            let stored = stored_item(item, &trial_result).expect("only a refusal has no stored item");
            let record = trial_result.record.clone().expect("only a refusal has no record");
            let cleared = before.is_some_and(|b| b.check.is_some()) && stored.check.is_none();
            let trial_because = match (cleared, because.map(str::trim).filter(|b| !b.is_empty())) {
                (true, None) => Some(format!(
                    "THOR's trial put it on the note track: {}",
                    trial_result.receipt.first().cloned().unwrap_or_else(|| "no tooth holds".to_string())
                )),
                _ => None,
            };
            WriteTrial::Tried { item: stored, record, outcome, receipt: trial_result.receipt, because: trial_because }
        }
    }
}

/// TEST SUPPORT, shared by every crate whose fixtures need a rule that is
/// already decided - since step 9 a rule with no verdict is owed by the Stop
/// hook and the filing gate whatever its weight, so a fixture about anything
/// else settles its rules first. Runs exactly what `serve retest --id <id>
/// --apply` runs.
pub mod fixture {
    use super::*;

    /// THOR's trial of `id` as it stands, and the rule or note it decides
    /// written as the `retest` actor. Panics when the trial reaches no
    /// verdict it can write (blocked, or needing the agent).
    pub fn decide(store: &mut EventStore, id: &str) -> Maintenance {
        let corpus = Corpus::load(store);
        let db = store.path();
        let tried = retest(store, db.as_deref(), id, &corpus).unwrap_or_else(|e| panic!("{id} cannot be tried: {e}"));
        apply_as_retest(store, &tried).unwrap_or_else(|e| panic!("{id}: {e} ({:?})", tried.outcome));
        tried.outcome
    }
}

/// The actor the unit tests apply a retest as: THOR's own token is built in
/// this module only (`model::store`'s `system_actor_is_built_only_in_the_
/// named_modules`), never in a test file.
#[cfg(test)]
fn retest_actor() -> SystemActor {
    SystemActor::new("retest")
}

#[cfg(test)]
mod tests;
