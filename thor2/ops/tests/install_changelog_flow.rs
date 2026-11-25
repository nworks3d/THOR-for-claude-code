//! The changelog flow, asked once per code project: the installer prints the
//! question when it registers a git project for the first time (not for a
//! second run, not for a folder that is not a code project), `install
//! --set-changelog-flow on|off` records the answer per project and is safe to
//! repeat, an older memory gets the flow notes when the answer is yes, and with
//! "off" the notes are not selected in that project.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use thor_core::event_store::EventStore;

const OLD_TEXT: &str = "If the project keeps a CHANGELOG, add a line for every user-visible change in the same commit, and bump the version when releases are versioned. Create a changelog only if the owner said yes to first-session item 12; else keep what exists. Re-read the README for claims made untrue.";
const QUESTION_START: &str = "Shall I keep a changelog and version numbers for this project?";

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
    fn run(&self, cwd: &Path, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_install"))
            .current_dir(cwd)
            .args(args)
            .env("HOME", self.home.path())
            .env("USERPROFILE", self.home.path())
            .env("LOCALAPPDATA", self.home.path().join("localappdata"))
            .env("GIT_CONFIG_GLOBAL", self.home.path().join(".gitconfig"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap()
    }
    fn db_args<'a>(&self, db: &'a str, rest: &[&'a str]) -> Vec<&'a str> {
        let mut v = rest.to_vec();
        v.extend(["--db", db]);
        v
    }
}

fn said(out: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr))
}

fn git(repo: &Path, args: &[&str]) {
    let out = Command::new("git").arg("-C").arg(repo).args(args).output().expect("git must be on PATH");
    assert!(out.status.success(), "git {:?} failed: {}", args, String::from_utf8_lossy(&out.stderr));
}

fn git_repo(parent: &Path, name: &str) -> PathBuf {
    let dir = parent.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "--quiet"]);
    let hooks = parent.join("no-hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    git(&dir, &["config", "core.hooksPath", &hooks.display().to_string()]);
    git(&dir, &["config", "user.email", "test@example.invalid"]);
    git(&dir, &["config", "user.name", "install cli tests"]);
    std::fs::write(dir.join("main.rs"), "fn main() {}\n").unwrap();
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "--quiet", "-m", "initial"]);
    dir
}

/// During the very first install the welcome carries the question (proposal 4),
/// so the installer does not tell the assistant to ask it now; no answer is
/// recorded, so the safety net at the first commit still applies.
#[test]
fn during_the_first_install_the_welcome_carries_the_question_and_the_installer_asks_nothing() {
    let sandbox = Sandbox::new();
    let base = tempfile::tempdir().unwrap();
    let repo = git_repo(base.path(), "demo-shop");

    let first = sandbox.run(&repo, &["--no-mcp"]);
    assert!(first.status.success(), "{}", said(&first));
    let text = said(&first);
    assert!(!text.contains("ONE QUESTION FOR THE PERSON") && !text.contains(QUESTION_START), "{text}");
    assert!(text.contains("part of the welcome in the first session (proposal 4)"), "{text}");
    assert_eq!(serve::changelog_flow::answer(&sandbox.db(), "demo-shop"), None, "no answer is made up");
}

#[test]
fn the_first_registration_of_a_later_git_project_asks_and_a_second_run_does_not() {
    let sandbox = Sandbox::new();
    let base = tempfile::tempdir().unwrap();
    let repo = git_repo(base.path(), "demo-shop");
    // A memory whose first session is long over: no setup note waits in it.
    std::fs::create_dir_all(sandbox.db().parent().unwrap()).unwrap();
    ops::install::ensure_store(&sandbox.db()).unwrap();

    let first = sandbox.run(&repo, &["--no-mcp"]);
    assert!(first.status.success(), "{}", said(&first));
    let text = said(&first);
    assert!(text.contains("ONE QUESTION FOR THE PERSON about 'demo-shop'"), "{text}");
    assert!(text.contains(QUESTION_START), "{text}");
    assert!(text.contains("a fault is found much faster") && text.contains("saves effort and cost"), "both reasons come with the question: {text}");
    assert!(text.contains("--set-changelog-flow on") && text.contains("--set-changelog-flow off"), "{text}");

    let second = sandbox.run(&repo, &["--no-mcp"]);
    assert!(second.status.success(), "{}", said(&second));
    assert!(!said(&second).contains(QUESTION_START), "a second run must not ask again: {}", said(&second));
}

#[test]
fn an_answer_on_record_means_the_question_is_not_printed() {
    let sandbox = Sandbox::new();
    let base = tempfile::tempdir().unwrap();
    let shop = git_repo(base.path(), "demo-shop");
    let blog = git_repo(base.path(), "demo-blog");
    // The memory exists once the first run made it; the blog is answered before it is registered.
    let first = sandbox.run(&shop, &["--no-mcp"]);
    assert!(first.status.success(), "{}", said(&first));
    serve::changelog_flow::set(&sandbox.db(), "demo-blog", false).unwrap();
    let after = sandbox.run(&blog, &["--no-mcp"]);
    assert!(after.status.success(), "{}", said(&after));
    assert!(said(&after).contains("recorded this project, 'demo-blog'"), "it was a first registration: {}", said(&after));
    assert!(!said(&after).contains(QUESTION_START), "{}", said(&after));
}

#[test]
fn a_topic_folder_is_never_asked() {
    let sandbox = Sandbox::new();
    let base = tempfile::tempdir().unwrap();
    let plain = base.path().join("kitchen-renovation");
    std::fs::create_dir_all(&plain).unwrap();
    let out = sandbox.run(&plain, &["--no-mcp", "--topic"]);
    assert!(out.status.success(), "{}", said(&out));
    assert!(!said(&out).contains(QUESTION_START), "{}", said(&out));
    assert!(!said(&out).contains("changelog"), "{}", said(&out));
}

#[test]
fn the_answer_is_recorded_per_project_read_back_and_a_repeat_changes_nothing() {
    let sandbox = Sandbox::new();
    let base = tempfile::tempdir().unwrap();
    let shop = git_repo(base.path(), "demo-shop");
    let blog = git_repo(base.path(), "demo-blog");
    let first = sandbox.run(&shop, &["--no-mcp"]);
    assert!(first.status.success(), "{}", said(&first));
    let db = sandbox.db();
    let db_s = db.display().to_string();

    let off = sandbox.run(&shop, &sandbox.db_args(&db_s, &["--set-changelog-flow", "off"]));
    assert!(off.status.success(), "{}", said(&off));
    assert!(said(&off).contains("OFF for 'demo-shop'"), "{}", said(&off));
    assert_eq!(serve::changelog_flow::answer(&db, "demo-shop"), Some(false));
    assert_eq!(serve::changelog_flow::answer(&db, "demo-blog"), None, "another project is not touched");

    let again = sandbox.run(&shop, &sandbox.db_args(&db_s, &["--set-changelog-flow", "off"]));
    assert!(said(&again).contains("already the answer"), "{}", said(&again));
    let on_blog = sandbox.run(&blog, &sandbox.db_args(&db_s, &["--set-changelog-flow", "on"]));
    assert!(on_blog.status.success(), "{}", said(&on_blog));
    assert!(said(&on_blog).contains("ON for 'demo-blog'") && said(&on_blog).contains("0.1.0"), "{}", said(&on_blog));
    assert_eq!(serve::changelog_flow::answer(&db, "demo-blog"), Some(true));
    assert_eq!(serve::changelog_flow::answer(&db, "demo-shop"), Some(false));

    // Not a code project, and not a word it knows: refused, nothing written.
    let plain = base.path().join("notes");
    std::fs::create_dir_all(&plain).unwrap();
    let refused = sandbox.run(&plain, &sandbox.db_args(&db_s, &["--set-changelog-flow", "on"]));
    assert!(!refused.status.success() && said(&refused).contains("REFUSED"), "{}", said(&refused));
    let bad = sandbox.run(&shop, &sandbox.db_args(&db_s, &["--set-changelog-flow", "maybe"]));
    assert!(!bad.status.success(), "{}", said(&bad));
}

#[test]
fn a_new_memory_has_the_flow_notes_and_off_leaves_them_selected_elsewhere_only() {
    let sandbox = Sandbox::new();
    let db = sandbox.db();
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    ops::install::ensure_store(&db).unwrap();
    let seeded = ops::install::seed_working_contract(&db).unwrap();
    assert!(seeded.iter().all(|s| s.stored), "{:?}", seeded.iter().filter(|s| !s.stored).collect::<Vec<_>>());
    for id in serve::changelog_flow::NOTE_IDS {
        let item = model::store::show(&EventStore::open_read_only(&db).unwrap(), id).unwrap_or_else(|e| panic!("{id}: {e}"));
        assert!(item.tags.iter().any(|t| t == serve::changelog_flow::TAG), "{id} is not tagged as the changelog flow");
        assert!(!item.bindings.contains(&model::item::Binding::Always), "{id} must never be pinned");
        assert!(item.check.is_none() && item.falsifier.is_some(), "{id}: no refusing check, and a falsifier");
    }

    serve::changelog_flow::set(&db, "demo-shop", false).unwrap();
    let store = EventStore::open_read_only(&db).unwrap();
    let candidates = serve::live::live_items(&store);
    let decay = serve::decay::DecayContext::load(&store);
    let at_commit = |project: &str| {
        let mut input = serve::input::ServeInput { project: Some(project.to_string()), ..Default::default() };
        input.add_command("git commit -m x");
        let selected = serve::decay::retain_live_at(serve::rank::select(&candidates, &input), &decay, &input);
        selected.into_iter().map(|r| r.id).collect::<Vec<_>>()
    };
    let shop = at_commit("demo-shop");
    let blog = at_commit("demo-blog");
    assert!(!shop.iter().any(|id| id == "changelog-with-every-user-visible-change"), "not selected where the answer is no: {shop:?}");
    assert!(shop.iter().any(|id| id == "commit-hygiene"), "every other commit note stays: {shop:?}");
    assert!(blog.iter().any(|id| id == "changelog-with-every-user-visible-change"), "selected in another project: {blog:?}");
}

/// The release notes are bound to the commands a release is made with, so the
/// commit, push and publish places keep room for the owner's own rule.
#[test]
fn the_release_notes_arrive_at_a_tag_and_a_release_and_no_place_holds_more_than_three_starters() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("thor.db");
    ops::install::ensure_store(&db).unwrap();
    let seeded = ops::install::seed_working_contract(&db).unwrap();
    assert!(seeded.iter().all(|s| s.stored), "{:?}", seeded.iter().filter(|s| !s.stored).collect::<Vec<_>>());
    let store = EventStore::open_read_only(&db).unwrap();
    let candidates = serve::live::live_items(&store);
    let decay = serve::decay::DecayContext::load(&store);
    let at = |command: &str| {
        let mut input = serve::input::ServeInput::default();
        input.add_command(command);
        let selected = serve::decay::retain_live_at(serve::rank::select(&candidates, &input), &decay, &input);
        selected.into_iter().map(|r| r.id).collect::<Vec<_>>()
    };
    for command in ["gh release create v0.1.0 --notes x", "git tag v0.1.0"] {
        let ids = at(command);
        for id in &serve::changelog_flow::NOTE_IDS[1..] {
            assert!(ids.iter().any(|i| i == id), "{id} is not served at '{command}': {ids:?}");
        }
    }
    let starters_at_commit = at("git commit -m x").into_iter().filter(|id| serve::changelog_flow::NOTE_IDS.contains(&id.as_str()) || id.starts_with("commit-") || id.starts_with("never-commit")).count();
    assert!(starters_at_commit <= 3, "a commit holds {starters_at_commit} starter notes");
}

#[test]
fn an_older_memory_gets_the_flow_notes_when_the_answer_is_yes() {
    let sandbox = Sandbox::new();
    let base = tempfile::tempdir().unwrap();
    let repo = git_repo(base.path(), "demo-shop");
    let db = sandbox.db();
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    ops::install::ensure_store(&db).unwrap();
    {
        // An older memory: it holds the commit note with its OLD starter text, and none of the release notes.
        let mut store = EventStore::new(&db).unwrap();
        let mut old = ops::install::changelog_commit_note();
        old.text = OLD_TEXT.to_string();
        old.tags = vec!["workflow-starter".to_string()];
        model::store::declare(&mut store, "s", "l", "a", &old).unwrap();
    }
    let db_s = db.display().to_string();
    let on = sandbox.run(&repo, &sandbox.db_args(&db_s, &["--set-changelog-flow", "on"]));
    assert!(on.status.success(), "{}", said(&on));
    let text = said(&on);
    assert!(text.contains("rewrote the note 'changelog-with-every-user-visible-change'"), "{text}");
    for id in &serve::changelog_flow::NOTE_IDS[1..] {
        assert!(text.contains(&format!("stored the note '{id}'")), "{id}: {text}");
    }
    let store = EventStore::open_read_only(&db).unwrap();
    let commit = model::store::show(&store, "changelog-with-every-user-visible-change").unwrap();
    assert_eq!(commit.text, ops::install::changelog_commit_note().text);
    assert!(commit.tags.iter().any(|t| t == serve::changelog_flow::TAG));
    for want in ops::install::changelog_flow_notes() {
        assert_eq!(model::store::show(&store, &want.id).unwrap().text, want.text, "{}", want.id);
    }
    let events_before = store.get_all_events().unwrap().len();
    drop(store);

    let again = sandbox.run(&repo, &sandbox.db_args(&db_s, &["--set-changelog-flow", "on"]));
    assert!(again.status.success(), "{}", said(&again));
    assert!(said(&again).contains("already the answer"), "{}", said(&again));
    assert_eq!(EventStore::open_read_only(&db).unwrap().get_all_events().unwrap().len(), events_before, "a second run writes nothing");
}

#[test]
fn a_commit_note_the_owner_changed_by_hand_is_left_alone() {
    let sandbox = Sandbox::new();
    let base = tempfile::tempdir().unwrap();
    let repo = git_repo(base.path(), "demo-shop");
    let db = sandbox.db();
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    ops::install::ensure_store(&db).unwrap();
    {
        let mut store = EventStore::new(&db).unwrap();
        let mut mine = ops::install::changelog_commit_note();
        mine.text = "Our own rule: every change gets a line in the CHANGES file.".to_string();
        model::store::declare(&mut store, "s", "l", "a", &mine).unwrap();
    }
    let db_s = db.display().to_string();
    let on = sandbox.run(&repo, &sandbox.db_args(&db_s, &["--set-changelog-flow", "on"]));
    assert!(on.status.success(), "{}", said(&on));
    assert!(said(&on).contains("left the note 'changelog-with-every-user-visible-change' as it is"), "{}", said(&on));
    let store = EventStore::open_read_only(&db).unwrap();
    assert!(model::store::show(&store, "changelog-with-every-user-visible-change").unwrap().text.starts_with("Our own rule"));
}
