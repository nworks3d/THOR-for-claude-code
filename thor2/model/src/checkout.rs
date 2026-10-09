//! "Which project is this folder in", and "can a project's registered working
//! copy be used right now" - resolved from the filesystem alone, in exactly one
//! place. `serve::project` re-exports every name here, so each caller keeps the
//! path it always used.
//!
//! WHY THIS LIVES IN `model` (step 9 of the two-tracks build). A rule's
//! settlement (`store::settlement`) is read by the evaluation-filing gate,
//! which runs inside `store`'s own write path, and by the Stop hook and doctor
//! in the crates above it. A file-check rule is settled while its project has
//! no usable working copy (`read_time_blocker`), and that judgement has to be
//! the SAME one the retest makes (`registered_checkout`) - or the filing gate
//! would demand a retest whose answer is "blocked", which nothing the agent
//! runs can settle. One judge, reachable from all of them, means it lives
//! here, below both.
//!
//! Resolution order, cheapest and most explicit first:
//! 1. A marker file (see [`MARKER_FILE_NAMES`]) directly in the starting
//!    directory - its first non-blank line, trimmed, IS the project key.
//! 2. Otherwise the repository this directory belongs to, found by walking up
//!    to the nearest `.git`. A linked worktree carries a `.git` FILE pointing
//!    at its main repository, and resolution follows it: a worktree is a
//!    place you work, never a project of its own (see [`git_root`]).
//! 3. That repository's own marker file, if it has one.
//! 4. Otherwise that repository directory's basename.
//! 5. Otherwise, no project: the caller falls back to the global layer only.
//!
//! Every step degrades to `None` on anything unreadable (a missing directory,
//! a permission error) rather than erroring - this is a READ used by an
//! injection surface (session start), so it must never block and never speak
//! on failure (R5).
//!
//! `serve/tests/single_project_resolution.rs` greps the whole workspace for a
//! second definition of the resolution function and fails if one appears.

use std::path::{Path, PathBuf};
use thor_core::event_store::EventStore;

use crate::item::{Binding, Check, Item};
use crate::store::Blocker;

/// A file named this, sitting directly in a directory, wins over any git-root
/// inference for that directory - see [`resolve_project`].
pub const MARKER_FILE_NAME: &str = ".thor-project";

/// Resolve the project key for `start_dir`, or `None` for "global only".
/// Never panics, never blocks, never returns an error: every failure mode
/// (missing directory, unreadable marker file, no git root anywhere above
/// `start_dir`) collapses to `None`, the same "nothing to say" the rest of
/// the read/serve boundary uses.
pub fn resolve_project(start_dir: &Path) -> Option<String> {
    if let Some(name) = read_marker(start_dir) {
        return Some(name);
    }
    // A worktree's root is not the project - see `git_root`'s own note.
    let Some(root) = git_root(start_dir) else {
        // No repository anywhere above: a plain topic folder is still a
        // project when it (or a folder above it) carries a marker.
        return plain_root(start_dir).and_then(|top| read_marker(&top));
    };
    if root != start_dir {
        if let Some(name) = read_marker(&root) {
            return Some(name);
        }
    }
    root.file_name().and_then(|n| n.to_str()).map(str::to_string)
}

/// Marker file names, in the order they are tried. `.thor-project` is this
/// build's own name; `.thor` is 1.0's, and it is honoured because it is the
/// one that actually exists on disk.
///
/// THE DEFECT THIS CLOSES, found by auditing the migration (2026-08-03).
/// 2.0 invented a new marker name and nothing ever wrote it: a filesystem
/// walk finds five `.thor` markers and zero `.thor-project`. So resolution
/// always fell through to the git-root basename. For four of the five repos
/// the basename happens to equal the key, which is why this stayed hidden.
/// For the fifth the directory is spelled with a space where the key has a
/// hyphen, so `applies_to` compared two different strings and 110 fireable
/// items - 92 rules and 18 orientations, one of them irreversible - could
/// never be selected in their own checkout. Reproduced live: asking that
/// repo what governs one of its own files answered "no item governs this".
///
/// Harmless until the moment surface started scoping by project earlier the
/// same day, exactly like the `project: "global"` case. A correct filter over
/// an identity nobody translated deletes whatever the identity names.
///
/// Public so `ops::health`'s own `--checkouts` inference (walking ancestors
/// for "is this directory a repo" - a THOR marker or a `.git` entry) can ask
/// the same two names this module already treats as authoritative, instead
/// of keeping a second, driftable copy of the list.
pub const MARKER_FILE_NAMES: &[&str] = &[MARKER_FILE_NAME, ".thor"];

/// The first marker file's first non-blank line, trimmed - `None` when none
/// is present, unreadable, or empty/whitespace-only (an empty marker names
/// nothing, so resolution falls through rather than stopping on a blank
/// answer).
fn read_marker(dir: &Path) -> Option<String> {
    for name in MARKER_FILE_NAMES {
        let Ok(content) = std::fs::read_to_string(dir.join(name)) else { continue };
        let Some(line) = content.lines().next().map(str::trim) else { continue };
        if !line.is_empty() {
            return Some(line.to_string());
        }
    }
    None
}

/// Walk `start_dir` and its ancestors (inclusive) looking for the nearest one
/// that contains a `.git` entry, and return THAT DIRECTORY. `None` when no
/// ancestor up to the filesystem root has one.
///
/// Public because the checkout a project lives in is the other half of
/// resolving it, and callers that need both (the MCP server picking which
/// code index applies to the directory it was started in) must not walk for
/// it themselves - the whole point of this module is that project resolution
/// happens in exactly one place (CONTRACT R6).
/// A LINKED WORKTREE resolves to the repository it belongs to, not to itself.
///
/// THE DEFECT THIS CLOSES, found by auditing the migration (2026-08-03). In a
/// linked worktree `.git` is a FILE, not a directory, holding a line like
/// `gitdir: <main>/.git/worktrees/<slug>`. The old walk stopped there and
/// handed back the worktree directory, whose basename is a generated slug.
/// Reproduced live: starting a session inside one of the owner's worktrees
/// resolved the project to `hungry-wright-171e5d`, so every rule that repo
/// owns was scoped out of every pushed surface - in the very checkout where
/// the work was happening. This build's own agent tooling creates worktrees,
/// so it is not a corner case.
///
/// Read from the file rather than by running git: this is on the path of a
/// hook that fires on every tool call, and it must never block on a
/// subprocess. Anything unexpected in the file falls back to the directory
/// itself, which is the old behaviour and never worse than it.
pub fn git_root(start_dir: &Path) -> Option<PathBuf> {
    let mut dir: Option<&Path> = Some(start_dir);
    while let Some(d) = dir {
        let dot_git = d.join(".git");
        if dot_git.is_dir() {
            return Some(d.to_path_buf());
        }
        if dot_git.is_file() {
            return Some(main_root_of_worktree(&dot_git).unwrap_or_else(|| d.to_path_buf()));
        }
        dir = d.parent();
    }
    None
}

/// The top of a PLAIN TOPIC FOLDER: the nearest folder at or above `start_dir`
/// that carries a `.thor-project` marker, when no git repository sits above
/// `start_dir` at all. `None` inside a repository (git decides there, exactly
/// as before) and for a folder nobody marked.
///
/// WHY A MARKER AND NOT THE FOLDER NAME ALONE. A session is always started in
/// some folder, including the home folder or a drive root; naming those a
/// project by their basename would put everything a person ever does there
/// into one memory. A topic folder is one somebody asked for (`install
/// --topic`), and the marker is the proof of that. Only the current marker
/// name counts above `start_dir`: the old `.thor` name is still honoured in
/// the folder itself, but never inherited by every folder below it.
pub fn plain_root(start_dir: &Path) -> Option<PathBuf> {
    if git_root(start_dir).is_some() {
        return None;
    }
    let mut dir: Option<&Path> = Some(start_dir);
    while let Some(d) = dir {
        let named = std::fs::read_to_string(d.join(MARKER_FILE_NAME))
            .ok()
            .is_some_and(|content| content.lines().next().is_some_and(|line| !line.trim().is_empty()));
        if named {
            return Some(d.to_path_buf());
        }
        dir = d.parent();
    }
    None
}

/// The root of the LINKED WORKTREE `start_dir` sits in - the directory that
/// carries the `.git` file - or `None` when the nearest `.git` above it is a
/// real repository (a directory) or a file this does not recognise as a
/// worktree pointer.
///
/// Exists for the one question `git_root` deliberately hides: "is the place
/// I am working in a worktree, and where is it". Every check is judged
/// against the main checkout (`git_root`), which is what doctor and the
/// commit gates see - and measured on 2026-09-26, a session attached a
/// `contains` check to code that existed only in its own worktree: stored
/// without complaint, it then blocked every commit on main for eleven
/// minutes. A caller that knows the worktree can refuse that before it is
/// written (the tool server's `remember`/`revise`).
pub fn linked_worktree_root(start_dir: &Path) -> Option<PathBuf> {
    let mut dir: Option<&Path> = Some(start_dir);
    while let Some(d) = dir {
        let dot_git = d.join(".git");
        if dot_git.is_dir() {
            return None;
        }
        if dot_git.is_file() {
            return main_root_of_worktree(&dot_git).map(|_| d.to_path_buf());
        }
        dir = d.parent();
    }
    None
}

/// The checkout a directory of checkouts should be judged as: the main
/// repository for a linked worktree's root, the directory itself otherwise.
/// Never walks upward - only a directory whose OWN `.git` is a worktree
/// pointer is redirected, so a plain folder is never swapped for some
/// unrelated repository above it.
pub fn checkout_for(dir: &Path) -> PathBuf {
    let dot_git = dir.join(".git");
    if dot_git.is_file() {
        if let Some(main) = main_root_of_worktree(&dot_git) {
            return main;
        }
    }
    dir.to_path_buf()
}

/// The main repository root a linked worktree points at, read from its `.git`
/// file. `None` for any shape this does not recognise, so the caller keeps
/// the old answer instead of inventing one.
fn main_root_of_worktree(dot_git_file: &Path) -> Option<PathBuf> {
    let content = std::fs::read_to_string(dot_git_file).ok()?;
    let gitdir = content.lines().find_map(|l| l.trim().strip_prefix("gitdir:"))?.trim();
    let gitdir = Path::new(gitdir);
    // <main>/.git/worktrees/<slug>  ->  <main>
    let worktrees = gitdir.parent()?;
    if worktrees.file_name()? != "worktrees" {
        return None;
    }
    let git_dir = worktrees.parent()?;
    if git_dir.file_name()? != ".git" {
        return None;
    }
    git_dir.parent().map(Path::to_path_buf)
}

/// The top of the checkout `cwd` sits in: a linked worktree's own root when
/// `cwd` is inside one, else the repository root (`git_root`), else `cwd`
/// itself. Unlike `git_root` it never swaps a worktree for its main checkout -
/// the place a session works in is where its files are. A plain topic folder
/// (`plain_root`) is its own top.
pub fn checkout_top(cwd: &Path) -> PathBuf {
    linked_worktree_root(cwd)
        .or_else(|| git_root(cwd))
        .or_else(|| plain_root(cwd))
        .unwrap_or_else(|| cwd.to_path_buf())
}

/// Why a project's registered working copy cannot be used right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RootProblem {
    /// Nothing registered for this project (`install.exe --register-root`
    /// was never run from its checkout).
    NotRegistered,
    /// The item names no project, so it has no working copy of its own.
    GlobalRule,
    /// The registered folder no longer exists.
    Gone(PathBuf),
    /// The registered folder is not the top of a git checkout (no `.git` in
    /// it, or it sits inside a larger checkout).
    NotCheckoutTop(PathBuf),
    /// The registered folder is a linked worktree, not a main checkout.
    Worktree(PathBuf),
    /// The registered folder now resolves to another project, or to none.
    ResolvesTo(PathBuf, Option<String>),
}

impl std::fmt::Display for RootProblem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let show = |p: &PathBuf| p.display().to_string().replace('\\', "/");
        match self {
            RootProblem::NotRegistered => write!(f, "no working copy is registered"),
            RootProblem::GlobalRule => write!(f, "a global item has no working copy of its own"),
            RootProblem::Gone(p) => write!(f, "the registered folder {} no longer exists", show(p)),
            RootProblem::NotCheckoutTop(p) => write!(f, "{} is not the top of a project folder (a git checkout, or a topic folder with a .thor-project file)", show(p)),
            RootProblem::Worktree(p) => write!(f, "{} is a linked worktree, not a main checkout", show(p)),
            RootProblem::ResolvesTo(p, Some(other)) => write!(f, "{} now resolves to project '{other}'", show(p)),
            RootProblem::ResolvesTo(p, None) => write!(f, "{} now resolves to no project", show(p)),
        }
    }
}

/// The working copy `project` is registered at, checked every time it is
/// read: the folder exists, it is the top of a git checkout, that checkout is
/// a main one (not a linked worktree), and it still resolves to `project`.
///
/// Read from the registration alone (`crate::store::registered_root_raw`,
/// installer events only). No fallback to the current folder, a server's
/// startup folder or any sidecar: each of those can be pointed at a copy by
/// whoever runs the session, which is exactly what a registration exists to
/// rule out.
pub fn registered_checkout(
    store: &EventStore,
    project: Option<&str>,
) -> Result<PathBuf, RootProblem> {
    let Some(project) = project else { return Err(RootProblem::GlobalRule) };
    let Some((root, _seq)) = crate::store::registered_root_raw(store, project) else {
        return Err(RootProblem::NotRegistered);
    };
    validate_checkout(&root, project)
}

/// The checks `registered_checkout` applies to a registered folder, on their
/// own - `Ok(root)` when all four hold.
pub fn validate_checkout(root: &Path, project: &str) -> Result<PathBuf, RootProblem> {
    let root = root.to_path_buf();
    if !root.exists() {
        return Err(RootProblem::Gone(root));
    }
    if checkout_for(&root) != root {
        return Err(RootProblem::Worktree(root));
    }
    // The top of a git checkout, or the top of a plain topic folder (the
    // marked folder itself, with no repository anywhere above it).
    let is_top = git_root(&root).as_deref() == Some(root.as_path()) || plain_root(&root).as_deref() == Some(root.as_path());
    if !is_top {
        return Err(RootProblem::NotCheckoutTop(root));
    }
    let resolved = resolve_project(&root);
    if resolved.as_deref() != Some(project) {
        return Err(RootProblem::ResolvesTo(root, resolved));
    }
    Ok(root)
}

// ------------------------------------------------------------ remotes, no process

/// Every URL the repository at `repo_root` names for a remote (`url` and
/// `pushurl` of each `[remote "..."]` section of its git config), read from the
/// config FILE: no process is started, no network is touched. `repo_root` is
/// the top of a checkout; a linked worktree's `.git` file is followed to the
/// repository's common config. Empty on anything unreadable, so a caller that
/// is only looking for a match simply finds none.
///
/// A remote that git REWRITES (`url.<base>.insteadOf = <prefix>` in this
/// config or the user's own) is listed twice: as written and as git reads it,
/// so a clone reached through a rewritten address still matches the project
/// that holds the other spelling (`rewrite_remotes`).
pub fn remote_urls(repo_root: &Path) -> Vec<String> {
    let Some(config) = config_file(repo_root) else { return Vec::new() };
    let Ok(text) = std::fs::read_to_string(config) else { return Vec::new() };
    let mut rewrites = url_rewrites(&text);
    rewrites.extend(user_url_rewrites());
    rewrite_remotes(remote_urls_in(&text), &rewrites)
}

/// Every `url.<base>.insteadOf = <prefix>` (and `pushInsteadOf`) of a git
/// config text, as (prefix, base).
fn url_rewrites(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut base: Option<String> = None;
    for line in text.lines() {
        let line = line.trim();
        if let Some(header) = line.strip_prefix('[') {
            let mut parts = header.trim_end_matches(']').splitn(2, '"');
            let section = parts.next().unwrap_or("").trim();
            base = (section.eq_ignore_ascii_case("url")).then(|| parts.next().unwrap_or("").trim_end_matches('"').to_string());
            continue;
        }
        let (Some(base), Some((key, value))) = (base.as_ref(), line.split_once('=')) else { continue };
        let key = key.trim();
        if !(key.eq_ignore_ascii_case("insteadof") || key.eq_ignore_ascii_case("pushinsteadof")) {
            continue;
        }
        let value = value.trim().trim_matches('"');
        if !value.is_empty() && !base.is_empty() {
            out.push((value.to_string(), base.clone()));
        }
    }
    out
}

/// The rewrites of the user's own git configuration (`GIT_CONFIG_GLOBAL`, else
/// `~/.gitconfig` and the XDG file). Empty when none can be read.
fn user_url_rewrites() -> Vec<(String, String)> {
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).map(PathBuf::from);
    let mut files: Vec<PathBuf> = Vec::new();
    match std::env::var_os("GIT_CONFIG_GLOBAL") {
        Some(path) => files.push(PathBuf::from(path)),
        None => {
            files.extend(home.iter().map(|h| h.join(".gitconfig")));
            let xdg = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).or_else(|| home.as_ref().map(|h| h.join(".config")));
            files.extend(xdg.map(|x| x.join("git").join("config")));
        }
    }
    files.iter().filter_map(|f| std::fs::read_to_string(f).ok()).flat_map(|text| url_rewrites(&text)).collect()
}

/// `urls`, each followed by the address git reads it as when a rewrite's
/// prefix starts it (the longest prefix wins, as in git).
fn rewrite_remotes(urls: Vec<String>, rewrites: &[(String, String)]) -> Vec<String> {
    let mut out = Vec::new();
    for url in urls {
        let rewritten = rewrites
            .iter()
            .filter(|(prefix, _)| url.starts_with(prefix.as_str()))
            .max_by_key(|(prefix, _)| prefix.len())
            .map(|(prefix, base)| format!("{base}{}", &url[prefix.len()..]));
        out.push(url);
        out.extend(rewritten);
    }
    out
}

fn remote_urls_in(text: &str) -> Vec<String> {
    let mut urls = Vec::new();
    let mut in_remote = false;
    for line in text.lines() {
        let line = line.trim();
        if let Some(header) = line.strip_prefix('[') {
            let name = header.split(|c: char| c.is_whitespace() || c == ']').next().unwrap_or("");
            in_remote = name.eq_ignore_ascii_case("remote") && header.contains('"');
            continue;
        }
        if !in_remote {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else { continue };
        if !(key.trim().eq_ignore_ascii_case("url") || key.trim().eq_ignore_ascii_case("pushurl")) {
            continue;
        }
        let value = value.trim();
        let value = match value.strip_prefix('"') {
            Some(quoted) => quoted.split('"').next().unwrap_or(""),
            None => value.split(['#', ';']).next().unwrap_or("").trim(),
        };
        if !value.is_empty() {
            urls.push(value.to_string());
        }
    }
    urls
}

/// The git config file of the checkout at `root`: `.git/config`, or, when
/// `.git` is a worktree pointer file, the common repository's config.
fn config_file(root: &Path) -> Option<PathBuf> {
    let dot_git = root.join(".git");
    if dot_git.is_dir() {
        return Some(dot_git.join("config"));
    }
    let content = std::fs::read_to_string(&dot_git).ok()?;
    let gitdir = content.lines().find_map(|l| l.trim().strip_prefix("gitdir:"))?.trim();
    let gitdir = root.join(gitdir);
    match std::fs::read_to_string(gitdir.join("commondir")) {
        Ok(common) => Some(gitdir.join(common.trim()).join("config")),
        Err(_) => Some(gitdir.join("config")),
    }
}

/// A remote URL in the one form two spellings of the same remote share:
/// `host/owner/repo`, lowercase, with no scheme, user, port, trailing slash or
/// `.git`. `https://Host/Owner/Repo.git`, `ssh://git@host:22/owner/repo` and
/// `git@host:owner/repo` all read `host/owner/repo`. `None` for a URL that
/// names no host (a local path, `file://`, a Windows drive path), which never
/// identifies a remote.
pub fn normalise_remote(url: &str) -> Option<String> {
    let url = url.trim();
    let (host, path) = match url.split_once("://") {
        Some((scheme, _)) if scheme.eq_ignore_ascii_case("file") => return None,
        Some((_, rest)) => {
            let (authority, path) = rest.split_once('/')?;
            let host = authority.rsplit('@').next()?.split(':').next()?;
            (host, path)
        }
        None => {
            let (left, path) = url.split_once(':')?;
            let host = left.rsplit('@').next()?;
            if host.len() < 2 || host.contains(['/', '\\']) {
                return None;
            }
            (host, path)
        }
    };
    let path = path.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path).trim_end_matches('/');
    if host.is_empty() || path.is_empty() {
        return None;
    }
    Some(format!("{}/{}", host.to_lowercase(), path.to_lowercase()))
}

// ------------------------------------------------------------ git, read only

/// `git -C <root> <args>`, its stdout when it succeeds. Read-only callers
/// only: nothing in this crate runs a git command that writes.
pub fn git(root: &Path, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("git").arg("-C").arg(root).args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).to_string())
}

/// The commit at the top of the checkout in `root`.
pub fn head_commit(root: &Path) -> Option<String> {
    git(root, &["rev-parse", "HEAD"]).map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// The git path of a file under `root`, with forward slashes.
pub fn git_path(rel: &str) -> String {
    rel.replace('\\', "/").trim_start_matches("./").to_string()
}

/// Whether `literal` is in the blob of `rel` at `rev` of the checkout.
pub fn in_blob(root: &Path, rev: &str, rel: &str, literal: &str) -> bool {
    git(root, &["show", &format!("{rev}:{}", git_path(rel))]).is_some_and(|blob| blob.contains(literal))
}

/// The default branch's name: the remote's `HEAD`, else `main` or `master`,
/// else the checkout's own `HEAD`.
pub fn default_branch(root: &Path) -> String {
    if let Some(r) = git(root, &["symbolic-ref", "--quiet", "--short", "refs/remotes/origin/HEAD"]) {
        return r.trim().to_string();
    }
    for b in ["main", "master"] {
        if git(root, &["rev-parse", "--verify", "--quiet", b]).is_some() {
            return b.to_string();
        }
    }
    "HEAD".to_string()
}

// ------------------------------------------------- the blocker, at read time

/// Whether `item` is anchored to the file `path`: a Path binding on exactly
/// it, or a Dir binding on a directory that holds it (slashes compared
/// alike). A file check on a rule anchored nowhere near its file never
/// reaches a write to it.
pub fn binds_the_file(item: &Item, path: &str) -> bool {
    let fold = |p: &str| p.replace('\\', "/").trim_end_matches('/').to_string();
    let file = fold(path);
    item.bindings.iter().any(|b| match b {
        Binding::Target { kind: crate::item::TargetKind::Path, value } => fold(value) == file,
        Binding::Target { kind: crate::item::TargetKind::Dir, value } => {
            let dir = fold(value);
            dir.is_empty() || dir == "." || file.starts_with(&format!("{dir}/"))
        }
        _ => false,
    })
}

/// `Blocker::AnchorFull` for a contains or absent check on `path` (step 24)
/// that the rule is not anchored to, when the file is really in the
/// registered checkout `root` and the one fix - anchoring the rule to it - is
/// refused because that place already holds the most facts a place ever
/// shows, AND no fact there honestly gives way (step 26: every one is a rule
/// whose check can refuse and is heavier). The same rule `crowded_path_anchor`
/// applies to a `path_exists` check: the rule keeps its true check and is
/// counted apart. With an honest exit the place is the agent's to clear
/// (`full_file_place`).
pub fn crowded_file_anchor(store: &EventStore, item: &Item, root: &Path, path: &str) -> Option<Blocker> {
    full_file_place(store, item, root, path)
        .filter(|place| !place.honest_exit)
        .map(|_| Blocker::AnchorFull { path: path.to_string() })
}

/// The full place a contains or absent check on `path` needs as its anchor
/// (the rule is not anchored to it, the file is really in the registered
/// checkout `root`), with its occupants and whether an honest exit exists -
/// `None` while the place has room (step 26). With an exit, making room is
/// the AGENT's (`FullPlace::agent_line`); without one, it is THOR's own point
/// (`crowded_file_anchor`).
pub fn full_file_place(store: &EventStore, item: &Item, root: &Path, path: &str) -> Option<crate::store::FullPlace> {
    if binds_the_file(item, path) || !root.join(path).exists() {
        return None;
    }
    crate::store::FullPlace::of(store, item, path)
}

/// Whether a check reads a file, and so needs a working copy to be tried.
pub fn is_file_check(check: &Check) -> bool {
    matches!(check, Check::Contains { .. } | Check::Absent { .. } | Check::AbsentAll { .. } | Check::PathExists { .. })
}

/// `Blocker::AnchorFull` for a `path_exists` check on `path` that the rule is
/// not anchored to (A4 refuses it), when the file is really in the registered
/// checkout `root` and the one fix - anchoring the rule to it - is refused
/// because that place already holds the most facts a place ever shows
/// (`store::full_place_occupants`) and no fact there honestly gives way (step
/// 26: every occupant is a rule whose check can refuse and is heavier). `None`
/// for any other state: a file that is gone is a rotten check with a different
/// fix, and a place with room - or a full one with an honest exit - is the
/// agent's to make (step 22, C3b B4; step 26).
pub fn crowded_path_anchor(store: &EventStore, item: &Item, root: &Path, path: &str) -> Option<Blocker> {
    full_path_place(store, item, root, path)
        .filter(|place| !place.honest_exit)
        .map(|_| Blocker::AnchorFull { path: path.to_string() })
}

/// `full_file_place` for a `path_exists` check on `path` whose literal the
/// text does not name (A4 refuses it): the full place, its occupants and
/// whether an honest exit exists - `None` while it has room (step 26).
pub fn full_path_place(store: &EventStore, item: &Item, root: &Path, path: &str) -> Option<crate::store::FullPlace> {
    if crate::gate::unnamed_literals(item).is_empty() || !root.join(path).exists() {
        return None;
    }
    crate::store::FullPlace::of(store, item, path)
}

/// What holds `item` from being decided right now, judged at READ time the
/// same way `serve::teeth::retest` judges it, so a reader never demands a
/// retest whose answer is "blocked" (F9):
/// - `NoWorkingCopy` - a file check of a project whose registered working
///   copy cannot be used (`registered_checkout` fails: none registered, the
///   folder gone, not a checkout top, a linked worktree, or resolving to
///   another project). A rule with NO project whose check reads a file is
///   `Blocker::global_rules` (step 20, K10): it has no working copy anywhere,
///   and giving it a project would narrow a rule meant for every repository.
/// - `WorkingTreeDiffers` - a `contains` check whose literals the text names
///   (A4 holds, which the retest asks first), whose file is in the registered
///   checkout without the literal, while the default branch's HEAD still has
///   it: the owner's checkout is mid-change or on another branch.
///
/// Reads the log and the filesystem; runs `git show` only for a contains
/// check whose file lacks its literal. `None` for every item nothing blocks.
pub fn read_time_blocker(store: &EventStore, item: &Item) -> Option<Blocker> {
    let check = item.check.as_ref().filter(|c| is_file_check(c))?;
    // A rule with no project has no working copy at all to try a file check
    // in (step 20, K10): blocked, counted apart, the owner's to decide.
    let Some(project) = crate::normalize::normalize_project(item.project.as_deref()) else {
        return Some(Blocker::global_rules());
    };
    let root = match registered_checkout(store, Some(&project)) {
        Ok(root) => root,
        Err(_) => return Some(Blocker::NoWorkingCopy { project }),
    };
    if let Check::PathExists { path } = check {
        return crowded_path_anchor(store, item, &root, path);
    }
    // A contains or absent check the rule is not anchored to, on a place that
    // is already full (step 24): the owner's decision, as for path_exists.
    if let Check::Contains { path, .. } | Check::Absent { path, .. } | Check::AbsentAll { path, .. } = check {
        if let Some(blocker) = crowded_file_anchor(store, item, &root, path) {
            return Some(blocker);
        }
    }
    let Check::Contains { path, literal } = check else { return None };
    if !crate::gate::unnamed_literals(item).is_empty() || !root.join(path).is_file() {
        return None;
    }
    if crate::check::run(check, &root) != crate::check::Outcome::Fails {
        return None;
    }
    in_blob(&root, &default_branch(&root), path, literal).then(|| Blocker::WorkingTreeDiffers { path: path.clone() })
}

#[cfg(test)]
mod remote_tests {
    use super::*;

    #[test]
    fn two_spellings_of_one_remote_read_the_same() {
        let same = [
            "https://example.invalid/Owner/Repo.git",
            "https://user:secret@EXAMPLE.invalid/owner/repo/",
            "ssh://git@example.invalid:22/owner/repo.git",
            "git@example.invalid:owner/repo.git",
            "example.invalid:owner/repo",
        ];
        for url in same {
            assert_eq!(normalise_remote(url).as_deref(), Some("example.invalid/owner/repo"), "{url}");
        }
        assert_ne!(normalise_remote("https://example.invalid/owner/other.git"), normalise_remote(same[0]));
        assert_ne!(normalise_remote("https://example.invalid/other/repo.git"), normalise_remote(same[0]));
    }

    #[test]
    fn a_local_path_names_no_remote() {
        for url in ["/srv/git/repo.git", "../repo", "C:/work/repo", r"C:\work\repo","file:///srv/repo.git", "", "https://example.invalid"] {
            assert_eq!(normalise_remote(url), None, "{url:?}");
        }
    }

    #[test]
    fn remote_urls_come_from_the_config_file_alone() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".git")).unwrap();
        std::fs::write(
            tmp.path().join(".git/config"),
            "[core]\n\turl = not-a-remote\n[remote \"origin\"]\n\turl = https://example.invalid/o/r.git\n\tfetch = +refs/heads/*:refs/remotes/origin/*\n[remote \"nas\"]\n\tPushURL = \"git@example.invalid:o/r.git\" # backup\n[branch \"main\"]\n\turl = nope\n",
        )
        .unwrap();
        assert_eq!(remote_urls(tmp.path()), vec!["https://example.invalid/o/r.git", "git@example.invalid:o/r.git"]);
        assert!(remote_urls(&tmp.path().join("not-there")).is_empty());
    }

    /// A remote rewritten with `url.<base>.insteadOf` is listed as written and
    /// as git reads it, so a clone reached through the rewritten address still
    /// matches the project that holds the other spelling (collect 16).
    #[test]
    fn a_rewritten_remote_is_listed_as_git_reads_it_too() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".git")).unwrap();
        std::fs::write(
            tmp.path().join(".git/config"),
            "[url \"git@example.invalid:\"]\n\tinsteadOf = https://example.invalid/\n[url \"git@example.invalid:org/\"]\n\tInsteadOf = https://example.invalid/org/\n[remote \"origin\"]\n\turl = https://example.invalid/org/repo.git\n[remote \"other\"]\n\turl = https://elsewhere.invalid/x/y.git\n",
        )
        .unwrap();
        let urls = remote_urls(tmp.path());
        assert_eq!(
            urls,
            vec!["https://example.invalid/org/repo.git", "git@example.invalid:org/repo.git", "https://elsewhere.invalid/x/y.git"],
            "the longest prefix wins, and an address no rewrite starts is listed once"
        );
        assert_eq!(normalise_remote(&urls[1]).as_deref(), Some("example.invalid/org/repo"));
        // Pure helpers.
        assert_eq!(
            rewrite_remotes(vec!["gh:o/r".to_string()], &[("gh:".to_string(), "https://example.invalid/".to_string())]),
            vec!["gh:o/r", "https://example.invalid/o/r"]
        );
        assert_eq!(url_rewrites("[url \"b\"]\n\tpushInsteadOf = p\n[core]\n\tinsteadOf = nope\n"), vec![("p".to_string(), "b".to_string())]);
    }
}
