//! The second-machine pages name commands, options and environment variables a
//! person pastes. These tests hold the pages and the programs to the same
//! words: an option the page names must exist in the program's own help, and
//! the one-line download scripts must say where the environment variables go.

use std::path::{Path, PathBuf};
use std::process::Command;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read(relative: &str) -> String {
    let path = repo_root().join(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

fn help(exe: &str, args: &[&str]) -> String {
    let out = Command::new(exe).args(args).arg("--help").output().unwrap();
    String::from_utf8_lossy(&out.stdout).to_string()
}

/// The README section, from its heading to the next top-level heading.
fn readme_section() -> String {
    let readme = read("README.md");
    let start = readme.find("## Use THOR on a second machine").expect("README.md lost its second-machine section");
    let rest = &readme[start + 5..];
    let end = rest.find("\n## ").map(|i| i + 5).unwrap_or(rest.len());
    readme[start..start + end].to_string()
}

#[test]
fn every_option_the_readme_names_exists_in_the_program() {
    let section = readme_section();
    let install = help(env!("CARGO_BIN_EXE_install"), &[]);
    for flag in ["--copy-from", "--scope", "--include-global", "--whole-memory", "--register-root", "--per-machine"] {
        assert!(section.contains(flag), "the README section never mentions {flag}");
        assert!(install.contains(flag), "install --help has no {flag}");
    }
    let pull = help(env!("CARGO_BIN_EXE_sync"), &["pull"]);
    for flag in ["--scope", "--include-global", "--whole", "--db"] {
        assert!(section.contains(flag), "the README section never mentions {flag}");
        assert!(pull.contains(flag), "sync pull --help has no {flag}");
    }
}

#[test]
fn the_pages_and_the_scripts_agree_on_the_environment_variables() {
    let section = readme_section();
    let agents = read("AGENTS.md");
    for name in ["THOR_COPY_FROM", "THOR_COPY_SCOPES", "THOR_COPY_GLOBAL", "THOR_COPY_WHOLE"] {
        assert!(section.contains(name), "README.md never mentions {name}");
        assert!(agents.contains(name), "AGENTS.md never mentions {name}");
        assert!(read("docs/WHAT-THOR-DOES.md").contains(name), "docs/WHAT-THOR-DOES.md never mentions {name}");
        for script in ["install.ps1", "install.sh"] {
            assert!(read(script).contains(name), "{script} never says where {name} goes");
        }
        let installer = read("thor2/ops/src/bin/install.rs");
        assert!(installer.contains(name), "the installer does not read {name}");
    }
}

#[test]
fn the_readme_section_holds_a_first_timers_hand_for_both_systems() {
    let section = readme_section();
    for needed in [
        "What you need first",
        "The one command",
        "If it stops",
        "After the command",
        "Windows, in PowerShell",
        "Linux:",
        "ONE THING LEFT FOR YOU",
        "git ls-remote",
        "read-only",
    ] {
        assert!(section.contains(needed), "the second-machine section lost '{needed}'");
    }
    // The pages say it plainly: other projects are not in the copy at all.
    assert!(section.contains("not in the copy's file at all"));
}

#[test]
fn the_pages_use_plain_typography() {
    for page in ["README.md", "AGENTS.md", "CHANGELOG.md", "docs/WHAT-THOR-DOES.md", "thor2/README.md", "install.ps1", "install.sh"] {
        let text = read(page);
        let section: String = if page == "README.md" { readme_section() } else { text };
        for glyph in ['\u{2014}', '\u{2013}', '\u{2212}', '\u{2018}', '\u{2019}', '\u{201c}', '\u{201d}', '\u{2026}'] {
            assert!(!section.contains(glyph), "{page} contains U+{:04X}", glyph as u32);
        }
    }
}
