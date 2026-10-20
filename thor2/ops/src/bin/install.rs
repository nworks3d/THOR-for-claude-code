//! CLI for `ops::install`: the whole setup in one command.
//!
//! It creates the store if there is not one yet, wires THOR's four hooks into
//! an agent's settings.json, registers the tool server the agent writes
//! through, and optionally gives the current folder its own memory scope.
//!
//! WHY ONE COMMAND AND NOT FOUR. Every step left to a person is a step that
//! gets skipped, and each of these fails QUIETLY when it is missing: no hooks
//! means the memory never speaks, no tool server means the agent can never
//! write anything down, no project marker means everything piles into the
//! shared memory every project sees. None of those announce themselves. The
//! only honest fix is to do them together and say plainly what happened.
//!
//! `--settings` and `--mcp-json` are paths this command WRITES to, so neither
//! is ever guessed: name them, or they are not touched. Everything else has a
//! default that can be worked out from where this binary is and where the
//! platform keeps per-user data.

use clap::Parser;
use std::path::Path;
use ops::githooks;
use ops::install::{
    check_registration, check_registration_at, default_data_dir, default_eval_command_path, default_settings_path,
    default_user_mcp_path, ensure_store, install_hooks, install_tool_server, record_project_opened,
    record_topic_opened, register_command_for, register_root_for, register_root_remote, seed_eval_command, seed_response_rulebook,
    seed_working_contract_at, standard_hooks, topic_folder_name, write_project_marker, EvalCommandOutcome, HookOutcome,
    MarkerOutcome, RegisterOutcome, RegisterRefusal, RulebookOutcome, ServerOutcome, StoreOutcome,
};
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Parser)]
#[command(
    name = "install",
    version = env!("CARGO_PKG_VERSION"),
    about = "Set THOR up: create the store, wire the hooks, register the tool server"
)]
struct Cli {
    /// The settings.json to install the hooks into. Backed up to <path>.bak
    /// first. Defaults to Claude Code's own per-user file
    /// (~/.claude/settings.json) - name one only to write somewhere else.
    #[arg(long)]
    settings: Option<PathBuf>,

    /// The file to register the tool server in. Backed up the same way.
    /// Defaults to Claude Code's per-user MCP config (~/.claude.json), so the
    /// agent can write its memory in every project. Name one to write
    /// elsewhere (a project's own .mcp.json, say).
    #[arg(long = "mcp-json")]
    mcp_json: Option<PathBuf>,

    /// Install the hooks WITHOUT registering the tool server. The agent will
    /// then be able to READ its memory but never write to it - only for the
    /// rare case where that is what you actually want.
    #[arg(long = "no-mcp")]
    no_mcp: bool,

    /// The `serve` binary the hooks will call. Defaults to the one next to
    /// this installer.
    #[arg(long = "serve-exe")]
    serve_exe: Option<PathBuf>,

    /// The `mcp` binary the agent writes through. Defaults to the one next to
    /// this installer.
    #[arg(long = "mcp-exe")]
    mcp_exe: Option<PathBuf>,

    /// Where the memory lives. Defaults to the per-user data directory, never
    /// next to the binaries: a store inside a build output directory is one
    /// `cargo clean` away from being gone.
    #[arg(long)]
    db: Option<PathBuf>,

    /// A directory of code indexes, one per project. Defaults to `codeindex`
    /// beside the store.
    #[arg(long = "code-index-root")]
    code_index_root: Option<PathBuf>,

    /// Give the current folder its own memory scope under this key. Without
    /// one, everything stored while working here lands in the shared memory
    /// that every project sees.
    #[arg(long)]
    project: Option<String>,

    /// Give THIS plain folder - one subject, with its notes and files in it,
    /// no code and no git needed - a memory of its own, named after the
    /// folder. Run it from inside that folder. It adds one small hidden file,
    /// `.thor-project`, and touches nothing else in the folder.
    #[arg(long, conflicts_with = "project")]
    topic: bool,

    /// Register ONLY: record the folder this is run from as its project's
    /// working copy, and nothing else - no settings, no hooks, no tool server,
    /// no code index. Run it from inside the checkout (or, for a plain topic
    /// folder, inside the folder: it is marked as a topic first). Needs --db,
    /// so it registers into the memory your assistant really uses. The project
    /// is the one the folder already resolves to (its .thor-project marker,
    /// else its folder name).
    #[arg(
        long = "register-root",
        conflicts_with_all = ["project", "topic", "settings", "mcp_json", "no_mcp", "serve_exe", "mcp_exe", "code_index_root"]
    )]
    register_root: bool,

    /// With --register-root, for a project checked out on TWO machines at
    /// different paths (opt-in; leave it off on a one-machine setup): register
    /// this checkout for this machine only. A second checkout on this same
    /// machine is still refused. Add --ask-main on a machine that keeps a
    /// copy of the memory (a laptop): nothing is written, and the one command
    /// to run on the main machine is printed instead.
    #[arg(long = "per-machine", requires = "register_root")]
    per_machine: bool,

    /// With --per-machine: write nothing here and print the command for the
    /// main machine (the memory has one writer, so only the main machine can
    /// record a registration).
    #[arg(long = "ask-main", requires = "per_machine")]
    ask_main: bool,

    /// Main machine only: register the working copy of --project at --root for
    /// the machine named here (the id `--per-machine --ask-main` printed). The
    /// folder is on the other machine, so it is not looked at.
    #[arg(
        long = "register-machine-root",
        value_name = "MACHINE_ID",
        requires_all = ["project", "root_path"],
        conflicts_with_all = ["register_root", "topic"]
    )]
    register_machine_root: Option<String>,

    /// The checkout's absolute path on the other machine (see
    /// --register-machine-root).
    #[arg(long = "root", id = "root_path", value_name = "PATH")]
    root_path: Option<PathBuf>,

    /// Set ONLY the mode of the bug nudge, `release`, `test` or `off` (nothing is
    /// then offered or sent, ever, and a later install keeps it off), and nothing
    /// else. THOR finds some of its own bugs and offers one per session to
    /// the agent, to be written up for the public THOR repository. In
    /// `release` the agent asks you for a go and then opens the issue; in
    /// `test` (a development setup) it only shows the write-up to you and
    /// pushes nothing. A normal install picks `release` on every build,
    /// source builds included, and never changes a mode already set; only
    /// this flag makes `test`. Needs --db.
    #[arg(
        long = "set-mode",
        value_name = "MODE",
        conflicts_with_all = ["project", "settings", "mcp_json", "no_mcp", "serve_exe", "mcp_exe", "code_index_root", "register_root"]
    )]
    set_mode: Option<String>,

    /// Set ONLY the plan THOR works by, `max` or `small` (Pro and anything
    /// smaller), and nothing else. THOR reads your plan from Claude Code's own
    /// settings file at install; this records YOUR answer when it is wrong or
    /// when the file does not say. On `max` the weekly check runs daily while
    /// something is found wrong; on `small` it runs strictly once a week, so
    /// your limit stays for your own work. Needs --db.
    #[arg(
        long = "set-plan",
        value_name = "PLAN",
        conflicts_with_all = ["project", "settings", "mcp_json", "no_mcp", "serve_exe", "mcp_exe", "code_index_root", "register_root", "set_mode", "set_weekly_check"]
    )]
    set_plan: Option<String>,

    /// Set ONLY whether THOR's weekly check asks by itself, `on` or `off`, and
    /// nothing else. `off` stops the automatic ask at the end of a turn; you can
    /// still type /thor-eval yourself. Needs --db.
    #[arg(
        long = "set-weekly-check",
        value_name = "on|off",
        conflicts_with_all = ["project", "settings", "mcp_json", "no_mcp", "serve_exe", "mcp_exe", "code_index_root", "register_root", "set_mode", "set_plan"]
    )]
    set_weekly_check: Option<String>,

    /// Set ONLY the backup of the memory, and nothing else: `local` (a git
    /// folder beside the memory, nothing leaves this machine), a place you name
    /// (a folder on a NAS or another computer, or a private repository address),
    /// or `off` (removes just the hook that runs the backup; copies already made
    /// stay). A fresh install wires NO backup: nothing is backed up until you
    /// choose here. It runs the backup once to prove it works. Needs --db.
    #[arg(
        long = "set-backup",
        value_name = "local|PLACE|off",
        conflicts_with_all = ["project", "mcp_json", "no_mcp", "serve_exe", "mcp_exe", "code_index_root", "register_root", "set_mode", "set_plan", "set_weekly_check", "set_recording", "topic"]
    )]
    set_backup: Option<String>,

    /// Set ONLY whether THOR keeps the record of real tool calls and replies
    /// (`on` or `off`), and nothing else. The record stays on this machine,
    /// redacted, and lets THOR find rules that would wrongly block you. `off`
    /// stops all new recording and the sweep then says it has no data. Needs --db.
    #[arg(
        long = "set-recording",
        value_name = "on|off",
        conflicts_with_all = ["project", "settings", "mcp_json", "no_mcp", "serve_exe", "mcp_exe", "code_index_root", "register_root", "set_mode", "set_plan", "set_weekly_check", "set_backup", "topic"]
    )]
    set_recording: Option<String>,

    /// Set ONLY helper-agent mode, `on` or `off`, and nothing else. It is OFF in
    /// a new memory. `on` stores the two notes that carry it (your assistant
    /// then hands work to cheaper helper agents and keeps the chat free); `off`
    /// retracts them again. Running it twice changes nothing the second time.
    /// Needs --db.
    #[arg(
        long = "set-helpers",
        value_name = "on|off",
        conflicts_with_all = ["project", "settings", "mcp_json", "no_mcp", "serve_exe", "mcp_exe", "code_index_root", "register_root", "set_mode", "set_plan", "set_weekly_check", "set_backup", "set_recording", "topic"]
    )]
    set_helpers: Option<String>,

    /// SECOND MACHINE: set THOR up as a read-only COPY of another machine's
    /// memory, built from that machine's backup, in this one command. Give the
    /// backup as a folder (a share on a NAS), a bare repository or a git
    /// address. Connects the hooks, makes the copy, writes no tool server (a
    /// note written on a copy would fork the memory). The same value can be
    /// given as the environment variable THOR_COPY_FROM, which is how the
    /// one-line download command passes it on.
    #[arg(
        long = "copy-from",
        value_name = "BACKUP",
        conflicts_with_all = ["project", "mcp_json", "no_mcp", "mcp_exe", "register_root", "register_machine_root", "topic", "set_mode", "set_plan", "set_weekly_check", "set_backup", "set_recording", "set_helpers"]
    )]
    copy_from: Option<String>,

    /// With --copy-from: hold only this project (repeat the flag, or give
    /// THOR_COPY_SCOPES with commas). Without any, the project of the folder
    /// you run this in. Everything else stays out of the copy file.
    #[arg(long = "scope", value_name = "PROJECT")]
    scopes: Vec<String>,

    /// With --copy-from: also hold the general work rules (the items that
    /// belong to no project). Also read from THOR_COPY_GLOBAL=1.
    #[arg(long = "include-global")]
    include_global: bool,

    /// With --copy-from: hold the whole memory, not a selection (the personal
    /// library included: use it only on a machine you trust). Also read from
    /// THOR_COPY_WHOLE=1. It wins over --scope and --include-global.
    #[arg(long = "whole-memory")]
    whole_memory: bool,
}

/// A value from the environment, ignoring an empty one.
fn env_value(name: &str) -> Option<String> {
    std::env::var(name).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

/// A yes/no value from the environment: 1, true, yes or on.
fn env_flag(name: &str) -> bool {
    env_value(name).is_some_and(|v| matches!(v.to_lowercase().as_str(), "1" | "true" | "yes" | "on"))
}

/// `install --copy-from <backup>`: the whole second-machine setup.
fn copy_install(cli: &Cli, source: String) -> ExitCode {
    let scopes: Vec<String> = if cli.scopes.is_empty() {
        env_value("THOR_COPY_SCOPES").map(|v| v.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()).unwrap_or_default()
    } else {
        cli.scopes.clone()
    };
    let include_global = cli.include_global || env_flag("THOR_COPY_GLOBAL");
    let Some(serve_exe) = cli.serve_exe.clone().or_else(|| sibling("serve")) else {
        eprintln!("could not work out where `serve` is - name it with --serve-exe");
        return ExitCode::FAILURE;
    };
    let Some(db) = cli.db.clone().or_else(|| default_data_dir().map(|d| d.join("thor.db"))) else {
        eprintln!("could not work out where to keep the copy - name it with --db");
        return ExitCode::FAILURE;
    };
    let Some(settings) = cli.settings.clone().or_else(default_settings_path) else {
        eprintln!("could not find your home directory to place settings.json - name it with --settings");
        return ExitCode::FAILURE;
    };
    // A program that is there but cannot start would make every hook fail open.
    if serve_exe.exists() {
        if let Some(problem) = ops::install::launch_problem(&serve_exe) {
            eprintln!("NOTHING WAS SET UP - refusing to write hooks that call a program that does not start. {problem}");
            return ExitCode::FAILURE;
        }
    }
    let request = ops::copy_setup::Request {
        db,
        source,
        scopes,
        include_global,
        whole: cli.whole_memory || env_flag("THOR_COPY_WHOLE"),
        folder: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        settings,
        serve_exe,
    };
    match ops::copy_setup::run(&request) {
        Ok(lines) => {
            lines.iter().for_each(|line| println!("{line}"));
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}

/// `install --set-helpers on|off`: store or retract the two helper-mode notes
/// and nothing else.
fn set_helpers_only(db: Option<PathBuf>, word: &str) -> ExitCode {
    use ops::install::{set_helpers, HelperChange};
    let on = match word {
        "on" => true,
        "off" => false,
        other => {
            eprintln!("helper-agent mode is 'on' or 'off', not '{other}'. Example: install --set-helpers on --db <your memory>");
            return ExitCode::from(2);
        }
    };
    let Some(db) = db else {
        eprintln!("name the memory to set this for: install --set-helpers {word} --db <your memory>");
        return ExitCode::from(2);
    };
    if !db.is_file() {
        eprintln!(
            "REFUSED: there is no memory at {} - check the path given with --db (the one your assistant's hooks name). Nothing was changed.",
            db.display()
        );
        return ExitCode::FAILURE;
    }
    let changes = match set_helpers(&db, on) {
        Ok(changes) => changes,
        Err(e) => {
            eprintln!("could not change helper-agent mode: {e}");
            return ExitCode::FAILURE;
        }
    };
    let mut refused = false;
    for (id, change) in &changes {
        match change {
            HelperChange::Stored => println!("+ stored the note '{id}'"),
            HelperChange::Restored => println!("+ stored the note '{id}' again (it had been switched off before)"),
            HelperChange::Retracted => println!("+ retracted the note '{id}' (history still shows it)"),
            HelperChange::AlreadyOn => println!("= the note '{id}' was already on"),
            HelperChange::AlreadyOff => println!("= the note '{id}' was already off"),
            HelperChange::Refused(why) => {
                refused = true;
                eprintln!("! could not change the note '{id}': {why}");
            }
        }
    }
    if refused {
        return ExitCode::FAILURE;
    }
    if on {
        println!("helper-agent mode is on for {}: your assistant is reminded of it right before it starts a helper agent. Switch it off again with: install --set-helpers off --db \"{}\"", db.display(), db.display());
    } else {
        println!("helper-agent mode is off for {}", db.display());
    }
    ExitCode::SUCCESS
}

/// `install --set-recording on|off`: write the switch and nothing else.
fn set_recording_only(db: Option<PathBuf>, word: &str) -> ExitCode {
    let on = match word {
        "on" => true,
        "off" => false,
        other => {
            eprintln!("recording is 'on' or 'off', not '{other}'. Example: install --set-recording off --db <your memory>");
            return ExitCode::from(2);
        }
    };
    let Some(db) = db else {
        eprintln!("name the memory to set this for: install --set-recording {word} --db <your memory>");
        return ExitCode::from(2);
    };
    if !db.is_file() {
        eprintln!(
            "REFUSED: there is no memory at {} - check the path given with --db (the one your assistant's hooks name). Nothing was changed.",
            db.display()
        );
        return ExitCode::FAILURE;
    }
    match serve::plan::set_recording(&db, on) {
        Ok(()) if on => {
            println!("recording switched on for {}: real tool calls and replies are kept again, on this machine only, redacted, for 30 days", db.display());
            ExitCode::SUCCESS
        }
        Ok(()) => {
            println!(
                "recording switched off for {}: nothing new is recorded, and the weekly sweep that looks for rules which would wrongly block you says it has no data. What was already recorded is deleted by itself after 30 days.",
                db.display()
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("could not write the switch: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `install --set-backup local|<place>|off`: wire (or remove) the one hook that
/// runs the backup, prepare the git folder, and prove it with a first copy.
fn set_backup_only(db: Option<PathBuf>, settings: Option<PathBuf>, word: &str) -> ExitCode {
    use ops::backup_setup::{backup_hooks_for, default_repo, hook_spec, parse_choice, prepare_repo, prove, remove_backup_hook, Choice};
    let Some(choice) = parse_choice(word) else {
        eprintln!("say where the backup goes: install --set-backup local (on this machine), a folder or address, or off. Example: install --set-backup local --db <your memory>");
        return ExitCode::from(2);
    };
    let Some(db) = db else {
        eprintln!("name the memory to back up: install --set-backup {word} --db <your memory>");
        return ExitCode::from(2);
    };
    if !db.is_file() {
        eprintln!(
            "REFUSED: there is no memory at {} - check the path given with --db (the one your assistant's hooks name). Nothing was changed.",
            db.display()
        );
        return ExitCode::FAILURE;
    }
    let Some(settings) = settings.or_else(default_settings_path) else {
        eprintln!("could not find your home directory to place settings.json - name it with --settings");
        return ExitCode::FAILURE;
    };
    if choice == Choice::Off {
        return match remove_backup_hook(&settings, &db) {
            Ok(0) => {
                println!("= no backup was wired for {}; nothing to remove", db.display());
                ExitCode::SUCCESS
            }
            Ok(n) => {
                println!("+ backup switched off: removed {n} backup hook from {} (a copy of the file was saved next to it). Copies already made stay where they are.", settings.display());
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("could not switch the backup off: {e}");
                ExitCode::FAILURE
            }
        };
    }
    let Some(backup_exe) = sibling("backup").filter(|p| p.exists()) else {
        eprintln!("NOTHING WAS CHANGED: the backup program (backup) is missing next to the installer. THOR is several small programs that must stay together in one folder: unpack the whole download there and run this again.");
        return ExitCode::FAILURE;
    };
    let repo = default_repo(&db);
    let spec = hook_spec(&backup_exe.display().to_string(), &db.display().to_string(), &repo.display().to_string());
    // Another backup already wired for this memory (somebody's own, pointing
    // elsewhere) is not ours to replace or to double up beside.
    match backup_hooks_for(&settings, &db) {
        Ok(existing) if existing.iter().any(|c| !ops::install::same_spelling(c, &spec.command)) => {
            eprintln!(
                "NOTHING WAS CHANGED: a backup is already wired for this memory ({}). To choose again, remove it first with: install --set-backup off --db \"{}\"",
                existing[0],
                db.display()
            );
            return ExitCode::FAILURE;
        }
        Ok(_) => {}
        Err(e) => {
            eprintln!("NOTHING WAS CHANGED: {e}");
            return ExitCode::FAILURE;
        }
    }
    if !ops::githooks::git_available() {
        eprintln!("NOTHING WAS CHANGED: git is not installed on this computer, and the backup is a git folder. Install git first, then run this again.");
        return ExitCode::FAILURE;
    }
    let place = match &choice {
        Choice::Place(place) => Some(place.as_str()),
        _ => None,
    };
    let prepared = match prepare_repo(&repo, place) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("NOTHING WAS WIRED: could not prepare the backup folder: {e}");
            return ExitCode::FAILURE;
        }
    };
    if prepared.created {
        println!("+ made the backup folder {} (a git folder)", repo.display());
    } else {
        println!("= the backup folder {} was already there", repo.display());
    }
    match &prepared.place {
        Some(address) => println!("+ copies also go to {address} (as the remote 'nas')"),
        None if prepared.dropped_remote => println!("+ the earlier place was taken away: copies now stay on this machine"),
        None => println!("= copies stay on this machine"),
    }
    if let Some(parent) = settings.parent().filter(|p| !p.as_os_str().is_empty()) {
        let _ = std::fs::create_dir_all(parent);
    }
    match install_hooks(&settings, &[spec]) {
        Ok(report) if report.results.iter().all(|(_, o)| *o == HookOutcome::AlreadyPresent) => {
            println!("= the backup was already connected to Claude Code")
        }
        Ok(_) => println!("+ connected the backup to Claude Code: it runs when a session starts (at most one copy a day)"),
        Err(e) => {
            eprintln!("the backup folder is ready but connecting it to Claude Code failed: {e}");
            return ExitCode::FAILURE;
        }
    }
    match prove(&backup_exe, &db, &repo) {
        Ok(line) => {
            println!("+ first copy made now, to prove it works: {line}");
            ExitCode::SUCCESS
        }
        Err(reason) => {
            eprintln!("the first copy FAILED, so the backup is wired but not working yet: {reason}");
            ExitCode::FAILURE
        }
    }
}

/// `install --set-weekly-check on|off`: write the switch and nothing else.
fn set_weekly_check_only(db: Option<PathBuf>, word: &str) -> ExitCode {
    let on = match word {
        "on" => true,
        "off" => false,
        other => {
            eprintln!("the weekly check is 'on' or 'off', not '{other}'. Example: install --set-weekly-check off --db <your memory>");
            return ExitCode::from(2);
        }
    };
    let Some(db) = db else {
        eprintln!("name the memory to set this for: install --set-weekly-check {word} --db <your memory>");
        return ExitCode::from(2);
    };
    if !db.is_file() {
        eprintln!(
            "REFUSED: there is no memory at {} - check the path given with --db (the one your assistant's hooks name).              Nothing was changed.",
            db.display()
        );
        return ExitCode::FAILURE;
    }
    match serve::plan::set_weekly_check(&db, on) {
        Ok(()) => {
            println!("weekly check set to '{word}' for {}", db.display());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("could not write the switch: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `install --set-plan max|small`: write the plan and nothing else.
fn set_plan_only(db: Option<PathBuf>, word: &str) -> ExitCode {
    let Some(plan) = serve::plan::Plan::from_word(word) else {
        eprintln!("the plan is 'max' or 'small' (Pro and anything smaller), not '{word}'. Example: install --set-plan small --db <your memory>");
        return ExitCode::from(2);
    };
    let Some(db) = db else {
        eprintln!("name the memory to set the plan for: install --set-plan {word} --db <your memory>");
        return ExitCode::from(2);
    };
    if !db.is_file() {
        eprintln!(
            "REFUSED: there is no memory at {} - check the path given with --db (the one your assistant's hooks name). \
             Nothing was changed.",
            db.display()
        );
        return ExitCode::FAILURE;
    }
    // Told by the user: `confirmed` when Claude Code's file had a tier to
    // confirm, `asked` when it had none.
    let found = serve::plan::default_claude_json().map(|p| serve::plan::detect_in(&p));
    let source = match found {
        Some(d) if d.tier_found => serve::plan::PlanSource::Confirmed,
        _ => serve::plan::PlanSource::Asked,
    };
    match serve::plan::write(&db, plan, source) {
        Ok(()) => {
            println!("plan set to '{}' ({}) for {}", plan.word(), source.word(), db.display());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("could not write the plan: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `install --set-mode release|test`: write the mode and nothing else.
fn set_mode_only(db: Option<PathBuf>, word: &str) -> ExitCode {
    let mode = match word {
        "release" => Some(serve::bug_report::Mode::Release),
        "test" => Some(serve::bug_report::Mode::Test),
        "off" => None,
        other => {
            eprintln!("the mode is 'release', 'test' or 'off', not '{other}'. Example: install --set-mode test --db <your memory>");
            return ExitCode::from(2);
        }
    };
    let Some(db) = db else {
        eprintln!("name the memory to set the mode for: install --set-mode {word} --db <your memory>");
        return ExitCode::from(2);
    };
    if !db.is_file() {
        eprintln!(
            "REFUSED: there is no memory at {} - check the path given with --db (the one your assistant's hooks name). \
             Nothing was changed.",
            db.display()
        );
        return ExitCode::FAILURE;
    }
    let written = match mode {
        Some(mode) => serve::thor_bugs::write_mode(&db, mode),
        None => serve::thor_bugs::write_bug_reports_off(&db),
    };
    match written {
        Ok(()) => {
            println!("bug nudge mode set to '{word}' for {}", db.display());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("could not write the mode: {e}");
            ExitCode::FAILURE
        }
    }
}

/// The binary named `name` sitting next to this installer, with the
/// platform's own executable suffix.
fn sibling(name: &str) -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    Some(dir.join(format!("{name}{}", std::env::consts::EXE_SUFFIX)))
}


/// Read this project's code once, under its own key, so `search_code`,
/// `where_used` and `outline` answer here and the post-commit hook has an
/// index to refresh. `Ok(None)` when one already exists: reading it again
/// would throw away the commit it is at for no gain.
fn build_project_index(index_root: &Path, key: &str, repo: &Path) -> anyhow::Result<Option<usize>> {
    let db = index_root.join(format!("{key}.db"));
    if db.exists() {
        return Ok(None);
    }
    // Not a git checkout? Then there is nothing to index against a commit,
    // and saying so beats writing an index that can never be refreshed.
    anyhow::ensure!(repo.join(".git").exists(), "this folder is not a git checkout");
    std::fs::create_dir_all(index_root)?;
    let mut store = codeindex::Store::open(&db).map_err(|e| anyhow::anyhow!(e))?;
    let stats = codeindex::build_full(&mut store, repo).map_err(|e| anyhow::anyhow!(e))?;
    Ok(Some(stats.files_indexed))
}

/// The file count on record in a sidecar that ALREADY exists for `key`, for
/// the branch below that finds one already built: there is no fresh
/// `BuildStats` to report there, but the sidecar itself still knows how many
/// files it holds.
fn existing_index_file_count(index_root: &Path, key: &str) -> anyhow::Result<usize> {
    let db = index_root.join(format!("{key}.db"));
    let store = codeindex::Store::open(&db).map_err(|e| anyhow::anyhow!(e))?;
    let files = store.file_count().map_err(|e| anyhow::anyhow!(e))?;
    Ok(usize::try_from(files).unwrap_or(0))
}

/// The topic a plain folder is, or would become: the folder that carries (or
/// will carry) the `.thor-project` marker, and the key it holds. Run from a
/// subfolder of an existing topic, it is the topic's own top. A folder with no
/// marker yet is named after itself, except the home folder and a drive root,
/// which are far too broad (`ops::install::topic_folder_name`).
fn topic_of(here: &Path) -> Result<(PathBuf, String), RegisterRefusal> {
    let dir = serve::project::plain_root(here).unwrap_or_else(|| here.to_path_buf());
    let key = match serve::project::resolve_project(&dir) {
        Some(key) => key,
        None => topic_folder_name(&dir)?,
    };
    Ok((dir, key))
}

/// `install --register-root`: register the checkout this is run from, and
/// nothing else. Refuses without `--db` before anything is touched (F7): a
/// registration in the default memory, when the assistant uses another one,
/// would be a registration nobody ever reads.
fn register_only(db: Option<PathBuf>, per_machine: bool, ask_main: bool) -> ExitCode {
    let Some(db) = db else {
        let default = default_data_dir()
            .map(|d| d.join("thor.db").display().to_string())
            .unwrap_or_else(|| "a default memory".to_string());
        eprintln!(
            "name the memory to register in: {} - without --db it would register into {default}, which may not be \
             the memory your assistant uses",
            register_command_for(None)
        );
        return ExitCode::from(2);
    };
    if !db.is_file() {
        eprintln!(
            "REFUSED: there is no memory at {} - check the path given with --db (the one your assistant's hooks \
             name). Nothing was changed.",
            db.display()
        );
        return ExitCode::FAILURE;
    }
    let here = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let existing = serve::project::git_root(&here)
        .or_else(|| serve::project::plain_root(&here))
        .and_then(|top| serve::project::resolve_project(&top));
    let key = match existing {
        Some(key) => key,
        // Not a repository and not a marked topic yet: this IS the plain
        // folder somebody wants a memory for, so it becomes a topic. The
        // second-copy check runs before the marker is written, so a refusal
        // leaves the folder exactly as it was.
        None => {
            let (dir, key) = match topic_of(&here) {
                Ok(found) => found,
                Err(refusal) => {
                    eprintln!("{refusal}");
                    return ExitCode::FAILURE;
                }
            };
            if let Err(refusal) = check_registration_at(&db, &dir, &key) {
                eprintln!("{refusal}");
                return ExitCode::FAILURE;
            }
            if let Err(e) = write_project_marker(&dir, &key) {
                eprintln!("could not mark this folder as a topic: {e}");
                return ExitCode::FAILURE;
            }
            key
        }
    };
    let mut store = match thor_core::event_store::EventStore::open_existing(&db) {
        Ok(store) => store,
        Err(e) => {
            eprintln!("{}", RegisterRefusal::Store(e.to_string()));
            return ExitCode::FAILURE;
        }
    };
    // A marked copy has one writer, so --per-machine there can only ask the main machine.
    let ask_main = ask_main || (per_machine && thor_core::copy_mode::is_copy(&db));
    if ask_main {
        // Validate here (is this a checkout of the project?), write nothing.
        let top = serve::project::git_root(&here).or_else(|| serve::project::plain_root(&here));
        let Some(top) = top.filter(|t| serve::project::resolve_project(t).as_deref() == Some(key.as_str())) else {
            eprintln!("REFUSED: run this from inside a checkout of '{key}'. Nothing was changed.");
            return ExitCode::FAILURE;
        };
        println!(
            "This memory is a copy and has one writer, so nothing was written here. On the MAIN machine run this one \
             line (change the --db path to the main machine's memory if it differs):"
        );
        println!(
            "install{} --register-machine-root \"{}\" --project \"{key}\" --root \"{}\" --db \"{}\"",
            std::env::consts::EXE_SUFFIX,
            model::machine::current_machine_id(),
            top.display().to_string().replace('\\', "/"),
            db.display()
        );
        return ExitCode::SUCCESS;
    }
    match register_root_for(&mut store, &here, &key, None, per_machine) {
        Ok(
            RegisterOutcome::Created { root }
            | RegisterOutcome::Registered { root }
            | RegisterOutcome::CreatedUnderRootRecord { root },
        ) => println!("registered '{key}' -> {root}"),
        Ok(RegisterOutcome::AlreadyHere { root }) => {
            println!("= '{key}' is already registered here ({root}) - nothing to do")
        }
        Ok(RegisterOutcome::Moved { from, to }) => {
            println!("moved '{key}' from {from} to {to} (the old checkout is gone)")
        }
        Err(refusal) => {
            eprintln!("{refusal}");
            return ExitCode::FAILURE;
        }
    }
    ExitCode::SUCCESS
}

/// `install --register-machine-root`: on the main machine, record the working
/// copy another machine (a laptop) keeps of `key`.
fn register_machine_only(db: Option<PathBuf>, key: &str, root: &str, machine: &str) -> ExitCode {
    let Some(db) = db.filter(|d| d.is_file()) else {
        eprintln!("REFUSED: name the main memory with --db (an existing thor.db). Nothing was changed.");
        return ExitCode::FAILURE;
    };
    let mut store = match thor_core::event_store::EventStore::open_existing(&db) {
        Ok(store) => store,
        Err(e) => {
            eprintln!("{}", RegisterRefusal::Store(e.to_string()));
            return ExitCode::FAILURE;
        }
    };
    match register_root_remote(&mut store, key, root, machine) {
        Ok(RegisterOutcome::AlreadyHere { root }) => {
            println!("= '{key}' is already registered for {machine} ({root}) - nothing to do")
        }
        Ok(RegisterOutcome::Moved { from, to }) => println!("moved '{key}' for {machine} from {from} to {to}"),
        Ok(
            RegisterOutcome::Created { root }
            | RegisterOutcome::Registered { root }
            | RegisterOutcome::CreatedUnderRootRecord { root },
        ) => println!("registered '{key}' for {machine} -> {root}"),
        Err(refusal) => {
            eprintln!("{refusal}");
            return ExitCode::FAILURE;
        }
    }
    ExitCode::SUCCESS
}

/// The line the per-project install prints for what registering this
/// checkout did.
fn print_record_outcome(key: &str, outcome: Result<RegisterOutcome, RegisterRefusal>) {
    match outcome {
        Ok(RegisterOutcome::Created { root }) => {
            println!("+ recorded this project, '{key}', in the memory: notes about it can now be saved without any extra question");
            println!("+ registered its working copy: {root}");
        }
        Ok(RegisterOutcome::CreatedUnderRootRecord { root }) | Ok(RegisterOutcome::Registered { root }) => {
            println!("+ registered the working copy of '{key}': {root}")
        }
        Ok(RegisterOutcome::AlreadyHere { .. }) => println!("= project record already names this checkout"),
        Ok(RegisterOutcome::Moved { from, to }) => {
            println!("+ moved '{key}' from {from} to {to} (the old checkout is gone)")
        }
        Err(e) => println!("  ! could not record this project in the memory: {e}"),
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    if cli.register_root {
        return register_only(cli.db, cli.per_machine, cli.ask_main);
    }
    if let (Some(machine), Some(root), Some(key)) = (cli.register_machine_root.as_deref(), cli.root_path.as_deref(), cli.project.as_deref()) {
        return register_machine_only(cli.db, key, &root.display().to_string(), machine);
    }
    if let Some(word) = cli.set_mode.as_deref() {
        return set_mode_only(cli.db, word);
    }
    if let Some(word) = cli.set_plan.as_deref() {
        return set_plan_only(cli.db, word);
    }
    if let Some(word) = cli.set_weekly_check.as_deref() {
        return set_weekly_check_only(cli.db, word);
    }
    if let Some(word) = cli.set_recording.as_deref() {
        return set_recording_only(cli.db, word);
    }
    if let Some(word) = cli.set_helpers.as_deref() {
        return set_helpers_only(cli.db, word);
    }
    if let Some(word) = cli.set_backup.as_deref() {
        return set_backup_only(cli.db, cli.settings.clone(), word);
    }

    // SECOND MACHINE: a read-only copy, not a memory of its own. The value may
    // come from the environment, which is how the one-line download passes it on.
    if let Some(source) = cli.copy_from.clone().or_else(|| env_value("THOR_COPY_FROM")) {
        return copy_install(&cli, source);
    }
    if !cli.scopes.is_empty() || cli.include_global || cli.whole_memory {
        eprintln!("--scope, --include-global and --whole-memory only work together with --copy-from <the backup>. Nothing was changed.");
        return ExitCode::FAILURE;
    }

    let data_dir = default_data_dir();
    let serve_exe = match cli.serve_exe.or_else(|| sibling("serve")) {
        Some(p) => p,
        None => {
            eprintln!("could not work out where `serve` is - name it with --serve-exe");
            return ExitCode::FAILURE;
        }
    };
    // Filtered on existence on purpose: `sibling` returns a path whether or
    // not the file is there, and registering a tool server that points at a
    // binary that is not present is the same silent half-install the serve
    // check below refuses - the agent would simply never be able to write,
    // with nothing saying why. A missing binary here becomes `None`, which the
    // step-3 match turns into a loud refusal, not a dead registration.
    let mcp_exe = cli.mcp_exe.or_else(|| sibling("mcp")).filter(|p| p.exists());
    let db = match cli.db.or_else(|| data_dir.as_ref().map(|d| d.join("thor.db"))) {
        Some(p) => p,
        None => {
            eprintln!("could not work out where to keep the memory - name it with --db");
            return ExitCode::FAILURE;
        }
    };
    let code_index_root = cli
        .code_index_root
        .or_else(|| db.parent().map(|d| d.join("codeindex")));

    // The two files this command WRITES to. Defaulted to Claude Code's own
    // per-user locations (see default_settings_path / default_user_mcp_path)
    // rather than demanded as absolute paths a newcomer has no way to know -
    // which was the actual reported blocker. Both writers back up first and
    // only ever add, so a well-known default keeps the safety and drops the
    // friction.
    let settings = match cli.settings.clone().or_else(default_settings_path) {
        Some(p) => p,
        None => {
            eprintln!("could not find your home directory to place settings.json - name it with --settings");
            return ExitCode::FAILURE;
        }
    };
    let mcp_target: Option<PathBuf> = if cli.no_mcp {
        None
    } else {
        match cli.mcp_json.clone().or_else(default_user_mcp_path) {
            Some(p) => Some(p),
            None => {
                eprintln!(
                    "could not find your home directory to place the MCP config - name it with \
                     --mcp-json, or pass --no-mcp to install a read-only memory on purpose"
                );
                return ExitCode::FAILURE;
            }
        }
    };
    // A SECOND COPY NEVER TAKES OVER A REGISTRATION (Z3, D3), and the check
    // runs here, before this run writes anything at all - settings, hooks,
    // the marker, the code index or the store - so "Nothing was changed" in
    // the refusal is literally true. A project already registered at another
    // folder that still exists is refused; see `ops::install::register_root`.
    let mut here = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut project = cli.project.clone();
    // `--topic` turns the plain folder this is run from (or the topic it sits
    // in) into one project, named after the folder. Inside a git repository
    // there is nothing to turn: the repository already is the project.
    if cli.topic && serve::project::git_root(&here).is_none() {
        match topic_of(&here) {
            Ok((dir, key)) => {
                here = dir;
                project = Some(key);
            }
            Err(refusal) => {
                eprintln!("{refusal}");
                return ExitCode::FAILURE;
            }
        }
    }
    if let Some(key) = project.clone().or_else(|| serve::project::resolve_project(&here)) {
        let top = serve::project::git_root(&here).unwrap_or_else(|| here.clone());
        let checked = if cli.topic { check_registration_at(&db, &top, &key) } else { check_registration(&db, &here, &key) };
        if let Err(refusal) = checked {
            eprintln!("{refusal}");
            return ExitCode::FAILURE;
        }
    }

    // ~/.claude may not exist yet on a machine where Claude Code has never run.
    // install_hooks writes the file, so its parent has to be there first;
    // best-effort, because install_hooks reports the real error if it is not.
    if let Some(parent) = settings.parent() {
        if !parent.as_os_str().is_empty() {
            let _ = std::fs::create_dir_all(parent);
        }
    }

    // A hook pointing at a binary that is not there fails OPEN: the agent
    // carries on and the memory simply never speaks again. Nothing reports
    // that afterwards, and THIS is the only moment it is still visible - so it
    // is a refusal, not a warning.
    //
    // A warning stood here until 2026-08-14. It went to stderr in the middle
    // of a wall of "+ done" lines, which is precisely where an install that
    // did not work hides: a newcomer reads the successes, restarts the agent,
    // and owns a memory that will never speak to them. An installer that can
    // see the failure and completes anyway is the same silent-failure class
    // this whole system exists to refuse.
    if !serve_exe.exists() {
        eprintln!(
            "NOTHING WAS SET UP: the file {} is missing, and THOR cannot work without it. THOR is several small \
             programs that must stay together in ONE folder: unpack the whole download there (do not copy install \
             on its own), then run install from that folder again. (For developers: build with `cargo build \
             --release --features semantic` in thor2, or name the real file with --serve-exe.)",
            serve_exe.display()
        );
        return ExitCode::FAILURE;
    }
    // A file that is there but cannot be started is the same failure one step
    // later: every hook would fail open. Asked once, with `--version`, which
    // writes nothing.
    if let Some(problem) = ops::install::launch_problem(&serve_exe) {
        eprintln!("NOTHING WAS SET UP - refusing to write hooks that call a program that does not start. {problem}");
        return ExitCode::FAILURE;
    }

    println!("Setting THOR up. It only changes the places listed here.");
    println!();
    println!("memory:       {}  (everything THOR remembers is kept in this one file)", db.display());
    println!("Claude Code:  {}  (a backup copy is made first)", settings.display());
    match (&mcp_target, &mcp_exe) {
        (Some(p), Some(_)) => println!("note-saving:  {}  (a backup copy is made first)", p.display()),
        (Some(_), None) => println!("note-saving:  the program that saves notes was not found - see below"),
        (None, _) => println!("note-saving:  off on purpose (--no-mcp): your assistant can read the memory but not add to it"),
    }
    println!();

    // 1. The store. First, because every later step points at it, and because
    // the health check refuses to run without one.
    match ensure_store(&db) {
        Ok(StoreOutcome::Created) => {
            println!("+ created a memory at {}", db.display());
            // Only ever on a store this run created. A brand new memory is
            // empty, and an agent with nothing in front of it writes notes in
            // shapes that never come back - which fails silently on both
            // sides. Seeding an EXISTING memory would be pushing four notes
            // into someone's real work uninvited, so that never happens.
            let absolute = |p: &Path| std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf());
            let paths = ops::questionnaire::Paths {
                installer: std::env::current_exe().ok(),
                db: Some(absolute(&db)),
                settings: Some(absolute(&settings)),
                tool_server_file: mcp_target.as_deref().map(absolute),
            };
            match seed_working_contract_at(&db, serve::plan::default_claude_json().as_deref(), &paths) {
                Ok(seeded) => {
                    // The numbered first-session list is a lookup item, not a note.
                    let stored = seeded.iter().filter(|s| s.stored && s.id != ops::questionnaire::LOOKUP_ID).count();
                    println!("+ put {stored} starting notes in it. They teach your assistant how to write notes that come back when needed, and give it a few good working habits.");
                    if seeded.iter().any(|s| s.stored && s.id == ops::questionnaire::LOOKUP_ID) {
                        println!("+ put the numbered list of everything this install set up in it too: your assistant shows it to you in your first session, and you answer by number (nothing waits for you).");
                    }
                    for s in seeded.iter().filter(|s| !s.stored) {
                        let why = s.refusal.as_deref().unwrap_or("no reason given");
                        println!("  ! {} was refused: {}", s.id, why);
                        // THOR's own note refused by THOR's own gate is a bug
                        // in THOR: it becomes a candidate (`serve::thor_bugs`).
                        serve::thor_bugs::add(&db, serve::thor_bugs::seed_refused_finding(&s.id, why));
                    }
                    println!("  You never have to read them. Any of them can be changed or deleted later: just ask your assistant.");
                }
                Err(e) => println!("  ! could not write the starting notes: {e}"),
            }
        }
        Ok(StoreOutcome::AlreadyThere) => println!("= your memory was already there; nothing in it was touched"),
        Err(e) => {
            eprintln!("install refused: could not create the memory: {e}");
            return ExitCode::FAILURE;
        }
    }

    // 1a. The mode of the bug nudge: THOR finds some of its own bugs and offers
    // one per session to the agent to be written up for the public THOR
    // repository (`serve::thor_bugs`). The default is `release` on every
    // build; only `install --set-mode test` makes the owner's test mode,
    // which publishes nothing. A mode already set is kept.
    // Whatever an earlier build left as a candidate is closed by this install:
    // only new evidence from this build may raise one (`retire_old_evidence`).
    let retired = serve::thor_bugs::retire_old_evidence_at(&db);
    if retired > 0 {
        println!("= {retired} older bug candidate(s) closed: they came from an earlier build");
    }
    match ops::install::settle_mode(&db, None) {
        Ok((mode, how)) => {
            let word = serve::thor_bugs::mode_word(mode);
            match how {
                ops::install::ModeHow::Kept => println!("= bug nudge mode already '{word}' (left untouched)"),
                ops::install::ModeHow::Default => println!(
                    "+ bug nudge mode 'release': if THOR finds a bug in itself, your assistant may offer to report it to the makers - it shows you the exact text first, and nothing is sent without your yes"
                ),
                ops::install::ModeHow::Told => println!("+ bug nudge mode '{word}'"),
                ops::install::ModeHow::Off => println!("= bug reporting is switched off (your choice); left that way"),
            }
        }
        Err(e) => println!("  ! could not write the bug nudge mode: {e}"),
    }

    // 1a-2. The plan: read from Claude Code's own file, never guessed silently.
    // The first-session questions confirm it with the user (numbered item 1).
    let claude_json = serve::plan::default_claude_json();
    let plan_before = serve::plan::read(&db);
    match serve::plan::settle(&db, claude_json.as_deref()) {
        // A second run that finds the same plan on record changes nothing and says so.
        Ok((plan, source, _)) if plan_before == Some((plan, source)) => {
            println!("= plan already '{}' ({}); left alone", plan.word(), source.word())
        }
        Ok((plan, source, found)) => match (source, found.tier_found) {
            (serve::plan::PlanSource::Detected, true) => println!(
                "+ plan: Claude Code says your plan is {} (the first-session questions ask you to confirm it; on a smaller plan THOR's weekly check never runs more than once a week)",
                if plan == serve::plan::Plan::Max { "Max" } else { "Pro or smaller" }
            ),
            (serve::plan::PlanSource::Detected, false) => println!(
                "+ plan: Claude Code's settings do not say which plan you have, so THOR assumes a smaller one (the cheap weekly check); the first-session questions ask you"
            ),
            _ => println!("= plan already '{}' ({}); left alone", plan.word(), source.word()),
        },
        Err(e) => println!("  ! could not write the plan: {e}"),
    }

    // 1b. The response-guard rulebook beside it: what gives the `Stop` hook
    // something to actually check. Checked on THIS run regardless of
    // whether the store above was just created or was already there - a
    // store from before this rulebook existed has exactly the same missing
    // file, and deserves the same fix. Never overwrites one already there.
    match seed_response_rulebook(&db) {
        Ok(report) => match report.outcome {
            RulebookOutcome::Written => {
                println!("+ added a small file that keeps your assistant's replies tidy: {}", report.path.display())
            }
            RulebookOutcome::AlreadyThere => {
                println!("= the reply-tidying file was already there ({}); left alone", report.path.display())
            }
        },
        Err(e) => println!("  ! could not write the reply-tidying file: {e}"),
    }

    // 1c. The end-of-session evaluation command. THOR holds a turn open for a
    // verdict on one served note at a time; this is how that debt gets
    // settled in bulk. Written regardless of --no-mcp (see
    // seed_eval_command's own doc comment for why), and never overwrites a
    // copy that is already there - the owner's own edited version, or one an
    // earlier install already wrote.
    match default_eval_command_path() {
        Some(path) => {
            let bin_dir = serve_exe.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| PathBuf::from("."));
            match seed_eval_command(&path, &bin_dir, &db) {
                Ok(report) => match report.outcome {
                    EvalCommandOutcome::Written => println!(
                        "+ added the command /thor-eval for your assistant (optional: type it at the end of a work session to tidy the memory): {}",
                        report.path.display()
                    ),
                    EvalCommandOutcome::AlreadyThere => {
                        println!("= the /thor-eval command was already there ({})", report.path.display())
                    }
                },
                Err(e) => println!("  ! could not add the /thor-eval command: {e}"),
            }
        }
        None => println!("  ! could not find your home folder to place the /thor-eval command"),
    }

    // 2. The hooks: what lets the memory speak at all.
    let specs = standard_hooks(&serve_exe.display().to_string(), &db.display().to_string());
    match install_hooks(&settings, &specs) {
        Ok(report) => {
            let changed = report.results.iter().filter(|(_, o)| *o != HookOutcome::AlreadyPresent).count();
            if changed == 0 {
                println!("= THOR was already connected to Claude Code");
            } else {
                println!("+ connected THOR to Claude Code ({changed} of {} connections), so your memory speaks up by itself", report.results.len());
            }
            for old in &report.replaced {
                println!("  fixed an old connection that pointed at a program that no longer exists: {old}");
            }
            if let Some(p) = report.backup_path.as_ref().filter(|_| changed > 0) {
                println!("  your previous Claude Code settings were saved as {}", p.display());
            }
        }
        Err(e) => {
            eprintln!("NOTHING WAS CONNECTED to Claude Code: {e}");
            eprintln!("Your memory file was created. Fix the problem named above, then run install again.");
            return ExitCode::FAILURE;
        }
    }

    // 3. The git hooks: what keeps the code index from drifting. Only when we
    // know where the indexes live - without an index root there is nothing for
    // a refresh to update, and wiring a machine-wide git setting for nothing is
    // exactly the kind of surprise this installer must not spring.
    // Someone who only keeps notes about a topic may never have installed git,
    // and does not need it: say so, instead of reporting a failed git command.
    if !githooks::git_available() {
        println!("= git is not installed on this computer. That is fine: it is only needed to keep programming projects searchable, not for topic folders.");
    } else if let Some(root) = &code_index_root {
        let hooks_dir = db.parent().map(|d| d.join("githooks"));
        let codeindex_exe = serve_exe.with_file_name(if cfg!(windows) {
            "codeindex.exe"
        } else {
            "codeindex"
        });

        match (hooks_dir, codeindex_exe.exists()) {
            (Some(hooks_dir), true) => {
                let exe = codeindex_exe.display().to_string().replace('\\', "/");
                let index_root = root.display().to_string().replace('\\', "/");
                match githooks::install_git_hooks(&hooks_dir, &exe, &index_root) {
                    Ok(results) => {
                        let added = results.iter().filter(|(_, o)| *o != HookOutcome::AlreadyPresent).count();
                        // The media gate lives in the pre-commit helper: say so
                        // whenever that one was written or changed.
                        let gate_is_new = results
                            .iter()
                            .any(|(event, outcome)| event == "pre-commit" && *outcome != HookOutcome::AlreadyPresent);
                        let say_gate = || {
                            println!("+ git now also stops a commit that adds a video file or a file above 25 MB, in every repository");
                            println!("  its message says what to do instead; the README section \"The media gate\" tells how to allow one file or change the limit");
                        };
                        if added == 0 {
                            println!("= the helpers for programming projects were already in {}", hooks_dir.display());
                        } else {
                            println!("+ wrote {added} small helper(s) for programming projects (git) to {}", hooks_dir.display());
                        }
                        match githooks::wire_hooks_path(&hooks_dir) {
                            Ok(githooks::HooksPathOutcome::Set) => {
                                println!("+ told git to use them, so THOR can keep a programming project searchable after every commit");
                                println!("  your own git hooks keep working: each helper hands control back to them");
                                say_gate();
                            }
                            Ok(githooks::HooksPathOutcome::AlreadyOurs) => {
                                println!("= git was already told to use them");
                                if gate_is_new {
                                    say_gate();
                                }
                            }
                            Ok(githooks::HooksPathOutcome::Foreign(theirs)) => {
                                println!("! git is already set up to use {theirs}, so that was left alone.");
                                println!("  Searching the code of a programming project will not update by itself until that is sorted out. Topic folders are not affected.");
                            }
                            Err(e) => println!("! could not tell git about the helpers: {e}"),
                        }
                    }
                    Err(e) => println!("! could not write the git helpers: {e}"),
                }
            }
            (_, false) => println!(
                "! the program that reads code (codeindex) is not next to {} - skipped the git helpers",
                serve_exe.display()
            ),
            (None, _) => println!("! could not work out where to keep the git helpers - skipped"),
        }
    }

    // 4. The tool server: what lets the agent write back.
    match (&mcp_target, &mcp_exe) {
        (Some(path), Some(exe)) => {
            let mut args = vec!["--db".to_string(), db.display().to_string()];
            if let Some(root) = &code_index_root {
                args.push("--code-index-root".to_string());
                args.push(root.display().to_string());
            }
            match install_tool_server(path, "thor", &exe.display().to_string(), &args) {
                Ok(report) => match report.outcome {
                    ServerOutcome::Added => println!("+ gave your assistant a way to save notes (registered in {})", path.display()),
                    ServerOutcome::AlreadyPresent => println!("= your assistant could already save notes"),
                    ServerOutcome::Replaced => {
                        println!("+ gave your assistant a way to save notes (registered in {})", path.display());
                        if let Some(old) = &report.replaced {
                            println!("  this replaced an older one that pointed at {old}");
                        }
                        if let Some(p) = &report.backup_path {
                            println!("  the previous file was saved as {}", p.display());
                        }
                    }
                },
                Err(e) => {
                    eprintln!("THOR is connected to Claude Code, but giving your assistant a way to save notes failed: {e}");
                    eprintln!("Fix the problem named above, then run install again.");
                    return ExitCode::FAILURE;
                }
            }
        }
        (Some(_), None) => {
            eprintln!("The program that lets your assistant save notes (mcp) is missing next to the installer, so your assistant could only read the memory, never add to it. Unpack the whole download into one folder and run install from there again. (To install a read-only memory on purpose: add --no-mcp.)");
            return ExitCode::FAILURE;
        }
        (None, _) => {
            println!("- note-saving was left off (--no-mcp)");
            println!("  your assistant will be able to READ the memory but never add to it");
        }
    }

    // 5. The project scope: the marker only with --project, the code index
    // either way.
    //
    // GIVING A FOLDER ITS OWN MEMORY IS ONE ACTION. Measured 2026-08-18, on a
    // fresh project the owner asked an agent to scope: it had to find three
    // separate things - the marker file that binds this folder to the key, a
    // first fact carrying that key so the scope exists at all, and a code
    // index under the same key so the post-commit hook has something to
    // refresh. Nothing named them together, and the marker was only found
    // because the owner pointed at another repository that had one. Three
    // steps nobody can discover is a step that gets skipped.
    //
    // THE DEFECT THIS CLOSES, measured 2026-09-09 in a sandboxed first run
    // that followed the setup page literally. Both the marker write and the
    // code index used to live behind `if let Some(key) = &cli.project`, but
    // the setup page tells a newcomer to leave that flag off most of the
    // time now, because a folder is already named after its own project (see
    // `serve::project::resolve_project`). Leaving it off therefore wrote no
    // marker AND built no index, with nothing printed either way: a stranger
    // followed the page, restarted their agent, and searching the code
    // answered nothing - the exact silent-failure shape this project refuses
    // everywhere else. Fixed by resolving the name through the same function
    // every other surface uses whenever `--project` is absent, and indexing
    // under it regardless. The flag keeps its two jobs and gains no third:
    // it writes the marker that overrides the folder-derived name, and it
    // still refuses to change a name that is already there.
    if let Some(key) = &project {
        match write_project_marker(&here, key) {
            Ok(MarkerOutcome::Written) => println!("+ this folder now has its own memory, under {key:?}"),
            Ok(MarkerOutcome::AlreadyThere) => println!("= this folder already has its own memory, under {key:?}"),
            Err(e) => {
                eprintln!("everything else is installed, but giving this folder its own memory was refused: {e}");
                return ExitCode::FAILURE;
            }
        }
    }
    let key_here = project.or_else(|| serve::project::resolve_project(&here));
    // A plain folder has no code to read and no commits to follow: it is
    // registered as the topic it is, and that is all there is to do.
    let is_plain = serve::project::git_root(&here).is_none();
    match key_here {
        Some(key) if is_plain => match record_topic_opened(&db, &here, &key) {
            Ok(RegisterOutcome::Created { .. } | RegisterOutcome::CreatedUnderRootRecord { .. } | RegisterOutcome::Registered { .. }) => {
                println!("+ '{key}' is now a topic: what you tell your assistant while working in this folder is remembered for this topic only");
                println!("  (this folder is not a programming project, so there is no code to search; your assistant reads your files itself)");
            }
            Ok(RegisterOutcome::AlreadyHere { .. }) => println!("= '{key}' was already set up as a topic here"),
            Ok(RegisterOutcome::Moved { from, to }) => {
                println!("+ the topic '{key}' moved from {from} to {to} (the old folder is gone)")
            }
            Err(e) => println!("  ! could not set up this topic: {e}"),
        },
        Some(key) => match code_index_root.as_deref().map(|r| build_project_index(r, &key, &here)).transpose() {
            Ok(Some(Some(files))) => {
                println!("+ read this project's code, {files} file(s), so it can be searched and kept fresh on every commit");
                // Immediately after the read succeeds: opens the project in
                // the memory itself, so the very first `remember` filed under
                // it is never refused as an unknown collection (see
                // `record_project_opened`'s own doc comment for the defect
                // this closes). Idempotent - a rerun that hits this same
                // branch (the code index was rebuilt, but the record already
                // exists) writes nothing more.
                print_record_outcome(&key, record_project_opened(&db, &here, &key, files));
            }
            Ok(Some(None)) => {
                println!("= this project's code was already read; every commit keeps it fresh");
                // THE UPGRADE GAP THIS CLOSES: an owner who installed before
                // `record_project_opened` existed (or any run that finds a
                // sidecar but lost the record some other way) hits this
                // branch on every later install, forever - the sidecar is
                // already built, so the fresh-build branch above, the only
                // other place that calls `record_project_opened`, never
                // fires again to close it. Closed the same way here, from
                // the sidecar's own file count rather than a fresh build's
                // stats, and just as idempotent: once recorded, a repeat of
                // THIS branch prints "already recorded" and writes nothing
                // more (see `record_project_opened`'s own doc comment).
                if let Some(root) = code_index_root.as_deref() {
                    match existing_index_file_count(root, &key) {
                        Ok(files) => print_record_outcome(&key, record_project_opened(&db, &here, &key, files)),
                        Err(e) => println!("  ! could not read the existing code index to record this project: {e}"),
                    }
                }
            }
            Ok(None) => {}
            Err(e) => println!("- the code here could not be read ({e}) - the memory works, code search does not"),
        },
        None => {
            let installer = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_else(|_| "install".to_string());
            println!("- no topic set up in this folder. That is fine: THOR already works everywhere.");
            println!("  To give one folder (one subject, like 'Kitchen renovation') its own memory, open that folder and run:  \"{installer}\" --topic");
        }
    }

    println!();
    println!("Done. ONE THING LEFT FOR YOU: close Claude Code completely and open it again.");
    println!("Claude Code only learns about THOR when it starts, so until you do this nothing above is active.");
    println!("To check that THOR is healthy afterwards, ask your assistant to run THOR's doctor:");
    let bin_dir = serve_exe.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| PathBuf::from("."));
    println!("  {}", ops::install::doctor_invocation(&bin_dir, &db, cfg!(windows)));
    ExitCode::SUCCESS
}
