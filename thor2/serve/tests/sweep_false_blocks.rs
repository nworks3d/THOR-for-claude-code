//! Calls that do nothing harmful and were refused, found by replaying 145,976
//! real tool calls of 30 days through the guard (2026-10-09): a shell program
//! anchor reaching a Claude tool's pseudo-segment, a bounded Monitor loop, a
//! dry run or a help request, a repository reached through a variable or
//! made by the call itself, and an exported sandbox variable. Each case has
//! its harmful twin that must still be refused.

use model::item::{Binding, Check, Item, Kind, Severity, TargetKind};
use serde_json::json;
use std::path::{Path, PathBuf};
use thor_core::event_store::EventStore;

fn command(value: &str) -> Binding {
    Binding::Target { kind: TargetKind::Command, value: value.to_string() }
}

fn rule(id: &str, project: Option<&str>, anchors: &[&str], check: Check, tags: &[&str]) -> Item {
    Item {
        id: id.to_string(),
        kind: Kind::Rule,
        text: format!("probe rule {id}: the {} call needs its own answer, nothing else will do", anchors.join(" and ")),
        bindings: anchors.iter().map(|a| command(a)).collect(),
        severity: Some(Severity::Costly),
        project: project.map(str::to_string),
        tags: tags.iter().map(|t| t.to_string()).collect(),
        expires: None,
        key: None,
        falsifier: Some("the guard turns out to refuse nothing of this kind".to_string()),
        check: Some(check),
    }
}

fn requires(when: &str, required: &[&str]) -> Check {
    Check::Requires { when: when.to_string(), required: required.iter().map(|r| r.to_string()).collect() }
}

fn forbidden(literals: &[&str]) -> Check {
    Check::Forbidden { literals: literals.iter().map(|l| l.to_string()).collect() }
}

/// `item` with every check literal its text does not name appended to the text.
fn named(item: &Item) -> Item {
    let mut out = item.clone();
    let unnamed = model::gate::unnamed_literals(&out);
    if !unnamed.is_empty() {
        let shown: Vec<String> = unnamed.iter().map(|l| format!("'{}'", model::gate::shown_literal(l))).collect();
        out.text = format!("{} (names {})", out.text, shown.join(" "));
    }
    out
}

struct Fx {
    _dir: tempfile::TempDir,
    store: EventStore,
    root: PathBuf,
}

impl Fx {
    fn new(items: &[Item]) -> Fx {
        let dir = tempfile::tempdir().unwrap();
        let mut store = EventStore::new(&dir.path().join("thor.db")).unwrap();
        for item in items {
            model::store::declare(&mut store, "s", "l", "a", &named(item)).unwrap_or_else(|e| panic!("{}: {e}", item.id));
        }
        let root = dir.path().to_path_buf();
        Fx { _dir: dir, store, root }
    }

    /// A folder that resolves to `project` through its own marker file.
    fn checkout(&self, project: &str) -> PathBuf {
        let dir = self.root.join(project);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(".thor-project"), format!("{project}\n")).unwrap();
        dir
    }

    /// Whether the guard refuses `tool` with `input`, as a session of `project` standing in `cwd`.
    fn refused(&self, tool: &str, input: serde_json::Value, project: Option<&str>, cwd: &Path) -> bool {
        self.verdict(tool, input, project, cwd).is_some()
    }

    fn verdict(&self, tool: &str, input: serde_json::Value, project: Option<&str>, cwd: &Path) -> Option<String> {
        let owner_said_nothing: &dyn Fn() -> Result<Vec<String>, String> = &|| Ok(Vec::new());
        serve::guard_verdict::pre_tool(&self.store, tool, Some(&input), project, Some(cwd), Some(cwd), Some(owner_said_nothing))
            .verdict
            .map(|v| v.reason)
    }

    fn bash(&self, command: &str) -> bool {
        self.refused("Bash", json!({"command": command}), None, &self.root)
    }

    fn powershell(&self, command: &str) -> bool {
        self.refused("PowerShell", json!({"command": command}), None, &self.root)
    }
}

fn slashed(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

// ------------------------------------------------------------ 1. tool anchors

#[test]
fn a_shell_program_anchor_does_not_reach_the_tool_of_the_same_name() {
    let fx = Fx::new(&[rule("grep-p-needs-perl", None, &["grep"], requires("grep", &["perl"]), &[])]);
    assert!(
        !fx.refused("Grep", json!({"pattern": "[\\x{2014}]", "path": "."}), None, &fx.root),
        "the Grep tool is not the grep program"
    );
    assert!(fx.bash("grep -nP '[\\x{2014}]' file"), "the grep program is still refused");
}

#[test]
fn a_tool_anchor_still_reaches_its_tool_and_only_in_its_own_spelling() {
    let fx = Fx::new(&[rule("agent-names-a-model", None, &["Agent"], requires("Agent", &["haiku"]), &[])]);
    assert!(fx.refused("Agent", json!({"prompt": "summarise"}), None, &fx.root), "the Agent tool is still refused");
    assert!(!fx.refused("Agent", json!({"prompt": "summarise", "model": "haiku"}), None, &fx.root));
    assert!(!fx.bash("agent run --fast"), "a shell word agent is not the Agent tool");
    assert!(!fx.bash("Agent run --fast"), "nor is one spelled with a capital");
}

#[test]
fn a_lowercase_anchor_does_not_reach_a_tool_and_an_mcp_tool_needs_its_exact_name() {
    let fx = Fx::new(&[
        rule("agent-lowercase", None, &["agent"], requires("agent", &["haiku"]), &[]),
        rule("mcp-send", None, &["mcp__mail__send_message"], requires("mcp__mail__send_message", &["draft"]), &[]),
    ]);
    assert!(!fx.refused("Agent", json!({"prompt": "x"}), None, &fx.root));
    assert!(fx.refused("mcp__mail__send_message", json!({"to": "a"}), None, &fx.root));
    assert!(!fx.refused("mcp__mail__send_message", json!({"to": "a", "note": "draft"}), None, &fx.root));
    assert!(!fx.refused("mcp__mail__other", json!({"to": "a"}), None, &fx.root));
}

// ------------------------------------------------------------ 2. bounded loops

fn polling_rule() -> Item {
    rule("poll-loop-needs-a-counter", None, &["until"], requires("until", &["-lt", "-le", "-ge", "-gt", "seq"]), &[])
}

#[test]
fn an_until_loop_in_a_monitor_call_is_bounded_by_its_deadline() {
    let fx = Fx::new(&[polling_rule()]);
    let wait = "until grep -q DONE run.log; do sleep 5; done; echo finished";
    assert!(fx.refused("Bash", json!({"command": wait}), None, &fx.root), "in Bash it can hang forever");
    assert!(
        !fx.refused("Monitor", json!({"command": wait, "description": "wait", "timeout_ms": 600000}), None, &fx.root),
        "Monitor kills the script at its timeout"
    );
    assert!(
        !fx.refused("Bash", json!({"command": "i=0; until [ $i -ge 9 ]; do i=$((i+1)); sleep 1; done"}), None, &fx.root),
        "a counted loop is still the way through in Bash"
    );
}

// ------------------------------------------------------------ 3. dry run and help

fn push_rules() -> Vec<Item> {
    vec![
        rule("no-push-without-go", None, &["git push"], requires("git push", &["go-owner"]), &[]),
        rule("no-push-all", None, &["git push --all", "git push --mirror"], forbidden(&["--all", "--mirror"]), &[]),
    ]
}

#[test]
fn a_pure_dry_run_of_a_push_is_not_refused() {
    let fx = Fx::new(&push_rules());
    assert!(!fx.bash("git push --dry-run origin main"));
    assert!(!fx.bash("git push origin main --dry-run 2>&1 | tail -1"));
    assert!(!fx.bash("git push -n origin main"));
    assert!(!fx.bash("git -C /tmp/x push --tags --dry-run origin"));
    assert!(!fx.bash("git push --all --dry-run origin"), "a forbidden literal is not refused in a dry run either");
    assert!(!fx.bash("git push --mirror --dry-run thor-probe-nonexistent-remote"));
}

#[test]
fn a_real_push_is_still_refused_whatever_else_the_line_says() {
    let fx = Fx::new(&push_rules());
    assert!(fx.bash("git push origin main"));
    assert!(fx.bash("git push origin main && echo --dry-run"), "the flag sits in another segment");
    assert!(fx.bash("git push origin main; git push --dry-run origin dev"), "the real push is a segment of its own");
    assert!(fx.bash("git push origin main -- --dry-run"), "after -- it is an operand");
    assert!(fx.bash("git push --all origin"));
    assert!(fx.bash("git push --no-dry-run origin main"));
    assert!(fx.bash("git commit -n -m x && git push origin main"));
    assert!(!fx.bash("git commit -n -m x"), "no rule is about a commit");
}

#[test]
fn dash_n_means_a_dry_run_for_push_only() {
    let fx = Fx::new(&[rule("no-commit-without-go", None, &["git commit"], requires("git commit", &["go-owner"]), &[])]);
    assert!(fx.bash("git commit -n -m x"), "git commit -n skips the hooks, it does not dry-run");
}

#[test]
fn what_if_is_a_dry_run_in_powershell_unless_it_is_switched_off() {
    let fx = Fx::new(&[rule("no-trigger-file", None, &["New-Item", "touch"], requires("New-Item", &["go-owner"]), &[])]);
    assert!(!fx.powershell("New-Item -WhatIf -ItemType File C:\\x\\rebuild-prod"));
    assert!(!fx.powershell("New-Item -ItemType File C:\\x\\rebuild-prod -whatif"));
    assert!(fx.powershell("New-Item -ItemType File C:\\x\\rebuild-prod"));
    assert!(fx.powershell("New-Item -WhatIf:$false -ItemType File C:\\x\\rebuild-prod"));
    assert!(fx.powershell("New-Item -ItemType File C:\\x\\a -WhatIf; New-Item -ItemType File C:\\x\\b"));
}

#[test]
fn a_help_request_is_not_refused_by_a_forbidden_literal_either() {
    let fx = Fx::new(&[rule("no-workflow-scope", None, &["gh auth refresh"], forbidden(&["workflow"]), &[])]);
    assert!(!fx.bash("gh auth refresh --help -s workflow 2>&1 | head -5"));
    assert!(fx.bash("gh auth refresh -s workflow"));
    assert!(fx.bash("gh auth refresh -s workflow && gh auth refresh --help"));
    assert!(fx.bash("gh auth refresh -s workflow -- --help"));
}

#[test]
fn a_version_flag_that_takes_a_value_is_not_a_version_request() {
    let fx = Fx::new(&[rule("installs-need-a-go", None, &["cargo install"], requires("cargo install", &["go-owner"]), &[])]);
    assert!(fx.bash("cargo install ripgrep --version 14.1.0"), "this installs");
    assert!(!fx.bash("cargo install --version"));
}

#[test]
fn an_owner_approval_rule_does_not_ask_for_the_owners_go_on_a_dry_run() {
    let fx = Fx::new(&[rule(
        "thor-push-needs-owner",
        None,
        &["git push"],
        requires("git push", &["go-owner"]),
        &[model::store::OWNER_APPROVAL_TAG],
    )]);
    let reason = fx.verdict("Bash", json!({"command": "git push origin main"}), None, &fx.root).expect("refused");
    assert!(reason.contains("explicit go"), "{reason}");
    assert!(fx.verdict("Bash", json!({"command": "git push origin main --dry-run"}), None, &fx.root).is_none());
}

// ------------------------------------------------------------ 4. folders

fn project_push_rule() -> Vec<Item> {
    vec![
        rule("a-push-needs-go", Some("proj-a"), &["git push"], requires("git push", &["go-owner"]), &[]),
        rule("b-has-a-rule-too", Some("proj-b"), &["git commit"], requires("git commit", &["go-owner"]), &[]),
        rule("a-publish-needs-go", Some("proj-a"), &["npm publish"], requires("npm publish", &["ship-it"]), &[]),
    ]
}

#[test]
fn a_repository_reached_through_a_variable_is_judged_under_its_own_project() {
    let fx = Fx::new(&project_push_rules_with_known_b());
    let a = fx.checkout("proj-a");
    let b = slashed(&fx.checkout("proj-b"));
    let sh = |c: &str| fx.refused("Bash", json!({"command": c}), Some("proj-a"), &a);
    let ps = |c: &str| fx.refused("PowerShell", json!({"command": c}), Some("proj-a"), &a);

    assert!(sh("git push origin main"), "its own project's rule refuses here");
    assert!(!sh(&format!("b={b}\ngit -C $b push origin main")));
    assert!(!sh(&format!("export b={b}; git -C \"$b\" push origin main")));
    assert!(!sh(&format!("b={b}; cd $b && git push origin main")));
    assert!(!sh(&format!("b={b}; git -C ${{b}} push origin main")));
    assert!(!ps(&format!("$b='{b}'\ngit -C $b push origin main 2>&1 | Select-Object -Last 2")));
    assert!(!ps(&format!("$b = '{b}'; git -C $b push origin main")));

    assert!(sh("git -C $nowhere push origin main"), "a variable nothing set stays unresolved");
    assert!(sh(&format!("b={b}; b=\"$b$(date)\"; git -C $b push origin main")), "a value that is itself unresolved stays unresolved");
    assert!(sh(&format!("(b={b}); git -C $b push origin main")), "an assignment in a subshell does not hold after it");
    assert!(sh(&format!("true && b={b}; git -C $b push origin main")), "a conditional assignment may not have run");
    assert!(sh(&format!("b={b}; b=\"$HOME\"; git -C $b push origin main")), "the last assignment wins");
}

fn project_push_rules_with_known_b() -> Vec<Item> {
    project_push_rule()
}

#[test]
fn a_repository_of_no_known_project_is_not_this_project_for_git_only() {
    let fx = Fx::new(&project_push_rule());
    let a = fx.checkout("proj-a");
    let scratch = fx.root.join("scratch-repo");
    std::fs::create_dir_all(scratch.join(".git")).unwrap();
    let scratch = slashed(&scratch);
    let sh = |c: &str| fx.refused("Bash", json!({"command": c}), Some("proj-a"), &a);

    assert!(!sh(&format!("git -C {scratch} push origin main")));
    assert!(!sh(&format!("cd {scratch} && git push origin main")));
    assert!(sh(&format!("cd {scratch} && npm publish")), "a program that is not git keeps the session's project");
    assert!(sh("git push origin main"), "the session's own repository is refused");
    let plain = slashed(&fx.root.join("plain-folder"));
    std::fs::create_dir_all(&plain).unwrap();
    assert!(sh(&format!("cd {plain} && git push origin main")), "a folder in no repository at all keeps the session's project");
}

#[test]
fn a_repository_the_call_makes_itself_is_scratch() {
    let fx = Fx::new(&project_push_rule());
    let a = fx.checkout("proj-a");
    let tmp = slashed(&fx.root.join("fresh"));
    let sh = |c: &str| fx.refused("Bash", json!({"command": c}), Some("proj-a"), &a);

    assert!(!sh(&format!(
        "cd {tmp} && git init -q --bare r.git && git clone -q r.git w; cd w && git commit -q --allow-empty -m i && git push -q -u origin main"
    )));
    assert!(!sh(&format!("mkdir -p {tmp} && cd {tmp} && git init -q w && cd w && git push origin main")));
    assert!(sh(&format!("cd {tmp} && git clone -q https://example.com/x/w.git w; cd w && git push origin main")), "a clone of a remote pushes to that remote");
    assert!(sh(&format!("cd {tmp} && git clone -q git@example.com:x/w.git w; cd w && git push origin main")));
    assert!(sh("git init -q . && git push origin main"), "a git init of the folder that exists makes nothing");
    assert!(sh(&format!("git init -q {} && git push origin main", slashed(&a))), "nor does one of the session's own checkout");
    assert!(sh(&format!("cd {tmp}; git init -q w && git -C ../elsewhere push origin main")), "only the made folder is scratch");
}

#[test]
fn the_probe_that_builds_its_own_hook_repository_under_tmp_is_scratch() {
    let fx = Fx::new(&project_push_rule());
    let a = fx.checkout("proj-a");
    let call = "cd /tmp && rm -rf hk && mkdir hk && cd hk && git init -q --bare r.git && git clone -q r.git w 2>&1 | tail -1; cd w && git checkout -q -b main && git config user.email a@b.c && git commit -q --allow-empty -m i && git push -q -u origin main 2>&1 | tail -1
head -c 200000 /dev/zero | tr '\0' x > big.bin; git add big.bin; git commit -q -m big; git push origin main 2>&1 | tail -5";
    assert!(!fx.refused("Bash", json!({"command": call}), Some("proj-a"), &a));
}

// ------------------------------------------------------------ 5. exported variables

fn install_rule() -> Item {
    rule("install-needs-db-or-sandbox", None, &["install.exe"], requires("install.exe", &["--db", "USERPROFILE="]), &[])
}

#[test]
fn an_exported_sandbox_variable_answers_the_install_rule() {
    let fx = Fx::new(&[install_rule()]);
    assert!(fx.bash("install.exe"));
    assert!(!fx.bash("install.exe --db C:\\x\\thor.db"));
    assert!(!fx.bash("USERPROFILE=/tmp/h install.exe"), "the inline form always answered");
    assert!(!fx.bash("export HOME=/tmp/h USERPROFILE=/tmp/h; install.exe"));
    assert!(!fx.bash("export HOME=/tmp/h\nexport USERPROFILE=/tmp/h\ncd /tmp/repo && install.exe --verbose"));
    assert!(!fx.bash("USERPROFILE=/tmp/h; install.exe"));
    assert!(!fx.powershell("$env:USERPROFILE = 'C:\\sandbox'; install.exe"));
    assert!(!fx.powershell("$env:USERPROFILE='C:\\sandbox'\n.\\install.exe"));
}

#[test]
fn only_an_assignment_that_certainly_ran_in_this_shell_answers() {
    let fx = Fx::new(&[install_rule()]);
    assert!(fx.bash("export HOME=/tmp/h; install.exe"), "another variable answers nothing");
    assert!(fx.bash("(export USERPROFILE=/tmp/h); install.exe"), "a subshell's export ends with it");
    assert!(fx.bash("true && export USERPROFILE=/tmp/h; install.exe"), "it may not have run");
    assert!(fx.bash("bash -c 'export USERPROFILE=/tmp/h'; install.exe"));
    assert!(fx.bash("echo hi | export USERPROFILE=/tmp/h; install.exe"));
    assert!(fx.bash("export USERPROFILE=/tmp/h &\ninstall.exe"));
    assert!(fx.bash("install.exe; export USERPROFILE=/tmp/h"), "an export after the call answers nothing");
    assert!(fx.bash("echo USERPROFILE=x; install.exe"), "text that mentions it is no assignment");
}
