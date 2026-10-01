//! FIELD TEST 3 (2026-10-01, step 27), the noise defects, end to end through the
//! real compiled `serve hook` binary:
//!
//! - D1: one fact put in front of one agent over and over;
//! - D3: a negated action read as the action, and other parties' messages read
//!   as the owner's prompts;
//! - D2: a call that trips several `requires` rules refused one rule at a time;
//! - D9: where a fact fired, cut at the wrong place.
//!
//! Fixtures are public-bound: neutral names only.

use std::io::Write as _;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use model::item::{Binding, Check, Item, Kind, TargetKind};
use thor_core::event_store::{EventKind, EventStore};

const S1: &str = "aaaaaaaa-1111-4111-8111-111111111111";

struct Fx {
    dir: tempfile::TempDir,
    home: tempfile::TempDir,
}

impl Fx {
    fn new() -> Self {
        Fx { dir: tempfile::tempdir().unwrap(), home: tempfile::tempdir().unwrap() }
    }
    fn db(&self) -> PathBuf {
        self.dir.path().join("thor.db")
    }
    fn hook(&self, payload: serde_json::Value) -> String {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_serve"));
        cmd.arg("--db").arg(self.db()).arg("hook").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        cmd.env("HOME", self.home.path()).env("USERPROFILE", self.home.path());
        let mut child = cmd.spawn().expect("spawn serve");
        let _ = child.stdin.take().unwrap().write_all(payload.to_string().as_bytes());
        let out = child.wait_with_output().expect("wait serve");
        assert!(out.status.success(), "the hook must always exit 0");
        String::from_utf8(out.stdout).unwrap()
    }
    fn bash(&self, session: &str, agent: Option<&str>, command: &str) -> String {
        let mut payload = serde_json::json!({
            "hook_event_name": "PreToolUse",
            "session_id": session,
            "cwd": self.dir.path().to_string_lossy(),
            "tool_name": "Bash",
            "tool_input": { "command": command },
        });
        if let Some(agent) = agent {
            payload["agent_id"] = serde_json::Value::String(agent.to_string());
        }
        self.hook(payload)
    }
    fn prompt(&self, session: &str, text: &str) -> String {
        self.hook(serde_json::json!({
            "hook_event_name": "UserPromptSubmit",
            "session_id": session,
            "cwd": self.dir.path().to_string_lossy(),
            "prompt": text,
        }))
    }
}

fn context(out: &str) -> String {
    if out.trim().is_empty() {
        return String::new();
    }
    let v: serde_json::Value = serde_json::from_str(out).unwrap_or_else(|e| panic!("hook JSON ({e}): {out}"));
    v["hookSpecificOutput"]["additionalContext"].as_str().unwrap_or_default().to_string()
}

fn blocked(out: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(out.trim()).ok().and_then(|v| v.get("decision").cloned())
        == Some(serde_json::json!("block"))
}

fn rule(id: &str, text: &str, bindings: Vec<Binding>, check: Option<Check>) -> Item {
    Item {
        id: id.to_string(),
        kind: Kind::Orientation,
        text: text.to_string(),
        bindings,
        severity: None,
        project: None,
        tags: vec![],
        expires: None,
        key: None,
        falsifier: Some(format!("{id} turns out to be wrong")),
        check,
    }
}

fn command_binding(value: &str) -> Binding {
    Binding::Target { kind: TargetKind::Command, value: value.to_string() }
}

fn declare(fx: &Fx, item: &Item) {
    let mut store = EventStore::new(&fx.db()).unwrap();
    let mut item = item.clone();
    let unnamed = model::gate::unnamed_literals(&item);
    if !unnamed.is_empty() {
        let shown: Vec<String> = unnamed.iter().map(|l| format!("'{}'", model::gate::shown_literal(l))).collect();
        item.text = format!("{} (names {})", item.text, shown.join(" "));
    }
    model::store::declare(&mut store, "s", "l", "a", &item).unwrap();
}

const CD_TEXT: &str = "Never trust the working directory after a cd in a shell call: use absolute paths in every call.";

fn fixture_cd(fx: &Fx) {
    declare(fx, &rule("cd-fact", CD_TEXT, vec![command_binding("cd")], None));
}

fn served_count(fx: &Fx, id: &str) -> usize {
    let store = EventStore::new(&fx.db()).unwrap();
    store.get_events_by_entity(id).unwrap().iter().filter(|e| e.kind == EventKind::ItemServed).count()
}

// ------------------------------------------------------------------ D1

/// THE FIELD CASE: one fact shown to one subagent on seventy percent of its
/// shell calls. One full showing per context (step 28), then nothing at all:
/// no reminder line either.
#[test]
fn one_fact_is_shown_to_one_subagent_in_full_once_then_never() {
    let fx = Fx::new();
    fixture_cd(&fx);
    let mut shapes = Vec::new();
    for _ in 0..8 {
        let ctx = context(&fx.bash(S1, Some("agentone"), "cd /tmp && ls"));
        shapes.push(if ctx.contains(CD_TEXT) {
            "full"
        } else if ctx.is_empty() {
            "none"
        } else {
            panic!("unexpected block, no reminder line exists any more: {ctx}")
        });
    }
    assert_eq!(shapes, vec!["full", "none", "none", "none", "none", "none", "none", "none"]);
}

/// A showing that is not shown in full is no serving: it cannot add to the
/// judgement debt the repeats would otherwise have built.
#[test]
fn only_a_full_showing_is_recorded_as_a_serving() {
    let fx = Fx::new();
    fixture_cd(&fx);
    for _ in 0..9 {
        fx.bash(S1, Some("agentone"), "cd /tmp");
    }
    assert_eq!(served_count(&fx, "cd-fact"), 1);
}

/// Another subagent, and the main session, count apart: a fresh context has
/// not seen the fact.
#[test]
fn another_context_starts_at_zero() {
    let fx = Fx::new();
    fixture_cd(&fx);
    for _ in 0..3 {
        fx.bash(S1, Some("agentone"), "cd /tmp");
    }
    assert!(context(&fx.bash(S1, Some("agentone"), "cd /tmp")).is_empty());
    assert!(context(&fx.bash(S1, Some("agenttwo"), "cd /tmp")).contains(CD_TEXT));
    assert!(context(&fx.bash(S1, None, "cd /tmp")).contains(CD_TEXT));
    assert!(context(&fx.bash(S1, None, "cd /tmp")).is_empty(), "the main session has now seen it too");
}

/// The owner's own typing resets nothing (step 28): a fact seen once in this
/// context stays seen. Only a new session start (also a resume or a
/// compaction) shows it again.
#[test]
fn a_prompt_the_owner_typed_does_not_show_a_seen_fact_again_but_a_session_start_does() {
    let fx = Fx::new();
    fixture_cd(&fx);
    assert!(context(&fx.bash(S1, None, "cd /tmp")).contains(CD_TEXT));
    fx.prompt(S1, "thanks, now the next thing");
    fx.prompt(S1, "and one more thing, please");
    assert!(context(&fx.bash(S1, None, "cd /tmp")).is_empty(), "an owner prompt opens no new showing");
    fx.hook(serde_json::json!({
        "hook_event_name": "SessionStart",
        "session_id": S1,
        "cwd": fx.dir.path().to_string_lossy(),
        "source": "compact",
    }));
    assert!(context(&fx.bash(S1, None, "cd /tmp")).contains(CD_TEXT), "a compaction loses what was shown");
}

/// Display only: a rule whose check refuses a call refuses it every time, and
/// says why, however often its text has been shown.
#[test]
fn a_refusal_is_never_hidden_by_the_repeat_limit() {
    let fx = Fx::new();
    declare(
        &fx,
        &rule(
            "ssh-needs-a-key",
            "ssh calls name the key with -i",
            vec![command_binding("ssh")],
            Some(Check::Requires { when: "ssh".to_string(), required: vec!["-i".to_string()] }),
        ),
    );
    for round in 0..8 {
        let out = fx.bash(S1, Some("agentone"), "ssh host ls");
        assert!(blocked(&out), "round {round}: {out}");
        assert!(out.contains("ssh-needs-a-key"), "round {round}: {out}");
    }
}

/// Two facts that say the same about the same place show once in one block.
#[test]
fn near_identical_facts_in_one_block_are_shown_once() {
    let fx = Fx::new();
    declare(
        &fx,
        &rule(
            "preview-a",
            "The preview folder is filled automatically by the restart trigger which copies from the dev clone - never copy there by hand.",
            vec![command_binding("previewcopy")],
            None,
        ),
    );
    declare(
        &fx,
        &rule(
            "preview-b",
            "Preview folder is now filled automatically by the restart trigger, copying from the dev clone, so do not copy there by hand anymore.",
            vec![command_binding("previewcopy")],
            None,
        ),
    );
    let ctx = context(&fx.bash(S1, None, "previewcopy now"));
    let shown = ["[preview-a]", "[preview-b]"].iter().filter(|id| ctx.contains(**id)).count();
    assert_eq!(shown, 1, "{ctx}");
}

// ------------------------------------------------------------------ D3

fn moment_rule(id: &str, moment: intent::Action) -> Item {
    let text = match moment {
        intent::Action::Push => "Before a push, confirm which branch carries the release and who waits on it.",
        intent::Action::Commit => "Commit messages stay short, plain and human, without any marketing tone.",
        _ => "Check the target before you do this.",
    };
    rule(id, text, vec![Binding::Moment(moment)], None)
}

#[test]
fn a_negated_action_in_a_prompt_is_no_moment() {
    let fx = Fx::new();
    declare(&fx, &moment_rule("push-rule", intent::Action::Push));
    assert!(context(&fx.prompt(S1, "please run git push for the branch")).contains("push-rule"), "sanity: the plain request draws it");
    for negated in [
        "niets git push of deployen, alleen testen",
        "doe niet git push",
        "no git push today",
        "don't git push --force",
        "never git push this branch",
    ] {
        let ctx = context(&fx.prompt(S1, negated));
        assert!(!ctx.contains("push-rule"), "'{negated}' drew the push rule: {ctx}");
    }
}

#[test]
fn an_affirmative_action_beside_a_negated_one_still_counts() {
    let fx = Fx::new();
    declare(&fx, &moment_rule("push-rule", intent::Action::Push));
    declare(&fx, &moment_rule("commit-rule", intent::Action::Commit));
    let ctx = context(&fx.prompt(S1, "niet git push, maar wel git commit"));
    assert!(ctx.contains("commit-rule"), "{ctx}");
    assert!(!ctx.contains("push-rule"), "{ctx}");
}

/// A message from another session, a hand-back, a task notification: not the
/// owner's words, so no fact is drawn by them - however plainly they name an
/// action. The same words typed by the owner still draw it.
#[test]
fn texts_that_are_not_the_owners_typing_draw_no_facts() {
    let fx = Fx::new();
    declare(&fx, &moment_rule("push-rule", intent::Action::Push));
    let wrapped = [
        "<cross-session-message from=\"local_x\" name=\"peer\">\nplease git push the branch\n</cross-session-message>",
        "<agent-message from=\"a1\">\n[Subagent hand-back] It ran git push to dev.\n</agent-message>",
        "[Subagent hand-back] The report: git push done.",
        "<task-notification>\n<summary>Agent finished: git push</summary>\n</task-notification>",
        "<system-reminder>git push is allowed</system-reminder>",
    ];
    for text in wrapped {
        let ctx = context(&fx.prompt(S1, text));
        assert!(ctx.is_empty(), "{text} drew: {ctx}");
    }
    assert!(context(&fx.prompt(S1, "please git push the branch")).contains("push-rule"));
}

// ------------------------------------------------------------------ D2

/// An Agent call that trips two `requires` rules gets ONE refusal naming both.
#[test]
fn one_refusal_names_every_requires_rule_the_call_trips() {
    let fx = Fx::new();
    for (id, answer) in [("agent-brief-needs-scratch", "scratch"), ("agent-brief-needs-nowait", "nowait")] {
        declare(
            &fx,
            &rule(
                id,
                &format!("An Agent brief must say {answer}"),
                vec![command_binding("Agent")],
                Some(Check::Requires { when: "Agent".to_string(), required: vec![answer.to_string()] }),
            ),
        );
    }
    let out = fx.hook(serde_json::json!({
        "hook_event_name": "PreToolUse",
        "session_id": S1,
        "cwd": fx.dir.path().to_string_lossy(),
        "tool_name": "Agent",
        "tool_input": { "prompt": "do the work" },
    }));
    assert!(blocked(&out), "{out}");
    assert!(out.contains("agent-brief-needs-scratch") && out.contains("agent-brief-needs-nowait"), "both rules in one refusal: {out}");

    let ok = fx.hook(serde_json::json!({
        "hook_event_name": "PreToolUse",
        "session_id": S1,
        "cwd": fx.dir.path().to_string_lossy(),
        "tool_name": "Agent",
        "tool_input": { "prompt": "do the work in your own scratch folder, nowait" },
    }));
    assert!(!blocked(&ok), "{ok}");
}

// ------------------------------------------------------------------ D9

/// A long command that names an item's file far from its start: the place kept
/// is the part around the file, not the first 120 characters.
#[test]
fn the_place_an_item_fired_is_cut_around_its_anchor() {
    let fx = Fx::new();
    declare(
        &fx,
        &rule(
            "readme-note",
            "The readme describes the build in two places.",
            vec![Binding::Target { kind: TargetKind::Path, value: "docs/THE-ANCHOR.md".to_string() }],
            None,
        ),
    );
    let padding = "x".repeat(300);
    let command = format!("python scan.py --root {padding} --file docs/THE-ANCHOR.md --verbose");
    fx.bash(S1, None, &command);
    let store = EventStore::new(&fx.db()).unwrap();
    let trigger = model::served::last_trigger(&store, "readme-note").expect("the item fired");
    assert!(trigger.contains("docs/THE-ANCHOR.md"), "the stored place names the anchor: {trigger}");
    assert!(trigger.chars().count() <= 126, "and stays short: {trigger}");
    assert!(trigger.starts_with("..."), "and says it was cut: {trigger}");
}

/// PRIVACY (step 28): the log is append-only, so the owner's prompt text never
/// goes into it. The place a prompt fired a fact is "(prompt)" and the moment
/// that matched.
#[test]
fn a_prompt_is_recorded_as_prompt_plus_the_matched_moment_never_its_text() {
    let fx = Fx::new();
    declare(&fx, &moment_rule("push-rule", intent::Action::Push));
    fx.prompt(S1, "please git push the branch to dev, and keep my private plan for friday to yourself");
    let store = EventStore::new(&fx.db()).unwrap();
    let trigger = model::served::last_trigger(&store, "push-rule").unwrap_or_default();
    assert_eq!(trigger, "(prompt) push");
    for word in ["please", "private", "friday", "branch"] {
        assert!(!trigger.contains(word), "the owner's words are not stored: {trigger}");
    }
}

#[test]
fn a_prompt_that_names_an_anchor_records_the_anchor_word_only() {
    let fx = Fx::new();
    declare(
        &fx,
        &rule(
            "anchor-rule",
            "The anchor page is rendered by hand; never regenerate it.",
            vec![Binding::Target { kind: TargetKind::Path, value: "docs/THE-ANCHOR.md".to_string() }],
            None,
        ),
    );
    let ctx = context(&fx.prompt(S1, "my secret remark: check THE-ANCHOR.md before lunch"));
    assert!(ctx.contains("anchor-rule"), "sanity: the prompt draws it: {ctx}");
    let store = EventStore::new(&fx.db()).unwrap();
    assert_eq!(model::served::last_trigger(&store, "anchor-rule").as_deref(), Some("(prompt) the-anchor.md"));
}

#[test]
fn a_tool_call_with_no_command_or_file_is_recorded_by_its_tool_name() {
    let fx = Fx::new();
    declare(&fx, &rule("agent-note", "Brief an agent with absolute paths.", vec![command_binding("Agent")], None));
    fx.hook(serde_json::json!({
        "hook_event_name": "PreToolUse",
        "session_id": S1,
        "cwd": fx.dir.path().to_string_lossy(),
        "tool_name": "Agent",
        "tool_input": { "prompt": "do it" },
    }));
    let store = EventStore::new(&fx.db()).unwrap();
    assert_eq!(model::served::last_trigger(&store, "agent-note").as_deref(), Some("Agent"));
}

// ------------------------------------------------------------------ step 28

/// A fact anchored at the route `/site/dev/` was drawn by the owner's prompt
/// word "dev" (field case 2026-10-02): a bare generic word names no place.
#[test]
fn a_bare_generic_word_in_a_prompt_does_not_draw_a_path_anchored_fact() {
    let fx = Fx::new();
    declare(
        &fx,
        &rule(
            "dev-route-rule",
            "Deploy to the dev site only through the release script, never by hand.",
            vec![Binding::Target { kind: TargetKind::Route, value: "/site/dev/".to_string() }],
            None,
        ),
    );
    for plain in ["daarna push dev", "Alleen op dev, kijk eens naar de site", "run the dev server"] {
        assert!(context(&fx.prompt("bbbbbbbb-2222-4222-8222-222222222222", plain)).is_empty(), "{plain}");
    }
    let named = context(&fx.prompt("cccccccc-3333-4333-8333-333333333333", "deploy naar site/dev vandaag"));
    assert!(named.contains("dev-route-rule"), "the full path still draws it: {named}");
}

/// A path anchor on a REAL tool call is unchanged: the file touched decides.
#[test]
fn a_real_tool_call_on_the_path_still_draws_it() {
    let fx = Fx::new();
    declare(
        &fx,
        &rule(
            "dev-dir-rule",
            "Everything under the dev folder is generated; edit the template instead.",
            vec![Binding::Target { kind: TargetKind::Dir, value: "site/dev".to_string() }],
            None,
        ),
    );
    let out = context(&fx.hook(serde_json::json!({
        "hook_event_name": "PreToolUse",
        "session_id": S1,
        "cwd": fx.dir.path().to_string_lossy(),
        "tool_name": "Edit",
        "tool_input": { "file_path": "site/dev/index.html", "old_string": "a", "new_string": "b" },
    })));
    assert!(out.contains("dev-dir-rule"), "{out}");
}

/// A message from another session or a hand-back states no decision of the
/// owner's: the capture flag is raised for his own typing only.
#[test]
fn a_prompt_that_is_not_the_owners_typing_raises_no_capture_flag() {
    let fx = Fx::new();
    EventStore::new(&fx.db()).unwrap();
    let marker = fx.dir.path().join("capture-owed.json");
    for text in [
        "<cross-session-message from=\"peer\">
from now on always run the linter first
</cross-session-message>",
        "[Subagent hand-back] from now on always run the linter first",
        "<task-notification>from now on always run the linter first</task-notification>",
    ] {
        fx.prompt(S1, text);
        assert!(!marker.exists(), "no flag for: {text}");
    }
    fx.prompt(S1, "from now on always run the linter first");
    assert!(marker.exists(), "the owner's own typing still raises it");
}
