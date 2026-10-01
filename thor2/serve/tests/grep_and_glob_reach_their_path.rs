//! A Grep or Glob call that names a `path` fires the rules anchored there,
//! end to end through the real compiled `serve hook` binary - read-only: it
//! informs, and never refuses a read.
//!
//! MEASURED (2026-09-26): no shape of Grep or Glob call fired a single path or
//! directory rule. The hook read a file only from `file_path`, which Read,
//! Write and Edit carry; Grep and Glob name theirs in `path`, so a search in a
//! file or folder the memory has a warning about went by without it.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn orientation(id: &str, text: &str, binding: model::item::Binding) -> model::item::Item {
    model::item::Item {
        id: id.to_string(),
        kind: model::item::Kind::Orientation,
        text: text.to_string(),
        bindings: vec![binding],
        severity: None,
        project: Some("search-project".to_string()),
        tags: vec![],
        expires: None,
        key: None,
        falsifier: Some("the place turns out to need no warning".to_string()),
        check: None,
    }
}

/// A checkout of `search-project` (its own marker file) with `src/lib.rs`
/// and `docs/guides/intro.md`, and a store holding one rule anchored at that
/// file, one at that folder, and one hard rule on the file whose check
/// refuses a WRITE carrying a marker.
fn fixture(dir: &Path) -> (PathBuf, PathBuf) {
    let work = dir.join("work");
    std::fs::create_dir_all(work.join("src")).unwrap();
    std::fs::create_dir_all(work.join("docs").join("guides")).unwrap();
    std::fs::write(work.join(".thor-project"), "search-project\n").unwrap();
    std::fs::write(work.join("src").join("lib.rs"), "fn main() {}\n").unwrap();
    std::fs::write(work.join("docs").join("guides").join("intro.md"), "# intro\n").unwrap();

    let db = dir.join("thor.db");
    let mut store = thor_core::event_store::EventStore::new(&db).unwrap();
    let file_rule = orientation(
        "lib-rs-is-generated",
        "this file is generated, change the template instead",
        model::item::Binding::Target { kind: model::item::TargetKind::Path, value: "src/lib.rs".to_string() },
    );
    let dir_rule = orientation(
        "guides-are-translated",
        "every page in this folder has a translated twin",
        model::item::Binding::Target { kind: model::item::TargetKind::Dir, value: "docs/guides".to_string() },
    );
    let mut guarded = orientation(
        "lib-rs-never-carries-the-marker",
        "this file never carries the release marker",
        model::item::Binding::Target { kind: model::item::TargetKind::Path, value: "src/lib.rs".to_string() },
    );
    guarded.kind = model::item::Kind::Rule;
    guarded.check =
        Some(model::item::Check::Absent { path: "src/lib.rs".to_string(), literal: "RELEASE-MARKER-XYZ".to_string() });
    for item in [&file_rule, &dir_rule, &guarded] {
        model::store::declare(&mut store, "s", "l", "a", &named(item)).unwrap();
    }
    (db, work)
}

/// A session of its own for every call: a fact is shown once per context
/// (step 28), and each call here asks what THAT call draws.
static NEXT_SESSION: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn run_hook(db: &Path, cwd: &Path, tool: &str, input: serde_json::Value) -> String {
    let session = format!("s{}", NEXT_SESSION.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
    let payload = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "session_id": session,
        "cwd": cwd.to_string_lossy(),
        "tool_name": tool,
        "tool_input": input,
    });
    let mut child = Command::new(env!("CARGO_BIN_EXE_serve"))
        .arg("--db")
        .arg(db)
        .arg("hook")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn serve");
    let _ = child.stdin.take().unwrap().write_all(payload.to_string().as_bytes());
    let out = child.wait_with_output().expect("wait serve");
    assert!(out.status.success(), "the hook must always exit 0");
    String::from_utf8(out.stdout).unwrap()
}

fn context(out: &str) -> String {
    serde_json::from_str::<serde_json::Value>(out.trim())
        .ok()
        .and_then(|v| v["hookSpecificOutput"]["additionalContext"].as_str().map(str::to_string))
        .unwrap_or_default()
}

fn blocked(out: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(out.trim()).ok().and_then(|v| v.get("decision").cloned())
        == Some(serde_json::json!("block"))
}

#[test]
fn a_grep_in_a_file_fires_the_rules_anchored_at_that_file() {
    let dir = tempfile::tempdir().unwrap();
    let (db, work) = fixture(dir.path());
    let file = work.join("src").join("lib.rs");
    let out = run_hook(&db, &work, "Grep", serde_json::json!({"pattern": "fn main", "path": file.to_string_lossy()}));
    assert!(context(&out).contains("lib-rs-is-generated"), "{out}");
    assert!(!context(&out).contains("guides-are-translated"), "{out}");
}

#[test]
fn a_grep_or_glob_in_a_folder_fires_the_rules_anchored_at_that_folder() {
    let dir = tempfile::tempdir().unwrap();
    let (db, work) = fixture(dir.path());
    let guides = work.join("docs").join("guides");
    for (tool, input) in [
        ("Grep", serde_json::json!({"pattern": "intro", "path": guides.to_string_lossy()})),
        ("Glob", serde_json::json!({"pattern": "**/*.md", "path": guides.to_string_lossy()})),
        ("Glob", serde_json::json!({"pattern": "*.md", "path": work.join("docs").join("guides").join("intro.md").to_string_lossy()})),
    ] {
        let out = run_hook(&db, &work, tool, input.clone());
        assert!(context(&out).contains("guides-are-translated"), "{tool} {input}: {out}");
    }
}

/// Anchored THERE, not somewhere below: a search over a folder does not
/// fire every rule about every file inside it.
#[test]
fn a_search_over_a_parent_folder_does_not_fire_a_rule_about_one_file_in_it() {
    let dir = tempfile::tempdir().unwrap();
    let (db, work) = fixture(dir.path());
    let out = run_hook(&db, &work, "Grep", serde_json::json!({"pattern": "x", "path": work.join("src").to_string_lossy()}));
    assert!(!context(&out).contains("lib-rs-is-generated"), "{out}");
}

#[test]
fn a_grep_or_glob_with_no_path_fires_no_place_rule() {
    let dir = tempfile::tempdir().unwrap();
    let (db, work) = fixture(dir.path());
    for tool in ["Grep", "Glob"] {
        let out = run_hook(&db, &work, tool, serde_json::json!({"pattern": "intro"}));
        let ctx = context(&out);
        assert!(!ctx.contains("lib-rs-is-generated") && !ctx.contains("guides-are-translated"), "{tool}: {out}");
    }
}

/// Read-only: a rule that refuses a WRITE carrying its marker never refuses
/// a search in that file, even one searching for the marker itself.
#[test]
fn a_search_is_never_refused() {
    let dir = tempfile::tempdir().unwrap();
    let (db, work) = fixture(dir.path());
    let file = work.join("src").join("lib.rs");
    let out = run_hook(
        &db,
        &work,
        "Grep",
        serde_json::json!({"pattern": "RELEASE-MARKER-XYZ", "path": file.to_string_lossy()}),
    );
    assert!(!blocked(&out), "{out}");
    assert!(context(&out).contains("lib-rs-never-carries-the-marker"), "the rule informs: {out}");
}

/// `item` with every check literal its text does not name appended to the
/// text (in `U+XXXX` form for a glyph): since step 8 of the two tracks the
/// write gate refuses a check whose literal the rule's own words never name
/// (A4), and these fixtures are about the hook, not about naming.
fn named(item: &model::item::Item) -> model::item::Item {
    let mut out = item.clone();
    let unnamed = model::gate::unnamed_literals(&out);
    if !unnamed.is_empty() {
        let shown: Vec<String> = unnamed.iter().map(|l| format!("'{}'", model::gate::shown_literal(l))).collect();
        out.text = format!("{} (names {})", out.text, shown.join(" "));
    }
    out
}
