//! THE ROUTE PROBE (item 71): a rule bound to one route of a capability is
//! silent on another. `fusion-geen-nieuwe-bodies-maken` was bound to an MCP
//! tool and to the host `127.0.0.1:27182`; a PowerShell script that talked to
//! the same host never reached it, because the host sat inside the script
//! file and the command line named no host.
//!
//! WHAT THIS DOES. For every live rule bound to a host (and every rule bound
//! to a tool by name, with the hosts its own text names), it looks through the
//! 30-day record of real calls (`corpus`) for calls that MENTION that host -
//! in the command, in the content of a Write or Edit, or in a script file the
//! call runs (when that file's content is in the record, which is cheap) - and
//! that the rule's bindings never reached. It names such a rule with an
//! example call and the `revise` that would add the missing binding.
//!
//! WHAT IT NEVER DOES. It only REPORTS. It blocks nothing, writes nothing and
//! widens no binding: the owner or the evaluating agent adds the real binding
//! knowingly. It runs at doctor and `serve sweep` time on data THOR already
//! keeps, so the hot path pays nothing.

use crate::corpus::{self, CallRow};
use crate::input::{self, ServeInput};
use model::item::{Binding, Item, TargetKind};
use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::time::{Duration, Instant};
use thor_core::event_store::EventStore;

/// How many rules a report names before it says how many more there are.
pub const MAX_RULES_LISTED: usize = 8;

/// One rule with a route it does not reach.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteGap {
    pub rule: String,
    pub host: String,
    /// How many real calls mention the host and were never reached.
    pub calls: usize,
    /// The newest such call, in a few words.
    pub example: String,
    /// The `revise` that adds the missing binding. A suggestion for a person
    /// to read; nothing applies it.
    pub revise: String,
}

/// The hosts a rule stands for: its Host bindings, plus - for a rule bound to a
/// tool by name - the hosts its own text names.
fn hosts_of(item: &Item) -> Vec<String> {
    let mut hosts: Vec<String> = item
        .bindings
        .iter()
        .filter_map(|b| match b {
            Binding::Target { kind: TargetKind::Host, value } => Some(value.trim().to_string()),
            _ => None,
        })
        .filter(|h| !h.is_empty())
        .collect();
    let names_a_tool = item.bindings.iter().any(|b| match b {
        Binding::Target { kind: TargetKind::Command, value } => model::shell::names_a_tool(value),
        _ => false,
    });
    if names_a_tool {
        for host in input::hosts_in_command(&item.text) {
            if !hosts.contains(&host) {
                hosts.push(host);
            }
        }
    }
    hosts
}

fn is_shell(tool: &str) -> bool {
    matches!(tool, "Bash" | "PowerShell")
}

fn is_writer(tool: &str) -> bool {
    matches!(tool, "Write" | "Edit" | "MultiEdit")
}

fn text_of(value: &serde_json::Value, out: &mut String, depth: usize) {
    if depth > 6 {
        return;
    }
    match value {
        serde_json::Value::String(s) => {
            out.push_str(s);
            out.push('\n');
        }
        serde_json::Value::Array(a) => a.iter().for_each(|v| text_of(v, out, depth + 1)),
        serde_json::Value::Object(o) => o.values().for_each(|v| text_of(v, out, depth + 1)),
        _ => {}
    }
}

/// Everything a call carries, as one lowercase text: the command, the content
/// of a write, or the flattened input values of any other tool.
fn call_text(row: &CallRow) -> String {
    let mut text = String::new();
    text_of(&row.input, &mut text, 0);
    text.to_lowercase()
}

fn file_name_of(path: &str) -> String {
    path.replace('\\', "/").rsplit('/').next().unwrap_or("").to_lowercase()
}

/// A path as a key: forward slashes, lowercase, no leading `./`.
fn norm_path(path: &str) -> String {
    let flat = path.trim().replace('\\', "/").to_lowercase();
    flat.strip_prefix("./").unwrap_or(&flat).to_string()
}

fn is_absolute(path: &str) -> bool {
    path.starts_with('/') || path.starts_with('~') || path.as_bytes().get(1) == Some(&b':')
}

/// Programs that run the script file they are given.
const INTERPRETERS: &[&str] = &[
    "node", "nodejs", "python", "python3", "py", "bash", "sh", "zsh", "dash", "ksh", "ruby", "perl", "php", "deno", "bun", "tsx", "ts-node",
    "cscript", "wscript", "pwsh", "powershell", "cmd",
];

/// Words in front of the program that change nothing about what runs.
const PASS_THROUGH: &[&str] = &["sudo", "env", "nohup", "exec", "command", "time", "nice"];

const SCRIPT_EXTENSIONS: &[&str] = &[".ps1", ".sh", ".bash", ".py", ".js", ".mjs", ".cjs", ".ts", ".bat", ".cmd", ".rb", ".pl", ".vbs", ".php"];

/// PowerShell options that take a value, which is not the script.
const POWERSHELL_VALUE_FLAGS: &[&str] = &["-executionpolicy", "-ep", "-windowstyle", "-w", "-workingdirectory", "-wd", "-configurationname", "-version"];

/// The script file an interpreter is told to run, if its arguments name one.
fn script_argument(program: &str, args: &[String]) -> Option<String> {
    match program {
        "pwsh" | "powershell" => {
            let mut i = 0;
            while i < args.len() {
                let arg = args[i].to_ascii_lowercase();
                if arg == "-file" || arg == "-f" {
                    return args.get(i + 1).cloned();
                }
                if matches!(arg.as_str(), "-command" | "-c" | "-encodedcommand" | "-e") {
                    return None;
                }
                if !arg.starts_with('-') {
                    return Some(args[i].clone());
                }
                i += if POWERSHELL_VALUE_FLAGS.contains(&arg.as_str()) { 2 } else { 1 };
            }
            None
        }
        "cmd" => {
            let at = args.iter().position(|a| matches!(a.to_ascii_lowercase().as_str(), "/c" | "/k"))?;
            args.get(at + 1)?.split_whitespace().next().map(str::to_string)
        }
        _ => {
            for arg in args {
                if matches!(arg.as_str(), "-c" | "-e" | "-m" | "-p" | "--eval" | "--print") {
                    return None;
                }
                if !arg.starts_with('-') {
                    return Some(arg.clone());
                }
            }
            None
        }
    }
}

/// The script files a shell call really executes: an interpreter followed by
/// the file, the file itself as the command, or a `&` / `.` / `source` call.
/// A file that is only edited, read, copied or moved is not in the list.
fn executed_scripts(segments: &[model::shell::Segment]) -> Vec<String> {
    let mut out = Vec::new();
    for seg in segments.iter().filter(|s| !s.remote && !s.tool) {
        let words = &seg.words;
        let mut i = 0;
        while i < words.len() {
            let word = words[i].to_ascii_lowercase();
            let assignment = word
                .split_once('=')
                .is_some_and(|(name, _)| !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'));
            if !assignment && !PASS_THROUGH.contains(&word.as_str()) {
                break;
            }
            i += 1;
        }
        let Some(first) = words.get(i) else { continue };
        let rest = &words[i + 1..];
        if matches!(first.as_str(), "&" | "." | "source") {
            out.extend(rest.first().cloned());
            continue;
        }
        let program = file_name_of(first);
        let program = program.strip_suffix(".exe").unwrap_or(&program);
        if INTERPRETERS.contains(&program) {
            out.extend(script_argument(program, rest));
        } else {
            let flat = first.replace('\\', "/").to_lowercase();
            if flat.contains('/') || SCRIPT_EXTENSIONS.iter().any(|e| flat.ends_with(e)) {
                out.push(first.clone());
            }
        }
    }
    out
}

/// What Write and Edit calls put on record, by full normalised path.
#[derive(Default)]
struct Written {
    /// Normalised path -> (the path as written, with forward slashes; the
    /// lowercase content written to it; the needles that content holds).
    files: HashMap<String, WrittenFile>,
    /// Lowercase file name -> normalised paths with that name.
    by_name: HashMap<String, Vec<String>>,
}

struct WrittenFile {
    path: String,
    content: String,
    hits: Vec<usize>,
}

impl Written {
    fn add(&mut self, path: &str, content: &str) {
        let key = norm_path(path);
        if !self.files.contains_key(&key) {
            self.by_name.entry(file_name_of(path)).or_default().push(key.clone());
        }
        let file = self.files.entry(key).or_insert_with(|| WrittenFile { path: path.replace('\\', "/"), content: String::new(), hits: Vec::new() });
        file.content.push_str(content);
    }

    /// The files a script argument can mean: the same full path, or - for a
    /// relative path - a file whose path ends with it. A bare file name never
    /// matches a file in a folder the path does not name.
    fn find(&self, script: &str) -> Vec<&WrittenFile> {
        let key = norm_path(script);
        if let Some(file) = self.files.get(&key) {
            return vec![file];
        }
        if is_absolute(&key) {
            return Vec::new();
        }
        let suffix = format!("/{key}");
        let Some(keys) = self.by_name.get(&file_name_of(&key)) else { return Vec::new() };
        keys.iter().filter(|k| k.ends_with(&suffix)).filter_map(|k| self.files.get(k)).collect()
    }
}

/// The input the hook builds for this call, to ask whether a rule's bindings
/// reach it.
fn input_of(row: &CallRow) -> Option<ServeInput> {
    let mut input = ServeInput::default();
    if is_shell(&row.tool) {
        input.add_command(row.input.get("command")?.as_str()?);
    } else if is_writer(&row.tool) {
        input.add_file(row.input.get("file_path")?.as_str()?);
    } else {
        input.add_tool_name(&row.tool);
    }
    Some(input)
}

/// [`input_of`] without the moments, which cost far more than everything else
/// the input holds: what a rule that is bound to no moment is tried against.
fn targets_of(row: &CallRow) -> Option<ServeInput> {
    let mut input = ServeInput::default();
    if is_writer(&row.tool) {
        input.targets.push((TargetKind::Path, row.input.get("file_path")?.as_str()?.to_string()));
        return Some(input);
    }
    let text = if is_shell(&row.tool) { row.input.get("command")?.as_str()? } else { row.tool.as_str() };
    if text.is_empty() {
        return Some(input);
    }
    input.targets.push((TargetKind::Command, text.to_string()));
    input.targets.extend(input::paths_in_command(text).into_iter().map(|p| (TargetKind::Path, p)));
    input.targets.extend(input::hosts_in_command(text).into_iter().map(|h| (TargetKind::Host, h)));
    Some(input)
}

/// The call read as a shell command, parsed the first time it is asked for.
fn segments_of<'a>(slot: &'a mut Option<Vec<model::shell::Segment>>, tool: &str, command: Option<&str>) -> &'a [model::shell::Segment] {
    slot.get_or_insert_with(|| command.map(|c| model::shell::call_segments(tool, c)).unwrap_or_default())
}

/// Whether any binding of `item` reaches the call `input` stands for (the same
/// primitives the hook and `pool_rivals` use). `segments` is the call read as
/// a shell command (empty for any other call).
fn reached(item: &Item, input: &ServeInput, segments: &[model::shell::Segment]) -> bool {
    item.bindings.iter().any(|binding| match binding {
        Binding::Moment(action) => input.moments.contains(action),
        Binding::Always => false,
        Binding::Target { kind, value } => {
            input.targets.iter().any(|(k, v)| model::normalize::target_matches(*kind, value, *k, v))
                || (*kind == TargetKind::Command && model::shell::reaches_any(value, segments))
        }
    })
}

/// How a call is shown as an example.
fn example_of(row: &CallRow) -> String {
    let short = |text: &str| -> String {
        let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
        if flat.chars().count() > 80 {
            format!("{}...", flat.chars().take(80).collect::<String>())
        } else {
            flat
        }
    };
    if is_writer(&row.tool) {
        let path = row.input.get("file_path").and_then(|p| p.as_str()).unwrap_or("a file");
        return format!("{} {}", row.tool, path.replace('\\', "/"));
    }
    if is_shell(&row.tool) {
        return format!("{}: {}", row.tool, short(row.input.get("command").and_then(|c| c.as_str()).unwrap_or("")));
    }
    format!("{} call", row.tool)
}

/// The binding that would reach the route this call took.
fn missing_binding(row: &CallRow, via_script: Option<&str>) -> (TargetKind, String) {
    if is_writer(&row.tool) {
        let path = row.input.get("file_path").and_then(|p| p.as_str()).unwrap_or("");
        return (TargetKind::Path, path.replace('\\', "/"));
    }
    if is_shell(&row.tool) {
        if let Some(script) = via_script {
            return (TargetKind::Path, script.to_string());
        }
        let command = row.input.get("command").and_then(|c| c.as_str()).unwrap_or("");
        let head: Vec<&str> = command.split_whitespace().take(2).collect();
        return (TargetKind::Command, head.join(" "));
    }
    (TargetKind::Command, row.tool.clone())
}

fn revise_for(item: &Item, id: &str, extra: &(TargetKind, String)) -> String {
    let mut bindings = item.bindings.clone();
    bindings.push(Binding::Target { kind: extra.0, value: extra.1.clone() });
    format!(
        "mcp__thor__revise(id: \"{id}\", {}, because: \"<this route reaches the same host, at least 20 characters>\")",
        model::store::binding_arguments(&bindings)
    )
}

/// The rules of `store` with a route in `calls` that their bindings never
/// reach, most calls first. Pure: reads the store and the calls, changes
/// neither.
pub fn probe(store: &EventStore, calls: &[CallRow]) -> Vec<RouteGap> {
    probe_within(store, calls, None).0
}

/// What the probe found for one rule and host: how many calls, and the newest
/// one (its place in the calls and the script it ran).
struct Found<'a> {
    calls: usize,
    t: i64,
    at: usize,
    via_script: Option<&'a str>,
}

/// The gaps, most calls first, from what was found per (rule, host) place.
/// The example and the `revise` text are built here, once per gap, not for
/// every call.
fn gaps_of(rules: &Rules, calls: &[CallRow], found: BTreeMap<(usize, usize), Found>) -> Vec<RouteGap> {
    let mut out: Vec<RouteGap> = found
        .into_iter()
        .map(|((r, h), f)| {
            let (id, item, hosts) = &rules[r];
            let row = &calls[f.at];
            RouteGap {
                rule: id.clone(),
                host: hosts[h].clone(),
                calls: f.calls,
                example: match f.via_script {
                    Some(script) => format!("{} (runs {script}, which names the host)", example_of(row)),
                    None => example_of(row),
                },
                revise: revise_for(item, id, &missing_binding(row, f.via_script)),
            }
        })
        .collect();
    out.sort_by(|a, b| b.calls.cmp(&a.calls).then_with(|| a.rule.cmp(&b.rule)).then_with(|| a.host.cmp(&b.host)));
    out
}

type Rules = Vec<(String, Item, Vec<String>)>;

/// The live rules that stand for at least one host, with those hosts.
fn rules_of(store: &EventStore) -> Rules {
    crate::live::live_items(store)
        .into_iter()
        .filter(|li| li.item.kind.can_fire())
        .map(|li| {
            let hosts = hosts_of(&li.item);
            (li.id, li.item, hosts)
        })
        .filter(|(_, _, hosts)| !hosts.is_empty())
        .collect()
}

/// Every host once, lowercase: what a call or a written file is searched for.
fn needles_of(rules: &Rules) -> Vec<String> {
    let mut needles: Vec<String> = Vec::new();
    for (_, _, hosts) in rules {
        for host in hosts {
            let needle = host.to_lowercase();
            if !needles.contains(&needle) {
                needles.push(needle);
            }
        }
    }
    needles
}

/// [`probe`] that stops at `deadline`. The second answer is how many of the
/// calls it looked at when it stopped early, `None` when it saw them all.
pub fn probe_within(store: &EventStore, calls: &[CallRow], deadline: Option<Instant>) -> (Vec<RouteGap>, Option<usize>) {
    let rules = rules_of(store);
    if rules.is_empty() {
        return (Vec::new(), None);
    }
    let out_of_time = |seen: usize| seen % 16 == 0 && deadline.is_some_and(|d| Instant::now() >= d);
    let needles = needles_of(&rules);
    // The content written by Write and Edit calls, by full path: a script a
    // later call runs is read from here, never from the disk.
    let mut written = Written::default();
    for (seen, row) in calls.iter().enumerate() {
        if out_of_time(seen) {
            return (Vec::new(), Some(0));
        }
        if !is_writer(&row.tool) {
            continue;
        }
        if let Some(path) = row.input.get("file_path").and_then(|p| p.as_str()) {
            written.add(path, &call_text(row));
        }
    }
    // Which hosts each written file names, once; only the names of the files
    // that name any are worth looking for in a command.
    let mut hot_names: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (key, file) in written.files.iter_mut() {
        file.hits = (0..needles.len()).filter(|n| file.content.contains(&needles[*n])).collect();
        file.content = String::new();
        if !file.hits.is_empty() {
            hot_names.insert(file_name_of(key));
        }
    }
    // Per rule and host: how many calls, and the newest one. The example and
    // the `revise` text are built once at the end, not for every call.
    let mut found: BTreeMap<(usize, usize), Found> = BTreeMap::new();
    for (seen, row) in calls.iter().enumerate() {
        if out_of_time(seen) {
            return (gaps_of(&rules, &calls, found), Some(seen));
        }
        let text = call_text(row);
        let named: Vec<bool> = needles.iter().map(|n| text.contains(n)).collect();
        // The shell call read as a command, once and only when needed.
        let command = row.input.get("command").and_then(|c| c.as_str());
        let mut segments: Option<Vec<model::shell::Segment>> = None;
        // The written files this shell call really executes that name a host.
        let mut scripts: Vec<&WrittenFile> = Vec::new();
        if is_shell(&row.tool) && !hot_names.is_empty() {
            let mentions_one = input::paths_in_command(command.unwrap_or("")).iter().any(|p| hot_names.contains(&file_name_of(p)));
            if mentions_one {
                for script in executed_scripts(segments_of(&mut segments, &row.tool, command)) {
                    scripts.extend(written.find(&script).into_iter().filter(|f| !f.hits.is_empty()));
                }
            }
        }
        // What the hook would see: with its moments (costly to work out) only
        // for a rule bound to a moment, else the targets alone.
        let mut targets_input: Option<ServeInput> = None;
        let mut full_input: Option<ServeInput> = None;
        for (r, (_, item, hosts)) in rules.iter().enumerate() {
            for (h, host) in hosts.iter().enumerate() {
                let needle = host.to_lowercase();
                let Some(n) = needles.iter().position(|x| *x == needle) else { continue };
                let via_script = scripts.iter().find(|f| f.hits.contains(&n)).map(|f| f.path.as_str());
                if !named[n] && via_script.is_none() {
                    continue;
                }
                let needs_moments = item.bindings.iter().any(|b| matches!(b, Binding::Moment(_)));
                let slot = if needs_moments { &mut full_input } else { &mut targets_input };
                if slot.is_none() {
                    *slot = if needs_moments { input_of(row) } else { targets_of(row) };
                }
                let Some(call_input) = slot.as_ref() else { continue };
                if reached(item, call_input, segments_of(&mut segments, &row.tool, command)) {
                    continue;
                }
                let entry = found.entry((r, h)).or_insert(Found { calls: 0, t: i64::MIN, at: seen, via_script: None });
                entry.calls += 1;
                if row.t >= entry.t {
                    entry.t = row.t;
                    entry.at = seen;
                    entry.via_script = via_script;
                }
            }
        }
    }
    (gaps_of(&rules, &calls, found), None)
}

/// The lines doctor and `serve sweep` show: silent when no rule has a route it
/// does not reach.
pub fn lines(gaps: &[RouteGap]) -> Vec<String> {
    lines_listing(gaps, MAX_RULES_LISTED)
}

/// [`lines`] naming at most `listed` rules (`serve sweep` names them all).
pub fn lines_listing(gaps: &[RouteGap], listed: usize) -> Vec<String> {
    let mut out = Vec::new();
    for gap in gaps.iter().take(listed) {
        out.push(format!(
            "route probe: rule '{}' is bound to {}, and {} real call(s) of the last 30 days mention it without reaching the rule (example: {}). If that route is meant to be covered: {}",
            gap.rule, gap.host, gap.calls, gap.example, gap.revise
        ));
    }
    if gaps.len() > listed {
        out.push(format!("route probe: and {} more rule(s), not listed here (run serve sweep)", gaps.len() - listed));
    }
    out
}

/// A case-insensitive search for any of `words`, which are searched as plain
/// text. Reads a line without copying it, which keeps a pass over the whole
/// record cheap.
fn any_of(words: &[String]) -> regex::Regex {
    let escaped: Vec<String> = words.iter().map(|w| regex::escape(w)).collect();
    regex::Regex::new(&format!("(?i)(?:{})", escaped.join("|"))).expect("escaped words make a valid pattern")
}

/// How many calls the probe keeps in memory at most (the newest).
const CALLS_KEPT: usize = 200_000;

/// The probe over the record beside `db`, within `budget` for the whole of it
/// (reading the record and probing): what doctor and `serve sweep` call. When
/// the time ran out first, or more calls matched than are kept, a last line
/// says so.
///
/// Only a call that names a host, or that runs a file which does, can be a
/// route, so only those lines of the record are parsed: one cheap pass for
/// the calls that name a host, and a second for the calls that name a file
/// the first pass saw written with a host in it.
pub fn lines_for(db: &Path, store: &EventStore, now: i64, budget: Duration, in_sweep: bool) -> Vec<String> {
    let deadline = Instant::now() + budget;
    let needles = needles_of(&rules_of(store));
    if needles.is_empty() {
        return Vec::new();
    }
    let names_a_host = any_of(&needles);
    let (mut calls, _, mut cut) = corpus::select_calls(db, now, CALLS_KEPT, &|line| names_a_host.is_match(line));
    let mut hot_names: Vec<String> = Vec::new();
    for row in calls.iter().filter(|r| is_writer(&r.tool)) {
        let name = row.input.get("file_path").and_then(|p| p.as_str()).map(file_name_of).unwrap_or_default();
        if !name.is_empty() && !hot_names.contains(&name) {
            hot_names.push(name);
        }
    }
    if !hot_names.is_empty() {
        let names_a_file = any_of(&hot_names);
        let (more, _, more_cut) = corpus::select_calls(db, now, CALLS_KEPT, &|line| names_a_file.is_match(line));
        cut |= more_cut;
        let known: std::collections::HashSet<String> = calls.iter().map(|r| r.id.clone()).collect();
        calls.extend(more.into_iter().filter(|r| is_shell(&r.tool) && !known.contains(&r.id)));
        calls.sort_by_key(|r| r.t);
    }
    let (gaps, stopped_at) = probe_within(store, &calls, Some(deadline));
    let mut out = lines_listing(&gaps, if in_sweep { usize::MAX } else { MAX_RULES_LISTED });
    if stopped_at.is_some() {
        out.push(format!(
            "route probe: stopped after {} second(s) before every real call of the last 30 days was checked, so the list may be incomplete{}",
            budget.as_secs(),
            if in_sweep { "" } else { " (run serve sweep for a longer look)" }
        ));
    } else if cut {
        out.push(format!(
            "route probe: more than {CALLS_KEPT} real calls of the last 30 days name a host or a script, so only the newest were checked and the list may be incomplete"
        ));
    }
    out
}


#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn rule(id: &str, text: &str, bindings: Vec<Binding>) -> Item {
        Item {
            id: id.to_string(),
            kind: model::item::Kind::Rule,
            text: text.to_string(),
            bindings,
            severity: None,
            project: None,
            tags: vec![],
            expires: None,
            key: None,
            falsifier: Some("it stops being true".to_string()),
            check: None,
        }
    }

    fn call(id: &str, t: i64, tool: &str, input: serde_json::Value) -> CallRow {
        serde_json::from_value(json!({ "k": "c", "id": id, "t": t, "tool": tool, "in": input })).unwrap()
    }

    fn store_with(rule: &Item) -> EventStore {
        let mut store = EventStore::in_memory().unwrap();
        model::store::declare(&mut store, "s", "s", "owner", rule).unwrap();
        store
    }

    const HOST: &str = "127.0.0.1:27182";

    fn fusion_rule() -> Item {
        rule(
            "no-new-bodies",
            "Make no new bodies through the fusion tool; edit the open ones.",
            vec![
                Binding::Target { kind: TargetKind::Host, value: HOST.to_string() },
                Binding::Target { kind: TargetKind::Command, value: "mcp__fusion__create_body".to_string() },
            ],
        )
    }

    /// The field case: a script that talks to the host, written and then run,
    /// is a route the rule's bindings never reached.
    #[test]
    fn a_script_that_names_the_host_is_a_route_the_rule_never_reached() {
        let store = store_with(&fusion_rule());
        let calls = vec![
            call("w1", 100, "Write", json!({ "file_path": "C:/work/fusion-script.ps1", "content": format!("Invoke-RestMethod http://{HOST}/create") })),
            call("p1", 200, "PowerShell", json!({ "command": "powershell -File fusion-script.ps1" })),
            call("p2", 300, "PowerShell", json!({ "command": "Get-Date" })),
        ];
        let gaps = probe(&store, &calls);
        assert_eq!(gaps.len(), 1, "{gaps:?}");
        let gap = &gaps[0];
        assert_eq!((gap.rule.as_str(), gap.host.as_str()), ("no-new-bodies", HOST));
        assert_eq!(gap.calls, 2, "the write and the run both mention the host: {gap:?}");
        assert!(gap.example.contains("fusion-script.ps1"), "{}", gap.example);
        assert!(gap.revise.starts_with("mcp__thor__revise(id: \"no-new-bodies\"") && gap.revise.contains("fusion-script.ps1"), "{}", gap.revise);
        assert!(gap.revise.contains(HOST) && gap.revise.contains("mcp__fusion__create_body"), "the existing bindings are kept: {}", gap.revise);
        let text = lines(&gaps).join("\n");
        assert!(text.contains("route probe: rule 'no-new-bodies'") && text.contains("example:"), "{text}");
    }

    /// A record with no such call: the probe is silent.
    #[test]
    fn a_clean_record_gives_no_line() {
        let store = store_with(&fusion_rule());
        let calls = vec![
            call("p1", 100, "PowerShell", json!({ "command": "Get-ChildItem" })),
            call("w1", 200, "Write", json!({ "file_path": "notes.md", "content": "nothing about it" })),
        ];
        assert!(probe(&store, &calls).is_empty());
        assert!(lines(&probe(&store, &[])).is_empty());
    }

    /// A route the rule is already bound to is not listed, and the probe
    /// changes nothing in the store.
    #[test]
    fn a_route_already_bound_is_not_listed_and_nothing_is_written() {
        let mut bound = fusion_rule();
        bound.bindings.push(Binding::Target { kind: TargetKind::Path, value: "C:/work/fusion-script.ps1".to_string() });
        let store = store_with(&bound);
        let before = store.get_next_seq().unwrap();
        let calls = vec![call(
            "w1",
            100,
            "Write",
            json!({ "file_path": "C:/work/fusion-script.ps1", "content": format!("curl http://{HOST}/x") }),
        )];
        assert!(probe(&store, &calls).is_empty(), "the script path is bound");
        assert_eq!(store.get_next_seq().unwrap(), before, "read-only");
    }

    /// The port: a URL with the port and a bare `ip:port` token both reach a
    /// Host binding that names that port, so the probe no longer lists them
    /// (it used to: the binding never fired on them).
    #[test]
    fn a_call_the_host_binding_with_a_port_now_reaches_is_not_listed() {
        let store = store_with(&fusion_rule());
        let calls = vec![
            call("c1", 100, "Bash", json!({ "command": format!("curl http://{HOST}/x") })),
            call("c2", 200, "Bash", json!({ "command": format!("nc {HOST}") })),
            call("t1", 300, "mcp__fusion__create_body", json!({ "name": HOST })),
        ];
        assert!(probe(&store, &calls).is_empty(), "{:?}", probe(&store, &calls));
    }

    fn written(path: &str) -> CallRow {
        call("w", 100, "Write", json!({ "file_path": path, "content": format!("see http://{HOST}/create") }))
    }

    /// How many real calls the probe counts, and the example it shows, for a
    /// record of one write of `path` and one shell call.
    fn run_of(path: &str, tool: &str, command: &str) -> (usize, String) {
        let store = store_with(&fusion_rule());
        let calls = vec![written(path), call("p", 200, tool, json!({ "command": command }))];
        let gaps = probe(&store, &calls);
        assert_eq!(gaps.len(), 1, "{gaps:?}");
        (gaps[0].calls, gaps[0].example.clone())
    }

    /// The field case: a document that a command only edits, reads, copies or
    /// moves is never a script that command runs.
    #[test]
    fn a_document_a_command_only_edits_is_not_a_route() {
        for command in [
            "sed -i 's/a/b/' CHANGELOG.md",
            "cat CHANGELOG.md",
            "cp CHANGELOG.md /tmp/old.md",
            "mv docs/CHANGELOG.md docs/OLD.md",
            "echo x >> docs/CHANGELOG.md",
            "git add docs/CHANGELOG.md",
        ] {
            let (calls, example) = run_of("C:/work/docs/CHANGELOG.md", "Bash", command);
            assert_eq!(calls, 1, "only the write names the host: {command}");
            assert!(!example.contains("runs"), "{command}: {example}");
        }
    }

    /// A bare file name is not enough: the same name in another folder is
    /// another file.
    #[test]
    fn the_same_file_name_in_another_folder_is_not_the_script() {
        let (calls, _) = run_of("C:/other/run.ps1", "PowerShell", "powershell -File C:/work/run.ps1");
        assert_eq!(calls, 1);
        let (calls, _) = run_of("C:/work/run.ps1", "PowerShell", "powershell -File C:/other/run.ps1");
        assert_eq!(calls, 1);
        let (calls, _) = run_of("C:/other/run.sh", "Bash", "cd /work && ./run.sh");
        assert_eq!(calls, 2, "a relative path matches by suffix, so ./run.sh may be the written one");
        let (calls, _) = run_of("C:/other/run.sh", "Bash", "bash /work/run.sh");
        assert_eq!(calls, 1, "an absolute path names exactly one folder");
    }

    /// Every way a command really executes a file is a route.
    #[test]
    fn a_command_that_executes_the_file_is_a_route() {
        for (tool, path, command) in [
            ("Bash", "C:/work/x.js", "node C:/work/x.js --flag"),
            ("Bash", "C:/work/x.py", "python3 -u C:/work/x.py"),
            ("Bash", "C:/work/x.sh", "bash -x C:/work/x.sh"),
            ("Bash", "C:/work/x.sh", "C:/work/x.sh"),
            ("Bash", "C:/work/x.sh", "sudo FOO=1 C:/work/x.sh"),
            ("Bash", "C:/work/x.sh", "source C:/work/x.sh"),
            ("PowerShell", "C:/work/x.ps1", "powershell -NoProfile -ExecutionPolicy Bypass -File C:\\work\\x.ps1"),
            ("PowerShell", "C:/work/x.ps1", "pwsh C:/work/x.ps1"),
            ("PowerShell", "C:/work/x.ps1", "& 'C:\\work\\x.ps1' -a 1"),
            ("PowerShell", "C:/work/x.ps1", ". C:\\work\\x.ps1"),
            ("PowerShell", "C:/work/x.bat", "cmd /c C:\\work\\x.bat"),
        ] {
            let (calls, example) = run_of(path, tool, command);
            assert_eq!(calls, 2, "{command}");
            assert!(example.contains(&format!("runs {path}")), "{command}: {example}");
        }
    }

    /// An interpreter given inline code runs no file.
    #[test]
    fn inline_code_runs_no_file() {
        for command in ["node -e \"require('./x.js')\" x.js", "python -c 'print(1)' x.py", "powershell -Command \"cat x.ps1\""] {
            let path = if command.contains(".js") { "C:/work/x.js" } else if command.contains(".py") { "C:/work/x.py" } else { "C:/work/x.ps1" };
            let (calls, _) = run_of(path, "Bash", command);
            assert_eq!(calls, 1, "{command}");
        }
    }

    /// From the record on disk: a script written with the host and then run is
    /// found by the two cheap passes, an unrelated call is never listed, and a
    /// record that fits the budget gives no "stopped" line.
    #[test]
    fn lines_for_finds_the_script_route_in_the_record_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        let rows = [
            call("w1", 100, "Write", json!({ "file_path": "C:/work/fusion-script.ps1", "content": format!("Invoke-RestMethod http://{HOST}/create") })),
            call("p1", 200, "PowerShell", json!({ "command": "powershell -File fusion-script.ps1" })),
            call("p2", 300, "PowerShell", json!({ "command": "Get-Date" })),
            call("p3", 400, "Bash", json!({ "command": "sed -i 's/a/b/' fusion-script.ps1.bak" })),
        ];
        let text: Vec<String> = rows.iter().map(|r| serde_json::to_string(r).unwrap()).collect();
        std::fs::write(corpus::calls_path(&db), text.join("\n") + "\n").unwrap();
        let store = store_with(&fusion_rule());
        let lines = lines_for(&db, &store, 500, Duration::from_secs(30), false);
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(lines[0].contains("2 real call(s)") && lines[0].contains("runs C:/work/fusion-script.ps1"), "{}", lines[0]);
    }

    /// `serve sweep` is the longer look: it lists every rule and never sends
    /// the reader to itself; doctor keeps its short list and the pointer.
    #[test]
    fn the_sweep_lists_every_rule_and_does_not_point_to_itself() {
        let gaps: Vec<RouteGap> = (0..MAX_RULES_LISTED + 3)
            .map(|i| RouteGap { rule: format!("r{i}"), host: "h".into(), calls: 1, example: "e".into(), revise: "v".into() })
            .collect();
        let short = lines(&gaps);
        assert_eq!(short.len(), MAX_RULES_LISTED + 1);
        assert!(short.last().unwrap().contains("run serve sweep"));
        let all = lines_listing(&gaps, usize::MAX);
        assert_eq!(all.len(), gaps.len());
        assert!(all.iter().all(|l| !l.contains("serve sweep")), "{all:?}");
    }

    /// A probe past its deadline stops and says how far it got.
    #[test]
    fn a_probe_past_its_deadline_stops_early() {
        let store = store_with(&fusion_rule());
        let calls: Vec<CallRow> = (0..1000).map(|i| call(&format!("c{i}"), i, "Bash", json!({ "command": format!("curl http://{HOST}/x{i}") }))).collect();
        let past = Instant::now() - Duration::from_secs(1);
        let (_, stopped_at) = probe_within(&store, &calls, Some(past));
        assert!(stopped_at.is_some());
        let (gaps, stopped_at) = probe_within(&store, &calls, None);
        assert!(stopped_at.is_none() && gaps.is_empty());
    }
}
