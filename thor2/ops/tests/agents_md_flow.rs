//! The zero-touch install path, as a fresh agent meets it: the top of
//! AGENTS.md is the whole job in at most five numbered steps, the install
//! scripts fetch exactly what the release workflow builds, and the page lists
//! the starter notes a new memory is seeded with.
//!
//! WHY TESTS FOR A PAGE. The owner's goal (2026-10-09): a friend sends one
//! sentence to their agent - the repository link and "install THOR" - and
//! gets the owner's own working experience, touching nothing but one restart.
//! That only holds while three files agree: the page the agent reads, the
//! scripts it runs and the notes the store is seeded with. Each can drift
//! from the others without any build failing, and the drift is silent: an
//! agent that follows a page naming a step that no longer exists simply
//! stops, and asks the person. These tests are that agreement, written down.

use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read(relative: &str) -> String {
    let path = repo_root().join(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

/// The lines of AGENTS.md between the heading that opens the numbered steps
/// and the first horizontal rule after it.
fn top_steps(agents: &str) -> Vec<String> {
    let mut lines = agents.lines();
    let found = lines.by_ref().any(|l| l.starts_with("## The whole install"));
    assert!(found, "AGENTS.md has no '## The whole install' heading above the details");
    lines.take_while(|l| l.trim() != "---").map(str::to_string).collect()
}

/// `N. ` at the start of a line: a top-level numbered step.
fn step_numbers(block: &[String]) -> Vec<u32> {
    block
        .iter()
        .filter_map(|l| {
            let (digits, rest) = l.split_once(". ")?;
            if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) || rest.trim().is_empty() {
                return None;
            }
            digits.parse().ok()
        })
        .collect()
}

#[test]
fn agents_md_opens_with_at_most_five_numbered_steps() {
    let agents = read("AGENTS.md");
    let steps = step_numbers(&top_steps(&agents));
    assert!(!steps.is_empty(), "the opening block has no numbered step");
    assert!(steps.len() <= 5, "the opening block has {} steps, the limit is five: {steps:?}", steps.len());
    let want: Vec<u32> = (1..=steps.len() as u32).collect();
    assert_eq!(steps, want, "the steps are not numbered 1..n in order");
}

#[test]
fn the_opening_steps_need_the_person_for_the_restart_alone() {
    let agents = read("AGENTS.md");
    let block = top_steps(&agents).join("\n").to_lowercase();
    assert!(block.contains("restart"), "the one thing only the person can do is not named at the top");
    // Nothing at the top may send the agent back to ask a question or to
    // hand the person a menu: those are the touch points this removes.
    for forbidden in ["ask them which", "ask which", "let them choose", "do you want", "which would you like"] {
        assert!(!block.contains(forbidden), "the opening steps ask the person something: '{forbidden}'");
    }
    // A Rust toolchain is for the unusual platform, never the first move.
    let first_cargo = block.find("cargo");
    let first_script = block.find("install.ps1").or_else(|| block.find("install.sh"));
    assert!(first_script.is_some(), "the opening steps never name the install scripts");
    if let (Some(cargo), Some(script)) = (first_cargo, first_script) {
        assert!(script < cargo, "the opening steps reach for a Rust build before the release download");
    }
}

#[test]
fn agents_md_names_the_scripts_by_their_real_address() {
    let agents = read("AGENTS.md");
    for script in ["install.ps1", "install.sh"] {
        let url = format!("https://raw.githubusercontent.com/nworks3d/THOR-memory-bible-for-AI/main/{script}");
        assert!(agents.contains(&url), "AGENTS.md never gives the address of {script}: {url}");
    }
}

#[test]
fn agents_md_lists_every_starter_note_and_says_any_can_go() {
    let agents = read("AGENTS.md");
    let section = agents
        .split("# First session with a new owner")
        .nth(1)
        .expect("AGENTS.md lost its 'First session with a new owner' section");
    for item in ops::install::workflow_starter_pack() {
        assert!(section.contains(&item.id), "the first-session section never lists the starter note '{}'", item.id);
    }
    let lowered = section.to_lowercase();
    assert!(lowered.contains("starter"), "the section never names the starter set");
    assert!(
        lowered.contains("retract") || lowered.contains("remove"),
        "the section never says a starter note can be removed"
    );
}

#[test]
fn the_component_readme_counts_the_starter_pack_right() {
    let words = ["Ten", "Eleven", "Twelve", "Thirteen", "Fourteen", "Fifteen", "Sixteen", "Seventeen", "Eighteen"];
    let n = ops::install::workflow_starter_pack().len();
    let word = words.get(n.wrapping_sub(10)).unwrap_or_else(|| panic!("add the word for {n} to this test"));
    let readme = read("thor2/README.md");
    assert!(
        readme.contains(&format!("{word} more notes make up the workflow starter pack")),
        "thor2/README.md does not say '{word} more notes make up the workflow starter pack' ({n} notes in the pack)"
    );
}

#[test]
fn the_first_session_has_defaults_that_never_block() {
    let agents = read("AGENTS.md");
    let section = agents.split("# First session with a new owner").nth(1).unwrap().to_lowercase();
    assert!(section.contains("defaults"), "the first-session section names no defaults");
    assert!(
        section.contains("do not wait") || section.contains("never wait") || section.contains("without waiting"),
        "the first-session section never says the defaults apply without waiting for an answer"
    );
    assert!(section.contains("owner-setup-answers"), "the section never says where the answers are recorded");
}

/// The assets the release workflow builds, by the names it gives them.
fn release_assets() -> Vec<String> {
    read(".github/workflows/release.yml")
        .lines()
        .filter_map(|l| l.trim().strip_prefix("asset:").map(|a| a.trim().to_string()))
        .collect()
}

#[test]
fn the_install_scripts_fetch_assets_the_release_workflow_builds() {
    let assets = release_assets();
    assert!(assets.len() >= 3, "release.yml names fewer assets than expected: {assets:?}");
    for (script, asset) in [("install.sh", "thor2-linux-x86_64.tar.gz"), ("install.ps1", "thor2-windows-x86_64.zip")] {
        assert!(assets.iter().any(|a| a == asset), "release.yml does not build {asset}: {assets:?}");
        assert!(read(script).contains(asset), "{script} does not fetch {asset}");
    }
}

#[test]
fn the_install_scripts_can_be_pointed_at_another_release_folder() {
    // Lets a mirror, a local folder or a test serve the archive; without it
    // the only way to run the real script is against the live internet.
    for script in ["install.sh", "install.ps1"] {
        let text = read(script);
        assert!(text.contains("THOR_RELEASE_BASE"), "{script} cannot be pointed at another release folder");
    }
}

#[test]
fn the_windows_script_checks_the_c_runtime_before_it_runs_anything() {
    let text = read("install.ps1");
    let check = text.to_lowercase().find("vcruntime140").expect("install.ps1 never looks for the C runtime");
    let run = text.find("install.exe").expect("install.ps1 never runs install.exe");
    assert!(check < run, "the runtime check comes after the first run of install.exe");
    assert!(text.contains("winget"), "install.ps1 does not print the command that installs the runtime");
}
