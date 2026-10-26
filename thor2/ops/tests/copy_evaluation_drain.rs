//! A COPY'S WEEKLY-CHECK REPORT, FROM THE CONNECTOR'S QUEUE TO THE MAIN LOG.
//!
//! The real `sync recv --inbox` (the receiver beside the connector) and the
//! real `sync drain`, over loopback - never the live store, never the NAS. The
//! queue is filled the way the connector fills it: `inbox::append` of the
//! report as a captured `remember`, and of the copy's attestation as a
//! `copy_attest` capture (`mcp::copy_routes`).
//!
//! What must hold: a report counts only when an attestation with its capture id
//! matches it; a forged, edited or missing attestation never counts and the
//! report says why; a report whose attestation has not come yet is held, not
//! lost, and not counted.

use serde_json::json;
use serve::copy_eval::{self, CopyAttestation};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;
use thor_core::event_store::EventStore;
use thor_core::inbox::{self, InboxOp};

const TOKEN: &str = "test-shared-secret";
const SESSION: &str = "11111111-2222-4333-8444-555555555555";
const TEXT: &str = "Judged the three notes I used; one was stale and is retracted.";

struct Receiver(std::process::Child);

impl Drop for Receiver {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

fn start_receiver(db: &Path, inbox: &Path, port: u16) -> Receiver {
    let child = Command::new(env!("CARGO_BIN_EXE_sync"))
        .args(["recv", "--db"])
        .arg(db)
        .args(["--bind", &format!("127.0.0.1:{port}"), "--inbox"])
        .arg(inbox)
        .env("THOR_SYNC_TOKEN", TOKEN)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start the fixture receiver");
    let addr: std::net::SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(200)).is_err() {
        assert!(std::time::Instant::now() < deadline, "the receiver never listened");
        std::thread::sleep(Duration::from_millis(50));
    }
    Receiver(child)
}

struct Fixture {
    _dir: tempfile::TempDir,
    main: std::path::PathBuf,
    inbox: std::path::PathBuf,
    _receiver: Receiver,
    base: String,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let main = dir.path().join("main.db");
    drop(EventStore::new(&main).unwrap());
    let inbox = dir.path().join("inbox.jsonl");
    let port = free_port();
    let receiver = start_receiver(&dir.path().join("replica.db"), &inbox, port);
    Fixture { _dir: dir, main, inbox, _receiver: receiver, base: format!("http://127.0.0.1:{port}") }
}

fn report_op(capture: &str, text: &str) -> InboxOp {
    let mut op = InboxOp::new(
        "remember",
        json!({"id": "eval-demo-1", "kind": "report", "project": "demo", "tags": ["evaluation-report"], "text": text}),
    );
    op.capture_id = capture.to_string();
    op
}

fn attestation_op(capture: &str, text: &str) -> InboxOp {
    let att = CopyAttestation {
        v: copy_eval::ATTESTATION_VERSION,
        session: SESSION.to_string(),
        project: "demo".to_string(),
        report: "eval-demo-1".to_string(),
        tool_use_id: "toolu_file".to_string(),
        capture_id: capture.to_string(),
        text_sha256: copy_eval::text_digest(text),
        attested_at: "2026-10-26T10:00:00.000Z".to_string(),
        covered: vec![copy_eval::Covered { tool: "mark".into(), id: "some-note".into(), capture_id: "cap-mark".into() }],
        seal: String::new(),
    }
    .sealed();
    InboxOp::new(copy_eval::ATTEST_TOOL, serde_json::to_value(att).unwrap())
}

fn drain(f: &Fixture) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_sync"))
        .args(["drain", "--db"])
        .arg(&f.main)
        .args(["--from", &f.base])
        .env("THOR_SYNC_TOKEN", TOKEN)
        .output()
        .expect("run sync drain");
    (out.status.success(), String::from_utf8_lossy(&out.stdout).to_string())
}

fn counted(f: &Fixture) -> Option<String> {
    let store = EventStore::open_existing(&f.main).unwrap();
    serve::eval_filing::newest_valid_evaluation(&store, Some("demo")).map(|v| v.report)
}

/// A queued report with a matching attestation counts as the project's
/// evaluation, and its machine section says what a copy covers and what it
/// does not.
#[test]
fn a_report_with_a_matching_attestation_counts_after_the_drain() {
    let f = fixture();
    inbox::append(&f.inbox, &report_op("cap-1", TEXT)).unwrap();
    inbox::append(&f.inbox, &attestation_op("cap-1", TEXT)).unwrap();

    let (ok, out) = drain(&f);

    assert!(ok, "{out}");
    assert!(out.contains("COUNTS"), "{out}");
    assert_eq!(counted(&f).as_deref(), Some("eval-demo-1"));
    let store = EventStore::open_existing(&f.main).unwrap();
    let item = model::store::show(&store, "eval-demo-1").unwrap();
    assert!(item.text.starts_with(model::gate::MACHINE_SECTION_PREFIX), "{}", item.text);
    assert!(item.text.contains("COPY EVALUATION") && item.text.contains("toolu_file") && item.text.contains("- mark some-note"), "{}", item.text);
    assert!(item.text.contains("Not computed for a copy evaluation"), "{}", item.text);
    assert!(item.text.ends_with(TEXT), "the writer's own text follows the machine section");
    let validation = serve::eval_filing::current_validation(&store, "eval-demo-1").unwrap();
    assert_eq!(validation.filed_at.as_deref(), Some("2026-10-26T10:00:00.000Z"));
    assert_eq!(validation.session, SESSION);
}

/// The attestation may arrive in a later drain than the report: the report is
/// held (not lost, not counted) until it does.
#[test]
fn a_report_waits_for_its_attestation_and_is_not_counted_meanwhile() {
    let f = fixture();
    inbox::append(&f.inbox, &report_op("cap-1", TEXT)).unwrap();

    let (ok, out) = drain(&f);
    assert!(ok, "{out}");
    assert!(out.contains("attestation has not arrived yet") && out.contains("0 NOT applied"), "{out}");
    assert_eq!(counted(&f), None, "nothing counts without its proof");
    assert!(model::store::show(&EventStore::open_existing(&f.main).unwrap(), "eval-demo-1").is_err(), "not on the log yet");

    inbox::append(&f.inbox, &attestation_op("cap-1", TEXT)).unwrap();
    let (ok, out) = drain(&f);
    assert!(ok, "{out}");
    assert_eq!(counted(&f).as_deref(), Some("eval-demo-1"), "{out}");
}

/// A forged or edited attestation: the report is on file but does not count,
/// and its machine section and the validator's record say why.
#[test]
fn an_attestation_for_another_text_never_counts_and_says_why() {
    let f = fixture();
    inbox::append(&f.inbox, &report_op("cap-1", "A report that was rewritten after the copy checked it.")).unwrap();
    inbox::append(&f.inbox, &attestation_op("cap-1", TEXT)).unwrap();

    let (ok, out) = drain(&f);

    assert!(ok, "a dead filing is not a lost write: {out}");
    assert!(out.contains("does NOT count") && out.contains("sha256 of the text differs"), "{out}");
    assert_eq!(counted(&f), None);
    let store = EventStore::open_existing(&f.main).unwrap();
    let (_, dead) = serve::eval_filing::reports_not_counted(&store, "demo");
    assert_eq!(dead.len(), 1, "the dead report is listed for the next ask: {dead:?}");
    assert!(dead[0].1.contains("sha256 of the text differs"), "{dead:?}");
    let item = model::store::show(&store, "eval-demo-1").unwrap();
    assert!(item.text.contains("DOES NOT COUNT"), "{}", item.text);
}

/// An attestation edited after the copy sealed it (here: another session id
/// put in) fails its seal.
#[test]
fn an_edited_attestation_fails_its_seal() {
    let f = fixture();
    inbox::append(&f.inbox, &report_op("cap-1", TEXT)).unwrap();
    let mut forged = attestation_op("cap-1", TEXT);
    forged.args["session"] = json!("99999999-2222-4333-8444-555555555555");
    inbox::append(&f.inbox, &forged).unwrap();

    let (ok, out) = drain(&f);

    assert!(ok, "{out}");
    assert!(out.contains("does not match its own seal"), "{out}");
    assert_eq!(counted(&f), None);
}

/// No attestation within a day: the report is filed dead, with the reason, so
/// the next ask names it instead of the report sitting held forever.
#[test]
fn a_report_with_no_attestation_after_a_day_is_filed_dead() {
    let f = fixture();
    let mut report = report_op("cap-1", TEXT);
    report.ts = "1000000000".to_string();
    inbox::append(&f.inbox, &report).unwrap();

    let (ok, out) = drain(&f);

    assert!(ok, "{out}");
    assert!(out.contains("no attestation arrived within 24 hours"), "{out}");
    assert_eq!(counted(&f), None);
    let store = EventStore::open_existing(&f.main).unwrap();
    let (_, dead) = serve::eval_filing::reports_not_counted(&store, "demo");
    assert_eq!(dead.len(), 1);
}

/// The inbox is at-least-once: a batch served again after a crash must not
/// file the same report twice.
#[test]
fn a_replayed_batch_does_not_file_the_report_twice() {
    let f = fixture();
    inbox::append(&f.inbox, &report_op("cap-1", TEXT)).unwrap();
    inbox::append(&f.inbox, &attestation_op("cap-1", TEXT)).unwrap();
    let (ok, out) = drain(&f);
    assert!(ok, "{out}");

    inbox::append(&f.inbox, &report_op("cap-1", TEXT)).unwrap();
    inbox::append(&f.inbox, &attestation_op("cap-1", TEXT)).unwrap();
    let (ok, out) = drain(&f);
    assert!(ok, "{out}");
    assert!(out.contains("already known"), "{out}");
    assert_eq!(counted(&f).as_deref(), Some("eval-demo-1"));
}
