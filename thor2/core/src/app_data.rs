//! Finding THOR's own files from a program that runs OUTSIDE the Claude
//! desktop app on Windows.
//!
//! The desktop app is a packaged (MSIX) app. A program started inside it sees
//! a merged view of the per-user local app data folder: a folder it creates at
//! `<local>\thor2` physically lives at
//! `<local>\Packages\<package>\LocalCache\Local\thor2`, and both paths show the
//! same file. A program started outside the app (a scheduled task, a normal
//! terminal, ssh) sees only the physical path; the short one is "not found".
//!
//! `resolve_in` maps a path that does not exist as given to that physical twin
//! when exactly one package folder holds it. It never creates anything and
//! leaves every path that exists, or has no single twin, exactly as it was.
//! The `_in` functions take the base folder as an argument so they can be
//! tested anywhere; the plain ones read `LOCALAPPDATA` and are a no-op off
//! Windows.

use std::path::{Component, Path, PathBuf};

/// The per-user local app data folder, on Windows only.
pub fn local_app_data() -> Option<PathBuf> {
    if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").filter(|v| !v.is_empty()).map(PathBuf::from)
    } else {
        None
    }
}

fn same_component(a: Component<'_>, b: Component<'_>) -> bool {
    a.as_os_str().to_string_lossy().eq_ignore_ascii_case(&b.as_os_str().to_string_lossy())
}

/// The part of `path` below `base`, compared the way Windows compares names
/// (case folded). `None` when `path` is not under `base` or is `base` itself.
fn relative_to(path: &Path, base: &Path) -> Option<PathBuf> {
    let mut path_parts = path.components();
    for base_part in base.components() {
        if !same_component(path_parts.next()?, base_part) {
            return None;
        }
    }
    let rest = path_parts.as_path();
    if rest.as_os_str().is_empty() {
        None
    } else {
        Some(rest.to_path_buf())
    }
}

/// `path`, or its physical twin under `<base>\Packages\*\LocalCache\Local\`
/// when `path` does not exist as given and exactly one package folder has the
/// same relative path. Anything else comes back unchanged.
pub fn resolve_in(path: &Path, base: &Path) -> PathBuf {
    if path.exists() {
        return path.to_path_buf();
    }
    let Some(rel) = relative_to(path, base) else { return path.to_path_buf() };
    let Ok(packages) = std::fs::read_dir(base.join("Packages")) else { return path.to_path_buf() };
    let mut found = packages
        .filter_map(Result::ok)
        .map(|package| package.path().join("LocalCache").join("Local").join(&rel))
        .filter(|twin| twin.exists());
    match (found.next(), found.next()) {
        (Some(only), None) => only,
        _ => path.to_path_buf(),
    }
}

/// The reverse spelling: for a physical path under
/// `<base>\Packages\<package>\LocalCache\Local\`, the short path the app shows
/// for the same file. `None` for any other path.
pub fn logical_in(path: &Path, base: &Path) -> Option<PathBuf> {
    let rel = relative_to(path, &base.join("Packages"))?;
    let mut parts = rel.components();
    parts.next()?;
    let inside = relative_to(parts.as_path(), &Path::new("LocalCache").join("Local"))?;
    Some(base.join(inside))
}

/// `resolve_in` against the real local app data folder; unchanged off Windows.
pub fn resolve(path: &Path) -> PathBuf {
    match local_app_data() {
        Some(base) => resolve_in(path, &base),
        None => path.to_path_buf(),
    }
}

/// Every spelling that names the same file as `path`: the path itself, its
/// physical twin when it has one and its short form when it is a physical path.
pub fn spellings_in(path: &Path, base: &Path) -> Vec<PathBuf> {
    let mut all = vec![path.to_path_buf()];
    for other in [Some(resolve_in(path, base)), logical_in(path, base)].into_iter().flatten() {
        if !all.contains(&other) {
            all.push(other);
        }
    }
    all
}

/// `spellings_in` against the real local app data folder.
pub fn spellings(path: &Path) -> Vec<PathBuf> {
    match local_app_data() {
        Some(base) => spellings_in(path, &base),
        None => vec![path.to_path_buf()],
    }
}

/// For clap's `value_parser`: a `--db` value, resolved with `resolve`.
pub fn parse_db_path(value: &str) -> Result<PathBuf, std::convert::Infallible> {
    Ok(resolve(Path::new(value)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn twin_file(base: &Path, package: &str, rel: &str) -> PathBuf {
        let file = base.join("Packages").join(package).join("LocalCache").join("Local").join(rel);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, b"x").unwrap();
        file
    }

    #[test]
    fn a_missing_path_resolves_to_the_one_twin() {
        let dir = tempfile::tempdir().unwrap();
        let twin = twin_file(dir.path(), "Claude_abc", "thor2/thor.db");
        let given = dir.path().join("thor2").join("thor.db");
        assert_eq!(resolve_in(&given, dir.path()), twin);
        assert!(!given.exists(), "resolving must never create the short path");
    }

    #[test]
    fn a_path_that_exists_is_never_moved() {
        let dir = tempfile::tempdir().unwrap();
        twin_file(dir.path(), "Claude_abc", "thor2/thor.db");
        let real = dir.path().join("thor2").join("thor.db");
        fs::create_dir_all(real.parent().unwrap()).unwrap();
        fs::write(&real, b"y").unwrap();
        assert_eq!(resolve_in(&real, dir.path()), real);
    }

    #[test]
    fn zero_or_several_twins_leave_the_path_alone() {
        let dir = tempfile::tempdir().unwrap();
        let given = dir.path().join("thor2").join("thor.db");
        assert_eq!(resolve_in(&given, dir.path()), given, "no Packages folder at all");
        twin_file(dir.path(), "Claude_abc", "other/file");
        assert_eq!(resolve_in(&given, dir.path()), given, "a package without the file");
        twin_file(dir.path(), "Claude_abc", "thor2/thor.db");
        twin_file(dir.path(), "Claude_def", "thor2/thor.db");
        assert_eq!(resolve_in(&given, dir.path()), given, "two packages have it");
    }

    #[test]
    fn a_path_outside_the_base_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        twin_file(dir.path(), "Claude_abc", "thor2/thor.db");
        let elsewhere = tempfile::tempdir().unwrap();
        let given = elsewhere.path().join("thor2").join("thor.db");
        assert_eq!(resolve_in(&given, dir.path()), given);
        assert_eq!(resolve_in(dir.path(), dir.path()), dir.path());
    }

    #[test]
    fn the_short_form_of_a_physical_path_is_found() {
        let dir = tempfile::tempdir().unwrap();
        let twin = twin_file(dir.path(), "Claude_abc", "thor2/thor.db");
        let given = dir.path().join("thor2").join("thor.db");
        assert_eq!(logical_in(&twin, dir.path()), Some(given.clone()));
        assert_eq!(logical_in(&given, dir.path()), None);
        assert_eq!(spellings_in(&given, dir.path()), vec![given.clone(), twin.clone()]);
        assert_eq!(spellings_in(&twin, dir.path()), vec![twin, given]);
    }
}
