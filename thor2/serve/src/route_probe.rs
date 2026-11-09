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

/// Whether any binding of `item` reaches the call `input` stands for (the same
/// primitives the hook and `pool_rivals` use).
fn reached(item: &Item, input: &ServeInput, row: &CallRow) -> bool {
    let segments = row
        .input
        .get("command")
        .and_then(|c| c.as_str())
        .map(|c| model::shell::call_segments(&row.tool, c))
        .unwrap_or_default();
    item.bindings.iter().any(|binding| match binding {
        Binding::Moment(action) => input.moments.contains(action),
        Binding::Always => false,
        Binding::Target { kind, value } => {
            input.targets.iter().any(|(k, v)| model::normalize::target_matches(*kind, value, *k, v))
                || (*kind == TargetKind::Command && model::shell::reaches_any(value, &segments))
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
    let rules: Vec<(String, Item, Vec<String>)> = crate::live::live_items(store)
        .into_iter()
        .filter(|li| li.item.kind.can_fire())
        .map(|li| {
            let hosts = hosts_of(&li.item);
            (li.id, li.item, hosts)
        })
        .filter(|(_, _, hosts)| !hosts.is_empty())
        .collect();
    if rules.is_empty() {
        return Vec::new();
    }
    // The content written by Write and Edit calls, by file name: a script a
    // later call runs is read from here, never from the disk.
    let mut written: HashMap<String, String> = HashMap::new();
    for row in calls.iter().filter(|r| is_writer(&r.tool)) {
        if let Some(path) = row.input.get("file_path").and_then(|p| p.as_str()) {
            written.entry(file_name_of(path)).or_default().push_str(&call_text(row));
        }
    }
    let mut gaps: BTreeMap<(String, String), RouteGap> = BTreeMap::new();
    let mut newest: BTreeMap<(String, String), i64> = BTreeMap::new();
    for row in calls {
        let text = call_text(row);
        let Some(call_input) = input_of(row) else { continue };
        // The scripts a shell call runs whose written content is on record.
        let scripts: Vec<(String, String)> = if is_shell(&row.tool) {
            input::paths_in_command(row.input.get("command").and_then(|c| c.as_str()).unwrap_or(""))
                .into_iter()
                .filter_map(|p| written.get(&file_name_of(&p)).map(|content| (p, content.clone())))
                .collect()
        } else {
            Vec::new()
        };
        for (id, item, hosts) in &rules {
            for host in hosts {
                let needle = host.to_lowercase();
                let via_script = scripts.iter().find(|(_, content)| content.contains(&needle)).map(|(p, _)| p.clone());
                if !text.contains(&needle) && via_script.is_none() {
                    continue;
                }
                if reached(item, &call_input, row) {
                    continue;
                }
                let key = (id.clone(), host.clone());
                let gap = gaps.entry(key.clone()).or_insert_with(|| RouteGap {
                    rule: id.clone(),
                    host: host.clone(),
                    calls: 0,
                    example: String::new(),
                    revise: String::new(),
                });
                gap.calls += 1;
                if newest.get(&key).is_none_or(|t| row.t >= *t) {
                    newest.insert(key, row.t);
                    gap.example = match &via_script {
                        Some(script) => format!("{} (runs {script}, which names the host)", example_of(row)),
                        None => example_of(row),
                    };
                    gap.revise = revise_for(item, id, &missing_binding(row, via_script.as_deref().map(|s| s.trim())));
                }
            }
        }
    }
    let mut out: Vec<RouteGap> = gaps.into_values().collect();
    out.sort_by(|a, b| b.calls.cmp(&a.calls).then_with(|| a.rule.cmp(&b.rule)));
    out
}

/// The lines doctor and `serve sweep` show: silent when no rule has a route it
/// does not reach.
pub fn lines(gaps: &[RouteGap]) -> Vec<String> {
    let mut out = Vec::new();
    for gap in gaps.iter().take(MAX_RULES_LISTED) {
        out.push(format!(
            "route probe: rule '{}' is bound to {}, and {} real call(s) of the last 30 days mention it without reaching the rule (example: {}). If that route is meant to be covered: {}",
            gap.rule, gap.host, gap.calls, gap.example, gap.revise
        ));
    }
    if gaps.len() > MAX_RULES_LISTED {
        out.push(format!("route probe: and {} more rule(s), not listed here (run serve sweep)", gaps.len() - MAX_RULES_LISTED));
    }
    out
}

/// The probe over the record beside `db`, within `budget`: what doctor and
/// `serve sweep` call.
pub fn lines_for(db: &Path, store: &EventStore, now: i64, budget: Duration) -> Vec<String> {
    let deadline = Instant::now() + budget;
    let mut calls: Vec<CallRow> = Vec::new();
    corpus::for_each_call(db, now, |row| {
        calls.push(row);
        Instant::now() < deadline
    });
    lines(&probe(store, &calls))
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
}
