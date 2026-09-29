//! Read-only replay of the door (`serve::shell`): every live command rule
//! with a `forbidden` or `requires` check, run over real calls twice - once
//! with the matcher from before the door (a frozen private copy below), once
//! with the live guard (`serve::absent_guard`, the same functions the hook
//! runs) - and what changed, per rule.
//!
//! WHY THIS EXISTS. The door changes what live rules refuse: it reaches calls
//! the old matcher never saw (after a `cd`, behind `ssh`, in a loop), it stops
//! refusing a literal that sits in an unrelated command of the same line, and
//! it reads a literal only on token boundaries. Each of those is meant, and
//! each can also lose a refusal a rule really needed. This lists them apart,
//! so the owner reviews every loss before the new build is installed:
//!
//! 1. lost refusals caused by segmenting and the unit (the old matcher
//!    refused, the new one does not, even with substring matching);
//! 2. lost refusals caused ONLY by the token boundaries (the new matcher
//!    refuses the same call when its literals are matched as plain
//!    substrings);
//! 3. per rule, the calls it would refuse among the real calls it reaches,
//!    flagging every rule that refuses more than half of at least 5 (the
//!    preview of the "refuses most of what it reaches" check);
//! 4. lost reach: a command anchor the old matcher said reached a call and
//!    the new one does not, with the average number of command-anchored
//!    items reached per call before and after.
//!
//! THE CORPUS. `--transcripts <dir>`: every Bash and PowerShell call in the
//! Claude Code transcripts under that folder (`*.jsonl`, walked recursively),
//! each distinct (tool, command, working folder) once, with every value of
//! its input as the content a requires answer is looked for in. `--since
//! <YYYY-MM-DD>` keeps only calls made on or after that day (the hold-out).
//! Without `--transcripts`, the distinct triggers the store served on
//! (`item_served`), read in the sh dialect with no working folder; a trigger
//! cut at the serving limit is marked cut and never counts in the list of 3.
//!
//! Each call is judged under the project its working folder resolves to
//! (`serve::project::resolve_project`), each segment under its own
//! (`serve::guard_verdict::segment_projects`), exactly as the hook judges it.
//!
//! READ ONLY: the store is opened with `EventStore::open_read_only`, the
//! transcripts are only read, and everything goes to stdout. What it prints
//! holds real commands: redirect it to a private folder, never into the
//! repository.
//!
//! Run (from the `thor2` workspace root):
//!   cargo run --release --example door_replay -p serve -- --db <store copy> [--transcripts <dir>] [--since <day>] [--samples <n>] [--rule <id>]...
//!
//! `--rule <id>` (repeatable) replays only those items - for a quick look at
//! one rule; the lost-reach list then covers only them too.

use model::item::{Binding, Check, TargetKind};
use serde_json::Value;
use serve::live::LiveItem;
use serve::shell::{self, Segment};
use std::collections::{BTreeMap, HashMap};
use std::io::BufRead;
use std::path::{Path, PathBuf};

/// The matcher from before the door, copied verbatim so the replay compares
/// against what really ran: the anchor against the first words of the whole
/// line, the literal anywhere in it, an inner trigger as whole words.
mod old {
    pub fn strip_invocation_wrapper(command: &str) -> String {
        let mut words: Vec<&str> = command.split_whitespace().collect();
        while let Some(first) = words.first() {
            if *first == "sudo" || (first.contains('=') && !first.starts_with('-')) {
                words.remove(0);
            } else {
                break;
            }
        }
        if let Some(first) = words.first_mut() {
            let base = first.rsplit(['/', '\\']).next().unwrap_or(first);
            *first = base.strip_suffix(".exe").or_else(|| base.strip_suffix(".EXE")).unwrap_or(base);
        }
        words.join(" ")
    }

    pub fn command_anchor_names(anchor: &str, command: &str) -> bool {
        command_anchor_matches(anchor, command) || command_anchor_matches(anchor, &strip_invocation_wrapper(command))
    }

    pub fn command_anchor_matches(anchor: &str, command: &str) -> bool {
        let anchor_words: Vec<String> = anchor.split_whitespace().map(str::to_lowercase).collect();
        if anchor_words.is_empty() {
            return false;
        }
        let command_words: Vec<String> =
            command.split_whitespace().take(anchor_words.len()).map(str::to_lowercase).collect();
        command_words == anchor_words
    }

    pub fn contains_as_words(haystack: &str, needle: &str) -> bool {
        let needle = needle.trim();
        if needle.is_empty() {
            return false;
        }
        let joins = |c: char| c.is_alphanumeric() || "-_.".contains(c);
        haystack.match_indices(needle).any(|(at, _)| {
            let before = haystack[..at].chars().next_back();
            let after = haystack[at + needle.len()..].chars().next();
            let starts_clean = before.is_none_or(|c| !joins(c) || !needle.starts_with(joins));
            let ends_clean = after.is_none_or(|c| !joins(c) || !needle.ends_with(joins));
            starts_clean && ends_clean
        })
    }

    pub fn forbidden_refuses(anchors: &[String], literals: &[String], command: &str) -> bool {
        let normalised = strip_invocation_wrapper(command);
        if !anchors.iter().any(|a| command_anchor_matches(a, command) || command_anchor_matches(a, &normalised)) {
            return false;
        }
        literals.iter().any(|l| command.contains(l.as_str()))
    }

    pub fn requires_refuses(anchors: &[String], inner: bool, when: &str, required: &[String], command: &str, content: &str) -> bool {
        if !anchors.iter().any(|a| command_anchor_names(a, command)) {
            return false;
        }
        if inner && !contains_as_words(command, when) {
            return false;
        }
        !required.iter().any(|r| content.contains(r.as_str()))
    }
}

/// One real call.
struct Call {
    tool: String,
    command: String,
    cwd: Option<PathBuf>,
    content: String,
    cut: bool,
}

struct Args {
    db: PathBuf,
    transcripts: Option<PathBuf>,
    since: Option<String>,
    samples: usize,
    only: Vec<String>,
}

fn args() -> Args {
    let mut db = None;
    let mut transcripts = None;
    let mut since = None;
    let mut samples = 5;
    let mut only = Vec::new();
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--db" => db = it.next().map(PathBuf::from),
            "--transcripts" => transcripts = it.next().map(PathBuf::from),
            "--since" => since = it.next(),
            "--samples" => samples = it.next().and_then(|n| n.parse().ok()).unwrap_or(5),
            "--rule" => only.extend(it.next()),
            other => {
                eprintln!("unknown argument {other}: use --db <store> [--transcripts <dir>] [--since <YYYY-MM-DD>] [--samples <n>]");
                std::process::exit(2);
            }
        }
    }
    let Some(db) = db else {
        eprintln!("name the store to read: --db <path to a copy of thor.db>");
        std::process::exit(2);
    };
    Args { db, transcripts, since, samples, only }
}

fn values(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::String(s) => out.push(s.clone()),
        Value::Number(n) => out.push(n.to_string()),
        Value::Bool(b) => out.push(b.to_string()),
        Value::Array(a) => a.iter().for_each(|x| values(x, out)),
        Value::Object(m) => m.values().for_each(|x| values(x, out)),
        Value::Null => {}
    }
}

fn jsonl_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            jsonl_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "jsonl") {
            out.push(path);
        }
    }
}

fn transcript_calls(dir: &Path, since: Option<&str>) -> Vec<Call> {
    let mut files = Vec::new();
    jsonl_files(dir, &mut files);
    files.sort();
    let mut seen: HashMap<(String, String, String), ()> = HashMap::new();
    let mut calls = Vec::new();
    for file in files {
        let Ok(handle) = std::fs::File::open(&file) else { continue };
        for line in std::io::BufReader::new(handle).lines() {
            let Ok(line) = line else { continue };
            if !line.contains("\"tool_use\"") || !(line.contains("\"Bash\"") || line.contains("\"PowerShell\"")) {
                continue;
            }
            let Ok(entry) = serde_json::from_str::<Value>(&line) else { continue };
            if let Some(since) = since {
                let day = entry.get("timestamp").and_then(Value::as_str).unwrap_or("");
                if day < since {
                    continue;
                }
            }
            let cwd = entry.get("cwd").and_then(Value::as_str).unwrap_or("").to_string();
            let Some(content) = entry.pointer("/message/content").and_then(Value::as_array) else { continue };
            for block in content {
                if block.get("type").and_then(Value::as_str) != Some("tool_use") {
                    continue;
                }
                let tool = block.get("name").and_then(Value::as_str).unwrap_or("");
                if tool != "Bash" && tool != "PowerShell" {
                    continue;
                }
                let Some(input) = block.get("input") else { continue };
                let Some(command) = input.get("command").and_then(Value::as_str).filter(|c| !c.is_empty()) else {
                    continue;
                };
                let key = (tool.to_string(), command.to_string(), cwd.clone());
                if seen.insert(key, ()).is_some() {
                    continue;
                }
                let mut vals = Vec::new();
                values(input, &mut vals);
                calls.push(Call {
                    tool: tool.to_string(),
                    command: command.to_string(),
                    cwd: (!cwd.is_empty()).then(|| PathBuf::from(&cwd)),
                    content: vals.join("\n"),
                    cut: false,
                });
            }
        }
    }
    calls
}

fn served_calls(store: &thor_core::event_store::EventStore) -> Vec<Call> {
    let mut seen = HashMap::new();
    let mut calls = Vec::new();
    for event in store.get_all_events().unwrap_or_default() {
        if event.kind != thor_core::event_store::EventKind::ItemServed {
            continue;
        }
        let Ok(served) = serde_json::from_str::<model::served::ItemServed>(&event.body) else { continue };
        let Some(trigger) = served.trigger.filter(|t| !t.trim().is_empty()) else { continue };
        if seen.insert(trigger.clone(), ()).is_some() {
            continue;
        }
        let cut = trigger.chars().count() >= serve::deliver::TRIGGER_LIMIT;
        calls.push(Call { tool: "Bash".to_string(), content: trigger.clone(), command: trigger, cwd: None, cut });
    }
    calls
}

fn command_anchors(item: &LiveItem) -> Vec<String> {
    item.item
        .bindings
        .iter()
        .filter_map(|b| match b {
            Binding::Target { kind: TargetKind::Command, value } => Some(value.clone()),
            _ => None,
        })
        .collect()
}

/// The new matcher with its literals and triggers read as plain substrings -
/// the same reach and unit, only without the token boundaries. Replay-only:
/// it tells a loss caused by the boundaries from one caused by segmenting.
fn substring_forbidden(anchors: &[String], literals: &[String], segs: &[Segment], applies: &[bool]) -> bool {
    segs.iter().zip(applies).any(|(seg, ok)| {
        *ok && anchors.iter().any(|a| shell::anchor_reaches(a, seg))
            && literals.iter().any(|l| {
                let short = l.split_whitespace().collect::<Vec<_>>().join(" ");
                seg.unit.contains(l.as_str())
                    || (!short.is_empty() && (seg.unit_words.contains(&short) || seg.head_text().contains(&short)))
            })
    })
}

fn substring_requires(
    anchors: &[String],
    inner: bool,
    when: &str,
    required: &[String],
    segs: &[Segment],
    applies: &[bool],
    content: &str,
) -> bool {
    let stem = when.trim().strip_suffix('*').unwrap_or(when.trim());
    let hit = segs.iter().zip(applies).any(|(seg, ok)| {
        *ok && anchors.iter().any(|a| shell::anchor_reaches(a, seg) && !shell::is_pure_help_call(a, seg))
            && (!inner || seg.unit.contains(stem) || seg.unit_words.contains(stem))
    });
    hit && !required.iter().any(|r| content.contains(r.as_str()))
}

fn one_line(text: &str) -> String {
    text.replace('\n', "\\n").chars().take(300).collect()
}

#[derive(Default)]
struct Tally {
    old: usize,
    new: usize,
    gained: Vec<String>,
    lost_segmenting: Vec<String>,
    lost_boundaries: Vec<String>,
    lost_help: Vec<String>,
    reached_uncut: usize,
    refused_uncut: usize,
    refused_samples: Vec<String>,
}

fn main() {
    let args = args();
    let store = thor_core::event_store::EventStore::open_read_only(&args.db).unwrap_or_else(|e| {
        eprintln!("cannot open {} read-only: {e}", args.db.display());
        std::process::exit(1);
    });
    let live = serve::live::live_items(&store);
    let anchored: Vec<&LiveItem> = live
        .iter()
        .filter(|li| li.item.kind.can_fire() && !command_anchors(li).is_empty())
        .filter(|li| args.only.is_empty() || args.only.contains(&li.id))
        .collect();
    let rules: Vec<&LiveItem> = anchored
        .iter()
        .copied()
        .filter(|li| matches!(li.item.check, Some(Check::Forbidden { .. }) | Some(Check::Requires { .. })))
        .collect();
    let calls = match &args.transcripts {
        Some(dir) => transcript_calls(dir, args.since.as_deref()),
        None => served_calls(&store),
    };
    let root = Path::new(".");
    let mut project_cache: HashMap<PathBuf, Option<String>> = HashMap::new();
    let live = serve::guard_verdict::Live::of(&store);
    let mut tallies: BTreeMap<String, Tally> = BTreeMap::new();
    let mut lost_reach: BTreeMap<(String, String), usize> = BTreeMap::new();
    let (mut reach_old_total, mut reach_new_total) = (0usize, 0usize);
    let started = std::time::Instant::now();
    let mut worst = (std::time::Duration::ZERO, 0usize);
    let mut segmenting = std::time::Duration::ZERO;

    for call in &calls {
        let session_project = match &call.cwd {
            Some(cwd) => project_cache.entry(cwd.clone()).or_insert_with(|| serve::project::resolve_project(cwd)).clone(),
            None => None,
        };
        let t = std::time::Instant::now();
        let segs = shell::segments(&call.command, shell::dialect_for(&call.tool));
        let spent = t.elapsed();
        segmenting += spent;
        if spent > worst.0 {
            worst = (spent, call.command.len());
        }
        let projects = serve::guard_verdict::segment_projects(&live, &segs, call.cwd.as_deref(), session_project.as_deref());

        for item in &anchored {
            let anchors = command_anchors(item);
            let was = anchors.iter().any(|a| old::command_anchor_names(a, &call.command));
            let now = anchors.iter().any(|a| shell::reaches_any(a, &segs));
            reach_old_total += was as usize;
            reach_new_total += now as usize;
            if was && !now {
                *lost_reach.entry((item.id.clone(), one_line(&call.command))).or_default() += 1;
            }
        }

        for item in &rules {
            let anchors = command_anchors(item);
            let one = [LiveItem { id: item.id.clone(), item: item.item.clone() }];
            let session_applies = serve::project::applies_to(item.item.project.as_deref(), session_project.as_deref());
            let applies: Vec<bool> =
                projects.iter().map(|p| serve::project::applies_to(item.item.project.as_deref(), p.as_deref())).collect();
            let (old, new, substring) = match &item.item.check {
                Some(Check::Forbidden { literals }) => (
                    session_applies && old::forbidden_refuses(&anchors, literals, &call.command),
                    serve::absent_guard::find_command_violation_in(&one, &segs, Some(&projects), Some(root)).is_some(),
                    substring_forbidden(&anchors, literals, &segs, &applies),
                ),
                Some(Check::Requires { when, required }) => {
                    let inner = !model::gate::requires_trigger_names_a_binding(when, &item.item.bindings);
                    (
                        session_applies && old::requires_refuses(&anchors, inner, when, required, &call.command, &call.content),
                        serve::absent_guard::find_requires_violation_in(&one, &segs, Some(&projects), &call.content).is_some(),
                        substring_requires(&anchors, inner, when, required, &segs, &applies, &call.content),
                    )
                }
                _ => continue,
            };
            let tally = tallies.entry(item.id.clone()).or_default();
            let line = format!(
                "[{} @ {}] {}",
                call.tool,
                session_project.as_deref().unwrap_or("-"),
                one_line(&call.command)
            );
            tally.old += old as usize;
            tally.new += new as usize;
            if new && !old {
                tally.gained.push(line.clone());
            }
            let help_exempt = matches!(item.item.check, Some(Check::Requires { .. }))
                && segs.iter().zip(&applies).any(|(seg, ok)| *ok && anchors.iter().any(|a| shell::is_pure_help_call(a, seg)));
            if old && !new {
                if help_exempt {
                    tally.lost_help.push(line.clone());
                } else if substring {
                    tally.lost_boundaries.push(line.clone());
                } else {
                    tally.lost_segmenting.push(line.clone());
                }
            }
            let reached = segs
                .iter()
                .zip(&applies)
                .any(|(seg, ok)| *ok && anchors.iter().any(|a| shell::anchor_reaches(a, seg)));
            if reached && !call.cut {
                tally.reached_uncut += 1;
                if new {
                    tally.refused_uncut += 1;
                    tally.refused_samples.push(line);
                }
            }
        }
    }
    let elapsed = started.elapsed();

    let (mut old_sum, mut new_sum, mut gained_sum, mut seg_sum, mut bound_sum, mut help_sum) = (0, 0, 0, 0, 0, 0);
    let mut majority = Vec::new();
    for (id, t) in &tallies {
        old_sum += t.old;
        new_sum += t.new;
        gained_sum += t.gained.len();
        seg_sum += t.lost_segmenting.len();
        bound_sum += t.lost_boundaries.len();
        help_sum += t.lost_help.len();
        if t.reached_uncut >= 5 && t.refused_uncut * 2 > t.reached_uncut {
            majority.push(id.clone());
        }
    }
    println!(
        "corpus={} rules={} command_anchored_items={} old_refusals={} new_refusals={} gained={} lost_by_segmenting={} lost_by_help_exemption={} lost_by_boundaries={} lost_reach_pairs={}",
        calls.len(),
        rules.len(),
        anchored.len(),
        old_sum,
        new_sum,
        gained_sum,
        seg_sum,
        help_sum,
        bound_sum,
        lost_reach.len()
    );
    if !calls.is_empty() {
        println!(
            "serving: command-anchored items reached per call {:.3} -> {:.3}; segmenting cost {:?} per call on average, worst {:?} ({} bytes); whole replay {:?}",
            reach_old_total as f64 / calls.len() as f64,
            reach_new_total as f64 / calls.len() as f64,
            segmenting / calls.len() as u32,
            worst.0,
            worst.1,
            elapsed
        );
    }

    println!("\n== 1. lost refusals caused by segmenting and the unit");
    for (id, t) in &tallies {
        for line in &t.lost_segmenting {
            println!("  {id} :: {line}");
        }
    }
    println!("\n== 1b. lost refusals of a pure help or version call (a requires check never refuses one)");
    for (id, t) in &tallies {
        for line in &t.lost_help {
            println!("  {id} :: {line}");
        }
    }
    println!("\n== 2. lost refusals caused only by the token boundaries");
    for (id, t) in &tallies {
        for line in &t.lost_boundaries {
            println!("  {id} :: {line}");
        }
    }
    println!("\n== 3. rules refusing more than half of at least 5 uncut real calls they reach");
    for id in &majority {
        let t = &tallies[id];
        println!("  {id}: refuses {} of {} reached", t.refused_uncut, t.reached_uncut);
        for line in t.refused_samples.iter().take(args.samples) {
            println!("    e.g. {line}");
        }
    }
    println!("\n== 4. lost reach (the old matcher reached, the new one does not)");
    for ((id, call), n) in &lost_reach {
        println!("  {n}x {id} :: {call}");
    }
    println!("\n== per rule (old -> new, gained, lost)");
    let mut by_jump: Vec<(&String, &Tally)> = tallies.iter().collect();
    by_jump.sort_by(|a, b| (b.1.new as i64 - b.1.old as i64).cmp(&(a.1.new as i64 - a.1.old as i64)).then(a.0.cmp(b.0)));
    for (id, t) in by_jump {
        println!(
            "=== {id}: old={} new={} gained={} lost={} (segmenting {}, help {}, boundaries {}) reached_uncut={} refused_uncut={}",
            t.old,
            t.new,
            t.gained.len(),
            t.lost_segmenting.len() + t.lost_help.len() + t.lost_boundaries.len(),
            t.lost_segmenting.len(),
            t.lost_help.len(),
            t.lost_boundaries.len(),
            t.reached_uncut,
            t.refused_uncut
        );
        for line in t.gained.iter().take(args.samples) {
            println!("  + {line}");
        }
    }
}
