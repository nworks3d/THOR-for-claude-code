//! THE SCRIPT LOOK-THROUGH, end to end through the real compiled `serve hook`
//! binary: a call that runs a script file holding a host or an MCP tool name
//! offers the facts bound to them, marked as reached through the script. It
//! only informs: a refusing check never turns an inferred route into a block,
//! and a call the guard refuses is refused exactly the same with or without a
//! script that names something.

use model::item::{Binding, Check, Item, Kind, TargetKind};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const HOST: &str = "127.0.0.1:27182";
const TOOL: &str = "mcp__Autodesk_Fusion__fusion_mcp_execute";

fn item(id: &str, kind: Kind, text: &str, binding: Binding, check: Option<Check>) -> Item {
    Item {
        id: id.to_string(),
        kind,
        text: text.to_string(),
        bindings: vec![binding],
        severity: None,
        project: None,
        tags: vec![],
        expires: None,
        key: None,
        falsifier: Some("the place turns out to need no warning".to_string()),
        check,
    }
}

fn host(value: &str) -> Binding {
    Binding::Target { kind: TargetKind::Host, value: value.to_string() }
}

fn command(value: &str) -> Binding {
    Binding::Target { kind: TargetKind::Command, value: value.to_string() }
}

fn named(it: &Item) -> Item {
    let mut out = it.clone();
    let unnamed = model::gate::unnamed_literals(&out);
    if !unnamed.is_empty() {
        let shown: Vec<String> = unnamed.iter().map(|l| format!("'{}'", model::gate::shown_literal(l))).collect();
        out.text = format!("{} (names {})", out.text, shown.join(" "));
    }
    out
}

/// A store with a note on the host, a note on the tool, a RULE on the host
/// whose check refuses a literal the script holds, and a command rule that
/// refuses `--force`; plus two scripts, one that names the host and the tool
/// and one that names nothing.
fn fixture(dir: &Path) -> PathBuf {
    let db = dir.join("thor.db");
    let mut store = thor_core::event_store::EventStore::new(&db).unwrap();
    let items = [
        item("host-note", Kind::Orientation, "talk to the model server through its own client only", host(HOST), None),
        item("tool-note", Kind::Orientation, "the execute tool makes no new bodies", command(TOOL), None),
        item(
            "host-rule",
            Kind::Rule,
            "never send a wipe_everything_now request to the model server",
            host(HOST),
            Some(Check::Forbidden { literals: vec!["wipe_everything_now".to_string()] }),
        ),
        item(
            "no-force",
            Kind::Rule,
            "never run git push with --force on a shared branch",
            command("git push"),
            Some(Check::Forbidden { literals: vec!["--force".to_string()] }),
        ),
    ];
    for it in &items {
        model::store::declare(&mut store, "s", "l", "a", &named(it)).unwrap();
    }
    std::fs::write(
        dir.join("names.sh"),
        format!("#!/bin/sh\nURL=\"http://{HOST}/mcp\"\nNAME=\"fusion_mcp_execute\"\nBODY=\"wipe_everything_now\"\n"),
    )
    .unwrap();
    std::fs::write(dir.join("quiet.sh"), "#!/bin/sh\necho nothing to see\n").unwrap();
    db
}

static NEXT_SESSION: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn run_hook(db: &Path, cwd: &Path, command: &str) -> String {
    let session = format!("route{}", NEXT_SESSION.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
    let payload = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "session_id": session,
        "cwd": cwd.to_string_lossy(),
        "tool_name": "Bash",
        "tool_input": { "command": command, "description": "run a script" },
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

fn json(out: &str) -> serde_json::Value {
    serde_json::from_str(out.trim()).unwrap_or(serde_json::Value::Null)
}

fn context(out: &str) -> String {
    json(out)["hookSpecificOutput"]["additionalContext"].as_str().map(str::to_string).unwrap_or_default()
}

fn blocked(out: &str) -> bool {
    json(out).get("decision") == Some(&serde_json::json!("block"))
}

fn flat(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

#[test]
fn a_script_that_holds_the_host_and_the_tool_name_offers_their_facts() {
    let dir = tempfile::tempdir().unwrap();
    let db = fixture(dir.path());
    let out = run_hook(&db, dir.path(), &format!("sh {}", flat(&dir.path().join("names.sh"))));
    let ctx = context(&out);
    assert!(!blocked(&out), "{out}");
    assert!(ctx.contains("host-note"), "{out}");
    assert!(ctx.contains("tool-note"), "{out}");
    assert!(ctx.contains("reached through names.sh, which names"), "{out}");
    assert!(ctx.contains("the command line does not"), "{out}");
}

/// A relative script, read against the session's working folder.
#[test]
fn a_relative_script_is_read_against_the_working_folder() {
    let dir = tempfile::tempdir().unwrap();
    let db = fixture(dir.path());
    let out = run_hook(&db, dir.path(), "sh names.sh");
    assert!(context(&out).contains("host-note"), "{out}");
}

#[test]
fn a_script_that_names_nothing_a_missing_script_and_a_variable_path_offer_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let db = fixture(dir.path());
    for call in [
        format!("sh {}", flat(&dir.path().join("quiet.sh"))),
        format!("sh {}", flat(&dir.path().join("not-there.sh"))),
        "sh $HOME/names.sh".to_string(),
    ] {
        let out = run_hook(&db, dir.path(), &call);
        assert!(!blocked(&out), "{call}: {out}");
        assert!(!context(&out).contains("host-note") && !context(&out).contains("tool-note"), "{call}: {out}");
    }
}

/// A rule bound to the host whose check REFUSES a literal the script holds
/// informs and never blocks: the route is an inference.
#[test]
fn a_refusing_check_on_a_route_fact_informs_and_never_blocks() {
    let dir = tempfile::tempdir().unwrap();
    let db = fixture(dir.path());
    let out = run_hook(&db, dir.path(), &format!("sh {}", flat(&dir.path().join("names.sh"))));
    assert!(!blocked(&out), "{out}");
    assert!(context(&out).contains("host-rule"), "the rule informs: {out}");
    assert!(!context(&out).contains("[PROHIBITS]"), "{out}");
}

/// The host on the command line itself is the old, direct route: unchanged, and
/// not described as reached through a script.
#[test]
fn a_host_on_the_command_line_is_still_a_direct_hit() {
    let dir = tempfile::tempdir().unwrap();
    let db = fixture(dir.path());
    let out = run_hook(&db, dir.path(), &format!("sh {} http://{HOST}/mcp", flat(&dir.path().join("names.sh"))));
    let ctx = context(&out);
    let line = ctx.lines().find(|l| l.contains("[host-note]")).unwrap_or_else(|| panic!("{out}"));
    assert!(!line.contains("reached through"), "{line}");
}

/// A call the guard refuses is refused with the same reason whether or not the
/// script it also runs holds a host: the look-through comes after the guard and
/// never changes what it decides.
#[test]
fn the_guards_verdict_is_the_same_with_and_without_routes() {
    let dir = tempfile::tempdir().unwrap();
    let db = fixture(dir.path());
    let with = run_hook(&db, dir.path(), &format!("sh {} && git push --force origin main", flat(&dir.path().join("names.sh"))));
    let without = run_hook(&db, dir.path(), &format!("sh {} && git push --force origin main", flat(&dir.path().join("quiet.sh"))));
    assert!(blocked(&with) && blocked(&without), "{with} / {without}");
    let reason = |out: &str| json(out)["reason"].as_str().unwrap_or_default().replace("quiet.sh", "S").replace("names.sh", "S");
    assert_eq!(reason(&with), reason(&without));
    assert!(!with.contains("reached through"), "a refused call carries no route text: {with}");
}

/// STRUCTURE: no guard reads a route. The guard modules never mention the
/// look-through or the `routes` field of the input.
#[test]
fn no_guard_module_reads_a_route() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    for file in ["guard_verdict.rs", "absent_guard.rs", "capture.rs", "stale_guard.rs"] {
        let text = std::fs::read_to_string(src.join(file)).unwrap();
        for forbidden in ["script_routes", ".routes", "add_script_routes"] {
            assert!(!text.contains(forbidden), "{file} must not read a route ({forbidden})");
        }
    }
}
