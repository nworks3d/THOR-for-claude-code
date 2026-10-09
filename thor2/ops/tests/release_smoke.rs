//! Fresh-install smoke test: run what a NEW USER runs, on the programs that
//! would be shipped, in a throwaway environment, and fail loudly.
//!
//! WHY THIS EXISTS. Every other test in this tree runs the code from a
//! checkout. Nothing ever ran the packaged release the way a stranger does:
//! unpack the programs into a folder, run `install`, restart, expect the
//! memory to speak. A bundle built without `--features semantic` still
//! starts, still answers literal queries, and silently returns nothing for
//! meaning search. A hook pointing at a binary that is missing fails open,
//! with no error anywhere. Those failures are only visible by doing the
//! whole walk from AGENTS.md (steps 1-6) against the finished files.
//!
//! IGNORED BY DEFAULT: it needs the whole set of built programs, so a plain
//! `cargo test` stays fast. Run it explicitly:
//!
//!   cargo build --release --workspace --features semantic
//!   cargo test --release --features semantic -p ops --test release_smoke \
//!       -- --ignored --nocapture
//!
//! and in the release workflow against the packaged archive, unpacked:
//!
//!   THOR_SMOKE_BIN_DIR=<unpacked folder> THOR_SMOKE_SEMANTIC=yes|no cargo test ...
//!
//! THOR_SMOKE_BIN_DIR   folder holding the programs. Default: the folder this
//!                      test's own `install` was built into (target/release).
//!                      A plain `cargo test` rebuilds some programs there
//!                      without meaning search, and this test then fails on
//!                      purpose: build the workspace again before running it.
//! THOR_SMOKE_SEMANTIC  `yes` (default) or `no`: whether the programs under
//!                      test are meant to carry meaning search. The plain
//!                      server asset is `no`, the two client assets `yes`.
//!
//! SAFETY. Everything a new user's `install` writes is redirected into one
//! temp folder: HOME, USERPROFILE, LOCALAPPDATA, APPDATA and the XDG folders
//! point there, GIT_CONFIG_GLOBAL is a file there (install wires a machine
//! wide `core.hooksPath` with `git config --global`), and `--settings`,
//! `--mcp-json` and `--db` are named explicitly. The real settings, the real
//! store and the real git configuration are never opened.
//!
//! OUTPUT: one `PASS`/`FAIL` line per step, then a panic (non-zero exit) if
//! any step failed.

use serde_json::{json, Value};
use std::cell::RefCell;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// The programs a release ships. Kept equal to SHIPPED_BINS in
/// .github/workflows/release.yml - the first step below fails when they drift.
const SHIPPED_BINS: &[&str] = &[
    "install", "doctor", "serve", "mcp", "codeindex", "backup", "sync", "migrate", "backfill_teeth",
];

/// The four hooks AGENTS.md promises (see `ops::install::standard_hooks`).
const HOOK_EVENTS: &[&str] = &["SessionStart", "PreToolUse", "UserPromptSubmit", "Stop"];

/// The tools AGENTS.md names as the ones setup needs.
const SETUP_TOOLS: &[&str] = &[
    "remember", "revise", "lookup", "get", "history", "status", "retract", "mark", "search_code", "where_used",
    "outline",
];

/// A semantic `serve` or `mcp` is over 20 MB (README: "Build it").
const SEMANTIC_SIZE_FLOOR: u64 = 20 * 1024 * 1024;

/// A string only a program with meaning search compiled in carries.
const SEMANTIC_MARKER: &[u8] = b"fastembed";

const STEP_TIMEOUT: Duration = Duration::from_secs(180);

type StepResult = Result<String, String>;

struct Output {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

impl Output {
    fn ok(&self) -> bool {
        self.code == Some(0)
    }

    fn describe(&self) -> String {
        format!("exit {:?}\nstdout:\n{}\nstderr:\n{}", self.code, self.stdout.trim_end(), self.stderr.trim_end())
    }
}

/// Run `cmd` to completion or kill it after `limit`. `stdin` is written, then
/// closed, so a program that reads to EOF finishes.
fn run(mut cmd: Command, stdin: Option<&str>, limit: Duration) -> Result<Output, String> {
    cmd.stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() });
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| format!("could not start {:?}: {e}", cmd.get_program()))?;
    if let Some(text) = stdin {
        let mut pipe = child.stdin.take().expect("stdin was piped");
        pipe.write_all(text.as_bytes()).map_err(|e| format!("could not write stdin: {e}"))?;
    }
    let stdout = drain(child.stdout.take().expect("stdout was piped"));
    let stderr = drain(child.stderr.take().expect("stderr was piped"));
    let deadline = Instant::now() + limit;
    let status = loop {
        match child.try_wait().map_err(|e| e.to_string())? {
            Some(status) => break status,
            None if Instant::now() > deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("{:?} did not finish within {}s and was killed", cmd.get_program(), limit.as_secs()));
            }
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    };
    Ok(Output { code: status.code(), stdout: stdout.recv().unwrap_or_default(), stderr: stderr.recv().unwrap_or_default() })
}

fn drain<R: Read + Send + 'static>(mut reader: R) -> mpsc::Receiver<String> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = reader.read_to_end(&mut bytes);
        let _ = tx.send(String::from_utf8_lossy(&bytes).to_string());
    });
    rx
}

/// Split a hook command the way the installer writes it:
/// `"<exe>" --db "<db>" hook`. Quotes group, nothing else is special.
fn split_command(command: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    for ch in command.chars() {
        match ch {
            '"' => quoted = !quoted,
            ' ' if !quoted => {
                if !current.is_empty() {
                    parts.push(std::mem::take(&mut current));
                }
            }
            c => current.push(c),
        }
    }
    if !current.is_empty() {
        parts.push(current);
    }
    parts
}

fn same_path(a: &str, b: &Path) -> bool {
    let norm = |s: &str| s.replace('\\', "/").trim_end_matches('/').to_lowercase();
    norm(a) == norm(&b.display().to_string())
}

struct Smoke {
    /// Everything below lives in here and goes away with it.
    root: tempfile::TempDir,
    bin_dir: PathBuf,
    expect_semantic: bool,
    /// What the first install printed, for the steps that cross-check it.
    first_install: RefCell<String>,
}

impl Smoke {
    fn home(&self) -> PathBuf {
        self.root.path().join("home")
    }
    /// Where a user unpacks the download: one folder where it can stay.
    fn unpacked(&self) -> PathBuf {
        self.root.path().join("unpacked")
    }
    /// A folder that is not a git checkout, for steps that run "anywhere".
    fn elsewhere(&self) -> PathBuf {
        self.root.path().join("elsewhere")
    }
    fn settings(&self) -> PathBuf {
        self.home().join(".claude").join("settings.json")
    }
    fn mcp_json(&self) -> PathBuf {
        self.home().join(".claude.json")
    }
    fn db(&self) -> PathBuf {
        self.home().join("data").join("thor.db")
    }
    fn exe(&self, name: &str) -> PathBuf {
        self.unpacked().join(format!("{name}{}", std::env::consts::EXE_SUFFIX))
    }

    /// A packaged program, running as a new user with nothing of ours around.
    fn command(&self, name: &str) -> Command {
        let home = self.home();
        let mut cmd = Command::new(self.exe(name));
        cmd.current_dir(self.elsewhere());
        cmd.env("HOME", &home).env("USERPROFILE", &home);
        cmd.env("LOCALAPPDATA", home.join("localappdata")).env("APPDATA", home.join("appdata"));
        cmd.env("XDG_DATA_HOME", home.join("xdg-data")).env("XDG_CONFIG_HOME", home.join("xdg-config"));
        cmd.env("GIT_CONFIG_GLOBAL", home.join(".gitconfig")).env("GIT_CONFIG_NOSYSTEM", "1");
        cmd
    }

    /// The one install command a new user runs, naming the three paths it
    /// writes so none of them is guessed.
    fn install(&self, cwd: &Path) -> Result<Output, String> {
        self.install_with(cwd, &[])
    }

    /// `install` plus extra flags (`--topic`, for a plain folder).
    fn install_with(&self, cwd: &Path, extra: &[&str]) -> Result<Output, String> {
        let mut cmd = self.command("install");
        cmd.current_dir(cwd)
            .arg("--settings")
            .arg(self.settings())
            .arg("--mcp-json")
            .arg(self.mcp_json())
            .arg("--db")
            .arg(self.db())
            .args(extra);
        run(cmd, None, STEP_TIMEOUT)
    }

    fn read_json(&self, path: &Path) -> Result<Value, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        serde_json::from_str(&text).map_err(|e| format!("{} is not valid JSON: {e}", path.display()))
    }

    fn step_shipped_list_matches_release_workflow(&self) -> StepResult {
        let workflow = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.github/workflows/release.yml");
        let Ok(text) = std::fs::read_to_string(&workflow) else {
            return Ok("no release.yml beside this checkout, nothing to compare".to_string());
        };
        let line = text
            .lines()
            .find(|l| l.trim_start().starts_with("SHIPPED_BINS:"))
            .ok_or("release.yml has no SHIPPED_BINS line")?;
        let mut theirs: Vec<&str> = line.split_once(':').unwrap().1.split_whitespace().collect();
        let mut ours: Vec<&str> = SHIPPED_BINS.to_vec();
        theirs.sort_unstable();
        ours.sort_unstable();
        if theirs == ours {
            Ok(format!("{} programs, same as release.yml", ours.len()))
        } else {
            Err(format!("SHIPPED_BINS differs: release.yml has {theirs:?}, this test has {ours:?}"))
        }
    }

    /// Unpack: copy exactly the shipped programs into a fresh folder, as
    /// install.sh and install.ps1 do, and make each one start.
    fn step_unpack_and_start(&self) -> StepResult {
        std::fs::create_dir_all(self.unpacked()).map_err(|e| e.to_string())?;
        std::fs::create_dir_all(self.elsewhere()).map_err(|e| e.to_string())?;
        let mut missing = Vec::new();
        for name in SHIPPED_BINS {
            let file = format!("{name}{}", std::env::consts::EXE_SUFFIX);
            let from = self.bin_dir.join(&file);
            if from.is_file() {
                std::fs::copy(&from, self.unpacked().join(&file)).map_err(|e| format!("copy {file}: {e}"))?;
            } else {
                missing.push(file);
            }
        }
        if !missing.is_empty() {
            return Err(format!("not in {}: {}", self.bin_dir.display(), missing.join(", ")));
        }
        let want = env!("CARGO_PKG_VERSION");
        let mut wrong = Vec::new();
        for name in SHIPPED_BINS {
            let mut cmd = self.command(name);
            cmd.arg("--version");
            match run(cmd, None, Duration::from_secs(60)) {
                Ok(out) if out.ok() && out.stdout.contains(want) => {}
                Ok(out) => wrong.push(format!("{name}: {}", out.describe())),
                Err(e) => wrong.push(format!("{name}: {e}")),
            }
        }
        if wrong.is_empty() {
            Ok(format!("{} programs start and report {want}", SHIPPED_BINS.len()))
        } else {
            Err(wrong.join("\n"))
        }
    }

    /// The semantic build. A plain build starts and answers, and quietly
    /// returns nothing for meaning search - so look inside the files. The
    /// marker is only in the programs that embed text (the others link the
    /// ONNX runtime either way); doctor says its own mode in its own report,
    /// which the doctor step checks.
    fn step_semantic_build(&self) -> StepResult {
        // `--features serve/semantic` once built mcp and sync without it while
        // they were still over 20 MB, so every embedding program is checked.
        const SEARCHING: &[&str] = &["serve", "mcp", "sync"];
        let mut problems = Vec::new();
        for name in SEARCHING {
            let path = self.exe(name);
            let bytes = std::fs::read(&path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
            let has_marker = bytes.windows(SEMANTIC_MARKER.len()).any(|w| w == SEMANTIC_MARKER);
            if has_marker != self.expect_semantic {
                problems.push(format!(
                    "{name} {} meaning search (expected {})",
                    if has_marker { "has" } else { "lacks" },
                    if self.expect_semantic { "yes" } else { "no" }
                ));
            }
            if self.expect_semantic && *name != "sync" && (bytes.len() as u64) < SEMANTIC_SIZE_FLOOR {
                problems.push(format!("{name} is {} bytes, under the 20 MB a semantic build reaches", bytes.len()));
            }
        }
        if problems.is_empty() {
            Ok(format!(
                "{} build: {} carry the marker as expected",
                if self.expect_semantic { "semantic" } else { "plain" },
                SEARCHING.join(", ")
            ))
        } else {
            Err(problems.join("; "))
        }
    }

    /// `install` from a plain folder, as the very first thing a user does.
    fn step_install(&self) -> StepResult {
        let out = self.install(&self.elsewhere())?;
        if !out.ok() {
            return Err(out.describe());
        }
        if !out.stdout.contains("+ created a memory") {
            return Err(format!("the first install did not say it created a memory:\n{}", out.stdout));
        }
        if out.stdout.contains("  ! ") || out.stdout.contains("\n! ") {
            return Err(format!("install printed a warning line:\n{}", out.stdout));
        }
        *self.first_install.borrow_mut() = out.stdout;
        Ok("exit 0, memory created, no warning lines".to_string())
    }

    fn step_hooks_written(&self) -> StepResult {
        let settings = self.read_json(&self.settings())?;
        let mut checked = 0;
        for event in HOOK_EVENTS {
            let groups = settings["hooks"][event]
                .as_array()
                .ok_or_else(|| format!("settings.json has no hooks.{event} list"))?;
            let command = groups
                .iter()
                .flat_map(|g| g["hooks"].as_array().into_iter().flatten())
                .filter_map(|h| h["command"].as_str())
                .find(|c| c.contains(" hook"))
                .ok_or_else(|| format!("hooks.{event} has no hook command"))?;
            let parts = split_command(command);
            let exe = parts.first().ok_or_else(|| format!("hooks.{event} command is empty"))?;
            if !Path::new(exe).is_file() {
                return Err(format!("hooks.{event} calls {exe}, which does not exist"));
            }
            if !same_path(exe, &self.exe("serve")) {
                return Err(format!("hooks.{event} calls {exe}, not the unpacked serve"));
            }
            let db_pos = parts.iter().position(|p| p == "--db").ok_or("hook command has no --db")?;
            if !parts.get(db_pos + 1).is_some_and(|d| same_path(d, &self.db())) {
                return Err(format!("hooks.{event} names another store: {command}"));
            }
            checked += 1;
        }
        Ok(format!("{checked} hooks, each pointing at the unpacked serve and the new store"))
    }

    fn step_tool_server_written(&self) -> StepResult {
        let config = self.read_json(&self.mcp_json())?;
        let entry = &config["mcpServers"]["thor"];
        let command = entry["command"].as_str().ok_or("no mcpServers.thor.command in the MCP file")?;
        if !Path::new(command).is_file() {
            return Err(format!("the tool server entry calls {command}, which does not exist"));
        }
        if !same_path(command, &self.exe("mcp")) {
            return Err(format!("the tool server entry calls {command}, not the unpacked mcp"));
        }
        let args: Vec<&str> = entry["args"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
        let named = args.iter().position(|a| *a == "--db").and_then(|i| args.get(i + 1));
        if !named.is_some_and(|d| same_path(d, &self.db())) {
            return Err(format!("the tool server entry does not name the new store: {args:?}"));
        }
        Ok("mcp entry points at the unpacked mcp and the new store".to_string())
    }

    /// What a fresh store must hold, and the files install puts beside it.
    fn step_seeded(&self) -> StepResult {
        if !self.db().is_file() {
            return Err(format!("no store at {}", self.db().display()));
        }
        let said: u32 = self
            .first_install
            .borrow()
            .split("put ")
            .nth(1)
            .and_then(|rest| rest.split_whitespace().next()?.parse().ok())
            .ok_or("the first install never said how many starting notes it put in the memory")?;
        let mut cmd = self.command("serve");
        cmd.arg("--db").arg(self.db()).arg("status");
        let out = run(cmd, None, Duration::from_secs(60))?;
        if !out.ok() {
            return Err(out.describe());
        }
        let held: u32 = out
            .stdout
            .lines()
            .find_map(|l| l.strip_prefix("Rule")?.trim().parse().ok())
            .ok_or_else(|| format!("serve status shows no Rule count:\n{}", out.stdout))?;
        if said == 0 || held != said {
            return Err(format!("install said it put {said} notes in, the store holds {held} rules"));
        }
        let rulebook = self.db().parent().unwrap().join("guard-response-rulebook.json");
        if !rulebook.is_file() {
            return Err(format!("no response-guard rulebook at {}", rulebook.display()));
        }
        let eval = self.home().join(".claude").join("commands").join("thor-eval.md");
        if !eval.is_file() {
            return Err(format!("no /thor-eval command at {}", eval.display()));
        }
        let gitconfig = std::fs::read_to_string(self.home().join(".gitconfig")).unwrap_or_default();
        if !gitconfig.contains("hooksPath") {
            return Err("core.hooksPath did not land in the sandbox's own git config".to_string());
        }
        Ok(format!("{held} seeded rules in the store, plus the rulebook, /thor-eval and the git hooks"))
    }

    /// AGENTS.md step 4. One plain line per part: the version, the chain, the
    /// notes, and the search mode, which has to match what was built.
    fn step_doctor(&self) -> StepResult {
        let mut cmd = self.command("doctor");
        // A model folder that is not there: a new user has none, and doctor
        // must say so plainly instead of failing.
        cmd.arg("--db").arg(self.db()).arg("--model-dir").arg(self.home().join("no-model"));
        let out = run(cmd, None, STEP_TIMEOUT)?;
        if !out.ok() {
            return Err(out.describe());
        }
        let expected = [
            format!("doctor {}", env!("CARGO_PKG_VERSION")),
            "memory store:".to_string(),
            "chain intact".to_string(),
            "falsifiers:".to_string(),
            "pinned:".to_string(),
            "semantic search:".to_string(),
        ];
        let missing: Vec<&String> = expected.iter().filter(|e| !out.stdout.contains(e.as_str())).collect();
        if !missing.is_empty() {
            return Err(format!("doctor never said {missing:?}:\n{}", out.stdout));
        }
        let mode = out.stdout.lines().find(|l| l.starts_with("semantic search:")).unwrap_or_default();
        let compiled_out = mode.contains("not compiled");
        if compiled_out == self.expect_semantic {
            return Err(format!(
                "doctor's own build is {}, expected {}: {mode}",
                if compiled_out { "plain" } else { "semantic" },
                if self.expect_semantic { "semantic" } else { "plain" }
            ));
        }
        let lines = out.stdout.lines().filter(|l| !l.trim().is_empty()).count();
        Ok(format!("exit 0, {lines} report lines, search mode as built"))
    }

    /// A second run is a no-op: the same two files byte for byte, and no
    /// line that says something was added.
    fn step_install_is_idempotent(&self) -> StepResult {
        let before = (std::fs::read(self.settings()), std::fs::read(self.mcp_json()));
        let out = self.install(&self.elsewhere())?;
        if !out.ok() {
            return Err(out.describe());
        }
        let after = (std::fs::read(self.settings()), std::fs::read(self.mcp_json()));
        if before.0.is_err() || before.1.is_err() {
            return Err("settings.json or the MCP file went missing".to_string());
        }
        if before.0.ok() != after.0.ok() {
            return Err("the second install changed settings.json".to_string());
        }
        if before.1.ok() != after.1.ok() {
            return Err("the second install changed the MCP file".to_string());
        }
        let added: Vec<&str> = out.stdout.lines().filter(|l| l.starts_with("+ ")).collect();
        if !added.is_empty() {
            return Err(format!("the second install still added things: {added:?}"));
        }
        Ok("exit 0, both files byte-identical, nothing added".to_string())
    }

    /// AGENTS.md step 5: run install again from inside a project folder.
    fn step_project_install(&self) -> StepResult {
        let repo = self.root.path().join("smoke-project");
        std::fs::create_dir_all(&repo).map_err(|e| e.to_string())?;
        std::fs::write(repo.join("main.rs"), "fn main() { println!(\"smoke\"); }\n").map_err(|e| e.to_string())?;
        for args in [
            vec!["init", "--quiet"],
            vec!["add", "-A"],
            vec!["-c", "user.name=smoke", "-c", "user.email=smoke@example.invalid", "commit", "--quiet", "-m", "initial"],
        ] {
            let mut cmd = Command::new("git");
            cmd.arg("-C").arg(&repo).args(&args).env("GIT_CONFIG_GLOBAL", self.home().join(".gitconfig"));
            cmd.env("GIT_CONFIG_NOSYSTEM", "1");
            let out = run(cmd, None, Duration::from_secs(60))?;
            if !out.ok() {
                return Err(format!("git {args:?} failed: {}", out.describe()));
            }
        }
        let out = self.install(&repo)?;
        if !out.ok() {
            return Err(out.describe());
        }
        for promised in ["read this project's code", "+ registered"] {
            if !out.stdout.contains(promised) {
                return Err(format!("install inside a project never said {promised:?}:\n{}", out.stdout));
            }
        }
        Ok("project code read and its working copy registered".to_string())
    }

    /// A remember through the packaged `mcp`, over stdio JSON-RPC, as the
    /// assistant does it. Returns the unique text it stored.
    fn step_remember_through_mcp(&self, token: &str) -> StepResult {
        self.remember_in(&self.elsewhere(), "smoke-test-note", token, None)
    }

    /// The same, started in `cwd` (a session's own folder) and, for a topic,
    /// filed under that topic's name.
    fn remember_in(&self, cwd: &Path, id: &str, token: &str, project: Option<&str>) -> StepResult {
        let mut cmd = self.command("mcp");
        cmd.current_dir(cwd);
        cmd.arg("--db").arg(self.db()).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
        let mut child = cmd.spawn().map_err(|e| format!("could not start mcp: {e}"))?;
        let result = self.talk_to_mcp(&mut child, id, token, project);
        let _ = child.kill();
        let _ = child.wait();
        result
    }

    fn talk_to_mcp(&self, child: &mut Child, id: &str, token: &str, project: Option<&str>) -> StepResult {
        let mut stdin = child.stdin.take().ok_or("no stdin")?;
        let stdout = child.stdout.take().ok_or("no stdout")?;
        let (tx, rx) = mpsc::channel::<String>();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let mut send = |message: Value| -> Result<(), String> {
            writeln!(stdin, "{message}").and_then(|_| stdin.flush()).map_err(|e| format!("mcp closed its input: {e}"))
        };
        let reply = |id: u64| -> Result<Value, String> {
            let deadline = Instant::now() + Duration::from_secs(60);
            loop {
                let left = deadline.saturating_duration_since(Instant::now());
                let line = rx.recv_timeout(left).map_err(|_| format!("mcp gave no answer to request {id} in 60s"))?;
                if let Ok(value) = serde_json::from_str::<Value>(&line) {
                    if value["id"] == json!(id) {
                        return Ok(value);
                    }
                }
            }
        };

        send(json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "protocolVersion": "2025-03-26", "capabilities": {},
            "clientInfo": {"name": "release-smoke", "version": "0"}}}))?;
        let init = reply(1)?;
        if init["result"]["serverInfo"].is_null() {
            return Err(format!("initialize answered without serverInfo: {init}"));
        }
        send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))?;

        send(json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}))?;
        let listed = reply(2)?;
        let names: Vec<&str> =
            listed["result"]["tools"].as_array().into_iter().flatten().filter_map(|t| t["name"].as_str()).collect();
        let missing: Vec<&str> = SETUP_TOOLS.iter().copied().filter(|t| !names.contains(t)).collect();
        if !missing.is_empty() {
            return Err(format!("mcp does not offer {missing:?}; it offers {names:?}"));
        }

        let mut arguments = json!({
            "id": id,
            "kind": "rule",
            "text": format!("The release smoke test stored this note: {token}."),
            "falsifier": "The smoke test finds this note gone from a fresh store.",
            "always": true
        });
        if let Some(project) = project {
            arguments["project"] = json!(project);
        }
        send(json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {
            "name": "remember",
            "arguments": arguments}}))?;
        let stored = reply(3)?;
        let text = stored["result"]["content"][0]["text"].as_str().unwrap_or("").to_string();
        if stored["error"].is_object() || stored["result"]["isError"] == json!(true) {
            return Err(format!("remember was refused: {stored}"));
        }
        Ok(format!("{} tools offered, remember answered: {}", names.len(), text.lines().next().unwrap_or("")))
    }

    /// Run the SessionStart hook exactly as the settings.json line names it,
    /// against the new store, and expect the stored note back.
    fn step_session_start_serves_it(&self, token: &str) -> StepResult {
        self.session_start_in(&self.elsewhere(), token)
    }

    /// Run a hook event exactly as the settings.json line names it, from
    /// `cwd`, with `payload` on stdin. The raw stdout.
    fn run_hook(&self, event: &str, cwd: &Path, payload: Value) -> Result<String, String> {
        let settings = self.read_json(&self.settings())?;
        let command = settings["hooks"][event][0]["hooks"][0]["command"]
            .as_str()
            .ok_or_else(|| format!("no {event} command in settings.json"))?;
        let parts = split_command(command);
        let mut cmd = Command::new(&parts[0]);
        cmd.args(&parts[1..]);
        let home = self.home();
        cmd.current_dir(cwd).env("HOME", &home).env("USERPROFILE", &home);
        cmd.env("LOCALAPPDATA", home.join("localappdata")).env("XDG_DATA_HOME", home.join("xdg-data"));
        let out = run(cmd, Some(&payload.to_string()), Duration::from_secs(60))?;
        if !out.ok() {
            return Err(out.describe());
        }
        Ok(out.stdout)
    }

    /// The SessionStart hook from `cwd`, expecting the stored note back.
    fn session_start_in(&self, cwd: &Path, token: &str) -> StepResult {
        let stdout = self.run_hook(
            "SessionStart",
            cwd,
            json!({
                "hook_event_name": "SessionStart",
                "session_id": "release-smoke",
                "cwd": cwd.display().to_string(),
                "source": "startup"
            }),
        )?;
        let parsed: Value =
            serde_json::from_str(stdout.trim()).map_err(|e| format!("hook output is not JSON ({e}): {stdout}"))?;
        let context = parsed["hookSpecificOutput"]["additionalContext"].as_str().unwrap_or("");
        if !context.contains(token) {
            return Err(format!("SessionStart did not serve the stored note back. It served:\n{context}"));
        }
        Ok(format!("served {} characters, including the stored note", context.len()))
    }

    // ------------------------------------------------ a plain topic folder
    //
    // The walk a person makes who wants THOR for a SUBJECT, not for code
    // (AGENTS.md 5b): a plain folder with notes and a PDF in it, no git
    // anywhere. Each step is one thing that used to stop them.

    fn topic(&self) -> PathBuf {
        self.root.path().join("Documents").join("Kitchen renovation")
    }

    /// `install --topic` in the plain folder: it works, it is said in plain
    /// words, the folder gets exactly one new file, and nothing mentions code
    /// that could not be read.
    fn step_topic_install(&self) -> StepResult {
        let topic = self.topic();
        std::fs::create_dir_all(topic.join("PDFs")).map_err(|e| e.to_string())?;
        std::fs::write(topic.join("notes.txt"), "Quote from the builder. Tiles ordered at Tilehouse.\n").map_err(|e| e.to_string())?;
        std::fs::write(topic.join("PDFs").join("quote.pdf"), "%PDF-1.4 fixture\n").map_err(|e| e.to_string())?;
        let out = self.install_with(&topic, &["--topic"])?;
        if !out.ok() {
            return Err(out.describe());
        }
        for promised in ["is now a topic", "Kitchen renovation"] {
            if !out.stdout.contains(promised) {
                return Err(format!("install --topic never said {promised:?}:\n{}", out.stdout));
            }
        }
        for forbidden in ["could not be read", "REFUSED", "not a git", "  ! ", "\n! "] {
            if out.stdout.contains(forbidden) || out.stderr.contains(forbidden) {
                return Err(format!("install --topic printed {forbidden:?}:\n{}\n{}", out.stdout, out.stderr));
            }
        }
        let mut names: Vec<String> = std::fs::read_dir(&topic)
            .map_err(|e| e.to_string())?
            .map(|e| e.map(|e| e.file_name().to_string_lossy().to_string()).map_err(|e| e.to_string()))
            .collect::<Result<_, _>>()?;
        names.sort();
        if names != [".thor-project", "PDFs", "notes.txt"] {
            return Err(format!("the topic folder holds {names:?}, expected only one new file beside the user's own"));
        }
        let again = self.install_with(&topic, &["--topic"])?;
        if !again.ok() || again.stdout.lines().any(|l| l.starts_with("+ ")) {
            return Err(format!("a second `install --topic` changed something:\n{}", again.stdout));
        }
        let broad = self.install_with(&self.home(), &["--topic"])?;
        if broad.ok() || !broad.stderr.contains("too broad to be one topic") || self.home().join(".thor-project").exists() {
            return Err(format!("the home folder was not refused as a topic:\n{}", broad.describe()));
        }
        Ok("registered without git, one file added, second run quiet, home folder refused".to_string())
    }

    /// Doctor, run from inside the topic, opens with plain OK lines.
    fn step_topic_doctor(&self) -> StepResult {
        let mut cmd = self.command("doctor");
        cmd.current_dir(self.topic()).arg("--db").arg(self.db());
        let out = run(cmd, None, STEP_TIMEOUT)?;
        if !out.ok() {
            return Err(out.describe());
        }
        let head: Vec<&str> = out.stdout.lines().take(12).collect();
        let head = head.join("\n");
        for promised in [
            "OK: your memory file is there",
            "OK: THOR is connected to Claude Code.",
            "OK: your assistant can save notes.",
            "OK: this folder has its own memory, under the name 'Kitchen renovation'.",
        ] {
            if !head.contains(promised) {
                return Err(format!("doctor's opening lines never said {promised:?}:\n{head}"));
            }
        }
        if out.stdout.contains("NOT USABLE") {
            return Err(format!("doctor called the topic's working copy unusable:\n{}", out.stdout));
        }
        Ok("opens with a plain summary, all OK".to_string())
    }

    /// A note filed under the topic through the packaged `mcp`, started in the
    /// topic folder, comes back at session start in that folder - and not in
    /// another one.
    fn step_topic_memory_is_its_own(&self, token: &str) -> StepResult {
        self.remember_in(&self.topic(), "smoke-topic-note", token, Some("Kitchen renovation"))?;
        let served = self.session_start_in(&self.topic(), token)?;
        // Another folder must not be shown the topic's note.
        match self.session_start_in(&self.elsewhere(), token) {
            Ok(_) => Err("the topic's note was served in an unrelated folder".to_string()),
            Err(_) => Ok(format!("{served}; not served elsewhere")),
        }
    }

    /// What the person sees when the first-session hold stops the turn: the
    /// assistant's own text is untouched and one plain sentence travels
    /// beside it.
    fn step_hold_has_a_plain_line(&self) -> StepResult {
        let stdout = self.run_hook(
            "Stop",
            &self.elsewhere(),
            json!({
                "hook_event_name": "Stop",
                "session_id": "release-smoke-stop",
                "cwd": self.elsewhere().display().to_string(),
                "stop_hook_active": false,
                "last_assistant_message": ""
            }),
        )?;
        let parsed: Value =
            serde_json::from_str(stdout.trim()).map_err(|e| format!("Stop output is not JSON ({e}): {stdout}"))?;
        if parsed["decision"] != json!("block") {
            return Err(format!("a new memory's first turn end was not held:\n{stdout}"));
        }
        let line = parsed["systemMessage"].as_str().ok_or_else(|| format!("the hold carries no plain line:\n{stdout}"))?;
        if !line.contains("setup questions") || line.contains('\n') {
            return Err(format!("the plain line is not one plain sentence about setup: {line}"));
        }
        Ok(format!("held, with: {line}"))
    }
}

#[test]
#[ignore = "needs the whole set of built programs; run with --ignored (see the file header)"]
fn a_new_user_can_install_and_use_the_shipped_programs() {
    let bin_dir = std::env::var_os("THOR_SMOKE_BIN_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_BIN_EXE_install")).parent().unwrap().to_path_buf());
    let expect_semantic = match std::env::var("THOR_SMOKE_SEMANTIC").as_deref() {
        Ok("yes") | Err(_) => true,
        Ok("no") => false,
        Ok(other) => panic!("THOR_SMOKE_SEMANTIC must be yes or no, not {other:?}"),
    };
    let smoke = Smoke { root: tempfile::tempdir().unwrap(), bin_dir, expect_semantic, first_install: RefCell::default() };
    println!("release smoke: programs from {}, meaning search expected: {}", smoke.bin_dir.display(), expect_semantic);

    let token = "marmalade-quartz-4471";
    let mut failed = Vec::new();
    let mut step = |name: &str, result: StepResult| match result {
        Ok(detail) => println!("PASS  {name}: {detail}"),
        Err(why) => {
            println!("FAIL  {name}: {why}");
            failed.push(name.to_string());
        }
    };

    step("shipped list matches release.yml", smoke.step_shipped_list_matches_release_workflow());
    step("unpack and start every program", smoke.step_unpack_and_start());
    step("meaning search build", smoke.step_semantic_build());
    step("install (AGENTS.md step 2)", smoke.step_install());
    step("hooks written and point at existing programs", smoke.step_hooks_written());
    step("tool server entry written", smoke.step_tool_server_written());
    step("store seeded, rulebook and eval command written", smoke.step_seeded());
    step("doctor reports each part (step 4)", smoke.step_doctor());
    step("second install changes nothing", smoke.step_install_is_idempotent());
    step("install inside a project (step 5)", smoke.step_project_install());
    step("remember through mcp over stdio (step 6)", smoke.step_remember_through_mcp(token));
    step("SessionStart hook serves the note back", smoke.step_session_start_serves_it(token));
    // A person who wants THOR for a subject, not for code (AGENTS.md 5b).
    step("topic folder: install --topic, no git (5b)", smoke.step_topic_install());
    step("topic folder: doctor opens with a plain summary", smoke.step_topic_doctor());
    step("topic folder: its own memory, served only there", smoke.step_topic_memory_is_its_own("topic-quartz-8812"));
    step("a held turn carries one plain sentence", smoke.step_hold_has_a_plain_line());

    assert!(failed.is_empty(), "release smoke FAILED {} step(s): {}", failed.len(), failed.join("; "));
    println!("release smoke: all steps passed");
}
