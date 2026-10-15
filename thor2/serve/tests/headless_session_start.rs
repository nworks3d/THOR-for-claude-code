//! A headless session is not served what is written for the owner's interactive
//! chat, and a session start ranks what this project has been using first
//! (step 31).
//!
//! Field test 6 (2026-10-02): a `claude -p` run was served "delegate everything
//! to a background agent" facts, did so, and its worker could get no shell
//! permission, so nothing was done. Such a fact is tagged `interactive-only`; it
//! stays out of the session start and the prompt injection of a session whose
//! entrypoint is an `sdk...` one. The same run also found 28 of 33 start facts
//! beside the point, so the start block puts the facts this project's own
//! sessions were shown lately first.

use model::item::{Binding, Item, Kind, Severity, TargetKind};
use model::store;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use thor_core::event_store::EventStore;

fn always(id: &str, text: &str, severity: Option<Severity>, tags: &[&str]) -> Item {
    let mut all: Vec<String> = tags.iter().map(|t| t.to_string()).collect();
    all.push(format!("{}a fixture standing rule without a literal", store::NO_LITERAL_REASON_PREFIX));
    Item {
        id: id.to_string(),
        kind: Kind::Rule,
        text: text.to_string(),
        bindings: vec![Binding::Always],
        severity,
        project: None,
        tags: all,
        expires: None,
        key: None,
        falsifier: Some(format!("{id} turns out to be wrong")),
        check: None,
    }
}

fn hook(db: &Path, cwd: &Path, entrypoint: Option<&str>, payload: serde_json::Value) -> String {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_serve"));
    cmd.arg("--db").arg(db).arg("hook").current_dir(cwd).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    cmd.env_remove("CLAUDE_CODE_ENTRYPOINT");
    if let Some(entrypoint) = entrypoint {
        cmd.env("CLAUDE_CODE_ENTRYPOINT", entrypoint);
    }
    let mut child = cmd.spawn().unwrap();
    let _ = child.stdin.take().unwrap().write_all(payload.to_string().as_bytes());
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8(out.stdout).unwrap();
    if stdout.trim().is_empty() {
        return String::new();
    }
    let v: serde_json::Value = serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("hook JSON ({e}): {stdout}"));
    v["hookSpecificOutput"]["additionalContext"].as_str().unwrap_or_default().to_string()
}

fn start(db: &Path, cwd: &Path, entrypoint: Option<&str>, session: &str, transcript: Option<&Path>) -> String {
    let mut payload = serde_json::json!({ "session_id": session, "hook_event_name": "SessionStart", "source": "startup", "cwd": cwd.to_string_lossy() });
    if let Some(t) = transcript {
        payload["transcript_path"] = serde_json::Value::String(t.to_string_lossy().to_string());
    }
    hook(db, cwd, entrypoint, payload)
}

struct Fx {
    dir: tempfile::TempDir,
    db: std::path::PathBuf,
}

fn fixture() -> Fx {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("store.db");
    let mut s = EventStore::new(&db).unwrap();
    store::declare(&mut s, "s", "l", "a", &always("plain-rule", "PLAINTEXT keep going until the task is done.", Some(Severity::Costly), &[])).unwrap();
    store::declare(
        &mut s,
        "s",
        "l",
        "a",
        &always("chat-rule", "CHATTEXT delegate everything to a background agent.", Some(Severity::Costly), &["interactive-only"]),
    )
    .unwrap();
    for (id, text, tag) in [
        ("prompt-plain", "PROMPTPLAIN the pdf code embeds fonts.", None),
        ("prompt-chat", "PROMPTCHAT answer in chat first.", Some("interactive-only")),
    ] {
        let item = Item {
            id: id.to_string(),
            kind: Kind::Orientation,
            text: text.to_string(),
            bindings: vec![Binding::Target { kind: TargetKind::Command, value: "pdftool".to_string() }],
            severity: None,
            project: None,
            tags: tag.into_iter().map(str::to_string).collect(),
            expires: None,
            key: None,
            falsifier: Some(format!("{id} turns out to be wrong")),
            check: None,
        };
        store::declare(&mut s, "s", "l", "a", &item).unwrap();
    }
    std::fs::create_dir_all(dir.path().join("work")).unwrap();
    Fx { dir, db }
}

fn cwd(fx: &Fx) -> std::path::PathBuf {
    fx.dir.path().join("work")
}

#[test]
fn a_headless_start_is_not_served_the_interactive_only_facts() {
    let fx = fixture();
    let headless = start(&fx.db, &cwd(&fx), Some("sdk-cli"), "s-headless", None);
    assert!(headless.contains("PLAINTEXT"), "{headless}");
    assert!(!headless.contains("CHATTEXT"), "{headless}");
    for interactive in ["claude-desktop", "cli"] {
        let block = start(&fx.db, &cwd(&fx), Some(interactive), &format!("s-{interactive}"), None);
        assert!(block.contains("PLAINTEXT") && block.contains("CHATTEXT"), "{interactive}: {block}");
    }
    let unknown = start(&fx.db, &cwd(&fx), None, "s-unknown", None);
    assert!(unknown.contains("CHATTEXT"), "a session of unknown kind is served as before: {unknown}");
}

#[test]
fn the_transcript_names_a_headless_session_when_the_environment_does_not() {
    let fx = fixture();
    let transcript = fx.dir.path().join("t.jsonl");
    std::fs::write(&transcript, "{\"type\":\"system\",\"entrypoint\":\"sdk-cli\",\"sessionId\":\"s-t\"}\n").unwrap();
    let block = start(&fx.db, &cwd(&fx), None, "s-t", Some(&transcript));
    assert!(block.contains("PLAINTEXT") && !block.contains("CHATTEXT"), "{block}");
    let desktop = fx.dir.path().join("d.jsonl");
    std::fs::write(&desktop, "{\"type\":\"system\",\"entrypoint\":\"claude-desktop\"}\n").unwrap();
    let block = start(&fx.db, &cwd(&fx), None, "s-d", Some(&desktop));
    assert!(block.contains("CHATTEXT"), "{block}");
}

#[test]
fn a_headless_prompt_is_not_served_the_interactive_only_facts() {
    let fx = fixture();
    let prompt = |entrypoint: Option<&str>, session: &str| {
        hook(
            &fx.db,
            &cwd(&fx),
            entrypoint,
            serde_json::json!({
                "hook_event_name": "UserPromptSubmit",
                "session_id": session,
                "cwd": cwd(&fx).to_string_lossy(),
                "prompt": "please run pdftool on the sample and fix the fonts",
            }),
        )
    };
    let headless = prompt(Some("sdk-cli"), "p-headless");
    assert!(headless.contains("PROMPTPLAIN"), "fixture sanity, the plain fact reaches a prompt: {headless}");
    assert!(!headless.contains("PROMPTCHAT"), "{headless}");
    let interactive = prompt(Some("claude-desktop"), "p-desktop");
    assert!(interactive.contains("PROMPTPLAIN") && interactive.contains("PROMPTCHAT"), "{interactive}");
}

#[test]
fn the_start_says_in_the_volume_record_what_it_saw() {
    let fx = fixture();
    start(&fx.db, &cwd(&fx), Some("sdk-cli"), "s-log", None);
    let log = std::fs::read_to_string(fx.dir.path().join("volume-log.jsonl")).unwrap();
    let line = log.lines().find(|l| l.contains("\"k\":\"entry\"")).expect("an entry line");
    let v: serde_json::Value = serde_json::from_str(line).unwrap();
    assert_eq!((v["e"].as_str(), v["from"].as_str(), v["headless"].as_bool()), (Some("sdk-cli"), Some("env"), Some(true)));
}

// ----------------------------------------------------------- the ranking

fn bulky(id: &str, severity: Option<Severity>, project: Option<&str>) -> Item {
    let mut item = always(id, &format!("{id} {}", "words ".repeat(45)), severity, &[]);
    item.project = project.map(str::to_string);
    item
}

fn ids_in(block: &str) -> Vec<String> {
    block.lines().filter_map(|l| l.strip_prefix("- [")).filter_map(|l| l.split_once(']').map(|(id, _)| id.to_string())).collect()
}

fn now_unix() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64
}

#[test]
fn this_projects_facts_come_first_then_heavy_ones_then_what_was_shown_lately_among_house_style() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("store.db");
    let repo = dir.path().join("sample-quote-repo");
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    {
        let mut s = EventStore::new(&db).unwrap();
        for i in 0..30 {
            // ids sort so that, without the ranking, "zz-recent" would be last
            store::declare(&mut s, "s", "l", "a", &bulky(&format!("aa-light-{i:02}"), Some(Severity::HouseStyle), None)).unwrap();
        }
        store::declare(&mut s, "s", "l", "a", &bulky("zz-recent", Some(Severity::HouseStyle), None)).unwrap();
        store::declare(&mut s, "s", "l", "a", &bulky("zz-unweighted", None, None)).unwrap();
        store::declare(&mut s, "s", "l", "a", &bulky("zz-heavy", Some(Severity::Irreversible), None)).unwrap();
        store::declare(&mut s, "s", "l", "a", &bulky("zz-costly-here", Some(Severity::Costly), Some("sample-quote-repo"))).unwrap();
    }
    let now = now_unix();
    std::fs::write(
        dir.path().join("eval-debt-state.json"),
        serde_json::json!({ "sample-quote-repo": { "sessions": { "sess-a": { "last_event_unix": now - 3_600 } } } }).to_string(),
    )
    .unwrap();
    std::fs::write(
        dir.path().join("volume-log.jsonl"),
        format!(
            "{}
{}
",
            serde_json::json!({ "t": now - 3_000, "k": "show", "s": "sess-a", "i": ["zz-recent"], "r": 0 }),
            serde_json::json!({ "t": now - 3_000, "k": "show", "s": "sess-elsewhere", "i": ["aa-light-29"], "r": 0 }),
        ),
    )
    .unwrap();

    let block = start(&db, &repo, Some("claude-desktop"), "s-rank", None);
    let shown = ids_in(&block);
    assert!(block.len() <= 9_800, "the cap holds: {} bytes", block.len());
    assert!(block.contains("more standing rule(s) did not fit"), "the left-out line stays: {block}");
    assert_eq!(shown[0], "zz-costly-here", "this project's own fact comes first: {shown:?}");
    assert_eq!(shown[1], "zz-heavy", "then the global heavy ones: {shown:?}");
    assert_eq!(shown[2], "zz-recent", "then house style, what this project's sessions were shown first: {shown:?}");
    assert!(!shown.contains(&"aa-light-29".to_string()), "a fact shown in ANOTHER project's session gets no lift: {shown:?}");
    assert!(!shown.contains(&"zz-unweighted".to_string()), "a global fact with no weight comes last and is cut: {shown:?}");
}

// ------------------------------------------ what a headless start leaves out (step 32)

fn answer_only(id: &str) -> Item {
    let mut item = always(id, &format!("{id} about how to report"), None, &[]);
    item.bindings = vec![Binding::Moment(intent::Action::Answer), Binding::Always];
    item
}

#[test]
fn a_headless_start_drops_reply_only_facts_and_the_volume_record_names_what_it_left_out() {
    let fx = fixture();
    {
        let mut s = EventStore::new(&fx.db).unwrap();
        // The gate refuses a NEW answer binding; such facts exist in the store from before, so
        // the fixture writes the event the way an older build did.
        let item = answer_only("reply-rule");
        let body = store::canonical_body(&item).unwrap();
        s.append_event("s", "l", "a", thor_core::event_store::EventKind::FactCreated, &item.id, None, &body).unwrap();
    }
    let headless = start(&fx.db, &cwd(&fx), Some("sdk-cli"), "s-out", None);
    assert!(!headless.contains("reply-rule"), "{headless}");
    let interactive = start(&fx.db, &cwd(&fx), Some("claude-desktop"), "s-in", None);
    assert!(interactive.contains("reply-rule"), "{interactive}");

    let log = std::fs::read_to_string(fx.dir.path().join("volume-log.jsonl")).unwrap();
    let line_of = |kind: &str, session: &str| -> serde_json::Value {
        let line = log
            .lines()
            .find(|l| l.contains(&format!("\"k\":\"{kind}\"")) && l.contains(&format!("\"s\":\"{session}\"")))
            .unwrap_or_else(|| panic!("no {kind} line for {session}: {log}"));
        serde_json::from_str(line).unwrap()
    };
    for kind in ["entry", "start"] {
        let v = line_of(kind, "s-out");
        assert_eq!((v["lo"].as_u64(), v["ao"].as_u64()), (Some(1), Some(1)), "{kind}: {v}");
        assert_eq!((v["li"][0].as_str(), v["ai"][0].as_str()), (Some("chat-rule"), Some("reply-rule")), "{kind}: {v}");
        let v = line_of(kind, "s-in");
        assert!(v.get("lo").is_none() && v.get("ao").is_none(), "an interactive start left nothing out: {v}");
    }
}

fn register_raw(store: &mut EventStore, key: &str, root: &str) {
    let item = Item {
        id: store::project_opened_id(key),
        kind: Kind::Report,
        text: format!("Project {key}: a fixture record of a registered working copy."),
        bindings: Vec::new(),
        severity: None,
        project: Some(key.to_string()),
        tags: vec!["project-record".to_string()],
        expires: None,
        key: None,
        falsifier: None,
        check: None,
    };
    let body = store::canonical_body(&item).unwrap();
    let at = serve::time::iso8601_from_unix(now_unix() - 2 * 86_400);
    let body = format!(
        "{},\"root\":{},\"root_at\":{}}}",
        &body[..body.len() - 1],
        serde_json::to_string(root).unwrap(),
        serde_json::to_string(&at).unwrap()
    );
    store.append_event("install", "install", "installer", thor_core::event_store::EventKind::FactCreated, &item.id, None, &body).unwrap();
}

/// The registration line is for the owner. A headless run gets none, and it
/// does not use up the day's showing for the owner's own next session.
#[test]
fn a_headless_start_carries_no_registration_line_and_leaves_it_for_the_owner() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("store.db");
    let repo = dir.path().join("sample-info-repo");
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    {
        let mut s = EventStore::new(&db).unwrap();
        store::declare(&mut s, "s", "l", "a", &always("plain-rule", "PLAINTEXT keep going.", Some(Severity::Costly), &[])).unwrap();
        register_raw(&mut s, "sample-info-repo", &repo.display().to_string().replace('\\', "/"));
    }
    let headless = start(&db, &repo, Some("sdk-cli"), "s-h", None);
    assert!(headless.contains("PLAINTEXT") && !headless.contains("the working copy of"), "{headless}");
    let owner = start(&db, &repo, Some("claude-desktop"), "s-o", None);
    assert!(owner.contains("the working copy of sample-info-repo is registered at"), "{owner}");
}
