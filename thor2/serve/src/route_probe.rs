//! THE ROUTE PROBE (item 71): a rule bound to one route of a capability is
//! silent on another. `fusion-geen-nieuwe-bodies-maken` was bound to an MCP
//! tool and to the host `127.0.0.1:27182`; a PowerShell script that talked to
//! the same host never reached it, because the host sat inside the script
//! file and the command line named no host.
//!
//! WHAT THIS DOES. For every live rule bound to a host (and every rule bound
//! to a tool by name, with the hosts its own text names and the tool's own
//! name), it looks through the 30-day record of real calls (`corpus`) for the
//! two kinds of call that are a route to that host or tool the rule's bindings
//! never reached: a shell call that EXECUTES a script file that names it, and a
//! Write or Edit that PUTS it into a script file. The script's content is read
//! from the disk the way the hook reads it (`script_routes`, with a deadline),
//! and from the Write and Edit calls on record (matched by full path) when the
//! script is gone. Each gap says whether the hook's look-through already
//! reaches it. It names such a rule with an example call and the `revise` that
//! adds a binding for that script (its folder, when the gate would refuse the
//! script's own place).
//!
//! WHAT IT DOES NOT COUNT. A plain mention of the host in a command's text (a
//! grep for it), in an agent brief, in a tool argument or in a document is
//! text about the host, not a route to it: the guard's own host extraction
//! finds no host there, and binding a rule on that would widen it on no
//! evidence.
//!
//! WHAT IT NEVER DOES. It only REPORTS. It blocks nothing, writes nothing and
//! widens no binding: the owner or the evaluating agent adds the real binding
//! knowingly. It runs at `serve sweep` time only (not in doctor) on data THOR
//! already keeps, so the hot path pays nothing.
//!
//! KNOWN LIMITS. A host read from a rule's own text can be something that is
//! no host at all (an e-mail address, a file extension). And a script that
//! merely lives on a share named like the host is counted as talking to it.
//! That is why doctor does not print this: it is a look someone asks for.

use crate::corpus::{self, CallRow};
use crate::input::{self, ServeInput};
use crate::script_routes::{file_name_of, script_of, script_path, SCRIPT_EXTENSIONS};
use model::item::{Binding, Item, TargetKind};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use thor_core::event_store::EventStore;

/// How many rules a report names before it says how many more there are.
pub const MAX_RULES_LISTED: usize = 8;

/// One rule with a route it does not reach.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteGap {
    pub rule: String,
    /// The host, or the tool-name suffix, the route leads to.
    pub host: String,
    /// What the rule is bound to that this stands for, as a few words: `host
    /// 127.0.0.1:27182` or `the tool mcp__x__fusion_mcp_execute`.
    pub bound_to: String,
    /// Whether the hook's look-through reaches this route now (A), or why not.
    pub reach: Reach,
    /// How many real calls mention the host and were never reached.
    pub calls: usize,
    /// The newest such call, in a few words.
    pub example: String,
    /// The `revise` that adds the missing binding. A suggestion for a person
    /// to read; nothing applies it.
    pub revise: String,
}

/// Whether the hook's look-through (`script_routes`) reaches a route now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reach {
    /// The script can be read from the disk now, so the hook offers the rule
    /// when a call runs it.
    Now,
    /// A script was written that holds it: the hook reaches it once a call runs
    /// that script and the file is on disk.
    Later,
    /// The hook cannot reach it, for this reason. A true remaining gap.
    Cannot(&'static str),
}

impl Reach {
    fn words(self) -> String {
        match self {
            Reach::Now => "The hook already offers this rule when such a script runs, as information".to_string(),
            Reach::Later => "The hook will offer this rule once a call runs that script and it is on disk".to_string(),
            Reach::Cannot(why) => format!("The hook cannot reach this: {why}"),
        }
    }
}

/// What a rule stands for in a script: a host, or the tool-name suffix of a
/// tool it is bound to.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Lit {
    text: String,
    tool: bool,
}

/// The hosts a rule stands for: its Host bindings, plus - for a rule bound to a
/// tool by name - the hosts its own text names, plus the tool-name suffix of
/// every MCP tool it is bound to (`script_routes::tool_suffix`).
fn hosts_of(item: &Item) -> Vec<Lit> {
    let mut lits: Vec<Lit> = literals_of(item).into_iter().map(|text| Lit { text, tool: false }).collect();
    for binding in &item.bindings {
        if let Binding::Target { kind: TargetKind::Command, value } = binding {
            if let Some(suffix) = crate::script_routes::tool_suffix(value) {
                if !lits.iter().any(|l| l.text == suffix) {
                    lits.push(Lit { text: suffix, tool: true });
                }
            }
        }
    }
    lits
}

fn literals_of(item: &Item) -> Vec<String> {
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

/// A path as a key: forward slashes, lowercase, no leading `./`.
fn norm_path(path: &str) -> String {
    let flat = path.trim().replace('\\', "/").to_lowercase();
    flat.strip_prefix("./").unwrap_or(&flat).to_string()
}

fn is_absolute(path: &str) -> bool {
    path.starts_with('/') || path.starts_with('~') || path.as_bytes().get(1) == Some(&b':')
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

/// Whether the write gate would refuse `item` as it stands (a place already
/// full of rivals).
fn gate_refuses(store: &EventStore, item: &Item) -> bool {
    matches!(model::store::capacity(store, item), Ok(model::store::Capacity::Full(_)) | Ok(model::store::Capacity::DeadOnArrival(_)))
}

/// The `revise` that adds `extra` to the rule, as a call text. A Path binding
/// the gate would refuse (the script's own place already holds a block's worth
/// of facts) is replaced by the Dir of its folder, and the text says so.
fn revise_for(store: &EventStore, item: &Item, id: &str, extra: &(TargetKind, String)) -> String {
    let call = |bindings: &[Binding]| {
        format!(
            "mcp__thor__revise(id: \"{id}\", {}, because: \"<this route reaches the same host, at least 20 characters>\")",
            model::store::binding_arguments(bindings)
        )
    };
    let with = |kind: TargetKind, value: &str| {
        let mut bindings = item.bindings.clone();
        bindings.push(Binding::Target { kind, value: value.to_string() });
        let mut updated = item.clone();
        updated.id = id.to_string();
        updated.bindings = bindings.clone();
        (bindings, updated)
    };
    let (bindings, updated) = with(extra.0, &extra.1);
    if extra.0 != TargetKind::Path || !gate_refuses(store, &updated) {
        return call(&bindings);
    }
    let flat = extra.1.replace('\\', "/");
    let Some((folder, _)) = flat.rsplit_once('/').filter(|(f, _)| !f.is_empty()) else {
        return format!("{} (the script's own place already holds {} facts, so the gate would refuse this)", call(&bindings), model::item::MAX_ITEMS);
    };
    let (dir_bindings, dir_updated) = with(TargetKind::Dir, folder);
    if gate_refuses(store, &dir_updated) {
        return format!(
            "no call is offered: the script's own place and its folder {folder} both already hold {} facts, so the gate would refuse either binding. Make room there first (fold, move or retract one of them)",
            model::item::MAX_ITEMS
        );
    }
    format!(
        "{} (the script's own place already holds {} facts and would refuse a binding, so this binds its folder {folder} instead)",
        call(&dir_bindings),
        model::item::MAX_ITEMS
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
struct Found {
    calls: usize,
    t: i64,
    at: usize,
    via_script: Option<String>,
    reach: Reach,
}

/// The gaps, most calls first, from what was found per (rule, host) place.
/// The example and the `revise` text are built here, once per gap, not for
/// every call.
fn gaps_of(store: &EventStore, rules: &Rules, calls: &[CallRow], found: BTreeMap<(usize, usize), Found>) -> Vec<RouteGap> {
    let mut out: Vec<RouteGap> = found
        .into_iter()
        .map(|((r, h), f)| {
            let (id, item, lits) = &rules[r];
            let row = &calls[f.at];
            let lit = &lits[h];
            let what = if lit.tool { "the tool" } else { "the host" };
            let bound_to = if lit.tool {
                let full = item
                    .bindings
                    .iter()
                    .find_map(|b| match b {
                        Binding::Target { kind: TargetKind::Command, value } if crate::script_routes::tool_suffix(value).as_deref() == Some(lit.text.as_str()) => {
                            value.split_whitespace().next().map(str::to_string)
                        }
                        _ => None,
                    })
                    .unwrap_or_else(|| lit.text.clone());
                format!("the tool {full}")
            } else {
                format!("host {}", lit.text)
            };
            RouteGap {
                rule: id.clone(),
                host: lit.text.clone(),
                bound_to,
                reach: f.reach,
                calls: f.calls,
                example: match &f.via_script {
                    Some(script) => format!("{} (runs {script}, which names {what})", example_of(row)),
                    None => example_of(row),
                },
                revise: revise_for(store, item, id, &missing_binding(row, f.via_script.as_deref())),
            }
        })
        .collect();
    out.sort_by(|a, b| b.calls.cmp(&a.calls).then_with(|| a.rule.cmp(&b.rule)).then_with(|| a.host.cmp(&b.host)));
    out
}

type Rules = Vec<(String, Item, Vec<Lit>)>;

/// The live rules that stand for at least one host or tool, with those.
fn rules_of(store: &EventStore) -> Rules {
    crate::live::live_items(store)
        .into_iter()
        .filter(|li| li.item.kind.can_fire())
        .map(|li| {
            let lits = hosts_of(&li.item);
            (li.id, li.item, lits)
        })
        .filter(|(_, _, lits)| !lits.is_empty())
        .collect()
}

/// Every host and tool word once, lowercase: what a call or a written file is
/// searched for.
fn needles_of(rules: &Rules) -> Vec<String> {
    let mut needles: Vec<String> = Vec::new();
    for (_, _, lits) in rules {
        for lit in lits {
            let needle = lit.text.to_lowercase();
            if !needles.contains(&needle) {
                needles.push(needle);
            }
        }
    }
    needles
}

/// A normalised path as the key of a script's content.
fn key_of(path: &Path) -> String {
    norm_path(&path.to_string_lossy())
}

/// The script files shell calls run, read from the disk NOW (what the hook
/// would read): by normalised path, the needles (by index) each holds. A path
/// that cannot be read is absent, and so is every path on a share that did not
/// answer.
#[derive(Default)]
pub struct DiskScripts {
    by_path: HashMap<String, Vec<usize>>,
}

/// How many distinct script paths one probe tries to read.
const MAX_DISK_PATHS: usize = 4000;

/// The share a path lives on (`//server/share`), so one dead share is asked
/// once.
fn share_of(key: &str) -> Option<String> {
    let rest = key.strip_prefix("//")?;
    let mut parts = rest.split('/');
    Some(format!("//{}/{}", parts.next()?, parts.next().unwrap_or("")))
}

/// What the script of segment `seg` of `row` resolves to, the way the hook
/// resolves it (`script_routes::script_path`, against the call's own `cwd`).
fn resolved(row: &CallRow, seg: &model::shell::Segment, script: &str) -> Option<PathBuf> {
    let cwd = (!row.cwd.is_empty()).then(|| Path::new(row.cwd.as_str()));
    script_path(script, seg.dir.as_deref(), seg.dir != seg.any_cd_dir, cwd)
}

/// The scripts the shell calls in `calls` run that are on the disk now, read
/// the way the hook reads them, and which of `needles` each holds. Stops at
/// `deadline`; a read that does not answer in time marks its whole share dead.
pub fn disk_scripts(calls: &[CallRow], needles: &[String], deadline: Option<Instant>) -> DiskScripts {
    let mut paths: Vec<PathBuf> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for row in calls.iter().filter(|r| is_shell(&r.tool)) {
        let Some(command) = row.input.get("command").and_then(|c| c.as_str()) else { continue };
        let lower = command.to_lowercase();
        if !SCRIPT_EXTENSIONS.iter().any(|e| lower.contains(e)) {
            continue;
        }
        for seg in model::shell::call_segments(&row.tool, command).iter().filter(|s| !s.remote && !s.tool) {
            let Some(script) = script_of(seg) else { continue };
            let Some(path) = resolved(row, seg, &script) else { continue };
            if seen.insert(key_of(&path)) {
                paths.push(path);
            }
        }
        if paths.len() >= MAX_DISK_PATHS {
            break;
        }
    }
    let mut out = DiskScripts::default();
    let mut dead_shares: std::collections::HashSet<String> = std::collections::HashSet::new();
    for path in paths {
        if deadline.is_some_and(|d| Instant::now() >= d) {
            break;
        }
        let key = key_of(&path);
        let share = share_of(&key);
        if share.as_ref().is_some_and(|s| dead_shares.contains(s)) {
            continue;
        }
        let wait = deadline
            .map(|d| d.saturating_duration_since(Instant::now()))
            .unwrap_or(crate::script_routes::BUDGET)
            .min(crate::script_routes::BUDGET);
        let target = path.clone();
        match crate::work_folders::within(wait, move || crate::script_routes::read_script(&target)) {
            None => {
                dead_shares.extend(share);
            }
            Some(None) => {}
            Some(Some(text)) => {
                let text = text.to_lowercase();
                let hits = (0..needles.len()).filter(|n| text.contains(&needles[*n])).collect();
                out.by_path.insert(key, hits);
            }
        }
    }
    out
}

/// [`probe`] that stops at `deadline`. The second answer is how many of the
/// calls it looked at when it stopped early, `None` when it saw them all.
pub fn probe_within(store: &EventStore, calls: &[CallRow], deadline: Option<Instant>) -> (Vec<RouteGap>, Option<usize>) {
    probe_with(store, calls, deadline, &DiskScripts::default())
}

/// One script a shell call runs that holds a needle: where it is, which
/// needles, and whether the hook reaches it.
struct ScriptHit {
    path: String,
    hits: Vec<usize>,
    reach: Reach,
}

/// [`probe_within`] with the script files read from the disk (`disk_scripts`):
/// a script on the disk decides over what the Write record says it held, and
/// is one the hook reaches now.
pub fn probe_with(store: &EventStore, calls: &[CallRow], deadline: Option<Instant>, disk: &DiskScripts) -> (Vec<RouteGap>, Option<usize>) {
    let rules = rules_of(store);
    if rules.is_empty() {
        return (Vec::new(), None);
    }
    let out_of_time = |seen: usize| seen % 16 == 0 && deadline.is_some_and(|d| Instant::now() >= d);
    let needles = needles_of(&rules);
    // The content written by Write and Edit calls, by full path: the fallback
    // for a script that is no longer on the disk.
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
    // Which needles each written file holds, once; only the names of the files
    // that hold any are worth looking for in a command.
    let mut hot_names: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (key, file) in written.files.iter_mut() {
        file.hits = (0..needles.len()).filter(|n| file.content.contains(&needles[*n])).collect();
        file.content = String::new();
        if !file.hits.is_empty() {
            hot_names.insert(file_name_of(key));
        }
    }
    // Per rule and literal: how many calls, and the newest one. The example and
    // the `revise` text are built once at the end, not for every call.
    let mut found: BTreeMap<(usize, usize), Found> = BTreeMap::new();
    for (seen, row) in calls.iter().enumerate() {
        if out_of_time(seen) {
            return (gaps_of(store, &rules, calls, found), Some(seen));
        }
        let text = call_text(row);
        let named: Vec<bool> = needles.iter().map(|n| text.contains(n)).collect();
        let writes_script = is_writer(&row.tool)
            && row.input.get("file_path").and_then(|p| p.as_str()).is_some_and(|p| {
                let flat = p.replace('\\', "/").to_lowercase();
                SCRIPT_EXTENSIONS.iter().any(|e| flat.ends_with(e))
            });
        // The shell call read as a command, once and only when needed.
        let command = row.input.get("command").and_then(|c| c.as_str());
        let mut segments: Option<Vec<model::shell::Segment>> = None;
        // The scripts this shell call really executes that hold a needle.
        let mut scripts: Vec<ScriptHit> = Vec::new();
        if is_shell(&row.tool) && (!hot_names.is_empty() || !disk.by_path.is_empty()) {
            let lower = command.unwrap_or("").to_lowercase();
            if SCRIPT_EXTENSIONS.iter().any(|e| lower.contains(e)) {
                let run: Vec<(String, Option<PathBuf>)> = segments_of(&mut segments, &row.tool, command)
                    .iter()
                    .filter(|s| !s.remote && !s.tool)
                    .filter_map(|seg| script_of(seg).map(|script| (script.clone(), resolved(row, seg, &script))))
                    .collect();
                for (script, path) in run {
                    // A script on the disk now decides: the hook reads it.
                    if let Some(path) = &path {
                        if let Some(hits) = disk.by_path.get(&key_of(path)) {
                            if !hits.is_empty() {
                                scripts.push(ScriptHit { path: path.to_string_lossy().replace('\\', "/"), hits: hits.clone(), reach: Reach::Now });
                            }
                            continue;
                        }
                    }
                    let why = match path {
                        None => "the script's path holds a variable or a glob, or its folder cannot be told",
                        Some(_) => "the script is not on the disk any more (what it held comes from the record of what was written)",
                    };
                    for file in written.find(&script).into_iter().filter(|f| !f.hits.is_empty()) {
                        scripts.push(ScriptHit { path: file.path.clone(), hits: file.hits.clone(), reach: Reach::Cannot(why) });
                    }
                }
            }
        }
        // What the hook would see: with its moments (costly to work out) only
        // for a rule bound to a moment, else the targets alone.
        let mut targets_input: Option<ServeInput> = None;
        let mut full_input: Option<ServeInput> = None;
        for (r, (_, item, lits)) in rules.iter().enumerate() {
            for (h, lit) in lits.iter().enumerate() {
                let needle = lit.text.to_lowercase();
                let Some(n) = needles.iter().position(|x| *x == needle) else { continue };
                let via = scripts.iter().find(|f| f.hits.contains(&n));
                // Only two kinds of call are a route: a shell call that runs a
                // script with the host in it, and a Write or Edit that puts
                // the host into a script. A mention of the host in a command,
                // a brief, a tool argument or a document is only text.
                let writes_it_into_script = named[n] && writes_script;
                if via.is_none() && !writes_it_into_script {
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
                let reach = via.map(|f| f.reach).unwrap_or(Reach::Later);
                let entry = found.entry((r, h)).or_insert(Found { calls: 0, t: i64::MIN, at: seen, via_script: None, reach });
                entry.calls += 1;
                if row.t >= entry.t {
                    entry.t = row.t;
                    entry.at = seen;
                    entry.via_script = via.map(|f| f.path.clone());
                    entry.reach = reach;
                }
            }
        }
    }
    (gaps_of(store, &rules, calls, found), None)
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
        let what = if gap.bound_to.is_empty() { gap.host.clone() } else { gap.bound_to.clone() };
        let covered = match gap.reach {
            Reach::Now => "Nothing to do unless the rule should also refuse (a refusing check needs a real binding), and then:",
            _ => "If that route is meant to be covered:",
        };
        out.push(format!(
            "route probe: rule '{}' is bound to {}, and {} real call(s) of the last 30 days run a script with that host or tool name in it, or write it into a script, without reaching the rule (example: {}). {}. {} {}",
            gap.rule,
            what,
            gap.calls,
            gap.example,
            gap.reach.words(),
            covered,
            gap.revise
        ));
    }
    if gaps.len() > listed {
        out.push(format!("route probe: and {} more rule(s), not listed here (run serve sweep)", gaps.len() - listed));
    }
    out
}

/// A search for any of `words`, which are searched as plain text, in a line
/// that is lowercased first (`LowerSearch::is_match`). A plain literal
/// alternation is what the regex crate runs with its fast literal search; the
/// case-insensitive form of it ran in the slow engine and took minutes over the
/// whole record.
struct LowerSearch(regex::Regex);

impl LowerSearch {
    fn is_match(&self, line: &str) -> bool {
        if line.is_ascii() {
            self.0.is_match(&line.to_ascii_lowercase())
        } else {
            self.0.is_match(&line.to_lowercase())
        }
    }
}

fn any_of(words: &[String]) -> LowerSearch {
    let escaped: Vec<String> = words.iter().map(|w| regex::escape(&w.to_lowercase())).collect();
    LowerSearch(regex::Regex::new(&escaped.join("|")).expect("escaped words make a valid pattern"))
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
    let (mut calls, _, mut cut, mut ran_out) = corpus::select_calls_until(db, now, CALLS_KEPT, &|line| names_a_host.is_match(line), Some(deadline));
    let mut hot_names: Vec<String> = Vec::new();
    for row in calls.iter().filter(|r| is_writer(&r.tool)) {
        let name = row.input.get("file_path").and_then(|p| p.as_str()).map(file_name_of).unwrap_or_default();
        if !name.is_empty() && !hot_names.contains(&name) {
            hot_names.push(name);
        }
    }
    // The second pass: the shell calls that name a file written with a host,
    // and every shell call that runs a script at all (the script is read from
    // the disk, and may hold a host the record never saw written).
    let runs_a_script = any_of(&SCRIPT_EXTENSIONS.iter().map(|e| e.to_string()).collect::<Vec<_>>());
    let names_a_file = (!hot_names.is_empty()).then(|| any_of(&hot_names));
    let (more, _, more_cut, more_ran_out) = if ran_out.is_some() {
        (Vec::new(), 0, false, None)
    } else {
        corpus::select_calls_until(
            db,
            now,
            CALLS_KEPT,
            &|line| names_a_file.as_ref().is_some_and(|re| re.is_match(line)) || runs_a_script.is_match(line),
            Some(deadline),
        )
    };
    cut |= more_cut;
    ran_out = ran_out.or(more_ran_out);
    let known: std::collections::HashSet<String> = calls.iter().map(|r| r.id.clone()).collect();
    calls.extend(more.into_iter().filter(|r| is_shell(&r.tool) && !known.contains(&r.id)));
    calls.sort_by_key(|r| r.t);
    let disk = disk_scripts(&calls, &needles, Some(deadline));
    let (gaps, stopped_at) = probe_with(store, &calls, Some(deadline), &disk);
    let mut out = lines_listing(&gaps, if in_sweep { usize::MAX } else { MAX_RULES_LISTED });
    if let Some(stop) = ran_out {
        out.push(format!(
            "route probe: the route check ran out of time after {} of about {} calls, so the list may be incomplete{}",
            stop.calls_seen,
            stop.calls_in_all(),
            if in_sweep { "" } else { " (run serve sweep for a longer look)" }
        ));
    } else if stopped_at.is_some() {
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

    #[test]
    fn the_host_search_ignores_case_in_the_line_and_in_the_words() {
        let search = any_of(&["Staging.Example.Test".to_string(), "deploy.PS1".to_string()]);
        assert!(search.is_match("ssh STAGING.example.test uptime"));
        assert!(search.is_match("run Deploy.ps1 now"));
        assert!(search.is_match("caf\u{e9} STAGING.example.test"), "a line with other letters is lowercased whole");
        assert!(!search.is_match("ssh production.example.test"));
    }

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
        gaps.first().map(|g| (g.calls, g.example.clone())).unwrap_or((0, String::new()))
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
            assert_eq!(calls, 0, "a document is no route, written or edited: {command}");
            assert!(example.is_empty(), "{command}: {example}");
        }
    }

    /// A plain mention of the host is text about it, not a route: a grep for
    /// it, an agent brief, a tool argument, an Edit of a document.
    #[test]
    fn a_plain_mention_of_the_host_is_not_counted() {
        let store = store_with(&fusion_rule());
        let calls = vec![
            call("g", 100, "Bash", json!({ "command": format!("grep -rn '{HOST}' docs/") })),
            call("a", 200, "Agent", json!({ "prompt": format!("The fusion server listens on {HOST}; do not touch it.") })),
            call("e", 300, "Edit", json!({ "file_path": "C:/work/README.md", "old_string": "x", "new_string": format!("server at {HOST}") })),
            call("t", 400, "mcp__other__tool", json!({ "address": HOST })),
        ];
        assert!(probe(&store, &calls).is_empty(), "{:?}", probe(&store, &calls));
    }

    /// Writing the host into a script is counted, so the rule can be bound
    /// before the route is first used.
    #[test]
    fn a_write_of_a_script_that_names_the_host_is_counted() {
        let store = store_with(&fusion_rule());
        let calls = vec![call("w", 100, "Write", json!({ "file_path": "C:/work/x.py", "content": format!("requests.get('http://{HOST}/x')") }))];
        let gaps = probe(&store, &calls);
        assert_eq!(gaps.len(), 1, "{gaps:?}");
        assert!(gaps[0].revise.contains("C:/work/x.py"), "{}", gaps[0].revise);
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
            .map(|i| RouteGap {
                rule: format!("r{i}"),
                host: "h".into(),
                bound_to: "host h".into(),
                reach: Reach::Now,
                calls: 1,
                example: "e".into(),
                revise: "v".into(),
            })
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

    // ------------------------------------------------ B: the extended probe

    fn in_folder(id: &str, t: i64, tool: &str, cwd: &str, command: &str) -> CallRow {
        serde_json::from_value(json!({ "k": "c", "id": id, "t": t, "tool": tool, "cwd": cwd, "in": { "command": command } })).unwrap()
    }

    fn script_folder(name: &str, content: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("thor-route-probe-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("run.ps1"), content).unwrap();
        dir
    }

    /// The content comes from the disk, read the way the hook reads it: a
    /// script the Write record never saw (or saw clipped) is found, a relative
    /// path is resolved against the call's own folder, and the gap says the
    /// hook already reaches it.
    #[test]
    fn a_script_on_the_disk_is_found_by_its_content_and_labelled_as_reached() {
        let dir = script_folder("disk", &format!("$u = 'http://{HOST}/create'\n"));
        let store = store_with(&fusion_rule());
        let cwd = dir.to_string_lossy().to_string();
        let calls = vec![in_folder("p1", 200, "PowerShell", &cwd, "powershell -File run.ps1")];
        let needles = needles_of(&rules_of(&store));
        let disk = disk_scripts(&calls, &needles, None);
        let (gaps, _) = probe_with(&store, &calls, None, &disk);
        assert_eq!(gaps.len(), 1, "{gaps:?}");
        assert_eq!(gaps[0].reach, Reach::Now);
        assert!(gaps[0].example.contains("run.ps1"), "{}", gaps[0].example);
        assert!(lines(&gaps)[0].contains("already offers this rule"), "{}", lines(&gaps)[0]);
        // The same call with no folder on record cannot be resolved: no read.
        let calls = vec![in_folder("p2", 200, "PowerShell", "", "powershell -File run.ps1")];
        let disk = disk_scripts(&calls, &needles, None);
        assert!(probe_with(&store, &calls, None, &disk).0.is_empty());
    }

    /// A script that is gone from the disk is a gap the hook cannot reach, and
    /// says why; its content still comes from the record.
    #[test]
    fn a_script_gone_from_the_disk_is_labelled_as_unreachable_with_its_reason() {
        let store = store_with(&fusion_rule());
        let calls = vec![
            call("w1", 100, "Write", json!({ "file_path": "C:/gone/fusion-script.ps1", "content": format!("curl http://{HOST}/x") })),
            call("p1", 200, "PowerShell", json!({ "command": "powershell -File C:/gone/fusion-script.ps1" })),
            call("p2", 300, "PowerShell", json!({ "command": "powershell -File $env:W/fusion-script.ps1" })),
        ];
        let needles = needles_of(&rules_of(&store));
        let disk = disk_scripts(&calls, &needles, None);
        let (gaps, _) = probe_with(&store, &calls, None, &disk);
        assert_eq!(gaps.len(), 1, "{gaps:?}");
        match gaps[0].reach {
            Reach::Cannot(why) => assert!(why.contains("not on the disk") || why.contains("variable"), "{why}"),
            other => panic!("{other:?}"),
        }
        assert!(lines(&gaps)[0].contains("cannot reach this"), "{}", lines(&gaps)[0]);
    }

    /// The tool-name route: a script that holds the last part of a bound MCP
    /// tool is reported next to the host route.
    #[test]
    fn a_script_that_names_the_bound_tool_is_a_route() {
        let tool_rule = rule(
            "no-new-bodies",
            "Make no new bodies through the fusion tool.",
            vec![Binding::Target { kind: TargetKind::Command, value: "mcp__Autodesk_Fusion__fusion_mcp_execute".to_string() }],
        );
        let store = store_with(&tool_rule);
        let calls = vec![
            call("w1", 100, "Write", json!({ "file_path": "C:/work/fusion-script.ps1", "content": "name = \"fusion_mcp_execute\"" })),
            call("p1", 200, "PowerShell", json!({ "command": "powershell -File C:/work/fusion-script.ps1" })),
        ];
        let gaps = probe(&store, &calls);
        assert_eq!(gaps.len(), 1, "{gaps:?}");
        assert_eq!(gaps[0].host, "fusion_mcp_execute");
        assert!(gaps[0].bound_to.contains("mcp__Autodesk_Fusion__fusion_mcp_execute"), "{}", gaps[0].bound_to);
        assert!(gaps[0].example.contains("which names the tool"), "{}", gaps[0].example);
    }

    /// The printed `revise` is checked against the write gate: a script place
    /// that already holds a block's worth of facts would refuse a Path binding,
    /// so the call binds the script's folder instead and says so.
    #[test]
    fn a_revise_the_gate_would_refuse_is_replaced_by_the_scripts_folder() {
        let mut store = EventStore::in_memory().unwrap();
        for n in 0..model::item::MAX_ITEMS {
            let busy = rule(
                &format!("busy-{n}"),
                &format!("busy fact number {n} about the script"),
                vec![Binding::Target { kind: TargetKind::Path, value: "C:/work/fusion-script.ps1".to_string() }],
            );
            model::store::declare(&mut store, "s", "s", "owner", &busy).unwrap();
        }
        model::store::declare(&mut store, "s", "s", "owner", &fusion_rule()).unwrap();
        let calls = vec![
            call("w1", 100, "Write", json!({ "file_path": "C:/work/fusion-script.ps1", "content": format!("curl http://{HOST}/x") })),
            call("p1", 200, "PowerShell", json!({ "command": "powershell -File C:/work/fusion-script.ps1" })),
        ];
        let gaps = probe(&store, &calls);
        assert_eq!(gaps.len(), 1, "{gaps:?}");
        assert!(gaps[0].revise.contains("binds its folder C:/work instead"), "{}", gaps[0].revise);
        assert!(gaps[0].revise.contains("dir"), "{}", gaps[0].revise);
    }
}
