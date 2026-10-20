//! The pages that tell a person about the two folders they may name (a
//! temp-until-live folder and a tools folder) and the programs say the same
//! words: an option a page names exists in the program's own help, the file and
//! the keys the pages name are the ones the code reads, and the part of the
//! README a first-timer follows holds the whole way through.

use std::path::{Path, PathBuf};
use std::process::Command;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read(relative: &str) -> String {
    let path = repo_root().join(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

/// The README section, from its heading to the next top-level heading.
fn readme_section() -> String {
    let readme = read("README.md");
    let start = readme.find("## Two folders THOR can keep tidy").expect("README.md lost its work-folders section");
    let rest = &readme[start + 5..];
    let end = rest.find("\n## ").map(|i| i + 5).unwrap_or(rest.len());
    readme[start..start + end].to_string()
}

#[test]
fn every_page_names_both_options() {
    for page in ["README.md", "AGENTS.md", "docs/WHAT-THOR-DOES.md", "CHANGELOG.md"] {
        let text = read(page);
        for flag in ["--set-temp-folder", "--set-tools-folder"] {
            assert!(text.contains(flag), "{page} never mentions {flag}");
        }
    }
}

#[test]
fn the_options_the_pages_name_exist_in_the_installers_help() {
    let help = Command::new(env!("CARGO_BIN_EXE_install")).arg("--help").output().unwrap();
    let help = String::from_utf8_lossy(&help.stdout).to_string();
    for flag in ["--set-temp-folder", "--set-tools-folder"] {
        assert!(help.contains(flag), "install --help has no {flag}");
    }
}

#[test]
fn the_pages_use_the_file_and_the_keys_the_code_reads() {
    assert_eq!(serve::work_folders::FILE, "work-folders.json");
    assert_eq!(serve::work_folders::CACHE_FILE, "tools-overview-cache.json");
    assert_eq!(serve::leftover_scripts::FILE, "leftover-scripts.json");
    assert_eq!(serve::work_folders::DEFAULT_MAX_AGE_DAYS, 30);
    for page in ["README.md", "AGENTS.md", "docs/WHAT-THOR-DOES.md"] {
        let text = read(page);
        assert!(text.contains(serve::work_folders::FILE), "{page} never names {}", serve::work_folders::FILE);
        assert!(text.contains("temp_max_age_days"), "{page} never names the age limit key");
    }
    let reference = read("docs/WHAT-THOR-DOES.md");
    for name in [serve::work_folders::CACHE_FILE, serve::leftover_scripts::FILE, "temp_until_live", "\"tools\""] {
        assert!(reference.contains(name), "docs/WHAT-THOR-DOES.md never names {name}");
    }
}

#[test]
fn the_readme_section_holds_a_first_timers_hand() {
    let section = readme_section();
    for needed in [
        "never deletes",
        "STATUS.txt",
        "STATUS: live",
        "2026-10-20_shop_banner",
        "README.txt",
        "pdf-merge",
        "--db",
        "$HOME\\thor2\\install.exe",
        "~/.thor2/install",
        "\\\\nas\\projects",
        "30 minutes",
        "switch it off",
        "network share",
    ] {
        assert!(section.contains(needed), "the work-folders section lost '{needed}'");
    }
    // The pages say plainly what is not noticed.
    assert!(section.contains("shell command"), "the section no longer says a script made by a shell command is not noticed");
}

#[test]
fn the_readme_section_uses_plain_typography_and_no_internal_words() {
    let section = readme_section();
    for glyph in ['\u{2014}', '\u{2013}', '\u{2212}', '\u{2018}', '\u{2019}', '\u{201c}', '\u{201d}', '\u{2026}'] {
        assert!(!section.contains(glyph), "the work-folders section contains U+{:04X}", glyph as u32);
    }
    for jargon in ["MCP", "courier", "bm25", "sidecar", "falsifier", "binding"] {
        assert!(!section.contains(jargon), "the work-folders section uses {jargon:?}, a word a newcomer has never heard");
    }
}
