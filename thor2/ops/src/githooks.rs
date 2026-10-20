//! The git side of installation: one machine-wide hooks directory, so the code
//! index refreshes itself after every commit in every repository without anyone
//! having to remember to run anything.
//!
//! Why machine-wide and not one hook per repository. `codeindex::store::refresh`
//! is cheap, commit-diff driven, and a no-op the moment HEAD has not moved - but
//! nothing ever called it, so an index was only fresh if a person remembered.
//! That is CONTRACT R7 word for word ("If the design needs someone to remember
//! something, that is a design defect"), and measured on 2026-08-15 it cost a
//! lot: with an index three days behind, 83% of `where_used` sites named a line
//! that no longer held the symbol, against 0% from a fresh one.
//!
//! THE DEFECT THIS MODULE MUST NOT CAUSE. Setting `core.hooksPath` makes git
//! ignore every repository's own `.git/hooks` directory outright. This project's
//! own repository has a `pre-commit` hook that refuses to let the maintainer's
//! private identifiers reach a public remote. A naive install would switch that
//! gate off without a word - a gate going quiet is this project's worst failure
//! class, and it would have been caused by the change meant to help. So this
//! directory holds a dispatcher per event and every one of them ends by handing
//! control back to the repository's own hook.
//!
//! `--git-common-dir` and never `--git-path hooks`: once `core.hooksPath` is
//! set, the latter answers with THIS directory and the chain would point at
//! itself. That single word is the difference between a working gate and a
//! silent one.
//!
//! The same directory carries a second job, the media gate: the `pre-commit`
//! dispatcher refuses a commit that stages a video file or a file above a size
//! limit, in every repository, before it hands control to the repository's own
//! hook. Why it is a hook that reads the index and not a rule about command
//! text: `git add -A`, `git add .` and `git commit -a` stage files the command
//! never names, so only the index shows what a commit really contains. A 26 MB
//! 4K video and some slides went into a repository exactly that way.

use std::fs;
use std::path::Path;

use crate::install::HookOutcome;

/// Every hook event this directory answers for. A repository hook whose event
/// has no dispatcher here would be silently skipped once `core.hooksPath` is
/// set, so this list is a safety floor, not a convenience: it covers every
/// event in use plus the common neighbours someone may add later.
pub const DISPATCHED_EVENTS: &[&str] = &[
    "pre-commit",
    "prepare-commit-msg",
    "commit-msg",
    "post-commit",
    "post-checkout",
    "post-merge",
    "pre-push",
    "pre-rebase",
    "post-rewrite",
];

const HEADER: &str = r#"#!/bin/sh
# Installed by THOR. This directory is git's machine-wide core.hooksPath, which
# makes git ignore every repository's own .git/hooks. Each dispatcher here
# therefore hands control back to the repository's own hook, so wiring one
# machine-wide hook never silences a local one.
"#;

/// The tail every dispatcher ends with. Kept in one place because getting it
/// wrong in one file is exactly how a repository's own hook goes quiet.
const CHAIN: &str = r#"
common="$(git rev-parse --path-format=absolute --git-common-dir 2>/dev/null)"
if [ -z "$common" ]; then common="$(git rev-parse --git-common-dir 2>/dev/null)"; fi
repo_hook="$common/hooks/@EVENT@"
if [ -x "$repo_hook" ]; then
	exec "$repo_hook" "$@"
elif [ -f "$repo_hook" ]; then
	exec sh "$repo_hook" "$@"
fi
exit 0
"#;

/// The work `post-commit` does before it chains: resolve which project this
/// repository is, and refresh that project's index if one exists.
///
/// Resolution mirrors `serve::project` exactly - a `.thor-project` marker file's
/// first non-blank line wins, otherwise the repository directory's basename -
/// because an index this cannot find is an index `serve::lookup` cannot find
/// either. A repository THOR has no index for is left completely alone: this
/// never builds a full index unasked.
const REFRESH: &str = r#"
CODEINDEX="@CODEINDEX@"
INDEX_ROOT="@INDEX_ROOT@"

root="$(git rev-parse --show-toplevel 2>/dev/null)"
if [ -n "$root" ]; then
	marker="$root/.thor-project"
	if [ -f "$marker" ]; then
		key="$(grep -v '^[[:space:]]*$' "$marker" | head -n 1 | sed -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//')"
	else
		key="$(basename "$root")"
	fi
	if [ -n "$key" ] && [ -f "$INDEX_ROOT/$key.db" ]; then
		"$CODEINDEX" "$INDEX_ROOT/$key.db" "$root" refresh >/dev/null 2>&1
	fi
fi
"#;

/// The media gate: what `pre-commit` does before it chains. POSIX sh only (Git
/// for Windows runs it through its own sh, Linux through /bin/sh).
///
/// It reads the index, so every way of staging is seen. A commit costs about
/// seven short processes: three `git config`, one `git diff`, one `sed`, one
/// `git cat-file --batch-check` for every size at once, one `awk`; `git
/// check-attr` runs once, and only when something would be refused.
///
/// Limits, all on purpose:
/// - A removed file is never listed, and a rename counts as a new file (the
///   diff runs with `--no-renames`), so renaming a tracked video is refused.
/// - A submodule is not a file and is skipped.
/// - A name with a double quote, backslash, tab or newline in it is shown by git
///   between double quotes with C escapes; the gate checks it all the same and
///   prints it in that form.
/// - A commit that finishes a merge is not checked: its diff against HEAD lists
///   everything the other side brought in, which is already part of the history.
/// - It fails open. A missing helper program, an old git without `--batch-check`
///   formats or a value it cannot read finds nothing; it never stops a commit
///   because of a problem of its own, only because of a file it has seen.
const MEDIA_GATE: &str = r##"
# THOR media gate: refuses a commit that adds a video file or a file above a size limit.
# It reads the index, so `git add -A`, `git add .` and `git commit -a` are all seen.
# Plain git config, global or per repository:
#   thor.mediagate.enabled     false switches the gate off
#   thor.mediagate.extensions  video types, space separated (default: mp4 mov mkv webm avi m4v)
#   thor.mediagate.maxbytes    size limit in bytes (default: 26214400, which is 25 MB)
#   thor.mediagate.parkdir     a folder to keep refused files in (only used in the advice)
# A path with the git attribute thor-media-ok set (in .gitattributes) is let through.
thor_media_gate() {
	nl='
'
	if [ "$(git config --bool --get thor.mediagate.enabled 2>/dev/null)" = "false" ]; then
		return 0
	fi
	if exts="$(git config --get thor.mediagate.extensions 2>/dev/null)"; then
		:
	else
		exts="mp4 mov mkv webm avi m4v"
	fi
	limit="$(git config --get thor.mediagate.maxbytes 2>/dev/null)"
	case "$limit" in
	'')
		limit=26214400
		;;
	# Only a whole number above 0 of at most 15 digits fits the arithmetic below.
	*[!0-9]* | 0 | ????????????????*)
		echo "THOR media gate: thor.mediagate.maxbytes is '$limit', which is not a usable number of bytes (a plain whole number above 0 is needed), so 26214400 (25 MB) is used instead." >&2
		limit=26214400
		;;
	esac

	# What is staged: added, copied or modified files, as raw diff lines.
	staged="$(git -c core.quotePath=false diff --cached --no-renames --diff-filter=ACM --raw --no-abbrev --no-color 2>/dev/null)"
	if [ -z "$staged" ]; then
		return 0
	fi
	# One line per file that breaks a rule: "<kinds> <size in bytes> <path>", where
	# kinds is V (a video type), S (too big) or both. The sizes of all staged
	# blobs come from one cat-file call; %(rest) hands the path through, spaces and all.
	candidates="$(printf '%s\n' "$staged" |
		sed -n -e '/^:[0-7]* 160000 /d' -e 's/^:[0-7]* [0-7]* [0-9a-f]* \([0-9a-f]*\) [A-Z][0-9]*[[:blank:]]\(.*\)$/\1 \2/p' |
		git cat-file --batch-check='%(objectsize) %(rest)' 2>/dev/null |
		awk -v max="$limit" -v exts="$exts" '
			BEGIN {
				count = split(tolower(exts), listed, /[ \t]+/)
				for (i = 1; i <= count; i++) {
					sub(/^[*.]+/, "", listed[i])
					if (listed[i] != "") video[listed[i]] = 1
				}
			}
			$1 ~ /^[0-9]+$/ && length($1) < 16 {
				size = $1
				path = substr($0, length(size) + 2)
				kinds = ""
				name = tolower(path)
				sub(/"$/, "", name)
				sub(/.*\//, "", name)
				if (index(name, ".") > 0) {
					sub(/.*\./, "", name)
					if (name in video) kinds = "V"
				}
				if (size + 0 > max + 0) kinds = kinds "S"
				if (kinds != "") print kinds, size, path
			}')"
	if [ -z "$candidates" ]; then
		return 0
	fi

	# Finishing a merge lists everything the other side brought in, and that is
	# already part of the history: step aside.
	if git rev-parse -q --verify MERGE_HEAD >/dev/null 2>&1; then
		echo "THOR media gate: skipped, because this commit finishes a merge and the files it brings in are already part of your history." >&2
		return 0
	fi

	# Paths a project allowed on purpose, found with one check-attr call.
	exempt="$(printf '%s\n' "$candidates" | sed 's/^[A-Z]* [0-9]* //' |
		git -c core.quotePath=false check-attr --stdin thor-media-ok 2>/dev/null |
		sed -n 's/: thor-media-ok: set$//p')"

	# git restore --staged needs a commit to restore from; a brand new repository has none.
	if git rev-parse -q --verify HEAD >/dev/null 2>&1; then
		have_head=yes
	else
		have_head=no
	fi
	limit_tenths=$(( (limit * 10 + 524288) / 1048576 ))
	limit_mb="$((limit_tenths / 10)).$((limit_tenths % 10))"
	found=0
	report=""
	unstage=""
	allow=""
	while IFS= read -r line; do
		kinds=${line%% *}
		rest=${line#* }
		size=${rest%% *}
		path=${rest#* }
		case "$nl$exempt$nl" in
		*"$nl$path$nl"*) continue ;;
		esac
		found=$((found + 1))
		report="$report$nl  $path$nl"
		case "$kinds" in
		*V*) report="$report      this is a video file$nl" ;;
		esac
		case "$kinds" in
		*S*)
			tenths=$(( (size * 10 + 524288) / 1048576 ))
			report="$report      this file is $((tenths / 10)).$((tenths % 10)) MB ($size bytes), the limit is $limit_mb MB ($limit bytes)$nl"
			;;
		esac
		case "$path" in
		'"'*) shown="$path" ;;
		*) shown="\"$path\"" ;;
		esac
		if [ "$have_head" = "yes" ]; then
			unstage="$unstage       git restore --staged $shown$nl"
		else
			unstage="$unstage       git rm --cached $shown$nl"
		fi
		case "$path" in
		'"'*) attr="$path" ;;
		*" "*) attr="\"$path\"" ;;
		*) attr="$path" ;;
		esac
		allow="$allow       $attr thor-media-ok$nl"
	done <<EOF
$candidates
EOF
	if [ "$found" -eq 0 ]; then
		return 0
	fi

	parkdir="$(git config --get thor.mediagate.parkdir 2>/dev/null)"
	{
		echo "THOR stopped this commit. It would add video or very large files to git:"
		printf '%s' "$report"
		echo
		echo "Nothing was committed, and your files on disk are untouched."
		echo "Why: git keeps every version of every file for good. One video or one big file"
		echo "makes the repository heavy forever, and once it is pushed it is very hard to remove."
		echo
		echo "What to do, step by step:"
		echo "  1. Take the file out of this commit. Run this, one line per file:"
		printf '%s' "$unstage"
		echo "  2. Keep the file outside of the repository, for example in a folder of its own on your disk."
		if [ -n "$parkdir" ]; then
			top="$(git rev-parse --show-toplevel 2>/dev/null)"
			echo "     Put it in $parkdir/$(date +%Y-%m-%d)_${top##*/}_<subject>/ (replace <subject> by two or"
			echo "     three words about the file) and put a short text file called STATUS.txt in that"
			echo "     folder that says what the file is and why you keep it."
		fi
		echo "  3. Run your commit again."
		echo
		echo "If you are sure a file belongs in git, allow it on purpose:"
		echo "  1. Add this line to the file called .gitattributes in the top folder of the"
		echo "     repository (create the file if it is not there yet):"
		printf '%s' "$allow"
		echo "  2. Stage that file with: git add .gitattributes"
		echo "  3. Run your commit again."
		echo
		echo "To change the rules, run one of these (add --global after 'git config' to change it"
		echo "for every repository on this computer):"
		echo "  The size limit is now $limit bytes ($limit_mb MB). For example 50 MB:"
		echo "       git config thor.mediagate.maxbytes 52428800"
		echo "  The video types it stops are now: ${exts:-none}. For example:"
		echo "       git config thor.mediagate.extensions \"mp4 mov mkv\""
	} >&2
	return 1
}
thor_media_gate || exit 1
"##;

/// The exact text of one dispatcher. Pure, so a test can read it without
/// touching a filesystem or a git config.
pub fn dispatcher_body(event: &str, codeindex_exe: &str, index_root: &str) -> String {
    let mut body = String::from(HEADER);
    match event {
        "pre-commit" => body.push_str(MEDIA_GATE),
        "post-commit" => body.push_str(
            &REFRESH
                .replace("@CODEINDEX@", codeindex_exe)
                .replace("@INDEX_ROOT@", index_root),
        ),
        _ => {}
    }
    body.push_str(&CHAIN.replace("@EVENT@", event));
    body
}

/// Write every dispatcher into `dir`, reporting what happened to each. A file
/// whose content already matches is left untouched, so a second install is a
/// no-op rather than a rewrite.
pub fn install_git_hooks(
    dir: &Path,
    codeindex_exe: &str,
    index_root: &str,
) -> std::io::Result<Vec<(String, HookOutcome)>> {
    fs::create_dir_all(dir)?;
    let mut results = Vec::new();

    for event in DISPATCHED_EVENTS {
        let path = dir.join(event);
        let wanted = dispatcher_body(event, codeindex_exe, index_root);

        // Compare as bytes: a stray BOM or CRLF from an earlier hand-edit must
        // read as "different", not as "already fine".
        let outcome = match fs::read(&path) {
            Ok(existing) if existing == wanted.as_bytes() => HookOutcome::AlreadyPresent,
            Ok(_) => {
                fs::write(&path, &wanted)?;
                HookOutcome::Replaced
            }
            Err(_) => {
                fs::write(&path, &wanted)?;
                HookOutcome::Added
            }
        };

        // On unix a hook git cannot execute is a hook that does not run. On
        // Windows the bit does not exist and git runs it through its own sh.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755))?;
        }

        results.push(((*event).to_string(), outcome));
    }

    Ok(results)
}

/// What wiring `core.hooksPath` did, or refused to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HooksPathOutcome {
    /// Nothing was configured before; this run pointed git at our directory.
    Set,
    /// Already pointing here. Nothing written.
    AlreadyOurs,
    /// Something else owns this setting. Left exactly as it was, and reported -
    /// taking it over would silently disable whatever that directory does.
    Foreign(String),
}

/// Decide what to do about `core.hooksPath`, given whatever git reports now.
/// Split out from the git call so the decision is testable without touching the
/// machine's real global configuration.
pub fn decide_hooks_path(current: Option<&str>, ours: &str) -> HooksPathOutcome {
    match current.map(str::trim).filter(|c| !c.is_empty()) {
        None => HooksPathOutcome::Set,
        Some(c) if paths_equal(c, ours) => HooksPathOutcome::AlreadyOurs,
        Some(c) => HooksPathOutcome::Foreign(c.to_string()),
    }
}

/// Whether a `git` program can be started at all. Someone who only keeps
/// notes about a topic may never have installed it, and the installer must
/// then say that plainly instead of reporting a failed git command.
pub fn git_available() -> bool {
    std::process::Command::new("git")
        .arg("--version")
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

/// Read git's current global `core.hooksPath`, decide, and set it only when it
/// is free. Shelling out to `git` rather than editing its config file by hand,
/// the same line `codeindex::gitutil` already takes: git owns that format.
///
/// A directory someone else configured is LEFT ALONE and reported. Taking it
/// over would disable whatever that directory does, which is the exact failure
/// this whole module is built to avoid.
pub fn wire_hooks_path(dir: &Path) -> std::io::Result<HooksPathOutcome> {
    let ours = dir.to_string_lossy().replace('\\', "/");

    let current = std::process::Command::new("git")
        .args(["config", "--global", "--get", "core.hooksPath"])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string());

    let outcome = decide_hooks_path(current.as_deref(), &ours);
    if outcome == HooksPathOutcome::Set {
        let status = std::process::Command::new("git")
            .args(["config", "--global", "core.hooksPath", &ours])
            .status()?;
        if !status.success() {
            return Err(std::io::Error::other(
                "git config --global core.hooksPath did not succeed",
            ));
        }
    }
    Ok(outcome)
}

/// Git stores this path as written. The same directory can arrive with either
/// slash on Windows, and a false "someone else owns this" would stop the
/// installer for no reason.
fn paths_equal(a: &str, b: &str) -> bool {
    a.replace('\\', "/").trim_end_matches('/') == b.replace('\\', "/").trim_end_matches('/')
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXE: &str = "C:/fake/codeindex.exe";
    const ROOT: &str = "C:/fake/codeindex";

    #[test]
    fn install_writes_a_post_commit_hook_that_calls_refresh() {
        let dir = tempfile::tempdir().unwrap();
        let results = install_git_hooks(dir.path(), EXE, ROOT).unwrap();

        assert!(results.iter().all(|(_, o)| *o == HookOutcome::Added));
        let body = fs::read_to_string(dir.path().join("post-commit")).unwrap();
        assert!(body.contains(EXE), "the hook must call the codeindex binary");
        assert!(body.contains("refresh"), "and it must call refresh");
    }

    /// THE DEFECT THIS PREVENTS: `core.hooksPath` switches off every
    /// repository's own hooks. A dispatcher that does not chain would silence
    /// this project's private-data gate without a word.
    #[test]
    fn every_dispatcher_hands_control_back_to_the_repository_hook() {
        let dir = tempfile::tempdir().unwrap();
        install_git_hooks(dir.path(), EXE, ROOT).unwrap();

        for event in DISPATCHED_EVENTS {
            let body = fs::read_to_string(dir.path().join(event)).unwrap();
            assert!(
                body.contains(&format!("hooks/{event}")),
                "{event} must chain to the repository's own {event}"
            );
            assert!(
                body.contains("exec \"$repo_hook\""),
                "{event} must actually exec it, not merely name it"
            );
        }
    }

    #[test]
    fn the_chain_uses_git_common_dir_so_it_never_points_at_itself() {
        let body = dispatcher_body("pre-commit", EXE, ROOT);
        assert!(body.contains("--git-common-dir"));
        assert!(
            !body.contains("--git-path hooks"),
            "--git-path hooks answers with OUR directory once core.hooksPath is set, \
             so the chain would exec itself forever"
        );
    }

    #[test]
    fn an_existing_foreign_hooks_path_is_never_taken_over() {
        let ours = "C:/Users/dev/thor2/githooks";
        assert_eq!(decide_hooks_path(None, ours), HooksPathOutcome::Set);
        assert_eq!(decide_hooks_path(Some(""), ours), HooksPathOutcome::Set);
        assert_eq!(decide_hooks_path(Some(ours), ours), HooksPathOutcome::AlreadyOurs);
        assert_eq!(
            decide_hooks_path(Some("C:\\Users\\dev\\thor2\\githooks\\"), ours),
            HooksPathOutcome::AlreadyOurs,
            "the same directory with the other slash is still ours"
        );
        assert_eq!(
            decide_hooks_path(Some("D:/someone/else/hooks"), ours),
            HooksPathOutcome::Foreign("D:/someone/else/hooks".to_string())
        );
    }

    #[test]
    fn installing_twice_reports_already_present_and_rewrites_nothing() {
        let dir = tempfile::tempdir().unwrap();
        install_git_hooks(dir.path(), EXE, ROOT).unwrap();
        let before = fs::read(dir.path().join("post-commit")).unwrap();

        let second = install_git_hooks(dir.path(), EXE, ROOT).unwrap();
        assert!(second.iter().all(|(_, o)| *o == HookOutcome::AlreadyPresent));
        assert_eq!(before, fs::read(dir.path().join("post-commit")).unwrap());
    }

    /// A commit must never fail or stall because the index could not be
    /// refreshed. git ignores post-commit's exit code, but a hook that exits
    /// non-zero still prints noise, and `set -e` would abandon the chain.
    #[test]
    fn the_post_commit_body_never_lets_a_failure_escape() {
        let body = dispatcher_body("post-commit", EXE, ROOT);
        assert!(body.contains(">/dev/null 2>&1"), "refresh output stays out of the way");
        assert!(body.trim_end().ends_with("exit 0"));
        assert!(!body.contains("set -e"), "set -e would skip the chain on any failure");
    }

    #[test]
    fn the_post_commit_body_only_refreshes_when_an_index_exists() {
        let body = dispatcher_body("post-commit", EXE, ROOT);
        assert!(
            body.contains("[ -f \"$INDEX_ROOT/$key.db\" ]"),
            "a repository THOR has no index for must be left completely alone"
        );
    }

    /// The list is a safety floor: an event missing here is a repository hook
    /// git silently stops calling.
    #[test]
    fn the_dispatcher_set_covers_every_hook_event_git_can_call_here() {
        for event in ["pre-commit", "post-commit", "post-checkout", "post-merge", "pre-push"] {
            assert!(
                DISPATCHED_EVENTS.contains(&event),
                "{event} is in use in the owner's repositories and must have a dispatcher"
            );
        }
    }

    #[test]
    fn the_hook_files_are_written_with_lf_and_no_bom() {
        let dir = tempfile::tempdir().unwrap();
        install_git_hooks(dir.path(), EXE, ROOT).unwrap();

        for event in DISPATCHED_EVENTS {
            let bytes = fs::read(dir.path().join(event)).unwrap();
            assert!(!bytes.starts_with(&[0xEF, 0xBB, 0xBF]), "{event} carries a BOM");
            assert!(!bytes.contains(&b'\r'), "{event} carries CRLF, which git's sh chokes on");
        }
    }

    // ---- the media gate ---------------------------------------------------

    use std::path::PathBuf;
    use std::process::{Command, Output};

    fn slashes(path: &Path) -> String {
        path.to_string_lossy().replace('\\', "/")
    }

    /// What a command wrote to stderr: where a refusal explains itself.
    fn stderr_of(out: &Output) -> String {
        String::from_utf8_lossy(&out.stderr).to_string()
    }

    /// A throwaway git repository wired to the generated dispatchers, the way
    /// the installer wires a real one. The machine's own git configuration,
    /// global ignore file and home folder are kept out of the way, so a result
    /// does not depend on whose computer runs the test.
    struct GateRepo {
        root: tempfile::TempDir,
    }

    impl GateRepo {
        fn new() -> Self {
            let me = Self { root: tempfile::tempdir().unwrap() };
            fs::create_dir_all(me.home()).unwrap();
            fs::write(me.home().join("gitconfig"), "").unwrap();
            fs::write(me.home().join("ignore"), "").unwrap();
            install_git_hooks(&me.hooks(), EXE, ROOT).unwrap();
            fs::create_dir_all(me.repo()).unwrap();
            me.git_ok(&["init", "--quiet"]);
            me.git_ok(&["config", "core.hooksPath", &slashes(&me.hooks())]);
            me.git_ok(&["config", "core.excludesFile", &slashes(&me.home().join("ignore"))]);
            me.git_ok(&["config", "core.autocrlf", "false"]);
            me.git_ok(&["config", "commit.gpgsign", "false"]);
            me.git_ok(&["config", "user.email", "gate-test@example.invalid"]);
            me.git_ok(&["config", "user.name", "media gate test"]);
            me
        }

        fn home(&self) -> PathBuf {
            self.root.path().join("home")
        }

        fn hooks(&self) -> PathBuf {
            self.root.path().join("hooks")
        }

        fn repo(&self) -> PathBuf {
            self.root.path().join("repo")
        }

        fn git(&self, args: &[&str]) -> Output {
            Command::new("git")
                .current_dir(self.repo())
                .args(args)
                .env("HOME", self.home())
                .env("USERPROFILE", self.home())
                .env("XDG_CONFIG_HOME", self.home())
                .env("GIT_CONFIG_GLOBAL", self.home().join("gitconfig"))
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env_remove("GIT_DIR")
                .env_remove("GIT_INDEX_FILE")
                .env_remove("GIT_WORK_TREE")
                .output()
                .expect("git must be on PATH")
        }

        fn git_ok(&self, args: &[&str]) {
            let out = self.git(args);
            assert!(out.status.success(), "git {args:?} failed: {}", stderr_of(&out));
        }

        fn write(&self, name: &str, bytes: &[u8]) {
            let path = self.repo().join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, bytes).unwrap();
        }

        /// Stage everything, the way `git add -A` or `git add .` does.
        fn stage_all(&self) {
            self.git_ok(&["add", "-A"]);
        }

        fn commit(&self, message: &str) -> Output {
            self.git(&["commit", "--quiet", "-m", message])
        }

        fn set(&self, key: &str, value: &str) {
            self.git_ok(&["config", key, value]);
        }

        fn has_commit(&self) -> bool {
            self.git(&["rev-parse", "-q", "--verify", "HEAD"]).status.success()
        }

        /// Put a hook of the repository's own in `.git/hooks/pre-commit`.
        fn own_pre_commit(&self, body: &str) {
            let hook = self.repo().join(".git").join("hooks").join("pre-commit");
            fs::create_dir_all(hook.parent().unwrap()).unwrap();
            fs::write(&hook, body).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
            }
        }
    }

    fn assert_refused(out: &Output) {
        assert!(!out.status.success(), "the commit should have been refused");
        assert!(
            !stderr_of(out).contains("no-verify"),
            "the refusal must not teach how to skip the check: {}",
            stderr_of(out)
        );
    }

    #[test]
    fn the_gate_sits_in_pre_commit_only_and_comes_before_the_chain() {
        let body = dispatcher_body("pre-commit", EXE, ROOT);
        let gate = body.find("thor_media_gate || exit 1").expect("pre-commit carries the gate");
        let chain = body.find("exec \"$repo_hook\"").expect("and still chains");
        assert!(gate < chain, "the gate must run before the repository's own hook");
        assert!(body.is_ascii(), "no curly quote or dash may reach a hook file");
        assert!(!body.contains("no-verify"));
        assert!(!body.contains("--git-path hooks"));
        for event in DISPATCHED_EVENTS.iter().filter(|e| **e != "pre-commit") {
            assert!(
                !dispatcher_body(event, EXE, ROOT).contains("thor_media_gate"),
                "{event} must not carry the media gate"
            );
        }
        assert!(dispatcher_body("post-commit", EXE, ROOT).contains(EXE), "post-commit still refreshes");
    }

    #[test]
    fn an_install_over_an_older_pre_commit_dispatcher_replaces_it_with_the_gate() {
        let dir = tempfile::tempdir().unwrap();
        let older = format!("{HEADER}{}", CHAIN.replace("@EVENT@", "pre-commit"));
        fs::write(dir.path().join("pre-commit"), &older).unwrap();

        let results = install_git_hooks(dir.path(), EXE, ROOT).unwrap();

        let outcome = &results.iter().find(|(event, _)| event == "pre-commit").unwrap().1;
        assert_eq!(*outcome, HookOutcome::Replaced);
        let now = fs::read_to_string(dir.path().join("pre-commit")).unwrap();
        assert!(now.contains("thor_media_gate"));
    }

    #[test]
    fn a_staged_video_is_refused_named_and_nothing_is_committed() {
        let repo = GateRepo::new();
        repo.write("clip.mp4", b"not really a video");
        repo.write("notes.txt", b"just notes");
        repo.stage_all();

        let out = repo.commit("add a clip");

        assert_refused(&out);
        let said = stderr_of(&out);
        assert!(said.contains("clip.mp4"), "{said}");
        assert!(said.contains("video"), "{said}");
        assert!(!said.contains("notes.txt"), "a file that breaks no rule is not named: {said}");
        assert!(!repo.has_commit(), "a refused commit must leave no commit behind");
        assert!(said.contains("git rm --cached \"clip.mp4\""), "a repository without a commit cannot use restore: {said}");
    }

    #[test]
    fn the_advice_after_a_refusal_really_gets_the_commit_through() {
        let repo = GateRepo::new();
        repo.write("notes.txt", b"first");
        repo.stage_all();
        assert!(repo.commit("first").status.success());

        repo.write("clip.mp4", b"not really a video");
        repo.write("more.txt", b"second");
        repo.stage_all();
        let out = repo.commit("second");
        assert_refused(&out);
        assert!(stderr_of(&out).contains("git restore --staged \"clip.mp4\""), "{}", stderr_of(&out));

        repo.git_ok(&["restore", "--staged", "clip.mp4"]);
        let again = repo.commit("second");

        assert!(again.status.success(), "{}", stderr_of(&again));
        let tree = repo.git(&["ls-tree", "-r", "--name-only", "HEAD"]);
        let files = String::from_utf8_lossy(&tree.stdout).to_string();
        assert!(files.contains("more.txt") && !files.contains("clip.mp4"), "{files}");
    }

    #[test]
    fn an_uppercase_extension_is_refused() {
        let repo = GateRepo::new();
        repo.write("HOLIDAY.MP4", b"loud");
        repo.write("Mixed.MoV", b"mixed");
        repo.stage_all();

        let out = repo.commit("add");

        assert_refused(&out);
        assert!(stderr_of(&out).contains("HOLIDAY.MP4"));
        assert!(stderr_of(&out).contains("Mixed.MoV"));
    }

    #[test]
    fn every_default_video_type_is_refused_and_a_sound_file_is_not() {
        let repo = GateRepo::new();
        for name in ["a.mp4", "b.mov", "c.mkv", "d.webm", "e.avi", "f.m4v", "song.mp3"] {
            repo.write(name, b"x");
        }
        repo.stage_all();

        let out = repo.commit("add");

        assert_refused(&out);
        let said = stderr_of(&out);
        for name in ["a.mp4", "b.mov", "c.mkv", "d.webm", "e.avi", "f.m4v"] {
            assert!(said.contains(name), "{name} should be named: {said}");
        }
        assert!(!said.contains("song.mp3"), "{said}");
    }

    #[test]
    fn a_file_name_with_spaces_is_refused_and_printed_whole() {
        let repo = GateRepo::new();
        repo.write("holiday films/my holiday clip.mp4", b"x");
        repo.stage_all();

        let out = repo.commit("add");

        assert_refused(&out);
        let said = stderr_of(&out);
        assert!(said.contains("holiday films/my holiday clip.mp4"), "{said}");
        assert!(said.contains("git rm --cached \"holiday films/my holiday clip.mp4\""), "{said}");
        assert!(said.contains("\"holiday films/my holiday clip.mp4\" thor-media-ok"), "{said}");
    }

    #[test]
    fn a_file_above_a_small_limit_in_the_repository_config_is_refused() {
        let repo = GateRepo::new();
        repo.set("thor.mediagate.maxbytes", "100");
        repo.write("sub dir/data file.bin", &[b'x'; 101]);
        repo.stage_all();

        let out = repo.commit("add");

        assert_refused(&out);
        let said = stderr_of(&out);
        assert!(said.contains("sub dir/data file.bin"), "{said}");
        assert!(said.contains("this file is 0.0 MB (101 bytes), the limit is 0.0 MB (100 bytes)"), "{said}");
        assert!(!said.contains("this is a video file"), "a plain big file is not called a video: {said}");
    }

    #[test]
    fn a_file_exactly_at_the_limit_passes() {
        let repo = GateRepo::new();
        repo.set("thor.mediagate.maxbytes", "100");
        repo.write("exact.bin", &[b'x'; 100]);
        repo.stage_all();

        let out = repo.commit("add");

        assert!(out.status.success(), "{}", stderr_of(&out));
    }

    #[test]
    fn the_default_limit_is_25_mb() {
        let repo = GateRepo::new();
        repo.write("limit.bin", &vec![0u8; 26_214_400]);
        repo.stage_all();
        let at_limit = repo.commit("at the limit");
        assert!(at_limit.status.success(), "{}", stderr_of(&at_limit));

        repo.write("over.bin", &vec![0u8; 26_214_401]);
        repo.stage_all();
        let over = repo.commit("one byte over");

        assert_refused(&over);
        assert!(stderr_of(&over).contains("over.bin"), "{}", stderr_of(&over));
    }

    #[test]
    fn a_normal_small_file_passes() {
        let repo = GateRepo::new();
        repo.write("src/main.rs", b"fn main() {}\n");
        repo.stage_all();

        let out = repo.commit("add");

        assert!(out.status.success(), "{}", stderr_of(&out));
        assert!(repo.has_commit());
    }

    #[test]
    fn the_attribute_thor_media_ok_lets_exactly_that_path_through() {
        let repo = GateRepo::new();
        repo.write(".gitattributes", b"allowed.mp4 thor-media-ok\n\"my clip.mp4\" thor-media-ok\n");
        repo.write("allowed.mp4", b"x");
        repo.write("my clip.mp4", b"x");
        repo.stage_all();
        let both_allowed = repo.commit("on purpose");
        assert!(both_allowed.status.success(), "{}", stderr_of(&both_allowed));

        repo.write("allowed.mp4", b"changed");
        repo.write("other.mp4", b"x");
        repo.stage_all();
        let one_not = repo.commit("not on purpose");

        assert_refused(&one_not);
        let said = stderr_of(&one_not);
        assert!(said.contains("other.mp4"), "{said}");
        assert!(!said.contains("allowed.mp4"), "an allowed path is not named: {said}");
    }

    #[test]
    fn enabled_false_in_any_spelling_switches_the_gate_off() {
        let repo = GateRepo::new();
        for (round, value) in ["false", "off", "no", "0"].iter().enumerate() {
            repo.set("thor.mediagate.enabled", value);
            repo.write(&format!("clip{round}.mp4"), b"x");
            repo.stage_all();

            let out = repo.commit("add");

            assert!(out.status.success(), "enabled={value}: {}", stderr_of(&out));
        }
        repo.set("thor.mediagate.enabled", "true");
        repo.write("late.mp4", b"x");
        repo.stage_all();
        assert_refused(&repo.commit("gate back on"));
    }

    #[test]
    fn the_repository_own_pre_commit_still_runs_after_the_gate_passes() {
        let repo = GateRepo::new();
        let marker = repo.root.path().join("own-hook-ran");
        repo.own_pre_commit(&format!("#!/bin/sh\necho ran > \"{}\"\nexit 0\n", slashes(&marker)));
        repo.write("fine.txt", b"fine");
        repo.stage_all();

        let out = repo.commit("add");

        assert!(out.status.success(), "{}", stderr_of(&out));
        assert!(marker.exists(), "the gate swallowed the repository's own pre-commit");
    }

    #[test]
    fn the_repository_own_pre_commit_can_still_refuse_after_the_gate_passes() {
        let repo = GateRepo::new();
        repo.own_pre_commit("#!/bin/sh\necho 'own hook says no' >&2\nexit 1\n");
        repo.write("fine.txt", b"fine");
        repo.stage_all();

        let out = repo.commit("add");

        assert!(!out.status.success());
        assert!(stderr_of(&out).contains("own hook says no"), "{}", stderr_of(&out));
    }

    #[test]
    fn the_refusal_comes_before_the_repository_own_pre_commit() {
        let repo = GateRepo::new();
        let marker = repo.root.path().join("own-hook-ran");
        repo.own_pre_commit(&format!("#!/bin/sh\necho ran > \"{}\"\nexit 0\n", slashes(&marker)));
        repo.write("clip.mp4", b"x");
        repo.stage_all();

        let out = repo.commit("add");

        assert_refused(&out);
        assert!(!marker.exists(), "a refused commit should not get as far as the repository's hook");
    }

    #[test]
    fn the_list_of_video_types_can_be_changed_and_emptied() {
        let repo = GateRepo::new();
        repo.set("thor.mediagate.extensions", ".MP3 *.flac");
        repo.write("song.mp3", b"x");
        repo.write("clip.mp4", b"x");
        repo.stage_all();
        let out = repo.commit("add");
        assert_refused(&out);
        assert!(stderr_of(&out).contains("song.mp3"));
        assert!(!stderr_of(&out).contains("clip.mp4"), "mp4 is not on this repository's list: {}", stderr_of(&out));

        repo.set("thor.mediagate.extensions", "");
        let emptied = repo.commit("add");
        assert!(emptied.status.success(), "an empty list stops no type: {}", stderr_of(&emptied));
    }

    #[test]
    fn removing_a_video_that_is_already_in_the_repository_is_never_refused() {
        let repo = GateRepo::new();
        repo.write("old.mp4", b"x");
        repo.stage_all();
        let added = repo.git(&["-c", "thor.mediagate.enabled=false", "commit", "--quiet", "-m", "old"]);
        assert!(added.status.success(), "{}", stderr_of(&added));

        repo.git_ok(&["rm", "--quiet", "old.mp4"]);
        let out = repo.commit("remove it");

        assert!(out.status.success(), "{}", stderr_of(&out));
    }

    /// The incident behind the gate: `git commit -a` stages a tracked file the
    /// command never names, so only the index shows what the commit holds.
    #[test]
    fn commit_dash_a_sees_a_tracked_file_that_grew_past_the_limit() {
        let repo = GateRepo::new();
        repo.set("thor.mediagate.maxbytes", "100");
        repo.write("notes.txt", b"small");
        repo.stage_all();
        assert!(repo.commit("small").status.success());

        repo.write("notes.txt", &[b'x'; 500]);
        let out = repo.git(&["commit", "--quiet", "-a", "-m", "everything"]);

        assert_refused(&out);
        assert!(stderr_of(&out).contains("notes.txt"), "{}", stderr_of(&out));
        assert!(stderr_of(&out).contains("git restore --staged \"notes.txt\""), "{}", stderr_of(&out));
    }

    #[test]
    fn a_commit_that_finishes_a_merge_is_not_refused_for_what_the_other_side_brought() {
        let repo = GateRepo::new();
        repo.write("base.txt", b"base");
        repo.stage_all();
        assert!(repo.commit("base").status.success());

        repo.git_ok(&["checkout", "--quiet", "-b", "other"]);
        repo.write("clip.mp4", b"x");
        repo.stage_all();
        let video = repo.git(&["-c", "thor.mediagate.enabled=false", "commit", "--quiet", "-m", "video"]);
        assert!(video.status.success(), "{}", stderr_of(&video));
        repo.git_ok(&["checkout", "--quiet", "-"]);
        repo.write("mine.txt", b"mine");
        repo.stage_all();
        assert!(repo.commit("mine").status.success());

        repo.git_ok(&["merge", "--quiet", "--no-commit", "--no-ff", "other"]);
        let out = repo.commit("merge other");

        assert!(out.status.success(), "{}", stderr_of(&out));
        assert!(stderr_of(&out).contains("merge"), "the skip should be said aloud: {}", stderr_of(&out));
    }

    #[test]
    fn a_big_video_is_refused_for_both_reasons() {
        let repo = GateRepo::new();
        repo.set("thor.mediagate.maxbytes", "100");
        repo.write("clip.mp4", &[b'x'; 200]);
        repo.stage_all();

        let out = repo.commit("add");

        assert_refused(&out);
        let said = stderr_of(&out);
        assert!(said.contains("this is a video file"), "{said}");
        assert!(said.contains("this file is 0.0 MB (200 bytes)"), "{said}");
    }

    #[test]
    fn a_name_with_accents_is_printed_as_it_is_and_can_be_allowed() {
        let repo = GateRepo::new();
        repo.write("films/été vidéo.mp4", b"x");
        repo.stage_all();
        let out = repo.commit("add");
        assert_refused(&out);
        assert!(stderr_of(&out).contains("films/été vidéo.mp4"), "{}", stderr_of(&out));

        repo.write(".gitattributes", "\"films/été vidéo.mp4\" thor-media-ok\n".as_bytes());
        repo.stage_all();
        let allowed = repo.commit("add");
        assert!(allowed.status.success(), "{}", stderr_of(&allowed));
    }

    #[test]
    fn a_submodule_entry_is_not_a_file_and_is_skipped() {
        let repo = GateRepo::new();
        let sha = "1234567890123456789012345678901234567890";
        repo.git_ok(&["update-index", "--add", "--cacheinfo", &format!("160000,{sha},vendor.mp4")]);

        let out = repo.commit("add");

        assert!(out.status.success(), "{}", stderr_of(&out));
    }

    #[test]
    fn a_park_folder_in_the_config_is_named_in_the_advice() {
        let repo = GateRepo::new();
        repo.write("clip.mp4", b"x");
        repo.stage_all();
        let plain = repo.commit("add");
        assert_refused(&plain);
        assert!(!stderr_of(&plain).contains("STATUS.txt"), "no park folder is configured: {}", stderr_of(&plain));

        repo.set("thor.mediagate.parkdir", "D:/parked");
        let out = repo.commit("add");

        assert_refused(&out);
        let said = stderr_of(&out);
        assert!(said.contains("D:/parked/"), "{said}");
        assert!(said.contains("STATUS.txt"), "{said}");
        assert!(said.contains("<subject>"), "{said}");
    }

    #[test]
    fn a_limit_that_is_not_a_usable_number_falls_back_to_the_default_and_says_so() {
        let repo = GateRepo::new();
        for (round, value) in ["50m", "0", "-5", "12345678901234567890"].iter().enumerate() {
            repo.set("thor.mediagate.maxbytes", value);
            repo.write(&format!("small{round}.txt"), b"x");
            repo.stage_all();

            let out = repo.commit("add");

            assert!(out.status.success(), "maxbytes={value}: {}", stderr_of(&out));
            assert!(stderr_of(&out).contains("thor.mediagate.maxbytes"), "maxbytes={value}: {}", stderr_of(&out));
        }
    }
}
