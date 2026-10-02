//! THE VOLUME RECORD (step 28): how much THOR talks, kept so the owner can see
//! whether sessions get nagged.
//!
//! The owner's decision, 2026-10-02: "doctor moet deze dan maar bijhouden". A
//! session must not be handed something to judge every quarter of an hour, and
//! the facts shown to it are limited to one full showing per context
//! (`repeat`); doctor shows whether that holds.
//!
//! One small append-only file beside the store, `volume-log.jsonl`, one JSON
//! line per hook call that showed or hid something, and one per Stop:
//!
//! - `{"t":<unix>,"k":"show","s":"<session>","i":["<id>",...],"r":<n>}` - the
//!   facts shown in full (`i`) and how many repeats were left out (`r`);
//! - `{"t":<unix>,"k":"stop","s":"<session>","h":0|1}` - a Stop, and whether it
//!   held the turn;
//! - `{"t":<unix>,"k":"start","s":"<session>","i":["<id>",...],"n":<n>,"b":<bytes>}` -
//!   a session start (step 29): the standing rules actually put in the
//!   block (`i`), how many did not fit and were left out (`n`), and the bytes
//!   the block carried (`b`). Claude Code keeps only about the first 2 KB of a
//!   hook output over 10,000 characters, so a session start's size matters.
//!
//! Best effort like every sidecar: a line that cannot be written is lost, never
//! an error for the hook. Nothing here ever reads the store, and doctor only
//! reads this file. The file is cut to the last `KEEP_DAYS` days when it grows
//! past `PRUNE_ABOVE_BYTES`.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

/// The file beside the store.
pub const FILE: &str = "volume-log.jsonl";

/// How long a line is kept.
const KEEP_DAYS: i64 = 8;

/// The file is cut to `KEEP_DAYS` once it is bigger than this.
const PRUNE_ABOVE_BYTES: u64 = 3 * 1024 * 1024;

/// The windows doctor reports, in seconds, with their labels.
pub const WINDOWS: [(&str, i64); 2] = [("24 hours", 24 * 3600), ("7 days", 7 * 24 * 3600)];

/// How many of the most shown facts doctor names.
pub const TOP: usize = 5;

fn path(db: &Path) -> PathBuf {
    db.parent().unwrap_or_else(|| Path::new(".")).join(FILE)
}

fn append(db: &Path, line: String, now: i64) {
    let file = path(db);
    if crate::store_dir::is_held(&file) {
        return;
    }
    let opened = std::fs::OpenOptions::new().create(true).append(true).open(&file);
    let Ok(mut opened) = opened else { return };
    if writeln!(opened, "{line}").is_err() {
        return;
    }
    if opened.metadata().map(|m| m.len() > PRUNE_ABOVE_BYTES).unwrap_or(false) {
        drop(opened);
        prune(&file, now);
    }
}

fn prune(file: &Path, now: i64) {
    let Ok(text) = std::fs::read_to_string(file) else { return };
    let from = now - KEEP_DAYS * 24 * 3600;
    let kept: Vec<&str> = text.lines().filter(|l| parse(l).is_some_and(|e| e.t >= from)).collect();
    let _ = std::fs::write(file, format!("{}\n", kept.join("\n")));
}

/// One hook call showed `shown` facts in full and left `suppressed` repeats
/// out. Nothing is written when both are nothing.
pub fn record_showing(db: &Path, session_id: &str, shown: &[&str], suppressed: usize) {
    if shown.is_empty() && suppressed == 0 {
        return;
    }
    let now = crate::time::now_unix();
    let line = serde_json::json!({ "t": now, "k": "show", "s": session_id, "i": shown, "r": suppressed });
    append(db, line.to_string(), now);
}

/// A session start of `session_id` put `shown` standing rules in a block of
/// `bytes` bytes and left `hidden` out because they did not fit.
///
/// A headless start also records what it kept out (step 32): `lo` the number and
/// `li` the ids of the facts left out as interactive-only, `ao` and `ai` the
/// same for the facts bound only to the reply, `go` and `gi` for the global facts
/// cut to the headless cap (step 33). Absent for an interactive start.
pub fn record_start(
    db: &Path,
    session_id: &str,
    shown: &[&str],
    hidden: usize,
    bytes: usize,
    left_out: &crate::session_start::LeftOut,
) {
    let now = crate::time::now_unix();
    let mut line = serde_json::json!({ "t": now, "k": "start", "s": session_id, "i": shown, "n": hidden, "b": bytes });
    add_left_out(&mut line, left_out);
    append(db, line.to_string(), now);
}

fn add_left_out(line: &mut serde_json::Value, left_out: &crate::session_start::LeftOut) {
    if left_out.interactive_only.is_empty() && left_out.answer_only.is_empty() && left_out.global_capped.is_empty() {
        return;
    }
    line["lo"] = serde_json::json!(left_out.interactive_only.len());
    line["li"] = serde_json::json!(left_out.interactive_only);
    line["ao"] = serde_json::json!(left_out.answer_only.len());
    line["ai"] = serde_json::json!(left_out.answer_only);
    // Step 33: the global facts a headless start cut to its cap.
    line["go"] = serde_json::json!(left_out.global_capped.len());
    line["gi"] = serde_json::json!(left_out.global_capped);
}

/// How a session start saw the session was started (step 31): the entrypoint
/// Claude Code named (`env`: the hook's environment, `transcript`: the head
/// of the session's transcript, `none`: neither) and whether that made the
/// session headless. Only for the owner to check that the detection saw
/// something; `doctor` does not read it (an unknown line kind is skipped).
/// The same `lo`/`li`/`ao`/`ai` fields as the start line say what a headless
/// start left out (step 32).
pub fn record_entrypoint(
    db: &Path,
    session_id: &str,
    entrypoint: &str,
    source: &str,
    headless: bool,
    left_out: &crate::session_start::LeftOut,
) {
    let now = crate::time::now_unix();
    let mut line =
        serde_json::json!({ "t": now, "k": "entry", "s": session_id, "e": entrypoint, "from": source, "headless": headless });
    add_left_out(&mut line, left_out);
    append(db, line.to_string(), now);
}

/// A Stop of `session_id`, and whether it held the turn.
pub fn record_stop(db: &Path, session_id: &str, held: bool) {
    let now = crate::time::now_unix();
    let line = serde_json::json!({ "t": now, "k": "stop", "s": session_id, "h": u8::from(held) });
    append(db, line.to_string(), now);
}

struct Entry {
    t: i64,
    kind: Kind,
    session: String,
}

enum Kind {
    Show { ids: Vec<String>, suppressed: usize },
    Stop { held: bool },
    Start { shown: usize, hidden: usize, bytes: usize },
}

fn parse(line: &str) -> Option<Entry> {
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    let kind = match v.get("k")?.as_str()? {
        "show" => Kind::Show {
            ids: v.get("i")?.as_array()?.iter().filter_map(|x| x.as_str().map(str::to_string)).collect(),
            suppressed: v.get("r")?.as_u64()? as usize,
        },
        "stop" => Kind::Stop { held: v.get("h")?.as_u64()? != 0 },
        "start" => Kind::Start {
            shown: v.get("i")?.as_array()?.len(),
            hidden: v.get("n")?.as_u64()? as usize,
            bytes: v.get("b")?.as_u64()? as usize,
        },
        _ => return None,
    };
    Some(Entry { t: v.get("t")?.as_i64()?, kind, session: v.get("s")?.as_str()?.to_string() })
}

/// The facts shown in full at a prompt or before a tool call (never a session
/// start) in any of `sessions` since `from` (unix seconds). What a project's
/// own sessions have been using lately; a session start's block ranks by it
/// (`session_start::render_within_ranked`). Empty when the file is missing.
pub fn shown_in_sessions_since(
    db: &Path,
    sessions: &std::collections::BTreeSet<String>,
    from: i64,
) -> std::collections::BTreeSet<String> {
    let mut out = std::collections::BTreeSet::new();
    if sessions.is_empty() {
        return out;
    }
    for entry in read(db).into_iter().filter(|e| e.t >= from && sessions.contains(&e.session)) {
        if let Kind::Show { ids, .. } = entry.kind {
            out.extend(ids);
        }
    }
    out
}

/// What one window of the record adds up to.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Summary {
    /// Facts shown in full.
    pub full: usize,
    /// Repeats left out because the context had seen the fact.
    pub suppressed: usize,
    /// The most shown facts, most first, ties by id.
    pub top: Vec<(String, usize)>,
    /// Stops that held the turn.
    pub holds: usize,
    /// Sessions that had at least one Stop.
    pub sessions: usize,
    /// Session starts, the standing rules they showed, the rules that did not
    /// fit, and the bytes they carried.
    pub starts: usize,
    pub start_shown: usize,
    pub start_hidden: usize,
    pub start_bytes: usize,
}

/// The record read from the file, oldest first. A missing or unreadable file
/// reads as none.
fn read(db: &Path) -> Vec<Entry> {
    std::fs::read_to_string(path(db)).map(|t| t.lines().filter_map(parse).collect()).unwrap_or_default()
}

fn summarize(entries: &[Entry], from: i64) -> Summary {
    let mut summary = Summary::default();
    let mut shown: BTreeMap<&str, usize> = BTreeMap::new();
    let mut stop_sessions: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
    for entry in entries.iter().filter(|e| e.t >= from) {
        match &entry.kind {
            Kind::Show { ids, suppressed } => {
                summary.full += ids.len();
                summary.suppressed += suppressed;
                for id in ids {
                    *shown.entry(id.as_str()).or_default() += 1;
                }
            }
            Kind::Stop { held } => {
                stop_sessions.insert(entry.session.as_str());
                summary.holds += usize::from(*held);
            }
            Kind::Start { shown, hidden, bytes } => {
                summary.starts += 1;
                summary.start_shown += shown;
                summary.start_hidden += hidden;
                summary.start_bytes += bytes;
            }
        }
    }
    summary.sessions = stop_sessions.len();
    let mut top: Vec<(String, usize)> = shown.into_iter().map(|(id, n)| (id.to_string(), n)).collect();
    top.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    top.truncate(TOP);
    summary.top = top;
    summary
}

/// The one doctor line of a window.
fn line_of(label: &str, s: &Summary) -> String {
    let top = match s.top.is_empty() {
        true => "none".to_string(),
        false => s.top.iter().map(|(id, n)| format!("{id} ({n}x)")).collect::<Vec<_>>().join(", "),
    };
    let holds = match s.sessions {
        0 => "no Stop recorded".to_string(),
        n => format!("{} Stop hold(s) in {n} session(s), {:.1} per session on average", s.holds, s.holds as f64 / n as f64),
    };
    let starts = match s.starts {
        0 => "no session start recorded".to_string(),
        n => format!(
            "{n} session start(s) put {} standing rule(s) in {:.1} KB ({} did not fit and were left out)",
            s.start_shown,
            s.start_bytes as f64 / 1024.0,
            s.start_hidden
        ),
    };
    format!(
        "volume, last {label}: {} fact(s) shown in full at prompts and tool calls, {} repeat(s) left out; most shown: {top}; {starts}; {holds}",
        s.full, s.suppressed
    )
}

/// The doctor lines: one per window, or one line saying nothing is recorded
/// yet. Reads the record only.
pub fn lines(db: &Path, now: i64) -> Vec<String> {
    let entries = read(db);
    if entries.is_empty() {
        return vec![
            "volume: nothing recorded yet (THOR records how many facts were shown or left out, what each session start \
             carried and how many Stops held a turn, over the last 24 hours and 7 days)"
                .to_string(),
        ];
    }
    WINDOWS.iter().map(|(label, secs)| line_of(label, &summarize(&entries, now - secs))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(db: &Path, t: i64, line: serde_json::Value) {
        let mut line = line;
        line["t"] = serde_json::json!(t);
        append(db, line.to_string(), t);
    }

    fn show(s: &str, ids: &[&str], r: usize) -> serde_json::Value {
        serde_json::json!({ "k": "show", "s": s, "i": ids, "r": r })
    }

    fn stop(s: &str, held: u8) -> serde_json::Value {
        serde_json::json!({ "k": "stop", "s": s, "h": held })
    }

    #[test]
    fn the_windows_add_up_showings_repeats_the_top_facts_and_stop_holds_per_session() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        let now = 1_000_000_000;
        let day = 24 * 3600;
        at(&db, now - 3600, show("s1", &["a", "b"], 3));
        at(&db, now - 1800, show("s1", &["a"], 0));
        at(&db, now - 600, show("s2", &["c"], 1));
        at(&db, now - 500, stop("s1", 1));
        at(&db, now - 400, stop("s1", 0));
        at(&db, now - 300, stop("s2", 0));
        at(&db, now - 3 * day, show("s0", &["old", "old2"], 9));
        at(&db, now - 3 * day, stop("s0", 1));
        let all = lines(&db, now);
        assert_eq!(all.len(), 2);
        assert!(
            all[0].contains("last 24 hours: 4 fact(s) shown in full at prompts and tool calls, 4 repeat(s) left out; most shown: a (2x), b (1x), c (1x);")
                && all[0].contains("1 Stop hold(s) in 2 session(s), 0.5 per session on average"),
            "{}",
            all[0]
        );
        assert!(
            all[1].contains("last 7 days: 6 fact(s) shown in full at prompts and tool calls, 13 repeat(s) left out")
                && all[1].contains("2 Stop hold(s) in 3 session(s), 0.7 per session on average"),
            "{}",
            all[1]
        );
    }

    #[test]
    fn session_starts_are_counted_with_their_bytes_and_the_rules_that_did_not_fit() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        let now = 1_000_000_000;
        at(&db, now - 3600, serde_json::json!({ "k": "start", "s": "s1", "i": ["a", "b", "c"], "n": 40, "b": 9_000 }));
        at(&db, now - 1800, serde_json::json!({ "k": "start", "s": "s2", "i": ["a", "b"], "n": 41, "b": 6_000 }));
        let first = &lines(&db, now)[0];
        assert!(
            first.contains("2 session start(s) put 5 standing rule(s) in 14.6 KB (81 did not fit and were left out)"),
            "{first}"
        );
        // A session start is not a showing: the top list and the full count stay at prompts and tool calls.
        assert!(first.contains("0 fact(s) shown in full"), "{first}");
        assert!(first.contains("most shown: none"), "{first}");
    }

    #[test]
    fn a_window_without_a_session_start_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        at(&db, 1_000_000_000 - 60, show("s", &["a"], 0));
        assert!(lines(&db, 1_000_000_000)[0].contains("no session start recorded"));
    }

    #[test]
    fn only_the_five_most_shown_facts_are_named() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        let now = 1_000_000_000;
        for (i, id) in ["a", "b", "c", "d", "e", "f", "g"].iter().enumerate() {
            for _ in 0..(10 - i) {
                at(&db, now - 60, show("s", &[id], 0));
            }
        }
        let first = &lines(&db, now)[0];
        assert!(first.contains("most shown: a (10x), b (9x), c (8x), d (7x), e (6x);"), "{first}");
    }

    #[test]
    fn nothing_recorded_says_so_and_a_recording_run_leaves_no_file() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        assert_eq!(lines(&db, 100).len(), 1);
        assert!(lines(&db, 100)[0].starts_with("volume: nothing recorded yet"));
        record_showing(&db, "s", &[], 0);
        assert!(!dir.path().join(FILE).exists(), "a block that showed and hid nothing writes nothing");
        {
            let _held = crate::store_dir::hold(&db);
            record_showing(&db, "s", &["a"], 0);
            record_stop(&db, "s", true);
        }
        assert!(!dir.path().join(FILE).exists(), "a held directory takes no write");
    }

    #[test]
    fn an_old_line_goes_when_the_file_is_cut_and_a_new_one_stays() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        let now = 1_000_000_000;
        at(&db, now - 20 * 24 * 3600, show("s", &["old"], 0));
        at(&db, now - 60, show("s", &["new"], 0));
        prune(&path(&db), now);
        let text = std::fs::read_to_string(path(&db)).unwrap();
        assert!(!text.contains("old") && text.contains("new"), "{text}");
    }
}
