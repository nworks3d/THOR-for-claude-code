//! The hook and a rule's trial run one decision pipeline
//! (`serve::guard_verdict`), and moving the hook's decisions there changed
//! nothing the hook says.
//!
//! THE ORACLE IS THE BUILD BEFORE THE MOVE, never the code under test. Each
//! `tests/data/hook_golden/<fixture>.<case>.golden` file holds the exit code
//! and stdout that the `serve.exe` built BEFORE the move printed for that case
//! (`tests/data/hook_golden/fixtures.rs` says how they were made and builds
//! the same stores and payloads again). Comparing the moved pipeline with
//! itself would prove nothing: a behaviour change would move both sides at
//! once.
//!
//! Three things are held here:
//! - the compiled `serve hook --no-record` prints exactly the golden output
//!   for every case, and there is a golden file for every case and a case for
//!   every golden file;
//! - `guard_verdict::pre_tool`, called directly on the same store with the
//!   same payload, refuses exactly where the golden output refuses, names the
//!   same rule, and words it the same (a held hook call keeps no marker, so
//!   every refusal is a first attempt's);
//! - the comparison itself can fail: one byte changed in one golden file (in
//!   a temporary copy) makes it name exactly that case.
//!
//! EDITED ON PURPOSE, by hand, when the door landed (`serve::shell`): four
//! golden files say what the hook now says, each for a change the door was
//! built to make. `forbidden-command.bash-wrapped` (`bash -c "git push --force
//! ..."`) and `forbidden-command.bash-after-cd` (`cd sub && git push --force
//! ...`) are refused now - a command tooth reaches every segment of a call,
//! nested ones included - with the same refusal as the bare
//! `forbidden-command.bash-force-push`. `forbidden-command.bash-force-with-lease`
//! is no longer refused (`--force` matches only on token boundaries, never
//! inside `--force-with-lease`) and is served like
//! `forbidden-command.bash-plain-push`. `requires-plain.bash-pip-help` (`pip
//! install --help`) is a pure help call, never refused by a requires check,
//! and is served like `requires-plain.bash-pip-no-deps`. Every other golden
//! file is byte for byte what the build before the move printed.

#[path = "data/hook_golden/fixtures.rs"]
mod fixtures;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("data").join("hook_golden")
}

fn serve_exe() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_serve"))
}

fn golden_files(dir: &Path) -> BTreeSet<String> {
    std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("the golden folder {} must be readable: {e}", dir.display()))
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .filter(|name| name.ends_with(".golden"))
        .collect()
}

/// Run every case through the compiled hook and compare with the golden files
/// in `golden`. Returns one line per case that differs (named first), one per
/// case without a golden file, and one per golden file without a case.
fn differential(golden: &Path) -> Vec<String> {
    let base = tempfile::Builder::new().prefix("hookgolden").tempdir().unwrap();
    let payloads = base.path().join("payloads");
    let mut problems = Vec::new();
    let mut cases = BTreeSet::new();
    for fixture in fixtures::build_all(base.path()) {
        for case in &fixture.cases {
            let name = case.file_name();
            assert!(cases.insert(name.clone()), "two cases share the golden file name {name}");
            let (code, stdout, stderr) = fixtures::run_hook(serve_exe(), &fixture.db, &payloads, case);
            if !stderr.is_empty() {
                problems.push(format!("{name}: the hook wrote to stderr: {stderr}"));
            }
            let actual = fixtures::golden_text(code, &fixtures::normalize(&stdout, base.path(), serve_exe()));
            match std::fs::read_to_string(golden.join(&name)) {
                Ok(expected) if expected == actual => {}
                Ok(expected) => problems.push(format!("{name}: differs from its golden file\n  golden: {expected:?}\n  now:    {actual:?}")),
                Err(e) => problems.push(format!("{name}: no golden file ({e})")),
            }
        }
    }
    for orphan in golden_files(golden).difference(&cases) {
        problems.push(format!("{orphan}: a golden file with no case - a case was removed or renamed"));
    }
    problems
}

#[test]
fn the_hook_prints_exactly_what_the_build_before_the_move_printed() {
    let problems = differential(&golden_dir());
    assert!(
        problems.is_empty(),
        "the hook no longer says what it said before its decisions moved into serve::guard_verdict. \
         If a change of behaviour is intended, edit the one golden file it changes by hand, on purpose, \
         never by regenerating them all from the build under test:\n{}",
        problems.join("\n")
    );
}

#[test]
fn pre_tool_decides_exactly_what_the_hook_printed() {
    let base = tempfile::Builder::new().prefix("hookgolden").tempdir().unwrap();
    let mut refused = 0;
    let mut let_through = 0;
    for fixture in fixtures::build_all(base.path()) {
        let store = thor_core::event_store::EventStore::open_read_only(&fixture.db).unwrap();
        for case in &fixture.cases {
            let name = case.file_name();
            let golden = std::fs::read_to_string(golden_dir().join(&name))
                .unwrap_or_else(|e| panic!("{name}: no golden file ({e})"));
            let (_, golden_stdout) = golden.split_once('\n').unwrap_or_else(|| panic!("{name}: no exit line"));
            let golden_reason = serde_json::from_str::<serde_json::Value>(golden_stdout)
                .ok()
                .filter(|v| v.get("decision") == Some(&serde_json::json!("block")))
                .and_then(|v| v.get("reason").and_then(|r| r.as_str()).map(str::to_string));

            // The values the hook resolves from the payload's working
            // directory before it asks the pipeline anything: the session's
            // project, the top of the checkout, and the folder itself.
            let cwd = PathBuf::from(case.payload["cwd"].as_str().unwrap());
            let session = case.payload["session_id"].as_str().unwrap_or("");
            let folder = serve::project::resolve_project(&cwd);
            let project = serve::project::session_project(&fixture.db, session, Some(&cwd));
            let root = serve::project::guard_root(&store, Some(&cwd), folder.as_deref(), project.as_deref());
            let tool_name = case.payload["tool_name"].as_str().unwrap_or("");
            let judgement = serve::guard_verdict::pre_tool(
                &store,
                tool_name,
                case.payload.get("tool_input"),
                project.as_deref(),
                root.as_deref(),
                Some(&cwd),
            );

            match (golden_reason, judgement.verdict) {
                (None, None) => let_through += 1,
                (Some(reason), Some(verdict)) => {
                    refused += 1;
                    // The block exactly as the hook prints it, normalized the
                    // same way the golden file was.
                    let printed = serde_json::json!({ "decision": "block", "reason": verdict.reason }).to_string();
                    assert_eq!(
                        fixtures::normalize(&printed, base.path(), serve_exe()),
                        golden_stdout,
                        "{name}: pre_tool words the refusal differently from the hook"
                    );
                    assert_eq!(
                        verdict.rule_id.as_deref(),
                        serve::absent_guard::rule_id_of(&reason),
                        "{name}: pre_tool names another rule than the hook"
                    );
                }
                (None, Some(verdict)) => panic!("{name}: pre_tool refuses ({}) where the hook let it through", verdict.reason),
                (Some(reason), None) => panic!("{name}: pre_tool lets through what the hook refused: {reason}"),
            }
        }
    }
    assert!(refused > 0 && let_through > 0, "both sides must be exercised: {refused} refused, {let_through} let through");
}

/// The red side of the oracle: a comparison that still passes with a golden
/// file changed would be comparing the pipeline with itself.
#[test]
fn one_changed_golden_byte_is_named() {
    let copy = tempfile::tempdir().unwrap();
    for name in golden_files(&golden_dir()) {
        std::fs::copy(golden_dir().join(&name), copy.path().join(&name)).unwrap();
    }
    let changed = golden_files(copy.path())
        .into_iter()
        .find(|name| std::fs::read(copy.path().join(name)).unwrap().len() > "exit 0\n".len() + 2)
        .expect("at least one golden file prints something");
    let path = copy.path().join(&changed);
    let mut bytes = std::fs::read(&path).unwrap();
    let middle = bytes.len() / 2;
    bytes[middle] ^= 0x01;
    std::fs::write(&path, bytes).unwrap();

    let problems = differential(copy.path());
    assert_eq!(problems.len(), 1, "exactly the changed case must differ: {problems:#?}");
    assert!(problems[0].starts_with(&format!("{changed}: differs")), "the difference must name {changed}: {problems:#?}");
}
