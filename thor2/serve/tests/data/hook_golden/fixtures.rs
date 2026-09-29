//! The fixture writer for `hook_and_trial_share_one_pipeline.rs`: the stores,
//! the working folders and the PreToolUse payloads the golden hook outputs in
//! this folder were recorded from.
//!
//! HOW THE GOLDEN FILES WERE MADE. The `serve.exe` built from the tree BEFORE
//! the PreToolUse decisions moved into `serve::guard_verdict` was run as
//! `serve --db <store> hook --no-record < <payload file>` over every case
//! below, on stores and folders this module built, and each run's exit code
//! and stdout were saved as `<fixture>.<case>.golden` through [`golden_text`].
//! Those files are the oracle: the test runs today's binary over the same
//! cases and compares byte for byte. A comparison of the moved code with
//! itself would prove nothing, so the files are never regenerated from the
//! build under test. A change in what the hook says is a deliberate edit of
//! the one golden file it changes, visible in the diff.
//!
//! Everything here is plain ASCII with neutral names, and nothing depends on
//! the clock or on the machine: the only machine-specific text in an output is
//! the temporary folder and the binary's own path, and [`normalize`] replaces
//! both with a fixed word before anything is compared or saved.

#![allow(dead_code)]

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use model::item::{Binding, Check, Item, Kind, Severity, TargetKind};

/// The session every payload names. A held (`--no-record`) hook call keeps no
/// marker, so one session for every case changes nothing between them.
pub const SESSION: &str = "hook-golden";

/// One PreToolUse call and the fixture it runs against.
pub struct Case {
    pub fixture: &'static str,
    pub name: &'static str,
    pub payload: Value,
}

/// One store and the cases that run against it.
pub struct Fixture {
    pub name: &'static str,
    pub db: PathBuf,
    pub cases: Vec<Case>,
}

impl Case {
    /// The golden file this case is compared with.
    pub fn file_name(&self) -> String {
        format!("{}.{}.golden", self.fixture, self.name)
    }
}

fn rule(id: &str, text: &str, bindings: Vec<Binding>, severity: Option<Severity>, project: Option<&str>, check: Check) -> Item {
    Item {
        id: id.to_string(),
        kind: Kind::Rule,
        text: text.to_string(),
        bindings,
        severity,
        project: project.map(str::to_string),
        tags: vec![],
        expires: None,
        key: None,
        falsifier: Some(format!("the fixture rule {id} turns out to be wrong")),
        check: Some(check),
    }
}

fn target(kind: TargetKind, value: &str) -> Binding {
    Binding::Target { kind, value: value.to_string() }
}

fn contains(path: &str, literal: &str) -> Check {
    Check::Contains { path: path.to_string(), literal: literal.to_string() }
}

fn forbidden(literals: &[&str]) -> Check {
    Check::Forbidden { literals: literals.iter().map(|l| l.to_string()).collect() }
}

fn requires(when: &str, required: &[&str]) -> Check {
    Check::Requires { when: when.to_string(), required: required.iter().map(|r| r.to_string()).collect() }
}

/// A working folder of project `project`: a `.thor-project` marker (so no git
/// is needed to resolve it) and the files named, relative to the folder.
fn folder(base: &Path, fixture: &str, project: &str, files: &[(&str, &str)]) -> PathBuf {
    let dir = base.join(fixture).join(project);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(".thor-project"), format!("{project}\n")).unwrap();
    for (name, text) in files {
        let path = dir.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    dir
}

/// A store beside the folders, holding `items`, each through the real write
/// gate.
fn store(base: &Path, fixture: &str, items: &[Item]) -> PathBuf {
    let dir = base.join(fixture).join("store");
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("thor.db");
    let mut store = thor_core::event_store::EventStore::new(&db).unwrap();
    // Written raw - the exact body the write gate wrote when these goldens
    // were recorded (the project normalised, the canonical body) - because
    // since step 8 the gate refuses a check whose literal the text does not
    // name (A4), and several fixtures here were written before that ground:
    // the oracle is about the hook, never about the gate.
    for item in items {
        let mut item = item.clone();
        item.project = model::normalize::normalize_project(item.project.as_deref());
        let body = model::store::canonical_body(&item).unwrap();
        store
            .append_event("fixture", "fixture", "golden-fixture", thor_core::event_store::EventKind::FactCreated, &item.id, None, &body)
            .unwrap_or_else(|e| panic!("fixture {fixture} item {}: {e:?}", item.id));
    }
    db
}

fn call(cwd: &Path, tool_name: &str, tool_input: Value) -> Value {
    json!({
        "hook_event_name": "PreToolUse",
        "session_id": SESSION,
        "cwd": cwd.to_string_lossy(),
        "tool_name": tool_name,
        "tool_input": tool_input,
    })
}

fn bash(cwd: &Path, command: &str) -> Value {
    call(cwd, "Bash", json!({ "command": command }))
}

/// `file` (written with `/`) under `cwd`, with this platform's own separator
/// throughout - the absolute path a real Write or Edit payload carries.
fn at(cwd: &Path, file: &str) -> String {
    file.split('/').fold(cwd.to_path_buf(), |path, part| path.join(part)).to_string_lossy().to_string()
}

fn write(cwd: &Path, file: &str, content: &str) -> Value {
    call(cwd, "Write", json!({ "file_path": at(cwd, file), "content": content }))
}

fn edit(cwd: &Path, file: &str, old: &str, new: &str) -> Value {
    call(cwd, "Edit", json!({ "file_path": at(cwd, file), "old_string": old, "new_string": new }))
}

fn edit_all(cwd: &Path, file: &str, old: &str, new: &str) -> Value {
    call(cwd, "Edit", json!({ "file_path": at(cwd, file), "old_string": old, "new_string": new, "replace_all": true }))
}

fn case(fixture: &'static str, name: &'static str, payload: Value) -> Case {
    Case { fixture, name, payload }
}

/// Build every fixture under `base` (an empty folder the caller owns).
pub fn build_all(base: &Path) -> Vec<Fixture> {
    vec![
        contains_path(base),
        contains_command(base),
        contains_dir(base),
        contains_twice(base),
        forbidden_command(base),
        forbidden_always(base),
        absent(base),
        absent_all(base),
        requires_plain(base),
        requires_inner(base),
        location(base),
        hook_order(base),
        file_arm_order(base),
    ]
}

/// A `contains` check bound to the file it names.
fn contains_path(base: &Path) -> Fixture {
    const F: &str = "contains-path";
    let alpha = folder(base, F, "alpha", &[("NOTES.md", "hello world\nkeep this line\n"), ("README.md", "readme\n")]);
    let db = store(
        base,
        F,
        &[rule(
            "c-path",
            "NOTES.md must keep the line keep this line",
            vec![target(TargetKind::Path, "NOTES.md")],
            Some(Severity::Irreversible),
            Some("alpha"),
            contains("NOTES.md", "keep this line"),
        )],
    );
    let cases = vec![
        case(F, "edit-removes-the-line", edit(&alpha, "NOTES.md", "keep this line\n", "")),
        case(F, "edit-keeps-the-line", edit(&alpha, "NOTES.md", "keep this line\n", "keep this line\n\n")),
        case(F, "write-without-the-line", write(&alpha, "NOTES.md", "hello world\n")),
        case(F, "write-with-the-line", write(&alpha, "NOTES.md", "keep this line\n")),
        case(F, "bash-rm", bash(&alpha, "rm NOTES.md")),
        case(F, "bash-truncate", bash(&alpha, "truncate -s 0 NOTES.md")),
        case(F, "bash-redirect", bash(&alpha, "echo x > NOTES.md")),
        case(F, "edit-another-file", edit(&alpha, "README.md", "readme", "read me")),
    ];
    Fixture { name: F, db, cases }
}

/// A `contains` check on an item bound to a command, not to the file.
fn contains_command(base: &Path) -> Fixture {
    const F: &str = "contains-command";
    let alpha = folder(base, F, "alpha", &[("CHANGELOG.md", "# Changelog\n\n## Unreleased\n")]);
    let db = store(
        base,
        F,
        &[rule(
            "c-cmd",
            "a push needs an Unreleased section in CHANGELOG.md",
            vec![target(TargetKind::Command, "git push")],
            Some(Severity::Costly),
            Some("alpha"),
            contains("CHANGELOG.md", "## Unreleased"),
        )],
    );
    let cases = vec![
        case(F, "bash-push", bash(&alpha, "git push origin main")),
        case(F, "edit-drops-the-section", edit(&alpha, "CHANGELOG.md", "## Unreleased\n", "")),
        case(F, "bash-rm-changelog", bash(&alpha, "rm CHANGELOG.md")),
    ];
    Fixture { name: F, db, cases }
}

/// A `contains` check whose path is a directory: each direct child that holds
/// the literal must keep it.
fn contains_dir(base: &Path) -> Fixture {
    const F: &str = "contains-dir";
    let header = "SPDX-License-Identifier: GPL-3.0\n";
    let alpha = folder(
        base,
        F,
        "alpha",
        &[("docs/a.md", "SPDX-License-Identifier: GPL-3.0\nbody\n"), ("docs/b.md", "no header\n"), ("docs/sub/c.md", header)],
    );
    let db = store(
        base,
        F,
        &[rule(
            "c-dir",
            "every file in docs keeps its SPDX line",
            vec![target(TargetKind::Dir, "docs")],
            Some(Severity::Costly),
            Some("alpha"),
            contains("docs", "SPDX-License-Identifier"),
        )],
    );
    let cases = vec![
        case(F, "edit-a-drops-the-header", edit(&alpha, "docs/a.md", header, "")),
        case(F, "edit-b-that-never-had-it", edit(&alpha, "docs/b.md", "no header", "still none")),
        case(F, "edit-nested-file", edit(&alpha, "docs/sub/c.md", header, "")),
        case(F, "write-a-keeps-the-header", write(&alpha, "docs/a.md", "SPDX-License-Identifier: GPL-3.0\nnew body\n")),
        case(F, "bash-rm-a", bash(&alpha, "rm docs/a.md")),
    ];
    Fixture { name: F, db, cases }
}

/// A `contains` literal that occurs twice in its file.
fn contains_twice(base: &Path) -> Fixture {
    const F: &str = "contains-twice";
    let alpha = folder(base, F, "alpha", &[("NOTES.md", "keep one\nkeep two\n")]);
    let db = store(
        base,
        F,
        &[rule(
            "c-twice",
            "NOTES.md keeps the word keep",
            vec![target(TargetKind::Path, "NOTES.md")],
            Some(Severity::HouseStyle),
            Some("alpha"),
            contains("NOTES.md", "keep"),
        )],
    );
    let cases = vec![
        case(F, "edit-first-occurrence-only", edit(&alpha, "NOTES.md", "keep one", "one")),
        case(F, "edit-every-occurrence", edit_all(&alpha, "NOTES.md", "keep", "held")),
        case(F, "write-without-it", write(&alpha, "NOTES.md", "one\ntwo\n")),
        case(F, "edit-that-would-not-apply", edit(&alpha, "NOTES.md", "not in the file", "")),
    ];
    Fixture { name: F, db, cases }
}

/// A `forbidden` check on a command binding, project-scoped and global.
fn forbidden_command(base: &Path) -> Fixture {
    const F: &str = "forbidden-command";
    let alpha = folder(base, F, "alpha", &[]);
    let beta = folder(base, F, "beta", &[]);
    let db = store(
        base,
        F,
        &[
            rule(
                "f-push",
                "never force-push in alpha",
                vec![target(TargetKind::Command, "git push")],
                Some(Severity::Irreversible),
                Some("alpha"),
                forbidden(&["--force"]),
            ),
            rule(
                "f-visibility",
                "never make a repository public",
                vec![target(TargetKind::Command, "gh repo edit")],
                Some(Severity::Irreversible),
                None,
                forbidden(&["--visibility public"]),
            ),
        ],
    );
    let cases = vec![
        case(F, "bash-force-push", bash(&alpha, "git push --force origin main")),
        case(F, "bash-plain-push", bash(&alpha, "git push origin main")),
        case(F, "bash-force-with-lease", bash(&alpha, "git push --force-with-lease origin main")),
        case(F, "bash-force-push-in-beta", bash(&beta, "git push --force origin main")),
        case(F, "powershell-force-push", call(&alpha, "PowerShell", json!({ "command": "git push --force origin main" }))),
        case(F, "bash-visibility-in-beta", bash(&beta, "gh repo edit --visibility public")),
        case(F, "bash-wrapped", bash(&alpha, "bash -c \"git push --force origin main\"")),
        case(F, "bash-after-cd", bash(&alpha, "cd sub && git push --force origin main")),
        case(F, "bash-echo-mention", bash(&alpha, "echo git push --force")),
    ];
    Fixture { name: F, db, cases }
}

/// A `forbidden` check on an Always binding: every write to every file.
fn forbidden_always(base: &Path) -> Fixture {
    const F: &str = "forbidden-always";
    let alpha = folder(base, F, "alpha", &[("notes.txt", "x\n")]);
    let beta = folder(base, F, "beta", &[]);
    let db = store(
        base,
        F,
        &[
            rule(
                "f-marker",
                "no file ever carries DO-NOT-SHIP",
                vec![Binding::Always],
                Some(Severity::HouseStyle),
                None,
                forbidden(&["DO-NOT-SHIP"]),
            ),
            rule(
                "f-alpha-only",
                "alpha never writes ALPHA-SECRET",
                vec![Binding::Always],
                Some(Severity::Costly),
                Some("alpha"),
                forbidden(&["ALPHA-SECRET"]),
            ),
        ],
    );
    let cases = vec![
        case(F, "write-the-marker", write(&alpha, "notes.txt", "a DO-NOT-SHIP b")),
        case(F, "write-clean", write(&alpha, "notes.txt", "a - b")),
        case(F, "edit-adds-the-marker", edit(&alpha, "notes.txt", "x", "DO-NOT-SHIP")),
        case(F, "write-alpha-literal-in-alpha", write(&alpha, "notes.txt", "ALPHA-SECRET")),
        case(F, "write-alpha-literal-in-beta", write(&beta, "notes.txt", "ALPHA-SECRET")),
        case(F, "bash-echo-the-marker", bash(&alpha, "echo DO-NOT-SHIP")),
    ];
    Fixture { name: F, db, cases }
}

/// `absent` checks: one on a file binding, one on a command binding, one
/// whose anchor file is gone.
fn absent(base: &Path) -> Fixture {
    const F: &str = "absent";
    let alpha = folder(base, F, "alpha", &[("NOTES.md", "# notes\n"), ("deploy.sh", "#!/bin/sh\n")]);
    let db = store(
        base,
        F,
        &[
            rule(
                "a-notes",
                "NOTES.md never carries FIXME",
                vec![target(TargetKind::Path, "NOTES.md")],
                Some(Severity::HouseStyle),
                Some("alpha"),
                Check::Absent { path: "NOTES.md".to_string(), literal: "FIXME".to_string() },
            ),
            rule(
                "a-deploy",
                "deploy.sh is never run with --prod from here",
                vec![target(TargetKind::Command, "./deploy.sh")],
                Some(Severity::Costly),
                Some("alpha"),
                Check::Absent { path: "deploy.sh".to_string(), literal: "--prod".to_string() },
            ),
            rule(
                "a-gone",
                "GONE.md never carries FIXME",
                vec![target(TargetKind::Path, "GONE.md")],
                Some(Severity::HouseStyle),
                Some("alpha"),
                Check::Absent { path: "GONE.md".to_string(), literal: "FIXME".to_string() },
            ),
        ],
    );
    let cases = vec![
        case(F, "write-fixme", write(&alpha, "NOTES.md", "FIXME later")),
        case(F, "edit-adds-fixme", edit(&alpha, "NOTES.md", "# notes", "# notes FIXME")),
        case(F, "write-fixme-elsewhere", write(&alpha, "OTHER.md", "FIXME")),
        case(F, "write-clean", write(&alpha, "NOTES.md", "# notes\nclean\n")),
        case(F, "bash-deploy-prod", bash(&alpha, "./deploy.sh --prod")),
        case(F, "bash-deploy-staging", bash(&alpha, "./deploy.sh --staging")),
        case(F, "write-fixme-where-the-anchor-is-gone", write(&alpha, "GONE.md", "FIXME")),
    ];
    Fixture { name: F, db, cases }
}

/// `absent_all` checks: a set of literals, on a file and on a command.
fn absent_all(base: &Path) -> Fixture {
    const F: &str = "absent-all";
    let alpha = folder(base, F, "alpha", &[("CONFIG.md", "# config\n")]);
    let db = store(
        base,
        F,
        &[
            rule(
                "aa-config",
                "CONFIG.md never holds a secret",
                vec![target(TargetKind::Path, "CONFIG.md")],
                Some(Severity::Irreversible),
                Some("alpha"),
                Check::AbsentAll { path: "CONFIG.md".to_string(), literals: vec!["password=".to_string(), "token=".to_string()] },
            ),
            rule(
                "aa-curl",
                "curl never skips TLS checks here",
                vec![target(TargetKind::Command, "curl")],
                Some(Severity::Costly),
                Some("alpha"),
                Check::AbsentAll { path: "CONFIG.md".to_string(), literals: vec!["--insecure".to_string(), " -k ".to_string()] },
            ),
        ],
    );
    let cases = vec![
        case(F, "write-token", write(&alpha, "CONFIG.md", "token=abc")),
        case(F, "write-password", write(&alpha, "CONFIG.md", "password=abc")),
        case(F, "write-clean", write(&alpha, "CONFIG.md", "user=abc")),
        case(F, "bash-curl-insecure", bash(&alpha, "curl --insecure https://10.0.0.5/health")),
        case(F, "bash-curl-short-flag", bash(&alpha, "curl -k https://10.0.0.5/health")),
        case(F, "bash-curl-plain", bash(&alpha, "curl https://10.0.0.5/health")),
    ];
    Fixture { name: F, db, cases }
}

/// `requires` checks whose trigger is the binding itself.
fn requires_plain(base: &Path) -> Fixture {
    const F: &str = "requires-plain";
    let alpha = folder(base, F, "alpha", &[]);
    let beta = folder(base, F, "beta", &[]);
    let db = store(
        base,
        F,
        &[
            rule(
                "r-agent",
                "every Agent call names its model",
                vec![target(TargetKind::Command, "Agent")],
                Some(Severity::Costly),
                None,
                requires("Agent", &["sonnet", "haiku", "opus"]),
            ),
            rule(
                "r-pip",
                "pip install in alpha always carries --no-deps",
                vec![target(TargetKind::Command, "pip install")],
                Some(Severity::Costly),
                Some("alpha"),
                requires("pip install", &["--no-deps"]),
            ),
        ],
    );
    let cases = vec![
        case(F, "agent-without-a-model", call(&alpha, "Agent", json!({ "description": "look", "prompt": "find the thing" }))),
        case(
            F,
            "agent-with-a-model",
            call(&alpha, "Agent", json!({ "description": "look", "prompt": "find the thing", "model": "sonnet" })),
        ),
        case(F, "bash-pip", bash(&alpha, "pip install requests")),
        case(F, "bash-pip-no-deps", bash(&alpha, "pip install --no-deps requests")),
        case(F, "bash-pip-in-beta", bash(&beta, "pip install requests")),
        case(F, "bash-pip-help", bash(&alpha, "pip install --help")),
    ];
    Fixture { name: F, db, cases }
}

/// A `requires` check whose trigger is a literal inside the bound command.
fn requires_inner(base: &Path) -> Fixture {
    const F: &str = "requires-inner";
    let alpha = folder(base, F, "alpha", &[]);
    let db = store(
        base,
        F,
        &[rule(
            "r-inner",
            "a dev docker compose over ssh must carry -f docker-compose.dev.yml",
            vec![target(TargetKind::Command, "ssh")],
            None,
            None,
            requires("-p acme-dev", &["-f docker-compose.dev.yml"]),
        )],
    );
    let cases = vec![
        case(F, "dev-without-the-file", bash(&alpha, "ssh admin@nas \"cd /srv/app && sudo docker compose -p acme-dev up -d\"")),
        case(
            F,
            "dev-with-the-file",
            bash(&alpha, "ssh admin@nas \"cd /srv/app && sudo docker compose -f docker-compose.dev.yml -p acme-dev up -d\""),
        ),
        case(F, "a-longer-project-name", bash(&alpha, "ssh admin@nas \"sudo docker compose -p acme-dev-old logs\"")),
        case(F, "plain-ssh", bash(&alpha, "ssh admin@nas \"sudo docker ps\"")),
    ];
    Fixture { name: F, db, cases }
}

/// A location prohibition: nothing under a directory is written at all.
fn location(base: &Path) -> Fixture {
    const F: &str = "location";
    let alpha = folder(base, F, "alpha", &[("vendor/lock.json", "{}\n"), ("src/main.txt", "main\n")]);
    let db = store(
        base,
        F,
        &[rule(
            "l-vendor",
            "nothing under vendor is edited by hand",
            vec![target(TargetKind::Dir, "vendor")],
            Some(Severity::Irreversible),
            Some("alpha"),
            Check::PathExists { path: "vendor".to_string() },
        )],
    );
    let cases = vec![
        case(F, "write-into-vendor", write(&alpha, "vendor/lock.json", "{\"a\":1}\n")),
        case(F, "bash-rm-in-vendor", bash(&alpha, "rm vendor/lock.json")),
        case(F, "bash-redirect-into-vendor", bash(&alpha, "echo x > vendor/lock.json")),
        case(F, "write-elsewhere", write(&alpha, "src/main.txt", "main two\n")),
    ];
    Fixture { name: F, db, cases }
}

/// Several arms that would all refuse one call: which one speaks is the hook
/// order (file write, then shell write, then command literal, then requires).
fn hook_order(base: &Path) -> Fixture {
    const F: &str = "hook-order";
    let alpha = folder(base, F, "alpha", &[("NOTES.md", "keep this line\n")]);
    let db = store(
        base,
        F,
        &[
            rule(
                "o-keep",
                "NOTES.md must keep the line keep this line",
                vec![target(TargetKind::Path, "NOTES.md")],
                Some(Severity::Irreversible),
                Some("alpha"),
                contains("NOTES.md", "keep this line"),
            ),
            rule(
                "o-force",
                "rm never runs with --force in alpha",
                vec![target(TargetKind::Command, "rm")],
                Some(Severity::Costly),
                Some("alpha"),
                forbidden(&["--force"]),
            ),
            rule(
                "o-interactive",
                "rm in alpha always asks first with -i",
                vec![target(TargetKind::Command, "rm")],
                Some(Severity::Costly),
                Some("alpha"),
                requires("rm", &["-i"]),
            ),
            rule(
                "o-header",
                "every Write in alpha carries the header line",
                vec![target(TargetKind::Command, "Write")],
                Some(Severity::HouseStyle),
                Some("alpha"),
                requires("Write", &["# header"]),
            ),
        ],
    );
    let cases = vec![
        case(F, "rm-force-the-kept-file", bash(&alpha, "rm --force NOTES.md")),
        case(F, "rm-force-another-file", bash(&alpha, "rm --force other.txt")),
        case(F, "rm-another-file", bash(&alpha, "rm other.txt")),
        case(F, "rm-i-another-file", bash(&alpha, "rm -i other.txt")),
        case(F, "write-drops-the-kept-line", write(&alpha, "NOTES.md", "x\n")),
        case(F, "write-another-file-without-header", write(&alpha, "other.txt", "x\n")),
        case(F, "write-another-file-with-header", write(&alpha, "other.txt", "# header\nx\n")),
    ];
    Fixture { name: F, db, cases }
}

/// The file guard's own five arms against one write: location, then the
/// ranked content pool, then content under a directory binding, then an
/// Always-bound forbidden text, then a literal that must remain.
fn file_arm_order(base: &Path) -> Fixture {
    const F: &str = "file-arm-order";
    let alpha = folder(
        base,
        F,
        "alpha",
        &[("NOTES.md", "keep this line\n"), ("vendor/lock.json", "{}\n"), ("docs/a.md", "intro\n"), ("docs/b.md", "more\n")],
    );
    let db = store(
        base,
        F,
        &[
            rule(
                "w-vendor",
                "nothing under vendor is edited by hand",
                vec![target(TargetKind::Dir, "vendor")],
                Some(Severity::Irreversible),
                Some("alpha"),
                Check::PathExists { path: "vendor".to_string() },
            ),
            rule(
                "w-vendor-secret",
                "vendor/lock.json never carries SECRET",
                vec![target(TargetKind::Path, "vendor/lock.json")],
                Some(Severity::Costly),
                Some("alpha"),
                Check::Absent { path: "vendor/lock.json".to_string(), literal: "SECRET".to_string() },
            ),
            rule(
                "w-keep",
                "NOTES.md must keep the line keep this line",
                vec![target(TargetKind::Path, "NOTES.md")],
                Some(Severity::Irreversible),
                Some("alpha"),
                contains("NOTES.md", "keep this line"),
            ),
            rule(
                "w-notes-fixme",
                "NOTES.md never carries FIXME",
                vec![target(TargetKind::Path, "NOTES.md")],
                Some(Severity::HouseStyle),
                Some("alpha"),
                Check::Absent { path: "NOTES.md".to_string(), literal: "FIXME".to_string() },
            ),
            rule(
                "w-docs-todo",
                "nothing in docs carries TODO",
                vec![target(TargetKind::Dir, "docs")],
                Some(Severity::HouseStyle),
                Some("alpha"),
                Check::Absent { path: "docs".to_string(), literal: "TODO".to_string() },
            ),
            rule(
                "w-banned",
                "no file ever carries BANNED-WORD or FIXME",
                vec![Binding::Always],
                Some(Severity::HouseStyle),
                None,
                forbidden(&["BANNED-WORD", "FIXME"]),
            ),
        ],
    );
    let cases = vec![
        case(F, "location-before-content", write(&alpha, "vendor/lock.json", "SECRET\n")),
        case(F, "content-before-always-and-remain", write(&alpha, "NOTES.md", "FIXME\n")),
        case(F, "always-before-remain", write(&alpha, "NOTES.md", "BANNED-WORD\n")),
        case(F, "remain-alone", write(&alpha, "NOTES.md", "x\n")),
        case(F, "dir-content-before-always", write(&alpha, "docs/a.md", "TODO FIXME\n")),
        case(F, "dir-content-alone", write(&alpha, "docs/b.md", "TODO\n")),
        case(F, "clean-write", write(&alpha, "docs/a.md", "fine\n")),
    ];
    Fixture { name: F, db, cases }
}

/// Run `exe hook --no-record` over `case`, the payload written to a file in
/// `payloads` and fed as stdin. Returns (exit code, stdout, stderr).
pub fn run_hook(exe: &Path, db: &Path, payloads: &Path, case: &Case) -> (i32, String, String) {
    std::fs::create_dir_all(payloads).unwrap();
    let payload_file = payloads.join(format!("{}.{}.json", case.fixture, case.name));
    std::fs::write(&payload_file, case.payload.to_string()).unwrap();
    let stdin = std::fs::File::open(&payload_file).unwrap();
    let out = Command::new(exe)
        .arg("--db")
        .arg(db)
        .arg("hook")
        .arg("--no-record")
        .stdin(Stdio::from(stdin))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap_or_else(|e| panic!("could not run {}: {e}", exe.display()));
    let code = out.status.code().unwrap_or(-1);
    (code, String::from_utf8(out.stdout).unwrap(), String::from_utf8(out.stderr).unwrap())
}

/// Every spelling a path can take in the hook's output: as given, with
/// forward slashes, lower-cased, and each of those escaped for JSON.
fn spellings(path: &Path) -> Vec<String> {
    let raw = path.to_string_lossy().to_string();
    let forward = raw.replace('\\', "/");
    let mut out = Vec::new();
    for form in [raw.clone(), forward.clone(), raw.to_lowercase(), forward.to_lowercase()] {
        out.push(form.replace('\\', "\\\\"));
        out.push(form);
    }
    // Longest first, so an escaped spelling is replaced before the plain one
    // it contains.
    out.sort_by_key(|s| std::cmp::Reverse(s.len()));
    out.dedup();
    out
}

/// The hook's stdout with the temporary folder replaced by `<BASE>` and the
/// binary's own path by `<SERVE>`. Panics when the folder's unique name is
/// still in the text afterwards: a spelling this does not know would
/// otherwise make the comparison depend on the machine.
pub fn normalize(stdout: &str, base: &Path, exe: &Path) -> String {
    let mut text = stdout.to_string();
    for spelling in spellings(exe) {
        text = text.replace(&spelling, "<SERVE>");
    }
    for spelling in spellings(base) {
        text = text.replace(&spelling, "<BASE>");
    }
    let unique = base.file_name().unwrap().to_string_lossy().to_lowercase();
    assert!(
        !text.to_lowercase().contains(&unique),
        "the output still names the temporary folder {unique} in a spelling normalize does not know: {text}"
    );
    separators_after_base(&text)
}

/// The path that follows each `<BASE>` with `/` for every separator, so a
/// golden file recorded on Windows compares equal on Linux. The hook's stdout
/// is JSON, where a Windows separator is an escaped backslash (two
/// characters); a lone backslash (the start of any other escape, such as a
/// quote) ends the path and is kept as it is.
fn separators_after_base(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("<BASE>") {
        out.push_str(&rest[..at + "<BASE>".len()]);
        rest = &rest[at + "<BASE>".len()..];
        loop {
            if let Some(tail) = rest.strip_prefix("\\\\") {
                out.push('/');
                rest = tail;
                continue;
            }
            match rest.chars().next() {
                Some(c) if c == '/' || c == '.' || c == '-' || c == '_' || c.is_ascii_alphanumeric() => {
                    out.push(c);
                    rest = &rest[c.len_utf8()..];
                }
                _ => break,
            }
        }
    }
    out.push_str(rest);
    out
}

/// What a golden file holds: the exit code on the first line, then the
/// normalized stdout exactly as printed.
pub fn golden_text(exit: i32, normalized_stdout: &str) -> String {
    format!("exit {exit}\n{normalized_stdout}")
}
