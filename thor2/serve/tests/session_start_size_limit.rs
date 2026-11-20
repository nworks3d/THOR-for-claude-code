//! Field 2026-10-01 (step 29): the session-start block was 15.6 KB for 53
//! standing rules. Claude Code keeps a hook output over 10,000 characters in a
//! file and shows the agent only its first 2 KB, while THOR counted all 53
//! rules as shown. The real `serve hook`, run on such a store, must now put at
//! most 9,500 bytes in the block, say how many rules did not fit, and record as
//! served exactly the rules that are in the block.

use model::item::{Binding, Item, Kind, Severity};
use model::store;
use std::io::Write;
use std::process::{Command, Stdio};
use thor_core::event_store::{EventKind, EventStore};

fn standing_rule(i: usize) -> Item {
    Item {
        id: format!("standing-rule-{i:03}"),
        kind: Kind::Rule,
        text: format!("{i:03} {}", "standing words ".repeat(18)),
        bindings: vec![Binding::Always],
        severity: Some(Severity::Costly),
        project: None,
        tags: vec![format!("{}a fixture standing rule without a literal", model::store::NO_LITERAL_REASON_PREFIX)],
        expires: None,
        key: None,
        falsifier: Some("the fixture rule turns out to be wrong".to_string()),
        check: None,
    }
}

fn run_session_start(db: &std::path::Path, cwd: &std::path::Path, session: &str) -> serde_json::Value {
    let payload = serde_json::json!({ "session_id": session, "hook_event_name": "SessionStart", "cwd": cwd.to_string_lossy() });
    let mut child = Command::new(env!("CARGO_BIN_EXE_serve"))
        .arg("--db")
        .arg(db)
        .arg("hook")
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let _ = child.stdin.take().unwrap().write_all(payload.to_string().as_bytes());
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    serde_json::from_slice(&out.stdout).expect("the hook prints one JSON value")
}

/// Writes `item` straight to the log, past the write gate: these fixtures build
/// a store that ALREADY overflows the session start block (as an older store
/// did), which the gate now refuses to create.
fn store_overflowing(s: &mut EventStore, item: &Item) {
    let body = store::canonical_body(item).unwrap();
    s.append_event("legacy", &item.id, "migration", thor_core::event_store::EventKind::FactCreated, &item.id, None, &body).unwrap();
}

#[test]
fn a_big_standing_block_stays_under_the_limit_and_only_what_is_in_it_counts_as_served() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("store.db");
    {
        let mut s = EventStore::new(&db).unwrap();
        for i in 0..53 {
            store_overflowing(&mut s, &standing_rule(i));
        }
    }
    let cwd = dir.path().join("workdir");
    std::fs::create_dir_all(&cwd).unwrap();

    let json = run_session_start(&db, &cwd, "session-one");
    let block = json["hookSpecificOutput"]["additionalContext"].as_str().unwrap().to_string();
    assert!(block.len() <= 9_800, "the block is {} bytes: Claude Code would keep it in a file", block.len());
    let in_block: Vec<String> = block
        .lines()
        .filter_map(|l| l.strip_prefix("- ["))
        .filter_map(|l| l.split_once(']').map(|(id, _)| id.to_string()))
        .collect();
    assert!(in_block.len() < 53 && !in_block.is_empty(), "{} rules in the block", in_block.len());
    let hidden = 53 - in_block.len();
    assert!(block.contains(&format!("{hidden} more standing rule(s) did not fit")), "{block}");

    let store = EventStore::open_existing(&db).unwrap();
    let mut served: Vec<String> = store
        .get_all_events()
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == EventKind::ItemServed && e.session_id == "session-one")
        .map(|e| e.entity_id)
        .collect();
    served.sort();
    let mut expected = in_block.clone();
    expected.sort();
    assert_eq!(served, expected, "served events must be exactly the rules that are in the block");

    let log = std::fs::read_to_string(dir.path().join("volume-log.jsonl")).expect("the start is in the volume record");
    let line = log.lines().find(|l| l.contains("\"k\":\"start\"")).expect("a start line");
    let v: serde_json::Value = serde_json::from_str(line).unwrap();
    assert_eq!(v["n"].as_u64().unwrap() as usize, hidden);
    assert_eq!(v["b"].as_u64().unwrap() as usize, block.len());
    assert_eq!(v["i"].as_array().unwrap().len(), in_block.len());
}

#[test]
fn a_small_standing_block_is_unchanged_and_all_of_it_counts_as_served() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("store.db");
    {
        let mut s = EventStore::new(&db).unwrap();
        for i in 0..4 {
            store_overflowing(&mut s, &standing_rule(i));
        }
    }
    let cwd = dir.path().join("workdir");
    std::fs::create_dir_all(&cwd).unwrap();
    let json = run_session_start(&db, &cwd, "session-two");
    let block = json["hookSpecificOutput"]["additionalContext"].as_str().unwrap();
    assert!(!block.contains("did not fit"), "{block}");
    assert_eq!(block.lines().filter(|l| l.starts_with("- [")).count(), 4);
    let served = EventStore::open_existing(&db)
        .unwrap()
        .get_all_events()
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == EventKind::ItemServed && e.session_id == "session-two")
        .count();
    assert_eq!(served, 4);
}

/// THE PINNED NOTES KEEP THEIR PLACE. The write gate lets the standing rules
/// fill the block up to `NO_PROJECT_STANDING_LIMIT_BYTES`; the real session start,
/// with every extra line it can carry at its largest here (the compaction
/// reminder, the no-project line and a full tools overview), must still show
/// every one of those rules, whole, within the hook output limit, keep the
/// compaction reminder whole, and say that the other extras were left out.
#[test]
fn a_block_the_gate_filled_to_its_limit_still_shows_every_pinned_note_beside_every_extra() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("store.db");
    let mut ids: Vec<String> = Vec::new();
    let mut used = model::item::standing_head_bytes();
    {
        let mut s = EventStore::new(&db).unwrap();
        // Descending lengths: each pass of the loop adds a note while the gate allows it.
        for len in (20..=260).rev() {
            let mut item = standing_rule(len);
            item.id = format!("pin-{len:03}");
            item.text = format!("{len:03} {}", "w".repeat(len));
            if store::declare(&mut s, "s", "l", "a", &item).is_ok() {
                used += model::item::standing_line(&item.id, &item.text).len() + 1;
                ids.push(item.id);
            }
        }
    }
    let room = model::item::NO_PROJECT_STANDING_LIMIT_BYTES - used;
    assert!(room < 40 && ids.len() > 10, "fixture: the gate filled the block ({room} bytes left, {} notes)", ids.len());

    // A full tools overview, one of the extras with a cap of its own.
    let _ = sixty_tools(dir.path());
    fill_the_no_project_line(&db, dir.path());
    // A folder that belongs to no project: the start adds the no-project line.
    let cwd = dir.path().join("workdir");
    std::fs::create_dir_all(&cwd).unwrap();

    let payload = serde_json::json!({ "session_id": "s-full", "hook_event_name": "SessionStart", "source": "compact", "cwd": cwd.to_string_lossy() });
    let mut child = Command::new(env!("CARGO_BIN_EXE_serve"))
        .arg("--db")
        .arg(&db)
        .arg("hook")
        .current_dir(&cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let _ = child.stdin.take().unwrap().write_all(payload.to_string().as_bytes());
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let block = json["hookSpecificOutput"]["additionalContext"].as_str().unwrap();

    assert!(block.len() <= model::item::HOOK_OUTPUT_LIMIT_BYTES, "{} bytes", block.len());
    for id in &ids {
        assert!(block.contains(&format!("- [{id}] ")), "pinned note {id} lost its place: {block}");
    }
    assert!(!block.contains("did not fit in this block"), "no standing rule may be cut: {block}");
    assert!(block.contains(model::item::COMPACT_REMINDER), "the reminder is whole");
    // The room left is the reserve: the tools overview gives way, the no-project line never does.
    assert!(block.contains("belongs to no project") && block.contains("request_directory"), "the no-project line survives a full pinned block: {block}");
    assert!(!block.contains("Helper tools"), "{block}");
    assert!(block.contains(model::item::EXTRAS_DROPPED_NOTE), "what did not fit is said: {block}");
}

/// `fit_extras` on its own: an extra that must be shown never loses its place,
/// the rest give way, and a drop is always named.
#[test]
fn extras_give_way_to_the_standing_rules_and_a_drop_is_named() {
    let reminder = serve::session_start::COMPACT_REMINDER.to_string();
    let big = "x".repeat(6_000);
    let rules_len = model::item::NO_PROJECT_STANDING_LIMIT_BYTES;
    let kept = serve::session_start::fit_extras(rules_len, vec![big.clone(), reminder.clone(), "short".to_string()]);
    assert!(kept.contains(&reminder) && !kept.contains(&big), "{kept:?}");
    assert_eq!(kept.last().map(String::as_str), Some(model::item::EXTRAS_DROPPED_NOTE));
    let total: usize = rules_len + kept.iter().map(|e| e.len() + 2).sum::<usize>();
    assert!(total <= model::item::HOOK_OUTPUT_LIMIT_BYTES, "{total}");
    // The no-project line is never the one that gives way.
    let line = serve::other_project::no_project_line();
    let kept = serve::session_start::fit_extras(rules_len, vec![big.clone(), line.clone(), reminder.clone()]);
    assert!(kept.contains(&line) && kept.contains(&reminder) && !kept.contains(&big), "{kept:?}");
    let total: usize = rules_len + kept.iter().map(|e| e.len() + 2).sum::<usize>();
    assert!(total <= model::item::HOOK_OUTPUT_LIMIT_BYTES, "{total}");
    // Plenty of room: nothing is left out and nothing is said.
    let all = serve::session_start::fit_extras(100, vec![big.clone(), reminder.clone()]);
    assert_eq!(all, vec![big, reminder]);
}

// ---------------------------------------------------------------------------
// The tools overview shrinks to the room the pinned notes leave, it does not vanish.

/// Pinned notes up to `target` bytes of standing block; returns their ids.
fn pins_up_to(db: &std::path::Path, target: usize) -> Vec<String> {
    let mut ids = Vec::new();
    let mut used = model::item::standing_head_bytes();
    let mut s = EventStore::new(db).unwrap();
    for len in (20..=260).rev() {
        let mut item = standing_rule(len);
        item.id = format!("pin-{len:03}");
        item.text = format!("{len:03} {}", "w".repeat(len));
        let line = model::item::standing_line(&item.id, &item.text).len() + 1;
        if used + line <= target && store::declare(&mut s, "s", "l", "a", &item).is_ok() {
            used += line;
            ids.push(item.id);
        }
    }
    ids
}

/// Registers working copies of a few long-named projects, so the no-project line
/// names folders and takes nearly all of its own reserve (what is left free is
/// small); the tests that need a full block use it.
fn fill_the_no_project_line(db: &std::path::Path, dir: &std::path::Path) {
    let mut s = EventStore::new(db).unwrap();
    for n in 0..10 {
        let name = format!("Registered-project-with-a-long-name-{n}");
        let root = dir.join(&name);
        std::fs::create_dir_all(root.join(".git")).unwrap();
        let record = Item {
            id: store::project_opened_id(&name),
            kind: Kind::Report,
            text: format!("Project {name}: a fixture record."),
            bindings: Vec::new(),
            severity: None,
            project: Some(name.clone()),
            tags: vec!["project-record".to_string()],
            expires: None,
            key: None,
            falsifier: None,
            check: None,
        };
        let body = store::canonical_body(&record).unwrap();
        let body = format!("{},\"root\":{},\"root_at\":\"2026-09-29T00:00:00Z\"}}", &body[..body.len() - 1], serde_json::to_string(root.to_str().unwrap()).unwrap());
        s.append_event("install", "install", store::INSTALLER_ACTOR, EventKind::FactCreated, &record.id, None, &body).unwrap();
    }
}

/// A tools folder of 60 tools named in the store's settings; returns its label.
fn sixty_tools(dir: &std::path::Path) -> String {
    // A long folder name: the no-project line leaves a little of its reserve free,
    // and the one-line overview must still be longer than that to be left out.
    let tools = dir.join("tools-folder-with-a-rather-long-name-so-that-even-the-shortest-overview-line-needs-room-and-does-not-fit-in-what-the-no-project-line-leaves-free");
    for n in 0..60 {
        let tool = tools.join(format!("tool-{n:02}"));
        std::fs::create_dir_all(&tool).unwrap();
        std::fs::write(tool.join("README.txt"), format!("Tool number {n} does one small job and says so in one plain line of text\n")).unwrap();
    }
    std::fs::write(dir.join("work-folders.json"), serde_json::json!({ "tools": tools }).to_string()).unwrap();
    tools.display().to_string()
}

/// A compact start in a folder of no project (so the reminder and the
/// no-project line come before the overview); the block it printed.
fn compact_start(db: &std::path::Path, dir: &std::path::Path, session: &str) -> String {
    let cwd = dir.join("workdir");
    std::fs::create_dir_all(&cwd).unwrap();
    let payload = serde_json::json!({ "session_id": session, "hook_event_name": "SessionStart", "source": "compact", "cwd": cwd.to_string_lossy() });
    let mut child = Command::new(env!("CARGO_BIN_EXE_serve"))
        .arg("--db")
        .arg(db)
        .arg("hook")
        .current_dir(&cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let _ = child.stdin.take().unwrap().write_all(payload.to_string().as_bytes());
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    json["hookSpecificOutput"]["additionalContext"].as_str().unwrap().to_string()
}

/// The bytes a start keeps for the lines before the overview: the reminder and
/// the no-project line, each with its separator.
fn lines_before_the_overview() -> usize {
    model::item::COMPACT_REMINDER.len() + 2 + serve::other_project::no_project_line().len() + 2
}

fn check_pins_and_limit(block: &str, ids: &[String]) {
    assert!(block.len() <= model::item::HOOK_OUTPUT_LIMIT_BYTES, "{} bytes", block.len());
    for id in ids {
        assert!(block.contains(&format!("- [{id}] ")), "pinned note {id} lost its place");
    }
    assert!(block.contains(model::item::COMPACT_REMINDER) && block.contains("belongs to no project"), "the lines that come first are there");
}

#[test]
fn with_plenty_of_room_the_overview_is_as_it_always_was() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("store.db");
    let ids = pins_up_to(&db, 3_000);
    let label = sixty_tools(dir.path());
    let block = compact_start(&db, dir.path(), "s-plenty");
    check_pins_and_limit(&block, &ids);
    let serve::work_folders::ToolsRead::Built { overview, .. } = serve::work_folders::read_tools(std::path::Path::new(&label), None).unwrap() else { panic!("built") };
    assert!(block.contains(&overview), "the whole overview, unchanged: {block}");
    assert!(!block.contains("did not fit in the hook output"), "{block}");
}

#[test]
fn with_a_nearly_full_pinned_block_the_overview_shows_fewer_tools_and_counts_the_rest() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("store.db");
    let wanted = 900;
    let ids = pins_up_to(&db, model::item::HOOK_OUTPUT_LIMIT_BYTES - lines_before_the_overview() - wanted - 2);
    let label = sixty_tools(dir.path());
    let block = compact_start(&db, dir.path(), "s-fewer");
    check_pins_and_limit(&block, &ids);
    let shown = block.lines().filter(|l| l.starts_with("tool-")).count();
    assert!(shown > 0 && shown < 15, "fewer tool lines than the full overview: {shown}");
    assert!(block.contains(&format!("... and {} more in {label}", 60 - shown)), "{block}");
    assert!(block.contains(serve::work_folders::OVERVIEW_ADVICE));
    assert!(!block.contains("did not fit in the hook output"), "shrunk, not dropped: {block}");
}

#[test]
fn with_room_for_one_line_the_overview_is_one_line_that_counts_the_tools() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("store.db");
    let label = sixty_tools(dir.path());
    let one = serve::work_folders::overview_one_line(&label, 60);
    let ids = pins_up_to(&db, model::item::HOOK_OUTPUT_LIMIT_BYTES - lines_before_the_overview() - one.len() - 2 - 5);
    let block = compact_start(&db, dir.path(), "s-one");
    check_pins_and_limit(&block, &ids);
    assert!(block.contains(&one), "{block}");
    assert!(block.lines().all(|l| !l.starts_with("tool-")) && !block.contains("Helper tools available in"), "{block}");
    assert!(!block.contains("did not fit in the hook output"), "{block}");
}

#[test]
fn with_no_room_the_overview_is_dropped_and_the_drop_is_named() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("store.db");
    let label = sixty_tools(dir.path());
    let one = serve::work_folders::overview_one_line(&label, 60);
    let ids = pins_up_to(&db, model::item::NO_PROJECT_STANDING_LIMIT_BYTES);
    fill_the_no_project_line(&db, dir.path());
    let block = compact_start(&db, dir.path(), "s-none");
    assert!(block.len() <= model::item::HOOK_OUTPUT_LIMIT_BYTES, "{} bytes", block.len());
    for id in &ids {
        assert!(block.contains(&format!("- [{id}] ")), "pinned note {id} lost its place");
    }
    assert!(!block.contains(&one) && !block.contains("Helper tools"), "{block}");
    assert!(block.contains(model::item::EXTRAS_DROPPED_NOTE) && block.contains(model::item::COMPACT_REMINDER), "{block}");
}

/// The order when room is short: the short lines come before the overview.
#[test]
fn a_short_warning_is_kept_before_the_overview_is_shrunk() {
    let warning = "[THOR] a short warning someone must act on".to_string();
    let overview = {
        let tools: Vec<serve::work_folders::ToolEntry> = (0..30)
            .map(|n| serve::work_folders::ToolEntry { name: format!("t{n:02}"), readme: serve::work_folders::ReadmeState::Line("does a job".to_string()) })
            .collect();
        serve::work_folders::assemble("/srv/tools", &tools, &[]).0
    };
    let rules_len = model::item::HOOK_OUTPUT_LIMIT_BYTES - model::item::COMPACT_REMINDER.len() - 2 - warning.len() - 2 - 300;
    let kept = serve::session_start::fit_extras(rules_len, vec![overview.clone(), warning.clone(), model::item::COMPACT_REMINDER.to_string()]);
    assert!(kept.contains(&warning), "{kept:?}");
    let shrunk = kept.iter().find(|l| l.starts_with("Helper tools")).expect("shrunk, not dropped");
    assert!(shrunk.len() <= 300 && shrunk.len() < overview.len(), "{shrunk}");
    assert!(!kept.contains(&model::item::EXTRAS_DROPPED_NOTE.to_string()), "{kept:?}");
}

/// THE BUDGET IS PER SORT OF START. The line for a folder of no project takes its
/// room from the rules of that block only: the write gate judges the global pins
/// against it, and a project keeps the whole old budget.
#[test]
fn only_the_block_of_a_folder_of_no_project_gives_room_to_the_line() {
    assert_eq!(model::item::STANDING_BLOCK_LIMIT_BYTES, 9_800 - model::item::START_EXTRAS_RESERVE_BYTES);
    assert_eq!(model::item::NO_PROJECT_STANDING_LIMIT_BYTES + model::item::NO_PROJECT_LINE_MAX_BYTES + 2, model::item::STANDING_BLOCK_LIMIT_BYTES);

    // Global pins: the gate stops them at the no-project limit, whatever is asked.
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("store.db");
    let ids = pins_up_to(&db, model::item::STANDING_BLOCK_LIMIT_BYTES);
    let used: usize = model::item::standing_head_bytes()
        + ids.iter().map(|id| {
            let len: usize = id[4..].parse().unwrap();
            model::item::standing_line(id, &format!("{len:03} {}", "w".repeat(len))).len() + 1
        }).sum::<usize>();
    assert!(used <= model::item::NO_PROJECT_STANDING_LIMIT_BYTES, "{used}");

    // A project's own pins may fill the old budget, and a start in that folder shows them all.
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("store.db");
    let project = dir.path().join("proj");
    std::fs::create_dir_all(project.join(".git")).unwrap();
    let mut shown = Vec::new();
    let mut used = model::item::standing_head_bytes();
    {
        let mut s = EventStore::new(&db).unwrap();
        for len in (20..=260).rev() {
            let mut item = standing_rule(len);
            item.id = format!("pin-{len:03}");
            item.text = format!("{len:03} {}", "w".repeat(len));
            item.project = Some("proj".to_string());
            if store::declare(&mut s, "s", "l", "a", &item).is_ok() {
                used += model::item::standing_line(&item.id, &item.text).len() + 1;
                shown.push(item.id);
            }
        }
    }
    assert!(used > model::item::NO_PROJECT_STANDING_LIMIT_BYTES && used <= model::item::STANDING_BLOCK_LIMIT_BYTES, "{used}");
    let json = run_session_start(&db, &project, "s-project");
    let block = json["hookSpecificOutput"]["additionalContext"].as_str().unwrap();
    for id in &shown {
        assert!(block.contains(&format!("- [{id}] ")), "{id} lost its place in a project");
    }
    assert!(!block.contains("belongs to no project"), "{block}");
}
