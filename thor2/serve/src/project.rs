//! "Which project is this session in", resolved from a filesystem location -
//! the ONE door the serving crates use for this resolution. Before this
//! module existed, `serve session-start` and the `hook` channel's SessionStart
//! branch both required the caller to name the project explicitly (see
//! `bin/serve.rs`'s old doc comment: "there is no cwd-to-project resolver in
//! this workspace"); CONTRACT.md's "session start hands back this project's
//! standing rules" cannot hold without one, so this closes that gap.
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
//! Steps 2 to 4 exist because both of this module's measured defects had one
//! shape: an item named a project that resolution could never produce. See
//! [`MARKER_FILE_NAMES`] and [`git_root`] for the two cases, and
//! [`orphaned_project_keys`] for the inventory that makes the next one
//! visible instead of silent.
//!
//! Every step degrades to `None` on anything unreadable (a missing directory,
//! a permission error) rather than erroring - this is a READ used by an
//! injection surface (session start), so it must never block and never speak
//! on failure (R5), exactly like `live::live_items`.
//!
//! The resolution itself, and the check of a project's registered working
//! copy, live in `model::checkout` since step 9 of the two-tracks build (a
//! rule's settlement, read by the filing gate inside `model::store`, needs the
//! same judgement the retest makes) and are re-exported here unchanged, so
//! every caller keeps this path. `serve/tests/single_project_resolution.rs`
//! greps the whole workspace for a second definition and fails if one appears.

use std::path::Path;

pub use model::checkout::{
    checkout_for, checkout_top, git_root, linked_worktree_root, plain_root, registered_checkout, resolve_project,
    validate_checkout,
    RootProblem, MARKER_FILE_NAME, MARKER_FILE_NAMES,
};

/// May an item declared under `item_project` be served to a session sitting in
/// `session_project`? The ONE place that answers this - every injection
/// surface calls it instead of writing the comparison out again.
///
/// - An item with no project of its own is global: it applies everywhere.
/// - An item that names a project applies only inside that project.
/// - A session whose project could not be resolved gets the global layer
///   only. That is deliberately the conservative direction: serving one
///   project's rules inside another is worse than serving fewer, because a
///   rule that does not apply here teaches the reader to skim past the block.
///
/// THE DEFECT THIS CLOSES, measured on the live store (2026-08-03). Session
/// start already scoped by project; the moment-of-action surface did not, so
/// every moment-bound rule in the store competed for that surface's four
/// slots in every project. The store holds 136 moment-bound rules belonging
/// to one business project against 66 for this one and 74 global, so a coding
/// session here was mostly being shown another project's rules - `deploy`
/// alone had 48 applicable and showed 4. It was caught in the act: writing a
/// Dockerfile for the NAS drew rules about a website's dev directory, an
/// admin HTTP route and a production database wipe. Every one of those rules
/// is correct. Not one of them was about this.
pub fn applies_to(item_project: Option<&str>, session_project: Option<&str>) -> bool {
    match item_project {
        None => true,
        Some(owner) => session_project == Some(owner),
    }
}


/// The project a session is working in: the project its working folder
/// resolves to, else the project this session last did work in
/// (`usefulness::last_project_of_session`). The ONE answer the Stop hook's
/// debts, the work clock and the hard guards all use.
///
/// THE DODGE THIS CLOSES (D4). The guards used the folder's own answer alone,
/// so one `cd` into a folder of no project - a scratch folder, a temp dir -
/// switched off every guard scoped to the project the session was working
/// in, while the Stop hook and the work clock already kept it. A folder that
/// resolves to a project still wins, so a session that moves into ANOTHER
/// project is judged under that project's rules.
///
/// This answer keeps the guards that read only the TEXT of a call (a
/// command, a requires trigger, an always-forbidden literal). The guards
/// anchored at a PATH are kept by the other half, `guard_root`: after a drift
/// they resolve against the project's registered checkout, not the folder.
///
/// The fallback reads a sidecar beside the store, which any local process can
/// write (N3): forging it can drop the fallback or name another project, but
/// never overrides a folder that resolves to one. Serving (what a session is
/// shown) keeps the folder's own answer and does not use this.
pub fn session_project(db: &Path, session_id: &str, cwd: Option<&Path>) -> Option<String> {
    cwd.and_then(resolve_project).or_else(|| crate::usefulness::last_project_of_session(db, session_id))
}

/// The folder every hard guard resolves a check path against: the top of the
/// checkout the session works in.
///
/// - The working folder resolves to a project (`folder_project` is
///   `Some`): the top of the checkout it sits in (`checkout_top`), a linked
///   worktree's own root included.
/// - It resolves to none, but the session kept a project (the answer of
///   `session_project()` came from its fallback): that project's registered
///   checkout (`registered_checkout`), when it is registered and still valid.
/// - Anything else: `checkout_top(cwd)`, which for a folder of no project is
///   the folder itself - exactly as before.
///
/// THE HALF OF D4 THIS CLOSES. The path-anchored guards (contains, absent,
/// absent_all, location) take no project filter: their anchors are resolved
/// against this root. With the folder as the root, a session that worked in
/// a checkout and then stood in a scratch folder could Edit or `rm` that
/// checkout's own files by their absolute path with every one of those
/// guards off - the fallback project kept only the text guards. With the
/// registered checkout as the root, the project's real files keep their
/// guards from anywhere, and a scratch COPY of one of them is no longer
/// judged as if it were the project's file.
///
/// Only the registration is trusted for this (`registered_checkout`, the
/// installer's own events, validated on every read), never the sidecar that
/// supplied the fallback project: that sidecar is writable by any local
/// process (N3), so a root read from it could be pointed at a copy. A project
/// with no valid registration keeps the folder as its root, and so keeps only
/// its text guards after a drift until its checkout is registered
/// (`install.exe --register-root --db "<store>"`, run from the checkout).
pub fn guard_root(
    store: &thor_core::event_store::EventStore,
    cwd: Option<&Path>,
    folder_project: Option<&str>,
    session_project: Option<&str>,
) -> Option<std::path::PathBuf> {
    if folder_project.is_none() {
        if let Ok(registered) = registered_checkout(store, session_project) {
            return Some(registered);
        }
    }
    cwd.map(checkout_top)
}


/// How many days after a working copy was registered or moved the session
/// start still names it.
pub const REGISTRATION_NOTICE_DAYS: i64 = 7;

/// The one line a session start in `project` shows for
/// `REGISTRATION_NOTICE_DAYS` after its working copy was registered or moved
/// (N11: the first registration is first-come, so whoever registered it is
/// named where the owner looks). `None` outside that window, for a project
/// with no registration, or one whose time cannot be read.
pub fn registration_notice(
    store: &thor_core::event_store::EventStore,
    project: Option<&str>,
    now_unix: i64,
) -> Option<String> {
    let project = project?;
    let regs = model::store::registrations_for_machine(store, project, &model::machine::current_machine_id());
    let last = regs.last()?;
    let at_text = last.at.as_deref()?;
    let at = crate::time::unix_from_iso8601(at_text)?;
    if now_unix - at > REGISTRATION_NOTICE_DAYS * 86_400 {
        return None;
    }
    let date: String = at_text.chars().take(10).collect();
    let moved_from = regs.iter().rev().nth(1).map(|r| r.root.as_str()).filter(|old| *old != last.root);
    Some(match moved_from {
        Some(old) => format!(
            "[THOR] the working copy of {project} is registered at {} since {date} (moved from {old}, which no longer exists)",
            last.root
        ),
        None => format!("[THOR] the working copy of {project} is registered at {} since {date}", last.root),
    })
}

/// Every project key held by a live item that NO reachable checkout resolves
/// to. The inventory `doctor` needs, because the two defects this module has
/// now had are both of one shape - an item names a project nothing answers to
/// - and neither was visible from any surface. A rule scoped out is
/// indistinguishable from a rule that simply never matched.
///
/// `resolvable` is what the caller found by resolving every checkout it knows
/// about; anything an item claims that is not in there is orphaned.
pub fn orphaned_project_keys<'a>(
    item_projects: impl IntoIterator<Item = &'a str>,
    resolvable: &std::collections::BTreeSet<String>,
) -> std::collections::BTreeMap<String, usize> {
    let mut out: std::collections::BTreeMap<String, usize> = Default::default();
    for key in item_projects {
        if !resolvable.contains(key) {
            *out.entry(key.to_string()).or_default() += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tempdir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn a_marker_file_names_the_project_directly() {
        let dir = tempdir();
        fs::write(dir.path().join(MARKER_FILE_NAME), "widget-factory\n").unwrap();
        assert_eq!(resolve_project(dir.path()), Some("widget-factory".to_string()));
    }

    #[test]
    fn a_marker_file_wins_over_a_git_roots_basename() {
        let dir = tempdir();
        fs::create_dir(dir.path().join(".git")).unwrap();
        fs::write(dir.path().join(MARKER_FILE_NAME), "overridden-name\n").unwrap();
        assert_eq!(resolve_project(dir.path()), Some("overridden-name".to_string()));
    }

    #[test]
    fn the_git_roots_basename_is_used_when_there_is_no_marker() {
        let dir = tempdir();
        let repo = dir.path().join("repo-name");
        fs::create_dir(&repo).unwrap();
        fs::create_dir(repo.join(".git")).unwrap();
        assert_eq!(resolve_project(&repo), Some("repo-name".to_string()));
    }

    #[test]
    fn a_subdirectory_of_a_git_repo_still_resolves_the_repo_roots_name() {
        let dir = tempdir();
        let repo = dir.path().join("repo-name");
        let sub = repo.join("src").join("deep");
        fs::create_dir_all(&sub).unwrap();
        fs::create_dir(repo.join(".git")).unwrap();
        assert_eq!(resolve_project(&sub), Some("repo-name".to_string()));
    }

    #[test]
    fn a_git_file_not_only_a_git_directory_counts_as_a_repo_root() {
        // Worktrees and submodules record `.git` as a FILE pointing elsewhere,
        // not a directory - this must count too, since this function only
        // checks for the entry's existence, never its shape.
        let dir = tempdir();
        let repo = dir.path().join("worktree-name");
        fs::create_dir(&repo).unwrap();
        fs::write(repo.join(".git"), "gitdir: /somewhere/else\n").unwrap();
        assert_eq!(resolve_project(&repo), Some("worktree-name".to_string()));
    }

    #[test]
    fn no_marker_and_no_git_root_yields_no_project() {
        let dir = tempdir();
        assert_eq!(resolve_project(dir.path()), None, "an ordinary directory outside any git repo must resolve to global-only");
    }

    #[test]
    fn a_topic_folders_marker_reaches_its_subfolders_but_the_old_marker_name_does_not() {
        // A plain topic folder has no repository to climb to, so its marker
        // is what a subfolder (a PDFs folder, say) resolves through: a
        // session that moves into one must keep the topic's memory. Only the
        // current marker name is inherited; the old `.thor` names the folder
        // it sits in and nothing below it.
        let dir = tempdir();
        let child = dir.path().join("child").join("deeper");
        fs::create_dir_all(&child).unwrap();
        fs::write(dir.path().join(MARKER_FILE_NAME), "topic-folder\n").unwrap();
        assert_eq!(resolve_project(&child), Some("topic-folder".to_string()));
        assert_eq!(plain_root(&child), Some(dir.path().to_path_buf()));
        assert_eq!(checkout_top(&child), dir.path().to_path_buf());

        let old = tempdir();
        let old_child = old.path().join("child");
        fs::create_dir(&old_child).unwrap();
        fs::write(old.path().join(".thor"), "old-style\n").unwrap();
        assert_eq!(resolve_project(old.path()), Some("old-style".to_string()));
        assert_eq!(resolve_project(&old_child), None, "the old marker name is never inherited");
    }

    #[test]
    fn a_git_repository_below_a_topic_folder_is_its_own_project() {
        let dir = tempdir();
        fs::write(dir.path().join(MARKER_FILE_NAME), "topic-folder\n").unwrap();
        let repo = dir.path().join("inner-repo");
        fs::create_dir_all(repo.join(".git")).unwrap();
        assert_eq!(resolve_project(&repo), Some("inner-repo".to_string()), "git decides inside a repository");
        assert_eq!(plain_root(&repo), None);
    }

    #[test]
    fn a_blank_marker_falls_through_to_the_git_root() {
        let dir = tempdir();
        let repo = dir.path().join("repo-name");
        fs::create_dir(&repo).unwrap();
        fs::create_dir(repo.join(".git")).unwrap();
        fs::write(repo.join(MARKER_FILE_NAME), "   \n").unwrap();
        assert_eq!(resolve_project(&repo), Some("repo-name".to_string()), "a whitespace-only marker names nothing, so the git root still applies");
    }

    #[test]
    fn a_missing_starting_directory_resolves_to_no_project_not_an_error() {
        let dir = tempdir();
        let ghost = dir.path().join("does-not-exist");
        assert_eq!(resolve_project(&ghost), None);
    }

    #[test]
    fn only_the_first_line_of_the_marker_is_used() {
        let dir = tempdir();
        fs::write(dir.path().join(MARKER_FILE_NAME), "real-name\nextra ignored content\n").unwrap();
        assert_eq!(resolve_project(dir.path()), Some("real-name".to_string()));
    }

    // ------------------------------------- the two measured resolution bugs

    /// THE DEFECT THIS PREVENTS (2026-08-03). 2.0 invented the marker name
    /// `.thor-project` and nothing ever wrote one: the owner's machine holds
    /// five `.thor` markers and zero `.thor-project`. Resolution therefore
    /// always fell through to the git-root basename, which happened to equal
    /// the key for four of five repos - and for the fifth the directory has a
    /// space where the key has a hyphen. 110 fireable items, one of them
    /// irreversible, could never be selected in their own checkout.
    #[test]
    fn the_one_zero_marker_name_still_names_the_project() {
        let dir = tempdir();
        let repo = dir.path().join("acetone smoother");
        fs::create_dir_all(repo.join(".git")).unwrap();
        fs::write(repo.join(".thor"), "Acetone-Smoother
").unwrap();
        assert_eq!(
            resolve_project(&repo).as_deref(),
            Some("Acetone-Smoother"),
            "a legacy marker must win over the directory's own spelling"
        );
    }

    /// The new name still wins when both are present - a repo that adopts
    /// 2.0's own marker is not overruled by a leftover.
    #[test]
    fn the_two_zero_marker_name_wins_when_both_exist() {
        let dir = tempdir();
        let repo = dir.path().join("repo");
        fs::create_dir_all(repo.join(".git")).unwrap();
        fs::write(repo.join(".thor"), "old-key
").unwrap();
        fs::write(repo.join(MARKER_FILE_NAME), "new-key
").unwrap();
        assert_eq!(resolve_project(&repo).as_deref(), Some("new-key"));
    }

    /// THE DEFECT THIS PREVENTS (2026-08-03). In a linked worktree `.git` is
    /// a FILE pointing at `<main>/.git/worktrees/<slug>`. Resolution used to
    /// stop there and hand back the worktree's own basename - a generated
    /// slug - so every rule the repository owns was scoped out in the very
    /// checkout where the work was happening. Reproduced live before the fix:
    /// a session in one of the owner's worktrees resolved to a slug.
    #[test]
    fn a_linked_worktree_resolves_to_the_repository_it_belongs_to() {
        let dir = tempdir();
        let main = dir.path().join("acme-shop");
        fs::create_dir_all(main.join(".git").join("worktrees").join("hungry-wright-171e5d")).unwrap();

        let wt = dir.path().join("hungry-wright-171e5d");
        fs::create_dir_all(&wt).unwrap();
        let gitdir = main.join(".git").join("worktrees").join("hungry-wright-171e5d");
        fs::write(wt.join(".git"), format!("gitdir: {}
", gitdir.display())).unwrap();

        assert_eq!(git_root(&wt).as_deref(), Some(main.as_path()), "the main repo, not the worktree");
        assert_eq!(
            resolve_project(&wt).as_deref(),
            Some("acme-shop"),
            "a worktree is a place you work, never a project of its own"
        );
    }

    /// And it picks up the repository's marker, not just its basename.
    #[test]
    fn a_worktree_inherits_its_repositorys_marker() {
        let dir = tempdir();
        let main = dir.path().join("checkout-dir");
        fs::create_dir_all(main.join(".git").join("worktrees").join("slug")).unwrap();
        fs::write(main.join(".thor"), "Real-Project
").unwrap();

        let wt = dir.path().join("slug");
        fs::create_dir_all(&wt).unwrap();
        let gitdir = main.join(".git").join("worktrees").join("slug");
        fs::write(wt.join(".git"), format!("gitdir: {}
", gitdir.display())).unwrap();

        assert_eq!(resolve_project(&wt).as_deref(), Some("Real-Project"));
    }

    /// The worktree itself is still findable - the one thing `git_root`
    /// hides on purpose - and a directory of checkouts judges it as its main
    /// checkout. A real repository is never mistaken for a worktree.
    #[test]
    fn a_worktree_is_found_as_itself_and_judged_as_its_main_checkout() {
        let dir = tempdir();
        let main = dir.path().join("acme-shop");
        fs::create_dir_all(main.join(".git").join("worktrees").join("slug")).unwrap();
        let wt = dir.path().join("slug");
        fs::create_dir_all(wt.join("src")).unwrap();
        let gitdir = main.join(".git").join("worktrees").join("slug");
        fs::write(wt.join(".git"), format!("gitdir: {}\n", gitdir.display())).unwrap();

        assert_eq!(linked_worktree_root(&wt.join("src")).as_deref(), Some(wt.as_path()));
        assert_eq!(linked_worktree_root(&main), None, "a real repository is not a worktree");
        assert_eq!(checkout_for(&wt), main, "judged as its main checkout");
        assert_eq!(checkout_for(&main), main);
        let plain = dir.path().join("plain");
        fs::create_dir_all(&plain).unwrap();
        assert_eq!(checkout_for(&plain), plain, "a plain folder is never swapped for anything");
    }

    /// A `.git` file this does not recognise must never make things worse
    /// than the old behaviour.
    #[test]
    fn an_unrecognised_git_file_falls_back_to_the_directory_itself() {
        let dir = tempdir();
        let odd = dir.path().join("odd-checkout");
        fs::create_dir_all(&odd).unwrap();
        fs::write(odd.join(".git"), "something entirely different
").unwrap();
        assert_eq!(resolve_project(&odd).as_deref(), Some("odd-checkout"));
    }

    // ---------------------------------------- the registered working copy

    fn git(dir: &Path, args: &[&str]) {
        let out = std::process::Command::new("git").arg("-C").arg(dir).args(args).output().expect("git must be on PATH");
        assert!(out.status.success(), "git {args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
    }

    /// A real checkout at `dir` with one commit and a marker naming `key`.
    /// Its hooks point at an empty folder right after `git init`, so the
    /// machine's own shared git hooks never act on a throwaway repository.
    fn real_checkout(dir: &Path, key: &str, hooks: &Path) {
        fs::create_dir_all(dir).unwrap();
        fs::create_dir_all(hooks).unwrap();
        git(dir, &["init", "--quiet"]);
        git(dir, &["config", "core.hooksPath", &hooks.display().to_string()]);
        git(dir, &["config", "user.email", "fixture@example.invalid"]);
        git(dir, &["config", "user.name", "fixture"]);
        fs::write(dir.join(MARKER_FILE_NAME), format!("{key}\n")).unwrap();
        fs::write(dir.join("NOTES.md"), "intro only\n").unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "--quiet", "-m", "initial"]);
    }

    /// A registration written the way the installer writes one, straight into
    /// the log: THOR's own token is not for tests (see `model::store`'s
    /// `system_actor_is_built_only_in_the_named_modules`).
    fn register_raw(store: &mut thor_core::event_store::EventStore, key: &str, actor: &str, root: &str, at: &str) {
        let item = model::item::Item {
            id: model::store::project_opened_id(key),
            kind: model::item::Kind::Report,
            text: format!("Project {key}: a fixture record."),
            bindings: Vec::new(),
            severity: None,
            project: Some(key.to_string()),
            tags: vec!["project-record".to_string()],
            expires: None,
            key: None,
            falsifier: None,
            check: None,
        };
        let body = model::store::canonical_body(&item).unwrap();
        let body = format!(
            "{},\"root\":{},\"root_at\":{}}}",
            &body[..body.len() - 1],
            serde_json::to_string(root).unwrap(),
            serde_json::to_string(at).unwrap()
        );
        if store.get_events_by_entity(&item.id).unwrap().is_empty() {
            let kind = thor_core::event_store::EventKind::FactCreated;
            store.append_event(actor, actor, actor, kind, &item.id, None, &body).unwrap();
        } else {
            let kind = thor_core::event_store::EventKind::FactRevised;
            store.append_mutate_checked(actor, actor, actor, kind, &item.id, None, &body).unwrap();
        }
    }

    fn slashes(p: &Path) -> String {
        p.display().to_string().replace('\\', "/")
    }

    #[test]
    fn a_registered_checkout_that_is_a_real_top_resolving_to_its_project_is_ok() {
        let dir = tempdir();
        let hooks = dir.path().join("no-hooks");
        let beta = dir.path().join("beta");
        real_checkout(&beta, "beta", &hooks);
        assert_eq!(validate_checkout(&beta, "beta"), Ok(beta.clone()));

        // A folder with a space in its name, spelled with forward slashes the
        // way a registration stores it.
        let spaced = dir.path().join("acme shop");
        real_checkout(&spaced, "acme-shop", &hooks);
        let stored = std::path::PathBuf::from(slashes(&spaced));
        assert_eq!(validate_checkout(&stored, "acme-shop"), Ok(stored.clone()));
    }

    #[test]
    fn a_registered_folder_that_is_gone_is_named_gone() {
        let dir = tempdir();
        let ghost = dir.path().join("beta-moved-away");
        assert_eq!(validate_checkout(&ghost, "beta"), Err(RootProblem::Gone(ghost.clone())));
    }

    #[test]
    fn a_registered_folder_that_is_not_the_top_of_a_checkout_is_refused() {
        let dir = tempdir();
        let hooks = dir.path().join("no-hooks");
        let beta = dir.path().join("beta");
        real_checkout(&beta, "beta", &hooks);
        let sub = beta.join("sub");
        fs::create_dir_all(&sub).unwrap();
        fs::write(sub.join(MARKER_FILE_NAME), "beta\n").unwrap();
        assert_eq!(validate_checkout(&sub, "beta"), Err(RootProblem::NotCheckoutTop(sub.clone())));

        // A folder nobody marked is no project folder at all.
        let plain = dir.path().join("plain");
        fs::create_dir_all(&plain).unwrap();
        assert_eq!(validate_checkout(&plain, "beta"), Err(RootProblem::NotCheckoutTop(plain.clone())));
    }

    #[test]
    fn a_marked_topic_folder_is_a_usable_working_copy_and_its_subfolder_is_not() {
        let dir = tempdir();
        let topic = dir.path().join("Kitchen renovation");
        let sub = topic.join("PDFs");
        fs::create_dir_all(&sub).unwrap();
        fs::write(topic.join(MARKER_FILE_NAME), "Kitchen renovation\n").unwrap();
        assert_eq!(validate_checkout(&topic, "Kitchen renovation"), Ok(topic.clone()));
        assert_eq!(validate_checkout(&sub, "Kitchen renovation"), Err(RootProblem::NotCheckoutTop(sub.clone())));
        // A topic that now names another project is no longer this one's.
        assert_eq!(
            validate_checkout(&topic, "Bathroom"),
            Err(RootProblem::ResolvesTo(topic.clone(), Some("Kitchen renovation".to_string())))
        );
    }

    #[test]
    fn a_registered_linked_worktree_is_refused() {
        let dir = tempdir();
        let hooks = dir.path().join("no-hooks");
        let beta = dir.path().join("beta");
        real_checkout(&beta, "beta", &hooks);
        let wt = dir.path().join("beta-wt");
        git(&beta, &["worktree", "add", "--quiet", &wt.display().to_string(), "-b", "side"]);
        assert_eq!(validate_checkout(&wt, "beta"), Err(RootProblem::Worktree(wt.clone())));
        // And the top a session in that worktree works in is the worktree itself.
        let deep = wt.join("src");
        fs::create_dir_all(&deep).unwrap();
        assert_eq!(checkout_top(&deep), wt);
        assert_eq!(checkout_top(&beta.join("sub-not-there")), beta, "a folder inside the main checkout");
        let outside = dir.path().join("outside");
        fs::create_dir_all(&outside).unwrap();
        assert_eq!(checkout_top(&outside), outside, "no checkout at all: the folder itself");
    }

    #[test]
    fn a_registered_folder_whose_marker_now_names_another_project_is_refused() {
        let dir = tempdir();
        let hooks = dir.path().join("no-hooks");
        let beta = dir.path().join("beta");
        real_checkout(&beta, "beta", &hooks);
        fs::write(beta.join(MARKER_FILE_NAME), "other\n").unwrap();
        assert_eq!(
            validate_checkout(&beta, "beta"),
            Err(RootProblem::ResolvesTo(beta.clone(), Some("other".to_string())))
        );
    }

    #[test]
    fn registered_checkout_reads_the_installers_registration_only() {
        let dir = tempdir();
        let hooks = dir.path().join("no-hooks");
        let beta = dir.path().join("beta");
        real_checkout(&beta, "beta", &hooks);
        let copy = dir.path().join("beta-copy");
        real_checkout(&copy, "beta", &hooks);
        let mut store = thor_core::event_store::EventStore::in_memory().unwrap();

        assert_eq!(registered_checkout(&store, None), Err(RootProblem::GlobalRule));
        assert_eq!(registered_checkout(&store, Some("beta")), Err(RootProblem::NotRegistered));

        register_raw(&mut store, "beta", "mcp", &slashes(&copy), "2026-09-28T10:00:00Z");
        assert_eq!(registered_checkout(&store, Some("beta")), Err(RootProblem::NotRegistered), "not the installer's");

        register_raw(&mut store, "beta", "installer", &slashes(&beta), "2026-09-28T10:00:00Z");
        let got = registered_checkout(&store, Some("beta")).unwrap();
        assert_eq!(slashes(&got), slashes(&beta));

        register_raw(&mut store, "beta", "mcp", &slashes(&copy), "2026-09-28T11:00:00Z");
        assert_eq!(slashes(&registered_checkout(&store, Some("beta")).unwrap()), slashes(&beta), "a later forgery moves nothing");
    }

    #[test]
    fn the_registration_notice_shows_for_seven_days_and_names_a_move() {
        let mut store = thor_core::event_store::EventStore::in_memory().unwrap();
        let now = crate::time::unix_from_iso8601("2026-09-28T12:00:00Z").unwrap();
        assert_eq!(registration_notice(&store, Some("beta"), now), None, "nothing registered");

        register_raw(&mut store, "beta", "installer", "C:/tmp/beta", "2026-09-25T12:00:00Z");
        assert_eq!(
            registration_notice(&store, Some("beta"), now).as_deref(),
            Some("[THOR] the working copy of beta is registered at C:/tmp/beta since 2026-09-25")
        );
        assert_eq!(registration_notice(&store, None, now), None, "a session in no project");
        let eight_days_later = now + 5 * 86_400;
        assert_eq!(registration_notice(&store, Some("beta"), eight_days_later), None, "after seven days it stops");

        register_raw(&mut store, "beta", "installer", "C:/tmp/beta-moved", "2026-09-28T09:00:00Z");
        assert_eq!(
            registration_notice(&store, Some("beta"), now).as_deref(),
            Some(
                "[THOR] the working copy of beta is registered at C:/tmp/beta-moved since 2026-09-28 \
                 (moved from C:/tmp/beta, which no longer exists)"
            )
        );
    }

    // ------------------------------------------------ the session's project

    /// D4: a folder of no project keeps the project the session last worked
    /// in; a folder that resolves to a project always wins, another one
    /// included; a session that never worked in a project has none.
    #[test]
    fn a_session_keeps_its_project_in_a_folder_of_none_and_switches_to_another_one() {
        let dir = tempdir();
        let db = dir.path().join("thor.db");
        let alpha = dir.path().join("alpha");
        fs::create_dir_all(&alpha).unwrap();
        fs::write(alpha.join(MARKER_FILE_NAME), "alpha\n").unwrap();
        let beta = dir.path().join("beta");
        fs::create_dir_all(&beta).unwrap();
        fs::write(beta.join(MARKER_FILE_NAME), "beta\n").unwrap();
        let scratch = dir.path().join("scratch");
        fs::create_dir_all(&scratch).unwrap();

        assert_eq!(session_project(&db, "s1", Some(&scratch)), None, "no project anywhere yet");
        assert_eq!(session_project(&db, "s1", None), None, "no folder and no history");

        let now = 1_790_000_000;
        crate::usefulness::record_hook_event(&db, Some("alpha"), "s1", false, now, crate::usefulness::HookEventKind::Other);
        assert_eq!(session_project(&db, "s1", Some(&alpha)).as_deref(), Some("alpha"));
        assert_eq!(session_project(&db, "s1", Some(&scratch)).as_deref(), Some("alpha"), "drifted out: keeps alpha");
        assert_eq!(session_project(&db, "s1", None).as_deref(), Some("alpha"), "no folder at all: keeps alpha");
        assert_eq!(session_project(&db, "s1", Some(&beta)).as_deref(), Some("beta"), "another project's folder wins");
        assert_eq!(session_project(&db, "s2", Some(&scratch)), None, "another session never inherits alpha");
    }

    /// D4, the path half: after a drift into a folder of no project the
    /// guards resolve against the kept project's REGISTERED checkout; in
    /// every other case against the top of the folder's own checkout.
    #[test]
    fn the_guard_root_after_a_drift_is_the_registered_checkout_and_the_folders_own_top_otherwise() {
        let dir = tempdir();
        let hooks = dir.path().join("no-hooks");
        let beta = dir.path().join("beta");
        real_checkout(&beta, "beta", &hooks);
        let sub = beta.join("sub");
        fs::create_dir_all(&sub).unwrap();
        let gamma = dir.path().join("gamma");
        real_checkout(&gamma, "gamma", &hooks);
        let scratch = dir.path().join("scratch");
        fs::create_dir_all(&scratch).unwrap();
        let mut store = thor_core::event_store::EventStore::in_memory().unwrap();

        // No registration: nothing but the folder can be trusted.
        assert_eq!(guard_root(&store, Some(&scratch), None, Some("beta")), Some(scratch.clone()), "not registered");

        // A registration by anyone but the installer moves nothing.
        register_raw(&mut store, "beta", "mcp", &slashes(&gamma), "2026-09-28T09:00:00Z");
        assert_eq!(guard_root(&store, Some(&scratch), None, Some("beta")), Some(scratch.clone()), "not the installer's");

        register_raw(&mut store, "beta", "installer", &slashes(&beta), "2026-09-28T10:00:00Z");
        let drifted = guard_root(&store, Some(&scratch), None, Some("beta")).unwrap();
        assert_eq!(slashes(&drifted), slashes(&beta), "drifted out of beta: beta's registered checkout");
        let no_folder = guard_root(&store, None, None, Some("beta")).unwrap();
        assert_eq!(slashes(&no_folder), slashes(&beta), "no folder at all: still beta's checkout");

        // A folder that resolves to a project is judged at the top of its own
        // checkout, whatever is registered - a linked worktree of the
        // registered project included: its files are where it works.
        assert_eq!(guard_root(&store, Some(&sub), Some("beta"), Some("beta")), Some(beta.clone()), "a subfolder of beta");
        assert_eq!(guard_root(&store, Some(&gamma), Some("gamma"), Some("gamma")), Some(gamma.clone()), "another project");
        let wt = dir.path().join("beta-wt");
        git(&beta, &["worktree", "add", "--quiet", &wt.display().to_string(), "-b", "side"]);
        let deep = wt.join("src");
        fs::create_dir_all(&deep).unwrap();
        assert_eq!(guard_root(&store, Some(&deep), Some("beta"), Some("beta")), Some(wt.clone()), "beta's linked worktree");

        // A session that never had a project: the folder itself.
        assert_eq!(guard_root(&store, Some(&scratch), None, None), Some(scratch.clone()), "no project at all");

        // A registration that no longer holds (the folder now names another
        // project) is not trusted: the folder again.
        fs::write(beta.join(MARKER_FILE_NAME), "other\n").unwrap();
        assert_eq!(guard_root(&store, Some(&scratch), None, Some("beta")), Some(scratch.clone()), "registration invalid");
    }

    /// The inventory that makes the NEXT one of these visible instead of
    /// silent: a key no checkout resolves to is reported, with its weight.
    #[test]
    fn orphaned_keys_are_reported_with_how_many_items_hold_them() {
        let resolvable: std::collections::BTreeSet<String> =
            ["Sensor-Board".to_string(), "acme-shop".to_string()].into_iter().collect();
        let held = ["acme-shop", "Acetone-Smoother", "Acetone-Smoother", "Sensor-Board"];
        let orphans = orphaned_project_keys(held, &resolvable);
        assert_eq!(orphans.len(), 1);
        assert_eq!(orphans.get("Acetone-Smoother"), Some(&2));
    }
}
