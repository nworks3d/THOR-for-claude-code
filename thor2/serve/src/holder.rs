//! ONE HOLDER PER PROJECT (step 26).
//!
//! The owner's decision, 2026-10-01: never two sessions in the same
//! repository owe judgement at once. Field case that day, in one business project: two
//! sessions in one checkout were both held for the same project's judgement
//! debt and evaluation, and both did the same work - one of them voided the
//! other's evaluation, the other judged items the first had just judged.
//!
//! Step 24 already ran one EVALUATION per project. This extends the same rule
//! to every Stop hold that is DEBT about the project rather than about the
//! session's own writes: the judgement debt ("N item(s) have fired
//! repeatedly"), the rule upkeep (teeth) question and the evaluation ask. Per
//! project, the HOLDER is:
//!
//! 1. the session that runs the project's open evaluation (step 24's rule,
//!    read from the log), else
//! 2. the session currently held for the project's debt - a lease in a small
//!    sidecar beside the store, written when a Stop holds a session for the
//!    project's debt and released at a Stop of that session that holds it for
//!    nothing, else
//! 3. the first session asked: it takes the lease.
//!
//! Another session working in the same project is not held for any of those
//! while the holder showed activity on the log in the last
//! `model::store::EVALUATION_IDLE_MINUTES` (`usefulness::session_idle_minutes`,
//! the log's own record, never a file time); it gets one non-blocking line per
//! turn naming the holder. Once the holder has been quiet longer, the next
//! session asked takes the lease over. Nothing is voided: a verdict is global
//! per item anyway, so whoever gives it settles it for everyone.
//!
//! What stays per session: every hold about the session's OWN doing - a fact
//! it stored onto a full place, an unattested write of its own, setup, an
//! owner decision to capture, the Response Guard. Those are never the
//! holder's to answer.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use thor_core::event_store::EventStore;

/// The sidecar beside the store that keeps one lease per project.
pub const HOLDERS_FILE: &str = "debt-holders.json";

/// One project's lease: the session held for its debt, since when, and when
/// it was last held.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Lease {
    pub session: String,
    pub since_unix: i64,
    pub held_unix: i64,
}

/// Who holds a project's debt, as one session about to be held sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Holder {
    /// This session holds it (or nobody else does): it may be held.
    Me,
    /// Another session holds it and was active within the idle window: this
    /// session is not held for the project's debt.
    Other {
        session: String,
        /// Minutes since that session's last activity on the log.
        idle_minutes: i64,
        /// Why it is the holder, in words: "runs the project's evaluation" or
        /// "is held for the project's debt".
        why: &'static str,
    },
}

fn path(db: &Path) -> PathBuf {
    db.parent().unwrap_or_else(|| Path::new(".")).join(HOLDERS_FILE)
}

/// Every lease on file, by project. An unreadable sidecar reads as none:
/// the next ask then simply takes the lease.
pub fn leases(db: &Path) -> BTreeMap<String, Lease> {
    std::fs::read_to_string(path(db)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

fn write(db: &Path, all: &BTreeMap<String, Lease>) {
    if let Ok(text) = serde_json::to_string(all) {
        let _ = crate::store_dir::write(&path(db), text);
    }
}

/// Who holds `project`'s debt for `session` at `now_unix` - see this
/// module's doc comment for the order.
pub fn holder_of(store: &EventStore, db: &Path, project: &str, session: &str, now_unix: i64) -> Holder {
    if model::store::open_evaluation_start_at(store, session, project, now_unix).is_some() {
        return Holder::Me;
    }
    if let Some((start, idle_minutes)) = crate::usefulness::evaluation_running_elsewhere(store, project, session, now_unix) {
        return Holder::Other { session: start.frozen.session, idle_minutes, why: "runs the project's evaluation" };
    }
    if let Some(lease) = leases(db).get(project).filter(|l| l.session != session) {
        let idle = crate::usefulness::session_idle_minutes(store, &lease.session, now_unix, model::store::EVALUATION_IDLE_MINUTES);
        if let Some(idle_minutes) = idle {
            return Holder::Other { session: lease.session.clone(), idle_minutes, why: "is held for the project's debt" };
        }
    }
    Holder::Me
}

/// `session` is held for `project`'s debt now: it holds the lease (taken
/// over from a quiet holder when need be). Best effort, like every sidecar.
pub fn claim(db: &Path, project: &str, session: &str, now_unix: i64) {
    let mut all = leases(db);
    let since = all.get(project).filter(|l| l.session == session).map_or(now_unix, |l| l.since_unix);
    all.insert(project.to_string(), Lease { session: session.to_string(), since_unix: since, held_unix: now_unix });
    write(db, &all);
}

/// A Stop of `session` that held it for no project's debt: every lease it
/// holds is released, so the next session asked holds. Writes only when it
/// held one.
pub fn release(db: &Path, session: &str) {
    let mut all = leases(db);
    let before = all.len();
    all.retain(|_, lease| lease.session != session);
    if all.len() != before {
        write(db, &all);
    }
}

/// The one non-blocking line a session that is not held gets, naming the
/// holder.
pub fn holder_line(project: &str, holder_session: &str, idle_minutes: i64, why: &str) -> String {
    format!(
        "[THOR] Session {holder_session} {why} of '{project}' ({idle_minutes} minute(s) since its last activity). One \
         session at a time answers a project's debt, so this session is not held for it. If that session stays quiet \
         for {} minutes, the next ask here takes it over.",
        model::store::EVALUATION_IDLE_MINUTES
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lease_is_claimed_kept_and_released() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        assert!(leases(&db).is_empty());
        claim(&db, "alpha", "s1", 100);
        claim(&db, "alpha", "s1", 200);
        let lease = leases(&db).get("alpha").cloned().unwrap();
        assert_eq!(lease, Lease { session: "s1".to_string(), since_unix: 100, held_unix: 200 });
        claim(&db, "beta", "s2", 300);
        release(&db, "s1");
        let all = leases(&db);
        assert!(!all.contains_key("alpha"));
        assert_eq!(all.get("beta").map(|l| l.session.as_str()), Some("s2"));
    }

    #[test]
    fn another_sessions_lease_holds_only_while_it_is_active() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        let mut store = EventStore::new(&db).unwrap();
        let now = crate::time::now_unix();
        // s1 was active a minute ago.
        crate::deliver::record_delivery(
            &mut store,
            "s1",
            "s1",
            crate::usefulness::ASSISTANT_DELIVERY_ACTOR,
            &crate::time::iso8601_from_unix(now - 60),
            &["x".to_string()],
        );
        claim(&db, "alpha", "s1", now);
        assert!(matches!(holder_of(&store, &db, "alpha", "s2", now), Holder::Other { ref session, .. } if session == "s1"));
        assert_eq!(holder_of(&store, &db, "alpha", "s1", now), Holder::Me, "the holder itself is never kept out");
        assert_eq!(holder_of(&store, &db, "beta", "s2", now), Holder::Me, "a lease is per project");
        // An hour later s1 has been quiet longer than the idle window.
        let later = now + 3600;
        assert_eq!(holder_of(&store, &db, "alpha", "s2", later), Holder::Me);
    }
}
