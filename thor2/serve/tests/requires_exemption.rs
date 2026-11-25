//! A `requires` rule can carry an exemption, and the owner can change a rule
//! that needs his go from any session, end to end through the real `serve hook`.
//!
//! Part A: a tool call that holds only the values the exemption lists passes
//! without the rule's answer (for a rule that needs the owner's go, without his
//! words too); every other call is judged as before. Part B: the refusal of a
//! rule that needs the owner's go says the owner can change it himself, and the
//! hook leaves the record that lets the tool server accept his quoted message.

use std::io::Write as _;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use model::item::{Binding, Check, Item, Kind, TargetKind};
use thor_core::event_store::EventStore;

const SESSION: &str = "0b0b0b0b-1111-4222-8333-444444444444";
const TOOL: &str = "mcp__zz__send_message";
const MARKER: &str = "go-owner";
const ME: &str = "me@example.test";
const RULE: &str = "mail-needs-the-owners-go";

struct Fx {
    dir: tempfile::TempDir,
    home: tempfile::TempDir,
}

impl Fx {
    fn new(tags: Vec<String>) -> Fx {
        let fx = Fx { dir: tempfile::tempdir().unwrap(), home: tempfile::tempdir().unwrap() };
        let mut store = EventStore::new(&fx.db()).unwrap();
        let item = Item {
            id: RULE.to_string(),
            kind: Kind::Rule,
            text: format!("A mail needs the owner's explicit go ({MARKER}), except a mail only to {ME}."),
            bindings: vec![Binding::Target { kind: TargetKind::Command, value: TOOL.to_string() }],
            severity: Some(model::item::Severity::Costly),
            project: None,
            tags,
            expires: None,
            key: None,
            falsifier: Some("the owner lets agents mail anyone on their own".to_string()),
            check: Some(Check::Requires {
                when: TOOL.to_string(),
                required: vec![MARKER.to_string()],
                exempt: [format!("to={ME}"), format!("cc={ME}"), format!("bcc={ME}"), "draftId=".to_string()].to_vec(),
            }),
        };
        model::store::declare(&mut store, "s", "l", "a", &item).expect("fixture rule");
        fx
    }
    fn db(&self) -> PathBuf {
        self.dir.path().join("thor.db")
    }
    fn transcript(&self) -> PathBuf {
        let folder = self.home.path().join(".claude").join("projects").join("C--fx");
        std::fs::create_dir_all(&folder).unwrap();
        folder.join(format!("{SESSION}.jsonl"))
    }
    fn write_transcript(&self, lines: &[serde_json::Value]) {
        let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
        std::fs::write(self.transcript(), text).unwrap();
    }
    /// A transcript whose owner messages were all typed: the hook recorded each prompt.
    fn write_typed_transcript(&self, lines: &[serde_json::Value]) {
        self.write_transcript(lines);
        for text in lines.iter().filter_map(|l| serve::attest::owner_text(&l.to_string())) {
            serve::prompt_record::record(&self.db(), SESSION, &text, 0);
        }
    }
    fn hook(&self, payload: serde_json::Value) -> String {
        self.hook_in(payload, &[])
    }
    /// The hook run with Claude Code's own environment markers (`CLAUDE_CODE_ENTRYPOINT`,
    /// `CLAUDE_CODE_SESSION_ATTENDED`); with none given, neither is set.
    fn hook_in(&self, payload: serde_json::Value, env: &[(&str, &str)]) -> String {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_serve"));
        cmd.arg("--db").arg(self.db()).arg("hook").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        cmd.env("HOME", self.home.path()).env("USERPROFILE", self.home.path());
        cmd.env_remove("CLAUDE_CODE_ENTRYPOINT").env_remove("CLAUDE_CODE_SESSION_ATTENDED");
        for (k, v) in env {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn().expect("spawn serve");
        let _ = child.stdin.take().unwrap().write_all(payload.to_string().as_bytes());
        let out = child.wait_with_output().expect("wait serve");
        assert!(out.status.success(), "the hook must always exit 0");
        String::from_utf8(out.stdout).unwrap()
    }
    fn call(&self, tool: &str, input: serde_json::Value) -> String {
        self.hook(serde_json::json!({
            "hook_event_name": "PreToolUse",
            "session_id": SESSION,
            "transcript_path": self.transcript().to_string_lossy(),
            "cwd": self.dir.path().to_string_lossy(),
            "tool_name": tool,
            "tool_input": input,
        }))
    }
    fn mail(&self, input: serde_json::Value) -> String {
        self.call(TOOL, input)
    }
    /// The owner types a prompt: Claude Code runs the UserPromptSubmit hook, which records it.
    fn type_prompt(&self, text: &str) {
        self.type_prompt_in(text, &[]);
    }
    fn type_prompt_in(&self, text: &str, env: &[(&str, &str)]) {
        self.hook_in(serde_json::json!({
            "hook_event_name": "UserPromptSubmit",
            "session_id": SESSION,
            "transcript_path": self.transcript().to_string_lossy(),
            "cwd": self.dir.path().to_string_lossy(),
            "prompt": text,
        }), env);
    }
}

fn owner(text: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "user",
        "isSidechain": false,
        "origin": {"kind": "human"},
        "userType": "external",
        "message": {"role": "user", "content": text},
        "timestamp": "2026-10-01T12:00:00.000Z",
    })
}

fn blocked(out: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(out.trim()).ok().and_then(|v| v.get("decision").cloned())
        == Some(serde_json::json!("block"))
}

fn approval() -> Vec<String> {
    vec![model::store::OWNER_APPROVAL_TAG.to_string()]
}

#[test]
fn a_mail_only_to_the_owners_own_address_passes_and_every_other_mail_is_refused() {
    let fx = Fx::new(approval());
    fx.write_transcript(&[owner("please send me the notes")]);
    let own = fx.mail(serde_json::json!({"to": [ME], "subject": "notes", "body": "here"}));
    assert!(!blocked(&own), "a mail only to the owner owes no go: {own}");
    assert!(!blocked(&fx.mail(serde_json::json!({"to": ["ME@Example.Test "], "subject": "x"}))), "case and spaces do not matter");

    for (why, input) in [
        ("a second recipient", serde_json::json!({"to": [ME, "other@example.test"]})),
        ("a foreign cc", serde_json::json!({"to": [ME], "cc": ["other@example.test"]})),
        ("a foreign bcc", serde_json::json!({"to": [ME], "bcc": ["other@example.test"]})),
        ("a draft, which sends something else", serde_json::json!({"to": [ME], "draftId": "d1"})),
        ("no recipient at all", serde_json::json!({"subject": "x", "body": "y"})),
        ("only a foreign recipient", serde_json::json!({"to": ["other@example.test"]})),
    ] {
        let out = fx.mail(input);
        assert!(blocked(&out), "{why} must still need the owner's go: {out}");
        assert!(out.contains(RULE) && out.contains("explicit go"), "{why}: {out}");
    }
    // The owner saying the word himself still lets a foreign mail through.
    fx.write_typed_transcript(&[owner("ok, mail them - go-owner")]);
    assert!(!blocked(&fx.mail(serde_json::json!({"to": ["other@example.test"]}))));
}

#[test]
fn the_exemption_works_on_a_rule_the_call_itself_can_answer_too() {
    let fx = Fx::new(Vec::new());
    fx.write_transcript(&[owner("hello")]);
    assert!(!blocked(&fx.mail(serde_json::json!({"to": [ME]}))), "exempt, no answer owed");
    let foreign = fx.mail(serde_json::json!({"to": ["other@example.test"]}));
    assert!(blocked(&foreign) && foreign.contains("none of"), "{foreign}");
    assert!(!blocked(&fx.mail(serde_json::json!({"to": ["other@example.test"], "body": MARKER}))), "the call's own answer still counts");
}

#[test]
fn the_refusal_says_the_owner_can_change_the_rule_himself() {
    let fx = Fx::new(approval());
    fx.write_transcript(&[owner("please look at it")]);
    let out = fx.mail(serde_json::json!({"to": ["other@example.test"]}));
    assert!(blocked(&out), "{out}");
    for needed in ["change or lift this rule himself", "mcp__thor__revise", "whole message", "double quotes", "`because`"] {
        assert!(out.contains(needed), "the refusal must say '{needed}': {out}");
    }
    // The variant for a transcript THOR cannot read says it too.
    let unreadable = fx.hook(serde_json::json!({
        "hook_event_name": "PreToolUse",
        "session_id": SESSION,
        "transcript_path": fx.home.path().join("missing.jsonl").to_string_lossy(),
        "cwd": fx.dir.path().to_string_lossy(),
        "tool_name": TOOL,
        "tool_input": {"to": ["other@example.test"]},
    }));
    assert!(blocked(&unreadable) && unreadable.contains("change or lift this rule himself"), "{unreadable}");
}

#[test]
fn the_hook_records_the_owners_quoted_message_so_the_tool_server_can_accept_it() {
    let fx = Fx::new(approval());
    let message = "Ja, laat mail-needs-the-owners-go maar toe voor mails aan mijzelf, dat mag zonder codewoord.";
    fx.write_transcript(&[owner(message)]);
    let because = format!("the owner asked for it: \"{message}\"");
    let revise = serde_json::json!({"id": RULE, "check_exempt": [format!("to={ME}")], "because": because});
    assert!(!serve::unpin_gate::owner_go_recorded(&fx.db(), RULE, &because, Some(SESSION), serve::time::now_unix()));
    // The owner types the message: the hook writes THOR's own record of it.
    fx.type_prompt(message);
    let out = fx.call("mcp__thor__revise", revise);
    assert!(!blocked(&out), "the hook never blocks the revise itself: {out}");
    assert!(
        serve::unpin_gate::owner_go_recorded(&fx.db(), RULE, &because, Some(SESSION), serve::time::now_unix()),
        "the owner's whole message, quoted, is on record"
    );
    // A piece of his message leaves no record.
    let piece = "the owner asked for it: \"laat mail-needs-the-owners-go maar toe\"";
    fx.call("mcp__thor__revise", serde_json::json!({"id": RULE, "because": piece}));
    assert!(!serve::unpin_gate::owner_go_recorded(&fx.db(), RULE, piece, Some(SESSION), serve::time::now_unix()));
}

#[test]
fn only_a_prompt_in_a_session_a_person_reads_is_recorded_as_the_owners_word() {
    // The real markers, checked live in the desktop app: CLAUDE_CODE_ENTRYPOINT=claude-desktop and
    // CLAUDE_CODE_SESSION_ATTENDED=1.
    let recorded = |env: &[(&str, &str)]| {
        let fx = Fx::new(approval());
        fx.type_prompt_in("Ja, haal pinned-x maar weg, ik heb hem vervangen.", env);
        serve::prompt_record::has_any(&fx.db(), Some(SESSION))
    };
    assert!(recorded(&[("CLAUDE_CODE_ENTRYPOINT", "claude-desktop"), ("CLAUDE_CODE_SESSION_ATTENDED", "1")]), "the desktop app");
    assert!(recorded(&[("CLAUDE_CODE_ENTRYPOINT", "cli")]), "the terminal");
    assert!(recorded(&[("CLAUDE_CODE_ENTRYPOINT", "claude-vscode")]), "an IDE extension");
    assert!(recorded(&[]), "an unknown kind of session is a person's, as before");
    assert!(recorded(&[("CLAUDE_CODE_ENTRYPOINT", "sdk-cli"), ("CLAUDE_CODE_SESSION_ATTENDED", "1")]), "an SDK session Claude Code marks as attended");
    for headless in ["sdk-cli", "sdk-ts", "sdk-py"] {
        assert!(!recorded(&[("CLAUDE_CODE_ENTRYPOINT", headless)]), "{headless} without the attended marker is no owner");
        assert!(!recorded(&[("CLAUDE_CODE_ENTRYPOINT", headless), ("CLAUDE_CODE_SESSION_ATTENDED", "0")]), "{headless}, marker 0");
    }
}

#[test]
fn a_message_only_in_the_saved_conversation_leaves_no_record_and_nothing_can_run_the_hook_by_hand() {
    let fx = Fx::new(approval());
    let message = "Ja, laat mail-needs-the-owners-go maar toe voor mails aan mijzelf, dat mag zonder codewoord.";
    // The line is in the saved conversation, but the owner never typed it: the hook never saw a prompt.
    fx.write_transcript(&[owner(message)]);
    let because = format!("the owner asked for it: \"{message}\"");
    let revise = serde_json::json!({"id": RULE, "check_exempt": [format!("to={ME}")], "because": because});
    let out = fx.call("mcp__thor__revise", revise.clone());
    assert!(!blocked(&out), "the hook never blocks the revise itself: {out}");
    assert!(
        !serve::unpin_gate::owner_go_recorded(&fx.db(), RULE, &because, Some(SESSION), serve::time::now_unix()),
        "a line found only in the saved conversation counts for nothing"
    );
    // Running THOR's hook by hand would forge the record: a shell command that does it is refused.
    let forge = fx.call("Bash", serde_json::json!({"command": "echo payload | serve.exe --db x hook"}));
    assert!(blocked(&forge) && forge.contains("runs THOR's own hook"), "{forge}");
    let reading = fx.call("Bash", serde_json::json!({"command": "serve.exe --db x status"}));
    assert!(!blocked(&reading), "{reading}");
    // And a prompt the owner really types is recorded.
    fx.type_prompt(message);
    fx.call("mcp__thor__revise", revise);
    assert!(serve::unpin_gate::owner_go_recorded(&fx.db(), RULE, &because, Some(SESSION), serve::time::now_unix()));
}
