//! `doctor --eval`, the fast form the evaluation routine (`/thor-eval`) tells
//! its reader to wait for, proven against the REAL compiled binary on one
//! fixture store: it prints exactly the lines the routine asks for, and every
//! one of them is the very line the full report prints for that store - a
//! second, divergent computation would show up here as a line that is not in
//! the full report.

use std::collections::HashSet;
use std::path::Path;
use std::process::Command;

use model::item::{Binding, Check, Item, Kind, Severity, TargetKind};
use thor_core::event_store::{EventKind, EventStore};

/// Write `item` straight to the log (the gate is not what is under test, and
/// a real store can hold shapes it would no longer let in).
fn put(store: &mut EventStore, item: &Item) {
    let body = serde_json::to_string(item).unwrap();
    store.append_event("s", "l", "migration", EventKind::FactCreated, &item.id, None, &body).unwrap();
}

fn rule(id: &str, bindings: Vec<Binding>) -> Item {
    Item {
        id: id.to_string(),
        kind: Kind::Rule,
        text: format!("fixture rule {id}"),
        bindings,
        severity: None,
        project: None,
        tags: vec![],
        expires: None,
        key: None,
        falsifier: Some("this fixture rule turns out to be wrong".to_string()),
        check: None,
    }
}

/// A store with a pinned heavy rule, a rule with a check, a project whose
/// working copy is on disk (one live anchor, one dead one), and a recorded ship.
/// Returns (store path, checkouts root).
fn fixture(dir: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
    let checkouts = dir.join("dev");
    let repo = checkouts.join("eval-proj");
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(repo.join(".thor-project"), "eval-proj\n").unwrap();
    std::fs::write(repo.join("src").join("lib.rs"), "fn main() {}\n").unwrap();

    let db = dir.join("thor.db");
    let mut store = EventStore::new(&db).unwrap();

    let mut pinned = rule("pinned-heavy", vec![Binding::Always]);
    pinned.severity = Some(Severity::Costly);
    put(&mut store, &pinned);

    let mut checked = rule("forbids-force", vec![Binding::Always]);
    checked.text = "never use --force here".to_string();
    checked.check = Some(Check::Forbidden { literals: vec!["--force".to_string()] });
    put(&mut store, &checked);

    let mut alive = rule(
        "anchored-alive",
        vec![Binding::Target { kind: TargetKind::Path, value: "src/lib.rs".to_string() }],
    );
    alive.project = Some("eval-proj".to_string());
    put(&mut store, &alive);

    let mut dead = rule(
        "anchored-dead",
        vec![Binding::Target { kind: TargetKind::Path, value: "src/gone.rs".to_string() }],
    );
    dead.project = Some("eval-proj".to_string());
    put(&mut store, &dead);

    // Something served, so the pinned and judgement lines have a history to read.
    store.append_event("session-1", "l", "hook", EventKind::ItemServed, "pinned-heavy", None, "{}").unwrap();
    drop(store);

    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    ops::ship_state::record_success_at(&db, 0, now).unwrap();
    (db, checkouts)
}

fn run(db: &Path, checkouts: &Path, extra: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_doctor"))
        .arg("--db")
        .arg(db)
        .arg("--checkouts")
        .arg(checkouts)
        .args(extra)
        .output()
        .unwrap()
}

fn lines(out: &std::process::Output) -> Vec<String> {
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout.clone()).unwrap().lines().map(str::to_string).collect()
}

/// What starts a block of the evaluation report. Anything not indented must
/// start with one of these; an indented line belongs to the block above it.
const EVAL_BLOCKS: &[&str] = &[
    "doctor 2",
    "checkouts root:",
    "tool server:",
    "installed programs:",
    "ship:",
    "provable rules:",
    "gate:",
    "teeth",
    "pinned:",
    "decay:",
    "crowding",
    "judgement debt:",
    "contradictions:",
    "doctor --eval:",
];

#[test]
fn eval_prints_only_the_lines_the_routine_asks_for_and_each_is_the_full_reports_line() {
    let dir = tempfile::tempdir().unwrap();
    let (db, checkouts) = fixture(dir.path());

    let full = lines(&run(&db, &checkouts, &[]));
    let eval = lines(&run(&db, &checkouts, &["--eval"]));

    // Exactly the expected blocks, and none of the others.
    for line in eval.iter().filter(|l| !l.starts_with(' ')) {
        assert!(EVAL_BLOCKS.iter().any(|b| line.starts_with(b)), "--eval printed a line the routine never asks for: {line}");
    }
    for expected in ["ship:", "provable rules:", "gate:", "teeth:", "pinned:", "decay:", "crowding:"] {
        assert!(eval.iter().any(|l| l.starts_with(expected)), "--eval is missing its {expected} line:\n{}", eval.join("\n"));
    }
    for left_out in ["memory store:", "falsifiers:", "dead moments:", "command anchors never fired:", "semantic search"] {
        assert!(full.iter().any(|l| l.starts_with(left_out)), "the full report no longer has {left_out}: pick another witness");
        assert!(!eval.iter().any(|l| l.starts_with(left_out)), "--eval must leave out {left_out}");
    }
    // It opens with the version and the checkouts root, and closes with what it left out.
    assert!(eval[0].starts_with("doctor "), "{}", eval[0]);
    assert!(eval[1].starts_with("checkouts root: "), "{}", eval[1]);
    assert!(eval.last().unwrap().starts_with("doctor --eval: only the lines"), "{:?}", eval.last());

    // Every line of it, bar the footer, is a line of the full report - the same
    // computation, not a second one - and in the same order.
    let full_set: HashSet<&str> = full.iter().map(String::as_str).collect();
    let body = &eval[..eval.len() - 1];
    for line in body {
        assert!(full_set.contains(line.as_str()), "--eval line is not in the full report: {line}");
    }
    let positions: Vec<usize> = body.iter().map(|l| full.iter().position(|f| f == l).unwrap()).collect();
    assert!(positions.windows(2).all(|w| w[0] < w[1]), "--eval lines keep the full report's order: {positions:?}");

    // The fixture's own facts really are in it (a vacuous pass would print none).
    assert!(eval.iter().any(|l| l.starts_with("decay:") && l.contains("1 anchor(s) point at nothing")), "{}", eval.join("\n"));
    assert!(eval.iter().any(|l| l.starts_with("pinned: 2 item(s)")), "{}", eval.join("\n"));
}

#[test]
fn eval_cannot_be_combined_with_gate() {
    let dir = tempfile::tempdir().unwrap();
    let (db, checkouts) = fixture(dir.path());
    let out = run(&db, &checkouts, &["--eval", "--gate"]);
    assert!(!out.status.success(), "--eval with --gate must be refused");
}

/// FIELD TEST 13, D1: the fast form hid the one line that predicts a report
/// that would be on file and dead (a tool server or an installed program from
/// another build). It prints the installation lines too, in the full report's
/// order, and they are silent when nothing is wrong.
#[test]
fn eval_prints_the_installation_lines_the_full_report_prints() {
    let dir = tempfile::tempdir().unwrap();
    let (db, checkouts) = fixture(dir.path());
    // A folder like the installed one: doctor with a "serve" and an "mcp" beside it
    // that print no build id (an older program), which doctor names as skew.
    let bin = dir.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let suffix = std::env::consts::EXE_SUFFIX;
    let doctor = bin.join(format!("doctor{suffix}"));
    std::fs::copy(env!("CARGO_BIN_EXE_doctor"), &doctor).unwrap();
    std::fs::copy(env!("CARGO_BIN_EXE_doctor"), bin.join(format!("serve{suffix}"))).unwrap();
    std::fs::copy(env!("CARGO_BIN_EXE_doctor"), bin.join(format!("mcp{suffix}"))).unwrap();
    let run_here = |extra: &[&str]| {
        Command::new(&doctor).arg("--db").arg(&db).arg("--checkouts").arg(&checkouts).args(extra).output().unwrap()
    };
    let eval = lines(&run_here(&["--eval"]));
    let full = lines(&run_here(&[]));
    let skew = eval.iter().find(|l| l.starts_with("installed programs:")).unwrap_or_else(|| panic!("--eval hides the skew line:\n{}", eval.join("\n")));
    assert!(skew.contains("prints no build id"), "{skew}");
    assert!(full.contains(skew), "the very line the full report prints");
    assert!(eval.iter().position(|l| l == skew) > eval.iter().position(|l| l.starts_with("checkouts root:")), "after the checkouts root");
}
