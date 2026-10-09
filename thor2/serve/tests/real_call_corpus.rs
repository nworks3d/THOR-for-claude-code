//! THE REAL-CALL CORPUS (`serve::corpus`): the tool calls and the replies THOR
//! keeps in two sidecars beside the store, so a rule can be tried against what
//! really happened.
//!
//! Every case builds a transcript the way Claude Code writes one (a
//! `tool_use` block in an assistant line, its `tool_result` in a user line)
//! and reads it back through the same functions the Stop hook uses. The names
//! in it are neutral: nothing here is a real user, host or project.

use serde_json::json;
use serve::corpus::{self, CallRow, FileCtx};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

const SESSION: &str = "0a0a0a0a-1111-4222-8333-444444444444";
const NOW: i64 = 1_791_000_000; // 2026-10-03

fn at(secs_before_now: i64) -> String {
    // `unix_of_timestamp` reads the fraction away; the date is what matters.
    format!("{}.000Z", serve::time::iso8601_from_unix(NOW - secs_before_now).trim_end_matches('Z'))
}

fn use_line(id: &str, tool: &str, input: serde_json::Value, ago: i64) -> String {
    json!({
        "type": "assistant", "timestamp": at(ago), "cwd": "/work/demo", "sessionId": SESSION,
        "message": {"content": [{"type": "tool_use", "id": id, "name": tool, "input": input}]},
    })
    .to_string()
}

fn result_line(id: &str, text: &str, is_error: bool, ago: i64) -> String {
    json!({
        "type": "user", "timestamp": at(ago), "cwd": "/work/demo", "sessionId": SESSION,
        "message": {"content": [{"type": "tool_result", "tool_use_id": id, "content": text, "is_error": is_error}]},
    })
    .to_string()
}

struct Fx {
    dir: tempfile::TempDir,
    home: tempfile::TempDir,
}

impl Fx {
    fn new() -> Fx {
        let fx = Fx { dir: tempfile::tempdir().unwrap(), home: tempfile::tempdir().unwrap() };
        thor_core::event_store::EventStore::new(&fx.db()).unwrap();
        fx
    }
    fn db(&self) -> PathBuf {
        self.dir.path().join("thor.db")
    }
    fn root(&self) -> PathBuf {
        self.home.path().join(".claude").join("projects")
    }
    fn transcript(&self) -> PathBuf {
        let folder = self.root().join("C--demo");
        std::fs::create_dir_all(&folder).unwrap();
        folder.join(format!("{SESSION}.jsonl"))
    }
    fn write(&self, lines: &[String]) {
        let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
        std::fs::write(self.transcript(), text).unwrap();
    }
    fn append(&self, lines: &[String]) {
        let mut file = std::fs::OpenOptions::new().append(true).open(self.transcript()).unwrap();
        for l in lines {
            writeln!(file, "{l}").unwrap();
        }
    }
    fn record(&self) -> corpus::Recorded {
        corpus::record_session(&self.db(), SESSION, None, Some(&self.root()), None, NOW)
    }
    fn calls(&self) -> Vec<CallRow> {
        corpus::load_calls(&self.db(), NOW)
    }
}

fn demo_project(_dir: &Path) -> Option<String> {
    Some("demo".to_string())
}

fn ctx(project_of: &dyn Fn(&Path) -> Option<String>) -> FileCtx<'_> {
    FileCtx { session: "0a0a0a0a".to_string(), agent: false, folder: "C--demo".to_string(), project_of, fallback: None, build: String::new() }
}

#[test]
fn a_transcript_becomes_call_rows_with_what_became_of_each_call() {
    let fx = Fx::new();
    let refusal = "PreToolUse:Bash hook error: [THOR] rule acme-wipe-ban forbids this text (\"never wipe\"); found in what is about to be written: \"acme-tool --wipe\"";
    fx.write(&[
        use_line("toolu_ok", "Bash", json!({"command": "ls -la"}), 600),
        result_line("toolu_ok", "total 0", false, 599),
        use_line("toolu_refused", "Bash", json!({"command": "acme-tool --wipe"}), 500),
        result_line("toolu_refused", refusal, true, 499),
        use_line("toolu_other", "Bash", json!({"command": "git commit -m x"}), 400),
        result_line("toolu_other", "PreToolUse:Bash hook error: the precommit gate says no", true, 399),
        use_line("toolu_failed", "Bash", json!({"command": "false"}), 300),
        result_line("toolu_failed", "Exit code 1", true, 299),
    ]);
    let project_of: &dyn Fn(&Path) -> Option<String> = &demo_project;
    let slice = corpus::read_slice(&fx.transcript(), 0, u64::MAX, None, &ctx(project_of), None).unwrap();
    let by = |id: &str| slice.calls.iter().find(|c| c.id == id).unwrap_or_else(|| panic!("no row for {id}"));

    let ok = by("toolu_ok");
    assert_eq!((ok.ok, ok.r.is_empty(), ok.oth), (Some(true), true, false));
    assert_eq!(ok.tool, "Bash");
    assert_eq!(ok.input["command"], "ls -la");
    assert_eq!(ok.p.as_deref(), Some("demo"));
    assert_eq!(ok.cwd, "/work/demo");
    assert_eq!(ok.s, "0a0a0a0a");

    let refused = by("toolu_refused");
    assert_eq!(refused.r, vec!["acme-wipe-ban".to_string()], "the rule the hook refused it under");
    assert_eq!(refused.ok, Some(false));
    assert!(!refused.oth);

    let other = by("toolu_other");
    assert!(other.oth && other.r.is_empty(), "another hook's refusal is not THOR's");

    let failed = by("toolu_failed");
    assert_eq!((failed.ok, failed.r.is_empty(), failed.oth), (Some(false), true, false), "a failed call is not a refused one");
    assert!(slice.complete);
}

#[test]
fn a_secret_in_a_call_is_redacted_and_a_long_text_is_cut_and_marked() {
    let fx = Fx::new();
    let long = "x".repeat(corpus::STRING_CLIP + 500);
    fx.write(&[
        use_line("toolu_secret", "Bash", json!({"command": "export DEMO_TOKEN=s3cr3tvalue123456 && run"}), 100),
        use_line("toolu_long", "Write", json!({"file_path": "a.txt", "content": long}), 90),
        use_line("toolu_page", "WebFetch", json!({"url": "https://example.test/", "prompt": "y".repeat(3000)}), 80),
    ]);
    fx.record();
    let calls = fx.calls();
    let secret = calls.iter().find(|c| c.id == "toolu_secret").unwrap();
    let text = secret.input["command"].as_str().unwrap();
    assert!(!text.contains("s3cr3tvalue123456"), "the log's own redaction applies: {text}");
    assert!(text.contains("DEMO_TOKEN"), "the rest of the command stays: {text}");
    let long_row = calls.iter().find(|c| c.id == "toolu_long").unwrap();
    assert!(long_row.cut, "a text over the clip marks the call");
    assert_eq!(long_row.input["content"].as_str().unwrap().chars().count(), corpus::STRING_CLIP);
    let page = calls.iter().find(|c| c.id == "toolu_page").unwrap();
    assert!(page.cut, "a tool no guard reads keeps far less of its text");
    assert_eq!(page.input["prompt"].as_str().unwrap().chars().count(), corpus::OTHER_TOOL_CLIP);
}

#[test]
fn reading_resumes_where_it_stopped_and_never_records_a_call_twice() {
    let fx = Fx::new();
    fx.write(&[use_line("toolu_a", "Bash", json!({"command": "echo a"}), 300), result_line("toolu_a", "a", false, 299)]);
    let first = fx.record();
    assert_eq!(first.calls, 1);
    let again = fx.record();
    assert_eq!(again.calls, 0, "nothing new, nothing recorded");
    fx.append(&[use_line("toolu_b", "Bash", json!({"command": "echo b"}), 200), result_line("toolu_b", "b", false, 199)]);
    let third = fx.record();
    assert_eq!(third.calls, 1, "only the new call");
    let ids: Vec<String> = fx.calls().into_iter().map(|c| c.id).collect();
    assert_eq!(ids, vec!["toolu_a".to_string(), "toolu_b".to_string()]);
}

#[test]
fn a_result_that_comes_later_patches_its_call_and_a_line_still_being_written_waits() {
    let fx = Fx::new();
    fx.write(&[use_line("toolu_late", "Bash", json!({"command": "sleep 1"}), 300)]);
    fx.record();
    assert_eq!(fx.calls()[0].ok, None, "no result yet: unknown, not a failure");
    // The result arrives after the call's line was recorded; the next line is
    // still being written (no newline yet).
    fx.append(&[result_line("toolu_late", "done", false, 250)]);
    let mut file = std::fs::OpenOptions::new().append(true).open(fx.transcript()).unwrap();
    write!(file, "{{\"type\":\"assistant\",\"timestamp\":\"{}\",\"mess", at(200)).unwrap();
    drop(file);
    fx.record();
    let calls = fx.calls();
    assert_eq!(calls.len(), 1, "the patch is not a second call");
    assert_eq!(calls[0].ok, Some(true), "the late result is applied");
    // The half line is finished: now it is read, once.
    let mut file = std::fs::OpenOptions::new().append(true).open(fx.transcript()).unwrap();
    writeln!(
        file,
        "age\":{{\"content\":[{{\"type\":\"tool_use\",\"id\":\"toolu_next\",\"name\":\"Bash\",\"input\":{{\"command\":\"pwd\"}}}}]}}}}"
    )
    .unwrap();
    drop(file);
    fx.record();
    assert_eq!(fx.calls().len(), 2);
}

#[test]
fn a_hook_attachment_marks_the_call_so_a_session_with_a_live_hook_can_be_told() {
    let fx = Fx::new();
    let attachment = json!({
        "type": "attachment", "timestamp": at(290), "uuid": "u1",
        "attachment": {"type": "hook_additional_context", "hookEvent": "PreToolUse", "toolUseID": "toolu_h", "content": ["x"]},
    })
    .to_string();
    fx.write(&[use_line("toolu_h", "Bash", json!({"command": "ls"}), 300), attachment, result_line("toolu_h", "ok", false, 280)]);
    fx.record();
    assert!(fx.calls()[0].h, "a hook spoke on this call");
}

#[test]
fn a_subagent_file_of_the_session_is_read_too_and_marked() {
    let fx = Fx::new();
    fx.write(&[use_line("toolu_main", "Bash", json!({"command": "ls"}), 300)]);
    let sub = fx.root().join("C--demo").join(SESSION).join("subagents");
    std::fs::create_dir_all(&sub).unwrap();
    std::fs::write(sub.join("agent-abc123.jsonl"), format!("{}\n", use_line("toolu_sub", "Bash", json!({"command": "pwd"}), 200))).unwrap();
    let done = fx.record();
    assert_eq!(done.files, 2);
    let calls = fx.calls();
    assert!(!calls.iter().find(|c| c.id == "toolu_main").unwrap().a);
    assert!(calls.iter().find(|c| c.id == "toolu_sub").unwrap().a, "made by a subagent");
    assert_eq!(calls.iter().find(|c| c.id == "toolu_sub").unwrap().s, "0a0a0a0a", "the session of the folder it lives in");
}

#[test]
fn the_corpus_keeps_thirty_days_and_its_cap_and_drops_the_oldest_first() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    let row = |id: &str, t: i64| {
        serde_json::to_string(&CallRow {
            k: "c".into(), id: id.into(), t, s: String::new(), a: false, tool: "Bash".into(), cwd: String::new(), p: None, d: None,
            input: json!({"command": "x".repeat(200)}), r: vec![], oth: false, ok: Some(true), h: false, cut: false, b: String::new(),
        })
        .unwrap()
    };
    let day = 86_400;
    let lines = vec![
        row("old", NOW - 31 * day),
        row("a", NOW - 5 * day),
        row("b", NOW - 4 * day),
        row("c", NOW - 3 * day),
        row("d", NOW - day),
    ];
    let path = corpus::calls_path(&db);
    std::fs::write(&path, format!("{}\n", lines.join("\n"))).unwrap();
    corpus::prune_file(&path, 10 * 1024 * 1024, NOW);
    let ids: Vec<String> = corpus::load_calls(&db, NOW).into_iter().map(|c| c.id).collect();
    assert_eq!(ids, ["a", "b", "c", "d"], "the row older than 30 days went");
    // A cap that holds two rows and a bit keeps the newest two.
    let one = lines[1].len() as u64 + 1;
    corpus::prune_file(&path, one * 2 + one / 2, NOW);
    let ids: Vec<String> = corpus::load_calls(&db, NOW).into_iter().map(|c| c.id).collect();
    assert_eq!(ids, ["c", "d"], "the oldest went first: {ids:?}");
}

#[test]
fn a_held_store_directory_takes_no_corpus_write() {
    let fx = Fx::new();
    fx.write(&[use_line("toolu_a", "Bash", json!({"command": "ls"}), 100)]);
    {
        let _hold = serve::store_dir::hold(&fx.db());
        let done = fx.record();
        assert_eq!(done.calls, 0);
        corpus::record_reply(&fx.db(), &corpus::reply_row(NOW, SESSION, false, false, "/work", "a reply", "a prompt", &[], false, false, ""));
    }
    assert!(!corpus::calls_path(&fx.db()).exists(), "--no-record leaves the directory as it found it");
    assert!(!corpus::replies_path(&fx.db()).exists());
}

#[test]
fn backfill_reads_every_recent_transcript_once() {
    let fx = Fx::new();
    fx.write(&[use_line("toolu_a", "Bash", json!({"command": "ls"}), 100), use_line("toolu_b", "Read", json!({"file_path": "a"}), 90)]);
    let other = fx.root().join("C--other");
    std::fs::create_dir_all(&other).unwrap();
    std::fs::write(
        other.join("11111111-2222-4333-8444-555555555555.jsonl"),
        format!("{}\n", use_line("toolu_c", "Bash", json!({"command": "pwd"}), 80)),
    )
    .unwrap();
    let done = corpus::backfill(&fx.db(), &fx.root(), Duration::from_secs(30), serve::time::now_unix());
    assert_eq!((done.files_total, done.files_read, done.calls, done.complete), (2, 2, 3, true));
    let again = corpus::backfill(&fx.db(), &fx.root(), Duration::from_secs(30), serve::time::now_unix());
    assert_eq!(again.calls, 0, "a second backfill finds nothing new");
}

fn hook(fx: &Fx, payload: serde_json::Value) -> String {
    let mut child = Command::new(env!("CARGO_BIN_EXE_serve"))
        .arg("--db")
        .arg(fx.db())
        .arg("hook")
        .env("HOME", fx.home.path())
        .env("USERPROFILE", fx.home.path())
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

#[test]
fn the_stop_hook_records_the_calls_of_the_turn_and_the_reply_the_guard_judged() {
    let fx = Fx::new();
    std::fs::write(
        fx.dir.path().join("guard-response-rulebook.json"),
        r#"[{"id":"ask-user-to-check","any_of":["can you check"],"none_of":["any_of"],"reminder":"check it yourself first"}]"#,
    )
    .unwrap();
    // The recent past, so the rows are inside the 30 days the sweep keeps.
    let now = serve::time::now_unix();
    let line_at = |ago: i64, id: &str| {
        json!({
            "type": "assistant", "timestamp": format!("{}.000Z", serve::time::iso8601_from_unix(now - ago).trim_end_matches('Z')),
            "cwd": "/work/demo", "sessionId": SESSION,
            "message": {"content": [{"type": "tool_use", "id": id, "name": "Bash", "input": {"command": "cargo test"}}]},
        })
        .to_string()
    };
    fx.write(&[line_at(60, "toolu_turn")]);
    let out = hook(
        &fx,
        json!({
            "hook_event_name": "Stop", "session_id": SESSION, "transcript_path": fx.transcript().to_string_lossy(),
            "cwd": fx.dir.path().to_string_lossy(), "last_assistant_message": "can you check whether the file exists?",
        }),
    );
    assert!(out.contains("\"block\""), "the guard held this reply: {out}");
    let calls = corpus::load_calls(&fx.db(), now);
    assert_eq!(calls.len(), 1, "the call of the turn is in the corpus");
    assert_eq!(calls[0].input["command"], "cargo test");
    let replies = corpus::load_replies(&fx.db(), now);
    assert_eq!(replies.len(), 1);
    assert!(replies[0].reply.contains("can you check"));
    assert!(replies[0].held && !replies[0].warn, "what the guard said at the time is kept");
    assert!(replies[0].why.contains("check it yourself first"));
    assert!(replies[0].cmds.iter().any(|c| c == "cargo test"), "and the turn's tool-call list: {:?}", replies[0].cmds);
    assert!(!replies[0].a && !replies[0].o);
}

#[test]
fn a_subagents_stop_records_its_reply_but_leaves_the_calls_to_the_main_session() {
    let fx = Fx::new();
    std::fs::write(
        fx.dir.path().join("guard-response-rulebook.json"),
        r#"[{"id":"ask-user-to-check","any_of":["can you check"],"none_of":["any_of"],"reminder":"check it yourself first"}]"#,
    )
    .unwrap();
    fx.write(&[use_line("toolu_x", "Bash", json!({"command": "ls"}), 60)]);
    hook(
        &fx,
        json!({
            "hook_event_name": "Stop", "session_id": SESSION, "agent_id": "abc", "transcript_path": fx.transcript().to_string_lossy(),
            "cwd": fx.dir.path().to_string_lossy(), "last_assistant_message": "all done here.",
        }),
    );
    let now = serve::time::now_unix();
    assert!(corpus::load_calls(&fx.db(), now).is_empty());
    let replies = corpus::load_replies(&fx.db(), now);
    assert_eq!(replies.len(), 1);
    assert!(replies[0].a, "marked as a subagent's reply");
}

fn prompt_line(text: &str, ago: i64) -> String {
    json!({"type": "user", "timestamp": at(ago), "cwd": "/work/demo", "sessionId": SESSION, "message": {"role": "user", "content": text}}).to_string()
}

fn reply_line(id: &str, text: &str, ago: i64) -> String {
    json!({
        "type": "assistant", "timestamp": at(ago), "cwd": "/work/demo", "sessionId": SESSION,
        "message": {"id": id, "content": [{"type": "text", "text": text}]},
    })
    .to_string()
}

fn stop_line(said: &[&str], ago: i64) -> String {
    json!({
        "type": "system", "subtype": "stop_hook_summary", "timestamp": at(ago), "cwd": "/work/demo", "sessionId": SESSION,
        "hookErrors": said, "hookAdditionalContext": [], "preventedContinuation": false,
    })
    .to_string()
}

#[test]
fn a_back_fill_reads_each_reply_with_its_prompt_its_tool_calls_and_what_the_guard_said() {
    let fx = Fx::new();
    fx.write(&[
        prompt_line("did the build pass?", 900),
        use_line("toolu_a", "Bash", json!({"command": "cargo test"}), 890),
        result_line("toolu_a", "ok", false, 880),
        reply_line("msg_1", "I checked: it passed.", 870),
        stop_line(&["[THOR] check it yourself first"], 860),
        prompt_line("and the other one?", 500),
        reply_line("msg_2", "can you check that one for me?", 490),
        stop_line(&[], 480),
        // A turn still open: its prompt and a reply, no Stop yet.
        prompt_line("one more", 100),
        reply_line("msg_3", "working on it", 90),
    ]);
    let ctx_project: &dyn Fn(&Path) -> Option<String> = &demo_project;
    let ctx = ctx(ctx_project);
    let slice = corpus::read_replies(&fx.transcript(), 0, u64::MAX, None, &ctx).unwrap();
    assert_eq!(slice.replies.len(), 2, "one reply per Stop");
    let first = &slice.replies[0];
    assert_eq!(first.reply, "I checked: it passed.");
    assert_eq!(first.prompt, "did the build pass?");
    assert_eq!(first.cmds, vec!["cargo test".to_string()], "the turn's tool-call list");
    assert!(first.held && first.why.contains("check it yourself first"), "what THOR said then");
    assert_eq!(first.s, "0a0a0a0a");
    let second = &slice.replies[1];
    assert_eq!(second.prompt, "and the other one?");
    assert!(second.cmds.is_empty() && !second.held, "the second turn ran nothing and heard nothing");
    // The open turn is read again, whole, next time: the cursor stops at its prompt.
    let text = std::fs::read_to_string(fx.transcript()).unwrap();
    let open_at = text.find("one more").unwrap();
    let line_start = text[..open_at].rfind('\n').map_or(0, |i| i + 1) as u64;
    assert_eq!(slice.end, line_start, "{} vs {line_start}", slice.end);
}

#[test]
fn a_back_fill_of_replies_runs_once_and_a_reply_the_live_stop_also_recorded_counts_once() {
    let fx = Fx::new();
    let now = serve::time::now_unix();
    let recent = |ago: i64| format!("{}.000Z", serve::time::iso8601_from_unix(now - ago).trim_end_matches('Z'));
    let lines = vec![
        json!({"type": "user", "timestamp": recent(900), "sessionId": SESSION, "message": {"role": "user", "content": "status?"}}).to_string(),
        json!({"type": "assistant", "timestamp": recent(890), "sessionId": SESSION, "message": {"id": "m1", "content": [{"type": "text", "text": "all green"}]}}).to_string(),
        json!({"type": "system", "subtype": "stop_hook_summary", "timestamp": recent(880), "sessionId": SESSION, "hookErrors": [], "cwd": "/work/demo"}).to_string(),
    ];
    fx.write(&lines);
    // The live Stop recorded the same reply a moment earlier.
    corpus::record_reply(&fx.db(), &corpus::reply_row(now - 881, SESSION, false, false, "/work/demo", "all green", "status?", &[], false, false, ""));
    let done = corpus::backfill(&fx.db(), &fx.root(), Duration::from_secs(30), now);
    assert_eq!(done.replies, 1);
    assert_eq!(corpus::load_replies(&fx.db(), now).len(), 1, "one reply, not two");
    let again = corpus::backfill(&fx.db(), &fx.root(), Duration::from_secs(30), now);
    assert_eq!(again.replies, 0, "a second back-fill finds nothing new");
}

#[test]
fn the_stop_stamps_each_call_with_the_build_that_recorded_it_and_a_back_fill_does_not() {
    let fx = Fx::new();
    fx.write(&[use_line("toolu_a", "Bash", json!({"command": "ls"}), 100)]);
    fx.record();
    assert_eq!(fx.calls()[0].b, serve::BUILD_ID, "a live Stop knows its own build");
    let other = Fx::new();
    other.write(&[use_line("toolu_b", "Bash", json!({"command": "ls"}), 100)]);
    corpus::backfill(&other.db(), &other.root(), Duration::from_secs(30), serve::time::now_unix());
    let rows = corpus::load_calls(&other.db(), serve::time::now_unix());
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].b, "", "a call read back from a transcript has no known engine");
}

#[test]
fn a_read_that_runs_out_of_time_loses_no_call_and_the_next_read_starts_where_it_stopped() {
    let fx = Fx::new();
    let lines: Vec<String> = (0..600).map(|i| use_line(&format!("toolu_{i:04}"), "Bash", json!({"command": format!("echo {i}")}), 1_000 - i)).collect();
    fx.write(&lines);
    let project_of: &dyn Fn(&Path) -> Option<String> = &demo_project;
    let ctx = ctx(project_of);
    let first = corpus::read_slice(&fx.transcript(), 0, u64::MAX, Some(std::time::Instant::now()), &ctx, None).unwrap();
    assert!(!first.complete, "the time was already up");
    assert!(first.calls.len() < 600, "{} calls", first.calls.len());
    let second = corpus::read_slice(&fx.transcript(), first.end, u64::MAX, None, &ctx, first.project.clone()).unwrap();
    assert!(second.complete);
    let mut ids: Vec<String> = first.calls.iter().chain(second.calls.iter()).map(|c| c.id.clone()).collect();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), 600, "every call exactly once: {} + {}", first.calls.len(), second.calls.len());
}

#[test]
fn switching_recording_off_keeps_nothing_and_the_sweep_says_it_has_no_data() {
    let fx = Fx::new();
    fx.write(&[use_line("toolu_a", "Bash", json!({"command": "ls"}), 100)]);
    assert!(serve::plan::recording_on(&fx.db()), "on until the user switches it off");
    serve::plan::set_recording(&fx.db(), false).unwrap();
    assert!(!serve::plan::recording_on(&fx.db()));

    assert_eq!(fx.record().calls, 0, "no call is recorded at a Stop");
    corpus::record_reply(&fx.db(), &corpus::reply_row(NOW, SESSION, false, false, "/work", "a reply", "a prompt", &[], false, false, ""));
    let filled = corpus::backfill(&fx.db(), &fx.root(), Duration::from_secs(30), serve::time::now_unix());
    assert_eq!((filled.calls, filled.replies), (0, 0), "and a back-fill records nothing either");
    assert!(!corpus::calls_path(&fx.db()).exists() && !corpus::replies_path(&fx.db()).exists(), "no file is even created");

    let store = thor_core::event_store::EventStore::open_read_only(&fx.db()).unwrap();
    let report = serve::sweep::run(&fx.db(), &store, &serve::sweep::Opts::new(Duration::from_secs(5)));
    let lines = serve::sweep::detail_lines(&report);
    assert!(lines[0].contains("no data") && lines[0].contains("--set-recording on"), "{lines:?}");
    assert!(!lines.iter().any(|l| l.contains("none would refuse")), "an empty sweep must not read as 'all fine': {lines:?}");

    serve::plan::set_recording(&fx.db(), true).unwrap();
    assert_eq!(fx.record().calls, 1, "switched back on, recording resumes");
}
