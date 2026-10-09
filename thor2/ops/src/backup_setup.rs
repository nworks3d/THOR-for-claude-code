//! `install --set-backup local|<place>|off`: the backup is a CHOICE the user
//! makes, never something a fresh install switches on by itself.
//!
//! WHY IT IS NOT A DEFAULT. The memory is one file, so a copy of it matters.
//! But a copy that goes somewhere the user never heard of is a surprise, and
//! the first-session list used to claim "a daily copy on this machine" when a
//! fresh install wired nothing at all. Now the list says the truth (nothing is
//! backed up until the user answers) and this module does what the answer
//! says:
//!
//! - `local`: a git folder beside the memory (`backup-repo`) with NO remote.
//!   The backup program commits into it and nothing leaves the machine. It
//!   protects against a damaged or wrongly edited memory file; it does not
//!   survive the disk dying, and the list says so.
//! - a place (a folder on a NAS, another computer, a private repository): the
//!   same folder gets a remote called `nas`, exactly as `AGENTS.md` describes,
//!   and the branch is pushed once so the first backup has something to sync
//!   with.
//! - `off`: only the SessionStart hook that runs the backup is removed. The
//!   copies already made stay where they are.
//!
//! The hook is placed with `install_hooks`, so it is idempotent and the same
//! command in another path spelling is the same hook.

use crate::install::{command_words, flatten_word, HookSpec};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;

/// What the user asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Choice {
    /// A git folder on this machine with no remote.
    Local,
    /// A place the user names: a folder, a network share or a repository address.
    Place(String),
    /// No backup hook.
    Off,
}

/// The word the user gave, read as a choice. `None` for an empty word.
pub fn parse_choice(word: &str) -> Option<Choice> {
    let word = word.trim();
    match word {
        "" => None,
        w if w.eq_ignore_ascii_case("local") => Some(Choice::Local),
        w if w.eq_ignore_ascii_case("off") => Some(Choice::Off),
        w => Some(Choice::Place(w.to_string())),
    }
}

/// The git folder the backup writes into: beside the memory.
pub fn default_repo(db: &Path) -> PathBuf {
    db.parent().unwrap_or_else(|| Path::new(".")).join("backup-repo")
}

/// The hook that runs the backup at every session start (the program itself
/// debounces to once a day and backs off after a failure).
pub fn hook_spec(backup_exe: &str, db: &str, repo: &str) -> HookSpec {
    HookSpec {
        event: "SessionStart",
        matcher: None,
        command: format!("\"{backup_exe}\" --db \"{db}\" --repo \"{repo}\""),
    }
}

/// Whether `command` is a backup hook for the memory at `db`: the program is
/// called `backup` and its `--db` is this memory, in any path spelling.
fn is_backup_command_for(command: &str, db: &str) -> bool {
    let words = command_words(command);
    let Some(program) = words.first() else { return false };
    let stem = Path::new(program).file_stem().and_then(|s| s.to_str()).unwrap_or_default();
    if !stem.eq_ignore_ascii_case("backup") {
        return false;
    }
    let wanted = flatten_word(db);
    words.windows(2).any(|pair| pair[0] == "--db" && pair[1] == wanted)
}

/// Every SessionStart command in `settings` that runs the backup for `db`.
pub fn backup_hooks_for(settings: &Path, db: &Path) -> anyhow::Result<Vec<String>> {
    if !settings.exists() {
        return Ok(Vec::new());
    }
    let root: Value = serde_json::from_str(&std::fs::read_to_string(settings)?)
        .map_err(|e| anyhow::anyhow!("{} is not valid JSON ({e}) - refusing to touch it", settings.display()))?;
    let db = db.display().to_string();
    let mut found = Vec::new();
    for group in root["hooks"]["SessionStart"].as_array().into_iter().flatten() {
        for hook in group["hooks"].as_array().into_iter().flatten() {
            if let Some(command) = hook["command"].as_str().filter(|c| is_backup_command_for(c, &db)) {
                found.push(command.to_string());
            }
        }
    }
    Ok(found)
}

/// Remove the backup hook(s) for `db` from `settings` and nothing else: a group
/// that held anything else keeps it. The file is saved as `<name>.bak` first,
/// unless a backup of it already exists (the first one is the user's own).
/// Returns how many hooks went.
pub fn remove_backup_hook(settings: &Path, db: &Path) -> anyhow::Result<usize> {
    if backup_hooks_for(settings, db)?.is_empty() {
        return Ok(0);
    }
    let raw = std::fs::read_to_string(settings)?;
    let mut root: Value = serde_json::from_str(&raw)?;
    let db = db.display().to_string();
    let mut removed = 0;
    if let Some(groups) = root["hooks"]["SessionStart"].as_array_mut() {
        for group in groups.iter_mut() {
            if let Some(hooks) = group["hooks"].as_array_mut() {
                let before = hooks.len();
                hooks.retain(|h| !h["command"].as_str().is_some_and(|c| is_backup_command_for(c, &db)));
                removed += before - hooks.len();
            }
        }
        groups.retain(|g| g["hooks"].as_array().is_none_or(|h| !h.is_empty()));
    }
    crate::install::backup_once(settings, &raw)?;
    std::fs::write(settings, serde_json::to_string_pretty(&root)? + "\n")?;
    Ok(removed)
}

fn git_command(dir: Option<&Path>) -> Command {
    let mut command = Command::new("git");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    if let Some(dir) = dir {
        command.arg("-C").arg(dir);
    }
    command
}

/// Run git and return what it printed, or git's own reason for failing.
fn git(dir: Option<&Path>, args: &[&str]) -> anyhow::Result<String> {
    let out = git_command(dir).args(args).output().map_err(|e| anyhow::anyhow!("git could not be started ({e}); install git first"))?;
    if !out.status.success() {
        let reason = String::from_utf8_lossy(&out.stderr);
        let reason = reason.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("no reason given");
        anyhow::bail!("git {} failed: {}", args.first().copied().unwrap_or(""), reason.trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Whether a place is an address rather than a path on this computer
/// (`https://...`, `git@host:owner/repo.git`, `fileserver:/srv/thor.git`). A
/// Windows drive letter (`D:\x`) is a path.
pub fn looks_like_an_address(place: &str) -> bool {
    if place.contains("://") {
        return true;
    }
    match place.find(':') {
        Some(i) if i > 1 => !place[..i].contains(['/', '\\']),
        _ => false,
    }
}

/// Make the place usable and return the address to give git. An address is used
/// as it is. A folder that does not exist, or is empty, becomes an empty bare
/// repository (`git init --bare`, AGENTS.md step 7); one that already is a
/// repository is used as it is; anything else is refused rather than written
/// into.
fn prepare_place(place: &str) -> anyhow::Result<String> {
    if looks_like_an_address(place) {
        return Ok(place.to_string());
    }
    let dir = Path::new(place);
    if dir.exists() {
        anyhow::ensure!(dir.is_dir(), "{place} is a file, not a folder");
        let is_repo = dir.join("HEAD").is_file() && dir.join("objects").is_dir() || dir.join(".git").exists();
        let empty = dir.read_dir()?.next().is_none();
        anyhow::ensure!(
            is_repo || empty,
            "{place} already holds other files and is not a git repository, so nothing was written there; name an empty folder or a new one"
        );
        if is_repo {
            return Ok(place.to_string());
        }
    }
    std::fs::create_dir_all(dir)?;
    git(None, &["init", "--bare", place])?;
    git(Some(dir), &["symbolic-ref", "HEAD", "refs/heads/main"])?;
    Ok(place.to_string())
}

/// What `prepare_repo` did, for the plain-words report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prepared {
    pub repo: PathBuf,
    pub created: bool,
    /// The address the backup now also goes to, `None` for a local copy.
    pub place: Option<String>,
    /// A remote an earlier choice had set was taken away (local was chosen).
    pub dropped_remote: bool,
}

/// Make `repo` a git folder on branch `main` that can commit, and set its
/// remote to match the choice: none for `place == None`, `nas` otherwise (the
/// branch is pushed once so the first backup has something to sync with).
pub fn prepare_repo(repo: &Path, place: Option<&str>) -> anyhow::Result<Prepared> {
    let created = !repo.join(".git").exists();
    if created {
        std::fs::create_dir_all(repo)?;
        git(Some(repo), &["init"])?;
        git(Some(repo), &["symbolic-ref", "HEAD", "refs/heads/main"])?;
    }
    // A machine with no git identity cannot commit; give this folder (only) one.
    if git(Some(repo), &["config", "user.name"]).unwrap_or_default().is_empty() {
        git(Some(repo), &["config", "user.name", "THOR backup"])?;
    }
    if git(Some(repo), &["config", "user.email"]).unwrap_or_default().is_empty() {
        git(Some(repo), &["config", "user.email", "thor-backup@localhost.invalid"])?;
    }
    let has_nas = git(Some(repo), &["remote"]).unwrap_or_default().lines().any(|r| r.trim() == "nas");
    match place {
        None => {
            if has_nas {
                git(Some(repo), &["remote", "remove", "nas"])?;
            }
            Ok(Prepared { repo: repo.to_path_buf(), created, place: None, dropped_remote: has_nas })
        }
        Some(place) => {
            let address = prepare_place(place)?;
            if has_nas {
                git(Some(repo), &["remote", "set-url", "nas", &address])?;
            } else {
                git(Some(repo), &["remote", "add", "nas", &address])?;
            }
            if git(Some(repo), &["rev-parse", "--verify", "-q", "HEAD"]).is_err() {
                git(Some(repo), &["commit", "--allow-empty", "-m", "THOR backup repository"])?;
            }
            git(Some(repo), &["push", "-u", "nas", "main"])?;
            Ok(Prepared { repo: repo.to_path_buf(), created, place: Some(address), dropped_remote: false })
        }
    }
}

/// Run the backup program once, now, so the user knows it works. The first line
/// it prints, or the reason it failed.
pub fn prove(backup_exe: &Path, db: &Path, repo: &Path) -> Result<String, String> {
    let mut command = Command::new(backup_exe);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    let out = command.arg("--db").arg(db).arg("--repo").arg(repo).arg("--force").output().map_err(|e| format!("could not start {}: {e}", backup_exe.display()))?;
    let said = |bytes: &[u8]| String::from_utf8_lossy(bytes).lines().filter(|l| !l.trim().is_empty()).collect::<Vec<_>>().join(" | ");
    if out.status.success() {
        Ok(said(&out.stdout))
    } else {
        Err(format!("{} {}", said(&out.stdout), said(&out.stderr)).trim().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_word_is_read_as_a_choice() {
        assert_eq!(parse_choice("local"), Some(Choice::Local));
        assert_eq!(parse_choice(" OFF "), Some(Choice::Off));
        assert_eq!(parse_choice("D:\\share\\thor.git"), Some(Choice::Place("D:\\share\\thor.git".into())));
        assert_eq!(parse_choice("  "), None);
    }

    #[test]
    fn an_address_is_told_from_a_path_on_this_computer() {
        for address in ["https://example.invalid/me/thor.git", "git@example.invalid:me/thor.git", "fileserver:/srv/thor.git"] {
            assert!(looks_like_an_address(address), "{address}");
        }
        for path in ["D:\\share\\thor.git", "D:/share/thor.git", "/mnt/nas/thor.git", "\\\\nas\\share\\thor.git", "relative/thor.git"] {
            assert!(!looks_like_an_address(path), "{path}");
        }
    }

    #[test]
    fn only_a_backup_program_called_for_this_memory_counts_as_its_backup_hook() {
        let db = "C:\\thor\\thor.db";
        assert!(is_backup_command_for("\"C:/x/backup.exe\" --db \"C:/thor/thor.db\" --repo \"C:/r\"", db));
        assert!(!is_backup_command_for("\"C:/x/backup.exe\" --db \"C:/other/thor.db\" --repo \"C:/r\"", db), "another memory");
        assert!(!is_backup_command_for("\"C:/x/serve.exe\" --db \"C:/thor/thor.db\" hook", db), "not the backup program");
        assert!(!is_backup_command_for("\"C:/x/mybackup-tool.exe\" --db \"C:/thor/thor.db\"", db), "another program that only has backup in its name");
    }
}
