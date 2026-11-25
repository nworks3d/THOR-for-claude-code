//! A working folder of 32,000 characters used to cost tens of seconds per hook (one walk up
//! thousands of parents, several times). Such a folder is now no folder, at once.

use std::path::Path;
use std::time::{Duration, Instant};

#[test]
fn a_cwd_of_32000_characters_is_answered_at_once_with_nothing() {
    let huge = format!("C:/{}", "a/".repeat(16_000));
    let began = Instant::now();
    assert!(model::checkout::git_root(Path::new(&huge)).is_none());
    assert!(model::checkout::plain_root(Path::new(&huge)).is_none());
    assert!(model::checkout::linked_worktree_root(Path::new(&huge)).is_none());
    assert!(began.elapsed() < Duration::from_secs(2), "took {:?}", began.elapsed());
}
