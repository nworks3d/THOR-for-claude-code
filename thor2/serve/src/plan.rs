//! THE PLAN THE USER PAYS FOR, as far as THOR can tell, and what it changes.
//!
//! WHY THOR CARES. A small plan has small limits, and THOR's own upkeep (the
//! end-of-session evaluation) draws on the same limits as the user's work. On
//! Max the evaluation runs once a week and daily while rot is found; on any
//! smaller plan it runs strictly once a week (`usefulness::EvalCadence`).
//!
//! HOW IT IS KNOWN. Claude Code writes `organizationRateLimitTier` into the
//! user's `~/.claude.json` (on a Max 20x plan: `default_claude_max_20x`). A
//! value containing "max" means Max; any other value, or no such key, means a
//! small plan. The field is UNDOCUMENTED and Claude Code may rename or move it,
//! so everything here fails soft: an unreadable file or a missing key is a
//! small plan, never an error, and the install asks the user to confirm it as
//! one numbered question (`ops::questionnaire`), so a rename costs a question,
//! not a wrong guess nobody sees.
//!
//! WHERE IT LIVES. In the mode sidecar beside the store (`thor-mode.json`),
//! next to the bug-nudge mode, as `"plan"` (`max` | `small`) and
//! `"plan_source"` (`detected` | `confirmed` | `asked`). A plan the user
//! confirmed or told is never overwritten by a later detection; only a plan
//! that was merely `detected` follows the file.
//!
//! Best effort like every sidecar: a failed write loses a refinement, never a
//! turn.

use crate::thor_bugs::MODE_FILE;
use serde_json::Value;
use std::path::{Path, PathBuf};

/// The key Claude Code writes the plan tier under.
pub const TIER_KEY: &str = "organizationRateLimitTier";

/// How deep below the top of `~/.claude.json` the tier is looked for. It sits
/// one level down today (`oauthAccount`); a little slack survives a regrouping.
const SEARCH_DEPTH: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Plan {
    Max,
    Small,
}

/// Who said so.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanSource {
    /// THOR read it from Claude Code's file and nobody has confirmed it.
    Detected,
    /// THOR read it and the user said it is right.
    Confirmed,
    /// THOR could not read it; the user told it.
    Asked,
}

impl Plan {
    pub fn word(self) -> &'static str {
        match self {
            Plan::Max => "max",
            Plan::Small => "small",
        }
    }

    pub fn from_word(word: &str) -> Option<Plan> {
        match word.trim().to_ascii_lowercase().as_str() {
            "max" => Some(Plan::Max),
            "small" | "pro" => Some(Plan::Small),
            _ => None,
        }
    }
}

impl PlanSource {
    pub fn word(self) -> &'static str {
        match self {
            PlanSource::Detected => "detected",
            PlanSource::Confirmed => "confirmed",
            PlanSource::Asked => "asked",
        }
    }

    fn from_word(word: &str) -> Option<PlanSource> {
        match word {
            "detected" => Some(PlanSource::Detected),
            "confirmed" => Some(PlanSource::Confirmed),
            "asked" => Some(PlanSource::Asked),
            _ => None,
        }
    }
}

/// What reading Claude Code's file found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Detection {
    pub plan: Plan,
    /// Whether the tier key was there at all. Without it the plan is a guess
    /// (small) and the user is asked outright.
    pub tier_found: bool,
}

/// The plan a tier value stands for: "max" anywhere in it (any case) is Max.
pub fn plan_of_tier(tier: &str) -> Plan {
    match tier.to_ascii_lowercase().contains("max") {
        true => Plan::Max,
        false => Plan::Small,
    }
}

fn find_tier(value: &Value, depth: usize) -> Option<String> {
    let object = value.as_object()?;
    if let Some(tier) = object.get(TIER_KEY).and_then(Value::as_str) {
        return Some(tier.to_string());
    }
    if depth == 0 {
        return None;
    }
    // The documented neighbour first, then anything else.
    if let Some(found) = object.get("oauthAccount").and_then(|v| find_tier(v, depth - 1)) {
        return Some(found);
    }
    object.values().filter(|v| v.is_object()).find_map(|v| find_tier(v, depth - 1))
}

/// Read the plan from the `~/.claude.json`-shaped file at `path`. Fails soft:
/// a file that is missing, unreadable or not JSON reads as a small plan with
/// no tier found.
pub fn detect_in(path: &Path) -> Detection {
    let none = Detection { plan: Plan::Small, tier_found: false };
    let Ok(text) = std::fs::read_to_string(path) else { return none };
    // The file is large and stateful; most of it is not ours to parse.
    if !text.contains(TIER_KEY) {
        return none;
    }
    let Ok(value) = serde_json::from_str::<Value>(&text) else { return none };
    match find_tier(&value, SEARCH_DEPTH) {
        Some(tier) => Detection { plan: plan_of_tier(&tier), tier_found: true },
        None => none,
    }
}

/// Claude Code's per-user state file, `~/.claude.json`, from the environment.
pub fn default_claude_json() -> Option<PathBuf> {
    let home = std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME")).ok()?;
    Some(PathBuf::from(home).join(".claude.json"))
}

fn mode_file(db: &Path) -> PathBuf {
    db.parent().unwrap_or_else(|| Path::new(".")).join(MODE_FILE)
}

fn read_object(db: &Path) -> serde_json::Map<String, Value> {
    std::fs::read_to_string(mode_file(db))
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default()
}

/// The plan and who said so, `None` when nothing was ever written.
pub fn read(db: &Path) -> Option<(Plan, PlanSource)> {
    let object = read_object(db);
    let plan = Plan::from_word(object.get("plan")?.as_str()?)?;
    let source = object.get("plan_source").and_then(Value::as_str).and_then(PlanSource::from_word).unwrap_or(PlanSource::Detected);
    Some((plan, source))
}

/// The plan THOR runs by: what is on record, a small plan when nothing is.
/// Cheap is the safe default.
pub fn current(db: &Path) -> Plan {
    read(db).map(|(plan, _)| plan).unwrap_or(Plan::Small)
}

/// Write the plan into the mode sidecar, keeping every other key (the bug
/// nudge mode lives there too).
pub fn write(db: &Path, plan: Plan, source: PlanSource) -> std::io::Result<()> {
    let mut object = read_object(db);
    object.insert("plan".to_string(), Value::String(plan.word().to_string()));
    object.insert("plan_source".to_string(), Value::String(source.word().to_string()));
    std::fs::write(mode_file(db), Value::Object(object).to_string())
}

/// Whether the weekly check is on: yes unless the user switched it off
/// (`install --set-weekly-check off`, first-session question 4). Only the
/// automatic ask is switched off; the user can still run `/thor-eval`.
pub fn weekly_check_on(db: &Path) -> bool {
    read_object(db).get("weekly_check").and_then(Value::as_str) != Some("off")
}

/// Switch the weekly check on or off, keeping every other key of the sidecar.
pub fn set_weekly_check(db: &Path, on: bool) -> std::io::Result<()> {
    let mut object = read_object(db);
    object.insert("weekly_check".to_string(), Value::String(if on { "on" } else { "off" }.to_string()));
    std::fs::write(mode_file(db), Value::Object(object).to_string())
}

/// Install: settle the plan from `claude_json`. A plan the user already
/// confirmed or told is kept as it is; anything else is (re)detected. Returns
/// what is on record afterwards and what the file said.
pub fn settle(db: &Path, claude_json: Option<&Path>) -> std::io::Result<(Plan, PlanSource, Detection)> {
    let found = claude_json.map(detect_in).unwrap_or(Detection { plan: Plan::Small, tier_found: false });
    if let Some((plan, source)) = read(db) {
        if source != PlanSource::Detected {
            return Ok((plan, source, found));
        }
    }
    write(db, found.plan, PlanSource::Detected)?;
    Ok((found.plan, PlanSource::Detected, found))
}

/// Session start: follow the file for a plan that was only ever detected (an
/// upgrade or a downgrade). Never touches a confirmed or told plan, and never
/// creates a record where install wrote none. Returns whether it changed.
pub fn refresh_detected(db: &Path, claude_json: &Path) -> bool {
    let Some((on_record, PlanSource::Detected)) = read(db) else { return false };
    let found = detect_in(claude_json);
    if !found.tier_found || found.plan == on_record {
        return false;
    }
    write(db, found.plan, PlanSource::Detected).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claude_json(dir: &Path, body: &str) -> PathBuf {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        std::hash::Hash::hash(body, &mut hasher);
        let path = dir.join(format!("claude-{:x}.json", std::hash::Hasher::finish(&hasher)));
        std::fs::write(&path, body).unwrap();
        path
    }

    fn store(dir: &Path) -> PathBuf {
        let db = dir.join("thor.db");
        std::fs::write(&db, b"").unwrap();
        db
    }

    #[test]
    fn a_tier_containing_max_is_max_and_anything_else_is_small() {
        assert_eq!(plan_of_tier("default_claude_max_20x"), Plan::Max);
        assert_eq!(plan_of_tier("default_claude_max_5x"), Plan::Max);
        assert_eq!(plan_of_tier("DEFAULT_CLAUDE_MAX"), Plan::Max);
        assert_eq!(plan_of_tier("default_claude_pro"), Plan::Small);
        assert_eq!(plan_of_tier("default"), Plan::Small);
        assert_eq!(plan_of_tier(""), Plan::Small);
    }

    #[test]
    fn the_tier_is_found_where_claude_code_keeps_it_and_one_level_off() {
        let dir = tempfile::tempdir().unwrap();
        let nested = claude_json(dir.path(), r#"{"oauthAccount":{"organizationRateLimitTier":"default_claude_max_20x"}}"#);
        assert_eq!(detect_in(&nested), Detection { plan: Plan::Max, tier_found: true });
        let top = claude_json(dir.path(), r#"{"organizationRateLimitTier":"default_claude_pro"}"#);
        assert_eq!(detect_in(&top), Detection { plan: Plan::Small, tier_found: true });
        let moved = claude_json(dir.path(), r#"{"account":{"organizationRateLimitTier":"default_claude_max_5x"}}"#);
        assert_eq!(detect_in(&moved), Detection { plan: Plan::Max, tier_found: true });
    }

    #[test]
    fn detection_fails_soft_on_a_missing_key_a_missing_file_and_garbage() {
        let dir = tempfile::tempdir().unwrap();
        let no_key = claude_json(dir.path(), r#"{"oauthAccount":{"emailAddress":"x"}}"#);
        assert_eq!(detect_in(&no_key), Detection { plan: Plan::Small, tier_found: false });
        assert_eq!(detect_in(&dir.path().join("nothing.json")), Detection { plan: Plan::Small, tier_found: false });
        let garbage = claude_json(dir.path(), "organizationRateLimitTier is not json");
        assert_eq!(detect_in(&garbage), Detection { plan: Plan::Small, tier_found: false });
        let wrong_type = claude_json(dir.path(), r#"{"organizationRateLimitTier": 5}"#);
        assert_eq!(detect_in(&wrong_type), Detection { plan: Plan::Small, tier_found: false });
    }

    #[test]
    fn the_plan_is_kept_beside_the_bug_mode_without_touching_it() {
        let dir = tempfile::tempdir().unwrap();
        let db = store(dir.path());
        crate::thor_bugs::write_mode(&db, crate::bug_report::Mode::Test).unwrap();
        write(&db, Plan::Max, PlanSource::Confirmed).unwrap();
        assert_eq!(read(&db), Some((Plan::Max, PlanSource::Confirmed)));
        assert_eq!(crate::thor_bugs::read_mode(&db), Some(crate::bug_report::Mode::Test), "the bug mode survives");
        crate::thor_bugs::write_mode(&db, crate::bug_report::Mode::Release).unwrap();
        assert_eq!(read(&db), Some((Plan::Max, PlanSource::Confirmed)), "writing the bug mode keeps the plan");
    }

    #[test]
    fn the_weekly_check_is_on_until_the_user_switches_it_off() {
        let dir = tempfile::tempdir().unwrap();
        let db = store(dir.path());
        assert!(weekly_check_on(&db), "on by default, with no sidecar at all");
        write(&db, Plan::Max, PlanSource::Confirmed).unwrap();
        assert!(weekly_check_on(&db));
        set_weekly_check(&db, false).unwrap();
        assert!(!weekly_check_on(&db));
        assert_eq!(read(&db), Some((Plan::Max, PlanSource::Confirmed)), "the plan survives");
        crate::thor_bugs::write_mode(&db, crate::bug_report::Mode::Release).unwrap();
        assert!(!weekly_check_on(&db), "writing the bug mode keeps the switch");
        set_weekly_check(&db, true).unwrap();
        assert!(weekly_check_on(&db));
    }

    #[test]
    fn with_nothing_on_record_the_plan_is_small() {
        let dir = tempfile::tempdir().unwrap();
        let db = store(dir.path());
        assert_eq!(read(&db), None);
        assert_eq!(current(&db), Plan::Small);
    }

    #[test]
    fn install_records_the_detected_plan_and_keeps_one_the_user_confirmed() {
        let dir = tempfile::tempdir().unwrap();
        let db = store(dir.path());
        let max = claude_json(dir.path(), r#"{"oauthAccount":{"organizationRateLimitTier":"default_claude_max_20x"}}"#);
        let (plan, source, found) = settle(&db, Some(&max)).unwrap();
        assert_eq!((plan, source, found.tier_found), (Plan::Max, PlanSource::Detected, true));
        write(&db, Plan::Small, PlanSource::Confirmed).unwrap();
        let (plan, source, _) = settle(&db, Some(&max)).unwrap();
        assert_eq!((plan, source), (Plan::Small, PlanSource::Confirmed), "a confirmed plan is not overwritten");
    }

    #[test]
    fn session_start_follows_a_detected_plan_but_never_a_confirmed_or_told_one() {
        let dir = tempfile::tempdir().unwrap();
        let db = store(dir.path());
        let pro = claude_json(dir.path(), r#"{"oauthAccount":{"organizationRateLimitTier":"default_claude_pro"}}"#);
        let max = claude_json(dir.path(), r#"{"oauthAccount":{"organizationRateLimitTier":"default_claude_max_20x"}}"#);
        settle(&db, Some(&pro)).unwrap();
        assert!(refresh_detected(&db, &max), "an upgrade is followed");
        assert_eq!(read(&db), Some((Plan::Max, PlanSource::Detected)));
        assert!(!refresh_detected(&db, &max), "nothing changed, nothing written");
        write(&db, Plan::Max, PlanSource::Confirmed).unwrap();
        assert!(!refresh_detected(&db, &pro), "a confirmed plan stays");
        assert_eq!(read(&db), Some((Plan::Max, PlanSource::Confirmed)));
    }

    #[test]
    fn a_file_that_lost_the_key_does_not_demote_a_detected_plan() {
        let dir = tempfile::tempdir().unwrap();
        let db = store(dir.path());
        let max = claude_json(dir.path(), r#"{"oauthAccount":{"organizationRateLimitTier":"default_claude_max_20x"}}"#);
        settle(&db, Some(&max)).unwrap();
        let renamed = claude_json(dir.path(), r#"{"oauthAccount":{"tier":"x"}}"#);
        assert!(!refresh_detected(&db, &renamed));
        assert_eq!(current(&db), Plan::Max);
    }

    #[test]
    fn session_start_creates_nothing_where_install_wrote_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let db = store(dir.path());
        let max = claude_json(dir.path(), r#"{"oauthAccount":{"organizationRateLimitTier":"default_claude_max_20x"}}"#);
        assert!(!refresh_detected(&db, &max));
        assert_eq!(read(&db), None);
    }
}
