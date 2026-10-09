//! Proof that a PLAIN folder - no git, no code, just a topic with some notes
//! and PDFs in it - works as a project: it is keyed by its folder name,
//! registered as the project's working copy without a repository, a rule with
//! a file check is tried against its files, and doctor calls it fine.
//!
//! WHY THIS EXISTS. THOR used to refuse every folder that was not a git
//! checkout ("Start a repository there first (git init, then one commit)"),
//! which sent a person who only wanted a memory for "Kitchen renovation" to a
//! tool they had never heard of. The folder is now the project, and a small
//! `.thor-project` file in it is the proof that somebody asked for that.
//!
//! Every spawned program runs with HOME, USERPROFILE and LOCALAPPDATA pointed
//! at a throwaway sandbox (see `install_records_project_opened.rs`), and every
//! install passes `--no-mcp` plus an explicit `--settings`, so no real file of
//! the machine is ever opened.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use thor_core::event_store::EventStore;

struct Sandbox {
    home: tempfile::TempDir,
}

impl Sandbox {
    fn new() -> Self {
        Self { home: tempfile::tempdir().unwrap() }
    }

    fn db(&self) -> PathBuf {
        self.home.path().join("localappdata").join("thor2").join("thor.db")
    }

    fn apply(&self, cmd: &mut Command) {
        cmd.env("HOME", self.home.path());
        cmd.env("USERPROFILE", self.home.path());
        cmd.env("LOCALAPPDATA", self.home.path().join("localappdata"));
        cmd.env("GIT_CONFIG_GLOBAL", self.home.path().join(".gitconfig"));
        cmd.env("GIT_CONFIG_NOSYSTEM", "1");
    }

    /// A topic folder with a couple of ordinary files in it, and no git.
    fn topic(&self, name: &str) -> PathBuf {
        let dir = self.home.path().join("Documents").join(name);
        std::fs::create_dir_all(dir.join("PDFs")).unwrap();
        std::fs::write(dir.join("notes.txt"), "Quote from the builder: 8400. Tiles ordered at Tilehouse.\n").unwrap();
        std::fs::write(dir.join("PDFs").join("quote.pdf"), "%PDF-1.4 fixture\n").unwrap();
        dir
    }
}

fn install(dir: &Path, sandbox: &Sandbox, args: &[&str]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_install"));
    let settings = sandbox.home.path().join(".claude").join("settings.json");
    cmd.current_dir(dir).arg("--no-mcp").arg("--settings").arg(settings).args(args);
    sandbox.apply(&mut cmd);
    cmd.output().unwrap()
}

fn register(dir: &Path, sandbox: &Sandbox) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_install"));
    cmd.current_dir(dir).arg("--register-root").arg("--db").arg(sandbox.db());
    sandbox.apply(&mut cmd);
    cmd.output().unwrap()
}

fn text(out: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr))
}

fn slash(p: &Path) -> String {
    p.display().to_string().replace('\\', "/")
}

fn event_count(db: &Path) -> usize {
    EventStore::open_existing(db).unwrap().get_all_events().unwrap().len()
}

/// `install --topic` from a plain folder: no failure, no talk of git or code
/// that "could not be read", the folder's name becomes the project, the
/// marker is written, the working copy is registered and no code index is
/// built.
#[test]
fn install_topic_in_a_plain_folder_registers_it_without_git() {
    let sandbox = Sandbox::new();
    let topic = sandbox.topic("Kitchen renovation");

    let out = install(&topic, &sandbox, &["--topic"]);
    assert!(out.status.success(), "{}", text(&out));
    let said = text(&out);
    assert!(!said.contains("could not be read"), "no complaint about code: {said}");
    assert!(!said.contains("not a git"), "no talk of git: {said}");
    assert!(said.contains("Kitchen renovation"), "the topic is named: {said}");

    let marker = std::fs::read_to_string(topic.join(".thor-project")).unwrap();
    assert_eq!(marker.trim(), "Kitchen renovation");

    let store = EventStore::open_existing(&sandbox.db()).unwrap();
    let root = serve::project::registered_checkout(&store, Some("Kitchen renovation")).expect("registered and usable");
    assert_eq!(slash(&root), slash(&topic));
    assert!(
        !sandbox.db().parent().unwrap().join("codeindex").join("Kitchen renovation.db").exists(),
        "a plain folder has no code to index"
    );
    // The files in the folder are untouched, and only the marker was added.
    let mut names: Vec<String> =
        std::fs::read_dir(&topic).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().to_string()).collect();
    names.sort();
    assert_eq!(names, vec![".thor-project", "PDFs", "notes.txt"]);
}

/// A second run says it is already set up and writes nothing.
#[test]
fn a_second_topic_install_changes_nothing() {
    let sandbox = Sandbox::new();
    let topic = sandbox.topic("Kitchen renovation");
    assert!(install(&topic, &sandbox, &["--topic"]).status.success());
    let events = event_count(&sandbox.db());

    let again = install(&topic, &sandbox, &["--topic"]);
    assert!(again.status.success(), "{}", text(&again));
    assert_eq!(event_count(&sandbox.db()), events, "{}", text(&again));
    assert!(text(&again).contains("already"), "{}", text(&again));
}

/// Run from a subfolder of the topic, `--topic` still registers the topic's
/// own top, not the subfolder.
#[test]
fn a_topic_install_from_a_subfolder_registers_the_topic_itself() {
    let sandbox = Sandbox::new();
    let topic = sandbox.topic("Kitchen renovation");
    assert!(install(&topic, &sandbox, &["--topic"]).status.success());

    let again = install(&topic.join("PDFs"), &sandbox, &["--topic"]);
    assert!(again.status.success(), "{}", text(&again));
    assert!(!topic.join("PDFs").join(".thor-project").exists(), "no second marker below the topic");
}

/// The short route the setup page already names: `--register-root` from a
/// plain folder marks it as a topic and registers it, in one step.
#[test]
fn register_root_in_a_plain_folder_marks_and_registers_it() {
    let sandbox = Sandbox::new();
    let topic = sandbox.topic("Kitchen renovation");
    // A memory must exist first, as for any registration.
    ops::install::ensure_store(&sandbox.db()).unwrap();

    let out = register(&topic, &sandbox);
    assert!(out.status.success(), "{}", text(&out));
    assert!(text(&out).contains(&format!("registered 'Kitchen renovation' -> {}", slash(&topic))), "{}", text(&out));
    assert_eq!(std::fs::read_to_string(topic.join(".thor-project")).unwrap().trim(), "Kitchen renovation");

    let events = event_count(&sandbox.db());
    let again = register(&topic, &sandbox);
    assert!(again.status.success(), "{}", text(&again));
    assert!(text(&again).contains("already registered here"), "{}", text(&again));
    assert_eq!(event_count(&sandbox.db()), events);
}

/// The home folder is far too broad to be one topic: refused in plain words,
/// and nothing is written into it.
#[test]
fn the_home_folder_is_refused_as_a_topic() {
    let sandbox = Sandbox::new();
    ops::install::ensure_store(&sandbox.db()).unwrap();
    let events = event_count(&sandbox.db());

    let out = register(sandbox.home.path(), &sandbox);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    assert!(text(&out).contains("too broad to be one topic"), "{}", text(&out));
    assert!(!sandbox.home.path().join(".thor-project").exists());

    let topic_flag = install(sandbox.home.path(), &sandbox, &["--topic"]);
    assert_eq!(topic_flag.status.code(), Some(1), "{}", text(&topic_flag));
    assert!(text(&topic_flag).contains("too broad to be one topic"), "{}", text(&topic_flag));
    assert!(!sandbox.home.path().join(".thor-project").exists());
    assert_eq!(event_count(&sandbox.db()), events);
}

/// A rule whose file check reads a file of the topic is tried in the topic
/// folder: stored with its check, and a file that lacks the literal is
/// refused as the check's rot rather than silently skipped.
#[tokio::test]
async fn a_file_check_in_a_plain_topic_folder_is_tried() {
    let sandbox = Sandbox::new();
    let topic = sandbox.topic("Kitchen renovation");
    assert!(install(&topic, &sandbox, &["--topic"]).status.success());

    let store = EventStore::open_existing(&sandbox.db()).unwrap();
    let server = mcp::ThorMcpServer::new(store).with_root(topic.clone());
    let op = thor_core::inbox::InboxOp::new(
        "remember",
        serde_json::json!({
            "id": "tiles-come-from-tilehouse",
            "kind": "rule",
            "text": "The kitchen tiles are ordered at Tilehouse; notes.txt keeps the word Tilehouse in it.",
            "project": "Kitchen renovation",
            "targets": [{ "kind": "path", "value": "notes.txt" }],
            "check_kind": "contains",
            "check_path": "notes.txt",
            "check_literal": "Tilehouse",
            "falsifier": "notes.txt no longer mentions Tilehouse."
        }),
    );
    match server.apply_captured(&op).await {
        mcp::CapturedOutcome::Applied(said) => {
            assert!(said.starts_with("stored "), "{said}");
            assert!(said.contains("teeth: wired"), "the check was tried in the topic folder: {said}");
            assert!(!said.contains("no working copy"), "the topic folder is the working copy: {said}");
        }
        mcp::CapturedOutcome::NotApplied(said) => panic!("a file check in a plain topic folder was refused: {said}"),
    }

    // The same rule against a file the topic does not have is refused, which
    // proves the check was really tried in the topic's folder.
    let bad = thor_core::inbox::InboxOp::new(
        "remember",
        serde_json::json!({
            "id": "invoice-says-paid",
            "kind": "rule",
            "text": "invoice.txt always says PAID somewhere in it.",
            "project": "Kitchen renovation",
            "targets": [{ "kind": "path", "value": "invoice.txt" }],
            "check_kind": "contains",
            "check_path": "invoice.txt",
            "check_literal": "PAID",
            "falsifier": "invoice.txt does not say PAID."
        }),
    );
    match server.apply_captured(&bad).await {
        mcp::CapturedOutcome::Applied(said) => panic!("a check on a file the topic does not have was stored: {said}"),
        mcp::CapturedOutcome::NotApplied(said) => assert!(said.contains("invoice.txt does not exist"), "{said}"),
    }
}

/// The proof note AGENTS.md (5b) tells an assistant to store for a topic goes
/// through the write gate as written: filed under the topic by the folder's
/// name, pinned to every session start, with no collection question.
#[tokio::test]
async fn the_topic_proof_note_from_agents_md_is_accepted() {
    let sandbox = Sandbox::new();
    let topic = sandbox.topic("Kitchen renovation");
    assert!(install(&topic, &sandbox, &["--topic"]).status.success());

    let server = mcp::ThorMcpServer::new(EventStore::open_existing(&sandbox.db()).unwrap()).with_root(topic.clone());
    let op = thor_core::inbox::InboxOp::new(
        "remember",
        serde_json::json!({
            "id": "tiles-from-tilehouse",
            "kind": "orientation",
            "project": "Kitchen renovation",
            "always": true,
            "text": "The kitchen tiles are ordered at Tilehouse.",
            "falsifier": "A later note says the tiles come from somewhere else."
        }),
    );
    match server.apply_captured(&op).await {
        mcp::CapturedOutcome::Applied(said) => assert!(said.starts_with("stored "), "{said}"),
        mcp::CapturedOutcome::NotApplied(said) => panic!("the AGENTS.md example was refused: {said}"),
    }

    // It is stored under the topic. (That it comes back at session start in
    // the topic folder is proven with the real programs by release_smoke.)
    let store = EventStore::open_existing(&sandbox.db()).unwrap();
    let item = model::store::show(&store, "tiles-from-tilehouse").unwrap();
    assert_eq!(item.project.as_deref(), Some("Kitchen renovation"));
}

/// Doctor, run from inside the topic folder, lists its working copy as usable.
#[test]
fn doctor_calls_a_topic_folders_working_copy_usable() {
    let sandbox = Sandbox::new();
    let topic = sandbox.topic("Kitchen renovation");
    assert!(install(&topic, &sandbox, &["--topic"]).status.success());

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_doctor"));
    cmd.current_dir(&topic).arg("--db").arg(sandbox.db());
    sandbox.apply(&mut cmd);
    let out = cmd.output().unwrap();
    assert!(out.status.success(), "{}", text(&out));
    let said = text(&out);
    assert!(
        said.contains(&format!("working copy: Kitchen renovation -> {}", slash(&topic))),
        "doctor names the topic's folder: {said}"
    );
    assert!(!said.contains("NOT USABLE"), "{said}");
}
