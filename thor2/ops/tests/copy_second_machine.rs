//! The second-machine flow end to end, with the real programs and no network:
//! one `install --copy-from` builds a SELECTIVE read-only copy from a backup
//! folder, `doctor` says what the copy is, and a later `sync pull` repeats the
//! remembered selection by itself.

use model::item::{Binding, Item, Kind};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use thor_core::backup::export_segments;
use thor_core::event_store::EventStore;

const CANARY: &str = "CANARYBETAENDTOEND";

fn rule(id: &str, project: Option<&str>, words: &str) -> Item {
    Item {
        id: id.to_string(),
        kind: Kind::Rule,
        text: format!("{id} {words}"),
        bindings: vec![Binding::Always],
        severity: None,
        project: project.map(str::to_string),
        tags: vec![],
        expires: None,
        key: None,
        falsifier: Some("it stops being true".to_string()),
        check: None,
    }
}

struct Fx {
    home: tempfile::TempDir,
    main: EventStore,
}

impl Fx {
    fn new() -> Fx {
        let home = tempfile::tempdir().unwrap();
        let mut main = EventStore::new(&home.path().join("main.db")).unwrap();
        for item in [
            rule("alpha-one", Some("alpha"), "guards the alpha pipeline every single release"),
            rule("beta-one", Some("beta"), &format!("{CANARY} describes the beta customer contract")),
            rule("general-one", None, "never commit credentials into any repository whatsoever"),
        ] {
            model::store::declare(&mut main, "s", "s", "s", &item).unwrap();
        }
        let fx = Fx { home, main };
        std::fs::create_dir_all(fx.backup().join("thor2")).unwrap();
        fx.publish();
        fx
    }
    fn publish(&self) {
        export_segments(&self.main, &self.backup().join("thor2"), 2_000).unwrap();
    }
    fn backup(&self) -> PathBuf {
        self.home.path().join("nas-backup")
    }
    fn db(&self) -> PathBuf {
        self.home.path().join("localappdata").join("thor2").join("thor.db")
    }
    fn settings(&self) -> PathBuf {
        self.home.path().join(".claude").join("settings.json")
    }
    fn command(&self, exe: &str) -> Command {
        let mut command = Command::new(exe);
        command
            .current_dir(self.home.path())
            .env("HOME", self.home.path())
            .env("USERPROFILE", self.home.path())
            .env("LOCALAPPDATA", self.home.path().join("localappdata"))
            .env("GIT_CONFIG_GLOBAL", self.home.path().join(".gitconfig"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("THOR_COPY_FROM")
            .env_remove("THOR_COPY_SCOPES")
            .env_remove("THOR_COPY_GLOBAL")
            .env_remove("THOR_COPY");
        command
    }
    fn install(&self, extra: &[&str]) -> Output {
        self.command(env!("CARGO_BIN_EXE_install"))
            .arg("--db")
            .arg(self.db())
            .arg("--settings")
            .arg(self.settings())
            // Any program that answers --version stands in for the hook program.
            .arg("--serve-exe")
            .arg(env!("CARGO_BIN_EXE_sync"))
            .args(extra)
            .output()
            .unwrap()
    }
}

fn said(out: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr))
}

fn bytes_of(db: &Path) -> Vec<u8> {
    let mut all = Vec::new();
    for suffix in ["", "-wal", "-shm"] {
        let mut name = db.as_os_str().to_owned();
        name.push(suffix);
        all.extend(std::fs::read(PathBuf::from(name)).unwrap_or_default());
    }
    all
}

fn has(haystack: &[u8], needle: &str) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle.as_bytes())
}

#[test]
fn one_command_makes_a_selective_copy_and_doctor_describes_it() {
    let fx = Fx::new();
    let out = fx.install(&["--copy-from", &fx.backup().display().to_string(), "--scope", "alpha", "--include-global"]);
    let text = said(&out);
    assert!(out.status.success(), "{text}");
    assert!(text.contains("what it holds: project alpha, general rules (no project)"), "{text}");
    assert!(text.contains("ONE THING LEFT FOR YOU"), "{text}");
    let copy = bytes_of(&fx.db());
    assert!(has(&copy, "alpha pipeline"));
    assert!(!has(&copy, CANARY), "the other project is not in the copy file");
    assert!(thor_core::copy_mode::is_copy(&fx.db()));
    let hooks = std::fs::read_to_string(fx.settings()).unwrap();
    assert!(hooks.contains("SessionStart") && hooks.contains("Stop"));
    assert!(!fx.home.path().join(".claude.json").exists(), "no tool server that could write to the copy");

    let doctor = fx.command(env!("CARGO_BIN_EXE_doctor")).arg("--db").arg(fx.db()).output().unwrap();
    let report = said(&doctor);
    assert!(report.contains("this is a copy, selection: project alpha, general rules (no project), last pull:"), "{report}");
    assert!(report.contains("2 items"), "{report}");
    assert!(report.contains("chain intact"), "{report}");
    assert!(!report.contains("CHAIN BROKEN") && !report.contains("PROBLEM: your memory"), "{report}");
    assert!(!report.contains(CANARY));
}

#[test]
fn the_download_one_liner_passes_its_choices_as_environment_variables() {
    let fx = Fx::new();
    let out = fx
        .command(env!("CARGO_BIN_EXE_install"))
        .arg("--db")
        .arg(fx.db())
        .arg("--settings")
        .arg(fx.settings())
        .arg("--serve-exe")
        .arg(env!("CARGO_BIN_EXE_sync"))
        .env("THOR_COPY_FROM", fx.backup())
        .env("THOR_COPY_SCOPES", "beta")
        .env("THOR_COPY_GLOBAL", "1")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", said(&out));
    let marker = thor_core::copy_mode::read_marker(&fx.db());
    assert_eq!(marker.scopes, vec!["beta".to_string()]);
    assert!(marker.include_global);
}

#[test]
fn a_later_pull_repeats_the_remembered_selection_without_options() {
    let mut fx = Fx::new();
    let out = fx.install(&["--copy-from", &fx.backup().display().to_string(), "--scope", "alpha"]);
    assert!(out.status.success(), "{}", said(&out));
    model::store::declare(&mut fx.main, "s", "s", "s", &rule("alpha-two", Some("alpha"), "a second alpha rule about nightly builds")).unwrap();
    model::store::declare(&mut fx.main, "s", "s", "s", &rule("beta-two", Some("beta"), "CANARYBETATWO another beta clause")).unwrap();
    fx.publish();
    let pull = fx.command(env!("CARGO_BIN_EXE_sync")).args(["pull", "--db"]).arg(fx.db()).output().unwrap();
    assert!(pull.status.success(), "{}", said(&pull));
    let copy = bytes_of(&fx.db());
    assert!(has(&copy, "second alpha rule"), "the new project item arrived");
    assert!(!has(&copy, "CANARYBETATWO") && !has(&copy, CANARY), "and the other project still stays out");
    assert!(!has(&copy, "never commit credentials"), "the general rules were not asked for");
}

#[test]
fn a_command_without_a_selection_flag_misplaced_is_refused_before_anything_changes() {
    let fx = Fx::new();
    let out = fx.install(&["--scope", "alpha"]);
    assert!(!out.status.success());
    assert!(said(&out).contains("only work together with --copy-from"), "{}", said(&out));
    assert!(!fx.db().exists() && !fx.settings().exists());
}

#[test]
fn an_existing_memory_that_is_not_a_copy_is_left_alone() {
    let fx = Fx::new();
    std::fs::create_dir_all(fx.db().parent().unwrap()).unwrap();
    EventStore::new(&fx.db()).unwrap();
    let before = std::fs::read(fx.db()).unwrap();
    let out = fx.install(&["--copy-from", &fx.backup().display().to_string(), "--scope", "alpha"]);
    assert!(!out.status.success());
    assert!(said(&out).contains("not a copy"), "{}", said(&out));
    assert!(before == std::fs::read(fx.db()).unwrap());
    assert!(!fx.settings().exists());
}
