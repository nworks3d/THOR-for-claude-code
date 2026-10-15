//! Proof that `install` registers a project's working copy once, and that a
//! second copy can never take that registration over (A2, Z3, D3) - run
//! against the REAL compiled `install` binary.
//!
//! THE LEAK THIS CLOSES. A rule's file check is only worth trusting when it
//! is tried against the project's real checkout, and "real" used to be
//! whatever folder the session ran in: a clone with one planted line could
//! prove anything, and the per-project install run from that clone simply
//! re-pointed the project at it. Now the installer records the checkout once
//! (a trailing `"root"` key on the project's record, written by the
//! installer only) and moves it only when the registered folder is gone.
//!
//! Every spawned `install` runs with HOME, USERPROFILE and LOCALAPPDATA
//! pointed at a throwaway sandbox, plus GIT_CONFIG_GLOBAL/GIT_CONFIG_NOSYSTEM,
//! exactly like `install_project_index.rs` (read its header for why). Every
//! fixture repository gets `core.hooksPath` pointed at an empty folder right
//! after `git init` (and every clone is made with it), so the machine's own
//! shared git hooks never act on a throwaway repository.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use thor_core::event_store::{EventKind, EventStore};

struct Sandbox {
    home: tempfile::TempDir,
}

impl Sandbox {
    fn new() -> Self {
        Self { home: tempfile::tempdir().unwrap() }
    }

    fn local_app_data(&self) -> PathBuf {
        self.home.path().join("localappdata")
    }

    fn apply(&self, cmd: &mut Command) {
        cmd.env("HOME", self.home.path());
        cmd.env("USERPROFILE", self.home.path());
        cmd.env("LOCALAPPDATA", self.local_app_data());
        cmd.env("GIT_CONFIG_GLOBAL", self.home.path().join(".gitconfig"));
        cmd.env("GIT_CONFIG_NOSYSTEM", "1");
    }
}

/// An empty folder for `core.hooksPath`.
fn no_hooks(base: &Path) -> PathBuf {
    let hooks = base.join("no-hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    hooks
}

fn git(repo: &Path, hooks: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-c")
        .arg(format!("core.hooksPath={}", hooks.display()))
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .expect("git must be on PATH");
    assert!(out.status.success(), "git {:?} failed: {}", args, String::from_utf8_lossy(&out.stderr));
}

/// A real checkout with one commit. `marker` is committed as the checkout's
/// `.thor-project` when given.
fn checkout(dir: &Path, hooks: &Path, marker: Option<&str>, notes: &str) {
    std::fs::create_dir_all(dir).unwrap();
    git(dir, hooks, &["init", "--quiet"]);
    git(dir, hooks, &["config", "core.hooksPath", &hooks.display().to_string()]);
    git(dir, hooks, &["config", "user.email", "fixture@example.invalid"]);
    git(dir, hooks, &["config", "user.name", "install register tests"]);
    if let Some(key) = marker {
        std::fs::write(dir.join(".thor-project"), format!("{key}\n")).unwrap();
    }
    std::fs::write(dir.join("NOTES.md"), notes).unwrap();
    std::fs::write(dir.join("main.rs"), "fn main() { println!(\"a findable fixture sentence\"); }\n").unwrap();
    git(dir, hooks, &["add", "-A"]);
    git(dir, hooks, &["commit", "--quiet", "-m", "initial"]);
}

/// A clone of `from` at `to`, made with the hooks switched off and kept off.
fn clone(from: &Path, to: &Path, hooks: &Path) {
    let parent = to.parent().unwrap();
    git(parent, hooks, &["clone", "--quiet", &from.display().to_string(), &to.display().to_string()]);
    git(to, hooks, &["config", "core.hooksPath", &hooks.display().to_string()]);
}

fn slash(p: &Path) -> String {
    p.display().to_string().replace('\\', "/")
}

fn install_in(dir: &Path, sandbox: &Sandbox, args: &[&str]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_install"));
    cmd.current_dir(dir).args(args);
    sandbox.apply(&mut cmd);
    cmd.output().unwrap()
}

fn register(dir: &Path, sandbox: &Sandbox, db: &Path) -> Output {
    install_in(dir, sandbox, &["--register-root", "--db", &db.display().to_string()])
}

fn text(out: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr))
}

fn event_count(db: &Path) -> usize {
    EventStore::open_existing(db).unwrap().get_all_events().unwrap().len()
}

fn fresh_store(base: &Path) -> PathBuf {
    let db = base.join("thor.db");
    ops::install::ensure_store(&db).unwrap();
    db
}

fn registered(db: &Path, key: &str) -> Option<String> {
    let store = EventStore::open_existing(db).unwrap();
    model::store::registered_root_raw(&store, key).map(|(p, _)| slash(&p))
}

/// G2.1: the first run registers the checkout it is run from; a second run
/// says so and appends nothing.
#[test]
fn registers_the_checkout_and_a_second_run_writes_nothing() {
    let base = tempfile::tempdir().unwrap();
    let hooks = no_hooks(base.path());
    let beta = base.path().join("beta");
    checkout(&beta, &hooks, Some("beta"), "intro only\n");
    let db = fresh_store(base.path());
    let sandbox = Sandbox::new();

    let first = register(&beta, &sandbox, &db);
    assert!(first.status.success(), "{}", text(&first));
    assert!(text(&first).contains(&format!("registered 'beta' -> {}", slash(&beta))), "{}", text(&first));
    assert_eq!(registered(&db, "beta"), Some(slash(&beta)));

    let events = event_count(&db);
    let second = register(&beta, &sandbox, &db);
    assert!(second.status.success(), "{}", text(&second));
    assert!(text(&second).contains("already registered here"), "{}", text(&second));
    assert_eq!(event_count(&db), events, "a second run must append nothing");
}

/// R2.1: a live second copy of the checkout is refused, and nothing is
/// appended.
#[test]
fn a_live_second_copy_is_refused() {
    let base = tempfile::tempdir().unwrap();
    let hooks = no_hooks(base.path());
    let beta = base.path().join("beta");
    checkout(&beta, &hooks, Some("beta"), "intro only\n");
    let copy = base.path().join("beta-copy");
    clone(&beta, &copy, &hooks);
    std::fs::write(copy.join("NOTES.md"), "keep me\n").unwrap();
    let db = fresh_store(base.path());
    let sandbox = Sandbox::new();
    assert!(register(&beta, &sandbox, &db).status.success());
    let events = event_count(&db);

    let refused = register(&copy, &sandbox, &db);
    assert_eq!(refused.status.code(), Some(1), "{}", text(&refused));
    assert!(
        text(&refused).contains(&format!(
            "REFUSED: 'beta' is registered at {}, which is still a checkout of it - a second copy cannot take over its \
             registration. Nothing was changed.",
            slash(&beta)
        )),
        "{}",
        text(&refused)
    );
    assert_eq!(event_count(&db), events, "a refused registration must append nothing");
    assert_eq!(registered(&db, "beta"), Some(slash(&beta)));
}

/// R2.7 (D3): editing the old checkout's marker so it no longer resolves to
/// the project is not a move - the old folder still exists.
#[test]
fn a_changed_marker_in_an_existing_old_folder_is_refused() {
    let base = tempfile::tempdir().unwrap();
    let hooks = no_hooks(base.path());
    let beta = base.path().join("beta");
    checkout(&beta, &hooks, Some("beta"), "intro only\n");
    let copy = base.path().join("beta-copy");
    clone(&beta, &copy, &hooks);
    let db = fresh_store(base.path());
    let sandbox = Sandbox::new();
    assert!(register(&beta, &sandbox, &db).status.success());
    std::fs::write(beta.join(".thor-project"), "other\n").unwrap();
    let events = event_count(&db);

    let refused = register(&copy, &sandbox, &db);
    assert_eq!(refused.status.code(), Some(1), "{}", text(&refused));
    assert!(
        text(&refused).contains(&format!(
            "REFUSED: 'beta' is registered at {}, which still exists. Moving a registration is the owner's call: if \
             the project really moved, the owner removes or renames the old folder, then runs this again. Nothing \
             was changed.",
            slash(&beta)
        )),
        "{}",
        text(&refused)
    );
    assert_eq!(event_count(&db), events);
    assert_eq!(registered(&db, "beta"), Some(slash(&beta)));
}

/// G2.3: once the registered folder is gone, registering from where the
/// project now lives moves the registration - one revise by the installer -
/// and the session start names both paths.
#[test]
fn a_move_is_accepted_only_when_the_old_path_is_gone() {
    let base = tempfile::tempdir().unwrap();
    let hooks = no_hooks(base.path());
    let beta = base.path().join("beta");
    checkout(&beta, &hooks, Some("beta"), "intro only\n");
    let db = fresh_store(base.path());
    let sandbox = Sandbox::new();
    assert!(register(&beta, &sandbox, &db).status.success());

    let moved = base.path().join("beta-moved");
    std::fs::rename(&beta, &moved).unwrap();
    let out = register(&moved, &sandbox, &db);
    assert!(out.status.success(), "{}", text(&out));
    assert!(
        text(&out).contains(&format!(
            "moved 'beta' from {} to {} (the old checkout is gone)",
            slash(&beta),
            slash(&moved)
        )),
        "{}",
        text(&out)
    );
    assert_eq!(registered(&db, "beta"), Some(slash(&moved)));

    let store = EventStore::open_existing(&db).unwrap();
    let history = model::store::history(&store, "project-beta-opened-by-install").unwrap();
    let last = history.last().unwrap();
    assert_eq!((last.kind.as_str(), last.actor.as_str()), ("fact_revised", "installer"));
    let now = serve::time::now_unix();
    let notice = serve::project::registration_notice(&store, Some("beta"), now).expect("a fresh move is named");
    assert!(notice.contains(&slash(&moved)) && notice.contains(&format!("moved from {}", slash(&beta))), "{notice}");
    assert!(serve::project::registered_checkout(&store, Some("beta")).is_ok());
}

/// R2.3, R2.6 (D2): through the tool server, a revise that tries to move the
/// root (a `checkout-root:` tag) and a retract of the project's record are
/// both refused with the sentence that names the fix; the record stays live
/// and the registration unchanged.
#[tokio::test]
async fn an_mcp_revise_or_retract_never_changes_the_registration() {
    let base = tempfile::tempdir().unwrap();
    let hooks = no_hooks(base.path());
    let demo = base.path().join("demo");
    checkout(&demo, &hooks, Some("demo"), "intro only\n");
    let copy = base.path().join("copy");
    clone(&demo, &copy, &hooks);
    let db = fresh_store(base.path());
    let sandbox = Sandbox::new();
    assert!(register(&demo, &sandbox, &db).status.success());
    let events = event_count(&db);

    let server = mcp::ThorMcpServer::new(EventStore::new(&db).unwrap());
    let revise = thor_core::inbox::InboxOp::new(
        "revise",
        serde_json::json!({ "id": "project-demo-opened-by-install", "tags": ["project-record", format!("checkout-root:{}", slash(&copy))] }),
    );
    let retract = thor_core::inbox::InboxOp::new(
        "retract",
        serde_json::json!({ "id": "project-demo-opened-by-install", "reason": "this project record is in the way" }),
    );
    for op in [revise, retract] {
        match server.apply_captured(&op).await {
            mcp::CapturedOutcome::Applied(text) => panic!("{} went through: {text}", op.tool),
            mcp::CapturedOutcome::NotApplied(text) => {
                assert!(text.contains("this record is THOR's own registration of a project's working copy"), "{text}");
                assert!(text.contains("--register-root --db"), "{text}");
            }
        }
    }
    assert_eq!(event_count(&db), events, "nothing may be appended");
    let store = EventStore::open_existing(&db).unwrap();
    assert!(model::store::show(&store, "project-demo-opened-by-install").is_ok(), "the record stays live");
    assert_eq!(slash(&serve::project::registered_checkout(&store, Some("demo")).unwrap()), slash(&demo));
}

/// R2.8 (F7): `--register-root` without `--db` names the flag it needs and
/// writes nothing anywhere - the default data folder is not even created.
#[test]
fn register_root_without_db_is_refused_and_writes_nothing() {
    let base = tempfile::tempdir().unwrap();
    let hooks = no_hooks(base.path());
    let beta = base.path().join("beta");
    checkout(&beta, &hooks, Some("beta"), "intro only\n");
    let sandbox = Sandbox::new();

    let out = install_in(&beta, &sandbox, &["--register-root"]);
    assert_eq!(out.status.code(), Some(2), "{}", text(&out));
    let expected = format!(
        "name the memory to register in: install{} --register-root --db \"<path to thor.db>\" - without --db it would \
         register into",
        std::env::consts::EXE_SUFFIX
    );
    assert!(text(&out).contains(&expected), "{}", text(&out));
    assert!(text(&out).contains("which may not be the memory your assistant uses"), "{}", text(&out));
    assert!(!sandbox.local_app_data().exists(), "the default data folder must not be created");
    assert!(!beta.join("thor.db").exists());
}

/// G2.4: a project record retracted before this build (a raw tombstone, as
/// the tool server could once write) cannot be revised again, so the working
/// copy is registered under `project-<key>-root-by-install`.
#[test]
fn a_record_tombstoned_before_this_build_registers_under_its_own_record() {
    let base = tempfile::tempdir().unwrap();
    let hooks = no_hooks(base.path());
    let beta = base.path().join("beta");
    checkout(&beta, &hooks, Some("beta"), "intro only\n");
    let db = fresh_store(base.path());
    {
        let mut store = EventStore::new(&db).unwrap();
        let body = serde_json::json!({
            "id": "project-beta-opened-by-install", "kind": "report",
            "text": "Project beta: a record written before registration existed.",
            "bindings": [], "severity": null, "project": "beta", "tags": ["project-record"],
            "expires": null, "key": null, "falsifier": null, "check": null
        })
        .to_string();
        store.append_event("mcp", "mcp", "mcp", EventKind::FactCreated, "project-beta-opened-by-install", None, &body).unwrap();
        let tomb = serde_json::json!({ "retracted": "project-beta-opened-by-install", "reason": "a fixture retraction" }).to_string();
        store
            .append_mutate_checked("mcp", "mcp", "mcp", EventKind::FactRetracted, "project-beta-opened-by-install", None, &tomb)
            .unwrap();
    }
    let sandbox = Sandbox::new();

    let out = register(&beta, &sandbox, &db);
    assert!(out.status.success(), "{}", text(&out));
    assert!(text(&out).contains(&format!("registered 'beta' -> {}", slash(&beta))), "{}", text(&out));
    let store = EventStore::open_existing(&db).unwrap();
    assert!(model::store::show(&store, "project-beta-root-by-install").is_ok());
    let events = store.get_events_by_entity("project-beta-root-by-install").unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].actor, "installer");
    assert_eq!(registered(&db, "beta"), Some(slash(&beta)));
}

/// R2.2 and G2.2: the per-project install registers the checkout it reads;
/// a rerun from there writes nothing; and the same install run from a clone
/// is refused before anything is written - no marker in the clone, no
/// settings, no index, the record unchanged.
#[test]
fn a_per_project_install_from_a_clone_is_refused_before_anything_is_written() {
    let base = tempfile::tempdir().unwrap();
    let hooks = no_hooks(base.path());
    let real = base.path().join("demo-real");
    checkout(&real, &hooks, None, "intro only\n");
    let db = base.path().join("store").join("thor.db");
    let db_arg = db.display().to_string();
    let sandbox = Sandbox::new();

    let first = install_in(&real, &sandbox, &["--no-mcp", "--db", &db_arg, "--project", "demo"]);
    assert!(first.status.success(), "{}", text(&first));
    assert!(text(&first).contains(&format!("+ registered its working copy: {}", slash(&real))), "{}", text(&first));
    assert_eq!(registered(&db, "demo"), Some(slash(&real)));

    let events = event_count(&db);
    let again = install_in(&real, &sandbox, &["--no-mcp", "--db", &db_arg, "--project", "demo"]);
    assert!(again.status.success(), "{}", text(&again));
    assert!(text(&again).contains("= project record already names this checkout"), "{}", text(&again));
    assert_eq!(event_count(&db), events, "a rerun from the registered checkout writes nothing");

    let copy = base.path().join("copy");
    clone(&real, &copy, &hooks);
    let index = db.parent().unwrap().join("codeindex").join("demo.db");
    let index_before = std::fs::metadata(&index).unwrap().modified().unwrap();
    let other = Sandbox::new();
    let refused = install_in(&copy, &other, &["--no-mcp", "--db", &db_arg, "--project", "demo"]);
    assert_eq!(refused.status.code(), Some(1), "{}", text(&refused));
    assert!(
        text(&refused).contains(&format!(
            "REFUSED: 'demo' is registered at {}, which is still a checkout of it - a second copy cannot take over its \
             registration. Nothing was changed.",
            slash(&real)
        )),
        "{}",
        text(&refused)
    );
    assert!(!copy.join(".thor-project").exists(), "no marker may be written in the copy");
    assert!(!other.home.path().join(".claude").exists(), "no settings may be written");
    assert_eq!(std::fs::metadata(&index).unwrap().modified().unwrap(), index_before, "no index may be built");
    assert_eq!(event_count(&db), events, "the record is unchanged");
    assert_eq!(registered(&db, "demo"), Some(slash(&real)));
}

// ---------------------------------------------------------------- per machine
//
// One project checked out on two machines at different paths (opt-in). A run
// without `--per-machine` must stay exactly what it was.

fn install_as(machine: &str, dir: &Path, sandbox: &Sandbox, args: &[&str]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_install"));
    cmd.current_dir(dir).args(args).env("THOR_MACHINE_ID", machine);
    sandbox.apply(&mut cmd);
    cmd.output().unwrap()
}

fn registrations(db: &Path, key: &str) -> Vec<model::store::RootRegistration> {
    let store = EventStore::open_existing(db).unwrap();
    model::store::root_registrations(&store, key)
}

fn root_for(db: &Path, key: &str, machine: &str) -> Option<String> {
    let store = EventStore::open_existing(db).unwrap();
    model::store::registrations_for_machine(&store, key, machine).pop().map(|r| r.root)
}

/// One machine: the refusal text is byte for byte today's, and an ordinary
/// registration carries no machine id at all.
#[test]
fn one_machine_refusal_text_is_unchanged_and_writes_no_machine_id() {
    let base = tempfile::tempdir().unwrap();
    let hooks = no_hooks(base.path());
    let beta = base.path().join("beta");
    checkout(&beta, &hooks, Some("beta"), "intro only\n");
    let copy = base.path().join("beta-copy");
    clone(&beta, &copy, &hooks);
    let db = fresh_store(base.path());
    let sandbox = Sandbox::new();
    let db_arg = db.display().to_string();
    assert!(install_as("pc", &beta, &sandbox, &["--register-root", "--db", &db_arg]).status.success());
    assert!(registrations(&db, "beta").iter().all(|r| r.machine.is_none()), "no flag, no machine id");

    let refused = install_as("pc", &copy, &sandbox, &["--register-root", "--db", &db_arg]);
    assert_eq!(refused.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&refused.stderr).trim_end(),
        format!(
            "REFUSED: 'beta' is registered at {}, which is still a checkout of it - a second copy cannot take over its \
             registration. Nothing was changed.",
            slash(&beta)
        )
    );
}

/// Two machines, one project, two paths: each resolves its own. The laptop's
/// registration does not touch the PC's, and the PC (untagged, legacy) still
/// resolves for every machine that has no registration of its own.
#[test]
fn each_machine_resolves_its_own_working_copy() {
    let base = tempfile::tempdir().unwrap();
    let hooks = no_hooks(base.path());
    let pc = base.path().join("pc").join("beta");
    std::fs::create_dir_all(pc.parent().unwrap()).unwrap();
    checkout(&pc, &hooks, Some("beta"), "intro only\n");
    let laptop = base.path().join("laptop").join("beta");
    std::fs::create_dir_all(laptop.parent().unwrap()).unwrap();
    clone(&pc, &laptop, &hooks);
    let db = fresh_store(base.path());
    let sandbox = Sandbox::new();
    let db_arg = db.display().to_string();

    assert!(install_as("pc/user", &pc, &sandbox, &["--register-root", "--db", &db_arg]).status.success());
    // Without the flag the laptop is a second copy: refused, as ever.
    let plain = install_as("laptop/user", &laptop, &sandbox, &["--register-root", "--db", &db_arg]);
    assert_eq!(plain.status.code(), Some(1), "{}", text(&plain));
    let events = event_count(&db);

    let out = install_as("laptop/user", &laptop, &sandbox, &["--register-root", "--per-machine", "--db", &db_arg]);
    assert!(out.status.success(), "{}", text(&out));
    assert_eq!(event_count(&db), events + 1);
    assert_eq!(root_for(&db, "beta", "laptop/user"), Some(slash(&laptop)));
    assert_eq!(root_for(&db, "beta", "pc/user"), Some(slash(&pc)), "the legacy registration is the main machine's");
    assert_eq!(root_for(&db, "beta", "tablet/user"), Some(slash(&pc)), "an unknown machine falls back to the ordinary one");

    // The laptop again: nothing to write.
    let again = install_as("laptop/user", &laptop, &sandbox, &["--register-root", "--per-machine", "--db", &db_arg]);
    assert!(text(&again).contains("already registered here"), "{}", text(&again));
    assert_eq!(event_count(&db), events + 1);
    // The PC again, without the flag: its own registration, nothing written.
    let pc_again = install_as("pc/user", &pc, &sandbox, &["--register-root", "--db", &db_arg]);
    assert!(text(&pc_again).contains("already registered here"), "{}", text(&pc_again));
    assert_eq!(event_count(&db), events + 1);
}

/// The same machine with a second checkout is still refused, with or without
/// the flag.
#[test]
fn a_second_checkout_on_the_same_machine_is_still_refused() {
    let base = tempfile::tempdir().unwrap();
    let hooks = no_hooks(base.path());
    let beta = base.path().join("beta");
    checkout(&beta, &hooks, Some("beta"), "intro only\n");
    let copy = base.path().join("beta-copy");
    clone(&beta, &copy, &hooks);
    let db = fresh_store(base.path());
    let sandbox = Sandbox::new();
    let db_arg = db.display().to_string();
    assert!(install_as("laptop/user", &beta, &sandbox, &["--register-root", "--per-machine", "--db", &db_arg]).status.success());
    let events = event_count(&db);

    for args in [&["--register-root", "--per-machine", "--db", &db_arg][..], &["--register-root", "--db", &db_arg][..]] {
        let refused = install_as("laptop/user", &copy, &sandbox, args);
        assert_eq!(refused.status.code(), Some(1), "{}", text(&refused));
        assert!(text(&refused).contains("which is still a checkout of it - a second copy cannot take over"), "{}", text(&refused));
    }
    assert_eq!(event_count(&db), events);
}

/// The laptop's copy has one writer: `--ask-main` writes nothing and prints
/// the one line for the main machine, which records the laptop's path
/// without looking at the (absent) folder.
#[test]
fn a_copy_asks_the_main_machine_to_record_its_checkout() {
    let base = tempfile::tempdir().unwrap();
    let hooks = no_hooks(base.path());
    let pc = base.path().join("pc").join("beta");
    std::fs::create_dir_all(pc.parent().unwrap()).unwrap();
    checkout(&pc, &hooks, Some("beta"), "intro only\n");
    let laptop = base.path().join("laptop").join("beta");
    std::fs::create_dir_all(laptop.parent().unwrap()).unwrap();
    clone(&pc, &laptop, &hooks);
    let db = fresh_store(base.path());
    let sandbox = Sandbox::new();
    let db_arg = db.display().to_string();
    assert!(install_as("pc/user", &pc, &sandbox, &["--register-root", "--db", &db_arg]).status.success());
    let events = event_count(&db);

    let asked = install_as("laptop/user", &laptop, &sandbox, &["--register-root", "--per-machine", "--ask-main", "--db", &db_arg]);
    assert!(asked.status.success(), "{}", text(&asked));
    assert_eq!(event_count(&db), events, "a copy writes nothing");
    let said = text(&asked);
    assert!(said.contains("--register-machine-root \"laptop/user\" --project \"beta\""), "{said}");
    assert!(said.contains(&format!("--root \"{}\"", slash(&laptop))), "{said}");

    // The main machine runs that line; the laptop folder does not exist there.
    let elsewhere = base.path().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let main = install_as(
        "pc/user",
        &elsewhere,
        &sandbox,
        &["--register-machine-root", "laptop/user", "--project", "beta", "--root", "D:/laptop/beta", "--db", &db_arg],
    );
    assert!(main.status.success(), "{}", text(&main));
    assert_eq!(root_for(&db, "beta", "laptop/user"), Some("D:/laptop/beta".to_string()));
    assert_eq!(root_for(&db, "beta", "pc/user"), Some(slash(&pc)));
}
