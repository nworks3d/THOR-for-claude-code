//! Which machine this is, so one project can have one working copy PER
//! machine (the owner's PC and laptop keep the same project at different
//! absolute paths). Opt-in: a registration carries a machine id only when
//! `install --register-root --per-machine` wrote it; every other registration
//! has none and counts as the main machine's.
//!
//! The id is the computer name plus the user name, lowercased: stable across
//! restarts, readable, and no secret in it. `THOR_MACHINE_ID` overrides it
//! (tests, or two accounts that must count as one machine).

/// The id of the machine this process runs on.
pub fn current_machine_id() -> String {
    if let Ok(over) = std::env::var("THOR_MACHINE_ID") {
        let over = over.trim();
        if !over.is_empty() {
            return over.to_string();
        }
    }
    let host = std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .ok()
        .or_else(|| std::fs::read_to_string("/etc/hostname").ok())
        .map(|h| h.trim().to_string())
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "unknown-host".to_string());
    let user = std::env::var("USERNAME").or_else(|_| std::env::var("USER")).unwrap_or_else(|_| "unknown-user".to_string());
    format!("{host}/{user}").to_lowercase()
}
