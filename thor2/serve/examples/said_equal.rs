//! Proof that the fast, bounded reader of `serve::said` gives the same list as
//! the old full read, byte for byte.
//!
//! For each project and each window start it computes the list with
//! `said::collect_in_full` (the old code: every file read whole) and with
//! `said::collect_in`, and also with `said::collect_resumable` under a tiny
//! deadline that is repeated until it finishes (every round goes through the
//! sidecar's JSON, like the real thing). It compares what `serve said` would
//! print (`said::render`), the number of messages, the unreadable count and the
//! plan (how many are shown and the cursor it would set). Any difference makes
//! it exit 1. Read-only: it never writes a transcript or a store.
//!
//! Usage: said_equal <transcripts-folder> <project> [<project> ...]
//!   --days 7,3,1   window starts, in days before now (default 7,3,1)
//!   --db <store copy>  the store whose project names are used to tell sessions apart
//!   --box-ms 40    the tiny deadline of the continuation run (default 40)

use serve::said;
use std::time::{Duration, Instant};

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let take = |args: &mut Vec<String>, name: &str, default: &str| -> String {
        match args.iter().position(|a| a == name) {
            Some(i) if i + 1 < args.len() => {
                let value = args.remove(i + 1);
                args.remove(i);
                value
            }
            _ => default.to_string(),
        }
    };
    let days: Vec<i64> = take(&mut args, "--days", "7,3,1").split(',').filter_map(|d| d.trim().parse().ok()).collect();
    let box_ms: u64 = take(&mut args, "--box-ms", "40").parse().unwrap_or(40);
    let db = take(&mut args, "--db", "");
    if !db.is_empty() {
        serve::project::load_known_scopes(std::path::Path::new(&db));
    }
    let Some(root) = args.first().cloned() else {
        eprintln!("usage: said_equal <transcripts-folder> <project> [<project> ...]");
        std::process::exit(2);
    };
    let root = std::path::PathBuf::from(root);
    let now_ms = serve::time::now_unix() * 1000;
    let mut differences = 0;
    for project in &args[1..] {
        for d in &days {
            let since = now_ms - d * 86_400_000;
            let (files, _) = said::files_of(&root, project, since);
            let bytes: u64 = files.iter().filter_map(|f| std::fs::metadata(f).ok()).map(|m| m.len()).sum();
            let t = Instant::now();
            let old = said::collect_in_full(&root, project, since, now_ms);
            let old_s = t.elapsed();
            let t = Instant::now();
            let new = said::collect_in(&root, project, since, now_ms);
            let new_s = t.elapsed();
            // The continuation run: tiny deadline, progress through JSON each round.
            let t = Instant::now();
            let mut progress = said::Progress::default();
            let mut rounds = 1;
            let boxed = loop {
                let deadline = Instant::now() + Duration::from_millis(box_ms);
                if let Some(done) = said::collect_resumable(&root, project, since, now_ms, Some(deadline), &mut progress) {
                    break done;
                }
                let text = serde_json::to_string(&progress).unwrap();
                progress = serde_json::from_str(&text).unwrap();
                rounds += 1;
                if rounds > 100_000 {
                    eprintln!("no progress after {rounds} rounds");
                    std::process::exit(1);
                }
            };
            let boxed_s = t.elapsed();
            let reference = said::render(project, &old.list);
            let same_new = said::render(project, &new.list) == reference && new == old && said::plan(&new.list) == said::plan(&old.list);
            let same_boxed = said::render(project, &boxed.list) == reference && boxed == old && said::plan(&boxed.list) == said::plan(&old.list);
            if !same_new || !same_boxed {
                differences += 1;
            }
            println!(
                "{project}: window {d}d: {} file(s) {} MB read by the old code, {} message(s), unreadable {}, printed {} | old {:.2}s new {:.2}s boxed({box_ms}ms) {:.2}s in {rounds} round(s) | new equal: {same_new}, boxed equal: {same_boxed}",
                files.len(),
                bytes / 1_000_000,
                old.list.len(),
                old.unreadable,
                said::plan(&old.list).0,
                old_s.as_secs_f64(),
                new_s.as_secs_f64(),
                boxed_s.as_secs_f64(),
            );
        }
    }
    println!("differences: {differences}");
    if differences > 0 {
        std::process::exit(1);
    }
}
