//! The health check: one plain-language line per component, no jargon, no
//! silent gaps. Every reader here degrades to an honest "not configured" or
//! "unreachable" line instead of an error that stops the other lines from
//! printing - a health check that dies on the first broken thing is worse
//! than no health check.

use std::path::Path;
use thor_core::auditor::{verify_chain_integrity, DifferentialAuditor};
use thor_core::event_store::{Event, EventStore};

/// The first line of every report `doctor` prints - which build produced it.
/// THE GAP THIS CLOSES: verified 2026-09-08 by grepping every binary in the
/// workspace for `CARGO_PKG_VERSION` and finding it nowhere - a pasted report
/// carried no build number at all, so a new user's bug report could not say
/// which THOR was running, and the only way to check was a build log or git
/// tag. Called directly from `main`, the same way `checkouts_root_line`
/// already is, rather than folded into `report`'s own Vec below - both are
/// "said once, before the per-component lines", not a component of the
/// health check itself.
pub fn version_line() -> String {
    format!("doctor {}", env!("CARGO_PKG_VERSION"))
}

// ------------------------------------------------------ the short version
//
// THE DEFECT THIS CLOSES. The report below is written for the people who
// build THOR and for the assistant that reads it to them: forty lines of
// "teeth", "falsifier" and "crowding". Someone who set THOR up for a topic
// folder and typed "is it working?" got that wall and could not tell a
// healthy memory from a broken one. These few lines come first, say in
// ordinary words whether the three things that matter are in place, and each
// problem names the one next step.

/// What a hook command or a tool-server entry has to contain to count as ours:
/// the memory file's own path, compared the way the installer compares paths
/// (separators and case folded away).
fn folded(path: &str) -> String {
    path.replace('\\', "/").to_lowercase()
}

/// Whether Claude Code's settings file wires a THOR hook to this memory.
fn hooks_name_this_memory(settings: &Path, db: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(settings) else { return false };
    let Ok(root) = serde_json::from_str::<serde_json::Value>(&text) else { return false };
    let want = folded(&db.display().to_string());
    let Some(events) = root.get("hooks").and_then(|h| h.as_object()) else { return false };
    events.values().filter_map(|groups| groups.as_array()).flatten().any(|group| {
        group.get("hooks").and_then(|h| h.as_array()).is_some_and(|hooks| {
            hooks.iter().filter_map(|h| h.get("command").and_then(|c| c.as_str())).any(|command| {
                let command = folded(command);
                command.contains(&want) && command.contains(" hook")
            })
        })
    })
}

/// Whether Claude Code's tool-server file registers THOR's note-saving
/// program for this memory.
fn tool_server_names_this_memory(mcp_json: &Path, db: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(mcp_json) else { return false };
    let Ok(root) = serde_json::from_str::<serde_json::Value>(&text) else { return false };
    let want = folded(&db.display().to_string());
    root.get("mcpServers")
        .and_then(|servers| servers.get("thor"))
        .and_then(|entry| entry.get("args"))
        .and_then(|args| args.as_array())
        .is_some_and(|args| args.iter().filter_map(|a| a.as_str()).any(|a| folded(a) == want))
}

/// The short version of the report, in plain words: is the memory there, is
/// it connected to Claude Code, can the assistant save notes, and is the
/// folder this was run from a topic or project with its own memory. Nothing
/// here uses THOR's own vocabulary, and each problem ends in one next step.
///
/// `settings` and `mcp_json` are the files Claude Code reads (the installer's
/// defaults); `cwd` is the folder doctor was run from.
pub fn plain_summary(db: &Path, cwd: &Path, settings: Option<&Path>, mcp_json: Option<&Path>) -> Vec<String> {
    let ask = "ask your assistant to run THOR's install again, then restart Claude Code";
    let mut lines = vec!["THOR check - the short version".to_string()];

    match open_and_read(db) {
        Err(why) => lines.push(format!("  PROBLEM: your memory file cannot be used ({why}). Do not delete anything; {ask}.")),
        Ok(events) if !chain_intact(&events) => lines.push(
            "  PROBLEM: your memory file looks damaged. Do not delete it: it is kept in full and can be repaired. Show your assistant the detailed report below."
                .to_string(),
        ),
        Ok(_) => lines.push("  OK: your memory file is there and in good shape.".to_string()),
    }

    match settings.map(|s| hooks_name_this_memory(s, db)) {
        Some(true) => lines.push("  OK: THOR is connected to Claude Code.".to_string()),
        _ => lines.push(format!(
            "  PROBLEM: Claude Code does not know about THOR yet, so the memory will not speak up. Next step: {ask}."
        )),
    }

    match mcp_json.map(|m| tool_server_names_this_memory(m, db)) {
        Some(true) => lines.push("  OK: your assistant can save notes.".to_string()),
        _ => lines.push(format!(
            "  PROBLEM: your assistant can read the memory but cannot save notes. Next step: {ask}."
        )),
    }

    match serve::project::resolve_project(cwd) {
        Some(key) => {
            let usable = EventStore::open_existing(db)
                .ok()
                .is_some_and(|store| serve::project::registered_checkout(&store, Some(&key)).is_ok());
            if usable {
                lines.push(format!("  OK: this folder has its own memory, under the name '{key}'."));
            } else {
                lines.push(format!(
                    "  NOTE: this folder is called '{key}' but is not set up yet. Next step: open a terminal inside it and run THOR's install with --topic added."
                ));
            }
        }
        None => lines.push(
            "  NOTE: this folder has no memory of its own, so what you tell your assistant here is shared with every other folder. To give one subject its own memory, open that folder and run THOR's install with --topic added."
                .to_string(),
        ),
    }

    lines.push("Everything below is the detailed report, written for your assistant. You do not need to read it.".to_string());
    lines
}

/// Open `db` and read every event, or explain in one line (no "memory
/// store: " prefix - the caller adds whatever prefix fits it) why that
/// could not be done. The one place that decides what "the store is not
/// available" means: shared by `store_line` (which turns a failure here
/// into the first health line) and `gate_verdict` (which fails OPEN on
/// exactly the same two cases - missing, or present but unreadable).
fn open_and_read(db: &Path) -> Result<Vec<Event>, String> {
    if !db.exists() {
        return Err(format!("not found at {}", db.display()));
    }
    let store = EventStore::open_existing(db)
        .map_err(|e| format!("found at {}, but could not be opened ({e})", db.display()))?;
    store
        .get_all_events()
        .map_err(|e| format!("found at {} but could not be read ({e})", db.display()))
}

/// Whether the event log's hash chain, and the independent differential
/// fold, both hold. The one place that decides "is the chain broken",
/// shared by `store_line`'s prose and `gate_verdict`'s pass/fail check, so
/// the two can never disagree about what counts as intact.
fn chain_intact(events: &[Event]) -> bool {
    verify_chain_integrity(events).is_ok() && DifferentialAuditor::verify_consistency(events).is_ok()
}

/// Component 1+2: does the store exist, how many events, is the chain
/// (hash continuity + the independent differential fold) intact.
pub fn store_line(db: &Path) -> String {
    let events = match open_and_read(db) {
        Ok(e) => e,
        Err(msg) => return format!("memory store: {msg}"),
    };
    if chain_intact(&events) {
        format!("memory store: {} events, chain intact", events.len())
    } else {
        let reason = verify_chain_integrity(&events)
            .err()
            .or_else(|| DifferentialAuditor::verify_consistency(&events).err())
            .unwrap_or_else(|| "unknown".to_string());
        format!("memory store: {} events, CHAIN BROKEN ({reason})", events.len())
    }
}

/// Component 3: is a code index configured, and if so, at which commit
/// (plus how far the working copy has drifted from it).
pub fn code_index_line(index_db: Option<&Path>, repo: Option<&Path>) -> String {
    match (index_db, repo) {
        (Some(index_db), Some(repo)) => {
            if !index_db.exists() {
                return format!("code index: not found at {}", index_db.display());
            }
            match serve::lookup::code_index_status(index_db, repo) {
                Ok(p) => match p.current_commit {
                    // The same COMMIT is not the same FILES. This line said
                    // "matches the working copy" while four files sat
                    // uncommitted - a rosier answer than the truth, and a
                    // line number out of the index is wrong for exactly those
                    // four files. Checked now, and said out loud.
                    Ok(current)
                        if current == p.indexed_commit
                            && p.uncommitted_changed.unwrap_or(0) == 0 =>
                    {
                        format!("code index: at commit {}, matches the working copy", short(&p.indexed_commit))
                    }
                    Ok(current) if current == p.indexed_commit => format!(
                        "code index: at commit {}, but {} file(s) in the working copy are \
                         uncommitted - their line numbers are stale",
                        short(&p.indexed_commit),
                        p.uncommitted_changed
                            .map(|n| n.to_string())
                            .unwrap_or_else(|| "an unknown number of".to_string())
                    ),
                    Ok(current) => format!(
                        "code index: at commit {}, working copy is at {} ({} file(s) differ)",
                        short(&p.indexed_commit),
                        short(&current),
                        p.files_differ.map(|n| n.to_string()).unwrap_or_else(|| "an unknown number of".to_string())
                    ),
                    Err(e) => format!(
                        "code index: at commit {}, but the repository could not be read ({e})",
                        short(&p.indexed_commit)
                    ),
                },
                Err(e) => format!("code index: could not be checked ({e})"),
            }
        }
        (None, None) => "code index: not configured".to_string(),
        _ => "code index: not configured (pass both the index and the repository to check one)".to_string(),
    }
}

/// Component 4: is a replica configured, and does it run level with us.
pub fn replica_line(db: &Path, replica: Option<(&str, &str)>) -> String {
    let Some((base, token)) = replica else {
        return "replica: not configured".to_string();
    };
    let store = match EventStore::open_existing(db) {
        Ok(s) => s,
        Err(e) => return format!("replica: configured at {base}, but the local store could not be opened ({e})"),
    };
    let (local_seq, _) = match store.contiguous_tip() {
        Ok(t) => t,
        Err(e) => return format!("replica: configured at {base}, but the local tip could not be read ({e})"),
    };
    match crate::transport::remote_cursor(base, token) {
        Ok(remote) => {
            let lag = local_seq - remote.contiguous_seq;
            if lag == 0 {
                format!("replica: {base}, in sync ({local_seq} events on both sides)")
            } else if lag > 0 {
                format!("replica: {base}, BEHIND by {lag} event(s) ({} of {local_seq})", remote.contiguous_seq)
            } else {
                format!("replica: {base}, AHEAD of us by {} event(s) - not a plain replica of this store", -lag)
            }
        }
        Err(e) => format!("replica: {base}, UNREACHABLE ({e})"),
    }
}

/// How long a successful ship (`ops::transport::push_once`, run hourly by
/// the owner's own scheduled task via `sync ship`) may age before this line
/// calls it stale.
///
/// WHY THREE HOURS. The task runs hourly, so one missed cycle is an hour
/// overdue and two missed cycles is two - either is explained by an ordinary
/// reboot or a NAS that blinked off for a firmware update, and neither is
/// the failure this line exists to catch. Three hours is the first point the
/// SAME hourly task has had three consecutive chances to run and used none
/// of them - a real gap, not noise from a single missed run. THE DEFECT THIS
/// LINE CLOSES ran four WEEKS past any such ceiling before a person noticed
/// the date by hand (see `ops::ship_state`'s own doc comment) - three hours
/// only has to be small next to that.
pub const SHIP_STALE_CEILING_HOURS: u64 = 3;

/// FIELD TEST 5 (D4): the tool server (`mcp.exe`) a Claude Code session talks
/// to is loaded once, when Claude starts. After the program is replaced a
/// running session keeps the old one, and the two disagree about what the
/// memory says (one reported 140 owed where the new count was 13). This names
/// every running `mcp.exe` that started before the file on disk was replaced.
/// `None` when none is stale, none runs, or the platform cannot say.
pub fn stale_tool_server_line(bin_dir: &Path) -> Option<String> {
    let exe = bin_dir.join(if cfg!(windows) { "mcp.exe" } else { "mcp" });
    let modified = std::fs::metadata(&exe).ok()?.modified().ok()?;
    let modified_unix = modified.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs() as i64;
    let started = running_tool_servers(&exe);
    stale_servers_line(modified_unix, &started)
}

/// The words of the finding, from when the file was replaced and the
/// `(process id, start time)` of every running tool server.
fn stale_servers_line(replaced_unix: i64, started: &[(u32, i64)]) -> Option<String> {
    let stale: Vec<&(u32, i64)> = started.iter().filter(|(_, start)| *start < replaced_unix).collect();
    if stale.is_empty() {
        return None;
    }
    let ids: Vec<String> = stale
        .iter()
        .map(|(pid, start)| format!("{pid} (started {})", serve::time::iso8601_from_unix(*start)))
        .collect();
    Some(format!(
        "tool server: {} running mcp.exe process(es) started before mcp.exe was replaced ({}): {} - they still run the OLD program and may count and judge differently. Restart Claude to load the new tool server.",
        stale.len(),
        serve::time::iso8601_from_unix(replaced_unix),
        ids.join(", ")
    ))
}

/// Every running process of `exe`, as (process id, start time in Unix
/// seconds). Windows only: asks PowerShell, windowless; anywhere else, or when
/// that fails, an empty list (the finding is then simply not made).
#[cfg(windows)]
fn running_tool_servers(exe: &Path) -> Vec<(u32, i64)> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let script = "Get-Process -Name mcp -ErrorAction SilentlyContinue | ForEach-Object { $_.Id.ToString() + '|' + ([DateTimeOffset]$_.StartTime).ToUnixTimeSeconds() + '|' + $_.Path }";
    let Ok(out) = std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
    else {
        return Vec::new();
    };
    let wanted = exe.to_string_lossy().replace('/', "\\").to_lowercase();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| {
            let mut parts = line.trim().splitn(3, '|');
            let pid = parts.next()?.parse::<u32>().ok()?;
            let start = parts.next()?.parse::<i64>().ok()?;
            let path = parts.next()?.replace('/', "\\").to_lowercase();
            (path == wanted).then_some((pid, start))
        })
        .collect()
}

#[cfg(not(windows))]
fn running_tool_servers(_exe: &Path) -> Vec<(u32, i64)> {
    Vec::new()
}

/// The installed programs, run once each with `--version`: `None` for one that
/// is missing or could not be started (the installer says so when it refuses;
/// here that is not a skew finding).
fn version_output(program: &Path) -> Option<String> {
    let mut command = std::process::Command::new(program);
    command.arg("--version");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let out = command.output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// THE SKEW A SINGLE VERSION NUMBER HIDES. Every program prints the release
/// version, so a tool server older than the hook's checker looked identical to
/// the current one, and a filing it wrote was refused with a reason that blamed
/// the agent (2026-10-06). The build id (`serve::BUILD_ID`) tells two builds
/// apart: this runs the programs installed beside doctor and names the case
/// where they were not built from one source. `None` when they agree, or when
/// there is nothing to compare.
pub fn version_skew_line(bin_dir: &Path) -> Option<String> {
    let suffix = std::env::consts::EXE_SUFFIX;
    let mut programs: Vec<(String, Option<String>)> = ["serve", "mcp"]
        .iter()
        .map(|name| (format!("{name}{suffix}"), version_output(&bin_dir.join(format!("{name}{suffix}")))))
        .collect();
    programs.push((format!("doctor{suffix}"), Some(serve::version_line("doctor"))));
    version_skew_words(&programs)
}

/// The words of the finding, from each program's `--version` output.
pub fn version_skew_words<S: AsRef<str>>(programs: &[(S, Option<String>)]) -> Option<String> {
    let mut with_id: Vec<(&str, &str)> = Vec::new();
    let mut without_id: Vec<&str> = Vec::new();
    for (name, output) in programs {
        let Some(output) = output else { continue };
        match serve::build_id_in(output) {
            Some(id) => with_id.push((name.as_ref(), id)),
            None => without_id.push(name.as_ref()),
        }
    }
    let first = with_id.first()?.1;
    let differ = with_id.iter().any(|(_, id)| *id != first);
    if !differ && without_id.is_empty() {
        return None;
    }
    // WHAT DECIDES WHETHER A FILING COUNTS is the machine-section renderer each
    // program prints, not the build id (field test 15, C2): programs of two
    // builds that render alike (a deliberate serve-only install) leave every
    // filing counting, and the line used to tell the owner to restart anyway.
    let renderer_of = |name: &str| programs.iter().find(|(n, _)| n.as_ref() == name).and_then(|(_, out)| out.as_deref()).and_then(serve::renderer_in);
    let mut renderers: Vec<Option<u32>> = Vec::new();
    let mut parts: Vec<String> = with_id
        .iter()
        .map(|(name, id)| {
            let renderer = renderer_of(name);
            renderers.push(renderer);
            match renderer {
                Some(n) => format!("{name} is build {id} (renderer {n})"),
                None => format!("{name} is build {id} (prints no renderer)"),
            }
        })
        .collect();
    parts.extend(without_id.iter().map(|name| format!("{name} prints no build id (it is older than the others)")));
    renderers.extend(without_id.iter().map(|_| None));
    let known: Vec<u32> = renderers.iter().flatten().copied().collect();
    let apart = known.iter().any(|r| *r != known[0]);
    let all_known_and_alike = !apart && renderers.iter().all(|r| r.is_some());
    let verdict = if apart {
        "Renderers differ: a report filed by the program with the older renderer is on file and dead. Copy every program from one build into the folder (run install.exe again), then restart Claude."
    } else if all_known_and_alike {
        "Renderers match: evaluation filings count, so this does not hold your report. Restart Claude only when a \"tool server:\" line is also printed."
    } else {
        "Evaluation filings still count. Restart Claude only when a \"tool server:\" line is also printed."
    };
    Some(format!(
        "installed programs: {} - the programs come from different builds, which happens when a fix is installed into one program without a restart. {verdict}",
        parts.join(", ")
    ))
}

/// How far back the hook failure line looks.
const HOOK_FAILURE_DAYS: i64 = 7;

/// Whether a Windows Code Integrity message names one of THOR's programs.
fn names_a_thor_program(message: &str) -> bool {
    let message = message.to_ascii_lowercase().replace('/', "\\");
    ["\\thor2\\", "\\serve.exe", "\\mcp.exe", "\\doctor.exe"].iter().any(|name| message.contains(name))
}

/// The `unix|message` lines the Code Integrity reader prints, as the times of
/// those inside the span that name a THOR program.
fn parse_code_integrity_events(text: &str, since_unix: i64) -> Vec<i64> {
    text.lines()
        .filter_map(|line| {
            let (at, message) = line.split_once('|')?;
            let at: i64 = at.trim().parse().ok()?;
            (at >= since_unix && names_a_thor_program(message)).then_some(at)
        })
        .collect()
}

/// The times of Windows Code Integrity block events (event 3077) naming a
/// THOR program since `since_unix`. The log is readable without admin rights;
/// when it is not, or on any other system, nothing is reported.
#[cfg(windows)]
fn code_integrity_blocks(since_unix: i64) -> Vec<i64> {
    use std::os::windows::process::CommandExt;
    let script = "try { Get-WinEvent -FilterHashtable @{LogName='Microsoft-Windows-CodeIntegrity/Operational'; Id=3077} -MaxEvents 500 -ErrorAction Stop | ForEach-Object { ([DateTimeOffset]$_.TimeCreated).ToUnixTimeSeconds().ToString() + '|' + ($_.Message -replace '\\s+', ' ') } } catch {}";
    let Ok(out) = std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .creation_flags(0x0800_0000)
        .output()
    else {
        return Vec::new();
    };
    parse_code_integrity_events(&String::from_utf8_lossy(&out.stdout), since_unix)
}

#[cfg(not(windows))]
fn code_integrity_blocks(_since_unix: i64) -> Vec<i64> {
    Vec::new()
}

/// THE FAILURE NOBODY COULD REPORT. A hook Claude Code cannot start (Windows
/// Smart App Control or an antivirus blocking `serve.exe`, a permission, the
/// wrong shell) leaves a record in the session's transcript and nothing in
/// THOR, so the memory was silent with no error anywhere. This reads those
/// records (`serve::hook_faults`) and the Windows Code Integrity log, and says
/// so in one line. `None` at zero.
pub fn hook_failures_line(now_unix: i64) -> Option<String> {
    let blocks = code_integrity_blocks(now_unix - HOOK_FAILURE_DAYS * 86_400);
    hook_failures_line_in(serve::attest::transcripts_root().as_deref(), &blocks, now_unix)
}

/// `hook_failures_line` for transcripts under `root` and the block times
/// already read.
pub fn hook_failures_line_in(root: Option<&Path>, blocks: &[i64], now_unix: i64) -> Option<String> {
    let since = now_unix - HOOK_FAILURE_DAYS * 86_400;
    let faults: Vec<serve::hook_faults::Scanned> = root
        .map(|root| serve::hook_faults::faults_since(root, since))
        .unwrap_or_default()
        .into_iter()
        .filter(|f| serve::eval_filing::unix_of_timestamp(&f.fault.at).is_some_and(|at| at >= since))
        .collect();
    hook_failures_words(&faults, blocks)
}

/// The words of the finding, from the failures found and the Code Integrity
/// block times.
pub fn hook_failures_words(faults: &[serve::hook_faults::Scanned], blocks: &[i64]) -> Option<String> {
    // A failure the machine caused (a hook of another program failed at the
    // same moment: sleep or shutdown) is said apart and is never THOR's.
    let (host_wide, faults): (Vec<&serve::hook_faults::Scanned>, Vec<&serve::hook_faults::Scanned>) =
        faults.iter().partition(|f| f.fault.host_wide);
    let mut parts: Vec<String> = Vec::new();
    if let Some(last) = faults.iter().max_by(|a, b| a.fault.at.cmp(&b.fault.at)) {
        parts.push(format!(
            "hook launch failures: {} in the last {HOOK_FAILURE_DAYS} days, the last at {} ({}, exit {}: {}) - the output of that hook never reached the session, so THOR was silent for it",
            faults.len(),
            last.fault.at,
            last.fault.hook_event,
            last.fault.exit_code,
            last.cause
        ));
    }
    if let Some(last) = blocks.iter().max() {
        parts.push(format!(
            "Windows Code Integrity blocked a THOR program {} time(s) in the last {HOOK_FAILURE_DAYS} days, the last at {}",
            blocks.len(),
            serve::time::iso8601_from_unix(*last)
        ));
    }
    let mut out = String::new();
    if !parts.is_empty() {
        out = format!(
            "{}. THOR cannot change Windows security settings: allow the program there (or in your antivirus), then restart Claude.",
            parts.join("; ")
        );
    }
    if let Some(last) = host_wide.iter().max_by(|a, b| a.fault.at.cmp(&b.fault.at)) {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(&format!(
            "{} hook(s) {} (in the last {HOOK_FAILURE_DAYS} days, the last at {}, {}, exit {}): a hook of another program failed at the same moment, so the machine was suspending or shutting down. Nothing to do.",
            host_wide.len(),
            model::store::HOST_WIDE_WORDS,
            last.fault.at,
            last.fault.hook_event,
            last.fault.exit_code
        ));
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

/// The off-site backup, in plain words: where the last copy went and when,
/// or - when the last attempt failed - that it is failing, since when and
/// why. `None` only before any backup has run here. The backup runs inside a
/// SessionStart hook, where a failure is invisible, so this is where the owner
/// learns the copy has gone stale, and which place it is supposed to reach.
pub fn backup_line(db: &Path) -> Option<String> {
    backup_line_at(db, crate::backup_state::now_unix())
}

/// `nas (//host/share/folder)`, or just the folder name when no remote was
/// ever recorded.
fn backup_target_text(state: &crate::backup_state::BackupState) -> String {
    let repo = if state.repo.is_empty() { "the backup repository" } else { state.repo.as_str() };
    match (state.remote.is_empty(), state.address.is_empty()) {
        (true, _) => repo.to_string(),
        (false, true) => format!("{repo} (remote {})", state.remote),
        (false, false) => format!("{repo} (remote {}: {})", state.remote, state.address),
    }
}

fn backup_line_at(db: &Path, now: u64) -> Option<String> {
    let state = crate::backup_state::read(db)?;
    let target = backup_target_text(&state);
    if !state.is_failing() {
        if state.last_ok_unix == 0 {
            return None;
        }
        let hours = now.saturating_sub(state.last_ok_unix) / 3600;
        return Some(format!("backup: last copy went to {target}, {hours}h ago"));
    }
    let hours = now.saturating_sub(state.failing_since_unix) / 3600;
    Some(format!(
        "backup to {target} failing since {} ({hours}h ago): {} - the off-site copy is not being updated; run backup.exe by hand and read what it prints",
        serve::time::iso8601_from_unix(state.failing_since_unix as i64),
        state.last_error
    ))
}

/// Component: how long ago `sync ship` last completed without error, read
/// from the sidecar `ops::ship_state` writes beside `db` on every attempt.
///
/// FOUR SHAPES, CHECKED IN THIS ORDER, once a sidecar carries the newer
/// per-attempt fields (`ShipState::last_attempt_unix` is `Some` - see below
/// for the sidecar that does not):
///   1. THE MOST RECENT ATTEMPT FAILED (`last_attempt_error` is `Some`) - an
///      alarm naming when it failed, the one-line reason, and when the last
///      success was (or that there has never been one).
///   2. NOTHING HAS BEEN WRITTEN TO THE STORE SINCE THE LAST SUCCESS (our
///      current local tip is no further than `covered_seq`) - fresh, no
///      matter how many hours old, because nothing is waiting to be lost.
///   3. CHANGES ARE WAITING and the last success is still within
///      `SHIP_STALE_CEILING_HOURS` - fresh, the same line this has always
///      printed for a healthy hourly cadence.
///   4. CHANGES ARE WAITING and the last success is OLDER than the ceiling -
///      never worded as a failure, because it may not be one: the hourly
///      task simply may not have had a chance to run (the machine could be
///      asleep, not broken). Says how much is waiting, how long ago the
///      last ship was, and that the hourly ship has not run since, so the
///      owner can tell a sleeping PC from a broken task.
///
/// A SIDECAR WRITTEN BEFORE THIS SPLIT EXISTED (`last_attempt_unix` is
/// `None`) falls back to exactly the two-shape (fresh/STALE) text this line
/// always printed - old data cannot support the newer distinction, so this
/// does not try to guess one. The same fallback covers the rare case where
/// the newer fields ARE present but the local store cannot be opened to
/// compare against `covered_seq`: best effort, never a hard failure.
///
/// `None`, ON PURPOSE, WHEN THIS MACHINE HAS NEVER SHIPPED. Shipping is a
/// per-machine role - only the one machine with the hourly task cares, and
/// most machines a report runs on (a laptop, the NAS receiver itself, a
/// cloud sandbox) were never meant to ship anywhere. A permanent "ship: not
/// configured" line on every one of them would be exactly the alarm that
/// cries wolf `gate_verdict`'s own doc comment already refuses to become
/// elsewhere - so this line simply does not exist until the first ship
/// attempt writes the sidecar that makes it meaningful.
pub fn ship_line(db: &Path) -> Option<String> {
    let state = crate::ship_state::read(db)?;
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
    let age_hours = now.saturating_sub(state.completed_unix) / 3600;
    let stale_text = || {
        format!(
            "ship: last succeeded {age_hours}h ago - STALE (ceiling {SHIP_STALE_CEILING_HOURS}h): the \
             hourly ship may be stuck or failing silently; check it by hand"
        )
    };
    let fresh_text = || format!("ship: last succeeded {age_hours}h ago - fresh");

    // Old sidecar, written before attempts were tracked separately from
    // successes: keep printing exactly what this line always printed.
    let Some(last_attempt_unix) = state.last_attempt_unix else {
        return Some(if age_hours >= SHIP_STALE_CEILING_HOURS { stale_text() } else { fresh_text() });
    };

    if let Some(reason) = &state.last_attempt_error {
        let attempt_age = now.saturating_sub(last_attempt_unix) / 3600;
        return Some(if state.completed_unix == 0 {
            format!("ship: last attempt FAILED {attempt_age}h ago: {reason} - no successful ship yet")
        } else {
            format!("ship: last attempt FAILED {attempt_age}h ago: {reason} - last succeeded {age_hours}h ago")
        });
    }

    // The last attempt succeeded. Compare what it covered to our current
    // local tip to tell "nothing waiting" from "changes waiting" - best
    // effort: a store that cannot be opened here falls back to the plain
    // age, the same text this line printed before it could compare at all.
    let waiting = state.covered_seq.and_then(|covered| {
        let store = EventStore::open_existing(db).ok()?;
        let (local_seq, _) = store.contiguous_tip().ok()?;
        Some(local_seq.saturating_sub(covered).max(0))
    });

    Some(match waiting {
        Some(0) => format!("ship: last succeeded {age_hours}h ago - fresh, nothing new has been written since"),
        Some(n) if age_hours >= SHIP_STALE_CEILING_HOURS => format!(
            "ship: {n} change(s) not yet on the replica - last succeeded {age_hours}h ago and no attempt has \
             been recorded since, so the hourly ship has not run since, or it started and died before it \
             could record anything. Check the task's Last Run Result in Task Scheduler (a machine that was \
             asleep looks the same); a program that was removed or quarantined is named by the line above, \
             and a script that records its failures (thor2/deploy/ship-hourly.example.ps1) shows the error here"
        ),
        Some(_) => fresh_text(),
        None if age_hours >= SHIP_STALE_CEILING_HOURS => stale_text(),
        None => fresh_text(),
    })
}

/// The third shape of a silent ship: a scheduled task calls the ship but this
/// store has never recorded an attempt. Either the task never ran, or its
/// script died before it could record (the 2026-10-11 quarantine did exactly
/// that). `None` when a sidecar exists or no task names the ship.
pub fn ship_never_recorded_line(db: &Path, task_called: &[std::path::PathBuf]) -> Option<String> {
    if crate::ship_state::read(db).is_some() {
        return None;
    }
    let ship = task_called.iter().find(|p| {
        let name = p.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_lowercase();
        name == "sync" || name.contains("ship")
    })?;
    Some(format!(
        "ship: a scheduled task calls {} but nothing has ever been recorded for it - the task has never run, or its script stops before it can record a failure. Check the task's Last Run Result in Task Scheduler; the example script thor2/deploy/ship-hourly.example.ps1 records every failure, including a missing program",
        ship.display()
    ))
}

/// The lines about THOR programs that something calls and that are not on
/// disk (hooks, the tool server, the sidecars' programs and the scheduled
/// tasks), plus the ship that left no record. Empty when all is in place.
/// Reads Windows Defender's history only when something is missing, and only
/// as far as Windows lets this user read it.
pub fn program_lines(db: &Path, settings: Option<&Path>, mcp_json: Option<&Path>, bin_dir: Option<&Path>) -> Vec<String> {
    let xml = serve::missing_programs::scheduled_tasks_xml();
    let task_called = serve::missing_programs::task_programs(&xml, &|p| std::fs::read_to_string(p).ok());
    let gone = serve::missing_programs::find(db, settings, mcp_json, bin_dir, Some(&xml));
    let detected = if gone.is_empty() || !cfg!(windows) { Vec::new() } else { serve::missing_programs::defender_names() };
    serve::missing_programs::words_for(&gone, &detected, cfg!(windows))
        .into_iter()
        .chain(ship_never_recorded_line(db, &task_called))
        .collect()
}

/// How many writes queued on a replica did NOT land at the drain - refused by
/// the gate or the trial there, or not replayable - and where their copies
/// are. The replica session that wrote them was told "queued" and can never
/// hear the refusal (step 8, F13: the trial needs the working copy, which only
/// this machine has), so this line is the only place anyone learns of it.
/// Silent while nothing was ever stranded.
pub fn drain_refusals_line(db: &Path) -> Option<String> {
    let path = crate::drain::stranded_path(db);
    let text = std::fs::read_to_string(&path).ok()?;
    let count = text.lines().filter(|l| !l.trim().is_empty()).count();
    (count > 0).then(|| {
        format!(
            "drain: {count} queued write(s) from a replica were refused at the drain (or could not be replayed) and \
             never landed - the replica session was only told 'queued'. Each is kept in {}: read it, and write the \
             ones that still matter again from a session on this machine",
            path.display()
        )
    })
}

/// Component 5: how many fireable items (Rule/Orientation) still have no
/// falsifier - the worklist number, in plain language.
pub fn falsifier_line(db: &Path) -> String {
    let store = match EventStore::open_existing(db) {
        Ok(s) => s,
        Err(_) => return "falsifiers: memory store not available - nothing to count".to_string(),
    };
    let status = serve::status::store_status(&store);
    if status.fireable_total == 0 {
        return "falsifiers: no rules or orientations declared yet".to_string();
    }
    if status.missing_falsifier == 0 {
        format!("falsifiers: all {} rule(s)/orientation(s) have one", status.fireable_total)
    } else {
        format!(
            "falsifiers: {} of {} rule(s)/orientation(s) still have no falsifier",
            status.missing_falsifier, status.fireable_total
        )
    }
}

/// How many rules and orientations carry a machine-runnable proof, not just
/// prose - the number that decides how much of this memory can ever stop a
/// wrong write.
///
/// WHY THIS LINE EXISTS. The doctrine is that only a rule whose check runs
/// and holds may block, and that prose can inform but never forbid. That
/// was built, tested, and proven blind. What nobody could see was how much
/// of the store it actually applied to, because nothing counted it: this
/// health check reported falsifier coverage at 100% and read as healthy
/// while proof coverage sat at 2 items out of 2999 (measured 2026-08-06).
/// A capability nothing is wired into looks exactly like one that works.
pub fn proof_line(db: &Path, full: bool) -> String {
    let store = match EventStore::open_existing(db) {
        Ok(s) => s,
        Err(_) => return "provable rules: memory store not available - nothing to count".to_string(),
    };
    let items = serve::live::live_items(&store);
    let fireable: Vec<_> = items
        .iter()
        .filter(|i| matches!(i.item.kind, model::item::Kind::Rule | model::item::Kind::Orientation))
        .collect();
    if fireable.is_empty() {
        return "provable rules: no rules or orientations declared yet".to_string();
    }
    let with_check = fireable.iter().filter(|i| i.item.check.is_some()).count();

    // WHY THIS LINE BREAKS THE NUMBER DOWN, since 2026-08-08. "Carries a
    // check" was reported as if it meant "can block", and two independent
    // reviews caught that it does not. What a check can actually do depends
    // entirely on its FORM, and the forms differ more than the single
    // percentage suggested:
    //   Absent / AbsentAll - refuse a write that INTRODUCES the literal into
    //     the file the check names. The strongest form.
    //   Forbidden - the same, with no file at all - but ONLY on an item bound
    //     Always. `serve::absent_guard::find_forbidden_violation` is fed the
    //     Always pool and nothing else, so a Forbidden check on a moment- or
    //     target-bound item passes the write gate, looks like the strongest
    //     form, and can never block. It is counted as blocking nothing.
    //   Contains - refuses only a write to that same file that REMOVES the
    //     literal (`absent_guard::find_missing_required`). Real, but narrow:
    //     it protects a line, it never tests whether the rule's claim is true.
    //   PathExists - blocks nothing ON ITS OWN, but it is exactly what the
    //     LOCATION arm blocks with: Irreversible severity plus a Path/Dir
    //     binding equal to the check's own path makes the whole location out
    //     of bounds (`absent_guard::location_anchor`). Counted separately,
    //     because it refuses a write for WHERE it lands, not what it carries.
    //
    // Reporting one figure let a store full of Contains checks read as if it
    // were enforced. Counting by FORM rather than by capability then let a
    // Forbidden-without-Always read as the strongest thing in the store. Both
    // corrections came from a review reading the arms rather than the checks.
    let (mut forbidding, mut protecting, mut location, mut blocks_nothing) = (0usize, 0usize, 0usize, 0usize);
    for i in &fireable {
        // One definition, shared with `teeth_line` via `can_refuse`, so the
        // two lines can never disagree about what "can refuse" means.
        let refuses = can_refuse(&i.item);
        // A `Forbidden` check reaches wherever its BINDING says, and there are
        // exactly two bindings that carry a reach it can honour: Always (every
        // file write, via `absent_guard::find_forbidden_violation`) and Command
        // (that one command, via `find_command_violation`). Anything else -
        // a moment, a path, a directory - passes the write gate, looks like the
        // strongest form, and can never fire.
        match &i.item.check {
            Some(model::item::Check::Absent { .. })
            | Some(model::item::Check::AbsentAll { .. })
            | Some(model::item::Check::Forbidden { .. })
                if refuses =>
            {
                forbidding += 1
            }
            Some(model::item::Check::Absent { .. })
            | Some(model::item::Check::AbsentAll { .. })
            | Some(model::item::Check::Forbidden { .. }) => blocks_nothing += 1,
            // A conditional check refuses a CALL that forgot something. Its
            // reach is the Always binding, like Forbidden's, and it is the
            // one form whose requirement is an absence rather than a word
            // to catch - so it counts as teeth exactly when it can fire.
            Some(model::item::Check::Requires { .. }) if refuses => forbidding += 1,
            Some(model::item::Check::Requires { .. }) => blocks_nothing += 1,
            Some(model::item::Check::Contains { .. }) => protecting += 1,
            Some(model::item::Check::PathExists { path }) => {
                let bar = i.item.severity == Some(model::item::Severity::Irreversible)
                    && i.item.bindings.iter().any(|b| {
                        matches!(
                            b,
                            model::item::Binding::Target { kind: model::item::TargetKind::Path | model::item::TargetKind::Dir, value }
                                if value == path
                        )
                    });
                if bar {
                    location += 1;
                } else {
                    blocks_nothing += 1;
                }
            }
            None => {}
        }
    }
    // The other population, and until 2026-08-14 nothing counted it: the rules
    // that were ASKED whether they could refuse and answered no. That answer is
    // the one exit from the gate nothing can verify, so the only honest thing
    // to do with it is show how much of the store rests on it - and how much of
    // that was taken on a bare word, before the reason was required. Bare ones
    // are grandfathered by `gate::revise`, never re-asked by the backlog burn,
    // and would otherwise be invisible forever.
    // NAMED, not just counted, since this is the second half of the same gap
    // `judgement_debt_named` already closed for the debt list below: a bare
    // `no-literal` is grandfathered by `gate::revise` and never re-asked by
    // any backlog burn (see `TeethAnswer::Bare`'s own doc comment), so
    // nothing else in this system will ever surface it again on its own.
    // Sorted OLDEST first - by `first_seq`, the earliest event this store
    // holds for the id, since no event carries a wall-clock timestamp to
    // sort by more precisely - because this is exactly the population nothing
    // ever revisits automatically: the longer one of these has sat bare, the
    // less likely anyone still remembers why, and the more it is the one
    // worth a second look before the next 20 push it off the list.
    let (mut answered_no, mut without_reason) = (0usize, 0usize);
    let mut bare: Vec<(i64, Rot)> = Vec::new();
    for i in &fireable {
        match i.item.tags.iter().find_map(|t| model::store::teeth_answer(t)) {
            Some(model::store::TeethAnswer::Bare) => {
                answered_no += 1;
                without_reason += 1;
                bare.push((
                    first_seq(&store, &i.id),
                    Rot {
                        id: i.id.clone(),
                        project: i.item.project.clone().unwrap_or_else(|| "global".to_string()),
                        what: format!("{:?}, tagged '{}' with no reason", i.item.severity, model::store::NO_LITERAL_TAG),
                    },
                ));
            }
            Some(model::store::TeethAnswer::Reasoned(_)) => answered_no += 1,
            None => {}
        }
    }
    bare.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.id.cmp(&b.1.id)));
    let bare: Vec<Rot> = bare.into_iter().map(|(_, r)| r).collect();
    // THE NEW POPULATION, since gate ground 11 (continued, command) started
    // asking a heavy Command-anchored fact the SHARPER question
    // (`model::gate::command_teeth_answer`) instead of the plain one just
    // above (`model::store::teeth_answer`): how many answered it with
    // `no-catch-on-command:<why>`. Counted separately, never folded into
    // `answered_no` above - the two loops read two different tags, on
    // purpose, the same way the gate itself now reads only one of them
    // depending on the item's own shape.
    let mut command_answered_no = 0usize;
    for i in &fireable {
        let heavy = matches!(
            i.item.severity,
            Some(model::item::Severity::Irreversible) | Some(model::item::Severity::Costly)
        );
        let command_bound = i.item.bindings.iter().any(|b| {
            matches!(b, model::item::Binding::Target { kind: model::item::TargetKind::Command, .. })
        });
        if heavy
            && command_bound
            && i.item.check.is_none()
            && i.item.tags.iter().find_map(|t| model::gate::command_teeth_answer(t)).is_some()
        {
            command_answered_no += 1;
        }
    }
    let mut out = vec![format!(
        "provable rules: {} of {} rule(s)/orientation(s) carry a runnable check ({:.1}%) - of those, \
         {forbidding} can refuse a write that introduces something forbidden, {location} can refuse a \
         write for landing in a place that is out of bounds, {protecting} can only refuse one that \
         removes a required line from their own file, and {blocks_nothing} block nothing at all; \
         {answered_no} other rule(s) were asked and answered that nothing can catch them, {without_reason} \
         of those without saying why (a bare answer nothing will ask about again), and \
         {command_answered_no} of those anchored to a command answered with the sharper \
         no-catch-on-command tag instead",
        with_check,
        fireable.len(),
        100.0 * with_check as f64 / fireable.len() as f64
    )];
    out.extend(name_rot("bare no-catch", &bare, full));
    out.join("\n")
}

// ------------------------------------------------------ the checkouts root
//
// THE DEFECT THIS CLOSES, reported at the end of two separate sessions:
// "decay and crowding not measured (requires --checkouts)". `decay_line` and
// `crowding_line` below already say so when `checkouts` is `None` - but that
// left a new reader to somehow already know the flag exists, and what a
// working value even looks like, with nothing here to suggest one. Doctor
// now tries the same thing a person would: the directory holding the repo
// you are standing in is usually a directory of sibling checkouts, which is
// exactly what `--checkouts` has always wanted (see `Cli::checkouts`'s own
// doc comment in `bin/doctor.rs`).

/// The nearest ancestor of `start_dir` (inclusive) that is itself a repo: it
/// carries a THOR project marker (`serve::project::MARKER_FILE_NAMES`,
/// checked directly in the directory - never inherited from a parent, same
/// scope `serve::project::resolve_project` gives a marker) or a `.git` entry,
/// directory or file (a linked worktree's own shape). `None` all the way to
/// the filesystem root.
///
/// A LINKED WORKTREE counts as its MAIN checkout (`serve::project::
/// checkout_for`). Measured 2026-09-26: a session standing in a detached
/// worktree kept in a scratch folder got that scratch folder as its inferred
/// checkouts root, so doctor judged every check against a worktree six
/// commits behind main, called a true fact false, and the session revised
/// the fact backwards. What doctor measures is the main checkout, the same
/// one the commit gates read.
fn nearest_repo_root(start_dir: &Path) -> Option<std::path::PathBuf> {
    let mut dir: Option<&Path> = Some(start_dir);
    while let Some(d) = dir {
        let has_marker = serve::project::MARKER_FILE_NAMES.iter().any(|name| d.join(name).is_file());
        let dot_git = d.join(".git");
        if has_marker || dot_git.is_dir() || dot_git.is_file() {
            return Some(serve::project::checkout_for(d));
        }
        dir = d.parent();
    }
    None
}

/// Every project under `root` - an immediate subdirectory that resolves to a
/// project key - mapped to the checkout its checks are judged against. The
/// one place decay, contradictions and crowding learn where a project lives.
///
/// A linked worktree is judged as its MAIN checkout (`serve::project::
/// checkout_for`), and a main checkout found under `root` always wins over
/// a worktree of it: a worktree is a place someone works, its files lag or
/// lead main, and a check read there says nothing about the memory's own
/// truth (see `nearest_repo_root` for the day that mattered).
///
/// THE REGISTERED WORKING COPY WINS when it lies under `root` at any depth
/// and is still valid (`serve::project::registered_checkout`: the runtime's own
/// reading). Found by the scan above it, a repository nested one level deeper
/// (`<root>/<group>/<repo>`) was never seen, so every anchor of its project
/// read as dead while `serve why` resolved the same file correctly (measured
/// 2026-10-07, field test 12). A registered folder outside `root` is left to
/// the scan: `--checkouts` still bounds what doctor measures.
fn checkout_roots(store: &EventStore, root: &Path) -> std::collections::BTreeMap<String, std::path::PathBuf> {
    let mut roots = scanned_roots(root);
    let projects: std::collections::BTreeSet<String> =
        serve::live::live_items(store).into_iter().filter_map(|li| li.item.project).collect();
    for project in projects {
        if let Ok(registered) = serve::project::registered_checkout(store, Some(&project)) {
            if is_under(root, &registered) {
                roots.insert(project, registered);
            }
        }
    }
    roots
}

/// The immediate subdirectories of `root` that resolve to a project key, each
/// mapped to its checkout: the scan half of `checkout_roots`.
fn scanned_roots(root: &Path) -> std::collections::BTreeMap<String, std::path::PathBuf> {
    let mut roots: std::collections::BTreeMap<String, std::path::PathBuf> = Default::default();
    if let Ok(entries) = std::fs::read_dir(root) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if let Some(key) = serve::project::resolve_project(&path) {
                    roots.insert(key, serve::project::checkout_for(&path));
                }
            }
        }
    }
    roots
}

/// Whether `path` lies inside `root`, compared as text with one separator,
/// without case and without a Windows verbatim prefix.
fn is_under(root: &Path, path: &Path) -> bool {
    let plain = |p: &Path| {
        let text = p.to_string_lossy().replace('\\', "/").to_lowercase();
        text.trim_start_matches("//?/").trim_end_matches('/').to_string()
    };
    let (root, path) = (plain(root), plain(path));
    path.strip_prefix(&root).is_some_and(|rest| rest.starts_with('/'))
}

/// Doctor's own inferred `--checkouts` root when the flag itself is absent:
/// the PARENT of the repo `start_dir` sits in - the directory whose
/// immediate children are checkouts, exactly what `--checkouts` has always
/// asked for. `None` when no ancestor is a repo, or the repo has no parent
/// to use (sitting at a filesystem root).
pub fn infer_checkouts_root(start_dir: &Path) -> Option<std::path::PathBuf> {
    nearest_repo_root(start_dir)?.parent().map(Path::to_path_buf)
}

/// Where doctor's own effective `--checkouts` root came from this run -
/// `report`'s/`main`'s own "checkouts root" line names exactly this, once,
/// so `decay_line`/`crowding_line` never go quiet without saying why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckoutsRoot {
    /// `--checkouts <path>` was given directly - always wins over inference
    /// (see `resolve_checkouts_root`): a caller who stated a root outright
    /// is never second-guessed by a convenience this replaces silence with.
    Explicit(std::path::PathBuf),
    /// No flag - `infer_checkouts_root` found one instead.
    Inferred(std::path::PathBuf),
    /// Neither a flag nor an inferable repo: decay and crowding go
    /// unmeasured this run, and the report says so in one plain line rather
    /// than the previous silence.
    Unresolved,
}

impl CheckoutsRoot {
    /// The path to actually feed `decay_line`/`crowding_line`/
    /// `orphan_projects_line`/`gate_verdict` - `None` for `Unresolved`,
    /// which is exactly the `Option<&Path>` those already accept.
    pub fn as_path(&self) -> Option<&Path> {
        match self {
            CheckoutsRoot::Explicit(p) | CheckoutsRoot::Inferred(p) => Some(p),
            CheckoutsRoot::Unresolved => None,
        }
    }
}

/// Decide the checkouts root doctor actually uses this run: an explicit
/// `--checkouts` always wins; otherwise fall back to `infer_checkouts_root`
/// from `start_dir` (doctor's own working directory).
pub fn resolve_checkouts_root(explicit: Option<&Path>, start_dir: &Path) -> CheckoutsRoot {
    if let Some(p) = explicit {
        return CheckoutsRoot::Explicit(p.to_path_buf());
    }
    match infer_checkouts_root(start_dir) {
        Some(p) => CheckoutsRoot::Inferred(p),
        None => CheckoutsRoot::Unresolved,
    }
}

/// The report's own line naming which root fed `decay_line`/`crowding_line`
/// this run, and that `--checkouts` overrides it - or, when neither an
/// explicit flag nor an inferable repo gave one, ONE plain line saying so
/// instead of the silence two sessions in a row ended on.
pub fn checkouts_root_line(root: &CheckoutsRoot) -> String {
    match root {
        CheckoutsRoot::Explicit(p) => format!("checkouts root: {} (from --checkouts)", p.display()),
        CheckoutsRoot::Inferred(p) => format!(
            "checkouts root: {} (inferred: the parent of the repo you are standing in; pass --checkouts <dir> to use a different one)",
            p.display()
        ),
        CheckoutsRoot::Unresolved => "checkouts root: none found (not standing inside a git repo or a THOR-marked \
             directory, and no parent to fall back to) - decay and crowding were NOT measured; pass --checkouts \
             <dir> (a directory whose immediate subdirectories are checkouts) to measure them"
            .to_string(),
    }
}

/// What has quietly rotted since the last time anyone looked: anchors that
/// resolve to nothing, and proofs that now come out false.
///
/// WHY THIS LINE EXISTS. A memory decays between writes, not during them.
/// The write gate can refuse a bad declaration and the neighbourhood toll
/// can refuse a write next to rot, but neither ever fires while a file is
/// simply moved or deleted by ordinary work. On 2026-08-06 that had left
/// 128 anchors in four projects pointing at nothing: those facts fired
/// NOWHERE, and no surface said so. Repairing them took a full sweep. The
/// point of counting it here is that the next hundred announce themselves
/// instead of waiting for someone to go looking.
///
/// Needs `--checkouts`, for the same reason `orphan_projects_line` does: an
/// anchor is relative to a project's own working copy, and this refuses to
/// guess where that is.
/// One rotted item, NAMED.
///
/// THE DEFECT THIS CLOSES, reported by an agent on 2026-08-14 after it went
/// looking. This check knew exactly which item was broken - it holds the id
/// in its own loop - and returned a number. The line then said "run doctor to
/// see them", and doctor said "8". Nothing anywhere could turn that 8 into a
/// list: not `audit` (counts), not `anchorprobe` (wants the file you are
/// trying to find), not the MCP tools (they work per id, and the id is what
/// you do not have). The only route left was reading the SQLite store by
/// hand, around the whole tool. A number that names a problem and hides the
/// subject is not a report, it is a rumour.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rot {
    pub id: String,
    pub project: String,
    /// The anchor that points at nothing, or the proof that came out false.
    pub what: String,
}

/// The result of trying to check decay: either it could not be checked at
/// all (and why), or it was, and every rotted item is named. The one place
/// that computes decay - shared by `decay_line` (prose, every project under
/// `checkouts`) and `gate_verdict` (pass/fail, optionally narrowed to one
/// project).
enum DecayCheck {
    StoreUnreadable,
    NoCheckouts,
    Counted { dead: Vec<Rot>, failing: Vec<Rot>, judged: usize },
}

/// How many rotted items the decay line names before it stops. Enough that a
/// normal repair session needs no second surface, small enough that a sweep
/// of 128 does not bury every other line in the report - and whatever it
/// holds back, it says so out loud rather than reading as the whole list.
const NAMES_AT_MOST: usize = 20;

/// How many to name: the readable default, or every one of them when the
/// caller asked for the whole list (`doctor --full`) - a cleanup needs all of
/// them, and reading the store by hand to get them is the detour this report
/// exists to remove.
fn name_cap(full: bool) -> usize {
    if full { usize::MAX } else { NAMES_AT_MOST }
}

/// The reason `id`'s dispute was written with, in words for a doctor line:
/// the writer's own `because`, or "no reason recorded" for every dispute from
/// before one was required.
fn dispute_reason_in_words(store: &EventStore, id: &str) -> String {
    match model::store::current_dispute_reason(store, id) {
        Some(reason) => reason.split_whitespace().collect::<Vec<_>>().join(" "),
        None => "no reason recorded".to_string(),
    }
}

/// The rotted items as text, one per line, indented under their own line.
fn name_rot(label: &str, rot: &[Rot], full: bool) -> Vec<String> {
    let cap = name_cap(full);
    let mut out = Vec::new();
    for r in rot.iter().take(cap) {
        out.push(format!("  {label}: {} [{}] {}", r.id, r.project, r.what));
    }
    if rot.len() > cap {
        out.push(format!("  {label}: and {} more, not named here (use --full)", rot.len() - cap));
    }
    out
}

/// The `seq` of the earliest event on record for `id` - the closest thing to
/// an age this store can answer, since no event carries a wall-clock
/// timestamp of its own (`thor_core::event_store::Event`'s own fields).
/// `get_events_by_entity` orders its rows `ORDER BY seq`, oldest first (the
/// same fact `model::served::last_trigger`'s own doc comment already relies
/// on), so the first row back is the item's own declaration - the point this
/// function exists to find, without adding a timestamp column nothing else
/// in this store has ever needed.
///
/// Fails open to `i64::MAX` on anything that goes wrong (a broken log, an id
/// with no events on record) - the same fail-open reasoning `model::served::
/// served_count` already gives for the identical query: the only thing this
/// number ever feeds is the ORDER of an advisory list, never a refusal, and
/// sorting a lookup failure to the very end is safer than guessing it is the
/// oldest thing in the store.
fn first_seq(store: &EventStore, id: &str) -> i64 {
    store.get_events_by_entity(id).ok().and_then(|events| events.first().map(|e| e.seq)).unwrap_or(i64::MAX)
}

/// For every live, fireable item whose project resolves to a checkout under
/// `checkouts`, does its anchor exist and does its proof still hold.
///
/// `project_filter`, when given, skips every item whose project does not
/// match it - this is what lets the gate narrow decay to one repository's
/// own items, so that repository cannot be failed by another checkout's rot
/// under the same `--checkouts` directory. `decay_line` always passes
/// `None`, so its own output is unchanged by this function existing:
/// every project's rot, exactly as before this was split out.
fn decay_check(db: &Path, checkouts: Option<&Path>, project_filter: Option<&str>) -> DecayCheck {
    let Ok(store) = EventStore::open_existing(db) else {
        return DecayCheck::StoreUnreadable;
    };
    let Some(root) = checkouts else {
        return DecayCheck::NoCheckouts;
    };

    let roots = checkout_roots(&store, root);
    // A project can have two folders that both resolve to it (an outer folder
    // with the marker, and a repository nested inside it that is the
    // registered working copy), and a session stands in either. An anchor or
    // a proof that holds in the folder the scan found is not decay, even when
    // the registered working copy is the other one (measured 2026-10-07: two
    // anchors written relative to the outer folder fire from it, and read as
    // dead against the nested repository alone).
    let scanned = scanned_roots(root);

    let items = serve::live::live_items(&store);
    let (mut dead, mut failing, mut judged) = (Vec::new(), Vec::new(), 0usize);
    for li in items.iter().filter(|li| li.item.kind.can_fire()) {
        let Some(project) = li.item.project.as_deref() else { continue };
        if let Some(only) = project_filter {
            if only != project {
                continue;
            }
        }
        let Some(base) = roots.get(project) else { continue };
        // An anchor on something DELIBERATELY absent is not decay. A rule
        // guarding a gitignored secrets file is anchored there exactly so it
        // fires the moment that file appears; counting it as rot is what got
        // six of them swept into the archive on 2026-08-07.
        if li.item.tags.iter().any(|t| t == model::store::DELIBERATE_ANCHOR_TAG) {
            continue;
        }
        judged += 1;
        for binding in &li.item.bindings {
            // Path AND Dir. A fact anchored to a directory that no longer
            // exists fires exactly as nowhere as one anchored to a deleted
            // file, and this loop counted only the first kind: reported by a
            // session on 2026-08-14 that had a dead Dir anchor in front of it
            // while this line said zero. `proof_line`'s location arm above
            // already treats the two kinds as one thing; this is the same
            // pair, and leaving Dir out here made "no decay" mean "no decay
            // among the anchors I happen to look at".
            if let model::item::Binding::Target {
                kind: model::item::TargetKind::Path | model::item::TargetKind::Dir,
                value,
            } = binding
            {
                // An absolute path is somewhere else on this machine, not in
                // the checkout, so it is not this line's business.
                if value.contains(':') || value.starts_with('/') || value.starts_with("\\\\") {
                    continue;
                }
                let relative = value.replace('\\', "/");
                let alive_in_the_scanned_folder = scanned.get(project).is_some_and(|alt| alt.join(&relative).exists());
                if !base.join(&relative).exists() && !alive_in_the_scanned_folder {
                    dead.push(Rot {
                        id: li.id.clone(),
                        project: project.to_string(),
                        what: value.clone(),
                    });
                }
            }
        }
        if let Some(check) = li.item.check.as_ref() {
            let fails_in_the_scanned_folder = scanned
                .get(project)
                .is_none_or(|alt| alt == base || model::check::run(check, alt) == model::check::Outcome::Fails);
            if model::check::run(check, base) == model::check::Outcome::Fails && fails_in_the_scanned_folder {
                failing.push(Rot {
                    id: li.id.clone(),
                    project: project.to_string(),
                    // The check's own Debug form. Deliberately not a hand-written
                    // sentence per kind: a description that has to be kept in
                    // step with the enum is a description that goes stale, and
                    // this one only has to say enough to find the thing.
                    what: format!("{check:?}"),
                });
            }
        }
    }
    DecayCheck::Counted { dead, failing, judged }
}

pub fn decay_line(db: &Path, checkouts: Option<&Path>, full: bool) -> String {
    match decay_check(db, checkouts, None) {
        DecayCheck::StoreUnreadable => "decay: store unreadable, not checked".to_string(),
        DecayCheck::NoCheckouts => {
            "decay: pass --checkouts <dir> to check anchors and proofs against real working copies".to_string()
        }
        DecayCheck::Counted { judged: 0, .. } => {
            "decay: no project under --checkouts answers to any item's key, nothing checked".to_string()
        }
        // Both live arms say WHOSE rot this is. The session-start notice
        // counts the repository you are standing in and this counts every
        // project under --checkouts, so the two numbers legitimately differ -
        // and until 2026-08-14 neither line said which it was, which read as
        // the tool contradicting itself.
        DecayCheck::Counted { dead, failing, judged } if dead.is_empty() && failing.is_empty() => {
            format!("decay: none - across every project under --checkouts, every anchor of {judged} project-scoped item(s) resolves and every proof holds")
        }
        DecayCheck::Counted { dead, failing, judged } => {
            let mut out = vec![format!(
                "decay: {} anchor(s) point at nothing (those facts fire NOWHERE) and {} proof(s) now come out FALSE, across {judged} project-scoped item(s) in every project under --checkouts - each one named below; repair with revise (the file moved) or retract (the fact went with it)",
                dead.len(),
                failing.len()
            )];
            out.extend(name_rot("dead anchor", &dead, full));
            out.extend(name_rot("false proof", &failing, full));
            out.join("\n")
        }
    }
}

/// How many items have been put in front of a reader over and over without
/// ever being judged either way.
///
/// WHY THIS IS A COUNT AND NOT A REFUSAL. Judging a served fact costs the
/// session that does it and pays only the next one, so it does not happen -
/// on this store, in its whole life, exactly four judgements had ever been
/// recorded, all on one day. The obvious fix is a toll, the way the write
/// gate tolls a write next to rot. It does not fit here: a toll needs
/// something local to hold hostage, and the worst offenders are the pinned
/// rules that fire at every session start with no target at all. The other
/// candidate, refusing at the end of a turn, is the capture guard's shape -
/// measured on a blind hold-out at 55% catch and 11.4% false blocks, and
/// recorded as not deployable.
///
/// So this does the one thing that demonstrably moved the other number:
/// count it where someone will see it. Proof coverage sat at 3 of 3004
/// until a line here said so out loud, and it is 246 a day later.
pub fn unjudged_line(db: &Path) -> String {
    const HEAVY: usize = 40;
    let Ok(store) = EventStore::open_existing(db) else {
        return "unjudged: store unreadable, not checked".to_string();
    };
    let Ok(events) = store.event_kinds() else {
        return "unjudged: the log could not be read, not checked".to_string();
    };
    use thor_core::event_store::EventKind;
    let mut served: std::collections::HashMap<String, usize> = Default::default();
    let mut judged: std::collections::HashSet<String> = Default::default();
    for (kind, id) in events {
        match kind {
            EventKind::ItemServed => *served.entry(id).or_default() += 1,
            EventKind::ItemMarkedUseful | EventKind::ItemMarkedNoise => {
                judged.insert(id);
            }
            _ => {}
        }
    }
    // Pinned items are excluded here for the same reason the Stop-time ask
    // excludes them: "did it belong where it fired" is already answered by
    // the owner having pinned it. Counting them would make this line
    // disagree with the mechanism it exists to report on.
    let pinned: std::collections::HashSet<String> = serve::live::live_items(&store)
        .into_iter()
        .filter(|li| li.item.bindings.iter().any(|b| matches!(b, model::item::Binding::Always)))
        .map(|li| li.id)
        .collect();
    // ONLY WHAT IS STILL LIVE, the same rule the Stop-time ask already
    // applies. These counts are folded from event kinds, which keep every
    // serving an item ever had - including the ones it had before somebody
    // retracted it. Counting those made doctor name four items owed a
    // judgement that were all retracted (2026-08-19), and marking one of them
    // moved the number: a debt payable with the dead is not a debt.
    let alive: std::collections::HashSet<String> =
        serve::live::live_items(&store).into_iter().map(|li| li.id).collect();
    let owed = |id: &String| alive.contains(id) && !judged.contains(id) && !pinned.contains(id);
    let heavy = served.iter().filter(|(id, n)| **n >= HEAVY && owed(*id)).count();
    let total_unjudged = served.keys().filter(|id| owed(*id)).count();
    if total_unjudged == 0 {
        return "unjudged: every item that has ever fired has been judged at least once".to_string();
    }
    format!(
        "unjudged: {heavy} trigger-bound item(s) fired {HEAVY}+ times and were never judged either way ({total_unjudged} unjudged in total, pinned items excluded - the owner answered that question by pinning them) - `mark` is the only thing that ever retires noise, and silence decides nothing"
    )
}

/// One binding, rendered as short plain text for a report line - never the
/// enum's own `{:?}` spelling, which would leak Rust syntax into a line meant
/// for a person: the raw value for a path, a command, or any other `Target`
/// kind, `moment <name>` for a `Moment`, and `Always` for the pinned layer.
/// Same three-way shape `bin/anchorprobe.rs`'s own `list_armed` already
/// renders a binding as (that binary is untouched by this change; the shape
/// is worth keeping identical for a reader who has seen either report, not
/// worth sharing code over one match expression).
fn binding_short(binding: &model::item::Binding) -> String {
    match binding {
        model::item::Binding::Always => "Always".to_string(),
        model::item::Binding::Moment(action) => format!("moment {}", action.as_str()),
        model::item::Binding::Target { value, .. } => value.clone(),
    }
}

/// Every binding an item carries, short and joined - WHERE a named
/// judgement-debt item fires, so the owner can judge "did it belong where it
/// fired" without opening the store. `+`-joined, the same joiner
/// `anchorprobe`'s `list_armed` already uses for the identical list; "unbound"
/// is defensive only - `rank::eligible` never selects a bindingless item, so
/// `served_since_last_verdict` should never hand `judgement_debt_named` one.
fn bindings_short(bindings: &[model::item::Binding]) -> String {
    if bindings.is_empty() {
        return "unbound".to_string();
    }
    bindings.iter().map(binding_short).collect::<Vec<_>>().join(" + ")
}

/// One sentence naming every OTHER actor's own firings (`serve::usefulness::
/// served_to_other_actors`) - up to three, busiest first, ties broken by
/// name for a deterministic order across runs (the same rule `judgement_
/// debt_named`'s own sort already uses), with a trailing "and N more" for
/// any beyond that - so a program driving this boundary itself (see `serve::
/// usefulness::ASSISTANT_DELIVERY_ACTOR`'s own doc comment for why only an
/// assistant's own firings ever become debt) stays VISIBLE in `doctor`
/// instead of silently vanishing the moment its firings stopped counting
/// toward the judgement debt. Kept to exactly ONE sentence, however many
/// actors a store has ever recorded - this must never grow into a second
/// named list the way the debt items above already have one.
fn other_actors_sentence(counts: &std::collections::HashMap<String, usize>) -> String {
    let mut actors: Vec<(&str, usize)> = counts.iter().map(|(a, n)| (a.as_str(), *n)).collect();
    actors.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    const NAMED: usize = 3;
    let named: Vec<String> = actors.iter().take(NAMED).map(|(actor, n)| format!("'{actor}' ({n}x)")).collect();
    let more = actors.len().saturating_sub(NAMED);
    let tail = if more > 0 { format!(", and {more} more actor(s)") } else { String::new() };
    format!(
        "{}{} also had items served here - not counted as debt, because no session was ever there to judge those firings",
        named.join(", "),
        tail
    )
}

/// Component: the judgement debt's own two-number backlog - store-wide, and
/// how much of it belongs to the checkout this run stands in - built on the
/// exact same fold the Stop hook's own `judgement_debt` acts on
/// (`serve::usefulness::judgement_debt_counts`), so this line can never
/// silently disagree with the mechanism it reports on - including about
/// pinned items, which both this line and `unjudged_line` above now exclude
/// (see `usefulness::owed_items`'s own doc comment for the brief 2026-09-08
/// to 2026-09-12 window where the two disagreed about that, and why).
///
/// A SEPARATE LINE, ADDED RATHER THAN REPLACING `unjudged_line`: the two
/// still differ in scope (store-wide plus this checkout's own share, versus
/// a flat total) and nothing here removes either capability - it only adds
/// the number `doctor` had no way to report before today.
///
/// WHY THIS EXISTS AT ALL, recorded as `oordeelschuld-komt-binnen-de-sessie-
/// terug` (2026-09-08). The Stop hook itself now floors each item to at most
/// one ask per session and only ever asks about what the CURRENT session and
/// its own project actually saw fire - correctly quieter, but that also
/// means no single session is ever shown the size of the whole backlog
/// again. `doctor` runs outside any one session, so it is the one place left
/// that can still say how large that backlog is - store-wide, and for this
/// checkout specifically - without re-introducing the nagging the session
/// floor exists to stop.
///
/// NAMED, not just counted, since 2026-09-12 - `doctor` counted this backlog
/// from the day above, but never listed it, so the owner's end-of-session
/// evaluation had a number and nothing to walk: neither `audit` nor the MCP
/// tools can turn "35 owed" into which 35, the same gap `Rot` above was built
/// to close for decay. `serve::usefulness::judgement_debt_named` names every
/// item that applies to this checkout, sorted by count descending; this line
/// prints them exactly the way `decay_line`'s "dead anchor"/"false proof" and
/// `crowding_line`'s "outranked" lists already do - `NAMES_AT_MOST` by
/// default, all of them with `--full` (`name_cap`, shared, not a second cap).
/// An item owed only to another project is never named here at all (see
/// `judgement_debt_named`'s own doc comment); that is why the named lines
/// always total exactly `in_project`, never `total`.
///
/// SILENT AT ZERO, the same convention as `ship_line` above: a clean backlog
/// is not a finding, and a permanent "0 owed" line on every quiet run would
/// be exactly the noise that trains a reader to stop reading this report.
///
/// CARRIES THE EVALUATION DEBT'S OWN SIGNALS TOO, since 2026-09-12 (the
/// trigger rewritten four times, three of them on 2026-09-16 - see
/// `serve::usefulness`'s own "evaluation debt" section for why), appended to
/// this same first line rather than a separate one: the NEWEST live
/// evaluation-report Report this store holds for this project - read fresh
/// from the store itself, not only from the sidecar, so a report no Stop
/// has looked at yet is still named (see `serve::usefulness::newest_
/// evaluation_report`'s own doc comment for why the sidecar alone cannot
/// answer this) - and the day it was first seen; whether the last one still
/// COVERS this project (a rolling `serve::usefulness::
/// EVAL_REPORT_COVERS_HOURS`-hour window, `serve::usefulness::
/// eval_report_covers` - replaced the UTC calendar day 2026-09-17, seventh
/// rewrite, `serve::usefulness`'s own "evaluation debt" section, for the
/// measured case a calendar day got wrong); and, only when it does NOT, how
/// many times this project has been asked since its last report, since
/// when, and that the Stop hook blocks every turn once a session has
/// accrued an hour of work here. That last note is necessarily approximate
/// about the accrued-work half: `doctor` runs cold, outside any session, so
/// it can name whether a covering report exists but never with full
/// certainty how long any PARTICULAR session has worked here right now
/// (`serve::usefulness::EVAL_FIRST_WORK_MINUTES`/`EVAL_REPEAT_WORK_
/// MINUTES`) - only a real Stop, inside a real session, can ever know that
/// for certain. Since 2026-09-17 (fifth rewrite, `serve::usefulness`'s own
/// "evaluation debt" section) this line also names the MOST RECENTLY ACTIVE
/// session's own accrued total from the sidecar (`serve::usefulness::
/// most_recently_active_session`) and when it is next due - the closest a
/// cold read gets to an honest answer, and explicitly named as coming from
/// the sidecar rather than a live measurement. Also names, once a covering
/// report already exists, that a REPEAT evaluation follows after
/// (no longer: part G dropped the repeat) more of accrued work since it, rather than
/// staying silent for the rest of the window. READ-ONLY: this reads the
/// evaluation debt's own sidecar (`serve::usefulness::project_eval_state`)
/// but, unlike the Stop hook's own `update_eval_debt_state`/`record_eval_
/// debt_asked`, never writes it - a diagnostic that mutated state on every
/// run would make the two facts depend on whichever tool happened to run
/// last. Built on the exact same sidecar fields the Stop hook acts on
/// (`bin/serve.rs`'s `evaluation_debt`), so this can never silently
/// disagree with what it reports on - the same reasoning this whole line
/// already exists for (see this doc comment's own history above).
///
/// NEVER NAMES ANY OF THIS FOR A CHECKOUT WITH NO PROJECT (`checkout_
/// project: None`), since 2026-09-16 (`no evaluation is asked outside a
/// project, where no report can be filed`): the Stop hook itself never asks
/// for the evaluation there any more - see `serve::usefulness`'s own
/// "evaluation debt" section, third rewrite, for why - so a report clause
/// or an ask count here would describe an obligation that can no longer
/// fire. This line says so in one clause instead, and never reads
/// `project_eval_state`/`eval_filing::newest_valid_evaluation` at all for `None`:
/// correct either way, since a no-project checkout's own sidecar entry is
/// inert leftover state at best (see `update_eval_debt_state`'s own doc
/// comment), but reading it anyway would invite exactly the drift this
/// whole line exists to avoid.
///
/// COUNTS ONLY WHAT AN ASSISTANT WAS SERVED, since 2026-09-19 - for free,
/// from `judgement_debt_counts` itself (built on `serve::usefulness::
/// served_since_last_verdict`, which now filters by `serve::usefulness::
/// ASSISTANT_DELIVERY_ACTOR`; see that constant's own doc comment for why).
/// Nothing here duplicates that filter. When `serve::usefulness::served_to_
/// other_actors` is not empty, ONE extra sentence names each other actor and
/// its own firing count (`other_actors_sentence` above) and says plainly
/// that none of it is judgement debt - a program driving this boundary on
/// its own must stay VISIBLE, never silently vanish, but must also never be
/// asked to answer for a firing no assistant session was ever there to
/// judge. This can make the line speak even when `total` is zero (nothing an
/// assistant owes, but another actor has been busy) - staying silent then
/// would be exactly the vanishing act this whole mechanism exists to
/// prevent.
pub fn judgement_debt_line(db: &Path, checkout_project: Option<&str>, full: bool) -> Option<String> {
    let store = EventStore::open_existing(db).ok()?;
    let (total, in_project) = serve::usefulness::judgement_debt_counts(&store, checkout_project);
    let other_actors = serve::usefulness::served_to_other_actors(&store);
    // D9 (step 12): a verdict counts toward the debt only once a Stop has
    // verified it was made through an attested THOR call. The rest are
    // written and kept, and named here, so a verdict that cleared nothing is
    // never silent.
    let outside = match serve::usefulness::unattested_verdicts(&store) {
        0 => None,
        n => Some(format!(
            "judgement debt: {n} verdicts outside an attested THOR call - they clear no debt (a verdict clears it only \
             when it is made with THOR's mark tool inside the Claude Code session the item fired in)"
        )),
    };
    if total == 0 {
        let mut lines: Vec<String> = Vec::new();
        if !other_actors.is_empty() {
            lines.push(format!("judgement debt: {}", other_actors_sentence(&other_actors)));
        }
        lines.extend(outside);
        return (!lines.is_empty()).then(|| lines.join("\n"));
    }
    let threshold = serve::usefulness::JUDGEMENT_DEBT_AFTER;
    let mut problem_lines: Vec<String> = Vec::new();
    let mut out = vec![match checkout_project {
        Some(project) => format!(
            "judgement debt: {total} item(s) store-wide are owed a verdict (fired in {threshold}+ sessions since \
             the last one, or never judged at all; pinned items excluded - the owner answered that question \
             by pinning them) - {in_project} of them apply to this checkout ('{project}', or global); they are \
             never asked at the end of a turn, and an evaluation asks about one only when the evaluating session saw \
             it fire - so an item that never fires there stays owed until it is settled with mark or retract; the \
             longest owed are named below, each with its sessions since its last verdict"
        ),
        None => format!(
            "judgement debt: {total} item(s) store-wide are owed a verdict (fired in {threshold}+ sessions since \
             the last one, or never judged at all; pinned items excluded - the owner answered that question \
             by pinning them) - this checkout resolves to no project, so only the {in_project} global one(s) \
             among them could ever be asked about here; they are never asked at the end of a turn, and an \
             evaluation asks about one only when the evaluating session saw it fire - so an item that never fires \
             there stays owed until it is settled with mark or retract; the longest owed are named below, each \
             with its sessions since its last verdict"
        ),
    }];
    match checkout_project {
        Some(_) => {
            let state = serve::usefulness::project_eval_state(db, checkout_project);
            let now = serve::time::now_unix();
            // THE VALIDATOR'S ANSWER, read from the log (step 16): only a
            // report whose filing THOR checked and found valid is this
            // project's evaluation; one merely on file is named below with
            // the reason it does not count.
            let valid = serve::eval_filing::newest_valid_evaluation(&store, checkout_project);
            let filed = valid.as_ref().and_then(|v| v.filed_at.as_deref()).and_then(serve::eval_filing::unix_of_timestamp);
            let report_clause = match &valid {
                None => "no evaluation report of this project counts (none on file passed THOR's check)".to_string(),
                Some(valid) => format!(
                    "{}, filed {} day(s) ago",
                    valid_report_words(valid),
                    serve::usefulness::days_ago(now, filed.unwrap_or(now))
                ),
            };
            out[0].push_str(&format!(" - {report_clause}"));
            if let Some(project) = checkout_project {
                problem_lines = report_problem_lines(&store, project);
            }
            // From the log too - the same time the Stop hook overwrites the
            // sidecar's copy with, so doctor never reads a hand-edited one.
            let cadence = serve::usefulness::cadence_in(db);
            let rot = cadence == serve::usefulness::EvalCadence::WeeklyUnlessRot
                && checkout_project.is_some_and(|p| serve::eval_filing::project_rot(&store, p).is_some());
            let window_hours = serve::usefulness::covers_hours(cadence, rot);
            let covered = serve::usefulness::eval_report_covers(filed, now, window_hours);
            if covered {
                out[0].push_str(&format!(
                    "; the last evaluation still covers this project for {} day(s) - nothing more is asked before then",
                    window_hours / 24
                ));
            } else {
                let ask_clause = match state.first_asked_since_report {
                    Some(first) => {
                        format!("asked {} time(s), the first {} day(s) ago", state.asked_count, serve::usefulness::days_ago(now, first))
                    }
                    None => "not asked yet".to_string(),
                };
                out[0].push_str(&format!(
                    "; no evaluation in the last {} day(s) - {ask_clause}; the Stop hook blocks every turn once a session \
                     has accrued an hour of work here, until the report is filed",
                    window_hours / 24
                ));
            }
            // THE MOST RECENTLY ACTIVE SESSION'S OWN ACCRUAL, since the fifth
            // rewrite (2026-09-17, `serve::usefulness`'s own "evaluation
            // debt" section): `doctor` has no session of its own to measure
            // "how long has THIS session worked here" the way a real Stop
            // can, but the sidecar now carries every session's own accrued
            // total (`ProjectEvalState::sessions`), so the most recently
            // active one - by `last_event_unix` - is the closest cold read
            // gets to an honest answer, named explicitly as an
            // approximation rather than silently guessed at. Silent when the
            // sidecar holds no session yet for this project (a fresh
            // sidecar, or one written before this field existed) - there is
            // nothing to name, not an error.
            if let Some((session_id, work)) = serve::usefulness::most_recently_active_session(&state) {
                let accrued_minutes = work.accrued_secs / 60;
                let threshold = serve::usefulness::EVAL_FIRST_WORK_MINUTES;
                // Since step 17 the PROJECT's clock decides (every session
                // together); it is never below any one session's own.
                let project_minutes = (state.work.accrued_secs / 60).max(accrued_minutes);
                let due_clause = match (threshold - project_minutes).max(0) {
                    0 => "already due".to_string(),
                    remaining => format!("due after {remaining} more minute(s) of accrued work"),
                };
                // THE REPEAT'S OWN RISK (sixth rewrite, 2026-09-17 - see
                // `serve::usefulness`'s own "the repeat's own risk" doc
                // comment): named here too, alongside the accrued minutes, so
                // a cold read of `doctor` sees exactly what a live Stop would
                // gate a repeat on - the untested-edit count since the last
                // test or build run, and whether a context summary has
                // happened since this session's own last reset.
                let week_clause = if covered { " once the week is up" } else { "" };
                let summary_clause =
                    if work.compacted_since_anchor { "a context summary has happened" } else { "no context summary yet" };
                out[0].push_str(&format!(
                    "; its most recently active session ('{session_id}') has accrued {accrued_minutes} minute(s) of work \
                     here since its last reset, the project {project_minutes} minute(s) with every session together - \
                     {due_clause}{week_clause}, with {} untested edit(s) since the last test or build run and \
                     {summary_clause}",
                    work.edits_since_test
                ));
            }
        }
        // No report clause, no ask clause: a checkout with no project is
        // never asked for the evaluation at all (see this function's own
        // doc comment), so naming either would describe an obligation that
        // can never fire here.
        None => {
            out[0].push_str("; the evaluation is only ever asked inside a project, so this checkout is never asked for one");
        }
    }
    out.extend(problem_lines);
    let named = serve::usefulness::judgement_debt_named(&store, checkout_project);
    let cap = name_cap(full);
    for item in named.iter().take(cap) {
        // ", last fired at '<trigger>'" only when the newest serving on
        // record actually carried one - see `JudgementDebtItem::last_
        // trigger`'s own doc comment for the three cases that answer `None`.
        // Without this an evaluation could see WHERE an item was BOUND but
        // not where among that binding's own reach it had actually been
        // firing - measured 2026-09-17 (acme-shop eval 1 and 2): five
        // owed items were skipped outright for exactly that gap.
        let last_fired = match &item.last_trigger {
            Some(trigger) => {
                // The log keeps a longer place than a doctor line shows.
                let anchors: Vec<String> = item
                    .bindings
                    .iter()
                    .filter_map(|b| match b {
                        model::item::Binding::Target { value, .. } => Some(value.clone()),
                        _ => None,
                    })
                    .collect();
                format!(", last fired at '{}'", model::store::trigger_excerpt(trigger, &anchors))
            }
            None => String::new(),
        };
        out.push(format!(
            "  judgement debt: {} ({}x, {:?}) at {}{last_fired}",
            item.id,
            item.count,
            item.kind,
            bindings_short(&item.bindings)
        ));
    }
    if named.len() > cap {
        out.push(format!("  judgement debt: and {} more, not named here", named.len() - cap));
    }
    if !other_actors.is_empty() {
        out.push(format!("  judgement debt: {}", other_actors_sentence(&other_actors)));
    }
    if let Some(outside) = outside {
        out.push(format!("  {outside}"));
    }
    Some(out.join("\n"))
}

/// "newest evaluation report '<id>' (valid, filed by session <S>)", with the
/// subagent that filed it when one did - the one wording doctor names a
/// report that counts by.
fn valid_report_words(valid: &model::store::EvaluationValidation) -> String {
    let agent = valid.agent.as_deref().map(|a| format!(", agent {a}")).unwrap_or_default();
    format!("newest evaluation report '{}' (valid, filed by session {}{agent})", valid.report, valid.session)
}

/// One line per evaluation report of `project` that is on file but does not
/// count, with the validator's reason, and per report no Stop has checked
/// yet - so a report that silences nothing is never silent itself.
fn report_problem_lines(store: &EventStore, project: &str) -> Vec<String> {
    serve::eval_filing::reports_of_project(store, project)
        .into_iter()
        .filter_map(|(id, _, validation)| match validation {
            Some(v) if v.valid => None,
            Some(v) => Some(format!(
                "  evaluation report '{id}' does not count as this project's evaluation: {}",
                serve::eval_filing::why_in_words(store, &v)
            )),
            None => Some(format!(
                "  evaluation report '{id}' is not checked yet - the next Stop of a main session checks its filing"
            )),
        })
        .collect()
}

/// THE EVALUATIONS OF THIS CHECKOUT'S PROJECT (step 16, B7): the newest one
/// that counts, then the last five valid ones, each with how its verdicts
/// split - "eval-<id>: <u> useful, <n> noise, <s> skipped (<p>% useful)" -
/// and a flag on one whose ten or more verdicts all went one way (a blind
/// pass looks exactly like that). Every report on file that does not count
/// is named with its reason, and the format-drift line when the newest
/// check found the transcript unreadable (D14). Silent outside a project and
/// for a project with no report and no record at all.
pub fn evaluations_line(db: &Path, project: Option<&str>) -> Option<String> {
    let project = project?;
    let store = EventStore::open_existing(db).ok()?;
    let mut valid: Vec<model::store::EvaluationValidation> = model::store::evaluation_validations(&store)
        .into_iter()
        .map(|(_, v)| v)
        .filter(|v| v.valid && v.project.as_deref() == Some(project))
        .collect();
    valid.sort_by(|a, b| b.report_seq.cmp(&a.report_seq));
    let problems = report_problem_lines(&store, project);
    let drift = serve::eval_filing::drift_recorded(&store);
    if valid.is_empty() && problems.is_empty() && !drift {
        return None;
    }
    let mut out = vec![match serve::eval_filing::newest_valid_evaluation(&store, Some(project)) {
        Some(newest) => format!(
            "evaluations: {} - the last {} valid evaluation(s) of '{project}', newest first:",
            valid_report_words(&newest),
            valid.len().min(5)
        ),
        None => format!(
            "evaluations: no evaluation report of '{project}' counts - a report counts only once THOR checked its filing \
             call against the session's transcript and the log"
        ),
    }];
    for v in valid.iter().take(5) {
        let flag = if serve::eval_filing::went_one_way(v) {
            " - every verdict went one way - a blind pass looks exactly like this; nothing here can tell whether the items \
             were read"
        } else {
            ""
        };
        out.push(format!("  {}{flag}", serve::eval_filing::share_line(v)));
    }
    out.extend(problems);
    if drift {
        out.push(format!("  {}", serve::eval_filing::DRIFT_LINE));
    }
    Some(out.join("\n"))
}

/// THE CADENCE (step 17): which rule decides when a project owes its
/// evaluation - a code constant, named on every doctor run so a flip can
/// never go unseen.
pub fn cadence_line(db: &Path) -> String {
    if !serve::plan::weekly_check_on(db) {
        return format!("{} - switched off in the first-session questions", serve::usefulness::cadence_clause(serve::usefulness::EvalCadence::Off));
    }
    let (cadence, how) = match serve::plan::read(db) {
        Some((plan, source)) => (serve::usefulness::cadence_for(plan), format!("plan {} ({})", plan.word(), source.word())),
        None => (serve::usefulness::cadence_for(serve::plan::Plan::Small), "no plan on record, so the cheap weekly rhythm".to_string()),
    };
    format!("{} - {how}", serve::usefulness::cadence_clause(cadence))
}

/// How many minutes the work-clock sidecar and the transcripts may disagree
/// before doctor names it (step 17).
pub const WORK_CLOCK_TOLERANCE_MINUTES: i64 = 15;

/// At most this many sessions' transcripts are read for the cross-check.
const WORK_CLOCK_SESSIONS_READ: usize = 40;

/// THE WORK CLOCK CROSS-CHECK (step 17, N3): the project's clock in the
/// sidecar against what the transcripts of the project's sessions say, since
/// its newest valid evaluation (or the last `EVAL_FLOOR_DAYS` days, whichever
/// is later) - the sessions the sidecar holds for the project and the Claude
/// Code sessions the log shows working in it, each transcript read to its
/// tail, parallel minutes counted once. Silent unless the two differ by more
/// than `WORK_CLOCK_TOLERANCE_MINUTES`: a sidecar any local process can write
/// is named when it has drifted from what really happened.
pub fn work_clock_line(db: &Path, project: Option<&str>) -> Option<String> {
    work_clock_line_in(db, project, serve::attest::transcripts_root().as_deref())
}

/// `work_clock_line` against the transcripts under `root`.
pub fn work_clock_line_in(db: &Path, project: Option<&str>, root: Option<&Path>) -> Option<String> {
    let project = project?;
    let root = root?;
    let store = EventStore::open_existing(db).ok()?;
    let state = serve::usefulness::project_eval_state(db, Some(project));
    let now = serve::time::now_unix();
    let filed = serve::eval_filing::newest_valid_evaluation(&store, Some(project))
        .and_then(|v| v.filed_at)
        .as_deref()
        .and_then(serve::eval_filing::unix_of_timestamp);
    let since = filed.unwrap_or(i64::MIN).max(now - serve::usefulness::EVAL_FLOOR_DAYS * 86400);
    let mut sessions: Vec<String> = state
        .sessions
        .iter()
        .filter(|(id, s)| serve::attest::is_uuid_shaped(id) && s.last_event_unix.is_some_and(|t| t >= since))
        .map(|(id, _)| id.clone())
        .collect();
    for id in serve::usefulness::project_activity(&store, project, since, false).1 {
        if !sessions.contains(&id) {
            sessions.push(id);
        }
    }
    let lines: Vec<serve::attest::TimedLine> = sessions
        .iter()
        .take(WORK_CLOCK_SESSIONS_READ)
        .flat_map(|id| serve::attest::session_timeline(root, id))
        .collect();
    if lines.is_empty() {
        return None;
    }
    let sidecar = state.work.accrued_secs / 60;
    let transcripts = serve::attest::minutes_in(&lines, project, Some(since));
    ((sidecar - transcripts).abs() > WORK_CLOCK_TOLERANCE_MINUTES).then(|| {
        format!(
            "work clock of '{project}': the work clock sidecar says {sidecar} min, the transcripts say {transcripts} min \
             (since its last valid evaluation, or the last {} days) - the sidecar is a file any local process can write; \
             the Stop hook takes the larger of the two, and the weekly floor reads the log only",
            serve::usefulness::EVAL_FLOOR_DAYS
        )
    })
}

/// THE OWNER QUEUE (step 16, B5, B6): per project, how many of THOR's own open
/// points wait on the owner - a subject a valid evaluation already filed past
/// while it held - against `model::store::OPEN_POINTS_OWNER_BUDGET`. Over the
/// budget the line opens with the same banner the owner's session start
/// shows. Nothing is refused over it. Silent when no point waits on him.
pub fn owner_queue_line(db: &Path) -> Option<String> {
    let store = EventStore::open_existing(db).ok()?;
    let mut projects: Vec<String> = serve::live::live_items(&store)
        .into_iter()
        .filter(|li| model::store::is_machine_point(&store, &li.id))
        .filter_map(|li| li.item.project)
        .collect();
    projects.sort();
    projects.dedup();
    let mut out: Vec<String> = Vec::new();
    for project in projects {
        let queue = model::store::owner_queue(&store, &project);
        if queue.is_empty() {
            continue;
        }
        out.extend(model::store::owner_queue_banner(&project, queue.len()));
        out.push(format!(
            "owner queue: {} of THOR's open points in '{project}' wait on the owner (budget {}) - a valid evaluation \
             was already filed while each one held, and no agent can lift it:",
            queue.len(),
            model::store::OPEN_POINTS_OWNER_BUDGET
        ));
        for (point, carriers) in &queue {
            out.push(format!("  {} - carried by {}", point.id, model::store::carriers_in_words(carriers)));
        }
    }
    // THOR's one store-wide point (the rules with no project, step 21):
    // counted once here, against no project's budget.
    for (point, carriers) in model::store::store_wide_owner_queue(&store) {
        out.push(format!(
            "owner queue, the whole memory: {} - one point for every project's rules with no project, waits on the \
             owner (counted once, in no project's budget) - carried by {}",
            point.id,
            model::store::carriers_in_words(&carriers)
        ));
    }
    (!out.is_empty()).then(|| out.join("\n"))
}

/// The old rules the next evaluation of this checkout's project starts with
/// (`model::store::evaluation_teeth_batch`, which reads
/// `model::store::settlement`): a PREVIEW of what THOR's machine pass will
/// try at that start (step 14) - it decides what it can itself, and only
/// what needs the agent lands in the evaluation's frozen batch. The ones
/// behind the first eight are named too, so nothing is hidden. Silent
/// outside a project (no report can be filed there) and when no rule is owed.
pub fn evaluation_batch_line(db: &Path, checkout_project: Option<&str>, full: bool) -> Option<String> {
    let project = checkout_project?;
    let store = EventStore::open_existing(db).ok()?;
    let batch = model::store::evaluation_teeth_batch(&store, Some(project));
    if batch.owed_now.is_empty() && batch.owed_later.is_empty() {
        return None;
    }
    let mut out = vec![format!(
        "evaluation batch: the next evaluation of '{project}' starts with {} rule(s) - its batch of old rules with \
         no current verdict, at most {} named here, the ones that fired since the previous report first, then the \
         ones written longest ago. THOR tries each one itself at that start (a rule, a note, or its own open point \
         when only the owner can release it) and hands the session only what needs a fix from it; {} were settled \
         since the previous report; {} more rule(s) with no current verdict from THOR's trial stay owed behind \
         these",
        batch.owed_now.len(),
        model::store::EVALUATION_TEETH_BATCH,
        batch.settled.len(),
        batch.owed_later.len()
    )];
    for (id, why) in &batch.owed_now {
        out.push(format!("  batch: {id} - {why}"));
    }
    let cap = name_cap(full);
    for (id, why) in batch.owed_later.iter().take(cap) {
        out.push(format!("  owed later: {id} - {why}"));
    }
    if batch.owed_later.len() > cap {
        out.push(format!("  owed later: and {} more, not named here (use --full)", batch.owed_later.len() - cap));
    }
    Some(out.join("\n"))
}

/// Every open point on the store, THOR's own apart from the legacy ones
/// (step 14). THOR writes and closes its own - one per subject a machine
/// blocker holds, each saying in its text what clears it. A LEGACY point is
/// one an agent filed before THOR wrote them itself: it settles nothing and
/// holds no filing, so each is listed here for the owner to decide about.
/// Silent when the store holds none.
pub fn open_points_line(db: &Path, full: bool) -> Option<String> {
    let store = EventStore::open_existing(db).ok()?;
    let mut machine: Vec<String> = Vec::new();
    let mut legacy: Vec<String> = Vec::new();
    let mut points: Vec<serve::live::LiveItem> = serve::live::live_items(&store)
        .into_iter()
        .filter(|li| li.item.kind == model::item::Kind::Report && li.item.tags.iter().any(|t| t == model::store::OPEN_POINT_TAG))
        .collect();
    points.sort_by(|a, b| a.id.cmp(&b.id));
    for li in points {
        let project = li.item.project.clone().unwrap_or_else(|| "global".to_string());
        if model::store::is_machine_point(&store, &li.id) {
            let tagged = |prefix: &str| li.item.tags.iter().find_map(|t| t.strip_prefix(prefix)).unwrap_or("?").to_string();
            machine.push(format!(
                "{} [{project}] {} - about {}",
                li.id,
                tagged(model::store::OPEN_POINT_BLOCKER_PREFIX),
                tagged(model::store::OPEN_POINT_ABOUT_PREFIX)
            ));
        } else {
            let text: String = li.item.text.chars().take(100).collect();
            let more = if li.item.text.chars().count() > 100 { "..." } else { "" };
            legacy.push(format!("{} [{project}] {text}{more}", li.id));
        }
    }
    if machine.is_empty() && legacy.is_empty() {
        return None;
    }
    let mut out = vec![format!(
        "open points: {} written by THOR itself (machine) - each holds rules only the owner can release, says in its own \
         text what clears it, and closes itself at the next evaluation start after that; {} legacy (filed by an agent \
         before THOR wrote them itself) - they settle nothing and hold no filing, and wait for the owner: retract each \
         with a reason once what it names is resolved",
        machine.len(),
        legacy.len()
    )];
    let cap = name_cap(full);
    for (label, list) in [("machine", &machine), ("legacy", &legacy)] {
        for line in list.iter().take(cap) {
            out.push(format!("  {label}: {line}"));
        }
        if list.len() > cap {
            out.push(format!("  {label}: and {} more, not named here (use --full)", list.len() - cap));
        }
    }
    Some(out.join("\n"))
}

/// Every live rule carrying an `answer-guard:<entry>` tag whose entry is not
/// in the response rulebook beside the store (`guard-response-rulebook.json`,
/// K14, Z8). The tag says "what this rule is about is enforced by that
/// rulebook entry"; when the entry does not exist, nothing enforces it
/// anywhere - and the tag settles nothing either way. Silent when every
/// named entry exists (or no rule carries the tag).
pub fn answer_guard_line(db: &Path) -> Option<String> {
    let store = EventStore::open_existing(db).ok()?;
    let rulebook = serve::respond::default_rulebook_path(db);
    let entries: Option<std::collections::HashSet<String>> = std::fs::read_to_string(&rulebook).ok().and_then(|text| {
        let parsed: serde_json::Value = serde_json::from_str(&text).ok()?;
        Some(
            parsed
                .as_array()?
                .iter()
                .filter_map(|entry| entry.get("id").and_then(|id| id.as_str()).map(str::to_string))
                .collect(),
        )
    });
    let mut missing: Vec<String> = Vec::new();
    let mut live = serve::live::live_items(&store);
    live.sort_by(|a, b| a.id.cmp(&b.id));
    for li in live.iter().filter(|li| li.item.kind.can_fire()) {
        for entry in li.item.tags.iter().filter_map(|t| t.strip_prefix(model::store::ANSWER_GUARD_TAG_PREFIX)) {
            let entry = entry.trim();
            if !entries.as_ref().is_some_and(|known| known.contains(entry)) {
                missing.push(format!("  {} - its answer-guard entry '{entry}' is not in the response rulebook", li.id));
            }
        }
    }
    if missing.is_empty() {
        return None;
    }
    let file = match entries {
        Some(_) => format!("{}", rulebook.display()),
        None => format!("{} - which does not exist or does not read as a rulebook", rulebook.display()),
    };
    let mut out = vec![format!(
        "answer-guard: {} rule(s) name a response-rulebook entry that is not there ({file}); the tag settles nothing, and \
         nothing enforces what it names - add the entry to the rulebook, or drop the tag",
        missing.len()
    )];
    out.extend(missing);
    Some(out.join("\n"))
}

fn short(hash: &str) -> &str {
    &hash[..hash.len().min(8)]
}

/// Component 6: which mode surface 4's meaning search (feature `semantic`)
/// runs in right now - the exact same line `serve status` prints (see
/// `serve::semantic_paths::mode_line`), so the two can never disagree about
/// what is active. Never opens the store: whether the mode is active or
/// falling back depends only on the binary's own compile flags and the
/// model directory, not on any particular store's content.
pub fn semantic_line(model_dir: Option<&Path>) -> String {
    serve::semantic_paths::mode_line(model_dir)
}

/// Component 6: project keys held by live items that NO checkout under
/// `checkouts` resolves to - items that can never reach a pushed surface
/// because nothing answers to their name.
///
/// WHY THIS LINE EXISTS. Three separate defects on 2026-08-03 had exactly
/// one shape: an item named a project that resolution could not produce.
/// The global tier stored as a project called "global"; 1.0's marker file
/// name that 2.0 never read; a linked worktree resolving to its own slug.
/// All three were invisible from every surface - a rule scoped out looks
/// exactly like a rule that never matched. This turns that class from
/// silent into a line anyone can read.
pub fn orphan_projects_line(db: &Path, checkouts: Option<&Path>) -> String {
    let Ok(store) = EventStore::open_existing(db) else {
        return "project keys: store unreadable, not checked".to_string();
    };
    let items = serve::live::live_items(&store);
    let held: Vec<String> = items
        .iter()
        .filter(|li| li.item.kind.can_fire())
        .filter_map(|li| li.item.project.clone())
        .collect();
    if held.is_empty() {
        return "project keys: every fireable item is global, nothing to check".to_string();
    }

    let Some(root) = checkouts else {
        let distinct: std::collections::BTreeSet<&str> = held.iter().map(String::as_str).collect();
        return format!(
            "project keys: {} distinct key(s) in use; pass --checkouts <dir> to check that a real checkout answers to each one",
            distinct.len()
        );
    };

    let mut resolvable: std::collections::BTreeSet<String> = Default::default();
    if let Ok(entries) = std::fs::read_dir(root) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if let Some(key) = serve::project::resolve_project(&path) {
                    resolvable.insert(key);
                }
            }
        }
    }
    let orphans = serve::project::orphaned_project_keys(held.iter().map(String::as_str), &resolvable);
    if orphans.is_empty() {
        return format!(
            "project keys: every key in use resolves to a checkout under {}",
            root.display()
        );
    }
    let detail: Vec<String> = orphans.iter().map(|(k, n)| format!("{k} ({n} item(s))")).collect();
    format!(
        "project keys: {} key(s) that no checkout under {} answers to, so their items only ever answer a lookup and never fire at a moment: {}",
        orphans.len(),
        root.display(),
        detail.join(", ")
    )
}

/// The working copy each project is registered at (`install.exe
/// --register-root`, or the per-project install), one line per project -
/// its folder, since when, where it was before a move, and what is wrong with
/// it when the registration can no longer be used - then one line naming the
/// projects that own rules but have no registered working copy yet, with the
/// command that registers one (always with `--db`, F7).
///
/// WHY DOCTOR SHOWS THIS. The first registration of a project is first-come
/// (N11): whoever runs the command first fixes the path. A registration that
/// nobody ever sees is one nobody can correct, so every registration is
/// listed here, and at the session start for seven days after it is made.
pub fn working_copy_lines(db: &Path) -> Vec<String> {
    let Ok(store) = EventStore::open_existing(db) else {
        return vec!["working copies: store unreadable, not checked".to_string()];
    };
    let items = serve::live::live_items(&store);
    let mut registered: std::collections::BTreeSet<String> = Default::default();
    let mut owners: std::collections::BTreeMap<String, usize> = Default::default();
    for li in &items {
        let key = li
            .id
            .strip_prefix("project-")
            .and_then(|rest| rest.strip_suffix("-opened-by-install").or_else(|| rest.strip_suffix("-root-by-install")));
        if let Some(key) = key {
            registered.insert(key.to_string());
        }
        if li.item.kind.can_fire() {
            if let Some(project) = &li.item.project {
                *owners.entry(project.clone()).or_default() += 1;
            }
        }
    }
    // A record retracted before this build is not live, but its fallback
    // record is - and a project that owns rules is asked about either way.
    registered.extend(owners.keys().cloned());

    let mut lines = Vec::new();
    let mut unregistered: Vec<String> = Vec::new();
    for key in &registered {
        let regs = model::store::registrations_for_machine(&store, key, &model::machine::current_machine_id());
        let Some(last) = regs.last() else {
            if let Some(n) = owners.get(key) {
                unregistered.push(format!("{key} ({n} rule(s))"));
            }
            continue;
        };
        let since = last.at.as_deref().map(|t| t.chars().take(10).collect::<String>()).unwrap_or_else(|| "an unknown date".to_string());
        let mut line = format!("working copy: {key} -> {} (registered {since})", last.root);
        if let Some(prev) = regs.iter().rev().nth(1).filter(|p| p.root != last.root) {
            line.push_str(&format!(", moved from {}", prev.root));
        }
        if let Err(problem) = serve::project::validate_checkout(Path::new(&last.root), key) {
            line.push_str(&format!(" - NOT USABLE: {problem}"));
        }
        lines.push(line);
    }
    if !unregistered.is_empty() {
        lines.push(format!(
            "working copies: {} project(s) own rules but have no registered working copy: {} - run {} from inside \
             each one's checkout (a project not on this machine needs nothing)",
            unregistered.len(),
            unregistered.join(", "),
            model::store::register_command_for(Some(db))
        ));
    }
    if lines.is_empty() {
        lines.push("working copies: no project has one registered, and no project owns a rule".to_string());
    }
    lines
}

/// Same-file pairs of live Rule/Orientation checks whose proofs currently
/// disagree - one comes back `Holds`, the other `Fails`, on the exact same
/// file, right now. See `serve/examples/check_contradictions.rs`'s own "(b)
/// BOTH RUNNABLE, DISAGREEING NOW" for the full definition this line shares:
/// neither side may be `CannotRun` (the absence of information - a missing
/// file, usually a different checkout - never a disagreement, see `model::
/// check::Outcome`'s own doc comment), and this says nothing about whether
/// the two checks could ever coexist in principle. That structural question
/// - can they NEVER both hold, regardless of the file's content - is GROUND
/// 29's own job (`model::gate::opposing_literals`), refused at write time;
/// this line answers a narrower, purely empirical one about right now, and
/// on purpose does NOT feed the write gate either way: a false alarm here is
/// exactly as expensive as one at write time, and this signal is measured
/// against the file as it happens to stand today, never proven the way
/// GROUND 29's structural test is.
///
/// PROJECT RESOLUTION is identical to `decay_check`'s own: every immediate
/// subdirectory of `checkouts` is handed to `serve::project::resolve_
/// project`, and a GLOBAL item (`project: None`) is skipped entirely - it
/// names no single checkout to run its check against, the same restriction
/// `decay_check` already places on itself, for the identical reason.
///
/// SILENT AT ZERO, the same convention `judgement_debt_line` above already
/// uses: a store with nothing disagreeing is not a finding, and a permanent
/// "0 pairs" line on every quiet run would be exactly the noise that trains
/// a reader to stop reading this report. Also silent (never a separate error
/// line) when the store cannot be opened or `checkouts` was not given - the
/// same fails-open stance `judgement_debt_line` takes; decay/crowding/
/// orphan-projects above already say, once, whether `--checkouts` was
/// resolved at all, so a fourth line repeating that would only be noise.
///
/// MEASURED on the owner's real store, 2026-09-09, before this line existed:
/// 0 same-file pairs disagree right now - see `serve/examples/
/// check_contradictions.rs`'s own phase-1 report for the full count this was
/// built from (357 checked items, 323 naming a file, 265 same-file pairs).
pub fn contradiction_line(db: &Path, checkouts: Option<&Path>) -> Option<String> {
    let store = EventStore::open_existing(db).ok()?;
    let root = checkouts?;

    let roots = checkout_roots(&store, root);

    let mut by_file: std::collections::BTreeMap<(String, String), Vec<model::item::Check>> = Default::default();
    for li in serve::live::live_items(&store) {
        if !li.item.kind.can_fire() {
            continue; // only Rule/Orientation can ever carry a check (gate::declare ground 12)
        }
        let Some(project) = li.item.project else { continue };
        let Some(check) = li.item.check else { continue };
        let Some(path) = model::check::named_path(&check) else { continue };
        let path = model::normalize::normalize_target(path);
        by_file.entry((project, path)).or_default().push(check);
    }

    let mut disagreeing = 0usize;
    for ((project, _path), checks) in &by_file {
        let Some(base) = roots.get(project) else { continue };
        let decided = |o: model::check::Outcome| matches!(o, model::check::Outcome::Holds | model::check::Outcome::Fails);
        for i in 0..checks.len() {
            for j in (i + 1)..checks.len() {
                let outcome_a = model::check::run(&checks[i], base);
                let outcome_b = model::check::run(&checks[j], base);
                if decided(outcome_a) && decided(outcome_b) && outcome_a != outcome_b {
                    disagreeing += 1;
                }
            }
        }
    }

    if disagreeing == 0 {
        return None;
    }
    Some(format!(
        "contradictions: {disagreeing} same-file pair(s) of live notes have proofs that cannot both be \
         true right now - see serve/examples/check_contradictions.rs for the full list; a measured \
         disagreement on the file as it stands today, not a proof the two could never coexist"
    ))
}

/// The write-ahead log's own sidecar path: SQLite spells it `<db>-wal`,
/// literally appended to the whole file name (never `.with_extension`, which
/// would replace `.db` instead of extending it).
fn wal_sidecar_path(db: &Path) -> std::path::PathBuf {
    let mut name = db.as_os_str().to_os_string();
    name.push("-wal");
    std::path::PathBuf::from(name)
}

/// The `-wal` sidecar's raw size in bytes, `None` when it does not exist.
/// Nothing but a `stat` - see `wal_line_from_size`'s doc comment for why
/// `report` has to call this before it opens any connection to `db`, rather
/// than leaving `wal_line_from_size` to read the file for itself.
fn wal_raw_size(db: &Path) -> Option<u64> {
    std::fs::metadata(wal_sidecar_path(db)).ok().map(|m| m.len())
}

/// Whether the write-ahead log has grown past anything a landing checkpoint
/// would ever leave behind, given an ALREADY-MEASURED `wal_size` - `None`
/// when there was nothing to measure or it has not grown past the bar, so
/// this stays silent for the ordinary case of a WAL that grows and shrinks
/// all the time by design.
///
/// WHY THIS TAKES `wal_size` AS A PARAMETER INSTEAD OF READING THE FILE
/// ITSELF. THE DEFECT THIS CLOSES, measured directly by a black-box
/// verification: a 500 MB `-wal` beside a copy of the store produced no wal
/// line at all, and the file was gone afterwards. The cause was ordering -
/// `report` used to call this function AFTER `store_line` had already opened
/// and closed the report's own connection to `db`, and when that connection
/// is the only one on the store (exactly the single-session case a new user
/// is in, on their very first `doctor` run), SQLite is free to checkpoint
/// and shrink the WAL back down to the `journal_size_limit` cap the moment
/// the connection closes (see `EventStore::new`/`open_existing`) - and it
/// discards a `-wal` file with an invalid header the moment a connection
/// opens at all, which is what a hand-built fixture (or any WAL left by a
/// different process) looks like to it. Either way, by the time the old code
/// read `std::fs::metadata` for itself, the evidence it meant to report on
/// was already gone. `report` now takes this measurement as the very first
/// thing it does, before `store_line` or anything else touches `db`, and
/// hands the result in here - the emitted line still lands in the same
/// position in the report as before, only the MEASUREMENT moved earlier.
///
/// BOTH CONDITIONS below, on purpose - bigger than the store AND at least 64
/// MB. The size comparison alone would flag a brand new, still-tiny store
/// the moment its WAL is merely bigger than an almost-empty database file: a
/// comparison that is honest about the RATIO and dishonest about the
/// STAKES. The 64 MB floor is what keeps silence the right answer for
/// exactly that case, so a fresh small store never nags.
fn wal_line_from_size(wal_size: Option<u64>, db: &Path) -> Option<String> {
    const MB: u64 = 1024 * 1024;
    const WAL_FLOOR_BYTES: u64 = 64 * MB;
    let wal_size = wal_size?;
    if wal_size < WAL_FLOOR_BYTES {
        return None;
    }
    let db_size = std::fs::metadata(db).ok()?.len();
    if wal_size <= db_size {
        return None;
    }
    Some(format!(
        "wal: the write-ahead log is {:.1} MB, bigger than the {:.1} MB store itself - a checkpoint is not \
         landing; a long-running reader or a repair loop is holding it open",
        wal_size as f64 / MB as f64,
        db_size as f64 / MB as f64
    ))
}

/// Same computation as `wal_line_from_size`, but measures the WAL itself
/// first - for a standalone caller that is not `report` and has not already
/// taken that measurement before opening a connection of its own. `report`
/// must NOT use this: see `wal_line_from_size`'s doc comment for why the
/// measurement has to happen before any connection to `db` is opened, which
/// this function has no way to guarantee on a caller's behalf.
pub fn wal_line(db: &Path) -> Option<String> {
    wal_line_from_size(wal_raw_size(db), db)
}

/// Where the rules stand, read through the one reader
/// (`model::store::settlement`), and what the log holds as field evidence
/// (`serve::attest::is_field_refusal`). Line by line:
/// - the SHAPE claim (kept): how many heavy rules carry a check shaped to
///   refuse at all, the rest named as `toothless`;
/// - ROT: every live rule and orientation, whatever its weight (D8), with no
///   current verdict and no machine blocker holding - with its heavy subset
///   beside the plan's starting line, and the share owing only the A4 naming
///   fix with its drain estimate; each named as `undecided`, with the reason;
/// - the VERDICTS: proven, wired, notes (by dispute, with a pending check),
///   rules kept by `accept_refusing`, wired forbidden rules whose literal no
///   real call ever carried and that no real session was ever refused by (a
///   requires check has no literal to carry, so it is never counted), and the
///   blocked ones per project and blocker;
/// - FIELD EVIDENCE: rules holding a field refusal, beside the ones passing
///   only its structural part, and every refusal that does not count, sorted
///   by why (not a Claude Code session, no matching tool call, no tool call
///   id at all, not verified yet, and the THOR actors counted apart);
/// - rules TAKEN OFF the rule track in the last 30 days, with reason and door,
///   and the REWORD line: refused for a tooth, then stored as a note that no
///   longer names it - information, never a verdict on the writer.
///
/// Tags are never read here: a label is only a readable copy of a record.
pub fn teeth_line(db: &Path, full: bool) -> String {
    let Ok(store) = EventStore::open_existing(db) else {
        return "teeth: store unreadable, not checked".to_string();
    };
    // Every refusal below asks the log about its session; one pass, not one per refusal.
    store.cache_session_reads();
    let Ok(events) = store.get_all_events() else {
        return "teeth: log unreadable, not checked".to_string();
    };
    let live = serve::live::live_items(&store);
    // How often each item has fired since its own last verdict - the fold
    // `judgement_debt_line` reads too, so "fires often" means one thing.
    let fired = serve::usefulness::served_since_last_verdict(&store);
    let project_of = |item: &model::item::Item| item.project.clone().unwrap_or_else(|| "global".to_string());

    // THE SHAPE CLAIM (kept): which heavy rules carry a check shaped to
    // refuse at all, and the ones that can only inform, heaviest first then
    // the busiest.
    let (mut heavy, mut heavy_with_teeth) = (0usize, 0usize);
    let mut toothless: Vec<(u8, usize, Rot)> = Vec::new();

    // THE ONE READER (step 9): every live rule and orientation, whatever its
    // weight, through `model::store::settlement` - the answer the filing gate
    // and the Stop hook read too.
    let (mut proven_n, mut wired_n, mut notes_n) = (0usize, 0usize, 0usize);
    let mut rot: Vec<(u8, usize, Rot)> = Vec::new();
    let (mut rot_heavy, mut a4_only) = (0usize, 0usize);
    let mut blocked: std::collections::BTreeMap<String, Vec<String>> = Default::default();
    let mut pending: std::collections::BTreeMap<&'static str, Vec<String>> = Default::default();
    let mut by_dispute: Vec<Rot> = Vec::new();
    let mut accepted: Vec<Rot> = Vec::new();
    let mut unseen: Vec<Rot> = Vec::new();
    // Old contains checks counted as teeth although their text does not name
    // the literal (Q24), each with the fragment it guards.
    let mut old_stock: Vec<Rot> = Vec::new();

    // FIELD EVIDENCE (K6): a rule counts as having really refused something
    // only through `serve::attest::is_field_refusal`, from the recorded
    // attester verdicts; the structural part alone is shown beside it.
    let mut field_rules = 0usize;
    let mut structural_rules = 0usize;
    // Only an item some refusal names can hold one: read those logs only.
    let refused_ids: std::collections::HashSet<&str> = events
        .iter()
        .filter(|e| e.kind == thor_core::event_store::EventKind::GateRefused)
        .map(|e| e.entity_id.as_str())
        .collect();
    // The sessions that served anything - what `served_ids_in_session` asks,
    // answered once from the events already in hand, not per session.
    let active = sessions_with_activity(&events);
    let session_active = |session: &str| active.contains(session);
    // The live set folded once, for the rules whose check reads a command - not
    // once per refusal.
    let projects = std::cell::OnceCell::new();
    let known = || projects.get_or_init(|| serve::teeth::known_projects(&store)).clone();

    for li in live.iter().filter(|li| li.item.kind.can_fire()) {
        let is_heavy = matches!(
            li.item.severity,
            Some(model::item::Severity::Irreversible) | Some(model::item::Severity::Costly)
        );
        let count = fired.get(&li.id).copied().unwrap_or(0);
        let rank = model::item::severity_rank(li.item.severity);
        let project = project_of(&li.item);
        let check_desc = match &li.item.check {
            None => "no check".to_string(),
            Some(c) => format!("check: {c:?}"),
        };
        if is_heavy {
            heavy += 1;
            if can_refuse(&li.item) {
                heavy_with_teeth += 1;
            } else {
                toothless.push((
                    rank,
                    count,
                    Rot {
                        id: li.id.clone(),
                        project: project.clone(),
                        what: format!("{:?}, {check_desc}, fired {count}x since its last verdict", li.item.severity),
                    },
                ));
            }
        }
        // A real session's hook refused a call under this check (the
        // structural part of field evidence): the rule bites, whatever the
        // call corpus holds.
        let mut really_refused = false;
        if li.item.check.is_some() && refused_ids.contains(li.id.as_str()) {
            if !serve::attest::field_refusals_known(&store, &li.id, &li.item, &known).is_empty() {
                field_rules += 1;
            }
            if let Ok(own) = store.get_events_by_entity(&li.id) {
                let since = serve::attest::check_since(&own, li.item.check.as_ref());
                let structural = own.iter().any(|e| {
                    e.kind == thor_core::event_store::EventKind::GateRefused
                        && e.actor == model::store::ASSISTANT_DELIVERY_ACTOR
                        && since != i64::MAX
                        && e.seq > since
                        && serve::attest::is_uuid_shaped(&e.session_id)
                        && session_active(&e.session_id)
                });
                if structural {
                    structural_rules += 1;
                    really_refused = true;
                }
            }
        }
        match model::store::settlement(&store, &li.id, None) {
            model::store::Settlement::Owed { why } => {
                if is_heavy {
                    rot_heavy += 1;
                }
                // Owes the A4 naming fix: its check names a literal its text
                // does not (K23) - words only the agent can choose.
                if why.starts_with("A4:") || !model::gate::unnamed_literals(&li.item).is_empty() {
                    a4_only += 1;
                }
                rot.push((rank, count, Rot { id: li.id.clone(), project: project.clone(), what: why }));
            }
            model::store::Settlement::Settled(model::store::Settled::Blocked(blocker)) => {
                blocked.entry(blocker.to_string()).or_default().push(li.id.clone());
            }
            model::store::Settlement::Settled(model::store::Settled::Rule { proven }) => {
                if proven {
                    proven_n += 1;
                } else {
                    wired_n += 1;
                }
                let record = model::store::current_record(&store, &li.id);
                let fields = record.as_ref().map(model::store::TrialRecord::fields);
                if let Some(f) = fields.filter(|f| !f.accept_refusing.is_empty()) {
                    let calls = f.accept_refusing.iter().map(|c| format!("'{c}'")).collect::<Vec<_>>().join(", ");
                    let because = dispute_reason_in_words(&store, &li.id);
                    accepted.push(Rot {
                        id: li.id.clone(),
                        project: project.clone(),
                        what: format!("accepts refusing {calls} - because: {because}"),
                    });
                }
                if let Some(mark) = fields.and_then(|f| f.a4_old_stock.as_ref()) {
                    old_stock.push(Rot { id: li.id.clone(), project: project.clone(), what: mark.describe() });
                }
                // Only a forbidden check has a literal to carry: a requires
                // check's trial never counts one (`whole_hits` is 0 by
                // construction), and a rule a real session was refused by is
                // not one nothing ever carried - the call corpus drops the
                // calls that were refused.
                let forbidden_check = matches!(li.item.check, Some(model::item::Check::Forbidden { .. }));
                if !proven && forbidden_check && !really_refused && fields.is_some_and(|f| f.corpus.whole_hits == 0) {
                    unseen.push(Rot { id: li.id.clone(), project: project.clone(), what: check_desc.clone() });
                }
            }
            model::store::Settlement::Settled(model::store::Settled::Note { pending: why, by_dispute: dispute }) => {
                notes_n += 1;
                if let Some(why) = why {
                    let label = match why {
                        model::store::PendingWhy::NoWorkingCopy => "no registered working copy",
                        model::store::PendingWhy::NotTrueInRegisteredCheckout => "not true in the registered checkout",
                    };
                    pending.entry(label).or_default().push(li.id.clone());
                }
                if dispute {
                    let record = model::store::current_record(&store, &li.id);
                    let what = record.as_ref().map_or_else(String::new, |r| {
                        let d = &r.fields().disputes;
                        let list = |ls: &[String]| ls.iter().map(|c| format!("'{c}'")).collect::<Vec<_>>().join(", ");
                        let because = dispute_reason_in_words(&store, &li.id);
                        format!(
                            "legitimate_calls [{}], may_change [{}] - because: {because}",
                            list(&d.legitimate_calls),
                            list(&d.may_change)
                        )
                    });
                    by_dispute.push(Rot { id: li.id.clone(), project: project.clone(), what });
                }
            }
            model::store::Settlement::Settled(_) => {}
        }
    }

    let refusals = refusals_apart(&store, &active, &events);
    let taken_off = taken_off_the_rule_track(&events);
    let reworded = reworded_after_a_refusal(&store, &events);

    let mut out = Vec::new();
    if heavy == 0 {
        out.push(format!(
            "teeth: no rule is marked irreversible or costly yet; {field_rules} rule(s) of any weight hold a field \
             refusal, the only count here that is proven rather than claimed"
        ));
    } else {
        out.push(format!(
            "teeth: {heavy_with_teeth} of {heavy} rule(s) marked irreversible or costly carry a check shaped to refuse - a claim about shape only, which an invented literal satisfies as well as a real one (the lines below judge them) - the other {} can only inform, and they are the ones where being wrong was already called expensive; {field_rules} rule(s) of any weight hold a field refusal, the only count here that is proven rather than claimed",
            heavy - heavy_with_teeth
        ));
    }
    let evaluations = a4_only.div_ceil(A4_PER_EVALUATION);
    out.push(format!(
        "teeth rot: {} live rule(s) and orientation(s) have no current verdict from THOR's trial, whatever their \
         weight - {rot_heavy} of them heavy (the plan's starting line: 306 heavy, 171 with neither); {a4_only} of \
         them owe only the A4 naming fix (at {A4_PER_EVALUATION} per evaluation: {evaluations} evaluation(s)). The \
         finish line is 0; each one is decided by `serve retest --id <id>`",
        rot.len()
    ));
    let pending_n: usize = pending.values().map(Vec::len).sum();
    let blocked_n: usize = blocked.values().map(Vec::len).sum();
    out.push(format!(
        "teeth verdicts: {proven_n} proven and {wired_n} wired on the rule track; {notes_n} note(s), {} of them by \
         dispute and {pending_n} with a pending check; {} rule(s) kept by accept_refusing; {} wired forbidden rule(s) \
         whose literal no real call on record ever carried and that never refused one; {blocked_n} blocked, held until their blocker lifts",
        by_dispute.len(),
        accepted.len(),
        unseen.len()
    ));
    // Said only where it can matter: an old contains check counted, a cutoff
    // recorded, or rules still owing the A4 naming fix.
    let recorded_cutoff = model::store::a4_cutoff(&store);
    if !old_stock.is_empty() || recorded_cutoff.is_some() || a4_only > 0 {
        let cutoff = match recorded_cutoff {
            Some(seq) => format!("the A4 cutoff is log seq {seq}, recorded once and never moved"),
            None => "no A4 cutoff is recorded yet - the first `serve retest --apply` that counts one records it"
                .to_string(),
        };
        out.push(format!(
            "teeth old contains (Q24): {} rule(s) on the rule track are old contains checks whose text does not name \
             the literal - counted as teeth because the check bites in the registered working copy, which proves the \
             literal real; each is named below with the fragment it guards, for the owner to judge whether it guards \
             the rule's subject; {cutoff}",
            old_stock.len()
        ));
    }
    out.push(format!(
        "teeth against reality: {field_rules} rule(s) hold a field refusal - the hook refused a call in a Claude \
         Code session whose transcript holds that tool call and the hook's block of it, under the check the rule \
         carries now; {structural_rules} rule(s) pass the structural part alone (the hook, a Claude Code session \
         with other activity, after the check was set), which never counts on its own"
    ));
    out.push(format!(
        "teeth refusals: {} of {} refusal(s) on the log count as field evidence; the other {} never count as proof",
        refusals.counted,
        refusals.total,
        refusals.total - refusals.counted
    ));
    out.extend(refusals.lines());
    for (blocker, ids) in &blocked {
        out.push(format!("  blocked: {blocker}: {}", ids.join(", ")));
    }
    for (why, ids) in &pending {
        out.push(format!("  note with a pending check ({why}): {}", ids.join(", ")));
    }
    out.extend(name_rot("note by dispute", &by_dispute, full));
    out.extend(name_rot("kept by accept_refusing", &accepted, full));
    out.extend(name_rot("wired, literal never seen", &unseen, full));
    out.extend(name_rot("old contains, text does not name it", &old_stock, full));
    let cap = name_cap(full);
    for (label, line) in taken_off.iter().take(cap) {
        out.push(format!("  {label}: {line}"));
    }
    if taken_off.len() > cap {
        out.push(format!(
            "  taken off the rule track or check weakened: and {} more, not named here (use --full)",
            taken_off.len() - cap
        ));
    }
    for line in &reworded {
        out.push(format!("  reworded: {line}"));
    }
    rot.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| b.1.cmp(&a.1)).then_with(|| a.2.id.cmp(&b.2.id)));
    let rot: Vec<Rot> = rot.into_iter().map(|(_, _, r)| r).collect();
    out.extend(name_rot("undecided", &rot, full));
    toothless.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| b.1.cmp(&a.1)).then_with(|| a.2.id.cmp(&b.2.id)));
    let toothless: Vec<Rot> = toothless.into_iter().map(|(_, _, r)| r).collect();
    out.extend(name_rot("toothless", &toothless, full));
    out.join("\n")
}

/// How many rules owing only the A4 naming fix one evaluation takes on (K23,
/// Q24) - what the drain estimate on the rot line divides by.
const A4_PER_EVALUATION: usize = 16;

/// Every refusal on the log, split by why it does or does not count (K6, EV
/// B7, Q5).
#[derive(Default)]
struct RefusalsApart {
    total: usize,
    /// Verified field evidence: a recorded attester verdict says field.
    counted: usize,
    /// Stamped by the hook in a session that is not a Claude Code session.
    not_claude_code: Vec<String>,
    /// Verified and recorded as NOT field evidence (no matching tool call, no
    /// block in the transcript, no other activity).
    no_matching_call: usize,
    /// Old: no tool call id, so never checkable.
    no_tool_call_id: usize,
    /// Not verified yet: no attester verdict recorded.
    unverified: usize,
    teeth_probe: usize,
    teeth_eval: usize,
    teeth_write: usize,
    /// Under any other actor: a hand-fed payload with `--actor`.
    other_actor: usize,
    /// A real refusal met by a session during its own evaluation (step 20,
    /// NEW-2, `serve::attest::self_proof`): never proof.
    own_evaluation: usize,
    /// A real refusal met by the session that wrote the rule's check: never
    /// proof.
    own_check: usize,
}

impl RefusalsApart {
    fn lines(&self) -> Vec<String> {
        let mut out = Vec::new();
        match self.not_claude_code.len() {
            0 => {}
            1 => out.push(format!(
                "  1 refusal from a session that is not a Claude Code session ({}) - not counted",
                self.not_claude_code[0]
            )),
            n => {
                let mut sessions = self.not_claude_code.clone();
                sessions.sort();
                sessions.dedup();
                let shown: Vec<&str> = sessions.iter().take(5).map(String::as_str).collect();
                out.push(format!(
                    "  {n} refusals from sessions that are not Claude Code sessions ({}{}) - not counted",
                    shown.join(", "),
                    if sessions.len() > 5 { ", ..." } else { "" }
                ));
            }
        }
        if self.no_matching_call > 0 {
            out.push(format!(
                "  {} refusal(s) from Claude Code sessions with no matching tool call in the transcript - never counted \
                 as proof",
                self.no_matching_call
            ));
        }
        if self.own_evaluation > 0 {
            out.push(format!(
                "  {} refusal(s) refused during the session's own evaluation - not proof (the evaluating session made \
                 those calls itself, while judging the rules that refused them)",
                self.own_evaluation
            ));
        }
        if self.own_check > 0 {
            out.push(format!(
                "  {} refusal(s) refused in the session that wrote the rule's check - not proof",
                self.own_check
            ));
        }
        if self.no_tool_call_id > 0 {
            out.push(format!(
                "  {} old refusal(s) carry no tool call id and can never be checked",
                self.no_tool_call_id
            ));
        }
        if self.unverified > 0 {
            out.push(format!(
                "  {} refusal(s) not verified yet - their session's transcript is read at its next Stop; not counted \
                 until then",
                self.unverified
            ));
        }
        // One count for every refusal nobody's real tool call stands behind
        // (Z9): a probe, a payload fed to the hook by hand, a session that
        // is not a Claude Code session, a call the transcript does not hold.
        let never_proof = self.teeth_probe + self.other_actor + self.not_claude_code.len() + self.no_matching_call;
        if never_proof > 0 {
            out.push(format!(
                "  probe or hand-fed refusals (never proof): {never_proof} - {} by teeth-probe, {} hand-fed under another \
                 actor, {} from a session that is not a Claude Code session, {} with no matching tool call in the \
                 session's transcript",
                self.teeth_probe,
                self.other_actor,
                self.not_claude_code.len(),
                self.no_matching_call
            ));
        }
        let apart = self.teeth_probe + self.teeth_eval + self.teeth_write + self.other_actor;
        if apart > 0 {
            out.push(format!(
                "  counted apart, never evidence: {} by teeth-probe, {} by teeth-eval, {} by teeth-write (writes THOR's \
                 trial refused), {} hand-fed under another actor",
                self.teeth_probe, self.teeth_eval, self.teeth_write, self.other_actor
            ));
        }
        out
    }
}

/// Every session that served anything on the log.
fn sessions_with_activity(events: &[Event]) -> std::collections::HashSet<&str> {
    events
        .iter()
        .filter(|e| e.kind == thor_core::event_store::EventKind::ItemServed)
        .map(|e| e.session_id.as_str())
        .collect()
}

/// Every `gate_refused` on the log, sorted into `RefusalsApart`. `active`:
/// the sessions that served anything (`sessions_with_activity`).
fn refusals_apart(store: &EventStore, active: &std::collections::HashSet<&str>, events: &[Event]) -> RefusalsApart {
    use thor_core::event_store::EventKind;
    let verdicts = model::store::recorded_refusal_verdicts(events);
    let mut out = RefusalsApart::default();
    for e in events.iter().filter(|e| e.kind == EventKind::GateRefused) {
        out.total += 1;
        match e.actor.as_str() {
            model::store::TEETH_PROBE_ACTOR => out.teeth_probe += 1,
            "teeth-eval" => out.teeth_eval += 1,
            model::store::TEETH_WRITE_ACTOR => out.teeth_write += 1,
            model::store::ASSISTANT_DELIVERY_ACTOR => {
                let body = serde_json::from_str::<serde_json::Value>(&e.body).unwrap_or(serde_json::Value::Null);
                let has_id = body.get("tool_use_id").and_then(|t| t.as_str()).is_some_and(|t| !t.is_empty());
                let session_active = active.contains(e.session_id.as_str());
                if !serve::attest::is_uuid_shaped(&e.session_id) {
                    out.not_claude_code.push(e.session_id.clone());
                } else if !has_id {
                    out.no_tool_call_id += 1;
                } else {
                    // A self-proof is shown apart, whether its verdict was
                    // recorded as one (step 20) or recorded field before.
                    let self_why = |why: &str| {
                        if why.starts_with(serve::attest::OWN_EVALUATION_WHY) {
                            Some(true)
                        } else if why.starts_with(serve::attest::OWN_CHECK_WHY) {
                            Some(false)
                        } else {
                            None
                        }
                    };
                    let apart = match verdicts.get(&e.seq) {
                        Some((false, why)) => self_why(why),
                        Some((true, _)) if session_active => serve::attest::self_proof(store, e).and_then(|why| self_why(&why)),
                        _ => None,
                    };
                    match (apart, verdicts.get(&e.seq)) {
                        (Some(true), _) => out.own_evaluation += 1,
                        (Some(false), _) => out.own_check += 1,
                        (None, Some((true, _))) if session_active => out.counted += 1,
                        (None, Some(_)) => out.no_matching_call += 1,
                        (None, None) => out.unverified += 1,
                    }
                }
            }
            _ => out.other_actor += 1,
        }
    }
    out
}

/// How many days back "taken off the rule track" looks.
const TAKEN_OFF_DAYS: i64 = 30;

/// Every Rule or Orientation taken off the rule track in the last
/// `TAKEN_OFF_DAYS` days - retracted, archived, or revised to a kind that
/// never fires (D16) - and every rule whose check was cleared, moved to another
/// file or changed by a write that is not THOR's own (step 22: a clear_check
/// with a because is a weakening the owner must be able to see) - newest
/// first, each with its reason and the door (the actor that wrote it), under
/// the label it is listed by. An event carries no time of its own, so each one
/// is dated by the newest served or refused event before it that records one;
/// one with none before it is listed rather than dropped.
fn taken_off_the_rule_track(events: &[Event]) -> Vec<(&'static str, String)> {
    use thor_core::event_store::EventKind;
    let now = serve::time::now_unix();
    let mut last_at: Option<i64> = None;
    let mut last_kind: std::collections::HashMap<&str, model::item::Kind> = Default::default();
    let mut last_check: std::collections::HashMap<&str, Option<model::item::Check>> = Default::default();
    let mut out: Vec<(i64, &'static str, String)> = Vec::new();
    for e in events {
        match e.kind {
            EventKind::ItemServed | EventKind::GateRefused | EventKind::GateStoodAside => {
                let body = serde_json::from_str::<serde_json::Value>(&e.body).unwrap_or(serde_json::Value::Null);
                let at = body.get("served_at").or_else(|| body.get("at")).and_then(|v| v.as_str());
                if let Some(t) = at.and_then(serve::time::unix_from_iso8601) {
                    last_at = Some(t);
                }
            }
            EventKind::FactCreated | EventKind::FactRevised | EventKind::FactResolved => {
                let Ok(item) = model::store::parse_body(&e.body) else { continue };
                let before = last_kind.insert(e.entity_id.as_str(), item.kind);
                let before_check = last_check.insert(e.entity_id.as_str(), item.check.clone()).flatten();
                let was_rule = before.is_some_and(|k| k.can_fire());
                if e.kind == EventKind::FactRevised && was_rule && !item.kind.can_fire() {
                    let reason = item
                        .tags
                        .iter()
                        .find_map(|t| t.strip_prefix("archived:"))
                        .map(|r| format!("archived: {r}"))
                        .or_else(|| {
                            serde_json::from_str::<serde_json::Value>(&e.body)
                                .ok()
                                .and_then(|v| v.get("because").and_then(|b| b.as_str()).map(|b| format!("because: {b}")))
                        })
                        .unwrap_or_else(|| "no reason recorded".to_string());
                    out.push((
                        e.seq,
                        "taken off the rule track",
                        format!("{} - kind changed to a report at log seq {} ({reason}; door: {})", e.entity_id, e.seq, e.actor),
                    ));
                } else if e.kind == EventKind::FactRevised && was_rule && item.kind.can_fire() && !model::store::RESERVED_ACTORS.contains(&e.actor.as_str()) {
                    if let Some(old) = before_check.filter(|old| item.check.as_ref() != Some(old)) {
                        let what = match &item.check {
                            None => format!("check cleared ({})", model::store::check_in_words(&old)),
                            Some(new) if model::store::check_file_of(&old) != model::store::check_file_of(new) => format!(
                                "check moved from {} to {}",
                                model::store::check_in_words(&old),
                                model::store::check_in_words(new)
                            ),
                            // The literals are named: kind and file alone read as
                            // no change when only the literal moved (field test 13, D6).
                            Some(new) => format!(
                                "check changed from {} to {}",
                                model::store::check_named(&old),
                                model::store::check_named(new)
                            ),
                        };
                        let because = serde_json::from_str::<serde_json::Value>(&e.body)
                            .ok()
                            .and_then(|v| v.get("because").and_then(|b| b.as_str()).map(str::to_string))
                            .unwrap_or_else(|| "no reason recorded".to_string());
                        out.push((
                            e.seq,
                            "check weakened",
                            format!("{} - {what} at log seq {} (because: {because}; door: {})", e.entity_id, e.seq, e.actor),
                        ));
                    }
                }
            }
            EventKind::FactRetracted => {
                let was_rule = last_kind.remove(e.entity_id.as_str()).is_some_and(|k| k.can_fire());
                if was_rule {
                    let reason = serde_json::from_str::<serde_json::Value>(&e.body)
                        .ok()
                        .and_then(|v| v.get("reason").and_then(|r| r.as_str()).map(str::to_string))
                        .unwrap_or_else(|| "no reason recorded".to_string());
                    out.push((
                        e.seq,
                        "taken off the rule track",
                        format!("{} - retracted at log seq {} (reason: {reason}; door: {})", e.entity_id, e.seq, e.actor),
                    ));
                }
            }
            _ => continue,
        }
        if matches!(e.kind, EventKind::FactRevised | EventKind::FactRetracted) {
            if let (Some((seq, _, _)), Some(at)) = (out.last(), last_at) {
                if *seq == e.seq && now - at > TAKEN_OFF_DAYS * 86_400 {
                    out.pop();
                }
            }
        }
    }
    out.reverse();
    out.into_iter().map(|(_, label, line)| (label, line)).collect()
}

/// R7: a write THOR's trial refused because a tooth it built from the text
/// bites (a `teeth-write` refusal with code `forcing`), after which the same
/// id was stored as a note whose text no longer names that tooth's literal -
/// "<id>: refused with <tooth> at log seq N, then stored as a note that no
/// longer names it". Information for the reader, never a verdict on the
/// writer. One line per id, from its latest such refusal.
fn reworded_after_a_refusal(store: &EventStore, events: &[Event]) -> Vec<String> {
    use thor_core::event_store::EventKind;
    let mut latest: std::collections::BTreeMap<&str, (i64, String)> = Default::default();
    for e in events.iter().filter(|e| e.kind == EventKind::GateRefused && e.actor == model::store::TEETH_WRITE_ACTOR) {
        let Ok(body) = serde_json::from_str::<serde_json::Value>(&e.body) else { continue };
        if body.get("code").and_then(|c| c.as_str()) != Some("forcing") {
            continue;
        }
        let Some(tooth) = body.get("forcing").and_then(|f| f.as_array()).and_then(|f| f.first()).and_then(|t| t.as_str()) else {
            continue;
        };
        latest.insert(e.entity_id.as_str(), (e.seq, tooth.to_string()));
    }
    let mut out = Vec::new();
    for (id, (seq, tooth)) in latest {
        let Ok(item) = model::store::show(store, id) else { continue };
        if model::store::current_verdict(store, id) != model::store::CurrentVerdict::Note {
            continue;
        }
        // "forbidden '--force' on 'git push' refuses ..." -> the tooth, and
        // its literal (the first quoted span).
        let named = tooth.split(" refuses ").next().unwrap_or(&tooth).to_string();
        let Some(literal) = named.split('\'').nth(1) else { continue };
        if model::gate::literal_is_named(&item.text, literal) {
            continue;
        }
        out.push(format!("{id}: refused with {named} at log seq {seq}, then stored as a note that no longer names it"));
    }
    out
}

/// What the only maintenance loop in this system almost never looks at.
///
/// WHY THIS LINE EXISTS. A pinned item is excluded from decay (pinning is a
/// PLACEMENT decision - "this fires at every session start" - and decay
/// checks anchors/proofs, which most pins never carry at all). Until
/// 2026-09-07 it was ALSO excluded from the judgement debt outright, on the
/// reasoning that pinning is itself a verdict, so "did it belong where it
/// fired" has no honest answer - measured never to let go again: 44 of 48
/// pinned items had never been examined by anything, a permanent blind spot,
/// not a temporary one. `judgement_debt` (`bin/serve.rs`) now gives each
/// never-judged pin exactly ONE verdict - the same `mark` call any other
/// item gets - after which it is treated identically to everything else
/// (asked again only if it fires `JUDGEMENT_DEBT_AFTER` more times since
/// that verdict). This line's own count still MEASURES the same blind spot
/// (a pin is also served IN FULL at every session start, uncapped, so it is
/// never crowded out of a share of servings), because a pin's first verdict
/// takes as many sessions to arrive as any other item's does - the exclusion
/// this line was built to expose is narrower now, not gone: measured
/// 2026-08-08, 31 pinned items were 48.9% of every serving this store had
/// ever made, and 23 of them had never been examined by anything.
///
/// No mechanism beyond that one verdict is proposed here. The number is
/// still the point: a large share of what this memory says comes from the
/// part of it nothing routinely re-reads.
pub fn pinned_line(db: &Path) -> String {
    let Ok(store) = EventStore::open_existing(db) else {
        return "pinned: store unreadable, not checked".to_string();
    };
    let Ok(events) = store.event_kinds() else {
        return "pinned: log unreadable, not checked".to_string();
    };
    let mut served: std::collections::HashMap<String, usize> = Default::default();
    let mut judged: std::collections::HashSet<String> = Default::default();
    for (kind, id) in events {
        match kind {
            thor_core::event_store::EventKind::ItemServed => *served.entry(id).or_default() += 1,
            thor_core::event_store::EventKind::ItemMarkedUseful
            | thor_core::event_store::EventKind::ItemMarkedNoise => {
                judged.insert(id);
            }
            _ => {}
        }
    }
    let live = serve::live::live_items(&store);
    let pinned: Vec<_> = live
        .iter()
        .filter(|li| li.item.kind.can_fire())
        .filter(|li| li.item.bindings.iter().any(|b| matches!(b, model::item::Binding::Always)))
        .collect();
    if pinned.is_empty() {
        return "pinned: nothing is pinned".to_string();
    }
    let never_judged = pinned.iter().filter(|li| !judged.contains(&li.id)).count();
    // A share of nothing is not a share. On a fresh store this line said "0% of
    // every serving" beside "4 never examined", which reads as an alarm about a
    // memory that has simply not been used yet - the first thing a new user
    // sees, and wrong.
    if served.values().sum::<usize>() == 0 {
        return format!(
            "pinned: {} item(s), served at every session start - nothing has been served yet, so there is nothing to weigh them against",
            pinned.len()
        );
    }
    let their_servings: usize = pinned.iter().map(|li| served.get(&li.id).copied().unwrap_or(0)).sum();
    let all: usize = served.values().sum();
    format!(
        "pinned: {} item(s), {never_judged} never examined by anything, together {:.0}% of every serving this store has made - decay skips them on purpose and the judgement debt only asks once per verdict, so this share is the part of your memory almost nothing routinely re-reads",
        pinned.len(),
        100.0 * their_servings as f64 / all.max(1) as f64
    )
}

/// The one definition of "this check can refuse something", shared by
/// `proof_line` and `teeth_line` so the two can never disagree about it.
fn can_refuse(item: &model::item::Item) -> bool {
    let always = item.bindings.iter().any(|b| matches!(b, model::item::Binding::Always));
    let command = item
        .bindings
        .iter()
        .any(|b| matches!(b, model::item::Binding::Target { kind: model::item::TargetKind::Command, .. }));
    match &item.check {
        Some(model::item::Check::Absent { .. }) | Some(model::item::Check::AbsentAll { .. }) => true,
        Some(model::item::Check::Forbidden { .. }) => always || command,
        // A `requires` check reaches wherever its Command target says (see
        // `absent_guard::find_requires_violation`) - never through Always,
        // which the gate refuses outright for this check kind (ground 23):
        // there is no content match left to narrow it with, so an
        // Always-bound one would either never fire or fire on everything,
        // and neither is a shape the store can hold any more. `command`
        // alone decides it here.
        Some(model::item::Check::Requires { .. }) => command,
        _ => false,
    }
}

/// THOR's own records that share the gate's `gate_stood_aside` kind without
/// being a stand-aside: the attester's verification of every write and every
/// refusal at each Stop, the evaluation validator's record of a filing, and
/// the A4 cutoff record written by `retest`. None of them is the gate holding
/// off, so `gate_line` never counts one as a stand-aside.
const RECORDS_NOT_STAND_ASIDES: &[&str] = &[model::store::ATTESTER_ACTOR, model::store::EVAL_VALIDATOR_ACTOR, "retest"];

/// How often the gate actually did its job, read straight out of the log.
///
/// THE POINT OF THIS LINE. Everything else in this report counts what the
/// store CONTAINS. This is the only line that counts what it DID. Until
/// 2026-08-08 no such number existed anywhere, which is how a gate that
/// stood down after one refusal per file per session survived unnoticed from
/// the first day of 2.0: an allow left no trace, so the failure mode was
/// literally unobservable. Two counts, deliberately side by side - a refusal
/// is visible to whoever it refused, a stand-aside is visible to nobody.
///
/// Reads history, so the numbers are lifetime totals for this store, not for
/// this session. That is the useful frame: "this memory has refused 40
/// writes" is the sentence that says whether 2.0 works.
pub fn gate_line(db: &Path) -> String {
    let Ok(store) = EventStore::open_existing(db) else {
        return "gate: store unreadable, not checked".to_string();
    };
    let Ok(events) = store.get_all_events() else {
        return "gate: log unreadable, not checked".to_string();
    };
    let mut refused = 0usize;
    let mut stood_aside = 0usize;
    let mut rules: std::collections::BTreeMap<String, usize> = Default::default();
    let mut sessions: std::collections::BTreeMap<String, usize> = Default::default();
    let mut actors: std::collections::BTreeMap<String, usize> = Default::default();
    for event in &events {
        match event.kind {
            thor_core::event_store::EventKind::GateRefused => {
                refused += 1;
                *rules.entry(event.entity_id.clone()).or_default() += 1;
                *sessions.entry(event.session_id.clone()).or_default() += 1;
                *actors.entry(event.actor.clone()).or_default() += 1;
            }
            thor_core::event_store::EventKind::GateStoodAside if !RECORDS_NOT_STAND_ASIDES.contains(&event.actor.as_str()) => {
                stood_aside += 1
            }
            _ => {}
        }
    }
    if refused == 0 && stood_aside == 0 {
        return "gate: has never refused a write, and has never stood aside from one".to_string();
    }
    // WHICH rules, not just how many, since 2026-08-14. This is the one line
    // that claims 2.0 works, and it was unreadable in the exact case that
    // matters: a probe fired against the real store writes a refusal that
    // counts the same as a real one (13 of the first 13 were probes, recorded
    // 2026-08-08). A total cannot be un-mixed afterwards. The ids can - a
    // throwaway probe id is obvious on sight, and a rule that has really
    // stopped something deserves to be named anyway.
    let mut named: Vec<(String, usize)> = rules.into_iter().collect();
    named.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    // And HOW CONCENTRATED, because that is the shape a probe run has: a
    // dozen refusals in one sitting is somebody testing the gate, a dozen
    // spread over a dozen sessions is the gate doing its job. Nothing marks a
    // write as a probe, so this does not decide it - it shows the reader the
    // one thing that tells the two apart.
    let busiest = sessions.values().copied().max().unwrap_or(0);
    let mut out = vec![format!(
        "gate: {refused} refusals by {} distinct rules across {} session(s), the busiest of them accounting for {busiest} - a burst inside one session is what a probe run looks like, not what protection looks like. {stood_aside} stand-asides, lifetime (a stand-aside is the stale-rule nudge holding off for the rest of a session; no prohibition stands aside SINCE 2026-08-08, so older ones in this total are real prohibitions that did)",
        named.len(),
        sessions.len()
    )];
    // WHO tried the write, and shown only when that is more than one answer.
    // A refusal written by "hook" is a tool call the guard actually stopped;
    // one written by a CLI is somebody testing the gate against the real
    // store, which is the thing that quietly inflates this whole line (see
    // the store's own note on probing against a throwaway instead). Measured
    // 2026-08-14: all of them were "hook", so a per-actor list would have been
    // one line saying nothing - it earns its place the moment a second actor
    // appears, and not before.
    if actors.len() > 1 {
        for (actor, count) in &actors {
            out.push(format!("  written by: {actor} ({count}x)"));
        }
    }
    for (id, count) in named.iter().take(NAMES_AT_MOST) {
        out.push(format!("  refused by: {id} ({count}x)"));
    }
    if named.len() > NAMES_AT_MOST {
        out.push(format!("  refused by: and {} more rule(s), not named here", named.len() - NAMES_AT_MOST));
    }
    out.join("\n")
}

/// How far back the refusal outcomes line looks for real refusals.
const REFUSAL_OUTCOMES_DAYS: i64 = 30;

/// FALSE BLOCKS AS A NUMBER, from the log alone (`model::refusal_outcome`):
/// every real refusal (field evidence, as `teeth_line` counts it, judged
/// against the check in force when it happened, so a rule weakened since
/// still counts) in the last `REFUSAL_OUTCOMES_DAYS` days, and how many were
/// followed within `OUTCOME_WINDOW_DAYS` by a weakening or retract of the
/// refusing rule. A proxy, never a verdict: it feeds no ranking and no gate,
/// and a noise mark is listed but never counted (the refused agent can write
/// one). An event carries no time of its own, so a write is dated by the
/// newest served, refused or verified event before it. Silent with no
/// refusals; `full` lists each probable false block.
pub fn refusal_outcomes_line(db: &Path, full: bool) -> Vec<String> {
    refusal_outcomes_at(db, full, serve::time::now_unix())
}

fn refusal_outcomes_at(db: &Path, full: bool, now: i64) -> Vec<String> {
    use model::refusal_outcome::{fates, rate_words, Fate, OUTCOME_WINDOW_DAYS};
    use thor_core::event_store::EventKind;
    let Ok(store) = EventStore::open_existing(db) else { return Vec::new() };
    // Every refusal below asks the log about its session; one pass, not one per refusal.
    store.cache_session_reads();
    let Ok(events) = store.get_all_events() else { return Vec::new() };
    let refused: std::collections::BTreeSet<&str> = events
        .iter()
        .filter(|e| e.kind == EventKind::GateRefused && e.actor == model::store::ASSISTANT_DELIVERY_ACTOR)
        .map(|e| e.entity_id.as_str())
        .collect();
    if refused.is_empty() {
        return Vec::new();
    }
    let mut at: std::collections::HashMap<i64, i64> = Default::default();
    let mut last: Option<i64> = None;
    for e in &events {
        if matches!(e.kind, EventKind::ItemServed | EventKind::GateRefused | EventKind::GateStoodAside) {
            let body = serde_json::from_str::<serde_json::Value>(&e.body).unwrap_or(serde_json::Value::Null);
            let when = body.get("served_at").or_else(|| body.get("at")).and_then(|v| v.as_str());
            last = when.and_then(serve::time::unix_from_iso8601).or(last);
        }
        if let (Some(t), true) = (last, refused.contains(e.entity_id.as_str())) {
            at.insert(e.seq, t);
        }
    }
    let horizon = now - REFUSAL_OUTCOMES_DAYS * 86_400;
    let window = OUTCOME_WINDOW_DAYS * 86_400;
    let mut all = Vec::new();
    // Folding the live set to name the known projects is the slow part of
    // asking a command rule's refusals; do it once, and only if one is asked.
    let projects = std::cell::OnceCell::new();
    let known = || projects.get_or_init(|| serve::teeth::known_projects(&store)).clone();
    for id in refused {
        let Ok(own) = store.get_events_by_entity(id) else { continue };
        let verdicts = model::store::recorded_refusal_verdicts(&own);
        let is_real = |e: &thor_core::event_store::Event, item: &model::item::Item, since: i64| {
            at.get(&e.seq).is_none_or(|t| *t >= horizon)
                && serve::attest::is_field_refusal_known(&store, e, id, item, since, &verdicts, &known)
        };
        let follows = |refusal: &thor_core::event_store::Event, later: &thor_core::event_store::Event| {
            match (at.get(&refusal.seq), at.get(&later.seq)) {
                (Some(then), Some(after)) => after - then <= window,
                _ => true,
            }
        };
        all.extend(fates(id, &own, &is_real, &follows).into_iter().map(|f| {
            // The call that was refused, as the log kept it (redacted when
            // it was written) - what tells a wrong refusal from a right one.
            let call = own.iter().find(|e| e.seq == f.refusal_seq).and_then(model::store::refused_call_excerpt);
            (at.get(&f.refusal_seq).copied(), f, call)
        }));
    }
    if all.is_empty() {
        return Vec::new();
    }
    let probable: Vec<_> = all.iter().filter(|(_, f, _)| f.probable_false_block()).map(|(_, f, call)| (f, call)).collect();
    let noisy = all.iter().filter(|(_, f, _)| f.fate == Fate::MarkedNoise).count();
    let young = all.iter().filter(|(t, _, _)| t.is_some_and(|t| now - t < window)).count();
    let mut line = format!(
        "refusal outcomes: {} real refusal(s) in the last {REFUSAL_OUTCOMES_DAYS} days, {} followed by weakening or retract of the \
         refusing rule within {OUTCOME_WINDOW_DAYS} days (probable false blocks), {}. A proxy read from the log: a rule can change for \
         other reasons, and one rule's refusals are counted one by one.",
        all.len(),
        probable.len(),
        rate_words(probable.len(), all.len())
    );
    if noisy > 0 {
        line.push_str(&format!(" {noisy} more were marked noise afterwards and are not counted."));
    }
    if young > 0 {
        line.push_str(&format!(" {young} refusal(s) are younger than the window and may still be followed."));
    }
    // Each probable false block is named (the rule, the call it refused, what
    // followed); a long list stops at `FALSE_BLOCKS_LISTED` unless --full.
    let shown = if full { probable.len() } else { probable.len().min(model::store::FALSE_BLOCKS_LISTED) };
    if probable.len() > shown {
        line.push_str(" --full lists them all.");
    }
    let mut out = vec![line];
    for (f, call) in probable.iter().take(shown) {
        let how = match &f.fate {
            Fate::Weakened(labels) => format!("weakened ({labels})"),
            _ => "retracted".to_string(),
        };
        let call = call.as_ref().map_or("a call whose text the log did not keep".to_string(), |c| format!("'{c}'"));
        out.push(format!(
            "  probable false block: '{}' refused {call} at log seq {}, then {how} at log seq {}",
            f.rule,
            f.refusal_seq,
            f.fate_seq.unwrap_or_default()
        ));
    }
    if probable.len() > shown {
        out.push(format!("  probable false block: and {} more, not listed here", probable.len() - shown));
    }
    out
}

/// How long a doctor run spends on the false-block sweep: the replay of what
/// is new in the record of real calls, and of what a changed rule reaches. A
/// run that is not finished says so nowhere (it is silent at zero) and carries
/// on at the next one; `serve sweep` has no such short limit.
const DOCTOR_SWEEP_SECS: u64 = 8;

/// The false-block sweep (`serve::sweep`): the rules that would refuse real
/// calls of the last 30 days that ran fine, and the answer-rulebook entries
/// that would fire on real replies that went through. Silent at zero, so a
/// store with nothing to report has no line. The sweep is incremental: this
/// is a short, bounded pass of it.
pub fn false_block_sweep_lines(db: &Path) -> Vec<String> {
    let Ok(store) = EventStore::open_existing(db) else { return Vec::new() };
    let report = serve::sweep::run(db, &store, &serve::sweep::Opts::new(std::time::Duration::from_secs(DOCTOR_SWEEP_SECS)));
    serve::sweep::doctor_lines(&report)
}

pub fn crowding_line(db: &Path, checkouts: Option<&Path>, full: bool) -> String {
    let Ok(store) = EventStore::open_existing(db) else {
        return "crowding: store unreadable, not checked".to_string();
    };
    let Some(root) = checkouts else {
        return "crowding: pass --checkouts <dir> to see how many facts never win a place".to_string();
    };

    let roots = checkout_roots(&store, root);
    // Loaded ONCE for every probe below - see `serve::serve_with_decay`'s own
    // doc comment for the day this loop's own command anchors doubled this
    // line's runtime by re-folding an unchanging log per probe.
    let decay = serve::decay::DecayContext::load(&store);
    let mut prober = Prober::new(&store, &decay);
    let mut text = project_crowding_text(&store, &roots, &mut prober, full);
    let global = global_crowding_lines(&store, &roots, &mut prober, full);
    if !global.is_empty() {
        text.push('\n');
        text.push_str(&global.join("\n"));
    }
    text
}

/// What one crowding probe found: the id of every item that applies at the
/// probed trigger, in rank order, and the ids the block would actually show.
struct Probe {
    all: Vec<String>,
    shown: std::collections::HashSet<String>,
}

/// Probes anchors the way the runtime would serve them (`serve_with_decay`),
/// for the crowding report.
///
/// A store with hundreds of anchors needs thousands of probes (17 s of the
/// health check on a 270k-event store), and almost none of that was ranking:
/// a probe read and parsed its candidate pool (9 ms) and built its input by
/// parsing the command or path (about 2 ms), and neither depends on the
/// project being probed. The pool depends only on the input's moments and
/// target kinds (`live::candidate_pool_key`) and the input only on the anchor,
/// so each is built once and reused. The ranking is `serve_from_candidates`,
/// the same code `serve_with_decay` runs.
struct Prober<'a> {
    store: &'a EventStore,
    decay: &'a serve::decay::DecayContext,
    /// (is a command, the anchor) -> the input for it, no project yet.
    inputs: std::collections::HashMap<(bool, String), serve::input::ServeInput>,
    pools: std::collections::HashMap<(Vec<String>, Vec<String>), Vec<serve::live::LiveItem>>,
}

impl<'a> Prober<'a> {
    fn new(store: &'a EventStore, decay: &'a serve::decay::DecayContext) -> Self {
        Prober { store, decay, inputs: Default::default(), pools: Default::default() }
    }

    /// What a command (`is_command`) or a file anchor serves in `project`.
    fn probe(&mut self, is_command: bool, anchor: &str, project: &str) -> Probe {
        let mut input = self
            .inputs
            .entry((is_command, anchor.to_string()))
            .or_insert_with(|| {
                let mut input = serve::input::ServeInput::default();
                if is_command {
                    input.add_command(anchor);
                } else {
                    input.add_file(anchor);
                }
                input
            })
            .clone();
        // The project is what `rank::select` filters on. Leaving it out serves
        // only global items and makes every pool look empty.
        input.project = Some(project.to_string());
        let store = self.store;
        let pool = self
            .pools
            .entry(serve::live::candidate_pool_key(&input))
            .or_insert_with(|| serve::live::candidates_for(store, &input));
        let served = serve::serve_from_candidates(pool, &input, self.decay);
        Probe {
            all: served.all.iter().map(|r| r.id.clone()).collect(),
            shown: served.selection.shown.iter().map(|r| r.id.clone()).collect(),
        }
    }
}

/// The crowding report for items that belong to a project: each is probed at
/// its own anchors, in its own project. A GLOBAL item is not probed here -
/// see `global_crowding_lines`.
fn project_crowding_text(
    store: &EventStore,
    roots: &std::collections::BTreeMap<String, std::path::PathBuf>,
    prober: &mut Prober,
    full: bool,
) -> String {
    // Every distinct anchor that resolves, and the pool it stands for.
    let mut anchors: std::collections::BTreeSet<(String, String)> = Default::default();
    // Every distinct COMMAND binding value of a live item whose project
    // resolves to one of the checkouts above - probed the same way a Path
    // anchor is, just below, but with nothing on disk to check for existence
    // against: a command is text a Rule/Orientation names, never a file this
    // function could look up. Added 2026-09-17 (measured, Printer-stuff eval
    // 2): `serve why --command "node tools/push.mjs"` showed 7 items
    // applying and 4 shown, while this line, probing files only, reported
    // nothing for that project - the crowd was real and this line could not
    // see it.
    let mut command_anchors: std::collections::BTreeSet<(String, String)> = Default::default();
    for li in serve::live::live_items(store).iter().filter(|li| li.item.kind.can_fire()) {
        let Some(project) = li.item.project.clone() else { continue };
        let Some(base) = roots.get(&project) else { continue };
        for binding in &li.item.bindings {
            match binding {
                model::item::Binding::Target { kind: model::item::TargetKind::Path, value } => {
                    if value.contains(':') || value.starts_with('/') || value.starts_with("\\\\") {
                        continue;
                    }
                    if base.join(value.replace('\\', "/")).exists() {
                        anchors.insert((project.clone(), value.clone()));
                    }
                }
                model::item::Binding::Target { kind: model::item::TargetKind::Command, value } => {
                    command_anchors.insert((project.clone(), value.clone()));
                }
                _ => {}
            }
        }
    }
    if anchors.is_empty() && command_anchors.is_empty() {
        return "crowding: no resolvable anchor to probe, nothing to say".to_string();
    }

    let (mut eligible, mut reachable) = (
        std::collections::HashSet::new(),
        std::collections::HashSet::new(),
    );
    let mut worst: Option<(usize, String, String)> = None;
    // Where each item lost, so the ones that never win anywhere can be NAMED
    // and folded. Same reason as the decay line: "185 items are invisible" is
    // a number nobody can act on, and the loop that produced it is the only
    // place that ever knew which 185.
    let mut lost_at: std::collections::BTreeMap<String, String> = Default::default();
    for (project, path) in &anchors {
        let probe = prober.probe(false, path, project);
        for id in &probe.all {
            eligible.insert(id.clone());
            if probe.shown.contains(id) {
                reachable.insert(id.clone());
            } else {
                lost_at.entry(id.clone()).or_insert_with(|| format!("{path} in {project}"));
            }
        }
        if worst.as_ref().map(|(n, _, _)| probe.all.len() > *n).unwrap_or(true) {
            worst = Some((probe.all.len(), project.clone(), path.clone()));
        }
    }
    // The identical probe, for a command anchor instead of a file - see
    // `command_anchors`'s own doc comment above for why this exists. Named
    // "<command> (command)" everywhere a path would otherwise print bare, so
    // a reader can tell the two apart in `lost_at` and in the worst-pool
    // line below without opening the store.
    for (project, command) in &command_anchors {
        let probe = prober.probe(true, command, project);
        let where_lost = format!("{command} (command)");
        for id in &probe.all {
            eligible.insert(id.clone());
            if probe.shown.contains(id) {
                reachable.insert(id.clone());
            } else {
                lost_at.entry(id.clone()).or_insert_with(|| format!("{where_lost} in {project}"));
            }
        }
        if worst.as_ref().map(|(n, _, _)| probe.all.len() > *n).unwrap_or(true) {
            worst = Some((probe.all.len(), project.clone(), where_lost));
        }
    }

    let invisible = eligible.len() - reachable.len();
    if invisible == 0 {
        return format!("crowding: none - every one of {} eligible item(s) wins a place somewhere", eligible.len());
    }
    let headline = match worst {
        Some((n, project, path)) if n > serve::render::MAX_ITEMS => format!(
            "crowding: {invisible} of {} item(s) are eligible at their own trigger and NEVER shown there, \
             because a block holds {} - they are current and correctly bound, just permanently outranked; \
             worst pool: {n} claimants at {path} in {project}. Fold them into the item that already carries \
             their ground, or archive them - a place that is full is not fixed by adding another claimant",
            eligible.len(),
            serve::render::MAX_ITEMS
        ),
        _ => format!(
            "crowding: {invisible} of {} item(s) are eligible at their own trigger and never shown there",
            eligible.len()
        ),
    };
    // An item already judged deliberately crowded is not a repair job, and
    // listing it again every run is what turns a work list into wallpaper.
    // Judging one therefore SHRINKS this list - that is the finish line the
    // grind needs - while the count of them is still said out loud, so the
    // list getting shorter can never be mistaken for the crowd getting
    // smaller.
    let judged_crowded: std::collections::HashSet<String> = serve::live::live_items(store)
        .iter()
        .filter(|li| li.item.tags.iter().any(|t| t == model::store::CROWDED_ON_PURPOSE_TAG))
        .map(|li| li.id.clone())
        .collect();
    let mut never_won: Vec<(&String, &String)> = lost_at
        .iter()
        .filter(|(id, _)| !reachable.contains(*id) && !judged_crowded.contains(*id))
        .collect();
    never_won.sort_by(|a, b| a.0.cmp(b.0));
    let settled = invisible - never_won.len().min(invisible);
    let mut out = vec![headline];
    if settled > 0 {
        out.push(format!("  outranked: {settled} of them already judged as crowded on purpose, not listed"));
    }
    for (id, where_lost) in never_won.iter().take(name_cap(full)) {
        out.push(format!("  outranked: {id} at {where_lost}"));
    }
    if never_won.len() > name_cap(full) {
        out.push(format!(
            "  outranked: and {} more, not named here",
            never_won.len() - name_cap(full)
        ));
    }
    out.join("\n")
}

/// The crowding report for GLOBAL items (no project): each is probed at its
/// own anchors in EVERY registered working copy, the way the runtime would
/// rank it there, and named with the project(s) where it applies but is not
/// shown.
///
/// WHY A SECOND PASS. `project_crowding_text` probes an item only in the
/// project that owns it, so a global item was never probed at its own anchor
/// at all, and counted only when a project's probe happened to reach it
/// (measured 2026-10-07, field test 12: six global command-anchored items
/// hidden in at least one registered project, one of them in all six, while
/// the line said "1 of 1112"). A global item hidden in one project is not
/// healthy because it wins in another: the project of the incident is the
/// one that matters. Reported on its own line group, so the project-scoped
/// numbers above keep meaning exactly what they meant. Empty when no global
/// item is outranked anywhere.
fn global_crowding_lines(
    store: &EventStore,
    roots: &std::collections::BTreeMap<String, std::path::PathBuf>,
    prober: &mut Prober,
    full: bool,
) -> Vec<String> {
    let live = serve::live::live_items(store);
    // (is_command, anchor) -> the global items bound to it.
    let mut anchors: std::collections::BTreeMap<(bool, String), std::collections::BTreeSet<String>> = Default::default();
    for li in live.iter().filter(|li| li.item.kind.can_fire() && li.item.project.is_none()) {
        for binding in &li.item.bindings {
            match binding {
                model::item::Binding::Target { kind: model::item::TargetKind::Path, value } => {
                    if value.contains(':') || value.starts_with('/') || value.starts_with("\\\\") {
                        continue;
                    }
                    anchors.entry((false, value.clone())).or_default().insert(li.id.clone());
                }
                model::item::Binding::Target { kind: model::item::TargetKind::Command, value } => {
                    anchors.entry((true, value.clone())).or_default().insert(li.id.clone());
                }
                _ => {}
            }
        }
    }
    // item id -> anchor label -> the projects where it applies and is not shown.
    let mut hidden: std::collections::BTreeMap<String, std::collections::BTreeMap<String, Vec<String>>> =
        Default::default();
    for ((is_command, anchor), ids) in &anchors {
        for (project, base) in roots {
            // A path anchor only means something where the file exists.
            if !*is_command && !base.join(anchor.replace('\\', "/")).exists() {
                continue;
            }
            let probe = prober.probe(*is_command, anchor, project);
            for id in ids {
                if probe.all.contains(id) && !probe.shown.contains(id) {
                    let label = if *is_command { format!("{anchor} (command)") } else { anchor.clone() };
                    hidden.entry(id.clone()).or_default().entry(label).or_default().push(project.clone());
                }
            }
        }
    }
    // Same rule as the project list: an item already judged crowded on
    // purpose is counted, not listed.
    let judged_crowded: std::collections::HashSet<&str> = live
        .iter()
        .filter(|li| li.item.tags.iter().any(|t| t == model::store::CROWDED_ON_PURPOSE_TAG))
        .map(|li| li.id.as_str())
        .collect();
    let listed: Vec<(&String, &std::collections::BTreeMap<String, Vec<String>>)> =
        hidden.iter().filter(|(id, _)| !judged_crowded.contains(id.as_str())).collect();
    if hidden.is_empty() {
        return Vec::new();
    }
    let mut out = vec![format!(
        "crowding (global): {} global item(s) are eligible at their own trigger and outranked in at least one project, \
         even where they win elsewhere - they are not shown there until a place is freed",
        hidden.len()
    )];
    let settled = hidden.len() - listed.len();
    if settled > 0 {
        out.push(format!("  outranked: {settled} of them already judged as crowded on purpose, not listed"));
    }
    for (id, places) in listed.iter().take(name_cap(full)) {
        let at: Vec<String> =
            places.iter().map(|(anchor, projects)| format!("{anchor} in {}", projects.join(", "))).collect();
        out.push(format!("  outranked: {id} (global) at {}", at.join("; ")));
    }
    if listed.len() > name_cap(full) {
        out.push(format!("  outranked: and {} more, not named here", listed.len() - name_cap(full)));
    }
    out
}

/// How many live items are bound ONLY to a moment nothing in `serve` ever
/// fires - the shape `model::gate::declare`'s ground 25 now refuses to ADD,
/// but a real store can still hold from before that ground existed: the
/// write gate protects only NEW writes, and says nothing about what is
/// already on disk.
///
/// WHY THIS LINE EXISTS. Ground 25 stops the bleeding going forward, but by
/// itself it is invisible to anyone who does not already know to look for
/// the shape it refuses - and an item bound only to `Answer`/`ClaimDone`
/// (`intent::from_draft`'s own two products, with no caller anywhere in
/// `serve` outside its own tests) fires exactly as silently as it did
/// before the ground shipped. This turns that silence into a worklist: the
/// owner still has to decide what each one should re-bind to (that needs
/// judgement this report cannot supply), but he can no longer be unaware
/// that they exist.
pub fn dead_moment_bindings_line(db: &Path) -> String {
    const NAMED_AT_MOST: usize = 10;
    let Ok(store) = EventStore::open_existing(db) else {
        return "dead moments: store unreadable, not checked".to_string();
    };
    let live = serve::live::live_items(&store);
    let fireable = || live.iter().filter(|li| li.item.kind.can_fire());
    let dead: Vec<&str> = fireable()
        .filter(|li| !li.item.bindings.is_empty())
        .filter(|li| {
            li.item.bindings.iter().all(|b| {
                matches!(b, model::item::Binding::Moment(action) if model::gate::is_dead_moment(*action))
            })
        })
        .map(|li| li.id.as_str())
        .collect();
    // The two other ways a live item can never fire at a tool call. The write
    // gate refuses both for a NEW write (grounds 1 and 26) and counts nothing
    // already stored, so they are counted here, never hidden behind "none".
    let unbound: Vec<&str> = fireable().filter(|li| model::gate::is_unbound(&li.item)).map(|li| li.id.as_str()).collect();
    let project_only: Vec<&str> =
        fireable().filter(|li| model::gate::is_weighted_project_only(&li.item)).map(|li| li.id.as_str()).collect();
    if dead.is_empty() && unbound.is_empty() && project_only.is_empty() {
        return "dead moments: none - every live item's bindings can actually fire".to_string();
    }
    let sections: [(&str, &str, &str, &Vec<&str>); 3] = [
        (
            "dead moments",
            "dead moment",
            "bound ONLY to a moment nothing in serve ever fires (answer/claim_done) - re-bind each one with revise, \
             to Always, a command or a file",
            &dead,
        ),
        (
            "no binding",
            "no binding",
            "with NO binding at all, so they can never fire - bind each one with revise, to Always, a command, a file \
             or a moment, or retract it",
            &unbound,
        ),
        (
            "project-only",
            "project-only",
            "costly or irreversible and bound ONLY to a project name, which reaches a prompt where the owner types \
             that name and never a tool call - bind each one with revise to the file or command it is about, or set \
             severity house_style if the typed name is enough",
            &project_only,
        ),
    ];
    let mut out = Vec::new();
    for (heading, each, what, ids) in sections {
        if ids.is_empty() {
            continue;
        }
        let mut section = format!("{heading}: {} live item(s) {what}", ids.len());
        for id in ids.iter().take(NAMED_AT_MOST) {
            section.push_str(&format!("\n  {each}: {id}"));
        }
        if ids.len() > NAMED_AT_MOST {
            section.push_str(&format!("\n  {each}: and {} more, not named here", ids.len() - NAMED_AT_MOST));
        }
        out.push(section);
    }
    out.join("\n")
}

/// Doctor's `secrets in notes` line: how many live notes already carry a real
/// secret token in their text or falsifier, found with the SAME function the
/// write gate refuses with (`model::secrets`). Silent at zero.
///
/// It says plainly what cannot be done: the log is append-only, a retract
/// keeps history, and the backup is that log pushed to a remote, so a token
/// that is already in cannot be cleaned out, only rotated. It names the note
/// ids and the shape, never a value.
pub fn secrets_in_notes_line(db: &Path) -> Option<String> {
    const NAMED_AT_MOST: usize = 10;
    let store = EventStore::open_existing(db).ok()?;
    let mut hits: Vec<(String, &'static str)> = Vec::new();
    for li in serve::live::live_items(&store) {
        let falsifier = li.item.falsifier.as_deref().unwrap_or("");
        for found in model::secrets::find(&li.item.text).into_iter().chain(model::secrets::find(falsifier)) {
            hits.push((li.id.clone(), found.shape));
        }
    }
    if hits.is_empty() {
        return None;
    }
    let notes: std::collections::BTreeSet<&str> = hits.iter().map(|(id, _)| id.as_str()).collect();
    let mut out = format!(
        "secrets in notes: {} live note(s) carry what looks like a real secret token - the log is append-only and its \
         backup copies it, so history cannot be cleaned: ROTATE each one, then revise the note to say where the secret \
         lives (a file or variable name) instead of its value",
        notes.len()
    );
    for (id, shape) in hits.iter().take(NAMED_AT_MOST) {
        out.push_str(&format!("\n  secret: {id} ({shape})"));
    }
    if hits.len() > NAMED_AT_MOST {
        out.push_str(&format!("\n  secret: and {} more, not named here", hits.len() - NAMED_AT_MOST));
    }
    Some(out)
}

/// Doctor's `command anchors never fired` line (step 40): the live rules and
/// orientations whose command anchors have ALL never fired, on a real call or on
/// the owner's own prompt, and that no call can reach (`serve::dead_anchor`: a
/// short tool name, a fragment without its program, a stored sentence, a
/// program that is gone). Never called dead: a keyword anchor still fires when
/// the owner types it. Each is named with its anchor and the reason class; the
/// ones with no other binding left are named first.
///
/// WHAT IT STANDS ON, said in the line itself: this shell's `PATH`, THOR's own
/// `bin` folder and the real calls in the log. A command that lives only on
/// another machine is never called a non-program when some real call ran it,
/// and when none did it is only called never reached.
pub fn command_anchors_never_fired_line(db: &Path, full: bool) -> String {
    let Ok(store) = EventStore::open_existing(db) else {
        return "command anchors never fired: store unreadable, not checked".to_string();
    };
    let corpus = serve::legit::Corpus::load(&store);
    let judge = serve::dead_anchor::Judge::new(&corpus, &mcp::served_tool_tails(), &serve::dead_anchor::own_dirs(Some(db)));
    let mut verdicts: std::collections::HashMap<String, Option<serve::dead_anchor::Reason>> = Default::default();
    let mut with_anchors = 0usize;
    // (fires nowhere, id, project, what, class)
    let mut dead: Vec<(bool, Rot, &'static str)> = Vec::new();
    for li in serve::live::live_items(&store).iter().filter(|li| li.item.kind.can_fire()) {
        let anchors = serve::dead_anchor::command_anchors(&li.item);
        if anchors.is_empty() {
            continue;
        }
        with_anchors += 1;
        let found: Vec<(&str, Option<serve::dead_anchor::Reason>)> = anchors
            .iter()
            .map(|a| (*a, verdicts.entry((*a).to_string()).or_insert_with(|| judge.judge(a)).clone()))
            .collect();
        if found.iter().any(|(_, reason)| reason.is_none()) {
            continue;
        }
        let nowhere = li.item.bindings.iter().all(|b| {
            matches!(b, model::item::Binding::Target { kind: model::item::TargetKind::Command, .. })
        });
        let class = found[0].1.as_ref().map(|r| r.class()).unwrap_or("");
        let what = found
            .iter()
            .filter_map(|(a, r)| r.as_ref().map(|r| format!("'{a}' - {}", r.short())))
            .collect::<Vec<_>>()
            .join("; ");
        let what = if nowhere { what } else { format!("{what} (the item has another binding)") };
        let project = li.item.project.clone().unwrap_or_else(|| "global".to_string());
        dead.push((nowhere, Rot { id: li.id.clone(), project, what }, class));
    }
    if dead.is_empty() {
        return format!(
            "command anchors never fired: none - of {with_anchors} live item(s) with command anchors, every one has an anchor that fired (at a call or at the owner's prompt), one a call can reach, or one that could not be judged"
        );
    }
    dead.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.id.cmp(&b.1.id)));
    let nowhere = dead.iter().filter(|d| d.0).count();
    let mut by_class: std::collections::BTreeMap<&str, usize> = Default::default();
    for d in &dead {
        *by_class.entry(d.2).or_default() += 1;
    }
    let classes = by_class.iter().map(|(c, n)| format!("{n} {c}")).collect::<Vec<_>>().join(", ");
    let rot: Vec<Rot> = dead.into_iter().map(|d| d.1).collect();
    let mut lines = vec![format!(
        "command anchors never fired: {} of {with_anchors} live item(s) with command anchors have ONLY anchors that never fired - no real call started with any of them and no prompt of the owner's contained them ({nowhere} of these have no other binding, so only the owner typing the phrase can fire them; by class: {classes}). As a command an anchor matches only a call that starts with it, so each is named with the class of why a call cannot reach it - judged against this shell's PATH, THOR's own bin folder and the real calls in the log. Decide for each: a command (put the program word first, or use the whole tool name), a keyword the owner types (leave it), or a file or an action (bind a path or a moment instead); revise it or retract it, each one named below",
        rot.len()
    )];
    lines.extend(name_rot("command anchor never fired", &rot, full));
    lines.join("\n")
}

/// Doctor's volume lines (step 28): for the last 24 hours and 7 days, how many
/// facts were shown in full and how many repeats were left out, the five most
/// shown facts, and how many Stops held a turn per session on average - so the
/// owner sees whether sessions get nagged. Read from the sidecar the hooks
/// keep (`serve::volume`); nothing is written.
pub fn volume_lines(db: &Path) -> Vec<String> {
    serve::volume::lines(db, serve::time::now_unix())
}

pub fn report(
    db: &Path,
    index_db: Option<&Path>,
    repo: Option<&Path>,
    replica: Option<(&str, &str)>,
    model_dir: Option<&Path>,
    checkouts: Option<&Path>,
    full: bool,
    checkout_project: Option<&str>,
) -> Vec<String> {
    // Measured before ANYTHING else in this function, including `store_line`
    // right below - see `wal_line_from_size`'s doc comment for why that
    // ordering is load-bearing rather than stylistic: `store_line` opens and
    // closes this report's own connection to `db`, and that is enough for
    // SQLite to have reclaimed or discarded the `-wal` file by the time a
    // measurement taken any later would read it.
    let wal_size = wal_raw_size(db);

    let mut lines = vec![store_line(db)];
    // Silent unless the WAL has actually outgrown its own store - see
    // `wal_line_from_size`'s own doc comment for why that is the right
    // default rather than a "wal: fine" line on every ordinary run.
    lines.extend(wal_line_from_size(wal_size, db));
    lines.push(dead_moment_bindings_line(db));
    // Silent at zero: a store with no secret in a note has nothing to say.
    lines.extend(secrets_in_notes_line(db));
    lines.push(command_anchors_never_fired_line(db, full));
    lines.push(code_index_line(index_db, repo));
    lines.push(replica_line(db, replica));
    // Silent unless this machine has ever completed a ship - see
    // `ship_line`'s own doc comment for why "never shipped" is not itself a
    // finding worth a permanent line.
    lines.extend(ship_line(db));
    // Where the last off-site copy went, or that it is failing right now.
    lines.extend(backup_line(db));
    lines.extend(drain_refusals_line(db));
    lines.extend([
        falsifier_line(db),
        proof_line(db, full),
        gate_line(db),
        teeth_line(db, full),
        pinned_line(db),
        decay_line(db, checkouts, full),
        crowding_line(db, checkouts, full),
        unjudged_line(db),
        semantic_line(model_dir),
        orphan_projects_line(db, checkouts),
    ]);
    lines.extend(working_copy_lines(db));
    // Silent unless a real refusal is on the log.
    lines.extend(refusal_outcomes_line(db, full));
    // Silent unless a rule would refuse real calls that ran fine.
    lines.extend(false_block_sweep_lines(db));
    // Silent unless something is actually owed - see `judgement_debt_line`'s
    // own doc comment for why this exists alongside `unjudged_line` above
    // rather than in place of it. `full` also lifts its named-items cap, the
    // same flag `decay_line`/`crowding_line` already take it from.
    lines.extend(judgement_debt_line(db, checkout_project, full));
    lines.extend(test_signal_line(db, checkout_project));
    lines.extend(volume_lines(db));
    lines.extend(evaluations_line(db, checkout_project));
    lines.push(cadence_line(db));
    lines.extend(work_clock_line(db, checkout_project));
    lines.extend(evaluation_batch_line(db, checkout_project, full));
    // Silent unless the store holds an open point / a rule names a
    // response-rulebook entry that is not there.
    lines.extend(open_points_line(db, full));
    lines.extend(owner_queue_line(db));
    lines.extend(answer_guard_line(db));
    // Silent unless two live checks on the same file actually disagree right
    // now - see `contradiction_line`'s own doc comment for how this differs
    // from GROUND 29 (`model::gate::opposing_literals`), which refuses a
    // structural opposition at write time and never runs here.
    lines.extend(contradiction_line(db, checkouts));
    // Silent unless a THOR bug candidate is waiting (`serve::thor_bugs`); the
    // full report also says the mode once a candidate or a mode exists.
    lines.extend(serve::thor_bugs::doctor_line(db));
    lines
}

/// THE TEST SIGNAL (part G): whether step 1 of the evaluation has to run the
/// project's tests again, from the real-call corpus (`serve::test_signal`).
/// `None` outside a project: there is no project whose tests it could name.
pub fn test_signal_line(db: &Path, checkout_project: Option<&str>) -> Option<String> {
    let project = checkout_project?;
    let now = serve::time::now_unix();
    Some(serve::test_signal::line(&serve::test_signal::read(db, project, now), now))
}

/// The last line of `doctor --eval`: what it printed and what it left out, so a
/// reader who needs another line knows it is one command away and not missing.
pub const EVAL_FOOTER: &str = "doctor --eval: only the lines the evaluation routine asks for (tool server and installed programs, when one is wrong; ship, provable rules, gate, teeth, pinned, decay, crowding, false-block sweep, judgement debt, test signal, contradictions; THOR bug candidates, while one is waiting); a line the store has nothing to say about is silent, as in the full report. For anything else - the store and chain line, command anchors, hook launch failures, refusal outcomes, open points, the evaluation clock - run doctor without --eval; that takes longer, so wait for it to end and do not quote it before it has finished";

/// The lines of `report` that the evaluation routine (`eval-command.example.md`)
/// asks a reader to quote - `provable rules`, `gate`, `teeth`, `pinned`, `decay`,
/// `crowding`, `ship`, `judgement debt` and `contradictions` - and nothing else,
/// for `doctor --eval`.
///
/// THE SAME FUNCTIONS, NEVER A SECOND COMPUTATION. Each line below is the call
/// `report` makes, with the same arguments, in the same order, so a line here
/// cannot say something its twin in the full report does not (a test holds
/// them equal on one store). What is left out is what the routine never reads:
/// the store/chain line, the command anchors, the code index and replica, the
/// backup, the falsifier count, the refusal outcomes, the working copies, the
/// evaluation clock and the open points. The full report still has them.
pub fn eval_report(
    db: &Path,
    checkouts: Option<&Path>,
    full: bool,
    checkout_project: Option<&str>,
) -> Vec<String> {
    let mut lines = Vec::new();
    lines.extend(ship_line(db));
    lines.extend([
        proof_line(db, full),
        gate_line(db),
        teeth_line(db, full),
        pinned_line(db),
        decay_line(db, checkouts, full),
        crowding_line(db, checkouts, full),
    ]);
    lines.extend(false_block_sweep_lines(db));
    lines.extend(judgement_debt_line(db, checkout_project, full));
    lines.extend(test_signal_line(db, checkout_project));
    lines.extend(contradiction_line(db, checkouts));
    lines.extend(serve::thor_bugs::open_line(db));
    lines
}

/// THOR checks its own doctor against its own routine: every labelled line
/// `doctor --eval` prints must be one the installed evaluation routine names,
/// or the agent is left to guess what the line means. A label the routine
/// never mentions becomes a THOR bug candidate (`serve::thor_bugs`). Silent
/// when the routine is not installed. Writes only the candidates sidecar.
pub fn note_routine_gaps(db: &Path, printed: &[String]) {
    let Some(path) = crate::install::default_eval_command_path() else { return };
    let Ok(routine) = std::fs::read_to_string(path) else { return };
    note_routine_gaps_against(db, &routine, printed);
}

/// `note_routine_gaps` against a routine text in hand; the labels it noted.
pub fn note_routine_gaps_against(db: &Path, routine: &str, printed: &[String]) -> Vec<String> {
    let gaps = serve::thor_bugs::routine_gaps(routine, printed);
    for label in &gaps {
        serve::thor_bugs::add(db, serve::thor_bugs::doctor_routine_finding(label));
    }
    gaps
}

/// The three-way answer `--gate` needs. See `gate_verdict` for exactly what
/// separates `Clean` from `Failing`, and why a store that could not be
/// judged is neither.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateVerdict {
    /// The store could not be judged at all: missing, or present but
    /// unreadable. The caller must treat this as a PASS, never a failure -
    /// see `gate_verdict`'s doc comment.
    NotAvailable,
    /// The store was judged and nothing gate-worthy was found.
    Clean,
    /// The store was judged and at least one real finding was found.
    Failing,
}

/// Decide `--gate`'s verdict, reusing exactly the computations the plain-
/// language lines above already do - never a second evaluation of what they
/// evaluate. This function is the one place that decides which finding is
/// gate-worthy; `doctor` only turns its answer into an exit code.
///
/// FAILS the gate (`Failing`):
///
/// - The event chain is broken (`chain_intact`, the same check `store_line`
///   reports as "CHAIN BROKEN"): the store's own audit trail no longer
///   holds together. Not a matter of degree or opinion, and worse than any
///   single item being wrong, so this applies regardless of `project`.
/// - A dead anchor, or a proof that now comes out FALSE (`decay_check`): a
///   fact that fires nowhere, or a rule whose own machine-checkable claim is
///   presently untrue. Both are current and concrete, not historical debt -
///   scoped to `project`'s own items when one is given, so one repository's
///   gate can never be failed by another checkout's rot under the same
///   `--checkouts` directory.
///
/// Does NOT fail the gate (`Clean`, even where the line above may read as
/// less than perfect) - and why each was left out:
///
/// - Code index / replica lines report a COMPANION system's staleness or
///   reachability, never the memory's own correctness. A replica that is
///   merely unreachable (no network, a cloud sandbox) would fail every
///   single commit for a reason that has nothing to do with what changed -
///   exactly the kind of noise that trains people to skip a gate.
/// - Falsifier coverage and provable-rule coverage are whole-store
///   PERCENTAGES of historical debt, not a defect introduced by this run.
///   Forcing either up in bulk was already tried and explicitly rejected
///   (raise per fact where being wrong is costly, never bulk-link the whole
///   store) because it reintroduces the noise this whole feature exists to
///   keep out - so the gate must not recreate that same pressure by another
///   route. (Also: `declare` already requires a falsifier for anything that
///   can fire, so a missing one is legacy debt from before that rule
///   existed, never something a normal write can create today.)
/// - Unjudged items are explicitly a nudge, not a defect: silence decides
///   nothing about whether an item belongs where it fired.
/// - Semantic search mode is a build/deploy fact (is the feature compiled
///   in, is a model installed on THIS machine) - never a statement about
///   any store's content, so it can never be a store's finding.
/// - Orphan project keys are real, but only as trustworthy as the
///   `--checkouts` directory is complete. Miss one sibling checkout, rename
///   a project, or archive one on purpose, and this reports an orphan that
///   has nothing to do with the commit being gated - a false positive with
///   no way for `project` to rule it out the way it can for decay. Left out
///   until it can be scoped as safely as decay already is, rather than risk
///   exactly the false positive this function exists to avoid.
///
/// FAILS OPEN (`NotAvailable`) when the store is missing or cannot be read.
/// A cloud session has no thor.db at all, and must never be blocked by its
/// absence - nor may a store that is present but corrupt/mid-sync turn into
/// a build failure, since neither case says anything about whether THIS
/// change is wrong. A gate that cries wolf gets bypassed, and this codebase
/// has a measured instance of exactly that, which is the reason every
/// "does not fail" case above errs toward `Clean` as well.
pub fn gate_verdict(db: &Path, checkouts: Option<&Path>, project: Option<&str>) -> GateVerdict {
    let events = match open_and_read(db) {
        Ok(e) => e,
        Err(_) => return GateVerdict::NotAvailable,
    };
    let chain_broken = !chain_intact(&events);
    let decay_finding = matches!(
        decay_check(db, checkouts, project),
        DecayCheck::Counted { dead, failing, .. } if !dead.is_empty() || !failing.is_empty()
    );
    if chain_broken || decay_finding {
        GateVerdict::Failing
    } else {
        GateVerdict::Clean
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use model::item::{Binding, Check, Item, Kind, TargetKind};
    use model::store;
    use thor_core::event_store::EventKind;

    // ------------------------------------------------------------ identity

    #[test]
    fn version_line_names_this_binary_and_the_workspace_version() {
        let line = version_line();
        assert!(line.starts_with("doctor "), "{line}");
        assert!(line.ends_with(env!("CARGO_PKG_VERSION")), "{line}");
    }

    fn rule(id: &str) -> Item {
        Item {
            id: id.to_string(),
            kind: Kind::Rule,
            text: "a fixture rule".to_string(),
            bindings: vec![Binding::Always],
            severity: None,
            project: None,
            tags: vec![],
            expires: None,
            key: None,
            falsifier: Some("this fixture rule turns out to be wrong".to_string()),
            check: None,
        }
    }

    /// Write `item` straight to the log, bypassing `store::declare`'s own
    /// gate entirely - the same escape hatch `serve`'s own `teeth_debt`
    /// tests use (`legacy_unanswered`) for a shape the write gate no longer
    /// allows to be CREATED, but a real store can still HOLD from before
    /// that ground existed. `can_refuse` (and the report lines it feeds)
    /// have to read whatever a store actually contains, gate-legal or not -
    /// this is how a test proves that reading stays correct on a shape
    /// `model::gate::declare` would now refuse outright.
    fn legacy_declare(store: &mut EventStore, item: &Item) {
        let body = serde_json::to_string(item).unwrap();
        store
            .append_event("legacy", &item.id, "migration", thor_core::event_store::EventKind::FactCreated, &item.id, None, &body)
            .unwrap();
    }

    /// FIELD TEST 12, R5: a project whose working copy is a repository nested
    /// one level under the `--checkouts` root (`<root>/<group>/<repo>`) was
    /// never found by the scan of immediate subdirectories, so every anchor of
    /// it read as dead while `serve why` resolved the same file. The registered
    /// working copy, when it lies under the root, is the one doctor reads.
    #[test]
    fn a_project_registered_in_a_nested_repository_is_not_reported_as_decayed() {
        let dir = tempfile::tempdir().unwrap();
        let checkouts = dir.path().join("dev");
        let nested = checkouts.join("group").join("nested-proj");
        std::fs::create_dir_all(nested.join(".git")).unwrap();
        std::fs::create_dir_all(nested.join("src").join("libs")).unwrap();
        std::fs::write(nested.join(".thor-project"), "nested-proj\n").unwrap();
        std::fs::write(nested.join("src").join("libs").join("Kernel.cpp"), "int main() {}\n").unwrap();

        let db = dir.path().join("t.db");
        let mut s = EventStore::new(&db).unwrap();
        let mut item = rule("nested-rule");
        item.project = Some("nested-proj".to_string());
        item.bindings = vec![Binding::Target { kind: TargetKind::Path, value: "src/libs/Kernel.cpp".to_string() }];
        legacy_declare(&mut s, &item);
        let mut record = rule(&store::project_opened_id("nested-proj"));
        record.kind = Kind::Report;
        record.bindings = vec![];
        record.falsifier = None;
        record.project = Some("nested-proj".to_string());
        record.tags = vec!["project-record".to_string()];
        let body = store::canonical_body(&record).unwrap();
        let top = serve::project::checkout_top(&nested);
        let body = format!(
            "{},\"root\":{},\"root_at\":\"2026-10-07T10:00:00Z\"}}",
            &body[..body.len() - 1],
            serde_json::to_string(&top.to_string_lossy()).unwrap()
        );
        s.append_event("install", "install", store::INSTALLER_ACTOR, EventKind::FactCreated, &record.id, None, &body).unwrap();
        drop(s);

        let line = decay_line(&db, Some(&checkouts), false);
        assert!(line.starts_with("decay: none"), "an existing file in a nested registered repo is not decay: {line}");

        // The nested repository really is the one judged: take the file away.
        std::fs::remove_file(nested.join("src").join("libs").join("Kernel.cpp")).unwrap();
        let line = decay_line(&db, Some(&checkouts), false);
        assert!(line.contains("1 anchor(s) point at nothing") && line.contains("nested-rule"), "{line}");

        // A registration outside the root is not measured by this root.
        let elsewhere = tempfile::tempdir().unwrap();
        assert!(!is_under(elsewhere.path(), &checkouts));
        assert!(is_under(&checkouts, &nested));
        assert!(!is_under(&checkouts, &checkouts), "the root itself is not under itself");
    }

    /// One project, two folders that resolve to it: an outer folder with the
    /// marker (which the scan finds) and the repository nested in it (the
    /// registered working copy). Anchors written relative to either one fire
    /// from the folder a session stands in, so neither is decay. Measured on
    /// the owner's store (2026-10-07): preferring the registered copy alone
    /// turned two live anchors into false dead ones.
    #[test]
    fn an_anchor_alive_in_either_folder_of_one_project_is_not_decay() {
        let dir = tempfile::tempdir().unwrap();
        let checkouts = dir.path().join("dev");
        let outer = checkouts.join("group");
        let nested = outer.join("fw");
        std::fs::create_dir_all(nested.join(".git")).unwrap();
        std::fs::create_dir_all(nested.join("src")).unwrap();
        std::fs::write(outer.join(".thor-project"), "two-folders\n").unwrap();
        std::fs::write(nested.join(".thor-project"), "two-folders\n").unwrap();
        std::fs::write(nested.join("src").join("kernel.cpp"), "int main() {}\n").unwrap();

        let db = dir.path().join("t.db");
        let mut s = EventStore::new(&db).unwrap();
        // One anchor relative to the nested repository, one relative to the outer folder.
        for (id, anchor) in [("rel-nested", "src/kernel.cpp"), ("rel-outer", "fw/src/kernel.cpp")] {
            let mut item = rule(id);
            item.project = Some("two-folders".to_string());
            item.bindings = vec![Binding::Target { kind: TargetKind::Path, value: anchor.to_string() }];
            legacy_declare(&mut s, &item);
        }
        let mut record = rule(&store::project_opened_id("two-folders"));
        record.kind = Kind::Report;
        record.bindings = vec![];
        record.falsifier = None;
        record.project = Some("two-folders".to_string());
        record.tags = vec!["project-record".to_string()];
        let body = store::canonical_body(&record).unwrap();
        let top = serve::project::checkout_top(&nested);
        let body = format!(
            "{},\"root\":{},\"root_at\":\"2026-10-07T10:00:00Z\"}}",
            &body[..body.len() - 1],
            serde_json::to_string(&top.to_string_lossy()).unwrap()
        );
        s.append_event("install", "install", store::INSTALLER_ACTOR, EventKind::FactCreated, &record.id, None, &body).unwrap();
        drop(s);

        let line = decay_line(&db, Some(&checkouts), false);
        assert!(line.starts_with("decay: none"), "{line}");

        // An anchor alive in neither folder is still decay.
        let mut s = EventStore::open_existing(&db).unwrap();
        let mut gone = rule("gone");
        gone.project = Some("two-folders".to_string());
        gone.bindings = vec![Binding::Target { kind: TargetKind::Path, value: "src/missing.cpp".to_string() }];
        legacy_declare(&mut s, &gone);
        drop(s);
        let line = decay_line(&db, Some(&checkouts), false);
        assert!(line.contains("1 anchor(s) point at nothing") && line.contains("gone"), "{line}");
    }

    /// The batch the next report is held to, and what stays owed beyond it,
    /// are both named - the summary count and the named lists agree.
    #[test]
    fn the_evaluation_batch_line_names_the_batch_and_every_rule_left_owed() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let mut s = EventStore::new(&db).unwrap();
        let mut report = rule("eval-batch-0");
        report.kind = Kind::Report;
        report.bindings = vec![];
        report.falsifier = None;
        report.project = Some("proj".to_string());
        report.tags = vec![store::EVALUATION_REPORT_TAG.to_string()];
        // Filed through THOR's remember tool, the way Claude Code files one
        // (step 15), by the shared attested fixture.
        serve::attest::fixture::attested_filing(&mut s, &dir.path().join("transcripts"), "doctor-fixture", &report, "toolu_batch_0")
            .unwrap();
        let owed = store::EVALUATION_TEETH_BATCH + 3;
        for i in 0..owed {
            let mut heavy = rule(&format!("heavy-{i:02}"));
            heavy.severity = Some(model::item::Severity::Irreversible);
            heavy.project = Some("proj".to_string());
            legacy_declare(&mut s, &heavy);
            s.append_event("s", "l", "hook", EventKind::ItemServed, &heavy.id, None, "{}").unwrap();
        }
        drop(s);

        let line = evaluation_batch_line(&db, Some("proj"), true).expect("rules are owed");
        assert_eq!(number_before(&line, " rule(s) - its batch"), store::EVALUATION_TEETH_BATCH, "{line}");
        assert_eq!(line.matches("\n  batch: ").count(), store::EVALUATION_TEETH_BATCH, "{line}");
        assert_eq!(number_before(&line, " more rule(s)"), 3, "{line}");
        assert_eq!(line.matches("\n  owed later: ").count(), 3, "{line}");
        assert!(evaluation_batch_line(&db, None, true).is_none(), "no project, no report to file, no line");
    }

    /// Pulls the plain integer immediately before `marker` out of a report
    /// line - e.g. `number_before("teeth: 3 of 9 ... the other 6 can only
    /// inform", " can only inform")` returns `6`. Used only by the two
    /// summary-versus-named-list invariant tests below: reading the actual
    /// claimed number out of the real sentence, rather than re-deriving it
    /// from the fixture, is what makes those tests catch a REAL drift
    /// between the two - a test that instead hard-codes what the summary
    /// "should" say would not notice the summary computing a different
    /// number than the list it sits next to.
    fn number_before(text: &str, marker: &str) -> usize {
        let idx = text.find(marker).expect("marker must appear in the line");
        text[..idx]
            .trim_end()
            .rsplit(|c: char| !c.is_ascii_digit())
            .next()
            .filter(|s| !s.is_empty())
            .expect("a number must precede the marker")
            .parse()
            .expect("the token right before the marker must be a plain number")
    }

    // ---------------------------------------------------- checkouts root
    //
    // THE DEFECT THESE PREVENT, reported at the end of two separate
    // sessions: "decay and crowding not measured (requires --checkouts)" -
    // doctor left its two most important lines unmeasured and gave a new
    // reader no way to learn the flag even existed. `resolve_checkouts_root`
    // now tries to infer one from where doctor is actually standing before
    // ever falling back to silence.

    /// THE EXACT CASE NAMED IN THE TASK: a `.thor` marker names the repo,
    /// several directories below where doctor is actually invoked from - the
    /// PARENT of that repo (not the repo itself) is the inferred root, since
    /// `--checkouts` has always wanted a directory of SIBLING checkouts.
    #[test]
    fn a_thor_marker_several_directories_up_infers_its_parent_as_the_root() {
        let dir = tempfile::tempdir().unwrap();
        let checkouts = dir.path().join("dev");
        let repo = checkouts.join("My-Repo");
        let cwd = repo.join("src").join("deep");
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::write(repo.join(".thor"), "My-Repo\n").unwrap();

        assert_eq!(infer_checkouts_root(&cwd).as_deref(), Some(checkouts.as_path()));
    }

    /// The other signal `nearest_repo_root` accepts: an ordinary `.git`
    /// directory, with no THOR marker at all.
    #[test]
    fn a_git_root_with_no_thor_marker_also_infers_its_parent() {
        let dir = tempfile::tempdir().unwrap();
        let checkouts = dir.path().join("dev");
        let repo = checkouts.join("Some-Repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();

        assert_eq!(infer_checkouts_root(&repo).as_deref(), Some(checkouts.as_path()));
    }

    /// Neither signal, all the way to the filesystem root: nothing to infer.
    #[test]
    fn no_marker_and_no_git_root_infers_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let lonely = dir.path().join("just-a-folder");
        std::fs::create_dir_all(&lonely).unwrap();
        assert_eq!(infer_checkouts_root(&lonely), None);
    }

    /// THE PRECEDENCE RULE: an explicit `--checkouts` is never second-guessed
    /// by inference, even when inference would have found a DIFFERENT real
    /// repo right where doctor stands.
    #[test]
    fn an_explicit_checkouts_flag_wins_over_inference() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("dev").join("Some-Repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        let explicit = dir.path().join("an-explicit-directory");
        std::fs::create_dir_all(&explicit).unwrap();

        let resolved = resolve_checkouts_root(Some(&explicit), &repo);
        assert_eq!(resolved, CheckoutsRoot::Explicit(explicit.clone()));
        assert_eq!(resolved.as_path(), Some(explicit.as_path()));
    }

    #[test]
    fn no_flag_and_no_inferable_repo_resolves_to_unresolved() {
        let dir = tempfile::tempdir().unwrap();
        let lonely = dir.path().join("just-a-folder");
        std::fs::create_dir_all(&lonely).unwrap();
        assert_eq!(resolve_checkouts_root(None, &lonely), CheckoutsRoot::Unresolved);
    }

    /// THE SILENCE THIS REPLACES: with neither a flag nor an inferable repo,
    /// the report must say so in one plain line - not go quiet the way two
    /// sessions in a row found it.
    #[test]
    fn the_unresolved_line_says_decay_and_crowding_were_not_measured_and_how_to_fix_it() {
        let line = checkouts_root_line(&CheckoutsRoot::Unresolved);
        assert!(line.contains("NOT measured"), "{line}");
        assert!(line.contains("--checkouts"), "{line}");
    }

    #[test]
    fn the_inferred_line_names_the_root_and_that_the_flag_overrides_it() {
        let dir = tempfile::tempdir().unwrap();
        let line = checkouts_root_line(&CheckoutsRoot::Inferred(dir.path().to_path_buf()));
        assert!(line.contains(&dir.path().display().to_string()), "{line}");
        assert!(line.contains("--checkouts"), "the line must say the flag can override this: {line}");
    }

    #[test]
    fn the_explicit_line_names_the_root_given() {
        let dir = tempfile::tempdir().unwrap();
        let line = checkouts_root_line(&CheckoutsRoot::Explicit(dir.path().to_path_buf()));
        assert!(line.contains(&dir.path().display().to_string()), "{line}");
    }

    /// THE DEFECT THIS CLOSES, reported from a real session 2026-08-19:
    /// doctor named four items owed a judgement and all four were retracted,
    /// so the debt could be settled with the dead and the number meant
    /// nothing. The Stop-time ask already dropped them; this line did not.
    #[test]
    fn a_retracted_item_is_no_longer_counted_as_owing_a_judgement() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        {
            let mut store = EventStore::new(&db).unwrap();
            let mut item = rule("gone");
            item.bindings = vec![Binding::Target { kind: TargetKind::Path, value: "src/main.rs".to_string() }];
            // A source-file anchor needs a project - a global one would fire in
            // every repository that happens to hold a file by that name.
            item.project = Some("a-project".to_string());
            store::declare(&mut store, "s", "l", "t", &item).unwrap();
            for i in 0..50 {
                serve::deliver::record_delivery(&mut store, &serve::deliver::nth_session("s", i), "l", "t", "now", &["gone".to_string()]);
            }
            store::retract(&mut store, "s", "l", "t", "gone", "wrong from the start", None).unwrap();
        }
        let line = unjudged_line(&db);
        assert!(
            line.contains("every item that has ever fired has been judged")
                || !line.contains("1 trigger-bound"),
            "a retracted item owes nothing: {line}"
        );
    }

    // ----------------------------------------------- judgement_debt_line
    //
    // Built on `serve::usefulness::judgement_debt_counts`, the same fold
    // `bin/serve.rs`'s own Stop-time `judgement_debt` now shares - see
    // `judgement_debt_line`'s own doc comment for why `doctor` needs this at
    // all now that the Stop hook floors each item to at most one ask per
    // session (`oordeelschuld-komt-binnen-de-sessie-terug`, 2026-09-08).

    /// Silent at zero, the same convention `ship_line` already uses above: a
    /// clean backlog is not a finding - `--full` must not change that.
    #[test]
    fn judgement_debt_line_is_silent_when_nothing_is_owed() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        EventStore::new(&db).unwrap();
        assert_eq!(judgement_debt_line(&db, None, false), None);
        assert_eq!(judgement_debt_line(&db, Some("thor"), false), None);
        assert_eq!(judgement_debt_line(&db, Some("thor"), true), None);
    }

    /// Once something is owed, the line names both the store-wide count and
    /// this checkout's own share of it - built on the exact same fold the
    /// Stop hook acts on, so it can never disagree with what it reports on -
    /// and, since 2026-09-12, NAMES the item itself: id, how many times it
    /// fired, its kind, and WHERE it fires, so an evaluation has a place to
    /// judge "did it belong there" against rather than only a number.
    #[test]
    fn judgement_debt_line_names_both_numbers_once_something_is_owed() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        {
            let mut store = EventStore::new(&db).unwrap();
            let mut item = rule("owed-global");
            item.bindings = vec![Binding::Target { kind: TargetKind::Path, value: "src/lib.rs".to_string() }];
            // A source-file anchor needs a project, same reason
            // `a_retracted_item_is_no_longer_counted_as_owing_a_judgement`
            // above already gives: a global one would fire in every
            // repository that happens to have a file by that name.
            item.project = Some("thor".to_string());
            store::declare(&mut store, "s", "l", "a", &item).unwrap();
            for i in 0..serve::usefulness::JUDGEMENT_DEBT_AFTER {
                serve::deliver::record_delivery(&mut store, &serve::deliver::nth_session("s", i), "l", serve::usefulness::ASSISTANT_DELIVERY_ACTOR, "2026-09-08T00:00:00Z", &["owed-global".to_string()]);
            }
        }
        let line = judgement_debt_line(&db, Some("thor"), false).expect("something is owed, the line must speak");
        assert!(line.starts_with("judgement debt: "), "{line}");
        assert!(line.contains("1 item"), "{line}");
        assert!(line.contains("thor"), "must name this checkout's own project: {line}");
        assert!(
            line.contains(&format!("  judgement debt: owed-global ({}x, Rule) at src/lib.rs", serve::usefulness::JUDGEMENT_DEBT_AFTER)),
            "must name the item, its count, its kind and where it fires: {line}"
        );
    }

    /// Field test 11: the line promised that every owed item "is asked about
    /// inside the project's next evaluation". By design an evaluation asks
    /// only about items the EVALUATING session saw fire, so an owed item that
    /// never fires there is never asked. The sentence now says exactly that,
    /// and points at the two calls that settle such an item by hand.
    #[test]
    fn judgement_debt_line_promises_only_what_the_evaluation_does() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        {
            let mut store = EventStore::new(&db).unwrap();
            let mut item = rule("owed-promise");
            item.bindings = vec![Binding::Target { kind: TargetKind::Path, value: "src/lib.rs".to_string() }];
            item.project = Some("thor".to_string());
            store::declare(&mut store, "s", "l", "a", &item).unwrap();
            for i in 0..serve::usefulness::JUDGEMENT_DEBT_AFTER {
                serve::deliver::record_delivery(&mut store, &serve::deliver::nth_session("s", i), "l", serve::usefulness::ASSISTANT_DELIVERY_ACTOR, "2026-09-08T00:00:00Z", &["owed-promise".to_string()]);
            }
        }
        for project in [Some("thor"), None] {
            let line = judgement_debt_line(&db, project, false).expect("something is owed, the line must speak");
            let first = line.lines().next().unwrap();
            assert!(!first.contains("asked about inside the project's next evaluation"), "the old promise is gone: {first}");
            assert!(first.contains("only when the evaluating session saw it fire"), "{first}");
            assert!(first.contains("mark or retract"), "and says how to settle the rest: {first}");
            assert!(first.contains("sessions since its last verdict"), "and what the number after each name means: {first}");
        }
    }

    /// THE DEFECT THIS PREVENTS (measured 2026-09-17): acme-shop eval 1
    /// skipped two owed items because it could not see where they fired;
    /// eval 2 skipped three more from the same gap in another session of the
    /// same project. The named list now prints the newest serving's own
    /// trigger after the binding, so "did it belong where it fired" has an
    /// answer to judge.
    #[test]
    fn judgement_debt_line_names_the_last_trigger_when_one_was_recorded() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        {
            let mut store = EventStore::new(&db).unwrap();
            let mut item = rule("owed-with-trigger");
            item.bindings = vec![Binding::Target { kind: TargetKind::Command, value: "npm run deploy".to_string() }];
            item.project = Some("thor".to_string());
            store::declare(&mut store, "s", "l", "a", &item).unwrap();
            for i in 0..(serve::usefulness::JUDGEMENT_DEBT_AFTER - 1) {
                serve::deliver::record_delivery(&mut store, &serve::deliver::nth_session("s", i), "l", serve::usefulness::ASSISTANT_DELIVERY_ACTOR, "2026-09-08T00:00:00Z", &["owed-with-trigger".to_string()]);
            }
            serve::deliver::record_delivery_with_trigger(
                &mut store,
                "the-last-session",
                "l",
                "hook",
                "2026-09-17T00:00:00Z",
                &["owed-with-trigger".to_string()],
                Some("npm run deploy"),
            );
        }
        let line = judgement_debt_line(&db, Some("thor"), false).expect("something is owed, the line must speak");
        assert!(
            line.contains("  judgement debt: owed-with-trigger (") && line.contains(", last fired at 'npm run deploy'"),
            "must name where the newest serving actually fired: {line}"
        );
    }

    /// The contrast: no trigger ever recorded (every delivery in this
    /// fixture is a plain `record_delivery`) must print no suffix at all,
    /// never "last fired at 'None'" or an empty pair of quotes.
    #[test]
    fn judgement_debt_line_names_no_trigger_clause_when_none_was_ever_recorded() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        {
            let mut store = EventStore::new(&db).unwrap();
            let mut item = rule("owed-without-trigger");
            item.bindings = vec![Binding::Target { kind: TargetKind::Command, value: "npm run build".to_string() }];
            item.project = Some("thor".to_string());
            store::declare(&mut store, "s", "l", "a", &item).unwrap();
            for i in 0..serve::usefulness::JUDGEMENT_DEBT_AFTER {
                serve::deliver::record_delivery(&mut store, &serve::deliver::nth_session("s", i), "l", serve::usefulness::ASSISTANT_DELIVERY_ACTOR, "2026-09-08T00:00:00Z", &["owed-without-trigger".to_string()]);
            }
        }
        let line = judgement_debt_line(&db, Some("thor"), false).expect("something is owed, the line must speak");
        assert!(!line.contains("last fired at"), "no trigger was ever recorded, so nothing must be named: {line}");
    }

    /// A checkout that resolves to no project at all still gets an honest
    /// answer - the global share of the backlog, said plainly rather than
    /// silently dropped - and still gets the item itself named.
    #[test]
    fn judgement_debt_line_still_speaks_for_a_checkout_with_no_project() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        {
            let mut store = EventStore::new(&db).unwrap();
            // NOT `rule()`'s own default binding (`Always`, i.e. pinned): a
            // pinned item is excluded from the judgement debt entirely since
            // 2026-09-12, so this fixture needs a real trigger to still be
            // "owed" - a global Moment, since this test is specifically about
            // a checkout that resolves to no project at all.
            let mut item = rule("owed-global-2");
            item.bindings = vec![Binding::Moment(intent::Action::Commit)];
            store::declare(&mut store, "s", "l", "a", &item).unwrap();
            for i in 0..serve::usefulness::JUDGEMENT_DEBT_AFTER {
                serve::deliver::record_delivery(&mut store, &serve::deliver::nth_session("s", i), "l", serve::usefulness::ASSISTANT_DELIVERY_ACTOR, "2026-09-08T00:00:00Z", &["owed-global-2".to_string()]);
            }
        }
        let line = judgement_debt_line(&db, None, false).expect("a global item is still owed with no project resolved");
        assert!(line.contains("resolves to no project"), "{line}");
        assert!(line.contains("1 global"), "{line}");
        assert!(line.contains("  judgement debt: owed-global-2 ("), "must name the global item too: {line}");
    }

    /// THE TWENTY-ITEM CAP, the same `NAMES_AT_MOST`/`name_cap` mechanism
    /// `decay_line`'s "dead anchor"/"false proof" lists and `crowding_line`'s
    /// "outranked" list already use: past twenty, the rest are held back and
    /// counted in a tail line rather than printed - `--full` (tested next)
    /// is the only way to see them all.
    #[test]
    fn judgement_debt_line_caps_named_items_at_twenty_with_a_tail() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        {
            let mut store = EventStore::new(&db).unwrap();
            for n in 0..25 {
                let id = format!("owed-{n:02}");
                // Distinct text per item - `rule`'s own fixture text is
                // identical for every id, and 25 near-identical rules trip
                // the write gate's own near-duplicate check (Jaccard word
                // overlap >= 0.8, see `model::store::NEAR_DUPLICATE_JACCARD_
                // THRESHOLD`); this fixture's own words are common on
                // purpose except the one that carries `n`, which keeps every
                // pair's overlap well under that bar.
                let mut item = rule(&id);
                item.text = format!("fixture debt case {n:02}");
                // NOT `rule()`'s own default binding (`Always`, i.e.
                // pinned): a pinned item is excluded from the judgement
                // debt entirely since 2026-09-12, and `Always` also exists
                // in this fixture to dodge the crowding limit this loop
                // would otherwise hit (`model::item::MAX_ITEMS`) - a unique
                // Command anchor per item does both jobs at once, with no
                // rival ever sharing one, and (unlike a Path anchor) with no
                // project required to clear ground 19.
                item.bindings =
                    vec![Binding::Target { kind: TargetKind::Command, value: format!("fixture-debt-command-{n:02}") }];
                store::declare(&mut store, "s", "l", "a", &item).unwrap();
                for i in 0..serve::usefulness::JUDGEMENT_DEBT_AFTER {
                    serve::deliver::record_delivery(&mut store, &serve::deliver::nth_session("s", i), "l", serve::usefulness::ASSISTANT_DELIVERY_ACTOR, "2026-09-08T00:00:00Z", &[id.clone()]);
                }
            }
        }
        let line = judgement_debt_line(&db, Some("thor"), false).expect("25 items are owed, the line must speak");
        assert!(line.contains("25 item"), "{line}");
        let named_lines = line.lines().filter(|l| l.starts_with("  judgement debt: owed-")).count();
        assert_eq!(named_lines, NAMES_AT_MOST, "must name at most twenty: {line}");
        assert!(line.contains("  judgement debt: and 5 more, not named here"), "{line}");
    }

    /// `--full` LIFTS THE CAP: the same 25-item store as above, but every one
    /// of them named and no tail line at all.
    #[test]
    fn judgement_debt_line_names_all_items_with_full() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        {
            let mut store = EventStore::new(&db).unwrap();
            for n in 0..25 {
                let id = format!("owed-{n:02}");
                // Distinct text per item - `rule`'s own fixture text is
                // identical for every id, and 25 near-identical rules trip
                // the write gate's own near-duplicate check (Jaccard word
                // overlap >= 0.8, see `model::store::NEAR_DUPLICATE_JACCARD_
                // THRESHOLD`); this fixture's own words are common on
                // purpose except the one that carries `n`, which keeps every
                // pair's overlap well under that bar.
                let mut item = rule(&id);
                item.text = format!("fixture debt case {n:02}");
                // NOT `rule()`'s own default binding (`Always`, i.e.
                // pinned): a pinned item is excluded from the judgement
                // debt entirely since 2026-09-12, and `Always` also exists
                // in this fixture to dodge the crowding limit this loop
                // would otherwise hit (`model::item::MAX_ITEMS`) - a unique
                // Command anchor per item does both jobs at once, with no
                // rival ever sharing one, and (unlike a Path anchor) with no
                // project required to clear ground 19.
                item.bindings =
                    vec![Binding::Target { kind: TargetKind::Command, value: format!("fixture-debt-command-{n:02}") }];
                store::declare(&mut store, "s", "l", "a", &item).unwrap();
                for i in 0..serve::usefulness::JUDGEMENT_DEBT_AFTER {
                    serve::deliver::record_delivery(&mut store, &serve::deliver::nth_session("s", i), "l", serve::usefulness::ASSISTANT_DELIVERY_ACTOR, "2026-09-08T00:00:00Z", &[id.clone()]);
                }
            }
        }
        let line = judgement_debt_line(&db, Some("thor"), true).expect("25 items are owed, the line must speak");
        let named_lines = line.lines().filter(|l| l.starts_with("  judgement debt: owed-")).count();
        assert_eq!(named_lines, 25, "--full must name every one of them: {line}");
        assert!(!line.contains("not named here"), "--full must leave no tail: {line}");
    }

    /// Declares `n` never-judged, trigger-bound items owed to `project` -
    /// shared by every evaluation-debt tail test below. NOT `rule()`'s own
    /// default `Always` binding - pinned since 2026-09-12 means excluded
    /// from the judgement debt entirely, which is exactly the count these
    /// fixtures need to actually reach.
    fn declare_owed_for_eval_debt(store: &mut EventStore, n: usize, project: &str) {
        for i in 0..n {
            let id = format!("owed-{project}-{i:02}");
            let mut item = rule(&id);
            item.text = format!("fixture eval debt case {project} {i:02}");
            item.project = Some(project.to_string());
            item.bindings =
                vec![Binding::Target { kind: TargetKind::Command, value: format!("fixture-eval-command-{project}-{i:02}") }];
            store::declare(store, "s", "l", "a", &item).unwrap();
            for i in 0..serve::usefulness::JUDGEMENT_DEBT_AFTER {
                serve::deliver::record_delivery(store, &serve::deliver::nth_session("s", i), "l", serve::usefulness::ASSISTANT_DELIVERY_ACTOR, "2026-09-08T00:00:00Z", &[id.clone()]);
            }
        }
    }

    /// A live evaluation-report Report, correctly tagged and scoped - the
    /// exact shape `eval-command.example.md`'s own final step files with
    /// `remember`.
    fn report_item(id: &str, project: &str) -> Item {
        Item {
            id: id.to_string(),
            kind: Kind::Report,
            text: "fixture evaluation report".to_string(),
            bindings: vec![],
            severity: None,
            project: Some(project.to_string()),
            tags: vec!["evaluation-report".to_string()],
            expires: None,
            key: None,
            falsifier: None,
            check: None,
        }
    }

    /// `item` filed the way Claude Code files one (step 15: through THOR's
    /// remember tool, for a quiet evaluation of its own session) and
    /// validated the way the next Stop validates it (step 16), by the shared
    /// fixture - made at unix time `at` as far as the transcript tells, the
    /// time its coverage is measured from. The tests calling this pair it
    /// with `declare_owed_for_eval_debt` on the SAME project, on purpose, to
    /// give `judgement_debt_line` something to report; the filing's session
    /// saw none of those firings, so its frozen judgement list is empty.
    fn declare_report_at(store: &mut EventStore, item: &Item, at: i64) {
        let transcripts = tempfile::tempdir().unwrap();
        let session = DOCTOR_EVALUATOR;
        serve::eval_filing::fixture::valid_filing_at(store, transcripts.path(), session, item, &format!("toolu_{}", item.id), at);
    }

    /// The evaluating session of these fixtures' reports (UUID-shaped: only
    /// such a session has a transcript THOR reads).
    const DOCTOR_EVALUATOR: &str = "d0c70000-0000-4000-8000-00000000d0c7";

    /// Write the evaluation-debt sidecar directly, the same JSON shape
    /// `serve::usefulness::update_eval_debt_state` itself writes - `doctor`
    /// must read it read-only, so these tests seed it by hand rather than
    /// through the Stop hook's own write path.
    fn seed_eval_debt_state(db: &Path, project: &str, state: serve::usefulness::ProjectEvalState) {
        let mut all: serve::usefulness::EvalDebtState = Default::default();
        all.insert(project.to_string(), state);
        std::fs::write(serve::usefulness::eval_debt_state_path(db), serde_json::to_string(&all).unwrap()).unwrap();
    }

    /// An instant `serve::usefulness::eval_report_covers` will always agree
    /// still covers, relative to the real "now" - regardless of what
    /// wall-clock hour the test suite happens to run at. Replaces the
    /// retired `start_of_today_utc` (seventh rewrite, 2026-09-17: the UTC
    /// calendar day it measured against is gone). Mirrors `bin/serve.rs`'s
    /// own test-only `recently_covered` helper.
    fn recently_covered() -> i64 {
        serve::time::now_unix() - 3600
    }

    /// THE EVALUATION DEBT'S OWN TAIL, since 2026-09-12 (trigger rewritten
    /// several times, most recently (2026-09-17, seventh rewrite) from a UTC
    /// calendar day to a rolling `serve::usefulness::EVAL_REPORT_COVERS_
    /// HOURS`-hour window, with no `tracking_since` grace period at all -
    /// see `serve::usefulness`'s own "evaluation debt" section for why: with
    /// no sidecar at all yet, a real backlog still gets its tail, and -
    /// UNLIKE THE RETIRED `tracking_since` GRACE PERIOD - the "not covered"
    /// note appears immediately: no report has ever been seen, so nothing
    /// can possibly cover, from the very first Stop this project is ever
    /// seen at.
    #[test]
    fn judgement_debt_line_shows_no_evaluation_in_the_window_with_no_sidecar_at_all() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        {
            let mut store = EventStore::new(&db).unwrap();
            declare_owed_for_eval_debt(&mut store, 12, "thor");
        }
        let line = judgement_debt_line(&db, Some("thor"), false).expect("a real backlog, the line must speak");
        assert!(line.contains("no evaluation report of this project counts (none on file passed THOR's check)"), "{line}");
        assert!(line.contains("no evaluation in the last 7 day(s)"), "{line}");
        assert!(line.contains("not asked yet"), "{line}");
        assert!(
            line.contains("the Stop hook blocks every turn once a session has accrued an hour of work here"),
            "{line}"
        );
    }

    /// Once the sidecar carries a non-zero ask count and a first-asked
    /// instant, the tail names both, alongside the "no evaluation in the
    /// window" note.
    #[test]
    fn judgement_debt_line_names_the_ask_count_and_first_asked_day_when_nothing_covers() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        {
            let mut store = EventStore::new(&db).unwrap();
            declare_owed_for_eval_debt(&mut store, 12, "thor");
        }
        let two_days_ago = serve::time::now_unix() - 2 * 86400;
        seed_eval_debt_state(
            &db,
            "thor",
            serve::usefulness::ProjectEvalState { asked_count: 4, first_asked_since_report: Some(two_days_ago), ..Default::default() },
        );
        let line = judgement_debt_line(&db, Some("thor"), false).expect("a real backlog, the line must speak");
        assert!(line.contains("no evaluation report of this project counts (none on file passed THOR's check)"), "{line}");
        assert!(line.contains("no evaluation in the last 7 day(s)"), "{line}");
        assert!(line.contains("asked 4 time(s), the first 2 day(s) ago"), "{line}");
        assert!(
            line.contains("the Stop hook blocks every turn once a session has accrued an hour of work here"),
            "{line}"
        );
    }

    /// THE FIX ITSELF (`no evaluation is asked outside a project, where no
    /// report can be filed`), proven for `doctor`: a checkout with no
    /// project still names its own judgement debt (a global item, exactly
    /// like `judgement_debt_line_still_speaks_for_a_checkout_with_no_
    /// project` above), but the evaluation tail names nothing at all - not
    /// a report clause, not a coverage status, not an ask count
    /// - and says plainly that the evaluation is only ever asked inside a
    /// project. Proven even with a sidecar entry already carrying an ask
    /// count and a first-asked clock - the exact shape a sidecar written
    /// before this fix could still carry (see `usefulness::update_eval_
    /// debt_state`'s own doc comment) - to show that legacy entry is read
    /// for nothing here, not merely unmentioned by coincidence.
    #[test]
    fn judgement_debt_line_names_no_clock_at_all_for_a_checkout_with_no_project() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        {
            let mut store = EventStore::new(&db).unwrap();
            let mut item = rule("owed-global-no-project-tail");
            item.bindings = vec![Binding::Moment(intent::Action::Commit)];
            store::declare(&mut store, "s", "l", "a", &item).unwrap();
            for i in 0..serve::usefulness::JUDGEMENT_DEBT_AFTER {
                serve::deliver::record_delivery(&mut store, &serve::deliver::nth_session("s", i), "l", serve::usefulness::ASSISTANT_DELIVERY_ACTOR, "2026-09-08T00:00:00Z", &["owed-global-no-project-tail".to_string()]);
            }
        }
        let yesterday = serve::time::now_unix() - 25 * 3600;
        seed_eval_debt_state(
            &db,
            "",
            serve::usefulness::ProjectEvalState { asked_count: 9, first_asked_since_report: Some(yesterday), ..Default::default() },
        );

        let line = judgement_debt_line(&db, None, false).expect("a global item is owed, the line must speak");
        assert!(line.contains("resolves to no project"), "{line}");
        assert!(
            line.contains("the evaluation is only ever asked inside a project"),
            "must say plainly why there is nothing else here: {line}"
        );
        assert!(!line.contains("no evaluation report of this project"), "must never name a report clause for no project: {line}");
        assert!(
            !line.contains("still covers this project") && !line.contains("no evaluation in the last"),
            "must never claim any evaluation status for no project: {line}"
        );
        assert!(!line.contains("asked 9 time(s)"), "must never name an ask count for no project: {line}");
        assert!(
            !line.contains("the Stop hook blocks every turn"),
            "must never claim the Stop hook will block, since it never does for no project: {line}"
        );
    }

    /// A recently seen report still names itself and its age, and says
    /// plainly that the last evaluation still covers this project - and the
    /// "no evaluation in the window"/ask clauses and the Stop-hook note are
    /// all absent. The report must be a REAL live Report in the store, not
    /// merely a sidecar entry - see `declare_report`'s own doc comment.
    #[test]
    fn judgement_debt_line_names_a_recently_seen_report_and_says_it_still_covers() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let today = recently_covered();
        {
            let mut store = EventStore::new(&db).unwrap();
            declare_owed_for_eval_debt(&mut store, 12, "thor");
            declare_report_at(&mut store, &report_item("eval-thor-2026-09-14", "thor"), today);
        }
        let line = judgement_debt_line(&db, Some("thor"), false).expect("a real backlog, the line must speak");
        assert!(
            line.contains(&format!(
                "newest evaluation report 'eval-thor-2026-09-14' (valid, filed by session {DOCTOR_EVALUATOR}), filed 0 day(s) ago"
            )),
            "{line}"
        );
        assert!(line.contains("the last evaluation still covers this project"), "{line}");
        assert!(!line.contains("no evaluation in the last"), "{line}");
        assert!(!line.contains("the Stop hook blocks every turn"), "a report that still covers must leave out the blocking note: {line}");
    }

    /// UNLIKE THE OLD CEILING GATE: the evaluation tail no longer depends on
    /// how many items are owed at all - even a single owed item (nowhere
    /// near what used to be the ceiling of ten) still shows the "no
    /// evaluation in the window" note.
    #[test]
    fn the_evaluation_tail_speaks_regardless_of_how_few_items_are_owed() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        {
            let mut store = EventStore::new(&db).unwrap();
            // Exactly one owed item: real judgement debt (the line must
            // still speak), but nowhere near what used to be the ceiling.
            declare_owed_for_eval_debt(&mut store, 1, "thor");
        }
        let line = judgement_debt_line(&db, Some("thor"), false).expect("one item is still owed, the line must speak");
        assert!(line.contains("no evaluation in the last 7 day(s)"), "{line}");
        assert!(
            line.contains("the Stop hook blocks every turn once a session has accrued an hour of work here"),
            "{line}"
        );
    }

    // -------------------------------------- the most recently active session
    //
    // Added 2026-09-17 (fifth rewrite, `serve::usefulness`'s own "evaluation
    // debt" section): the sidecar now carries every session's own accrued
    // work (`ProjectEvalState::sessions`), so `doctor` names the most
    // recently active one and how much more accrued work it needs before
    // the next evaluation is due - the closest a cold read gets to the
    // per-session detail only a real Stop can know for certain.

    /// Step 17 and part G: doctor names the cadence on every run, with the plan
    /// it follows - a small plan when nothing is on record, Max when the sidecar
    /// says so.
    #[test]
    fn cadence_clause_names_the_mode_and_the_plan() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        drop(EventStore::new(&db).unwrap());
        assert_eq!(
            cadence_line(&db),
            "evaluation cadence: once a week per project, never daily (smaller plan) - no plan on record, so the cheap weekly rhythm"
        );
        serve::plan::write(&db, serve::plan::Plan::Max, serve::plan::PlanSource::Confirmed).unwrap();
        assert_eq!(
            cadence_line(&db),
            "evaluation cadence: once a week per project, daily while rot is found (Max plan) - plan max (confirmed)"
        );
        let lines = report(&db, None, None, None, None, None, false, Some("thor"));
        assert!(lines.iter().any(|l| l == &cadence_line(&db)), "{lines:?}");
    }

    /// Step 17 (N3): a work-clock sidecar that disagrees with the project's
    /// transcripts by more than 15 minutes is named with both numbers; one
    /// that agrees is not.
    #[test]
    fn work_clock_line_names_a_sidecar_the_transcripts_contradict() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        drop(EventStore::new(&db).unwrap());
        let root = dir.path().join("projects");
        let checkout = dir.path().join("checkouts").join("thor");
        std::fs::create_dir_all(&checkout).unwrap();
        std::fs::write(checkout.join(".thor-project"), "thor\n").unwrap();
        let session = "5e1f0c3a-0000-4000-8000-00000000d0c7";
        let now = serve::time::now_unix();
        serve::attest::fixture::work_in(&root, session, &checkout, now - 50 * 60, 45);
        let state = |minutes: i64| serve::usefulness::ProjectEvalState {
            sessions: std::collections::BTreeMap::from([(
                session.to_string(),
                serve::usefulness::SessionWorkState { anchor_unix: Some(now - 3600), accrued_secs: minutes * 60, last_event_unix: Some(now), ..Default::default() },
            )]),
            work: serve::usefulness::SessionWorkState { anchor_unix: Some(now - 3600), accrued_secs: minutes * 60, last_event_unix: Some(now), ..Default::default() },
            ..Default::default()
        };
        seed_eval_debt_state(&db, "thor", state(0));
        let line = work_clock_line_in(&db, Some("thor"), Some(&root)).expect("zeroed: the line must speak");
        assert!(line.contains("the work clock sidecar says 0 min, the transcripts say 45 min"), "{line}");
        seed_eval_debt_state(&db, "thor", state(40));
        assert_eq!(work_clock_line_in(&db, Some("thor"), Some(&root)), None, "within 15 minutes: silent");
        assert_eq!(work_clock_line_in(&db, None, Some(&root)), None, "no project: silent");
    }

    /// Case named in the build brief (doctor tail): the most recently active
    /// session's own accrued work is named, and how much more is needed
    /// before the FIRST threshold (no report yet today).
    #[test]
    fn judgement_debt_line_names_the_most_recently_active_sessions_accrued_work() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        {
            let mut store = EventStore::new(&db).unwrap();
            declare_owed_for_eval_debt(&mut store, 12, "thor");
        }
        let now = serve::time::now_unix();
        seed_eval_debt_state(
            &db,
            "thor",
            serve::usefulness::ProjectEvalState {
                sessions: std::collections::BTreeMap::from([(
                    "s1".to_string(),
                    serve::usefulness::SessionWorkState { anchor_unix: Some(now - 40 * 60), accrued_secs: 40 * 60, last_event_unix: Some(now), ..Default::default() },
                )]),
                ..Default::default()
            },
        );
        let line = judgement_debt_line(&db, Some("thor"), false).expect("a real backlog, the line must speak");
        assert!(
            line.contains("its most recently active session ('s1') has accrued 40 minute(s) of work here since its last reset"),
            "{line}"
        );
        assert!(line.contains("due after 20 more minute(s) of accrued work"), "{line}");
    }

    /// Case named in the build brief (doctor tail): the untested-edit count
    /// and whether a summary happened are named for the most recently active
    /// session, and the line says plainly that a repeat only fires with a
    /// risk.
    #[test]
    fn judgement_debt_line_names_the_untested_edit_count_and_whether_a_summary_happened() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        {
            let mut store = EventStore::new(&db).unwrap();
            declare_owed_for_eval_debt(&mut store, 12, "thor");
        }
        let now = serve::time::now_unix();
        seed_eval_debt_state(
            &db,
            "thor",
            serve::usefulness::ProjectEvalState {
                sessions: std::collections::BTreeMap::from([(
                    "s1".to_string(),
                    serve::usefulness::SessionWorkState {
                        anchor_unix: Some(now - 40 * 60),
                        accrued_secs: 40 * 60,
                        last_event_unix: Some(now),
                        edits_since_test: 2,
                        compacted_since_anchor: true,
                        stops: 0,
                    },
                )]),
                ..Default::default()
            },
        );
        let line = judgement_debt_line(&db, Some("thor"), false).expect("a real backlog, the line must speak");
        assert!(line.contains("2 untested edit(s) since the last test or build run"), "{line}");
        assert!(line.contains("a context summary has happened"), "{line}");
        assert!(!line.contains("repeat"), "there is no repeat any more: {line}");
    }

    /// The contrast: zero edits and no summary reads as "no context summary
    /// yet" rather than silently omitting the clause.
    #[test]
    fn judgement_debt_line_says_no_context_summary_yet_when_none_has_happened() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        {
            let mut store = EventStore::new(&db).unwrap();
            declare_owed_for_eval_debt(&mut store, 12, "thor");
        }
        let now = serve::time::now_unix();
        seed_eval_debt_state(
            &db,
            "thor",
            serve::usefulness::ProjectEvalState {
                sessions: std::collections::BTreeMap::from([(
                    "s1".to_string(),
                    serve::usefulness::SessionWorkState { anchor_unix: Some(now - 40 * 60), accrued_secs: 40 * 60, last_event_unix: Some(now), ..Default::default() },
                )]),
                ..Default::default()
            },
        );
        let line = judgement_debt_line(&db, Some("thor"), false).expect("a real backlog, the line must speak");
        assert!(line.contains("0 untested edit(s) since the last test or build run"), "{line}");
        assert!(line.contains("no context summary yet"), "{line}");
    }

    #[test]
    fn judgement_debt_line_says_a_session_already_over_the_threshold_is_already_due() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        {
            let mut store = EventStore::new(&db).unwrap();
            declare_owed_for_eval_debt(&mut store, 12, "thor");
        }
        let now = serve::time::now_unix();
        seed_eval_debt_state(
            &db,
            "thor",
            serve::usefulness::ProjectEvalState {
                sessions: std::collections::BTreeMap::from([(
                    "s1".to_string(),
                    serve::usefulness::SessionWorkState { anchor_unix: Some(now - 90 * 60), accrued_secs: 90 * 60, last_event_unix: Some(now), ..Default::default() },
                )]),
                ..Default::default()
            },
        );
        let line = judgement_debt_line(&db, Some("thor"), false).expect("a real backlog, the line must speak");
        assert!(line.contains("already due"), "{line}");
    }

    /// A report that still covers says so, for how many days, and that
    /// nothing more is asked before then (part G: one evaluation per week;
    /// there is no repeat threshold any more).
    #[test]
    fn judgement_debt_line_says_a_covering_report_holds_for_the_week() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let today = recently_covered();
        {
            let mut store = EventStore::new(&db).unwrap();
            declare_owed_for_eval_debt(&mut store, 12, "thor");
            declare_report_at(&mut store, &report_item("eval-thor-2026-09-17", "thor"), today);
        }
        let now = serve::time::now_unix();
        seed_eval_debt_state(
            &db,
            "thor",
            serve::usefulness::ProjectEvalState {
                sessions: std::collections::BTreeMap::from([(
                    "s1".to_string(),
                    serve::usefulness::SessionWorkState { anchor_unix: Some(today), accrued_secs: 100 * 60, last_event_unix: Some(now), ..Default::default() },
                )]),
                ..Default::default()
            },
        );
        let line = judgement_debt_line(&db, Some("thor"), false).expect("a real backlog, the line must speak");
        assert!(
            line.contains("the last evaluation still covers this project for 7 day(s) - nothing more is asked before then"),
            "{line}"
        );
        assert!(!line.contains("repeat"), "there is no repeat any more: {line}");
        assert!(line.contains("already due once the week is up"), "the per-session work is measured against the one-hour floor: {line}");
    }

    /// A report on file that no Stop has checked yet is NAMED, honestly - it
    /// counts for nothing until a main session's Stop validates its filing.
    #[test]
    fn judgement_debt_line_names_a_report_no_stop_has_checked_yet() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        {
            let mut store = EventStore::new(&db).unwrap();
            declare_owed_for_eval_debt(&mut store, 12, "thor");
            let transcripts = tempfile::tempdir().unwrap();
            let item = report_item("eval-thor-2026-09-16", "thor");
            serve::attest::fixture::attested_filing(&mut store, transcripts.path(), DOCTOR_EVALUATOR, &item, "toolu_unchecked").unwrap();
        }
        let line = judgement_debt_line(&db, Some("thor"), false).expect("a real backlog, the line must speak");
        assert!(line.contains("no evaluation report of this project counts"), "{line}");
        assert!(
            line.contains("evaluation report 'eval-thor-2026-09-16' is not checked yet - the next Stop of a main session checks its filing"),
            "{line}"
        );
    }

    /// Step 16: every report on file that does NOT count is named with the
    /// validator's reason - here one written past the tool server's gate,
    /// the way an older binary leaves one.
    #[test]
    fn judgement_debt_line_names_every_report_that_does_not_count_with_its_reason() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        {
            let mut store = EventStore::new(&db).unwrap();
            declare_owed_for_eval_debt(&mut store, 12, "thor");
            let item = report_item("eval-thor-2026-09-10", "thor");
            let body = store::canonical_body(&item).unwrap();
            store.append_event("mcp", "mcp", "mcp", thor_core::event_store::EventKind::FactCreated, &item.id, None, &body).unwrap();
            serve::eval_filing::validate_new_reports(&mut store, Some(dir.path()), DOCTOR_EVALUATOR);
        }
        let line = judgement_debt_line(&db, Some("thor"), false).expect("a real backlog, the line must speak");
        assert!(
            line.contains(
                "evaluation report 'eval-thor-2026-09-10' does not count as this project's evaluation: not filed through an \
                 attested THOR tool call"
            ),
            "{line}"
        );
    }

    /// THE NAMED LIST NEVER CONTAINS A PINNED ITEM, with or without
    /// `--full` - the doctor-facing half of the exclusion
    /// `usefulness::owed_items` now applies before either the count or the
    /// named list ever sees a pinned item, closing the defect measured
    /// 2026-09-12: the named list once carried more than thirty Always-bound
    /// items, two of them pinned by the owner on purpose.
    #[test]
    fn judgement_debt_line_and_its_named_list_exclude_a_pinned_item() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        {
            let mut store = EventStore::new(&db).unwrap();
            // `rule()`'s own default binding is `Always` - exactly the
            // pinned shape this test needs, left untouched on purpose.
            let mut pinned = rule("pinned-excluded");
            pinned.text = "fixture rule that is pinned and heavily served".to_string();
            store::declare(&mut store, "s", "l", "a", &pinned).unwrap();
            let mut trigger = rule("trigger-included");
            trigger.text = "fixture rule that is trigger-bound and heavily served".to_string();
            trigger.project = Some("thor".to_string());
            trigger.bindings =
                vec![Binding::Target { kind: TargetKind::Command, value: "fixture-excl-command".to_string() }];
            store::declare(&mut store, "s", "l", "a", &trigger).unwrap();
            for id in ["pinned-excluded", "trigger-included"] {
                for i in 0..serve::usefulness::JUDGEMENT_DEBT_AFTER {
                    serve::deliver::record_delivery(&mut store, &serve::deliver::nth_session("s", i), "l", serve::usefulness::ASSISTANT_DELIVERY_ACTOR, "2026-09-08T00:00:00Z", &[id.to_string()]);
                }
            }
        }
        for full in [false, true] {
            let line = judgement_debt_line(&db, Some("thor"), full).expect("the trigger-bound item is still owed");
            assert!(line.contains("1 item"), "full={full}: {line}");
            assert!(line.contains("pinned items excluded"), "full={full}: {line}");
            assert!(line.contains("trigger-included"), "full={full}: {line}");
            assert!(!line.contains("pinned-excluded"), "full={full}: a pinned item must never be named: {line}");
        }
    }

    /// THE FOUR SURFACES AGREEING, end to end: `unjudged_line` and
    /// `judgement_debt_line`, run against the SAME store, must name and
    /// count the identical outstanding item - never the pinned one sitting
    /// right next to it - closing the disagreement `judgement_debt_line`'s
    /// own doc comment records for the 2026-09-08 to 2026-09-12 window where
    /// the two lines quietly meant different things by "owed".
    #[test]
    fn the_unjudged_line_and_judgement_debt_line_agree_on_the_same_store() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        {
            let mut store = EventStore::new(&db).unwrap();
            let mut pinned = rule("pinned-heavy");
            pinned.text = "fixture rule that is pinned and heavily served".to_string();
            store::declare(&mut store, "s", "l", "a", &pinned).unwrap();
            let mut trigger = rule("trigger-heavy");
            trigger.text = "fixture rule that is trigger-bound and heavily served".to_string();
            trigger.project = Some("thor".to_string());
            trigger.bindings =
                vec![Binding::Target { kind: TargetKind::Command, value: "fixture-agree-command".to_string() }];
            store::declare(&mut store, "s", "l", "a", &trigger).unwrap();
            for id in ["pinned-heavy", "trigger-heavy"] {
                for i in 0..serve::usefulness::JUDGEMENT_DEBT_AFTER {
                    serve::deliver::record_delivery(&mut store, &serve::deliver::nth_session("s", i), "l", serve::usefulness::ASSISTANT_DELIVERY_ACTOR, "2026-09-08T00:00:00Z", &[id.to_string()]);
                }
            }
        }
        let unjudged = unjudged_line(&db);
        assert!(unjudged.contains("1 trigger-bound item"), "{unjudged}");
        assert!(unjudged.contains("pinned items excluded"), "{unjudged}");

        let debt = judgement_debt_line(&db, Some("thor"), false).expect("the trigger-bound item is still owed");
        assert!(debt.contains("1 item"), "{debt}");
        assert!(debt.contains("trigger-heavy"), "{debt}");
        assert!(!debt.contains("pinned-heavy"), "both lines must agree: neither ever names the pinned item: {debt}");
    }

    /// THE DEFECT THIS PREVENTS (2026-09-19): a program that is not an
    /// assistant can drive this same boundary and rack up firings past the
    /// judgement-debt threshold on its own - `judgement_debt_counts` (and so
    /// this line's own "N item(s) owed" count) must never name that item as
    /// debt, since no assistant session was ever there to judge it. But the
    /// traffic must not silently vanish either: the line must still say who
    /// fired and how many times.
    #[test]
    fn judgement_debt_line_excludes_another_actors_item_but_names_the_actor_and_its_count() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        {
            let mut store = EventStore::new(&db).unwrap();
            // A real, assistant-owed item, so the line already has
            // something to speak about regardless of the question under
            // test here (see `judgement_debt_line_speaks_for_another_
            // actors_traffic_even_with_no_real_debt` below for the zero-debt
            // case on its own).
            let mut owed = rule("owed-by-assistant");
            owed.text = "fixture rule that is owed by a real assistant firing".to_string();
            owed.bindings = vec![Binding::Target { kind: TargetKind::Command, value: "fixture-assistant-command".to_string() }];
            store::declare(&mut store, "s", "l", "a", &owed).unwrap();
            for i in 0..serve::usefulness::JUDGEMENT_DEBT_AFTER {
                serve::deliver::record_delivery(
                    &mut store, &serve::deliver::nth_session("s", i), "l", serve::usefulness::ASSISTANT_DELIVERY_ACTOR,
                    "2026-09-19T00:00:00Z", &["owed-by-assistant".to_string()],
                );
            }
            // An item pushed over the threshold by another actor ALONE.
            let mut probed = rule("probed-only");
            probed.text = "fixture rule that is only ever served by another actor".to_string();
            probed.bindings = vec![Binding::Target { kind: TargetKind::Command, value: "fixture-probe-command".to_string() }];
            store::declare(&mut store, "s", "l", "a", &probed).unwrap();
            for i in 0..serve::usefulness::JUDGEMENT_DEBT_AFTER {
                serve::deliver::record_delivery(&mut store, &serve::deliver::nth_session("s", i), "l", "probe", "2026-09-19T00:00:00Z", &["probed-only".to_string()]);
            }
        }
        let line = judgement_debt_line(&db, None, false).expect("something is owed, the line must speak");
        assert!(line.contains("owed-by-assistant"), "the real debt must still be named: {line}");
        assert!(
            !line.contains("probed-only"),
            "an item pushed over the threshold by another actor alone must never be named as debt: {line}"
        );
        assert!(line.contains("'probe' (40x)"), "must name the other actor and its firing count: {line}");
        assert!(
            line.contains("not counted as") && line.contains("debt"),
            "must say plainly that this is not counted as debt: {line}"
        );
    }

    /// The zero-debt half of the same defect: a store where NOTHING is
    /// really owed by an assistant must still speak when another actor has
    /// been busy here - staying silent would be exactly the vanishing act
    /// this whole mechanism exists to prevent (see `serve::usefulness::
    /// ASSISTANT_DELIVERY_ACTOR`'s own doc comment, "must be VISIBLE").
    #[test]
    fn judgement_debt_line_speaks_for_another_actors_traffic_even_with_no_real_debt() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        {
            let mut store = EventStore::new(&db).unwrap();
            let mut probed = rule("probed-only");
            probed.text = "fixture rule that is only ever served by another actor".to_string();
            probed.bindings = vec![Binding::Target { kind: TargetKind::Command, value: "fixture-probe-command".to_string() }];
            store::declare(&mut store, "s", "l", "a", &probed).unwrap();
            for i in 0..serve::usefulness::JUDGEMENT_DEBT_AFTER {
                serve::deliver::record_delivery(&mut store, &serve::deliver::nth_session("s", i), "l", "probe", "2026-09-19T00:00:00Z", &["probed-only".to_string()]);
            }
        }
        let line = judgement_debt_line(&db, None, false)
            .expect("another actor's own traffic must still speak, even with zero real debt");
        assert!(!line.contains("probed-only"), "still never named as debt: {line}");
        assert!(line.contains("'probe' (40x)"), "{line}");
    }

    // ----------------------------------------------------- contradiction_line

    #[test]
    fn contradiction_line_is_silent_when_nothing_disagrees() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let checkouts = dir.path().join("dev");
        let repo = checkouts.join("Real-Repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::write(repo.join("LICENSE"), "Licensed under GPLv3").unwrap();
        {
            let mut s = EventStore::new(&db).unwrap();
            let mut a = rule("license-says-gpl");
            a.text = "the license file must say GPLv3".to_string();
            a.project = Some("Real-Repo".to_string());
            a.check = Some(Check::Contains { path: "LICENSE".to_string(), literal: "GPLv3".to_string() });
            store::declare(&mut s, "s", "l", "a", &a).unwrap();

            let mut b = rule("license-must-not-say-mit");
            b.text = "the license file must never mention the MIT license".to_string();
            b.project = Some("Real-Repo".to_string());
            b.check = Some(Check::Absent { path: "LICENSE".to_string(), literal: "MIT".to_string() });
            store::declare(&mut s, "s", "l", "a", &b).unwrap();
        }
        // Both checks currently Holds against the real file - nothing to
        // report, and this must stay silent rather than print "0 pairs".
        assert_eq!(contradiction_line(&db, Some(&checkouts)), None);
    }

    /// `PathExists` and `Contains` ask unrelated questions (see
    /// `contradiction_line`'s own doc comment) so GROUND 29 never refuses
    /// this pair at write time - it can only ever be caught here, once the
    /// two checks happen to disagree on the file as it stands today.
    #[test]
    fn contradiction_line_names_a_pair_that_currently_disagrees() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let checkouts = dir.path().join("dev");
        let repo = checkouts.join("Real-Repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::write(repo.join("STYLE.md"), "no special marker in here").unwrap();
        {
            let mut s = EventStore::new(&db).unwrap();
            let mut a = rule("style-guide-must-exist");
            a.text = "the style guide must exist at STYLE.md".to_string();
            a.project = Some("Real-Repo".to_string());
            a.check = Some(Check::PathExists { path: "STYLE.md".to_string() });
            legacy_declare(&mut s, &a);

            let mut b = rule("style-guide-carries-the-marker");
            b.text = "the style guide names the MARKER_TOKEN convention".to_string();
            b.project = Some("Real-Repo".to_string());
            b.check = Some(Check::Contains { path: "STYLE.md".to_string(), literal: "MARKER_TOKEN".to_string() });
            store::declare(&mut s, "s", "l", "a", &b).unwrap();
        }
        // STYLE.md exists (PathExists Holds) but does not contain
        // MARKER_TOKEN (Contains Fails) - a real disagreement right now.
        let line = contradiction_line(&db, Some(&checkouts)).expect("a pair that currently disagrees must be named");
        assert!(line.starts_with("contradictions: "), "{line}");
        assert!(line.contains('1'), "must name the count: {line}");
    }

    #[test]
    fn contradiction_line_is_silent_with_no_checkouts_given() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        EventStore::new(&db).unwrap();
        assert_eq!(contradiction_line(&db, None), None);
    }

    #[test]
    fn store_line_reports_missing_store_without_creating_one() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope.db");
        let line = store_line(&missing);
        assert!(line.contains("not found"), "{line}");
        assert!(!missing.exists(), "the health check must never create a store");
    }

    #[test]
    fn store_line_counts_events_and_reports_an_intact_chain() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        {
            let mut s = EventStore::new(&db).unwrap();
            store::declare(&mut s, "s", "l", "a", &rule("r1")).unwrap();
        }
        let line = store_line(&db);
        assert!(line.contains("intact"), "{line}");
        assert!(line.contains('1'), "{line}");
    }

    #[test]
    fn code_index_line_says_plainly_when_not_configured() {
        assert_eq!(code_index_line(None, None), "code index: not configured");
    }

    #[test]
    fn replica_line_says_plainly_when_not_configured() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        EventStore::new(&db).unwrap();
        assert_eq!(replica_line(&db, None), "replica: not configured");
    }

    // --------------------------------------------------------------- ship
    //
    // THE DEFECT THESE PREVENT A REPEAT OF: a hung, silently-refused ship
    // left no trace anywhere for four weeks (see `ops::ship_state`'s own doc
    // comment). These prove the three shapes this line must take: silent
    // when shipping was never this machine's job, brief when a ship just
    // succeeded, and named - age AND ceiling, never a bare number - when one
    // has not succeeded in far too long.

    /// Mirrors `replica_line_says_plainly_when_not_configured`, but ship is
    /// the ONE component in this report that stays completely silent rather
    /// than saying "not configured" - see `ship_line`'s own doc comment for
    /// why: most machines this ever runs on were never meant to ship at all.
    #[test]
    fn ship_line_is_none_when_this_machine_has_never_shipped() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        EventStore::new(&db).unwrap();
        assert_eq!(ship_line(&db), None, "a machine that never shipped must print nothing, not an alarm");
    }

    /// A success on a store with nothing MORE written since (the common
    /// case in this fixture: the store never grew past whatever
    /// `record_success_at` claimed as `covered_seq`) is FRESH and says so -
    /// this doubles as the proof for "nothing written since the last
    /// successful ship" (case 2 of `ship_line`'s own doc comment), since an
    /// empty store immediately after a success is exactly that case.
    #[test]
    fn ship_line_reports_fresh_just_after_a_successful_ship() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        EventStore::new(&db).unwrap();
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
        crate::ship_state::record_success_at(&db, 7, now).unwrap();

        let line = ship_line(&db).expect("a recorded ship must produce a line");
        assert!(line.starts_with("ship:"), "{line}");
        assert!(line.contains("fresh"), "{line}");
        assert!(line.contains("nothing new has been written since"), "{line}");
        assert!(!line.contains("STALE"), "just-completed must never read as stale: {line}");
    }

    /// Changes are waiting (the store grew past `covered_seq`) but the last
    /// success is still within the ceiling - fresh, same as a healthy
    /// hourly cadence has always read, even though something IS queued.
    #[test]
    fn ship_line_stays_fresh_when_changes_are_waiting_but_within_the_ceiling() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let mut s = EventStore::new(&db).unwrap();
        s.append_event("s", "l", "act", EventKind::FactCreated, "e1", None, "body").unwrap();
        drop(s);
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
        // Covered only seq 0 (nothing) - the one event above is waiting.
        crate::ship_state::record_success_at(&db, 0, now).unwrap();

        let line = ship_line(&db).expect("a recorded ship must produce a line");
        assert!(line.contains("fresh"), "{line}");
        assert!(!line.contains("STALE"), "{line}");
        assert!(!line.contains("not yet on the replica"), "still within the ceiling: {line}");
    }

    /// THE EXACT CASE THE DEFECT NEEDS CAUGHT, UPDATED FOR THE SHIP ALARM
    /// COUNTING WHAT IS WAITING: a ship well past the hourly ceiling, WITH
    /// real changes queued behind it, must say how many, how long ago the
    /// last success was, and that the hourly ship has not run since - and
    /// it must NOT read as a failure (no "STALE", no "failing silently"):
    /// the machine may simply be asleep, which this line can no longer tell
    /// apart from a broken task if it panics at the first sign of age.
    #[test]
    fn ship_line_names_waiting_changes_past_the_ceiling_without_calling_it_a_failure() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let mut s = EventStore::new(&db).unwrap();
        s.append_event("s", "l", "act", EventKind::FactCreated, "e1", None, "body").unwrap();
        s.append_event("s", "l", "act", EventKind::FactCreated, "e2", None, "body").unwrap();
        drop(s);
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
        let stale_by_a_day = now - 24 * 3600;
        // Covered only seq 0 - both events above are waiting.
        crate::ship_state::record_success_at(&db, 0, stale_by_a_day).unwrap();

        let line = ship_line(&db).expect("a recorded ship must produce a line even when stale");
        assert!(line.contains("2 change"), "the count must be named: {line}");
        assert!(line.contains("not yet on the replica"), "{line}");
        assert!(line.contains("24h"), "the age must be named: {line}");
        assert!(line.contains("has not run since"), "{line}");
        assert!(!line.contains("STALE"), "waiting changes are not worded as a failure: {line}");
        assert!(!line.contains("failing silently"), "waiting changes are not worded as a failure: {line}");
    }

    /// BACKWARD COMPATIBILITY: a sidecar written before this split existed
    /// (no `last_attempt_unix` at all) must still fall back to exactly the
    /// STALE wording this line always printed - old data cannot support the
    /// newer "changes waiting" distinction, so this does not try to guess.
    #[test]
    fn ship_line_falls_back_to_stale_wording_for_an_old_two_field_sidecar() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        EventStore::new(&db).unwrap();
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
        let stale_by_a_day = now - 24 * 3600;
        std::fs::write(
            crate::ship_state::path_for(&db),
            format!(r#"{{"completed_unix":{stale_by_a_day},"receiver_seq":3}}"#),
        )
        .unwrap();

        let line = ship_line(&db).expect("an old sidecar must still produce a line");
        assert!(line.contains("STALE"), "{line}");
        assert!(line.contains("24h"), "the age must be named: {line}");
        assert!(
            line.contains(&SHIP_STALE_CEILING_HOURS.to_string()),
            "the ceiling must be named, never a bare number: {line}"
        );
    }

    /// THE ALARM CASE: the most recent attempt failed. Names when it
    /// failed, the reason, and when the last success was - never silence,
    /// and never confused with a healthy "nothing waiting" line.
    #[test]
    fn ship_line_alarms_on_a_failed_attempt_and_names_the_last_success() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        EventStore::new(&db).unwrap();
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
        crate::ship_state::record_success_at(&db, 5, now - 5 * 3600).unwrap();
        crate::ship_state::record_failure_at(&db, "receiver rejected the shared secret (401)", now).unwrap();

        let line = ship_line(&db).expect("a failed attempt must still produce a line");
        assert!(line.contains("FAILED"), "{line}");
        assert!(line.contains("receiver rejected the shared secret (401)"), "{line}");
        assert!(line.contains("last succeeded 5h ago"), "{line}");
    }

    /// The edge of the alarm case: a machine whose FIRST EVER attempt
    /// failed has no last success to name - it must say so plainly instead
    /// of printing a nonsense age computed from an unset timestamp.
    #[test]
    fn ship_line_alarms_on_a_failed_first_attempt_with_no_prior_success() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        EventStore::new(&db).unwrap();
        crate::ship_state::record_failure_at(&db, "connection refused", 1_000_000).unwrap();

        let line = ship_line(&db).expect("a failed attempt must still produce a line");
        assert!(line.contains("FAILED"), "{line}");
        assert!(line.contains("connection refused"), "{line}");
        assert!(line.contains("no successful ship yet"), "{line}");
    }

    #[test]
    fn a_tool_server_started_before_the_program_was_replaced_is_named() {
        let line = stale_servers_line(1_790_000_000, &[(111, 1_789_990_000), (222, 1_790_000_500)]).expect("one is stale");
        assert!(line.starts_with("tool server: 1 running mcp.exe process(es) started before mcp.exe was replaced"), "{line}");
        assert!(line.contains("111 (started 2026-") && !line.contains("222"), "{line}");
        assert!(line.contains("Restart Claude to load the new tool server"), "{line}");
        assert_eq!(stale_servers_line(1_790_000_000, &[(222, 1_790_000_500)]), None, "a newer process is fine");
        assert_eq!(stale_servers_line(1_790_000_000, &[]), None, "none running is fine");
    }

    #[test]
    fn backup_line_is_silent_while_the_backup_is_healthy() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        EventStore::new(&db).unwrap();
        assert_eq!(backup_line(&db), None);
    }

    #[test]
    fn backup_line_names_the_nas_target_and_age_after_a_good_backup() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        EventStore::new(&db).unwrap();
        let at = 1_790_000_000;
        crate::backup_state::record_success_for(
            &db,
            std::path::Path::new("C:/x/acme-backup"),
            "nas",
            "//nas/share/acme-backup",
            at,
            true,
        );
        let line = backup_line_at(&db, at + 3 * 3600).expect("a good backup must be visible");
        assert!(line.starts_with("backup: last copy went to acme-backup (remote nas: //nas/share/acme-backup), 3h ago"), "{line}");
        let lines = report(&db, None, None, None, None, None, false, None);
        assert!(lines.iter().any(|l| l.contains("remote nas: //nas/share/acme-backup")), "{lines:#?}");
    }

    #[test]
    fn backup_line_says_plainly_that_the_backup_is_failing_and_since_when() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        EventStore::new(&db).unwrap();
        let since = 1_790_000_000;
        crate::backup_state::record_failure_at(
            &db,
            std::path::Path::new("C:/x/acme-backup"),
            "git push failed: file thor2/events.jsonl is 106.42 MB; this exceeds the file size limit",
            since,
        )
        .unwrap();
        let line = backup_line_at(&db, since + 31 * 3600).expect("a failing backup must produce a line");
        assert!(line.starts_with("backup to acme-backup failing since 2026-"), "{line}");
        // With a remote on record the target is named in the line.
        crate::backup_state::record_failure_for(
            &db,
            std::path::Path::new("C:/x/acme-backup"),
            "nas",
            "//nas/share/acme-backup",
            "share is gone",
            since + 3600,
        )
        .unwrap();
        let named = backup_line_at(&db, since + 4 * 3600).unwrap();
        assert!(named.starts_with("backup to acme-backup (remote nas: //nas/share/acme-backup) failing since"), "{named}");
        crate::backup_state::record_failure_at(
            &db,
            std::path::Path::new("C:/x/acme-backup"),
            "git push failed: file thor2/events.jsonl is 106.42 MB; this exceeds the file size limit",
            since,
        )
        .unwrap();
        assert!(line.contains("(31h ago)"), "{line}");
        assert!(line.contains("106.42 MB"), "the reason must be in the line: {line}");
        let lines = report(&db, None, None, None, None, None, false, None);
        assert!(lines.iter().any(|l| l.starts_with("backup to acme-backup failing")), "{lines:#?}");
    }

    #[test]
    fn ship_line_is_part_of_the_report_once_a_ship_has_happened() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        EventStore::new(&db).unwrap();
        crate::ship_state::record_success_at(&db, 1, 1_000_000).unwrap();

        let lines = report(&db, None, None, None, None, None, false, None);
        assert!(lines.iter().any(|l| l.starts_with("ship:")), "{lines:#?}");
    }

    /// THE DEFECT THIS PREVENTS: counting a check by its FORM instead of by
    /// what it can do. A `Forbidden` check reads as the strongest thing in the
    /// store, and it is - but only on an item bound Always, because
    /// `absent_guard::find_forbidden_violation` is fed the Always pool and
    /// nothing else. Bound to a moment instead, it passes the write gate,
    /// counted as blocking, and can never block. Found by a review reading the
    /// guard's arms rather than the check list.
    #[test]
    fn a_forbidden_check_without_an_always_binding_is_counted_as_blocking_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        {
            let mut s = EventStore::new(&db).unwrap();

            let mut reaches = rule("forbidden-and-pinned");
            reaches.check = Some(model::item::Check::Forbidden { literals: vec!["\u{2014}".to_string()] });
            legacy_declare(&mut s, &reaches);

            let mut cannot = rule("forbidden-but-not-pinned");
            cannot.text = "a webhook retry backs off before it gives up entirely, and its log never carries U+2014".to_string();
            cannot.project = Some("fixture-project".to_string());
            cannot.bindings = vec![model::item::Binding::Target {
                kind: model::item::TargetKind::Path,
                value: "server/lib/mail.js".to_string(),
            }];
            cannot.check = Some(model::item::Check::Forbidden { literals: vec!["\u{2014}".to_string()] });
            store::declare(&mut s, "s", "l", "a", &cannot).unwrap();
        }
        let line = proof_line(&db, false);
        assert!(line.contains("1 can refuse a write that introduces"), "{line}");
        assert!(line.contains("1 block nothing at all"), "{line}");
    }

    /// The `Requires` counterpart to the `Forbidden` test just above: reach
    /// is a `Command` target, never Always - the gate refuses declaring a
    /// `Requires` check bound Always outright (`gate::declare` ground 23), so
    /// `can_refuse` must count it exactly the way `absent_guard::
    /// find_requires_violation` decides reach: through a Command target, and
    /// nothing else.
    ///
    /// THE "CANNOT" ITEM IS LEGACY ON PURPOSE. Ground 24 (`gate::declare`)
    /// now also requires a `Requires` check to carry a Command binding, for
    /// its trigger to name or to sit inside - which makes a Path-bound
    /// `Requires` item like this one impossible to CREATE through `declare`
    /// any more. A real store
    /// can still hold one from before that ground existed, and `can_refuse`
    /// has to read it correctly regardless, so this fixture is written
    /// straight to the log (`legacy_declare`) rather than through the gate.
    #[test]
    fn a_requires_check_bound_to_a_command_is_counted_as_blocking_an_unbound_one_is_not() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        {
            let mut s = EventStore::new(&db).unwrap();

            let mut reaches = rule("requires-bound-to-a-command");
            reaches.bindings = vec![model::item::Binding::Target {
                kind: model::item::TargetKind::Command,
                value: "Agent".to_string(),
            }];
            reaches.check = Some(model::item::Check::Requires {
                when: "Agent".to_string(),
                required: vec!["haiku".to_string(), "sonnet".to_string()],
            });
            legacy_declare(&mut s, &reaches);

            let mut cannot = rule("requires-bound-to-a-path");
            cannot.text = "a webhook retry backs off before it gives up entirely".to_string();
            cannot.project = Some("fixture-project".to_string());
            cannot.bindings = vec![model::item::Binding::Target {
                kind: model::item::TargetKind::Path,
                value: "server/lib/mail.js".to_string(),
            }];
            cannot.check =
                Some(model::item::Check::Requires { when: "Agent".to_string(), required: vec!["haiku".to_string()] });
            legacy_declare(&mut s, &cannot);
        }
        let line = proof_line(&db, false);
        assert!(line.contains("1 can refuse a write that introduces"), "{line}");
        assert!(line.contains("1 block nothing at all"), "{line}");
    }

    /// The mirror: a `PathExists` check is NOT merely a currency proof when it
    /// meets the location arm's bar (Irreversible, plus a binding equal to the
    /// check's own path). Then it refuses a write for WHERE it lands.
    #[test]
    fn a_path_exists_check_that_meets_the_location_bar_is_counted_as_blocking() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        {
            let mut s = EventStore::new(&db).unwrap();

            let mut blocks = rule("frozen-place");
            blocks.severity = Some(model::item::Severity::Irreversible);
            blocks.project = Some("fixture-project".to_string());
            blocks.bindings =
                vec![model::item::Binding::Target { kind: model::item::TargetKind::Dir, value: "frozen".to_string() }];
            blocks.check = Some(model::item::Check::PathExists { path: "frozen".to_string() });
            store::declare(&mut s, "s", "l", "a", &blocks).unwrap();

            let mut currency = rule("just-currency");
            currency.text = "the estimator rounds a quote up to whole cents".to_string();
            currency.project = Some("fixture-project".to_string());
            currency.bindings = vec![model::item::Binding::Target {
                kind: model::item::TargetKind::Path,
                value: "server/lib/quote.js".to_string(),
            }];
            currency.check = Some(model::item::Check::PathExists { path: "server/lib/quote.js".to_string() });
            store::declare(&mut s, "s", "l", "a", &currency).unwrap();
        }
        let line = proof_line(&db, false);
        assert!(line.contains("1 can refuse a write for landing in a place"), "{line}");
        assert!(line.contains("1 block nothing at all"), "severity below Irreversible does not block: {line}");
    }

    /// THE DEFECT THIS PREVENTS: a health check that reads as fully healthy
    /// while almost nothing in the store can actually block anything. Every
    /// other line here was green on 2026-08-06 with proof coverage at 2 of
    /// 2999, and no line said so.
    #[test]
    fn the_proof_line_reports_coverage_and_never_stays_silent_at_zero() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        {
            let mut s = EventStore::new(&db).unwrap();
            store::declare(&mut s, "s", "l", "a", &rule("prose-only")).unwrap();
        }
        let line = proof_line(&db, false);
        assert!(line.contains("0 of 1"), "a store with no provable rule must say so: {line}");
        assert!(line.contains("block"), "and say what the number decides: {line}");
    }

    /// THE NEW POPULATION, since gate ground 11 (continued, command) started
    /// asking a heavy Command-anchored fact the SHARPER question instead of
    /// the plain one: how many answered it with `no-catch-on-command:<why>`
    /// must show up on this same line, not just the plainer
    /// `no-literal:<why>` bucket the line already counted - and a plain
    /// no-literal answer on a PATH anchor must not be mistaken for one.
    #[test]
    fn the_proof_line_counts_heavy_command_anchored_rules_answered_with_the_sharper_tag() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        {
            let mut s = EventStore::new(&db).unwrap();

            let mut command_answered = rule("command-anchored-no-catch");
            command_answered.text = "an archive run never skips the owner's own pathspec".to_string();
            command_answered.severity = Some(model::item::Severity::Irreversible);
            command_answered.bindings =
                vec![Binding::Target { kind: TargetKind::Command, value: "git archive".to_string() }];
            command_answered.tags = vec![format!(
                "{}the pathspec in 'git archive HEAD' is chosen at call time",
                store::NO_CATCH_ON_COMMAND_REASON_PREFIX
            )];
            store::declare(&mut s, "s", "l", "a", &command_answered).unwrap();

            let mut path_answered = rule("path-anchored-no-literal");
            path_answered.text = "a webhook retry backs off before it gives up entirely".to_string();
            path_answered.project = Some("fixture-project".to_string());
            path_answered.severity = Some(model::item::Severity::Irreversible);
            path_answered.bindings =
                vec![Binding::Target { kind: TargetKind::Path, value: "server/lib/mail.js".to_string() }];
            path_answered.tags = vec![format!(
                "{}an authorised mail.js send looks exactly like an unauthorised one",
                store::NO_LITERAL_REASON_PREFIX
            )];
            store::declare(&mut s, "s", "l", "a", &path_answered).unwrap();
        }
        let line = proof_line(&db, false);
        assert!(line.contains("1 of those anchored to a command"), "names the new population: {line}");
    }

    /// THE GAP THIS CLOSES: `without_reason` was a bare count nobody could
    /// act on, because nothing said which rules made it up. Sorted OLDEST
    /// first (`first_seq` - the earliest event this
    /// store holds for the id), because a bare answer is never re-asked by
    /// any backlog burn (`TeethAnswer::Bare`'s own doc comment): the longer
    /// one has sat bare, the less likely anyone still remembers why, and the
    /// more it deserves the next look. A REASONED answer must never appear in
    /// this list at all - it already said why, which is the whole point of
    /// the distinction this line draws.
    #[test]
    fn proof_line_names_the_bare_no_catch_rules_oldest_first_and_never_a_reasoned_one() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        {
            let mut s = EventStore::new(&db).unwrap();

            // Declared first, so its own first event carries the lowest seq
            // in this store - the oldest thing here, bare with no reason.
            let mut oldest = rule("bare-and-oldest");
            oldest.text = "the first bare rule this fixture ever declared".to_string();
            oldest.tags = vec![store::NO_LITERAL_TAG.to_string()];
            legacy_declare(&mut s, &oldest);

            // Declared second: bare too, but newer, so it must sort after
            // the one above.
            let mut newer = rule("bare-and-newer");
            newer.text = "a second bare rule this fixture declared afterward".to_string();
            newer.tags = vec![store::NO_LITERAL_TAG.to_string()];
            legacy_declare(&mut s, &newer);

            // A reasoned answer, declared through the real gate - must never
            // show up in the bare list, however old it is.
            let mut reasoned = rule("reasoned-not-bare");
            reasoned.text = "a rule that actually said why nothing can catch it".to_string();
            reasoned.tags = vec![format!("{}this fixture quotes 'the same call' as its reason", store::NO_LITERAL_REASON_PREFIX)];
            store::declare(&mut s, "s", "l", "a", &reasoned).unwrap();
        }
        let line = proof_line(&db, false);
        let named: Vec<&str> = line.lines().filter(|l| l.starts_with("  bare no-catch:")).collect();
        assert_eq!(named.len(), 2, "only the bare items, never a reasoned one: {line}");
        assert!(named[0].contains("bare-and-oldest"), "the older bare item must be named first: {line}");
        assert!(named[1].contains("bare-and-newer"), "{line}");
    }

    /// THE TWENTY-ITEM CAP, the same `NAMES_AT_MOST`/`name_cap` mechanism
    /// `decay_line`'s and `judgement_debt_line`'s own named lists already
    /// share: past twenty, the rest are held back and counted in a tail line
    /// - `--full` lifts it, same as it already does for those two.
    #[test]
    fn proof_line_caps_the_bare_no_catch_list_at_twenty_with_a_tail_and_full_names_them_all() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        {
            let mut s = EventStore::new(&db).unwrap();
            for n in 0..25 {
                let mut item = rule(&format!("bare-{n:02}"));
                item.tags = vec![store::NO_LITERAL_TAG.to_string()];
                legacy_declare(&mut s, &item);
            }
        }
        let capped = proof_line(&db, false);
        let capped_named = capped.lines().filter(|l| l.starts_with("  bare no-catch: bare-")).count();
        assert_eq!(capped_named, NAMES_AT_MOST, "must name at most twenty: {capped}");
        assert!(capped.contains("  bare no-catch: and 5 more, not named here"), "{capped}");

        let full = proof_line(&db, true);
        let full_named = full.lines().filter(|l| l.starts_with("  bare no-catch: bare-")).count();
        assert_eq!(full_named, 25, "--full must name every one of them: {full}");
        assert!(!full.contains("not named here"), "{full}");
    }

    /// THE CLASS OF BUG THIS CLOSES: a summary count and the named list
    /// sitting right under it silently disagreeing about what they counted -
    /// this project has already hit exactly that once, on `unjudged_line`,
    /// where the summary said none were open while a fuller read still
    /// found some. `without_reason` and `bare` are built in the SAME branch of the
    /// SAME loop over the SAME `fireable` population (see `proof_line`'s own
    /// body), so within one call they cannot disagree - this test pins that
    /// down as a standing invariant, not an accident of today's code, by
    /// reading the actual claimed number back out of the real sentence
    /// (`number_before`) rather than hard-coding what it "should" say.
    #[test]
    fn proof_line_the_bare_no_catch_count_in_its_own_summary_always_equals_its_named_list() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        {
            let mut s = EventStore::new(&db).unwrap();
            for n in 0..7 {
                let mut item = rule(&format!("bare-invariant-{n:02}"));
                item.tags = vec![store::NO_LITERAL_TAG.to_string()];
                legacy_declare(&mut s, &item);
            }
            // A reasoned answer too - a bug that folded the two answers
            // together would inflate the count without inflating the list,
            // and this is what would catch that.
            let mut reasoned = rule("reasoned-invariant");
            reasoned.tags = vec![format!("{}this fixture quotes 'the same call' as its reason", store::NO_LITERAL_REASON_PREFIX)];
            legacy_declare(&mut s, &reasoned);
        }
        // --full: the named list must never be capped, or a real mismatch
        // past the cap would hide behind the tail line instead of failing.
        let line = proof_line(&db, true);
        let claimed = number_before(&line, " of those without saying why");
        let named = line.lines().filter(|l| l.starts_with("  bare no-catch:")).count();
        assert_eq!(claimed, named, "the summary count and its own named list must agree: {line}");
        assert_eq!(claimed, 7, "sanity: exactly the seven bare fixtures, never the reasoned one: {line}");
    }

    /// THE INVERSION THIS REPORTS. Severity is the first ranking key, so a
    /// heavy item takes a place from every lighter one at the same trigger -
    /// and measured on the owner's real store, not one heavy rule could refuse
    /// anything, while every rule that could was marked house style or nothing.
    /// The store's sense of danger and its actual teeth pointed in opposite
    /// directions and no line said so.
    #[test]
    fn the_teeth_line_counts_heavy_rules_that_can_actually_refuse() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        {
            let mut s = EventStore::new(&db).unwrap();

            let mut heavy_prose = rule("heavy-but-toothless");
            heavy_prose.severity = Some(model::item::Severity::Irreversible);
            store::declare(&mut s, "s", "l", "a", &heavy_prose).unwrap();

            let mut heavy_armed = rule("heavy-and-armed");
            heavy_armed.text = "a webhook retry backs off before it gives up entirely".to_string();
            heavy_armed.severity = Some(model::item::Severity::Costly);
            heavy_armed.check = Some(model::item::Check::Forbidden { literals: vec!["\u{2014}".to_string()] });
            legacy_declare(&mut s, &heavy_armed);

            // Light and armed: counted by proof_line, never by this line.
            let mut light_armed = rule("light-and-armed");
            light_armed.text = "the estimator rounds a quote up to whole cents and never prints U+2026".to_string();
            light_armed.check = Some(model::item::Check::Forbidden { literals: vec!["\u{2026}".to_string()] });
            store::declare(&mut s, "s", "l", "a", &light_armed).unwrap();
        }
        let line = teeth_line(&db, false);
        assert!(line.contains("1 of 2"), "{line}");
        assert!(line.contains("the other 1 can only inform"), "{line}");
    }

    /// Step 20 (NEW-2): a refusal the evaluating session provoked during its
    /// own evaluation is recorded and shown apart - "refused during the
    /// session's own evaluation - not proof" - and holds no field refusal.
    #[test]
    fn teeth_line_shows_refusals_during_the_sessions_own_evaluation_apart() {
        let dir = tempfile::tempdir().unwrap();
        let transcripts = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let session = "0de7dc52-1b97-4b66-948f-61e4438043b2";
        {
            let mut s = EventStore::new(&db).unwrap();
            let mut bitten = rule("self-probed-fixture");
            bitten.text = "never run git push with --force".to_string();
            bitten.bindings = vec![model::item::Binding::Target {
                kind: model::item::TargetKind::Command,
                value: "git push".to_string(),
            }];
            bitten.check = Some(model::item::Check::Forbidden { literals: vec!["--force".to_string()] });
            legacy_declare(&mut s, &bitten);
            serve::attest::fixture::quiet_start(&mut s, session, "fxp");
            serve::attest::fixture::field_refusal(
                &mut s,
                transcripts.path(),
                session,
                "self-probed-fixture",
                "toolu_self",
                "Bash",
                serde_json::json!({"command": "git push --force nowhere-fixture"}),
                "git push --force nowhere-fixture",
            );
        }
        let line = teeth_line(&db, true);
        assert!(line.contains("0 rule(s) of any weight hold a field refusal"), "{line}");
        assert!(line.contains("teeth refusals: 0 of 1 refusal(s) on the log count as field evidence"), "{line}");
        assert!(line.contains("1 refusal(s) refused during the session's own evaluation - not proof"), "{line}");
        assert!(line.contains("the evaluating session made those calls itself"), "{line}");
    }

    /// Step 9 (A7, K6, EV B7): the field column counts only what passes
    /// `serve::attest::is_field_refusal` - a refusal of the hook in a Claude
    /// Code session whose transcript holds the call and its block - and
    /// every other refusal is counted apart, sorted by why. A label, a
    /// reason and a note never count as a refusal.
    #[test]
    fn teeth_line_counts_probe_and_hand_fed_refusals_apart() {
        let dir = tempfile::tempdir().unwrap();
        let transcripts = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        {
            let mut s = EventStore::new(&db).unwrap();
            let mut bitten = rule("bitten-fixture");
            bitten.text = "never run git push with --force".to_string();
            bitten.severity = Some(model::item::Severity::Irreversible);
            bitten.bindings = vec![model::item::Binding::Target {
                kind: model::item::TargetKind::Command,
                value: "git push".to_string(),
            }];
            bitten.check = Some(model::item::Check::Forbidden { literals: vec!["--force".to_string()] });
            legacy_declare(&mut s, &bitten);
            serve::attest::fixture::field_refusal(
                &mut s,
                transcripts.path(),
                "0de7dc52-1b97-4b66-948f-61e4438043b2",
                "bitten-fixture",
                "toolu_bitten",
                "Bash",
                serde_json::json!({"command": "git push --force origin main"}),
                "git push --force origin main",
            );
            let mut labelled = bitten.clone();
            labelled.id = "labelled-only-fixture".to_string();
            labelled.tags = vec![store::TEETH_PROVEN_TAG.to_string()];
            legacy_declare(&mut s, &labelled);
            // A hand-fed refusal from a session that is not a Claude Code
            // session, and one of the retired probe: never counted.
            serve::deliver::record_delivery(&mut s, "probe-1", "probe-1", "hook", "2026-09-29T00:00:00Z", &["labelled-only-fixture".to_string()]);
            serve::deliver::record_gate_outcome(&mut s, "probe-1", "probe-1", "hook", "2026-09-29T00:00:01Z", true, "labelled-only-fixture", "git push --force origin main");
            s.append_event("cli", "cli", store::TEETH_PROBE_ACTOR, EventKind::GateRefused, "labelled-only-fixture", None, "{\"forbidden_holds\":true,\"clean_holds\":true}")
                .unwrap();
            // An old refusal from a Claude Code session, recorded before THOR
            // kept a tool call id: it can never be checked, so it never counts.
            serve::deliver::record_gate_outcome(&mut s, "0de7dc52-1b97-4b66-948f-61e4438043b2", "l", "hook", "2026-09-20T00:00:00Z", true, "labelled-only-fixture", "git push --force origin main");
        }
        let line = teeth_line(&db, true);
        assert!(line.contains("1 rule(s) of any weight hold a field refusal"), "{line}");
        assert!(line.contains("teeth refusals: 1 of 4 refusal(s) on the log count as field evidence; the other 3 never count"), "{line}");
        assert!(line.contains("1 old refusal(s) carry no tool call id and can never be checked"), "{line}");
        assert!(line.contains("1 refusal from a session that is not a Claude Code session (probe-1) - not counted"), "{line}");
        assert!(line.contains("1 by teeth-probe"), "{line}");
        assert!(line.contains("probe or hand-fed refusals (never proof): 2 - 1 by teeth-probe"), "{line}");
        // Red half: without the transcript's verdict the same refusal is no
        // field evidence at all.
        let dir2 = tempfile::tempdir().unwrap();
        let db2 = dir2.path().join("t.db");
        {
            let mut s = EventStore::new(&db2).unwrap();
            let mut bitten = rule("bitten-fixture");
            bitten.text = "never run git push with --force".to_string();
            bitten.bindings = vec![model::item::Binding::Target {
                kind: model::item::TargetKind::Command,
                value: "git push".to_string(),
            }];
            bitten.check = Some(model::item::Check::Forbidden { literals: vec!["--force".to_string()] });
            legacy_declare(&mut s, &bitten);
            serve::deliver::record_delivery(&mut s, "0de7dc52-1b97-4b66-948f-61e4438043b2", "l", "hook", "2026-09-29T00:00:00Z", &["bitten-fixture".to_string()]);
            serve::deliver::record_gate_outcome_for_call(&mut s, "0de7dc52-1b97-4b66-948f-61e4438043b2", "l", "hook", "2026-09-29T00:00:01Z", true, "bitten-fixture", "git push --force origin main", Some("toolu_x"));
        }
        let line = teeth_line(&db2, true);
        assert!(line.contains("0 rule(s) of any weight hold a field refusal"), "{line}");
        assert!(line.contains("1 rule(s) pass the structural part alone"), "{line}");
        assert!(line.contains("1 refusal(s) not verified yet"), "{line}");
    }

    /// FALSE BLOCKS AS A NUMBER: each real refusal (field evidence, verified in
    /// a transcript) is linked to what happened to its rule within 7 days. A
    /// rule weakened or retracted after refusing is a probable false block; one
    /// left alone, one weakened only after the window, a refusal nobody
    /// verified and a refusal older than the 30 days are not counted.
    #[test]
    fn refusal_outcomes_link_each_real_refusal_to_what_happened_to_its_rule() {
        let dir = tempfile::tempdir().unwrap();
        let transcripts = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let force_rule = |id: &str| {
            let mut r = rule(id);
            r.text = format!("never run git push with --force ({id})");
            r.bindings = vec![Binding::Target { kind: TargetKind::Command, value: "git push".to_string() }];
            r.check = Some(Check::Forbidden { literals: vec!["--force".to_string()] });
            r
        };
        let call = serde_json::json!({"command": "git push --force origin main"});
        let weaken = |s: &mut EventStore, id: &str| {
            let item = force_rule(id);
            let mut cleared = item.clone();
            cleared.check = None;
            let baseline = model::gate::ClearedFields { check: true, ..Default::default() }.baseline(&item);
            store::revise_because(s, "s", "l", "mcp", &baseline, &cleared, &item, Some("the --force check blocked a legitimate push")).unwrap();
        };
        {
            let mut s = EventStore::new(&db).unwrap();
            for id in ["rule-weak", "rule-gone", "rule-same", "rule-late"] {
                legacy_declare(&mut s, &force_rule(id));
            }
            for (id, tool) in [("rule-weak", "toolu_1"), ("rule-gone", "toolu_2"), ("rule-same", "toolu_3"), ("rule-late", "toolu_4")] {
                serve::attest::fixture::field_refusal(
                    &mut s,
                    transcripts.path(),
                    serve::attest::fixture::SESSION,
                    id,
                    tool,
                    "Bash",
                    call.clone(),
                    "git push --force origin main",
                );
            }
            // A refusal no transcript backs: never counted.
            serve::deliver::record_gate_outcome_for_call(
                &mut s, serve::attest::fixture::SESSION, "l", "hook", "2026-09-28T00:00:02Z", true, "rule-same", "git push --force origin main", Some("toolu_5"),
            );
            weaken(&mut s, "rule-weak");
            store::retract(&mut s, "s", "l", "mcp", "rule-gone", "the --force check blocked a legitimate push", None).unwrap();
            // Three weeks of other work go by before this one is weakened.
            serve::deliver::record_delivery(&mut s, "other", "other", "hook", "2026-10-20T00:00:00Z", &["rule-same".to_string()]);
            weaken(&mut s, "rule-late");
        }
        let now = serve::time::unix_from_iso8601("2026-10-25T00:00:00Z").unwrap();
        let lines = refusal_outcomes_at(&db, false, now);
        assert!(lines[0].starts_with("refusal outcomes: 4 real refusal(s) in the last 30 days, 2 followed by weakening or retract of the refusing rule within 7 days (probable false blocks), "), "{lines:?}");
        assert!(lines[0].contains(&model::refusal_outcome::rate_words(2, 4)), "{lines:?}");
        assert!(!lines[0].contains("--full"), "two are fewer than the cap, so every one is listed: {lines:?}");
        assert_eq!(lines.len() - 1, 2, "the probable false blocks are named even without --full: {lines:?}");

        let full = refusal_outcomes_at(&db, true, now);
        assert_eq!(full.len(), 3, "{full:?}");
        assert!(full.iter().any(|l| l.starts_with("  probable false block: 'rule-weak' refused 'git push --force origin main' at log seq ") && l.contains(", then weakened (check cleared) at log seq ")), "{full:?}");
        assert!(full.iter().any(|l| l.starts_with("  probable false block: 'rule-gone' refused 'git push --force origin main' at log seq ") && l.contains(", then retracted at log seq ")), "{full:?}");
        assert!(!full[0].contains("--full lists them"), "{full:?}");

        // Silent: every refusal is older than the 30 days.
        let later = serve::time::unix_from_iso8601("2026-12-25T00:00:00Z").unwrap();
        assert!(refusal_outcomes_at(&db, true, later).is_empty());
        // Silent: a store that never refused anything.
        let quiet = dir.path().join("quiet.db");
        {
            let mut s = EventStore::new(&quiet).unwrap();
            legacy_declare(&mut s, &force_rule("rule-quiet"));
        }
        assert!(refusal_outcomes_at(&quiet, true, now).is_empty());
    }

    /// A long list of probable false blocks stops at the cap and says how many
    /// more there are, unless --full; a secret in a refused call is shown
    /// redacted.
    #[test]
    fn refusal_outcomes_cap_the_named_blocks_and_redact_the_refused_call() {
        let dir = tempfile::tempdir().unwrap();
        let transcripts = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let ids: Vec<String> = (0..10).map(|n| format!("rule-{n:02}")).collect();
        {
            let mut s = EventStore::new(&db).unwrap();
            for (n, id) in ids.iter().enumerate() {
                let mut r = rule(id);
                r.text = format!("never run git push with --force ({id})");
                r.bindings = vec![Binding::Target { kind: TargetKind::Command, value: "git push".to_string() }];
                r.check = Some(Check::Forbidden { literals: vec!["--force".to_string()] });
                legacy_declare(&mut s, &r);
                let command = if n == 0 {
                    "git push --force --token ghp_abcdefghijklmnopqrstuvwxyz0123456789ABCD origin main".to_string()
                } else {
                    "git push --force origin main".to_string()
                };
                serve::attest::fixture::field_refusal(
                    &mut s,
                    transcripts.path(),
                    serve::attest::fixture::SESSION,
                    id,
                    &format!("toolu_{n}"),
                    "Bash",
                    serde_json::json!({"command": command}),
                    &command,
                );
                store::retract(&mut s, "s", "l", "mcp", id, "the --force check blocked a legitimate push", None).unwrap();
            }
        }
        let now = serve::time::unix_from_iso8601("2026-10-01T00:00:00Z").unwrap();
        let lines = refusal_outcomes_at(&db, false, now);
        assert_eq!(lines.len(), 1 + model::store::FALSE_BLOCKS_LISTED + 1, "{lines:?}");
        assert!(lines[0].contains("--full lists them all."), "{lines:?}");
        assert_eq!(lines.last().unwrap(), "  probable false block: and 2 more, not listed here");
        let first = lines.iter().find(|l| l.contains("'rule-00'")).expect("rule-00 is named");
        assert!(first.contains("[redacted]") && !first.contains("ghp_abcdef"), "{first}");
        let full = refusal_outcomes_at(&db, true, now);
        assert_eq!(full.len(), 1 + 10, "--full names every one: {full:?}");
    }

    /// Step 14 (Z8, K14): doctor names every `answer-guard:` tag whose entry
    /// is not in the response rulebook beside the store; one whose entry is
    /// there is not named. No rulebook at all: every entry is missing.
    #[test]
    fn answer_guard_line_names_an_entry_missing_from_the_rulebook() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        {
            let mut s = EventStore::new(&db).unwrap();
            let mut say = rule("fx-say");
            say.tags = vec!["answer-guard:no-such-entry".to_string()];
            legacy_declare(&mut s, &say);
            let mut known = rule("fx-known");
            known.tags = vec!["answer-guard:no-plain-language-tldr".to_string()];
            legacy_declare(&mut s, &known);
            legacy_declare(&mut s, &rule("fx-plain"));
        }
        let line = answer_guard_line(&db).expect("no rulebook: both entries are missing");
        assert!(line.contains("answer-guard: 2 rule(s)") && line.contains("does not exist or does not read as a rulebook"), "{line}");

        std::fs::write(
            serve::respond::default_rulebook_path(&db),
            r#"[{"id": "no-plain-language-tldr", "any_of": ["commit "], "reminder": "Open with a summary."}]"#,
        )
        .unwrap();
        let line = answer_guard_line(&db).expect("one entry is still missing");
        assert!(line.contains("answer-guard: 1 rule(s) name a response-rulebook entry that is not there"), "{line}");
        assert!(line.contains("  fx-say - its answer-guard entry 'no-such-entry' is not in the response rulebook"), "{line}");
        assert!(!line.contains("fx-known") && !line.contains("fx-plain"), "{line}");
        assert!(report(&db, None, None, None, None, None, false, None).iter().any(|l| l.contains("fx-say - its answer-guard entry")), "part of doctor's report");

        std::fs::write(
            serve::respond::default_rulebook_path(&db),
            r#"[{"id": "no-plain-language-tldr", "reminder": "a"}, {"id": "no-such-entry", "reminder": "b"}]"#,
        )
        .unwrap();
        assert_eq!(answer_guard_line(&db), None, "every named entry exists: silent");
    }

    /// Step 14 (B4): the open points line - THOR's own counted and named
    /// apart from the legacy ones an agent filed, which are listed for the
    /// owner. Silent on a store with none.
    #[test]
    fn open_points_line_counts_machine_and_legacy_apart() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        {
            let mut s = EventStore::new(&db).unwrap();
            legacy_declare(&mut s, &rule("plain-fixture"));
        }
        assert_eq!(open_points_line(&db, false), None);
        {
            let mut s = EventStore::open_existing(&db).unwrap();
            // A file-check rule of a project with no registered working copy:
            // an evaluation start writes THOR's own point for it.
            let mut held = rule("held-fixture");
            held.project = Some("gamma".to_string());
            held.text = "README.md keeps `x marks it`.".to_string();
            held.bindings = vec![model::item::Binding::Target { kind: model::item::TargetKind::Path, value: "README.md".to_string() }];
            held.check = Some(model::item::Check::Contains { path: "README.md".to_string(), literal: "x marks it".to_string() });
            legacy_declare(&mut s, &held);
            serve::eval_session::start(&db, &mut s, "0de7dc52-1b97-4b66-948f-61e4438043b2", "gamma", "command").expect("the evaluation starts");
            // And one an agent filed before this build.
            let mut old = rule("open-old-fixture");
            old.kind = model::item::Kind::Report;
            old.bindings = vec![];
            old.falsifier = None;
            old.project = Some("gamma".to_string());
            old.text = "held-fixture still needs a real call, later.".to_string();
            old.tags = vec![store::OPEN_POINT_TAG.to_string()];
            legacy_declare(&mut s, &old);
        }
        let line = open_points_line(&db, false).expect("two open points");
        assert!(line.starts_with("open points: 1 written by THOR itself (machine)"), "{line}");
        assert!(line.contains("; 1 legacy (filed by an agent before THOR wrote them itself) - they settle nothing and hold no filing"), "{line}");
        assert!(line.contains("  machine: open-no-working-copy-gamma [gamma] no-working-copy - about gamma"), "{line}");
        assert!(line.contains("  legacy: open-old-fixture [gamma] held-fixture still needs a real call, later."), "{line}");
    }

    /// Step 9: the ROT line - every live rule and orientation without a
    /// current verdict, whatever its weight (D8), its heavy subset beside the
    /// plan's starting line, and the A4-only share with its drain estimate -
    /// and the verdict line, read through `model::store::settlement`. A
    /// rewrite by a bin leaves the rule undecided, with the reason named.
    #[test]
    fn teeth_line_counts_rot_and_verdicts_through_the_one_reader() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        {
            let mut s = EventStore::new(&db).unwrap();
            let mut heavy = rule("heavy-owed");
            heavy.severity = Some(model::item::Severity::Irreversible);
            legacy_declare(&mut s, &heavy);
            legacy_declare(&mut s, &rule("light-owed"));
            let mut a4 = rule("a4-owed");
            a4.bindings = vec![model::item::Binding::Target { kind: model::item::TargetKind::Command, value: "git push".to_string() }];
            a4.check = Some(model::item::Check::Forbidden { literals: vec!["--mirr0r-zz".to_string()] });
            legacy_declare(&mut s, &a4);
            legacy_declare(&mut s, &rule("noted"));
            serve::teeth::fixture::decide(&mut s, "noted");
            legacy_declare(&mut s, &rule("rewritten"));
            serve::teeth::fixture::decide(&mut s, "rewritten");
            let existing = store::show(&s, "rewritten").unwrap();
            let mut rewritten = existing.clone();
            rewritten.text = format!("{} - reworded by a bin", existing.text);
            store::revise(&mut s, "s", "l", "a", &existing, &rewritten).unwrap();
        }
        let line = teeth_line(&db, true);
        assert!(line.contains("teeth rot: 4 live rule(s) and orientation(s) have no current verdict"), "{line}");
        assert!(line.contains("1 of them heavy (the plan's starting line: 306 heavy, 171 with neither)"), "{line}");
        assert!(line.contains("1 of them owe only the A4 naming fix (at 16 per evaluation: 1 evaluation(s))"), "{line}");
        assert!(line.contains("teeth verdicts: 0 proven and 0 wired on the rule track; 1 note(s)"), "{line}");
        assert!(line.contains("  undecided: rewritten [global] its text changed after its last trial"), "{line}");
        assert!(!line.contains("honest_no"), "{line}");
    }

    /// Every dispute doctor lists carries the writer's reason beside it; one
    /// stored before a reason was required says "no reason recorded".
    #[test]
    fn teeth_line_shows_the_reason_beside_each_dispute_and_marks_the_old_ones() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        {
            let mut s = EventStore::new(&db).unwrap();
            let dispute = serve::teeth::Disputes { legitimate_calls: vec!["git bundle list-heads backup.bundle".to_string()], ..Default::default() };
            for (id, text, because) in [
                ("dsp-new", "Never run `git bundle list-heads` on a backup, prove it by cloning.", Some("list-heads is only the example, the rule is about cloning")),
                ("dsp-old", "Avoid zzz qqq www `git bundle list-heads` for eee rrr ttt.", None),
            ] {
                let mut item = rule(id);
                item.text = text.to_string();
                item.bindings = vec![Binding::Target { kind: TargetKind::Command, value: "git bundle".to_string() }];
                let mut corpus = serve::legit::Corpus::load(&s);
                let tried = serve::teeth::trial_for_write(&mut s, Some(&db), &serve::teeth::SessionCtx::default(), &mut corpus, &item, None, None, &dispute, false);
                let serve::teeth::WriteTrial::Tried { item: stored, record, .. } = tried else { panic!("expected a note by dispute") };
                store::declare_in_scoped_because(&mut s, "s", "l", "a", &stored, None, None, Some(&record), None, because).unwrap();
            }
        }
        let line = teeth_line(&db, true);
        assert!(
            line.contains("  note by dispute: dsp-new [global] legitimate_calls ['git bundle list-heads backup.bundle'], may_change [] - because: list-heads is only the example, the rule is about cloning"),
            "{line}"
        );
        assert!(line.contains("  note by dispute: dsp-old [global] legitimate_calls ['git bundle list-heads backup.bundle'], may_change [] - because: no reason recorded"), "{line}");

        // The agent's "confirm the dispute" call is a bare revise with a reason
        // (no trial record of its own): doctor shows the reason it gave.
        {
            let mut s = EventStore::open_existing(&db).unwrap();
            let old = store::show(&s, "dsp-old").unwrap();
            store::revise_tried_scoped(&mut s, "s", "l", "a", &old, &old, &old, Some("checked the file: list-heads is incidental here"), None, None, None, None).unwrap();
        }
        let line = teeth_line(&db, true);
        assert!(
            line.contains("  note by dispute: dsp-old [global] legitimate_calls ['git bundle list-heads backup.bundle'], may_change [] - because: checked the file: list-heads is incidental here"),
            "a reason given by a bare revise is shown: {line}"
        );
    }

    /// The "wired, literal never seen" count (measured 2026-10-03: 70 listed,
    /// 29 of them requires rules whose trial can never count a literal, and 35
    /// that a real session had been refused by - the call corpus drops refused
    /// calls). It now names only a forbidden rule whose literal no call carried
    /// and that no real session was refused by.
    #[test]
    fn teeth_line_never_seen_counts_only_forbidden_rules_that_never_refused_a_real_call() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let session = "0de7dc52-1b97-4b66-948f-61e4438043b2";
        {
            let mut s = EventStore::new(&db).unwrap();
            let on = |item: &mut Item, command: &str| {
                item.bindings = vec![model::item::Binding::Target {
                    kind: model::item::TargetKind::Command,
                    value: command.to_string(),
                }];
            };
            let mut quiet = rule("ban-quiet");
            quiet.text = "never run deploy-zz with --zz-quiet".to_string();
            on(&mut quiet, "deploy-zz");
            quiet.check = Some(model::item::Check::Forbidden { literals: vec!["--zz-quiet".to_string()] });
            let mut bitten = rule("ban-bitten");
            bitten.text = "never run ship-zz with --zz-bitten".to_string();
            on(&mut bitten, "ship-zz");
            bitten.check = Some(model::item::Check::Forbidden { literals: vec!["--zz-bitten".to_string()] });
            let mut asks = rule("requires-quiet");
            asks.text = "pack-zz must carry --zz-asked".to_string();
            on(&mut asks, "pack-zz");
            asks.check = Some(model::item::Check::Requires { when: "pack-zz".to_string(), required: vec!["--zz-asked".to_string()] });
            for item in [&quiet, &bitten, &asks] {
                legacy_declare(&mut s, item);
                serve::teeth::fixture::decide(&mut s, &item.id);
            }
            // A real session's hook refused a call under `ban-bitten`.
            serve::deliver::record_delivery(&mut s, session, "l", "hook", "2026-10-02T00:00:00Z", &["ban-bitten".to_string()]);
            serve::deliver::record_gate_outcome_for_call(&mut s, session, "l", "hook", "2026-10-02T00:00:01Z", true, "ban-bitten", "ship-zz --zz-bitten", Some("toolu_x"));
        }
        let line = teeth_line(&db, true);
        assert!(line.contains("3 wired on the rule track"), "{line}");
        assert!(line.contains("; 1 wired forbidden rule(s) whose literal no real call on record ever carried and that never refused one;"), "{line}");
        assert!(line.contains("wired, literal never seen") && line.contains("ban-quiet"), "{line}");
        let named = line.lines().find(|l| l.contains("wired, literal never seen")).unwrap_or_default();
        assert!(!named.contains("ban-bitten") && !named.contains("requires-quiet"), "{line}");
    }

    /// Step 9: rules settled only by a machine blocker are listed per project
    /// and blocker, and never counted as rot.
    #[test]
    fn teeth_line_lists_blocked_rules_per_project_and_blocker() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        {
            let mut s = EventStore::new(&db).unwrap();
            let mut gamma = rule("gamma-keeps");
            gamma.text = "NOTES.md must keep the line `keep me`.".to_string();
            gamma.project = Some("gamma".to_string());
            gamma.bindings = vec![model::item::Binding::Target { kind: model::item::TargetKind::Path, value: "NOTES.md".to_string() }];
            gamma.check = Some(model::item::Check::Contains { path: "NOTES.md".to_string(), literal: "keep me".to_string() });
            legacy_declare(&mut s, &gamma);
        }
        let line = teeth_line(&db, true);
        assert!(line.contains("  blocked: gamma - no registered working copy: gamma-keeps"), "{line}");
        assert!(line.contains("teeth rot: 0 live rule(s)"), "{line}");
    }

    /// Q24: an old contains check THOR's retest counted as a tooth although
    /// its text does not name the literal is named on its own line, with the
    /// fragment it guards and the recorded cutoff - for the owner to judge.
    #[test]
    fn teeth_line_names_old_contains_checks_with_the_fragment_they_guard() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let beta = dir.path().join("beta");
        let hooks = dir.path().join("no-hooks");
        std::fs::create_dir_all(&hooks).unwrap();
        std::fs::create_dir_all(&beta).unwrap();
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git").arg("-C").arg(&beta).args(args).output().unwrap();
            assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        };
        git(&["init", "--quiet"]);
        git(&["config", "core.hooksPath", &hooks.display().to_string()]);
        git(&["config", "user.email", "fixture@example.invalid"]);
        git(&["config", "user.name", "fixture"]);
        std::fs::write(beta.join(".thor-project"), "beta\n").unwrap();
        std::fs::write(beta.join("NOTES.md"), "keep me\nplain line\n").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "--quiet", "-m", "initial"]);
        {
            let mut s = EventStore::new(&db).unwrap();
            crate::install::register_root(&mut s, &beta, "beta", None).unwrap();
            let mut old = rule("old-keeps");
            old.text = "NOTES.md keeps its opening line.".to_string();
            old.project = Some("beta".to_string());
            old.bindings = vec![model::item::Binding::Target { kind: model::item::TargetKind::Path, value: "NOTES.md".to_string() }];
            old.check = Some(model::item::Check::Contains { path: "NOTES.md".to_string(), literal: "keep me".to_string() });
            legacy_declare(&mut s, &old);
            serve::teeth::fixture::decide(&mut s, "old-keeps");
        }
        let cutoff = model::store::a4_cutoff(&EventStore::open_existing(&db).unwrap()).expect("the retest recorded the cutoff");
        let line = teeth_line(&db, true);
        assert!(line.contains("teeth rot: 0 live rule(s)"), "{line}");
        assert!(line.contains("teeth old contains (Q24): 1 rule(s) on the rule track"), "{line}");
        assert!(line.contains(&format!("the A4 cutoff is log seq {cutoff}, recorded once and never moved")), "{line}");
        assert!(
            line.contains("  old contains, text does not name it: old-keeps [beta] it guards 'keep me' in NOTES.md (its check was set at log seq"),
            "{line}"
        );
    }

    /// Step 22 (C3b B4): a check an agent cleared or moved with a because is a
    /// weakening the owner sees in doctor, beside the rules taken off the rule
    /// track - not only in the item's history. THOR's own writes are not.
    #[test]
    fn teeth_line_names_a_check_cleared_or_moved_by_a_write_that_is_not_thors() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        {
            let mut s = EventStore::new(&db).unwrap();
            let mut plan = rule("plan-here");
            plan.check = Some(model::item::Check::PathExists { path: "files/PLAN.md".to_string() });
            let mut moved = rule("moved-here");
            moved.text = "The fixture keeps `keep me` in its notes.".to_string();
            moved.check = Some(model::item::Check::Contains { path: "docs/A.md".to_string(), literal: "keep me".to_string() });
            legacy_declare(&mut s, &plan);
            legacy_declare(&mut s, &moved);
            let mut cleared = plan.clone();
            cleared.check = None;
            let baseline = model::gate::ClearedFields { check: true, ..Default::default() }.baseline(&plan);
            store::revise_because(&mut s, "s", "l", "mcp", &baseline, &cleared, &plan, Some("the file is in a full anchor")).unwrap();
            let mut elsewhere = moved.clone();
            elsewhere.check = Some(model::item::Check::Contains { path: "docs/B.md".to_string(), literal: "keep me".to_string() });
            store::revise_because(&mut s, "s", "l", "mcp", &moved, &elsewhere, &moved, Some("the file moved")).unwrap();
            // The same file, another literal: the line names both literals.
            let mut reworded = rule("reworded-here");
            reworded.text = "The fixture keeps `keep me` or `keep it` in its notes.".to_string();
            reworded.check = Some(model::item::Check::Contains { path: "docs/C.md".to_string(), literal: "keep me".to_string() });
            legacy_declare(&mut s, &reworded);
            let mut other = reworded.clone();
            other.check = Some(model::item::Check::Contains { path: "docs/C.md".to_string(), literal: "keep it".to_string() });
            store::revise_because(&mut s, "s", "l", "mcp", &reworded, &other, &reworded, Some("the other phrase is the one that matters")).unwrap();
        }
        let line = teeth_line(&db, true);
        assert!(
            line.contains("reworded-here - check changed from contains 'keep me' in docs/C.md to contains 'keep it' in docs/C.md at log seq"),
            "{line}"
        );
        assert!(
            line.contains("  check weakened: plan-here - check cleared (path_exists files/PLAN.md) at log seq 3 (because: the file is in a full anchor; door: mcp)"),
            "{line}"
        );
        assert!(
            line.contains("  check weakened: moved-here - check moved from contains docs/A.md to contains docs/B.md at log seq 4 (because: the file moved; door: mcp)"),
            "{line}"
        );
    }

    /// Step 9 (R7): refused because a tooth THOR built from the text bites,
    /// then stored as a note whose text no longer names it - the reword line;
    /// and a rule taken off the rule track, with its reason and door.
    #[test]
    fn teeth_line_names_a_reword_after_a_refusal_and_a_rule_taken_off_the_track() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        {
            let mut s = EventStore::new(&db).unwrap();
            let body = serde_json::json!({
                "id": "force-push",
                "code": "forcing",
                "forcing": ["forbidden '--force' on 'git push' refuses 'git push --force' and lets 'git push' through"],
                "text_sha": "x",
            })
            .to_string();
            s.append_event("s", "s", store::TEETH_WRITE_ACTOR, EventKind::GateRefused, "force-push", None, &body).unwrap();
            let mut noted = rule("force-push");
            noted.text = "Never rewrite shared history.".to_string();
            legacy_declare(&mut s, &noted);
            serve::teeth::fixture::decide(&mut s, "force-push");
            legacy_declare(&mut s, &rule("gone-rule"));
            s.append_event("s", "s", "mcp", EventKind::ItemServed, "gone-rule", None, "{\"served_at\":\"2999-01-01T00:00:00Z\"}").unwrap();
            store::retract(&mut s, "s", "l", "mcp", "gone-rule", "the thing it named was removed", None).unwrap();
        }
        let line = teeth_line(&db, true);
        assert!(
            line.contains("force-push: refused with forbidden '--force' on 'git push' at log seq 1, then stored as a note that no longer names it"),
            "{line}"
        );
        assert!(line.contains("  taken off the rule track: gone-rule - retracted at log seq"), "{line}");
        assert!(line.contains("(reason: the thing it named was removed; door: mcp)"), "{line}");

        // Green: a refused write followed by one that hangs the named tooth
        // is on the rule track - no reword line.
        let dir2 = tempfile::tempdir().unwrap();
        let db2 = dir2.path().join("t.db");
        {
            let mut s = EventStore::new(&db2).unwrap();
            let body = serde_json::json!({
                "id": "force-push",
                "code": "forcing",
                "forcing": ["forbidden '--force' on 'git push' refuses 'git push --force' and lets 'git push' through"],
                "text_sha": "x",
            })
            .to_string();
            s.append_event("s", "s", store::TEETH_WRITE_ACTOR, EventKind::GateRefused, "force-push", None, &body).unwrap();
            let mut hung = rule("force-push");
            hung.text = "Never run `git push --force` on main.".to_string();
            hung.bindings = vec![model::item::Binding::Target { kind: model::item::TargetKind::Command, value: "git push".to_string() }];
            hung.check = Some(model::item::Check::Forbidden { literals: vec!["--force".to_string()] });
            legacy_declare(&mut s, &hung);
            serve::teeth::fixture::decide(&mut s, "force-push");
        }
        let line = teeth_line(&db2, true);
        assert!(!line.contains("reworded:"), "{line}");

        // Green too: stored on the rule track with another tooth, whose text
        // does not name '--force' either - a rule, never "stored as a note".
        let dir3 = tempfile::tempdir().unwrap();
        let db3 = dir3.path().join("t.db");
        {
            let mut s = EventStore::new(&db3).unwrap();
            let body = serde_json::json!({
                "id": "force-push",
                "code": "forcing",
                "forcing": ["forbidden '--force' on 'git push' refuses 'git push --force' and lets 'git push' through"],
                "text_sha": "x",
            })
            .to_string();
            s.append_event("s", "s", store::TEETH_WRITE_ACTOR, EventKind::GateRefused, "force-push", None, &body).unwrap();
            let mut other = rule("force-push");
            other.text = "Never run `git push --mirror` on main.".to_string();
            other.bindings = vec![model::item::Binding::Target { kind: model::item::TargetKind::Command, value: "git push".to_string() }];
            other.check = Some(model::item::Check::Forbidden { literals: vec!["--mirror".to_string()] });
            legacy_declare(&mut s, &other);
            serve::teeth::fixture::decide(&mut s, "force-push");
        }
        let line = teeth_line(&db3, true);
        assert!(!line.contains("reworded:"), "{line}");
    }

    /// THE ORDERING THIS ENFORCES: heaviest severity first (`model::item::
    /// severity_rank`, the same ranking `serve::rank` itself already selects
    /// delivery order by), because severity is the whole reason this line
    /// exists - an Irreversible rule with no teeth is a worse gap than a
    /// Costly one, and showing only a handful must not let that difference
    /// wash out into an arbitrary pick. Within the same severity, the item
    /// fired most often since its last verdict goes first: a live, exercised
    /// risk outranks a merely theoretical one.
    #[test]
    fn teeth_line_names_the_toothless_heavy_rules_heaviest_first_then_by_how_often_they_fired() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        {
            let mut s = EventStore::new(&db).unwrap();

            let mut costly_rare = rule("costly-and-rarely-served");
            costly_rare.severity = Some(model::item::Severity::Costly);
            legacy_declare(&mut s, &costly_rare);

            let mut irreversible_busy = rule("irreversible-and-often-served");
            irreversible_busy.severity = Some(model::item::Severity::Irreversible);
            legacy_declare(&mut s, &irreversible_busy);
            for i in 0..3 {
                serve::deliver::record_delivery(
                    &mut s, &serve::deliver::nth_session("s", i), "l", serve::usefulness::ASSISTANT_DELIVERY_ACTOR, "2026-09-08T00:00:00Z",
                    &["irreversible-and-often-served".to_string()],
                );
            }

            let mut irreversible_rare = rule("irreversible-and-never-served");
            irreversible_rare.severity = Some(model::item::Severity::Irreversible);
            legacy_declare(&mut s, &irreversible_rare);
        }
        let line = teeth_line(&db, false);
        let named: Vec<&str> = line.lines().filter(|l| l.starts_with("  toothless:")).collect();
        assert_eq!(named.len(), 3, "{line}");
        assert!(named[0].contains("irreversible-and-often-served"), "heaviest and busiest first: {line}");
        assert!(named[1].contains("irreversible-and-never-served"), "same severity, less busy, second: {line}");
        assert!(named[2].contains("costly-and-rarely-served"), "lighter severity last: {line}");
    }

    /// THE TWENTY-ITEM CAP, the same `NAMES_AT_MOST`/`name_cap` mechanism
    /// `decay_line`'s and `judgement_debt_line`'s own named lists already
    /// share: past twenty, the rest are held back and counted in a tail line
    /// - `--full` lifts it, same as it already does for those two.
    #[test]
    fn teeth_line_caps_the_toothless_list_at_twenty_with_a_tail_and_full_names_them_all() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        {
            let mut s = EventStore::new(&db).unwrap();
            for n in 0..25 {
                let mut item = rule(&format!("toothless-{n:02}"));
                item.severity = Some(model::item::Severity::Costly);
                legacy_declare(&mut s, &item);
            }
        }
        let capped = teeth_line(&db, false);
        let capped_named = capped.lines().filter(|l| l.starts_with("  toothless: toothless-")).count();
        assert_eq!(capped_named, NAMES_AT_MOST, "must name at most twenty: {capped}");
        assert!(capped.contains("  toothless: and 5 more, not named here"), "{capped}");

        let full = teeth_line(&db, true);
        let full_named = full.lines().filter(|l| l.starts_with("  toothless: toothless-")).count();
        assert_eq!(full_named, 25, "--full must name every one of them: {full}");
        assert!(!full.contains("not named here"), "{full}");
    }

    /// THE SAME INVARIANT `proof_line_the_bare_no_catch_count_in_its_own_
    /// summary_always_equals_its_named_list` pins down for `proof_line`,
    /// here for `teeth_line`'s own pair: `heavy - heavy_with_teeth` in the
    /// summary sentence and the `toothless` list built alongside it in the
    /// same loop. Both new named lists this change added carry the identical
    /// risk - a summary and its own list drifting apart - so both get the
    /// identical regression test, reading the real claimed number back out
    /// of the sentence (`number_before`) rather than hard-coding it.
    #[test]
    fn teeth_line_the_toothless_count_in_its_own_summary_always_equals_its_named_list() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        {
            let mut s = EventStore::new(&db).unwrap();
            for n in 0..7 {
                let mut item = rule(&format!("toothless-invariant-{n:02}"));
                item.severity = Some(model::item::Severity::Costly);
                legacy_declare(&mut s, &item);
            }
            // A heavy, armed rule too - a bug that folded the toothed and
            // toothless populations together would inflate the claimed
            // count without inflating the list, and this is what would
            // catch that.
            let mut armed = rule("armed-invariant");
            armed.severity = Some(model::item::Severity::Irreversible);
            armed.check = Some(model::item::Check::Forbidden { literals: vec!["nope".to_string()] });
            legacy_declare(&mut s, &armed);
        }
        // --full: the named list must never be capped, or a real mismatch
        // past the cap would hide behind the tail line instead of failing.
        let line = teeth_line(&db, true);
        let claimed = number_before(&line, " can only inform");
        let named = line.lines().filter(|l| l.starts_with("  toothless:")).count();
        assert_eq!(claimed, named, "the summary count and its own named list must agree: {line}");
        assert_eq!(claimed, 7, "sanity: exactly the seven toothless fixtures, never the armed one: {line}");
    }

    /// THE BLIND SPOT THIS REPORTS. A pinned item is skipped by decay outright
    /// and, until 2026-09-07, was skipped by the judgement debt outright too -
    /// both on the reasoning that pinning is itself a verdict. It is also
    /// served in full at every session start. Measured on the real store,
    /// that made 31 items 48.9% of every serving ever made, with 23 of them
    /// never examined by anything. The judgement debt now gives each
    /// never-judged pin exactly one verdict (see `judgement_debt`'s own doc
    /// comment in `bin/serve.rs`), which narrows this line's own count over
    /// time as pins get their first look - it does not change what this line
    /// measures, only how fast the number it reports should shrink.
    #[test]
    fn the_pinned_line_counts_what_no_maintenance_loop_ever_looks_at() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        {
            let mut s = EventStore::new(&db).unwrap();
            store::declare(&mut s, "s", "l", "a", &rule("pinned-one")).unwrap();

            let mut anchored = rule("anchored-one");
            anchored.text = "a webhook retry backs off before it gives up entirely".to_string();
            anchored.bindings = vec![model::item::Binding::Target {
                kind: model::item::TargetKind::Path,
                value: "docs/NOTES.md".to_string(),
            }];
            anchored.project = Some("fixture-project".to_string());
            store::declare(&mut s, "s", "l", "a", &anchored).unwrap();

            for i in 0..3 {
                serve::deliver::record_delivery(&mut s, &serve::deliver::nth_session("s", i), "s", "t", "2026-08-08T00:00:00Z", &["pinned-one".to_string()]);
            }
            serve::deliver::record_delivery(&mut s, "s", "s", "t", "2026-08-08T00:00:00Z", &["anchored-one".to_string()]);
        }
        let line = pinned_line(&db);
        assert!(line.contains("1 item(s)"), "{line}");
        assert!(line.contains("1 never examined"), "{line}");
        assert!(line.contains("75%"), "three of four servings came from the pin: {line}");
    }

    #[test]
    fn the_gate_line_says_plainly_when_the_gate_has_never_done_anything() {
        // A fresh store must not read as "fine". A gate with zero refusals is
        // either a very well behaved session or a gate that is not wired in,
        // and the line has to leave both readings open rather than print a
        // reassuring nothing.
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        {
            let _ = EventStore::new(&db).unwrap();
        }
        let line = gate_line(&db);
        assert!(line.contains("never refused"), "{line}");
    }

    #[test]
    fn the_gate_line_counts_refusals_and_stand_asides_apart() {
        // Apart, and never summed: a refusal is visible to whoever it
        // refused, a stand-aside is visible to nobody, and it was exactly the
        // invisible half that hid a broken gate for the whole of 2.0 until
        // 2026-08-08.
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        {
            let mut s = EventStore::new(&db).unwrap();
            for (kind, id) in [
                (thor_core::event_store::EventKind::GateRefused, "rule-a"),
                (thor_core::event_store::EventKind::GateRefused, "rule-a"),
                (thor_core::event_store::EventKind::GateRefused, "rule-b"),
                (thor_core::event_store::EventKind::GateStoodAside, "rule-c"),
            ] {
                s.append_event("s", "l", "t", kind, id, None, "{}").unwrap();
            }
        }
        let line = gate_line(&db);
        assert!(line.contains("3 refusals"), "{line}");
        assert!(line.contains("2 distinct rules"), "the same rule twice is one rule: {line}");
        assert!(line.contains("1 stand-aside"), "{line}");
    }

    /// THOR's own records share the `gate_stood_aside` kind without being a
    /// stand-aside: the attester's verification of every write at each Stop,
    /// the evaluation validator's record and the A4 cutoff. Counted as
    /// stand-asides they would swell the one count that says the gate held
    /// off (step 19, measured on a store copy: the cutoff alone turned 0 into 1).
    #[test]
    fn the_gate_line_never_counts_thors_own_records_as_stand_asides() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        {
            let mut s = EventStore::new(&db).unwrap();
            let stood = thor_core::event_store::EventKind::GateStoodAside;
            s.append_event("s", "l", "hook", stood, "rule-c", None, "{}").unwrap();
            for actor in [model::store::ATTESTER_ACTOR, model::store::EVAL_VALIDATOR_ACTOR, "retest"] {
                s.append_event("s", "l", actor, stood, "rule-d", None, "{}").unwrap();
            }
        }
        let line = gate_line(&db);
        assert!(line.contains(" 1 stand-asides, lifetime"), "only the hook's own stand-aside counts: {line}");
    }

    #[test]
    fn the_gate_line_is_part_of_the_report_not_an_optional_extra() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        {
            let _ = EventStore::new(&db).unwrap();
        }
        let lines = report(&db, None, None, None, None, None, false, None);
        assert!(lines.iter().any(|l| l.starts_with("gate:")), "{lines:#?}");
    }

    #[test]
    fn the_proof_line_is_part_of_the_report_not_an_optional_extra() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        {
            let mut s = EventStore::new(&db).unwrap();
            store::declare(&mut s, "s", "l", "a", &rule("prose-only")).unwrap();
        }
        let lines = report(&db, None, None, None, None, None, false, None);
        assert!(
            lines.iter().any(|l| l.starts_with("provable rules:")),
            "the health check must report proof coverage every run: {lines:?}"
        );
    }

    #[test]
    fn falsifier_line_counts_missing_ones() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        {
            let mut s = EventStore::new(&db).unwrap();
            store::declare(&mut s, "s", "l", "a", &rule("has-one")).unwrap();
            s.append_event(
                "s", "l", "a", thor_core::event_store::EventKind::FactCreated, "old-1", None,
                r#"{"id":"old-1","kind":"rule","text":"pre-migration","bindings":[],"severity":null,"project":null,"tags":[],"expires":null,"key":null}"#,
            )
            .unwrap();
        }
        let store = EventStore::open_existing(&db).unwrap();
        let _ = store; // keep the connection alive for the duration of the call below
        let line = falsifier_line(&db);
        assert!(line.contains("1 of 2"), "{line}");
    }

    #[test]
    fn report_has_one_line_per_component_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        EventStore::new(&db).unwrap();
        let lines = report(&db, None, None, None, None, None, false, None);
        // 17: `wal_line` stays silent on a fresh store (see its own doc
        // comment - it never contributes a line unless the WAL has actually
        // outgrown both the 64 MB floor and the store itself), but
        // `dead_moment_bindings_line` always says something, "none" included,
        // and so does `working_copy_lines`, the volume line (step 28, "nothing
        // recorded yet") and (step 17) the cadence line.
        assert_eq!(lines.len(), 18);
        assert!(lines[0].starts_with("memory store:"));
        assert!(lines[1].starts_with("dead moments:"));
        assert!(lines[2].starts_with("command anchors never fired:"));
        assert!(lines[3].starts_with("code index:"));
        assert!(lines[4].starts_with("replica:"));
        assert!(lines[5].starts_with("falsifiers:"));
        assert!(lines[6].starts_with("provable rules:"));
        // Straight after the coverage number, on purpose: one says how many
        // rules COULD refuse, the next says how often one DID. Read apart
        // they mislead in opposite directions.
        assert!(lines[7].starts_with("gate:"));
        // Next to the gate on purpose: one says how often it fired, the next
        // two say where it has no teeth and what nothing ever re-reads.
        assert!(lines[8].starts_with("teeth:"));
        assert!(lines[9].starts_with("pinned:"));
        assert!(lines[10].starts_with("decay:"));
        assert!(lines[11].starts_with("crowding:"));
        assert!(lines[12].starts_with("unjudged:"));
        assert!(lines[13].starts_with("semantic search:"));
        assert!(lines[14].starts_with("project keys:"));
        assert!(lines[15].starts_with("working copies:"));
        assert!(lines[16].starts_with("volume: nothing recorded yet"));
        assert!(lines[17].starts_with("evaluation cadence:"));
    }

    /// STEP 28: doctor shows the volume - what was shown in full, what was left
    /// out, the most shown facts and the Stop holds per session - for the last
    /// 24 hours and 7 days, from the record the hooks keep.
    #[test]
    fn doctor_shows_the_volume_of_the_last_day_and_week() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        EventStore::new(&db).unwrap();
        serve::volume::record_showing(&db, "s1", &["fact-a", "fact-b"], 3);
        serve::volume::record_stop(&db, "s1", true);
        serve::volume::record_stop(&db, "s1", false);
        let lines = volume_lines(&db);
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(lines[0].starts_with("volume, last 24 hours: 2 fact(s) shown in full"), "{}", lines[0]);
        assert!(lines[0].contains("3 repeat(s) left out") && lines[0].contains("most shown: fact-a (1x), fact-b (1x)"), "{}", lines[0]);
        assert!(lines[0].contains("1 Stop hold(s) in 1 session(s), 1.0 per session on average"), "{}", lines[0]);
        assert!(lines[1].starts_with("volume, last 7 days: 2 fact(s) shown in full"), "{}", lines[1]);
        assert!(report(&db, None, None, None, None, None, false, None).iter().any(|l| l.starts_with("volume, last 24 hours")));
    }

    /// Every registration is listed, with its date and where it moved from;
    /// a registration that can no longer be used says why; and the projects
    /// that own rules but have no registered working copy are named with the
    /// command that registers one, carrying `--db` (F7).
    #[test]
    fn doctor_lists_every_working_copy_and_the_projects_without_one() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        EventStore::new(&db).unwrap();
        let hooks = dir.path().join("no-hooks");
        std::fs::create_dir_all(&hooks).unwrap();
        let checkout = |path: &Path, key: &str| {
            let git = |args: &[&str]| {
                let out = std::process::Command::new("git").arg("-C").arg(path).args(args).output().unwrap();
                assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
            };
            std::fs::create_dir_all(path).unwrap();
            git(&["init", "--quiet"]);
            git(&["config", "core.hooksPath", &hooks.display().to_string()]);
            git(&["config", "user.email", "fixture@example.invalid"]);
            git(&["config", "user.name", "fixture"]);
            std::fs::write(path.join(".thor-project"), format!("{key}\n")).unwrap();
            git(&["add", "-A"]);
            git(&["commit", "--quiet", "-m", "initial"]);
        };
        let alpha = dir.path().join("alpha");
        checkout(&alpha, "alpha");
        let beta = dir.path().join("beta");
        checkout(&beta, "beta");
        {
            let mut s = EventStore::new(&db).unwrap();
            crate::install::register_root(&mut s, &alpha, "alpha", None).unwrap();
            crate::install::register_root(&mut s, &beta, "beta", None).unwrap();
            let mut owned = rule("gamma-rule");
            owned.project = Some("gamma".to_string());
            store::declare(&mut s, "s", "l", "a", &owned).unwrap();
        }
        let moved = dir.path().join("beta-moved");
        std::fs::rename(&beta, &moved).unwrap();
        {
            let mut s = EventStore::new(&db).unwrap();
            crate::install::register_root(&mut s, &moved, "beta", None).unwrap();
        }
        std::fs::write(alpha.join(".thor-project"), "renamed\n").unwrap();

        let lines = working_copy_lines(&db);
        let slash = |p: &Path| p.display().to_string().replace('\\', "/");
        let alpha_line = lines.iter().find(|l| l.starts_with("working copy: alpha ->")).expect("alpha is listed");
        assert!(alpha_line.contains(&slash(&alpha)), "{alpha_line}");
        assert!(alpha_line.contains("NOT USABLE") && alpha_line.contains("resolves to project 'renamed'"), "{alpha_line}");
        let beta_line = lines.iter().find(|l| l.starts_with("working copy: beta ->")).expect("beta is listed");
        assert!(beta_line.starts_with(&format!("working copy: beta -> {} (registered ", slash(&moved))), "{beta_line}");
        assert!(beta_line.ends_with(&format!("), moved from {}", slash(&beta))), "{beta_line}");
        let missing = lines.iter().find(|l| l.starts_with("working copies:")).expect("the unregistered line");
        assert!(missing.contains("1 project(s) own rules but have no registered working copy: gamma (1 rule(s))"), "{missing}");
        assert!(missing.contains(&format!("--register-root --db \"{}\"", db.display())), "{missing}");
    }

    /// THE DEFECT THIS PREVENTS: a health line that goes quiet exactly when it
    /// has nothing to read. Without a checkout there is no way to know which
    /// pool an item competes in, and guessing would be worse than saying so -
    /// the same stance `decay_line` takes one line above.
    #[test]
    fn crowding_says_what_it_needs_rather_than_guessing() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        EventStore::new(&db).unwrap();
        assert!(crowding_line(&db, None, false).contains("--checkouts"), "{}", crowding_line(&db, None, false));
    }

    /// The empty case has to read as an answer, not as a failure: a store whose
    /// every item wins a place somewhere is healthy, and a line that stays
    /// silent about that is indistinguishable from one that broke.
    #[test]
    fn crowding_on_a_store_with_nothing_to_probe_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        EventStore::new(&db).unwrap();
        let checkouts = dir.path().join("checkouts");
        std::fs::create_dir_all(&checkouts).unwrap();
        let line = crowding_line(&db, Some(&checkouts), false);
        assert!(line.starts_with("crowding:"), "{line}");
        assert!(!line.contains("--checkouts"), "a checkout WAS given: {line}");
    }

    /// THE DEFECT THIS PREVENTS (measured 2026-09-17, Printer-stuff eval 2):
    /// `serve why --command "node tools/push.mjs"` showed 7 items applying
    /// and 4 shown, while this line - probing `TargetKind::Path` bindings
    /// only - reported nothing for that project at all. More than
    /// `serve::render::MAX_ITEMS` live items bound to the same COMMAND must
    /// be counted and named too, the invisible ones tagged "(command)" so a
    /// reader can tell them apart from a file anchor in the same list.
    #[test]
    fn crowding_line_also_probes_a_command_anchor_and_names_it_as_such() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let checkouts = dir.path().join("dev");
        let repo = checkouts.join("Cmd-Project");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        {
            let mut store = EventStore::new(&db).unwrap();
            for i in 0..(serve::render::MAX_ITEMS + 1) {
                let mut item = rule(&format!("cmd-crowd-{i}"));
                item.text = format!("fixture command crowd case {i}");
                item.project = Some("Cmd-Project".to_string());
                item.bindings =
                    vec![Binding::Target { kind: TargetKind::Command, value: "npm run deploy".to_string() }];
                legacy_declare(&mut store, &item);
            }
            let line = crowding_line(&db, Some(&checkouts), false);
            assert!(line.starts_with("crowding:"), "{line}");
            assert!(!line.contains("none -"), "a real command crowd must not read as healthy: {line}");
            assert!(
                line.contains("npm run deploy (command) in Cmd-Project"),
                "the command anchor must be named, tagged, and scoped to its project: {line}"
            );
        }
    }

    /// Two registered checkouts, each with one project item at the command
    /// "npm run deploy", so both are probed; the first is then crowded with
    /// `MAX_ITEMS` irreversible project items, the second is not. Returns the
    /// store path and the checkouts root.
    fn two_projects_at_one_command(dir: &Path, crowd_alpha: bool) -> (std::path::PathBuf, std::path::PathBuf) {
        let db = dir.join("t.db");
        let checkouts = dir.join("dev");
        let deploy = || vec![Binding::Target { kind: TargetKind::Command, value: "npm run deploy".to_string() }];
        let mut store = EventStore::new(&db).unwrap();
        for project in ["Alpha-Crowded", "Beta-Quiet"] {
            std::fs::create_dir_all(checkouts.join(project).join(".git")).unwrap();
            let count = if project == "Alpha-Crowded" && crowd_alpha { serve::render::MAX_ITEMS } else { 1 };
            for i in 0..count {
                let mut item = rule(&format!("{}-owner-{i}", project.to_lowercase()));
                item.text = format!("fixture project rule {i} for {project}");
                item.project = Some(project.to_string());
                item.severity = Some(model::item::Severity::Irreversible);
                item.bindings = deploy();
                legacy_declare(&mut store, &item);
            }
        }
        let mut global = rule("global-deploy-note");
        global.text = "fixture global rule about deploying".to_string();
        global.bindings = deploy();
        legacy_declare(&mut store, &global);
        (db, checkouts)
    }

    /// THE DEFECT THIS PREVENTS (field test 12, R1): a GLOBAL item was never
    /// probed at its own anchor, and counted only when a project's probe
    /// reached it - so a global rule outranked in the one project of an
    /// incident, but shown in another, was invisible to this line.
    #[test]
    fn crowding_line_names_a_global_item_outranked_in_one_project() {
        let dir = tempfile::tempdir().unwrap();
        let (db, checkouts) = two_projects_at_one_command(dir.path(), true);
        let line = crowding_line(&db, Some(&checkouts), false);
        assert!(line.contains("crowding (global): 1 global item(s)"), "{line}");
        assert!(
            line.contains("outranked: global-deploy-note (global) at npm run deploy (command) in Alpha-Crowded"),
            "{line}"
        );
        assert!(!line.contains("Beta-Quiet"), "it wins in the quiet project, which must not be named: {line}");
    }

    /// The other half: a global item that is shown in every project it
    /// applies in is healthy and must not be counted.
    #[test]
    fn crowding_line_does_not_count_a_global_item_shown_everywhere() {
        let dir = tempfile::tempdir().unwrap();
        let (db, checkouts) = two_projects_at_one_command(dir.path(), false);
        let line = crowding_line(&db, Some(&checkouts), false);
        assert!(!line.contains("global"), "{line}");
        assert!(line.starts_with("crowding: none"), "{line}");
    }

    // ------------------------------------------------------- semantic_line

    #[test]
    fn semantic_line_is_never_blank_regardless_of_the_store() {
        // Named after the defect it prevents: doctor must always state the
        // active search mode, in every build - this is the one health line
        // that never even opens the store to answer.
        let line = semantic_line(None);
        assert!(line.starts_with("semantic search:"), "{line}");
    }

    #[test]
    fn semantic_line_agrees_with_serves_own_search_mode() {
        // Deliberately never relies on which --features flag built THIS
        // binary (that would make the test's own expectation a guess): it
        // reads `serve::semantic_paths::search_mode` for the same explicit,
        // deliberately-empty directory and checks doctor's line agrees with
        // it - meaningful in either build. Passes an explicit temp dir
        // rather than `None`, since a real model may already be installed
        // at this machine's own default model directory.
        let dir = tempfile::tempdir().unwrap(); // no model files at all
        let line = semantic_line(Some(dir.path()));
        match serve::semantic_paths::search_mode(Some(dir.path())) {
            serve::semantic_paths::SearchMode::CompiledOut => assert!(line.contains("not compiled"), "{line}"),
            serve::semantic_paths::SearchMode::ModelMissing => assert!(line.contains("no model found"), "{line}"),
            serve::semantic_paths::SearchMode::ModelPresent => panic!("an empty directory must never read as present"),
        }
    }

    /// THE DEFECT THIS LINE EXISTS FOR. Three times on 2026-08-03 an item
    /// named a project that resolution could never produce, and every time it
    /// was invisible: a scoped-out rule looks exactly like a rule that never
    /// matched. This asserts the report NAMES such a key and says how many
    /// items hold it, rather than counting them as healthy.
    #[test]
    fn a_project_key_no_checkout_answers_to_is_named_with_its_weight() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("store.db");
        {
            let mut store = EventStore::new(&db).unwrap();
            let mut owned = rule("orphan-1");
            owned.project = Some("Nobody-Answers-To-This".to_string());
            store::declare(&mut store, "s", "l", "a", &owned).unwrap();
        }
        // A checkouts root with one real checkout that resolves to something else.
        let checkouts = dir.path().join("dev");
        let repo = checkouts.join("Some-Other-Repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();

        let line = orphan_projects_line(&db, Some(&checkouts));
        assert!(line.contains("Nobody-Answers-To-This"), "the key must be named: {line}");
        assert!(line.contains("1 item"), "and weighted by how many items hold it: {line}");
    }

    #[test]
    fn a_store_whose_keys_all_resolve_says_so_plainly() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("store.db");
        let checkouts = dir.path().join("dev");
        let repo = checkouts.join("Real-Repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        {
            let mut store = EventStore::new(&db).unwrap();
            let mut owned = rule("ok-1");
            owned.project = Some("Real-Repo".to_string());
            store::declare(&mut store, "s", "l", "a", &owned).unwrap();
        }
        let line = orphan_projects_line(&db, Some(&checkouts));
        assert!(line.contains("every key in use resolves"), "{line}");
    }

    // --------------------------------------------------------- gate_verdict

    /// THE DEFECT THIS PREVENTS: a cloud session with no thor.db at all
    /// getting blocked by --gate for the absence of something it was never
    /// going to have. FAIL OPEN on a missing store is not a nicety here - it
    /// is the one behaviour --gate must never regress on.
    #[test]
    fn gate_verdict_fails_open_when_the_store_does_not_exist() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope.db");
        assert_eq!(gate_verdict(&missing, None, None), GateVerdict::NotAvailable);
        assert!(!missing.exists(), "judging the gate must never create a store");
    }

    /// THE DEFECT THIS PREVENTS: a store that is present but corrupt or
    /// mid-sync (a half-written replica, a file that is not a database at
    /// all) turning --gate into a build failure that says nothing about
    /// whether the change actually being gated is wrong.
    #[test]
    fn gate_verdict_fails_open_when_the_store_cannot_be_read() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("not-a-store.db");
        std::fs::write(&db, b"not a sqlite file").unwrap();
        assert_eq!(gate_verdict(&db, None, None), GateVerdict::NotAvailable);
    }

    /// THE DEFECT THIS PREVENTS: --gate failing every single commit just
    /// because decay could never be checked (no --checkouts given) or
    /// because most rules are prose-only - neither is a defect in what
    /// changed, both are whole-store debt, and failing on either is exactly
    /// the noise that trains people to bypass a gate.
    #[test]
    fn gate_verdict_is_clean_on_a_healthy_store_with_nothing_gate_worthy() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        {
            let mut s = EventStore::new(&db).unwrap();
            store::declare(&mut s, "s", "l", "a", &rule("prose-only")).unwrap();
        }
        assert_eq!(gate_verdict(&db, None, None), GateVerdict::Clean);
    }

    /// THE DEFECT THIS PREVENTS: an item whose anchor points at nothing - it
    /// fires nowhere - reading as a clean gate, the exact rot that sat
    /// undetected across 128 anchors in four projects until `decay_line` was
    /// built (see `decay_check`'s own doc comment).
    #[test]
    fn gate_verdict_fails_on_a_dead_anchor() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let checkouts = dir.path().join("dev");
        let repo = checkouts.join("Real-Repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        {
            let mut s = EventStore::new(&db).unwrap();
            let mut item = rule("dead-anchor-1");
            item.project = Some("Real-Repo".to_string());
            item.bindings = vec![Binding::Target { kind: TargetKind::Path, value: "missing.txt".to_string() }];
            store::declare(&mut s, "s", "l", "a", &item).unwrap();
        }
        assert_eq!(gate_verdict(&db, Some(&checkouts), None), GateVerdict::Failing);
    }

    /// A main checkout at `main` holding `file` with `main_text`, and a
    /// linked worktree of it at `worktree` holding the same file with
    /// `worktree_text` - the `.git` file and the `.git/worktrees/<slug>`
    /// directory laid out the way git itself lays them out.
    fn main_and_worktree(main: &Path, worktree: &Path, file: &str, main_text: &str, worktree_text: &str) {
        let slug = worktree.file_name().unwrap().to_string_lossy().to_string();
        std::fs::create_dir_all(main.join(".git").join("worktrees").join(&slug)).unwrap();
        std::fs::create_dir_all(worktree).unwrap();
        let gitdir = main.join(".git").join("worktrees").join(&slug);
        std::fs::write(worktree.join(".git"), format!("gitdir: {}\n", gitdir.display())).unwrap();
        std::fs::write(main.join(file), main_text).unwrap();
        std::fs::write(worktree.join(file), worktree_text).unwrap();
    }

    /// THE DEFECT THIS PREVENTS (2026-09-26): doctor, run from a detached
    /// worktree kept in a scratch folder, took that scratch folder as its
    /// checkouts root and judged every check against a worktree that was
    /// behind main - it called a true fact false, and the session revised
    /// the fact backwards. Standing in a worktree must infer the parent of
    /// the MAIN checkout.
    #[test]
    fn standing_in_a_worktree_infers_the_main_checkouts_parent() {
        let dir = tempfile::tempdir().unwrap();
        let dev = dir.path().join("dev");
        let main = dev.join("Real-Repo");
        let worktree = dir.path().join("scratch").join("fresh-1");
        main_and_worktree(&main, &worktree, "code.js", "new code\n", "old code\n");
        assert_eq!(infer_checkouts_root(&worktree).as_deref(), Some(dev.as_path()));
    }

    /// And a worktree met under a checkouts root is judged as its main
    /// checkout: the proof below holds on main, is false in the lagging
    /// worktree, and must read as clean.
    #[test]
    fn a_worktree_under_the_checkouts_root_is_judged_as_its_main_checkout() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let main = dir.path().join("elsewhere").join("Real-Repo");
        let scratch = dir.path().join("scratch");
        main_and_worktree(&main, &scratch.join("fresh-1"), "code.js", "handles res.ok\n", "ignores the reply\n");
        {
            let mut s = EventStore::new(&db).unwrap();
            let mut item = rule("true-on-main");
            item.project = Some("Real-Repo".to_string());
            item.bindings = vec![Binding::Target { kind: TargetKind::Path, value: "code.js".to_string() }];
            item.check = Some(model::item::Check::Contains { path: "code.js".to_string(), literal: "res.ok".to_string() });
            legacy_declare(&mut s, &item);
        }
        assert_eq!(gate_verdict(&db, Some(&scratch), None), GateVerdict::Clean, "judged on main, where it holds");
    }

    /// The contrast case for the test above: an anchor that DOES resolve
    /// must never fail the gate, or every project would fail on day one.
    #[test]
    fn gate_verdict_is_clean_when_every_anchor_resolves() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let checkouts = dir.path().join("dev");
        let repo = checkouts.join("Real-Repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::write(repo.join("present.txt"), "content").unwrap();
        {
            let mut s = EventStore::new(&db).unwrap();
            let mut item = rule("live-anchor-1");
            item.project = Some("Real-Repo".to_string());
            item.bindings = vec![Binding::Target { kind: TargetKind::Path, value: "present.txt".to_string() }];
            store::declare(&mut s, "s", "l", "a", &item).unwrap();
        }
        assert_eq!(gate_verdict(&db, Some(&checkouts), None), GateVerdict::Clean);
    }

    /// THE DEFECT THIS PREVENTS: a rule whose own machine-runnable claim is
    /// presently false reading as a clean gate because nothing besides the
    /// anchor was ever checked.
    #[test]
    fn gate_verdict_fails_on_a_proof_that_now_comes_out_false() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let checkouts = dir.path().join("dev");
        let repo = checkouts.join("Real-Repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        {
            let mut s = EventStore::new(&db).unwrap();
            let mut item = rule("false-proof-1");
            item.project = Some("Real-Repo".to_string());
            item.check = Some(Check::PathExists { path: "missing.txt".to_string() });
            legacy_declare(&mut s, &item);
        }
        assert_eq!(gate_verdict(&db, Some(&checkouts), None), GateVerdict::Failing);
    }

    /// THE DEFECT THIS PREVENTS: two checkouts sharing one --checkouts
    /// directory means one repository's rot could fail a DIFFERENT
    /// repository's gate - the exact reason --project exists.
    #[test]
    fn gate_verdict_with_a_project_filter_ignores_another_projects_rot() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let checkouts = dir.path().join("dev");
        std::fs::create_dir_all(checkouts.join("Repo-A").join(".git")).unwrap();
        std::fs::create_dir_all(checkouts.join("Repo-B").join(".git")).unwrap();
        {
            let mut s = EventStore::new(&db).unwrap();
            let mut rotten = rule("rotten-in-a");
            rotten.project = Some("Repo-A".to_string());
            rotten.bindings = vec![Binding::Target { kind: TargetKind::Path, value: "missing.txt".to_string() }];
            store::declare(&mut s, "s", "l", "a", &rotten).unwrap();

            let mut clean = rule("clean-in-b");
            clean.project = Some("Repo-B".to_string());
            store::declare(&mut s, "s", "l", "a", &clean).unwrap();
        }

        assert_eq!(
            gate_verdict(&db, Some(&checkouts), None),
            GateVerdict::Failing,
            "unscoped, Repo-A's dead anchor must still fail the gate"
        );
        assert_eq!(
            gate_verdict(&db, Some(&checkouts), Some("Repo-B")),
            GateVerdict::Clean,
            "Repo-B's own items are fine, and Repo-A's rot must not reach it"
        );
        assert_eq!(
            gate_verdict(&db, Some(&checkouts), Some("Repo-A")),
            GateVerdict::Failing,
            "scoped to its own project, Repo-A's dead anchor must still fail"
        );
    }

    /// THE DEFECT THIS PREVENTS: a tampered event log reading as a clean
    /// gate because --gate only ever looked at decay. A broken chain is
    /// worse than any single item's rot, so it fails regardless of
    /// --project, exactly as `gate_verdict`'s own doc comment says it must.
    #[test]
    fn gate_verdict_fails_on_a_broken_chain_regardless_of_project() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        {
            let mut s = EventStore::new(&db).unwrap();
            store::declare(&mut s, "s", "l", "a", &rule("r1")).unwrap();
        }
        {
            // Tamper the row directly, the same way
            // `test_fsck_recomputes_hashes_on_tampered_fields` (core::auditor)
            // proves a stale body_ch does not hide a body flip: this_hash was
            // computed over the ORIGINAL body at write time, so recomputing it
            // over the tampered body can never match again.
            let s = EventStore::open_existing(&db).unwrap();
            s.conn().execute("UPDATE event SET body = 'tampered' WHERE seq = 1", []).unwrap();
        }
        assert_eq!(gate_verdict(&db, None, None), GateVerdict::Failing);
        assert_eq!(
            gate_verdict(&db, None, Some("some-project-that-does-not-exist")),
            GateVerdict::Failing,
            "a broken chain must not be scoped away by --project"
        );
    }

    /// THE DEFECT THIS PREVENTS: an incomplete --checkouts directory (one
    /// sibling checkout missing, a project renamed or archived on purpose)
    /// failing a commit for a reason that has nothing to do with the commit
    /// itself - exactly the false positive `gate_verdict`'s own doc comment
    /// names as the reason orphan project keys are left out of --gate.
    #[test]
    fn gate_verdict_does_not_fail_when_a_projects_own_checkout_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let checkouts = dir.path().join("dev");
        std::fs::create_dir_all(&checkouts).unwrap();
        {
            let mut s = EventStore::new(&db).unwrap();
            let mut item = rule("ghost-1");
            item.project = Some("Ghost-Project".to_string());
            item.bindings = vec![Binding::Target { kind: TargetKind::Path, value: "missing.txt".to_string() }];
            store::declare(&mut s, "s", "l", "a", &item).unwrap();
        }
        assert_eq!(gate_verdict(&db, Some(&checkouts), None), GateVerdict::Clean);
    }

    // ------------------------------------------------------------- wal_line
    //
    // THE DEFECT THIS CLOSES, measured directly: thor.db sat at 75 MB while
    // thor.db-wal sat at 3.4 GB, after weeks of a repair storm nobody could
    // see - store_line reads the store, never the WAL sidecar beside it.
    // `File::set_len` builds the oversized fixture without actually writing
    // the bytes (a sparse file), so these stay fast.

    fn write_sized(path: &Path, bytes: u64) {
        let file = std::fs::File::create(path).unwrap();
        file.set_len(bytes).unwrap();
    }

    #[test]
    fn a_small_wal_is_silent() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        write_sized(&db, 1024);
        write_sized(&wal_sidecar_path(&db), 1024);
        assert_eq!(wal_line(&db), None, "an ordinary WAL that grows and shrinks by design must never nag");
    }

    #[test]
    fn a_wal_bigger_than_the_store_but_under_the_64mb_floor_is_still_silent() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        write_sized(&db, 100);
        write_sized(&wal_sidecar_path(&db), 10 * 1024 * 1024);
        assert_eq!(
            wal_line(&db),
            None,
            "a fresh small store must never nag, even when its WAL is already bigger than it"
        );
    }

    #[test]
    fn a_wal_past_both_the_store_size_and_the_floor_reports_in_mb() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        write_sized(&db, 5 * 1024 * 1024);
        write_sized(&wal_sidecar_path(&db), 70 * 1024 * 1024);
        let line = wal_line(&db).expect("a WAL bigger than the store and past 64 MB must report");
        assert!(line.starts_with("wal:"), "{line}");
        assert!(line.contains("70.0 MB"), "{line}");
        assert!(line.contains("5.0 MB"), "{line}");
        assert!(line.contains("checkpoint"), "must say what a grown WAL means: {line}");
    }

    #[test]
    fn a_missing_wal_is_silent() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        write_sized(&db, 1024);
        assert_eq!(wal_line(&db), None, "no -wal file at all is not a finding");
    }

    /// THE DEFECT THIS CLOSES, found by a black-box verification: `report`
    /// used to measure the `-wal` file only after `store_line` had already
    /// opened and closed the report's own connection to the store, and on a
    /// single-session store - exactly what a new user has on their very
    /// first `doctor` run - that connection closing is enough for SQLite to
    /// reclaim or discard the `-wal` file. Measured directly: a 500 MB
    /// `-wal` beside a copy of the store produced no wal line at all, and
    /// the file was gone afterwards. This reproduces the same shape through
    /// `report` itself (not `wal_line` directly, which never opened a
    /// connection and so never showed the defect) to prove the fix holds at
    /// the level where the bug actually lived.
    #[test]
    fn report_still_sees_a_grown_wal_after_its_own_connection_has_come_and_gone() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        {
            EventStore::new(&db).unwrap();
        }
        write_sized(&wal_sidecar_path(&db), 500 * 1024 * 1024);
        let lines = report(&db, None, None, None, None, None, false, None);
        assert!(
            lines.iter().any(|l| l.starts_with("wal:")),
            "a 500 MB WAL must still be reported even though report() itself opens and closes a \
             connection to the store before this line is produced: {lines:#?}"
        );
    }

    /// The companion case: pre-measuring the WAL up front must not turn an
    /// ordinary, harmless WAL into a false alarm - the size that gets stashed
    /// before `store_line` runs still has to clear the same floor and ratio
    /// checks as before.
    #[test]
    fn report_stays_silent_about_an_ordinary_small_wal() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        {
            EventStore::new(&db).unwrap();
        }
        write_sized(&wal_sidecar_path(&db), 1024);
        let lines = report(&db, None, None, None, None, None, false, None);
        assert!(!lines.iter().any(|l| l.starts_with("wal:")), "an ordinary small WAL must never nag: {lines:#?}");
    }

    // ------------------------------------------------- dead_moment_bindings_line
    //
    // THE SILENCE THIS SURFACES: `model::gate::declare`'s ground 25 refuses
    // a NEW binding to `Action::Answer`/`Action::ClaimDone` (nothing in
    // `serve` fires either - `intent::from_draft` is their only producer and
    // has no caller outside its own tests), but that gate protects only new
    // writes. A real store can still hold an item written before ground 25
    // existed, and `legacy_declare` (this module's own escape hatch, see its
    // doc comment) is how these fixtures get built without the gate
    // refusing the very shape they exist to reproduce.

    fn dead_moment_only(id: &str) -> Item {
        Item {
            id: id.to_string(),
            kind: Kind::Rule,
            text: "always open a reply with a plain-language summary".to_string(),
            bindings: vec![Binding::Moment(intent::Action::Answer)],
            severity: None,
            project: None,
            tags: vec![],
            expires: None,
            key: None,
            falsifier: Some("a reply opens with nothing of the sort and turns out fine anyway".to_string()),
            check: None,
        }
    }

    #[test]
    fn dead_moment_bindings_line_is_none_on_a_store_with_nothing_dead() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let mut store = EventStore::new(&db).unwrap();
        store::declare(&mut store, "s", "l", "a", &rule("live-one")).unwrap();

        let line = dead_moment_bindings_line(&db);
        assert!(line.contains("none"), "{line}");
    }

    #[test]
    fn dead_moment_bindings_line_names_an_item_bound_only_to_a_dead_moment() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let mut store = EventStore::new(&db).unwrap();
        legacy_declare(&mut store, &dead_moment_only("stuck-on-answer"));

        let line = dead_moment_bindings_line(&db);
        assert!(line.contains("1 live item"), "{line}");
        assert!(line.contains("stuck-on-answer"), "{line}");
        assert!(line.contains("revise"), "must say what to do about it: {line}");
    }

    /// THE "ONLY" IN THE SPEC. A dead moment sitting ALONGSIDE a binding
    /// that really can fire is not this line's business - the item is not
    /// silent, it just has one useless way in among others.
    #[test]
    fn dead_moment_bindings_line_ignores_an_item_with_a_live_binding_alongside_the_dead_one() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let mut store = EventStore::new(&db).unwrap();
        let mut mixed = dead_moment_only("mixed-bindings");
        mixed.bindings = vec![Binding::Moment(intent::Action::Answer), Binding::Always];
        legacy_declare(&mut store, &mixed);

        let line = dead_moment_bindings_line(&db);
        assert!(line.contains("none"), "a mixed binding still has a way to fire: {line}");
    }

    #[test]
    fn dead_moment_bindings_line_caps_names_at_ten_and_says_how_many_more() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let mut store = EventStore::new(&db).unwrap();
        // Zero-padded so id order (lexicographic - `live_items` is id-ordered,
        // never insertion order) is also numeric order: "dead-10" would
        // otherwise sort before "dead-2" and break the cap assertion below.
        for i in 0..12 {
            legacy_declare(&mut store, &dead_moment_only(&format!("dead-{i:02}")));
        }

        let line = dead_moment_bindings_line(&db);
        assert!(line.contains("12 live item"), "{line}");
        for i in 0..10 {
            assert!(line.contains(&format!("dead-{i:02}")), "the whole batch is named: {line}");
        }
        assert!(!line.contains("dead-10"), "and it stops at the cap: {line}");
        assert!(!line.contains("dead-11"), "{line}");
        assert!(line.contains("2 more"), "silence about the rest would read as done: {line}");
    }

    /// The two other ways a stored item never fires, counted instead of hidden
    /// behind "none": no binding at all (an Orientation stored before ground 1
    /// covered it), and a costly rule bound only to a project name.
    #[test]
    fn dead_moment_bindings_line_counts_unbound_and_project_only_items_instead_of_saying_none() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let mut store = EventStore::new(&db).unwrap();
        let mut unbound = rule("orientation-with-no-binding");
        unbound.kind = Kind::Orientation;
        unbound.bindings = vec![];
        legacy_declare(&mut store, &unbound);
        let mut weighted = rule("costly-but-project-only");
        weighted.severity = Some(model::item::Severity::Costly);
        weighted.bindings = vec![Binding::Target { kind: TargetKind::Project, value: "acme".to_string() }];
        legacy_declare(&mut store, &weighted);
        let mut light = rule("light-and-project-only");
        light.bindings = weighted.bindings.clone();
        legacy_declare(&mut store, &light);

        let line = dead_moment_bindings_line(&db);
        assert!(!line.contains("none"), "{line}");
        assert!(line.contains("no binding: 1 live item"), "{line}");
        assert!(line.contains("orientation-with-no-binding"), "{line}");
        assert!(line.contains("project-only: 1 live item"), "{line}");
        assert!(line.contains("costly-but-project-only"), "{line}");
        assert!(!line.contains("light-and-project-only"), "a light note is allowed to fire on the typed word: {line}");
    }

    /// A real token already in a note is counted, named by id and shape and
    /// never by value, with the plain statement that history cannot be cleaned.
    /// Silent when there is none.
    #[test]
    fn secrets_in_notes_line_names_the_note_and_the_shape_and_is_silent_at_zero() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let mut store = EventStore::new(&db).unwrap();
        legacy_declare(&mut store, &rule("clean-note"));
        assert_eq!(secrets_in_notes_line(&db), None);

        let token = format!("{}{}", ["gh", "p_"].concat(), "Qz".repeat(18));
        let mut leaky = rule("leaky-note");
        leaky.text = format!("log in with {token}");
        legacy_declare(&mut store, &leaky);
        let line = secrets_in_notes_line(&db).expect("one note carries a token");
        assert!(line.contains("1 live note"), "{line}");
        assert!(line.contains("leaky-note") && line.contains("GitHub token"), "{line}");
        assert!(line.contains("cannot be cleaned") && line.contains("ROTATE"), "{line}");
        assert!(!line.contains(&token), "{line}");
    }

    // ------------------------------------------------ command_anchors_never_fired_line (step 40)

    /// A rule bound only to the given command anchors (and `Always` too when
    /// `also_always`), written straight to the log.
    fn bound_to_commands(store: &mut EventStore, id: &str, anchors: &[&str], also_always: bool) {
        let mut item = rule(id);
        item.bindings = anchors
            .iter()
            .map(|a| Binding::Target { kind: TargetKind::Command, value: (*a).to_string() })
            .collect();
        if also_always {
            item.bindings.push(Binding::Always);
        }
        legacy_declare(store, &item);
    }

    #[test]
    fn command_anchors_never_fired_line_names_each_class_and_leaves_the_reachable_ones_alone() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let mut store = EventStore::new(&db).unwrap();
        // A real program in the folder doctor treats as THOR's own bin.
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let program = if cfg!(windows) { "fixtureprog.exe" } else { "fixtureprog" };
        let file = bin.join(program);
        std::fs::write(&file, b"x").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        // The dead ones.
        bound_to_commands(&mut store, "dead-bare-tool", &["remember"], false);
        bound_to_commands(&mut store, "dead-prose", &["geankerd feit"], false);
        bound_to_commands(&mut store, "dead-flag", &["--force"], false);
        bound_to_commands(&mut store, "dead-but-pinned", &["shelve"], true);
        // The reachable ones.
        bound_to_commands(&mut store, "ok-whole-tool-name", &["mcp__thor__remember"], false);
        bound_to_commands(&mut store, "ok-tool", &["Agent"], false);
        bound_to_commands(&mut store, "ok-cmdlet", &["Get-Command"], false);
        bound_to_commands(&mut store, "ok-program", &["fixtureprog run"], false);
        bound_to_commands(&mut store, "ok-one-live-anchor", &["remember", "fixtureprog"], false);
        // One that fired before: a real call reached it.
        bound_to_commands(&mut store, "ok-fired", &["sentence that ran"], false);
        bound_to_commands(&mut store, "ok-prompted", &["keyword the owner types"], false);
        serve::deliver::record_delivery_with_trigger(
            &mut store,
            "0de7dc52-1b97-4b66-948f-61e4438043b2",
            "0de7dc52-1b97-4b66-948f-61e4438043b2",
            "hook",
            "2026-09-28T00:00:00Z",
            &["ok-prompted".to_string()],
            Some("(prompt) keyword the owner types"),
        );
        serve::deliver::record_delivery_with_trigger(
            &mut store,
            "0de7dc52-1b97-4b66-948f-61e4438043b2",
            "0de7dc52-1b97-4b66-948f-61e4438043b2",
            "hook",
            "2026-09-28T00:00:00Z",
            &["ok-fired".to_string()],
            Some("sentence that ran today"),
        );

        let line = command_anchors_never_fired_line(&db, true);
        assert!(line.starts_with("command anchors never fired: 4 of 11 live item(s)"), "{line}");
        assert!(line.contains("3 of these have no other binding"), "the pinned one still fires: {line}");
        for id in ["dead-bare-tool", "dead-prose", "dead-flag", "dead-but-pinned"] {
            assert!(line.contains(&format!("command anchor never fired: {id}")), "{id} is named: {line}");
        }
        for id in ["ok-whole-tool-name", "ok-tool", "ok-cmdlet", "ok-program", "ok-one-live-anchor", "ok-fired", "ok-prompted"] {
            assert!(!line.contains(&format!("anchor: {id}")), "{id} can be reached: {line}");
        }
        assert!(line.contains("bare tool name") && line.contains("mcp__thor__remember"), "{line}");
        assert!(line.contains("never reached, not a program here"), "{line}");
        assert!(line.contains("starts with a flag"), "{line}");
        assert!(line.contains("(the item has another binding)"), "{line}");
    }

    #[test]
    fn command_anchors_never_fired_line_is_none_when_every_anchor_can_fire_and_caps_its_names() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let mut store = EventStore::new(&db).unwrap();
        bound_to_commands(&mut store, "fine", &["Agent"], false);
        let line = command_anchors_never_fired_line(&db, false);
        assert!(line.starts_with("command anchors never fired: none"), "{line}");

        for i in 0..22 {
            bound_to_commands(&mut store, &format!("dead-{i:02}"), &[&format!("nonsense word {i}")], false);
        }
        let capped = command_anchors_never_fired_line(&db, false);
        assert!(capped.contains("22 of 23 live item(s)"), "{capped}");
        assert_eq!(capped.matches("command anchor never fired: dead-").count(), 20, "{capped}");
        assert!(capped.contains("and 2 more, not named here (use --full)"), "{capped}");
        let all = command_anchors_never_fired_line(&db, true);
        assert_eq!(all.matches("command anchor never fired: dead-").count(), 22, "{all}");
    }

    // ------------------------------------------ the evaluations (step 16)

    /// One VALID evaluation of project `p` by `DOCTOR_EVALUATOR`: `useful`
    /// items judged useful, `noise` judged noise and `skipped` skipped with a
    /// reason, all on its frozen judgement list - each verdict given through
    /// THOR's mark tool and verified the way a Stop verifies it - then the
    /// report filed and validated. Returns the validator's record.
    fn evaluation_with_verdicts(store: &mut EventStore, p: &str, useful: usize, noise: usize, skipped: usize) -> model::store::EvaluationValidation {
        let transcripts = tempfile::tempdir().unwrap();
        let session = DOCTOR_EVALUATOR;
        let ids: Vec<String> = (0..useful + noise + skipped).map(|i| format!("judged-{p}-{i:02}")).collect();
        for id in &ids {
            let mut item = rule(id);
            item.text = format!("fixture judged rule {id}");
            item.project = Some(p.to_string());
            item.bindings = vec![Binding::Target { kind: TargetKind::Command, value: format!("fixture-judged-{id}") }];
            store::declare(store, "s", "l", "a", &item).unwrap();
            for i in 0..serve::usefulness::JUDGEMENT_DEBT_AFTER {
                serve::deliver::record_delivery(store, &serve::deliver::nth_session(session, i), session, serve::usefulness::ASSISTANT_DELIVERY_ACTOR, "2026-09-08T00:00:00Z", &[id.clone()]);
            }
        }
        let start = serve::attest::fixture::quiet_start(store, session, p);
        assert_eq!(start.frozen.judgement.len(), ids.len(), "fixture sanity: every item is on the frozen list");
        for (i, id) in ids.iter().enumerate().take(useful + noise) {
            serve::attest::fixture::attested_mark(store, transcripts.path(), session, id, i >= useful, &format!("toolu_mark_{i}"));
        }
        let mut report = report_item(&format!("eval-{p}-2026-09-30"), p);
        report.tags.extend(ids[useful + noise..].iter().map(|id| format!("not-judgeable:{id}:the fixture never saw where it fired")));
        serve::eval_filing::fixture::valid_filing(store, transcripts.path(), session, &report, "toolu_filing")
    }

    /// G16.4 (B7): 7 useful, 2 noise, 1 skipped - doctor names the newest
    /// evaluation that counts and its share, "78% useful", and no flag.
    #[test]
    fn evaluations_line_shows_the_share() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let record = evaluation_with_verdicts(&mut EventStore::new(&db).unwrap(), "fxp", 7, 2, 1);
        assert_eq!((record.verdicts.useful, record.verdicts.noise, record.verdicts.skipped), (7, 2, 1), "{record:?}");
        let line = evaluations_line(&db, Some("fxp")).expect("a valid evaluation, the line must speak");
        assert!(
            line.starts_with(&format!(
                "evaluations: newest evaluation report 'eval-fxp-2026-09-30' (valid, filed by session {DOCTOR_EVALUATOR})"
            )),
            "{line}"
        );
        assert!(line.contains("  eval-fxp-2026-09-30: 7 useful, 2 noise, 1 skipped (78% useful)"), "{line}");
        assert!(!line.contains("every verdict went one way"), "{line}");
        assert_eq!(evaluations_line(&db, Some("other")), None, "silent for a project with no report at all");
        assert_eq!(evaluations_line(&db, None), None, "silent outside a project");
    }

    /// R16.6 (B7): 12 items on the list, every one judged useful - doctor
    /// flags it: a blind pass looks exactly like this.
    #[test]
    fn one_way_flag_at_ten_verdicts() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        evaluation_with_verdicts(&mut EventStore::new(&db).unwrap(), "fxp", 12, 0, 0);
        let line = evaluations_line(&db, Some("fxp")).unwrap();
        assert!(
            line.contains(
                "eval-fxp-2026-09-30: 12 useful, 0 noise, 0 skipped (100% useful) - every verdict went one way - a blind pass \
                 looks exactly like this; nothing here can tell whether the items were read"
            ),
            "{line}"
        );

        // Nine the same way is below the flag's floor.
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        evaluation_with_verdicts(&mut EventStore::new(&db).unwrap(), "fxp", 0, 9, 0);
        let line = evaluations_line(&db, Some("fxp")).unwrap();
        assert!(line.contains("0 useful, 9 noise, 0 skipped (0% useful)") && !line.contains("went one way"), "{line}");
    }

    /// B5, B6: THOR's own open points of a project a valid evaluation was
    /// filed past are the owner's; doctor counts them against the budget of
    /// 8, names each with who carried it, and over the budget opens with the
    /// same banner as the owner's session start. The points and the
    /// validator's record are written raw here - what doctor reads, not how
    /// the machine pass makes them (`serve/tests/owner_queue_banner.rs` and
    /// `serve::eval_filing`'s units prove that).
    /// Step 21: THOR's one store-wide point (the rules with no project) is
    /// carried by every project's valid evaluations together and counted ONCE
    /// in the owner queue - never against a project's budget: eight points of
    /// fxp's own plus that one open no banner.
    #[test]
    fn owner_queue_line_counts_the_store_wide_point_once() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.db");
        let mut store = EventStore::new(&db).unwrap();
        let point = |id: String, project: Option<&str>, blocker: &store::Blocker| Item {
            id,
            kind: Kind::Report,
            text: "THOR's own open point".to_string(),
            bindings: vec![],
            severity: None,
            project: project.map(str::to_string),
            tags: store::machine_point_tags(blocker),
            expires: None,
            key: None,
            falsifier: None,
            check: None,
        };
        let entry = |subject: &str, session: &str| store::CarryEntry {
            subject: subject.to_string(),
            carried: 0,
            session: session.to_string(),
            agent: None,
        };
        let mut fxp_carry = Vec::new();
        for i in 0..store::OPEN_POINTS_OWNER_BUDGET {
            let subject = format!("notes-{i}.md");
            let blocker = store::Blocker::WorkingTreeDiffers { path: subject.clone() };
            let body = store::canonical_body(&point(store::machine_point_id("fxp", &blocker), Some("fxp"), &blocker)).unwrap();
            store.append_event("s", "s", "eval-machine", EventKind::FactCreated, &store::machine_point_id("fxp", &blocker), None, &body).unwrap();
            fxp_carry.push(entry(&subject, DOCTOR_EVALUATOR));
        }
        let global = store::Blocker::global_rules();
        let id = store::machine_point_id("fxp", &global);
        assert_eq!(id, store::GLOBAL_RULES_POINT_ID, "one id whatever project is evaluated");
        let body = store::canonical_body(&point(id.clone(), None, &global)).unwrap();
        store.append_event("s", "s", "eval-machine", EventKind::FactCreated, &id, None, &body).unwrap();
        fxp_carry.push(entry(store::GLOBAL_RULES_SUBJECT, DOCTOR_EVALUATOR));
        let other = "7a1e0c3a-0000-4000-8000-0000000000b2";
        for (report, project, session, carry, at) in [
            ("eval-fxp-2026-09-29", "fxp", DOCTOR_EVALUATOR, fxp_carry, "2026-09-29T10:00:00Z"),
            ("eval-beta-2026-09-30", "beta", other, vec![entry(store::GLOBAL_RULES_SUBJECT, other)], "2026-09-30T10:00:00Z"),
        ] {
            let validation = store::EvaluationValidation {
                valid: true,
                why: String::new(),
                drift: false,
                report: report.to_string(),
                report_seq: 1,
                project: Some(project.to_string()),
                session: session.to_string(),
                agent: None,
                started_seq: Some(1),
                filed_seq: Some(2),
                filed_at: Some(at.to_string()),
                machine_sha256: None,
                verdicts: store::VerdictShares::default(),
                carry,
            };
            let body = serde_json::to_string(&validation).unwrap();
            store.append_event("s", "s", "eval-validator", EventKind::GateStoodAside, report, None, &body).unwrap();
        }
        drop(store);
        let line = owner_queue_line(&db).expect("points wait on the owner");
        assert!(!line.contains("[THOR] 9 open points"), "the store-wide point is in no project's budget: {line}");
        assert!(line.starts_with("owner queue: 8 of THOR's open points in 'fxp' wait on the owner (budget 8)"), "{line}");
        assert!(
            line.contains(&format!(
                "owner queue, the whole memory: {} - one point for every project's rules with no project",
                store::GLOBAL_RULES_POINT_ID
            )) && line.contains(&format!("carried by session {DOCTOR_EVALUATOR} on 2026-09-29, {other} on 2026-09-30")),
            "counted once, carried by both projects' evaluations: {line}"
        );
        assert_eq!(line.matches(store::GLOBAL_RULES_POINT_ID).count(), 1, "{line}");
    }

    #[test]
    fn owner_queue_line_counts_against_the_budget() {
        let queue = |n: usize| {
            let dir = tempfile::tempdir().unwrap();
            let db = dir.path().join("t.db");
            let mut store = EventStore::new(&db).unwrap();
            let mut carry = Vec::new();
            for i in 0..n {
                let subject = format!("notes-{i}.md");
                let blocker = store::Blocker::WorkingTreeDiffers { path: subject.clone() };
                let point = Item {
                    id: store::machine_point_id("fxp", &blocker),
                    kind: Kind::Report,
                    text: format!("THOR's own open point about {subject}"),
                    bindings: vec![],
                    severity: None,
                    project: Some("fxp".to_string()),
                    tags: store::machine_point_tags(&blocker),
                    expires: None,
                    key: None,
                    falsifier: None,
                    check: None,
                };
                let body = store::canonical_body(&point).unwrap();
                store.append_event("s", "s", "eval-machine", EventKind::FactCreated, &point.id, None, &body).unwrap();
                carry.push(store::CarryEntry { subject, carried: 0, session: DOCTOR_EVALUATOR.to_string(), agent: None });
            }
            let validation = store::EvaluationValidation {
                valid: true,
                why: String::new(),
                drift: false,
                report: "eval-fxp-2026-09-29".to_string(),
                report_seq: 1,
                project: Some("fxp".to_string()),
                session: DOCTOR_EVALUATOR.to_string(),
                agent: None,
                started_seq: Some(1),
                filed_seq: Some(2),
                filed_at: Some("2026-09-29T10:00:00Z".to_string()),
                machine_sha256: None,
                verdicts: store::VerdictShares::default(),
                carry,
            };
            let body = serde_json::to_string(&validation).unwrap();
            store.append_event("s", "s", "eval-validator", EventKind::GateStoodAside, "eval-fxp-2026-09-29", None, &body).unwrap();
            drop(store);
            (dir, owner_queue_line(&db))
        };
        let (_dir, none) = queue(0);
        assert_eq!(none, None, "silent when nothing waits on the owner");

        let (_dir, within) = queue(3);
        let within = within.expect("three points wait on the owner");
        assert!(
            within.starts_with("owner queue: 3 of THOR's open points in 'fxp' wait on the owner (budget 8)"),
            "within the budget: no banner: {within}"
        );
        assert!(
            within.contains(&format!("  open-working-tree-differs-fxp-notes-0-md - carried by session {DOCTOR_EVALUATOR} on 2026-09-29")),
            "{within}"
        );

        let (_dir, over) = queue(9);
        let over = over.expect("nine points wait on the owner");
        assert!(
            over.starts_with(
                "[THOR] 9 open points wait on YOU in fxp (budget 8) - each one says what to do, step by step\nowner queue: 9 of \
                 THOR's open points in 'fxp' wait on the owner (budget 8)"
            ),
            "{over}"
        );
    }
}

/// The two health lines about THOR failing to run at all: the installed programs
/// built from different sources, and hooks Claude Code could not start.
#[cfg(test)]
mod launch_and_skew_tests {
    use super::*;
    use serve::hook_faults::Scanned;

    fn version(program: &str, id: &str) -> Option<String> {
        Some(format!("{program} 2.5.0 (build {id})\n"))
    }

    #[test]
    fn programs_of_one_build_are_not_a_finding() {
        let same = [("serve.exe", version("serve", "aaaaaaaaaaaa")), ("mcp.exe", version("mcp", "aaaaaaaaaaaa"))];
        assert_eq!(version_skew_words(&same), None);
    }

    #[test]
    fn programs_of_two_builds_are_named_with_both_ids_and_the_way_on() {
        let skewed = [
            ("serve.exe", version("serve", "aaaaaaaaaaaa")),
            ("mcp.exe", version("mcp", "bbbbbbbbbbbb")),
            ("doctor.exe", version("doctor", "aaaaaaaaaaaa")),
        ];
        let line = version_skew_words(&skewed).expect("two builds");
        assert!(line.contains("serve.exe is build aaaaaaaaaaaa") && line.contains("mcp.exe is build bbbbbbbbbbbb"), "{line}");
        assert!(line.contains("come from different builds") && line.contains("Evaluation filings still count"), "{line}");
        assert!(line.contains("only when a \"tool server:\" line is also printed"), "{line}");
        assert!(!line.contains("refused") && !line.contains("run install.exe again"), "no order to reinstall or restart: {line}");
        assert!(line.is_ascii() && !line.contains('\n'), "one plain line: {line}");
    }

    /// Field test 15, C2 (collect 18): the line names the machine-section
    /// renderer of each program and says plainly whether filings count. Two
    /// builds that render alike are a serve-only install, not a dead report.
    #[test]
    fn the_line_names_each_renderer_and_says_whether_filings_count() {
        let line_of = |serve_renderer: u32, mcp_renderer: u32| {
            let versioned = |program: &str, id: &str, renderer: u32| Some(format!("{program} 2.5.0 (build {id}, renderer {renderer})\n"));
            version_skew_words(&[
                ("serve.exe", versioned("serve", "aaaaaaaaaaaa", serve_renderer)),
                ("mcp.exe", versioned("mcp", "bbbbbbbbbbbb", mcp_renderer)),
            ])
            .expect("two builds")
        };
        let alike = line_of(7, 7);
        assert!(alike.contains("serve.exe is build aaaaaaaaaaaa (renderer 7)") && alike.contains("mcp.exe is build bbbbbbbbbbbb (renderer 7)"), "{alike}");
        assert!(alike.contains("Renderers match: evaluation filings count") && !alike.contains("restart Claude"), "{alike}");
        let apart = line_of(7, 6);
        assert!(apart.contains("(renderer 6)") && apart.contains("Renderers differ") && apart.contains("restart Claude"), "{apart}");
        assert!(!apart.contains("Renderers match"), "{apart}");
    }

    #[test]
    fn a_program_that_prints_no_build_id_is_older_than_the_others() {
        let mixed = [("serve.exe", version("serve", "aaaaaaaaaaaa")), ("mcp.exe", Some("mcp 2.5.0\n".to_string()))];
        let line = version_skew_words(&mixed).expect("one program is older");
        assert!(line.contains("mcp.exe prints no build id") && line.contains("older"), "{line}");
        let both_old = [("serve.exe", Some("serve 2.5.0\n".to_string())), ("mcp.exe", Some("mcp 2.5.0\n".to_string()))];
        assert_eq!(version_skew_words(&both_old), None, "nothing to compare");
    }

    #[test]
    fn a_program_that_could_not_be_run_or_is_missing_is_left_to_the_install_check() {
        let missing = [("serve.exe", version("serve", "aaaaaaaaaaaa")), ("mcp.exe", None)];
        assert_eq!(version_skew_words(&missing), None);
    }

    fn fault(at: &str, event: &str, exit: i64, cause: &'static str) -> Scanned {
        Scanned {
            session: "3f2ac470-767e-4df0-9ef3-2b89ed671524".to_string(),
            fault: model::store::HookFault {
                tool_use_id: format!("toolu-{at}"),
                hook_event: event.to_string(),
                exit_code: exit,
                at: at.to_string(),
                host_wide: false,
            },
            cause,
        }
    }

    #[test]
    fn no_failures_and_no_blocks_is_silent() {
        assert_eq!(hook_failures_words(&[], &[]), None);
    }

    #[test]
    fn hook_failures_give_one_line_with_the_count_the_last_time_and_the_cause() {
        let faults = [
            fault("2026-10-05T08:00:00.000Z", "PreToolUse", 66, "the hook could not run"),
            fault("2026-10-06T22:29:00.231Z", "SessionStart", 126, "the program was blocked or could not be opened (x)"),
        ];
        let line = hook_failures_words(&faults, &[]).expect("two failures");
        assert!(!line.contains('\n'), "one plain line: {line}");
        assert!(line.starts_with("hook launch failures: 2 in the last 7 days, the last at 2026-10-06T22:29:00.231Z"), "{line}");
        assert!(line.contains("SessionStart, exit 126: the program was blocked"), "{line}");
        assert!(line.contains("never reached the session") && line.contains("restart Claude"), "{line}");
    }

    #[test]
    fn host_wide_failures_are_said_apart_and_are_not_thors() {
        let mut sleeping = fault("2026-10-06T04:18:12.100Z", "PreToolUse", 66, "the hook could not run");
        sleeping.fault.host_wide = true;
        let only = hook_failures_words(std::slice::from_ref(&sleeping), &[]).expect("said apart");
        assert!(only.starts_with("1 hook(s) failed while the machine was going to sleep or shutting down - not THOR"), "{only}");
        assert!(!only.contains("hook launch failures") && !only.contains("restart Claude"), "no THOR alarm: {only}");
        assert!(only.contains("Nothing to do"), "{only}");

        let mixed = hook_failures_words(&[sleeping, fault("2026-10-06T22:29:00.231Z", "Stop", 126, "blocked")], &[]).unwrap();
        assert!(mixed.starts_with("hook launch failures: 1 in the last 7 days"), "only the real one is counted: {mixed}");
        assert!(mixed.contains("1 hook(s) failed while the machine was going to sleep"), "{mixed}");
    }
    #[test]
    fn windows_code_integrity_blocks_are_named_with_their_count_and_last_time() {
        let blocks = [1_791_000_000, 1_791_100_000];
        let line = hook_failures_words(&[], &blocks).expect("two blocks");
        assert!(line.contains("Windows Code Integrity blocked a THOR program 2 time(s)"), "{line}");
        assert!(line.contains(&serve::time::iso8601_from_unix(1_791_100_000)), "{line}");
        let both = hook_failures_words(&[fault("2026-10-06T22:29:00.231Z", "Stop", 126, "blocked")], &blocks).unwrap();
        assert!(both.contains("1 in the last 7 days") && both.contains("Code Integrity blocked a THOR program 2 time(s)"), "{both}");
    }

    #[test]
    fn a_code_integrity_message_counts_only_when_it_names_a_thor_program() {
        let ours = "Code Integrity determined that a process (\\Device\\HarddiskVolume3\\Users\\someone\\thor2\\bin\\serve.exe) attempted to load x.dll";
        let other = "Code Integrity determined that a process (\\Device\\HarddiskVolume3\\Users\\someone\\.rustup\\bin\\rustc.exe) attempted to load y.dll";
        assert!(names_a_thor_program(ours));
        assert!(names_a_thor_program("a process (\\Device\\HarddiskVolume3\\tools\\mcp.exe) was blocked"));
        assert!(!names_a_thor_program(other));
        let text = format!("1791100000|{ours}\n1791000000|{other}\nnot a line\n1700000000|{ours}\n");
        assert_eq!(parse_code_integrity_events(&text, 1_790_000_000), vec![1_791_100_000], "only a THOR program, only inside the span");
    }

    #[test]
    fn the_line_reads_the_transcripts_of_the_last_week_only() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("C--Users-someone-dev-demo");
        std::fs::create_dir_all(&project).unwrap();
        let record = serde_json::json!({
            "attachment": {
                "type": "hook_non_blocking_error",
                "toolUseID": "toolu-1",
                "hookEvent": "SessionStart",
                "stderr": "serve.exe: Permission denied",
                "exitCode": 126,
                "command": "\"C:\\thor2\\bin\\serve.exe\" --db \"C:\\thor2\\thor.db\" hook"
            },
            "timestamp": serve::time::iso8601_from_unix(serve::time::now_unix() - 3600),
            "sessionId": "3f2ac470-767e-4df0-9ef3-2b89ed671524",
        });
        std::fs::write(project.join("3f2ac470-767e-4df0-9ef3-2b89ed671524.jsonl"), record.to_string() + "\n").unwrap();
        let now = serve::time::now_unix();
        let line = hook_failures_line_in(Some(dir.path()), &[], now).expect("the failure is found");
        assert!(line.contains("1 in the last 7 days"), "{line}");
        let far_future = now + 30 * 86_400;
        assert_eq!(hook_failures_line_in(Some(dir.path()), &[], far_future), None, "a month later the file is older than the span");
        assert_eq!(hook_failures_line_in(None, &[], now), None, "no transcripts folder, nothing to read");
    }
}

#[cfg(test)]
mod plain_summary_tests {
    use super::*;

    /// A memory file, a settings file that names it in a hook, and a tool-server
    /// file that registers the note-saving program for it.
    fn wired(dir: &Path) -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
        let db = dir.join("thor.db");
        crate::install::ensure_store(&db).unwrap();
        let settings = dir.join("settings.json");
        let command = format!("\"C:\\thor2\\serve.exe\" --db \"{}\" hook", db.display());
        std::fs::write(
            &settings,
            serde_json::json!({ "hooks": { "SessionStart": [ { "hooks": [ { "type": "command", "command": command } ] } ] } })
                .to_string(),
        )
        .unwrap();
        let mcp = dir.join("claude.json");
        std::fs::write(
            &mcp,
            serde_json::json!({ "mcpServers": { "thor": { "type": "stdio", "command": "mcp", "args": ["--db", db.display().to_string()] } } })
                .to_string(),
        )
        .unwrap();
        (db, settings, mcp)
    }

    #[test]
    fn a_healthy_setup_reads_ok_on_every_line_and_names_no_internal_term() {
        let dir = tempfile::tempdir().unwrap();
        let (db, settings, mcp) = wired(dir.path());
        let topic = dir.path().join("Kitchen renovation");
        std::fs::create_dir_all(&topic).unwrap();
        std::fs::write(topic.join(".thor-project"), "Kitchen renovation\n").unwrap();
        let mut store = EventStore::open_existing(&db).unwrap();
        crate::install::register_root(&mut store, &topic, "Kitchen renovation", None).unwrap();
        drop(store);

        let lines = plain_summary(&db, &topic, Some(&settings), Some(&mcp));
        let all = lines.join("\n");
        assert!(all.contains("OK: your memory file is there"), "{all}");
        assert!(all.contains("OK: THOR is connected to Claude Code."), "{all}");
        assert!(all.contains("OK: your assistant can save notes."), "{all}");
        assert!(all.contains("OK: this folder has its own memory, under the name 'Kitchen renovation'."), "{all}");
        assert!(!all.contains("PROBLEM"), "{all}");
        for jargon in ["teeth", "falsifier", "crowding", "scope", "checkout", "hook", "mcp", "MCP"] {
            assert!(!all.contains(jargon), "the short version must not use {jargon:?}: {all}");
        }
    }

    #[test]
    fn every_problem_names_one_next_step() {
        let dir = tempfile::tempdir().unwrap();
        let (db, _, _) = wired(dir.path());
        let lines = plain_summary(&db, dir.path(), Some(&dir.path().join("missing.json")), None);
        let all = lines.join("\n");
        assert!(all.contains("PROBLEM: Claude Code does not know about THOR yet"), "{all}");
        assert!(all.contains("PROBLEM: your assistant can read the memory but cannot save notes"), "{all}");
        assert_eq!(all.matches("Next step:").count(), 2, "{all}");
        assert!(all.contains("NOTE: this folder has no memory of its own"), "{all}");
        assert!(all.contains("--topic"), "the plain folder is offered the one command: {all}");

        let gone = plain_summary(&dir.path().join("nowhere.db"), dir.path(), None, None).join("\n");
        assert!(gone.contains("PROBLEM: your memory file cannot be used"), "{gone}");
    }
}
