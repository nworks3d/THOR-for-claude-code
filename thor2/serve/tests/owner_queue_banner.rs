//! THE OWNER QUEUE AT SESSION START (step 16 of the watertight teeth build,
//! B5 and B6), end to end through the real `serve hook` binary.
//!
//! A subject THOR's own open point holds - here a file whose registered
//! working tree lost a line the default branch still has - that a VALID
//! evaluation was already filed past is the owner's to decide: no agent can
//! lift it. Up to `OPEN_POINTS_OWNER_BUDGET` (8) such points are listed in the
//! owner's session start like any open point; more than that, and the block
//! opens with a banner saying how many wait on him. A subagent's session
//! start shows neither: the open points are for the owner.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use model::item::{Binding, Check, Item, Kind, Severity, TargetKind};
use thor_core::event_store::{EventKind, EventStore};

const S1: &str = "0de7dc52-1b97-4b66-948f-61e4438043b2";
const S2: &str = "373e8610-8130-490d-ac29-56aa98709fd1";
const S3: &str = "5e1f0c3a-0000-4000-8000-000000000003";

struct Fx {
    dir: tempfile::TempDir,
    db: PathBuf,
    checkout: PathBuf,
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

/// `n` files of project fxp, each holding a line an old contains rule guards,
/// in a registered checkout; THOR's machine pass decides them (S1's start);
/// then an uncommitted edit removes every guarded line, so the next start
/// (S2's) holds each rule by its own open point about that file; and S2 files
/// a valid evaluation while all `n` points hold - each subject is carried
/// once, and every point is in the owner queue.
fn owner_queue_of(n: usize) -> Fx {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let root = dir.path().join("fxp");
    std::fs::create_dir_all(&root).unwrap();
    let hooks = dir.path().join("no-hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    git(&root, &["init", "-q", "-b", "main"]);
    git(&root, &["config", "core.hooksPath", &hooks.to_string_lossy()]);
    git(&root, &["config", "user.email", "fixture@example.invalid"]);
    git(&root, &["config", "user.name", "fixture"]);
    std::fs::write(root.join(serve::project::MARKER_FILE_NAME), "fxp\n").unwrap();
    for i in 0..n {
        std::fs::write(root.join(format!("notes-{i}.md")), format!("keep line {i}\nplain line\n")).unwrap();
    }
    git(&root, &["add", "-A"]);
    git(&root, &["commit", "-q", "-m", "fixture"]);
    let root = serve::project::checkout_top(&root);

    let mut store = EventStore::new(&db).unwrap();
    // The working-copy registration, raw, the way the installer writes it (a
    // test holds no installer token).
    let record = Item {
        id: model::store::project_opened_id("fxp"),
        kind: Kind::Report,
        text: "Project fxp: a fixture record.".to_string(),
        bindings: vec![],
        severity: None,
        project: Some("fxp".to_string()),
        tags: vec!["project-record".to_string()],
        expires: None,
        key: None,
        falsifier: None,
        check: None,
    };
    let body = model::store::canonical_body(&record).unwrap();
    let body = format!(
        "{},\"root\":{},\"root_at\":\"2026-09-28T10:00:00Z\"}}",
        &body[..body.len() - 1],
        serde_json::to_string(&root.to_string_lossy()).unwrap()
    );
    store.append_event("install", "install", "installer", EventKind::FactCreated, &record.id, None, &body).unwrap();
    for i in 0..n {
        let file = format!("notes-{i}.md");
        let rule = Item {
            id: format!("fx-notes-{i}"),
            kind: Kind::Rule,
            text: format!("{file} keeps `keep line {i}`."),
            bindings: vec![Binding::Target { kind: TargetKind::Path, value: file.clone() }],
            severity: Some(Severity::Irreversible),
            project: Some("fxp".to_string()),
            tags: vec![],
            expires: None,
            key: None,
            falsifier: Some("It turns out to be harmless.".to_string()),
            check: Some(Check::Contains { path: file, literal: format!("keep line {i}") }),
        };
        let body = model::store::canonical_body(&rule).unwrap();
        store.append_event("legacy", &rule.id, "migration", EventKind::FactCreated, &rule.id, None, &body).unwrap();
    }
    let decide = |store: &mut EventStore, session: &str| {
        let began = serve::eval_session::begin(&db, store, session, "fxp", "command").unwrap();
        serve::eval_session::decide(store, began.start, Duration::from_secs(120))
    };
    decide(&mut store, S1);
    for i in 0..n {
        std::fs::write(root.join(format!("notes-{i}.md")), "mid-edit\nplain line\n").unwrap();
    }
    let held = decide(&mut store, S2);
    assert_eq!(held.frozen.machine.as_ref().unwrap().points.len(), n, "fixture sanity: one point per file");
    let transcripts = dir.path().join("transcripts");
    let report = serve::eval_filing::fixture::report("eval-fxp-2026-09-29", "fxp", "TLDR: the checkout is mid-change.");
    let record = serve::eval_filing::fixture::valid_filing(&mut store, &transcripts, S2, &report, "toolu_filing");
    assert_eq!(record.carry.len(), n, "every held subject carried once: {record:?}");
    assert_eq!(model::store::owner_queue(&store, "fxp").len(), n, "fixture sanity: all of them wait on the owner");
    Fx { dir, db, checkout: root }
}

/// The additional context `serve hook` gives a SessionStart in the fixture's
/// checkout - of a subagent when `agent` names one.
fn session_start(fx: &Fx, agent: Option<&str>) -> String {
    let mut payload = serde_json::json!({
        "hook_event_name": "SessionStart",
        "session_id": S3,
        "source": "startup",
        "cwd": fx.checkout.to_string_lossy(),
    });
    if let Some(agent) = agent {
        payload["agent_id"] = serde_json::Value::String(agent.to_string());
        payload["agent_type"] = serde_json::Value::String("general-purpose".to_string());
    }
    let home = fx.dir.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_serve"))
        .arg("--db")
        .arg(&fx.db)
        .arg("hook")
        .env("USERPROFILE", &home)
        .env("HOME", &home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn serve");
    child.stdin.take().unwrap().write_all(payload.to_string().as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "the hook always exits 0");
    let stdout = String::from_utf8(out.stdout).unwrap();
    if stdout.trim().is_empty() {
        return String::new();
    }
    let v: serde_json::Value = serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("hook JSON: {e}: {stdout}"));
    v["hookSpecificOutput"]["additionalContext"].as_str().unwrap_or_default().to_string()
}

/// R16.5 and G16.3: nine points in fxp's owner queue open the owner's
/// session-start open-points block with the banner; three are listed in the
/// normal block, marked as the owner's, with no banner.
#[test]
fn owner_queue_over_budget_banner_at_session_start() {
    let over = owner_queue_of(9);
    let context = session_start(&over, None);
    let block = &context[context.find("[THOR] 9 open points wait on YOU").unwrap_or_else(|| panic!("no banner: {context}"))..];
    assert!(
        block.starts_with(
            "[THOR] 9 open points wait on YOU in fxp (budget 8) - each one says what to do, step by step\n[THOR] 9 open point(s) \
             in this project"
        ),
        "the banner opens the open-points block: {block}"
    );
    assert!(block.contains("(THOR's own, the owner decides)"), "{block}");

    let within = owner_queue_of(3);
    let context = session_start(&within, None);
    assert!(!context.contains("wait on YOU"), "within the budget: no banner: {context}");
    assert!(context.contains("[THOR] 3 open point(s) in this project"), "{context}");
    assert_eq!(context.matches("(THOR's own, the owner decides)").count(), 3, "{context}");
}

/// G16.3: a subagent's session start shows neither the banner nor the open
/// points - they are the owner's.
#[test]
fn subagent_never_gets_the_owner_banner() {
    let over = owner_queue_of(9);
    let context = session_start(&over, Some("a1dca2c0feb7f44fb"));
    assert!(!context.contains("wait on YOU"), "{context}");
    assert!(!context.contains("open point(s) in this project"), "{context}");
    let owner = session_start(&over, None);
    assert!(owner.contains("wait on YOU"), "fixture sanity, the owner's own start shows it: {owner}");
}
