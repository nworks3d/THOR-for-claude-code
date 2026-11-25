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
    record_topic_opened, register_command_for, topic_of, register_root_for, register_root_remote, seed_eval_command, seed_response_rulebook,
    seed_working_contract_at, standard_hooks, write_project_marker, EvalCommandOutcome, HookOutcome, HookSpec,
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
    #[arg(long, value_parser = thor_core::app_data::parse_db_path)]
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

    /// Set ONLY the connection between THOR and Claude Code, `on` or `off`, and
    /// nothing else. `off` removes THOR's own entries from Claude Code's
    /// settings file (nothing else there is touched) and records your no
    /// beside the memory, so a later plain install does not wire them again
    /// and the health check calls it your choice, not a problem. `on` wires
    /// them again. Claude Code must be closed completely and opened again for
    /// either to take effect. Needs --db.
    #[arg(
        long = "set-connections",
        value_name = "on|off",
        conflicts_with_all = ["project", "topic", "register_root", "mcp_json", "no_mcp", "set_mode", "set_plan", "set_weekly_check", "set_backup", "set_recording", "set_language", "set_helpers", "set_changelog_flow", "set_git_hooks", "set_reply_rules", "copy_from"]
    )]
    set_connections: Option<String>,

    /// Set ONLY THOR's helpers in git, `on` or `off`, and nothing else. `off`
    /// tells git to stop using THOR's helper folder (only when it is THOR's;
    /// another tool's setting is left alone) and records your no beside the
    /// memory. The code of a project then stops being refreshed after a
    /// commit, and the check against video and very large files stops too.
    /// `on` wires them again. Needs --db.
    #[arg(
        long = "set-git-hooks",
        value_name = "on|off",
        conflicts_with_all = ["project", "topic", "register_root", "settings", "mcp_json", "no_mcp", "set_mode", "set_plan", "set_weekly_check", "set_backup", "set_recording", "set_language", "set_helpers", "set_changelog_flow", "set_connections", "set_reply_rules", "copy_from"]
    )]
    set_git_hooks: Option<String>,

    /// Set ONLY the reply rules (the small file next to the memory that keeps
    /// your assistant's replies short and tidy), `on` or `off`, and nothing
    /// else. `off` sets the whole file aside (your changes in it are kept) and
    /// records your no beside the memory; `on` puts it back. Needs --db.
    #[arg(
        long = "set-reply-rules",
        value_name = "on|off",
        conflicts_with_all = ["project", "topic", "register_root", "settings", "mcp_json", "no_mcp", "set_mode", "set_plan", "set_weekly_check", "set_backup", "set_recording", "set_language", "set_helpers", "set_changelog_flow", "set_connections", "set_git_hooks", "copy_from"]
    )]
    set_reply_rules: Option<String>,

    /// One THOR per computer account: when Claude Code is already connected to
    /// another memory, a plain install refuses. This moves the connection
    /// (its four entries) to the memory named with --db.
    #[arg(long = "move-to-this-store")]
    move_to_this_store: bool,

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

    /// Set ONLY the language your assistant talks to you in (`nl`, `en`, `de`,
    /// `fr` or `es`), or `off`, and nothing else. Once a language is set, a
    /// reply meant for you that clearly reads as another language is sent back
    /// once to be rewritten. With none set, no language is checked. Running it
    /// twice changes nothing the second time. Needs --db.
    #[arg(
        long = "set-language",
        value_name = "CODE|off",
        conflicts_with_all = ["project", "settings", "mcp_json", "no_mcp", "serve_exe", "mcp_exe", "code_index_root", "register_root", "set_mode", "set_plan", "set_weekly_check", "set_backup", "set_recording", "set_helpers", "set_temp_folder", "set_tools_folder", "topic", "copy_from"]
    )]
    set_language: Option<String>,

    /// Set ONLY helper-agent mode, `on` or `off`, and nothing else. It is ON in
    /// a new memory. `off` retracts the five notes about helper agents (your
    /// assistant then does the work itself in the chat); `on` stores them again
    /// (your assistant hands work to cheaper helper agents and keeps the chat
    /// free). Running it twice changes nothing the second time. Needs --db.
    #[arg(
        long = "set-helpers",
        value_name = "on|off",
        conflicts_with_all = ["project", "settings", "mcp_json", "no_mcp", "serve_exe", "mcp_exe", "code_index_root", "register_root", "set_mode", "set_plan", "set_weekly_check", "set_backup", "set_recording", "topic"]
    )]
    set_helpers: Option<String>,

    /// Set ONLY the starter work rules, `on` or `off`, and nothing else. They
    /// are on in a new memory. `off` retracts the work-habit notes of the
    /// starting set (the reminders to run tests before saying done, never commit
    /// a secret, check your work, and so on) so your assistant is no longer
    /// reminded of them; `on` stores them again. The notes THOR itself needs,
    /// the helper notes and the changelog notes are not touched. Running it
    /// twice changes nothing the second time. Needs --db.
    #[arg(
        long = "set-starter-rules",
        value_name = "on|off",
        conflicts_with_all = ["project", "settings", "mcp_json", "no_mcp", "serve_exe", "mcp_exe", "code_index_root", "register_root", "set_mode", "set_plan", "set_weekly_check", "set_backup", "set_recording", "set_language", "set_helpers", "set_changelog_flow", "set_connections", "set_git_hooks", "set_reply_rules", "set_temp_folder", "set_tools_folder", "topic", "copy_from"]
    )]
    set_starter_rules: Option<String>,

    /// Set ONLY the answer to "shall I keep a changelog and version numbers for
    /// this project?", `on` or `off`, and nothing else. Run it from inside the
    /// project: the answer is kept for that project alone, beside the memory.
    /// `on` also makes sure the notes that know the flow are in the memory;
    /// `off` hides them in this project and the question is never raised there
    /// again. Running it twice changes nothing the second time. Needs --db.
    #[arg(
        long = "set-changelog-flow",
        value_name = "on|off",
        conflicts_with_all = ["project", "settings", "mcp_json", "no_mcp", "serve_exe", "mcp_exe", "code_index_root", "register_root", "set_mode", "set_plan", "set_weekly_check", "set_backup", "set_recording", "set_language", "set_helpers", "set_temp_folder", "set_tools_folder", "topic", "copy_from"]
    )]
    set_changelog_flow: Option<String>,

    /// Set ONLY the temp-until-live folder, and nothing else: the folder that
    /// holds one sub-folder per production, named <YYYY-MM-DD>_<repo>_<subject>,
    /// each with a STATUS.txt whose first line starts with "STATUS:" (a
    /// production that is finished has "STATUS: live" there and may be deleted).
    /// Give the full path, or `off` to switch it off. A session start then says,
    /// once a day, which productions may be deleted or are older than 30 days.
    /// THOR only reads the folder and deletes nothing. Needs --db.
    #[arg(
        long = "set-temp-folder",
        value_name = "PATH|off",
        conflicts_with_all = ["project", "settings", "mcp_json", "no_mcp", "serve_exe", "mcp_exe", "code_index_root", "register_root", "set_mode", "set_plan", "set_weekly_check", "set_backup", "set_recording", "set_helpers", "topic", "copy_from"]
    )]
    set_temp_folder: Option<String>,

    /// Set ONLY the tools folder, and nothing else: the folder that holds one
    /// sub-folder per reusable helper tool, each with a README.txt (or
    /// README.md) whose first line says in one sentence what the tool does.
    /// Give the full path, or `off` to switch it off. Every session start then
    /// lists the tools to your assistant, and a turn is held once to tidy up
    /// script files left outside any git repository. THOR only reads the
    /// folder. Needs --db.
    #[arg(
        long = "set-tools-folder",
        value_name = "PATH|off",
        conflicts_with_all = ["project", "settings", "mcp_json", "no_mcp", "serve_exe", "mcp_exe", "code_index_root", "register_root", "set_mode", "set_plan", "set_weekly_check", "set_backup", "set_recording", "set_helpers", "topic", "copy_from"]
    )]
    set_tools_folder: Option<String>,

    /// Put the person's loose files in order: carry out a plan (--plan) of
    /// `[{"folder": "<absolute path>", "files": ["<absolute path>", ...]}]`.
    /// Each folder is made if needed and given its own memory exactly as
    /// --topic does, and each file is moved into it. Never deletes, never
    /// overwrites (a taken name becomes "name (2)"), never touches a file
    /// outside the home folder or inside a git repository, and refuses the
    /// home folder, Desktop, Documents, Downloads, a drive root and a
    /// program's own folders. Every move is written to an undo log beside the
    /// memory. Needs --db.
    #[arg(
        long = "tidy",
        requires = "plan",
        conflicts_with_all = ["project", "mcp_exe", "no_mcp", "serve_exe", "code_index_root", "register_root", "topic", "copy_from", "set_mode", "set_plan", "set_weekly_check", "set_backup", "set_recording", "set_language", "set_helpers", "set_temp_folder", "set_tools_folder", "uninstall", "tidy_undo"]
    )]
    tidy: bool,

    /// With --tidy: the JSON file that holds the plan.
    #[arg(long = "plan", value_name = "FILE", requires = "tidy")]
    plan: Option<PathBuf>,

    /// Put the files of the newest tidy-up back where they were (never
    /// overwriting anything; the folders it made stay). Needs --db.
    #[arg(
        long = "tidy-undo",
        conflicts_with_all = ["project", "mcp_exe", "no_mcp", "serve_exe", "code_index_root", "register_root", "topic", "copy_from", "set_mode", "set_plan", "set_weekly_check", "set_backup", "set_recording", "set_language", "set_helpers", "set_temp_folder", "set_tools_folder", "uninstall"]
    )]
    tidy_undo: bool,

    /// Take THOR out again, step one (Claude Code may be running): removes only
    /// THOR's own connections from Claude Code's settings, THOR's note-saving
    /// connection, the /thor-eval command and git's global hooks folder when it
    /// is THOR's. It never removes git, Python, the Visual C++ runtime or any
    /// other program. Ends by saying to restart Claude Code. Needs --db.
    #[arg(
        long = "uninstall",
        conflicts_with_all = ["project", "mcp_exe", "no_mcp", "serve_exe", "code_index_root", "register_root", "topic", "copy_from", "set_mode", "set_plan", "set_weekly_check", "set_backup", "set_recording", "set_language", "set_helpers", "set_temp_folder", "set_tools_folder"]
    )]
    uninstall: bool,

    /// With --uninstall: step two, after Claude Code was closed and opened again.
    /// Refuses while a THOR program is still running, removes the saved copies
    /// of Claude Code's settings files, and keeps your memory unless you add
    /// --delete-memory. Ends by naming the program folder to delete.
    #[arg(long = "finish", requires = "uninstall")]
    finish: bool,

    /// With --uninstall --finish: also delete the memory and its side files
    /// (your safety copies in backup-repo stay). Without it the memory is kept.
    #[arg(long = "delete-memory", requires = "finish")]
    delete_memory: bool,

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

    /// With --copy-from: hold the whole notes memory, not a selection (every
    /// project, and the records of what fired: use it only on a machine you
    /// trust; the personal library file is never copied). Also read from
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
    let Some(db) = cli.db.clone().or_else(|| default_data_dir().map(|d| thor_core::app_data::resolve(&d.join("thor.db")))) else {
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

/// The git helpers: write them beside the memory and tell git to use them
/// (step 3 of a plain install, and `install --set-git-hooks on`). Only when we
/// know where the indexes live - without an index root there is nothing for a
/// refresh to update, and wiring a machine-wide git setting for nothing is
/// exactly the kind of surprise this installer must not spring. Someone who
/// only keeps notes about a topic may never have installed git, and does not
/// need it: say so, instead of reporting a failed git command.
fn wire_git_hooks(db: &Path, serve_exe: &Path, code_index_root: Option<&Path>) {
    if !githooks::git_available() {
        println!("= git is not installed on this computer. That is fine: it is only needed to keep programming projects searchable, not for topic folders.");
    } else if let Some(root) = code_index_root {
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
                                println!("+ told git to use them, so THOR can keep a programming project searchable after a commit, a merge or pull, a rebase and a branch switch");
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
}

/// Read `on` or `off` for a switch, or say plainly what is allowed (exit 2).
fn on_off(word: &str, what: &str, flag: &str) -> Result<bool, ExitCode> {
    match word {
        "on" => Ok(true),
        "off" => Ok(false),
        other => {
            eprintln!("{what} is 'on' or 'off', not '{other}'. Example: install {flag} off --db <your memory>. Nothing was changed.");
            Err(ExitCode::from(2))
        }
    }
}

/// The memory a switch changes: named with --db and really there.
fn switch_db(db: Option<PathBuf>, flag: &str, word: &str) -> Result<PathBuf, ExitCode> {
    let Some(db) = db else {
        eprintln!("name the memory to set this for: install {flag} {word} --db <your memory>");
        return Err(ExitCode::from(2));
    };
    if !db.is_file() {
        eprintln!(
            "REFUSED: there is no memory at {} - check the path given with --db (the one your assistant's hooks name). Nothing was changed.",
            db.display()
        );
        return Err(ExitCode::FAILURE);
    }
    Ok(db)
}

/// `install --set-connections on|off`: THOR's four entries in Claude Code's
/// settings file, and the recorded choice, and nothing else.
fn set_connections_only(db: Option<PathBuf>, settings: Option<PathBuf>, serve_exe: Option<PathBuf>, move_here: bool, word: &str) -> ExitCode {
    use ops::choices::{set, Choice};
    let on = match on_off(word, "the connection with Claude Code", "--set-connections") {
        Ok(on) => on,
        Err(code) => return code,
    };
    let db = match switch_db(db, "--set-connections", word) {
        Ok(db) => db,
        Err(code) => return code,
    };
    let Some(settings) = settings.or_else(default_settings_path) else {
        eprintln!("could not find your home folder to place Claude Code's settings - name the file with --settings. Nothing was changed.");
        return ExitCode::FAILURE;
    };
    if !on {
        match ops::uninstall::remove_thor_connections(&settings) {
            Ok(n) => println!("+ removed {n} of THOR's connection(s) from Claude Code's settings (nothing else there was touched, and the daily safety copy keeps running)"),
            Err(e) => {
                eprintln!("NOTHING WAS CHANGED: {e}");
                return ExitCode::FAILURE;
            }
        }
        if let Err(e) = set(&db, Choice::Connections, false) {
            eprintln!("the connections were removed, but your no could not be recorded: {e}");
            return ExitCode::FAILURE;
        }
        println!("+ recorded your no: a later install leaves the connections off. To switch them on again the assistant runs install --set-connections on");
        println!("Close Claude Code completely and open it again for this to take effect.");
        return ExitCode::SUCCESS;
    }
    let Some(serve_exe) = serve_exe.or_else(|| sibling("serve")).filter(|p| p.exists()) else {
        eprintln!("NOTHING WAS CHANGED: the program `serve` is not next to the installer. Unpack the whole download into one folder and run install from there.");
        return ExitCode::FAILURE;
    };
    if let Some(problem) = ops::install::launch_problem(&serve_exe) {
        eprintln!("NOTHING WAS CHANGED: {problem}");
        return ExitCode::FAILURE;
    }
    if let Some(refusal) = other_memory_refusal(&settings, &db, move_here) {
        eprintln!("{refusal}");
        return ExitCode::FAILURE;
    }
    if move_here {
        if let Err(e) = ops::uninstall::remove_thor_connections(&settings) {
            eprintln!("NOTHING WAS CHANGED: {e}");
            return ExitCode::FAILURE;
        }
    }
    let specs = standard_hooks(&serve_exe.display().to_string(), &db.display().to_string());
    match install_hooks(&settings, &specs) {
        Ok(report) => {
            let changed = report.results.iter().filter(|(_, o)| *o != HookOutcome::AlreadyPresent).count();
            if changed == 0 {
                println!("= THOR was already connected to Claude Code");
            } else {
                println!("+ connected THOR to Claude Code ({changed} of {} connections)", report.results.len());
            }
        }
        Err(e) => {
            eprintln!("NOTHING WAS CONNECTED to Claude Code: {e}");
            return ExitCode::FAILURE;
        }
    }
    if let Err(e) = set(&db, Choice::Connections, true) {
        eprintln!("the connections are on, but the record of your earlier no could not be cleared: {e}");
        return ExitCode::FAILURE;
    }
    println!("Close Claude Code completely and open it again for this to take effect.");
    ExitCode::SUCCESS
}

/// One THOR per computer account: when Claude Code is already connected to
/// ANOTHER memory that still exists, say so plainly (and the switch that moves
/// the connection), unless the person asked to move. `None` when the way is free.
fn other_memory_refusal(settings: &Path, db: &Path, move_here: bool) -> Option<String> {
    if move_here {
        return None;
    }
    let mine = db.display().to_string().replace('\\', "/").to_lowercase();
    let other = ops::uninstall::connected_memories(settings)
        .ok()?
        .into_iter()
        .find(|m| m.replace('\\', "/").to_lowercase() != mine && Path::new(m).exists())?;
    Some(format!(
        "REFUSED: Claude Code is already connected to another THOR memory ({other}), and one computer account has one. A second set of connections would make both speak at once. To use the memory {} instead, run this again with --move-to-this-store added: it moves the four connections here and the other memory is left as it is. Nothing was changed.",
        db.display()
    ))
}

/// `install --set-git-hooks on|off`.
fn set_git_hooks_only(db: Option<PathBuf>, serve_exe: Option<PathBuf>, code_index_root: Option<PathBuf>, word: &str) -> ExitCode {
    use ops::choices::{set, Choice};
    let on = match on_off(word, "THOR's helpers in git", "--set-git-hooks") {
        Ok(on) => on,
        Err(code) => return code,
    };
    let db = match switch_db(db, "--set-git-hooks", word) {
        Ok(db) => db,
        Err(code) => return code,
    };
    if on {
        let Some(serve_exe) = serve_exe.or_else(|| sibling("serve")).filter(|p| p.exists()) else {
            eprintln!("NOTHING WAS CHANGED: the program `serve` is not next to the installer. Unpack the whole download into one folder and run install from there.");
            return ExitCode::FAILURE;
        };
        let root = code_index_root.or_else(|| db.parent().map(|d| d.join("codeindex")));
        if let Err(e) = set(&db, Choice::GitHooks, true) {
            eprintln!("the record of your earlier no could not be cleared: {e}. Nothing was changed.");
            return ExitCode::FAILURE;
        }
        wire_git_hooks(&db, &serve_exe, root.as_deref());
        return ExitCode::SUCCESS;
    }
    let git = ops::uninstall::RealGit;
    let current = <ops::uninstall::RealGit as ops::uninstall::GitHooksPath>::current(&git);
    match current {
        Some(path) if ops::uninstall::hooks_path_is_thors(Some(&path), &db) => {
            if <ops::uninstall::RealGit as ops::uninstall::GitHooksPath>::unset(&git) {
                println!("+ told git to stop using THOR's helper folder ({path}); git itself stays as it was");
            } else {
                eprintln!("NOTHING WAS CHANGED: git would not let go of THOR's helper folder ({path}).");
                return ExitCode::FAILURE;
            }
        }
        Some(path) => println!("= git's helper folder ({path}) is not THOR's, so it was left alone"),
        None => println!("= git was not using THOR's helpers"),
    }
    if let Err(e) = set(&db, Choice::GitHooks, false) {
        eprintln!("git was changed, but your no could not be recorded: {e}");
        return ExitCode::FAILURE;
    }
    println!("+ recorded your no: a later install leaves them off. The code of a project is no longer refreshed after a commit, and git no longer stops video files or files above 25 MB. To switch them on again the assistant runs install --set-git-hooks on");
    ExitCode::SUCCESS
}

/// `install --set-reply-rules on|off`: the reply rulebook as a whole.
fn set_reply_rules_only(db: Option<PathBuf>, word: &str) -> ExitCode {
    use ops::choices::{set, Choice};
    let on = match on_off(word, "the reply rules", "--set-reply-rules") {
        Ok(on) => on,
        Err(code) => return code,
    };
    let db = match switch_db(db, "--set-reply-rules", word) {
        Ok(db) => db,
        Err(code) => return code,
    };
    let rulebook = serve::respond::default_rulebook_path(&db);
    let aside = rulebook.with_file_name("guard-response-rulebook.json.off");
    if on {
        if let Err(e) = set(&db, Choice::ReplyRules, true) {
            eprintln!("the record of your earlier no could not be cleared: {e}. Nothing was changed.");
            return ExitCode::FAILURE;
        }
        if !rulebook.exists() && aside.exists() {
            if let Err(e) = std::fs::rename(&aside, &rulebook) {
                eprintln!("could not put the reply rules back: {e}");
                return ExitCode::FAILURE;
            }
            println!("+ put your reply rules back as you had them");
        } else {
            match seed_response_rulebook(&db) {
                Ok(report) if report.outcome == RulebookOutcome::Written => println!("+ added the reply rules: {}", report.path.display()),
                Ok(_) => println!("= the reply rules were already there"),
                Err(e) => {
                    eprintln!("could not add the reply rules: {e}");
                    return ExitCode::FAILURE;
                }
            }
        }
        return ExitCode::SUCCESS;
    }
    if rulebook.exists() {
        let _ = std::fs::remove_file(&aside);
        if let Err(e) = std::fs::rename(&rulebook, &aside) {
            eprintln!("NOTHING WAS CHANGED: could not set the reply rules aside: {e}");
            return ExitCode::FAILURE;
        }
        println!("+ set the reply rules aside (your changes in them are kept in {})", aside.display());
    } else {
        println!("= there was no reply rules file");
    }
    if let Err(e) = set(&db, Choice::ReplyRules, false) {
        eprintln!("the rules were set aside, but your no could not be recorded: {e}");
        return ExitCode::FAILURE;
    }
    println!("+ recorded your no: a later install leaves the reply rules off. To switch them on again the assistant runs install --set-reply-rules on");
    ExitCode::SUCCESS
}

/// `install --set-helpers on|off`: store or retract the five notes about helper
/// assistants (the two of helper-agent mode and the three of the starting set
/// that are about helpers) and nothing else.
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

/// `install --set-starter-rules on|off`: store or retract the starter work
/// rules (`ops::install::starter_rule_notes`) and nothing else.
fn set_starter_rules_only(db: Option<PathBuf>, word: &str) -> ExitCode {
    use ops::install::{set_starter_rules, HelperChange};
    let on = match on_off(word, "the starter work rules", "--set-starter-rules") {
        Ok(on) => on,
        Err(code) => return code,
    };
    let db = match switch_db(db, "--set-starter-rules", word) {
        Ok(db) => db,
        Err(code) => return code,
    };
    let changes = match set_starter_rules(&db, on) {
        Ok(changes) => changes,
        Err(e) => {
            eprintln!("could not change the starter work rules: {e}");
            return ExitCode::FAILURE;
        }
    };
    let mut refused = false;
    let (mut stored, mut retracted, mut unchanged) = (0, 0, 0);
    for (id, change) in &changes {
        match change {
            HelperChange::Stored | HelperChange::Restored => stored += 1,
            HelperChange::Retracted => retracted += 1,
            HelperChange::AlreadyOn | HelperChange::AlreadyOff => unchanged += 1,
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
        println!("+ the starter work rules are on for {}: {stored} note(s) stored, {unchanged} were already there. Switch them off again with: install --set-starter-rules off --db \"{}\"", db.display(), db.display());
    } else {
        println!("+ the starter work rules are off for {}: {retracted} note(s) retracted (history still shows them), {unchanged} were already off. The notes THOR needs, the helper notes and the changelog notes stay. Switch them on again with: install --set-starter-rules on --db \"{}\"", db.display(), db.display());
    }
    ExitCode::SUCCESS
}

/// The question about a changelog and version numbers, printed for the
/// assistant when a git project gets its memory for the first time and has no
/// answer on record. Never at a folder that is not a code project.
fn print_changelog_question(db: &Path, key: &str, here: &Path) {
    if serve::project::git_root(here).is_none() || serve::changelog_flow::answer(db, key).is_some() {
        return;
    }
    // During the FIRST install the welcome carries the question (proposal 4),
    // so the assistant is not told to ask it now: the question stays pending
    // (no answer on record), and the safety net at the first commit stays.
    let first_session_pending = thor_core::event_store::EventStore::open_existing(db)
        .ok()
        .is_some_and(|store| model::store::show(&store, model::store::SETUP_NOTE_ID).is_ok());
    if first_session_pending {
        println!("+ the question about a changelog and version numbers for '{key}' is part of the welcome in the first session (proposal 4); nothing to ask now");
        return;
    }
    let intro = format!("+ ONE QUESTION FOR THE PERSON about '{key}' (asked once for this project):");
    println!("{}", serve::changelog_flow::ask_block(&intro, &serve::changelog_flow::install_command(), db));
}

/// `install --set-changelog-flow on|off`, from inside a code project: record
/// the answer for that project, and on `on` make sure its notes are in the memory.
fn set_changelog_flow_only(db: Option<PathBuf>, word: &str) -> ExitCode {
    use ops::install::{set_changelog_flow, FlowNoteChange};
    use serve::changelog_flow::Change;
    let on = match word {
        "on" => true,
        "off" => false,
        other => {
            eprintln!("the changelog flow is 'on' or 'off', not '{other}'. Example: install --set-changelog-flow on --db <your memory>");
            return ExitCode::from(2);
        }
    };
    let Some(db) = db else {
        eprintln!("name the memory to set this for: install --set-changelog-flow {word} --db <your memory>");
        return ExitCode::from(2);
    };
    if !db.is_file() {
        eprintln!(
            "REFUSED: there is no memory at {} - check the path given with --db (the one your assistant's hooks name). Nothing was changed.",
            db.display()
        );
        return ExitCode::FAILURE;
    }
    let here = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    serve::project::load_known_scopes(&db);
    let Some(top) = serve::project::git_root(&here) else {
        eprintln!("REFUSED: run this from inside the code project (a git folder) it is about. A folder that is not a code project is never asked. Nothing was changed.");
        return ExitCode::FAILURE;
    };
    let Some(key) = serve::project::resolve_project(&top) else {
        eprintln!("REFUSED: this folder has no project name to record the answer under. Nothing was changed.");
        return ExitCode::FAILURE;
    };
    let (change, notes) = match set_changelog_flow(&db, &key, on) {
        Ok(done) => done,
        Err(e) => {
            eprintln!("could not record the answer: {e}");
            return ExitCode::FAILURE;
        }
    };
    let mut refused = false;
    for (id, note) in &notes {
        match note {
            FlowNoteChange::Stored => println!("+ stored the note '{id}'"),
            FlowNoteChange::Refreshed => println!("+ rewrote the note '{id}' to the changelog flow (it still had the old starter text)"),
            FlowNoteChange::AlreadyThere => println!("= the note '{id}' was already there"),
            FlowNoteChange::LeftAlone(why) => println!("= left the note '{id}' as it is: {why}"),
            FlowNoteChange::Refused(why) => {
                refused = true;
                eprintln!("! could not store the note '{id}': {why}");
            }
        }
    }
    if refused {
        return ExitCode::FAILURE;
    }
    let again = match change {
        Change::Unchanged => " (this was already the answer; nothing changed)",
        _ => "",
    };
    if on {
        println!("changelog and version numbers are ON for '{key}'{again}: your assistant is reminded of the flow at a commit, a tag and a release. If the project has no CHANGELOG.md, create one ('# Changelog' and an 'Unreleased' section, nothing invented); if it has no version number, propose 0.1.0 to the person. Switch it off with: install --set-changelog-flow off --db \"{}\"", db.display());
    } else {
        println!("changelog and version numbers are OFF for '{key}'{again}: the flow notes are not shown in this project and it is never asked about again. Other projects are not affected. Switch it on later with: install --set-changelog-flow on --db \"{}\"", db.display());
    }
    ExitCode::SUCCESS
}

/// `install --set-temp-folder <folder|off>` and `--set-tools-folder <folder|off>`:
/// write the two folder names into `work-folders.json` beside the memory, and
/// nothing else. Every value is checked before anything is written, so a
/// refusal changes nothing.
fn set_work_folders_only(db: Option<PathBuf>, temp: Option<&str>, tools: Option<&str>) -> ExitCode {
    use serve::work_folders::{check_folder, path, set_temp_folder, set_tools_folder};
    let Some(db) = db else {
        eprintln!("name the memory to set this for: install --set-temp-folder <full path of the folder> --db <your memory>");
        return ExitCode::from(2);
    };
    if !db.is_file() {
        eprintln!(
            "REFUSED: there is no memory at {} - check the path given with --db (the one your assistant's hooks name). Nothing was changed.",
            db.display()
        );
        return ExitCode::FAILURE;
    }
    // None = switch off, Some = the checked folder.
    let mut chosen: Vec<(&str, Option<String>)> = Vec::new();
    for (flag, value) in [("--set-temp-folder", temp), ("--set-tools-folder", tools)] {
        let Some(value) = value else { continue };
        if value.trim().eq_ignore_ascii_case("off") {
            chosen.push((flag, None));
            continue;
        }
        match check_folder(value) {
            Ok(folder) => chosen.push((flag, Some(folder))),
            Err(why) => {
                eprintln!("REFUSED: {flag}: {why} Nothing was changed.");
                return ExitCode::FAILURE;
            }
        }
    }
    let file = path(&db);
    for (flag, folder) in &chosen {
        let written = match *flag {
            "--set-temp-folder" => set_temp_folder(&db, folder.as_deref()),
            _ => set_tools_folder(&db, folder.as_deref()),
        };
        if let Err(e) = written {
            eprintln!("could not write {}: {e}. Nothing more was changed.", file.display());
            return ExitCode::FAILURE;
        }
        match (*flag, folder) {
            ("--set-temp-folder", Some(folder)) => {
                let days = serve::work_folders::settings(&db).temp_max_age_days;
                println!("+ temp-until-live folder set to {folder} (written in {}).", file.display());
                println!("  At the start of a session THOR now tells your assistant, at most once a day, which production folders in it say \"STATUS: live\" (they may be deleted) and which are older than {days} days. THOR only reads the folder: it never deletes, moves or changes anything in it.");
                println!("  What each production folder needs: a name like 2026-10-20_shop_banner (date, repository, subject) and a STATUS.txt whose first line starts with \"STATUS:\". When the production is finished, change that line to \"STATUS: live\".");
                println!("  To change the {days} days, open {} in a text editor and change the number after \"temp_max_age_days\".", file.display());
                println!("  To switch this off again: install --set-temp-folder off --db \"{}\"", db.display());
            }
            ("--set-temp-folder", None) => println!("+ temp-until-live folder switched off: THOR no longer looks at it. Nothing in the folder was touched."),
            (_, Some(folder)) => {
                println!("+ tools folder set to {folder} (written in {}).", file.display());
                println!("  Every session now starts with a list of the helper tools in it, one line per sub-folder, taken from the first line of the README.txt (or README.md) in that sub-folder. Nobody has to keep the list: it is built from the folders.");
                println!("  A new reusable helper gets its own sub-folder there with a README.txt whose first line says in one sentence what it does. Script files written outside any git repository during a session are asked about once, 30 minutes after they were last changed: keep a useful one in the tools folder (a new version replaces the old tool), delete a throwaway one, and tell the owner about anything usable or in doubt (it is deleted only after the owner's own yes). THOR only reads the folder and deletes nothing.");
                println!("  To switch this off again: install --set-tools-folder off --db \"{}\"", db.display());
            }
            (_, None) => println!("+ tools folder switched off: no list, no check, no question about leftover scripts. Nothing in the folder was touched."),
        }
    }
    ExitCode::SUCCESS
}

/// `install --set-language <code>|off`: write the owner's chosen reply language
/// (or remove it) and nothing else.
fn set_language_only(db: Option<PathBuf>, word: &str) -> ExitCode {
    let word = word.trim().to_lowercase();
    let code = if word == "off" {
        None
    } else if serve::respond::language_name(&word).is_some() {
        Some(word.as_str())
    } else {
        let known: Vec<&str> = serve::respond::CHOOSABLE_LANGUAGES.iter().map(|(c, _)| *c).collect();
        eprintln!(
            "'{word}' is not a language THOR can check. Use one of: {}, or 'off'. Example: install --set-language nl --db <your memory>. Nothing was changed.",
            known.join(", ")
        );
        return ExitCode::from(2);
    };
    let Some(db) = db else {
        eprintln!("name the memory to set this for: install --set-language {word} --db <your memory>");
        return ExitCode::from(2);
    };
    if !db.is_file() {
        eprintln!(
            "REFUSED: there is no memory at {} - check the path given with --db (the one your assistant's hooks name). Nothing was changed.",
            db.display()
        );
        return ExitCode::FAILURE;
    }
    match serve::plan::set_reply_language(&db, code) {
        Ok(()) => {
            match code.and_then(serve::respond::language_name) {
                Some(name) => println!(
                    "reply language set to {name} ({word}) for {}: a reply meant for you that clearly reads as another language is sent back once to be rewritten in {name}. Short replies, code and quoted text are never judged. To switch it off: install --set-language off --db \"{}\"",
                    db.display(),
                    db.display()
                ),
                None => println!(
                    "reply language check switched off for {}: no language is checked any more. Your assistant still answers in the language you write in.",
                    db.display()
                ),
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("could not write the language: {e}");
            ExitCode::FAILURE
        }
    }
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
        eprintln!("NOTHING WAS CHANGED: the backup program (backup) is missing next to the installer. THOR is several small programs that must stay together in one folder: the assistant runs the install command again, which downloads and unpacks the whole set into one folder, then runs this again.");
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
        eprintln!("NOTHING WAS CHANGED: git is not installed on this computer, and the backup is a git folder. The install command installs git by itself: the assistant runs it again, then runs this again.");
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
        Ok(report) if report.results.iter().all(|(_, o)| *o == HookOutcome::Upgraded) => {
            println!("+ rewrote the backup's connection in the form Windows can always start; close Claude Code completely and open it again for that to take effect")
        }
        Ok(_) => println!("+ connected the backup to Claude Code: it runs when a session starts (at most one copy a day)"),
        Err(e) => {
            eprintln!("the backup folder is ready but connecting it to Claude Code failed: {e}");
            return ExitCode::FAILURE;
        }
    }
    match prove(&backup_exe, &db, &repo) {
        Ok(line) => {
            println!("+ first copy made now, to prove it works: {}", ops::backup_setup::plain_proof(&line, &repo));
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
/// The closing lines about restarting Claude Code. It reads THOR's connections
/// and its tool server only when it starts (and the tool server reads the code
/// index once, then), so a restart is asked for only when this run changed one
/// of those, or on the very first install; any other run says none is needed.
fn restart_lines(because: &[&str], code_read_now: bool) -> Vec<String> {
    if because.is_empty() {
        let mut lines = vec![
            "Done. Nothing is left for you to do: nothing that Claude Code loads when it starts was changed, so you do not need to restart it.".to_string(),
        ];
        if code_read_now {
            // The tool server looks up the code index once, when it starts.
            lines.push("Notes work at once. Searching this project's code starts the next time Claude Code is opened in this folder.".to_string());
        }
        return lines;
    }
    vec![
        "Done. ONE THING LEFT FOR YOU: close Claude Code completely and open it again.".to_string(),
        format!("Claude Code only learns about THOR when it starts, so until you do this nothing above is active ({}).", because.join("; ")),
    ]
}

fn existing_index_file_count(index_root: &Path, key: &str) -> anyhow::Result<usize> {
    let db = index_root.join(format!("{key}.db"));
    let store = codeindex::Store::open(&db).map_err(|e| anyhow::anyhow!(e))?;
    let files = store.file_count().map_err(|e| anyhow::anyhow!(e))?;
    Ok(usize::try_from(files).unwrap_or(0))
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
    serve::project::load_known_scopes(&db);
    let existing = serve::project::git_root(&here)
        // A broad folder an earlier install marked is not adopted: topic_of reports it.
        .or_else(|| serve::project::plain_root(&here).filter(|top| !model::checkout::is_too_broad_folder(top)))
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
        let Some(top) = top.filter(|t| serve::project::resolve_project(t).is_some_and(|r| serve::project::same_project_name(&r, &key))) else {
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
        ) => {
            println!("registered '{key}' -> {root}");
            print_changelog_question(&db, &key, &here);
        }
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
fn print_record_outcome(db: &Path, here: &Path, key: &str, outcome: Result<RegisterOutcome, RegisterRefusal>) {
    match outcome {
        Ok(RegisterOutcome::Created { root }) => {
            println!("+ recorded this project, '{key}', in the memory: notes about it can now be saved without any extra question");
            println!("+ registered its working copy: {root}");
            print_changelog_question(db, key, here);
        }
        Ok(RegisterOutcome::CreatedUnderRootRecord { root }) | Ok(RegisterOutcome::Registered { root }) => {
            println!("+ registered the working copy of '{key}': {root}");
            print_changelog_question(db, key, here);
        }
        Ok(RegisterOutcome::AlreadyHere { .. }) => println!("= project record already names this checkout"),
        Ok(RegisterOutcome::Moved { from, to }) => {
            println!("+ moved '{key}' from {from} to {to} (the old checkout is gone)")
        }
        Err(e) => println!("  ! could not record this project in the memory: {e}"),
    }
}

/// `install --tidy --plan <file>` and `install --tidy-undo`: see `ops::tidy`.
fn tidy_main(cli: &Cli) -> ExitCode {
    let Some(db) = cli.db.clone().filter(|d| d.is_file()) else {
        eprintln!("REFUSED: name the memory with --db (an existing thor.db). Nothing was changed.");
        return ExitCode::FAILURE;
    };
    let Some(home) = std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME")).ok().map(PathBuf::from) else {
        eprintln!("REFUSED: could not find your home folder. Nothing was changed.");
        return ExitCode::FAILURE;
    };
    serve::project::load_known_scopes(&db);
    let result = if cli.tidy_undo {
        ops::tidy::undo(&db, &home)
    } else {
        let plan = cli.plan.clone().unwrap_or_default();
        match std::fs::read_to_string(&plan) {
            Ok(text) => ops::tidy::tidy(&db, &text, &home),
            Err(e) => Err(format!("cannot read the plan {}: {e}. Nothing was changed.", plan.display())),
        }
    };
    match result {
        Ok(lines) => {
            for l in lines {
                println!("{l}");
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}

/// `install --uninstall [--finish [--delete-memory]]`: see `ops::uninstall`.
fn uninstall_main(cli: &Cli) -> ExitCode {
    let Some(db) = cli.db.clone().or_else(|| default_data_dir().map(|d| thor_core::app_data::resolve(&d.join("thor.db")))) else {
        eprintln!("NOTHING WAS CHANGED: could not work out which memory to remove - name it with --db");
        return ExitCode::FAILURE;
    };
    let (Some(settings), Some(claude_json)) = (cli.settings.clone().or_else(default_settings_path), cli.mcp_json.clone().or_else(default_user_mcp_path)) else {
        eprintln!("NOTHING WAS CHANGED: could not find your home folder to find Claude Code's files");
        return ExitCode::FAILURE;
    };
    let targets = ops::uninstall::Targets { settings, claude_json, eval_command: default_eval_command_path(), db };
    if cli.finish {
        let Some(bin_dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf)) else {
            eprintln!("NOTHING WAS CHANGED: could not work out where THOR's programs are");
            return ExitCode::FAILURE;
        };
        return match ops::uninstall::phase_two(&targets, &bin_dir, cli.delete_memory, &ops::uninstall::RealProcesses) {
            Ok(report) => {
                for line in report.plain_lines() {
                    println!("{line}");
                }
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("{e}");
                ExitCode::FAILURE
            }
        };
    }
    match ops::uninstall::phase_one(&targets, &ops::uninstall::RealGit) {
        Ok(report) => {
            for line in report.plain_lines() {
                println!("{line}");
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("NOTHING WAS FULLY REMOVED: {e}");
            ExitCode::FAILURE
        }
    }
}

/// The switch a run of the installer is, for the one neutral line a changed setting file gets
/// (`serve::mode_files`): `install --set-language` and its kin, else plain `install`.
fn switch_of(cli: &Cli) -> String {
    let switches: [(&str, bool); 15] = [
        ("--set-mode", cli.set_mode.is_some()),
        ("--set-plan", cli.set_plan.is_some()),
        ("--set-weekly-check", cli.set_weekly_check.is_some()),
        ("--set-recording", cli.set_recording.is_some()),
        ("--set-language", cli.set_language.is_some()),
        ("--set-helpers", cli.set_helpers.is_some()),
        ("--set-starter-rules", cli.set_starter_rules.is_some()),
        ("--set-changelog-flow", cli.set_changelog_flow.is_some()),
        ("--set-connections", cli.set_connections.is_some()),
        ("--set-git-hooks", cli.set_git_hooks.is_some()),
        ("--set-reply-rules", cli.set_reply_rules.is_some()),
        ("--set-temp-folder", cli.set_temp_folder.is_some()),
        ("--set-tools-folder", cli.set_tools_folder.is_some()),
        ("--set-backup", cli.set_backup.is_some()),
        ("--uninstall", cli.uninstall),
    ];
    switches.iter().find(|(_, on)| *on).map(|(name, _)| format!("install {name}")).unwrap_or_else(|| "install".to_string())
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    // Every setting file this run writes is recorded as the installer's, so the next session start names
    // it neutrally instead of warning about a change nobody asked for (`serve::mode_files`).
    let switch = switch_of(&cli);
    let db = cli.db.clone().or_else(|| default_data_dir().map(|d| thor_core::app_data::resolve(&d.join("thor.db"))));
    let before = db.as_deref().map(serve::mode_files::snapshot);
    let code = run(cli);
    if let (Some(db), Some(before)) = (db.as_deref(), before.as_ref()) {
        serve::mode_files::record_installer_change(db, &switch, before);
    }
    code
}

fn run(cli: Cli) -> ExitCode {
    if cli.uninstall {
        return uninstall_main(&cli);
    }
    if cli.tidy || cli.tidy_undo {
        return tidy_main(&cli);
    }
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
    if let Some(word) = cli.set_language.as_deref() {
        return set_language_only(cli.db, word);
    }
    if let Some(word) = cli.set_helpers.as_deref() {
        return set_helpers_only(cli.db, word);
    }
    if let Some(word) = cli.set_starter_rules.as_deref() {
        return set_starter_rules_only(cli.db, word);
    }
    if let Some(word) = cli.set_changelog_flow.as_deref() {
        return set_changelog_flow_only(cli.db, word);
    }
    if let Some(word) = cli.set_connections.as_deref() {
        return set_connections_only(cli.db.clone(), cli.settings.clone(), cli.serve_exe.clone(), cli.move_to_this_store, word);
    }
    if let Some(word) = cli.set_git_hooks.as_deref() {
        return set_git_hooks_only(cli.db.clone(), cli.serve_exe.clone(), cli.code_index_root.clone(), word);
    }
    if let Some(word) = cli.set_reply_rules.as_deref() {
        return set_reply_rules_only(cli.db.clone(), word);
    }
    if cli.set_temp_folder.is_some() || cli.set_tools_folder.is_some() {
        return set_work_folders_only(cli.db, cli.set_temp_folder.as_deref(), cli.set_tools_folder.as_deref());
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
    let db = match cli.db.or_else(|| data_dir.as_ref().map(|d| thor_core::app_data::resolve(&d.join("thor.db")))) {
        Some(p) => p,
        None => {
            eprintln!("could not work out where to keep the memory - name it with --db");
            return ExitCode::FAILURE;
        }
    };
    let code_index_root = cli
        .code_index_root
        .or_else(|| db.parent().map(|d| d.join("codeindex")));
    // This folder is filed under the spelling an existing project already has
    // (case, spaces and hyphens do not open a second project). The code index
    // below keeps the folder's own name: the git hook finds it by that name.
    serve::project::load_known_scopes(&db);

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
             programs that must stay together in ONE folder: the assistant runs the install command again, which \
             downloads and unpacks the whole set into one folder (install is never copied on its own). (For developers: build with `cargo build \
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

    // One THOR per computer account, refused BEFORE anything is written, so
    // "Nothing was changed" is literally true.
    if !ops::choices::is_off(&db, ops::choices::Choice::Connections) {
        if let Some(refusal) = other_memory_refusal(&settings, &db, cli.move_to_this_store) {
            eprintln!("{refusal}");
            return ExitCode::FAILURE;
        }
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
    // WHY CLAUDE CODE MUST BE RESTARTED, collected as the run goes: it loads
    // its hooks and its tool servers only when it starts, and the tool server
    // picks its code index once at its start. A run that changed none of
    // those, on a machine that was set up before, needs no restart.
    let mut restart_because: Vec<&str> = Vec::new();
    let mut code_read_now = false;
    match ensure_store(&db) {
        Ok(StoreOutcome::Created) => {
            restart_because.push("this is the first install");
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
            // The loose files that exist right now (paths only): a later "nothing to
            // order" is not believed for files that vanished behind the tidy step's back.
            if let Some(home) = serve::inventory::home_dir() {
                let _ = serve::inventory::snapshot_loose_now(&db, &home);
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
    if ops::choices::is_off(&db, ops::choices::Choice::ReplyRules) {
        println!("{}", ops::choices::respected_line(ops::choices::Choice::ReplyRules));
    } else {
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
    }

    // 1c. The end-of-session evaluation command. THOR holds a turn open for a
    // verdict on one served note at a time; this is how that debt gets
    // settled in bulk. Written regardless of --no-mcp (see
    // seed_eval_command's own doc comment for why). A copy that is already
    // there is brought up to date; one the owner changed is saved first.
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
                        println!("= the /thor-eval command was already there and is up to date ({})", report.path.display())
                    }
                    EvalCommandOutcome::Refreshed => println!(
                        "+ updated the /thor-eval command to the one that belongs to these programs; you had not changed the old one, so nothing was lost ({})",
                        report.path.display()
                    ),
                    EvalCommandOutcome::RefreshedKeepingCopy => {
                        let copy = report.backup.as_ref().map(|b| b.display().to_string()).unwrap_or_default();
                        println!(
                            "+ updated the /thor-eval command to the one that belongs to these programs ({}). The old one differed from what THOR wrote, so it was saved as {copy} - any changes you made yourself are in that file",
                            report.path.display()
                        )
                    }
                },
                Err(e) => println!("  ! could not add the /thor-eval command: {e}"),
            }
        }
        None => println!("  ! could not find your home folder to place the /thor-eval command"),
    }

    // 2. The hooks: what lets the memory speak at all. Not when the person
    // said no to them (`install --set-connections off`), and not beside the
    // hooks of ANOTHER memory (one THOR per computer account).
    let connections_off = ops::choices::is_off(&db, ops::choices::Choice::Connections);
    if connections_off {
        println!("{}", ops::choices::respected_line(ops::choices::Choice::Connections));
    } else if cli.move_to_this_store {
        match ops::uninstall::remove_thor_hooks(&settings) {
            Ok(n) if n > 0 => println!("+ took THOR's {n} old connection(s) out of Claude Code's settings, so they can point at this memory"),
            Ok(_) => {}
            Err(e) => {
                eprintln!("NOTHING WAS CHANGED: {e}");
                return ExitCode::FAILURE;
            }
        }
    }
    let specs = standard_hooks(&serve_exe.display().to_string(), &db.display().to_string());
    match if connections_off { None } else { Some(install_hooks(&settings, &specs)) } {
        None => {}
        Some(Ok(report)) => {
            let changed = report.results.iter().filter(|(_, o)| *o != HookOutcome::AlreadyPresent).count();
            if changed > 0 {
                restart_because.push("THOR's connections in Claude Code's settings changed");
            }
            if changed == 0 {
                println!("= THOR was already connected to Claude Code");
            } else {
                println!("+ connected THOR to Claude Code ({changed} of {} connections), so your memory speaks up by itself", report.results.len());
            }
            let upgraded = report.results.iter().filter(|(_, o)| *o == HookOutcome::Upgraded).count();
            if upgraded > 0 {
                println!("  rewrote {upgraded} older connection(s) in the form Windows can always start, with no shell in between; close Claude Code completely and open it again for that to take effect");
            }
            for old in &report.replaced {
                println!("  replaced an older connection of THOR's that pointed somewhere else, so there is only one set: {old}");
            }
            if let Some(p) = report.backup_path.as_ref().filter(|_| changed > 0) {
                println!("  your previous Claude Code settings were saved as {}", p.display());
            }
        }
        Some(Err(e)) => {
            eprintln!("NOTHING WAS CONNECTED to Claude Code: {e}");
            eprintln!("Your memory file was created. Fix the problem named above, then run install again.");
            return ExitCode::FAILURE;
        }
    }

    // 2b. A backup connection wired by an earlier install keeps its own
    // program, memory and folder; it is only rewritten in place in the form
    // Windows can always start (on Windows without Git Bash a shell-form line
    // never runs, so the daily copy would silently stop). Nothing is added.
    //
    // A reinstall from another program folder also MOVES the backup's connection
    // to this folder's backup program (same memory, same backup folder): left
    // pointing at the old folder it would fail silently, and the copies stop.
    if let Ok(existing) = ops::backup_setup::backup_hooks_for(&settings, &db) {
        let here = sibling("backup").filter(|p| p.exists());
        let specs: Vec<HookSpec> = existing
            .into_iter()
            .take(1)
            .map(|command| {
                let command = match (&here, ops::backup_setup::repo_of_command(&command)) {
                    (Some(exe), Some(repo)) => {
                        ops::backup_setup::hook_spec(&exe.display().to_string(), &db.display().to_string(), &repo).command
                    }
                    _ => command,
                };
                HookSpec { event: "SessionStart", matcher: None, command }
            })
            .collect();
        if !specs.is_empty() {
            match install_hooks(&settings, &specs) {
                Ok(report) if report.results.iter().any(|(_, o)| *o == HookOutcome::Replaced) => {
                    restart_because.push("the backup's connection in Claude Code's settings changed");
                    println!("+ moved the backup's connection to this folder's backup program, so the daily copy keeps running");
                }
                Ok(report) if report.results.iter().any(|(_, o)| *o == HookOutcome::Upgraded) => {
                    restart_because.push("the backup's connection in Claude Code's settings changed");
                    println!("  rewrote the backup's connection in the form Windows can always start; close Claude Code completely and open it again for that to take effect");
                }
                Ok(_) => {}
                Err(e) => println!("  ! could not check the backup's connection: {e}"),
            }
        }
    }

    // 3. The git hooks: what keeps the code index from drifting. Only when we
    // know where the indexes live - without an index root there is nothing for
    // a refresh to update, and wiring a machine-wide git setting for nothing is
    // exactly the kind of surprise this installer must not spring.
    // Someone who only keeps notes about a topic may never have installed git,
    // and does not need it: say so, instead of reporting a failed git command.
    if ops::choices::is_off(&db, ops::choices::Choice::GitHooks) {
        println!("{}", ops::choices::respected_line(ops::choices::Choice::GitHooks));
    } else {
        wire_git_hooks(&db, &serve_exe, code_index_root.as_deref());
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
                    ServerOutcome::Added => {
                        restart_because.push("the note-saving program was registered");
                        println!("+ gave your assistant a way to save notes (registered in {})", path.display())
                    }
                    ServerOutcome::AlreadyPresent => println!("= your assistant could already save notes"),
                    ServerOutcome::Replaced => {
                        restart_because.push("the note-saving program was registered");
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
    // The code index is named by the folder itself, not by the project it is
    // filed under: the git hook that refreshes it has no memory to ask.
    let index_key = serve::project::folder_own_name(&here).or_else(|| key_here.clone()).unwrap_or_default();
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
        Some(key) => match code_index_root.as_deref().map(|r| build_project_index(r, &index_key, &here)).transpose() {
            Ok(Some(Some(files))) => {
                // Not a reason to restart Claude Code: only its connections, the
                // note-saving program and the programs themselves are loaded at
                // its start. Searching the code of THIS folder starts answering
                // the next time Claude Code is opened in it.
                println!("+ read this project's code, {files} file(s), so it can be searched and kept fresh after a commit, a merge or pull, a rebase and a branch switch");
                println!("  searching this project's code starts working the next time Claude Code is opened in this folder");
                code_read_now = true;
                // Immediately after the read succeeds: opens the project in
                // the memory itself, so the very first `remember` filed under
                // it is never refused as an unknown collection (see
                // `record_project_opened`'s own doc comment for the defect
                // this closes). Idempotent - a rerun that hits this same
                // branch (the code index was rebuilt, but the record already
                // exists) writes nothing more.
                print_record_outcome(&db, &here, &key, record_project_opened(&db, &here, &key, files));
            }
            Ok(Some(None)) => {
                println!("= this project's code was already read; a commit, a merge or pull, a rebase and a branch switch each keep it fresh");
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
                    match existing_index_file_count(root, &index_key) {
                        Ok(files) => print_record_outcome(&db, &here, &key, record_project_opened(&db, &here, &key, files)),
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
            println!("  To give one folder (one subject, like 'Kitchen renovation') its own memory, the assistant runs this inside that folder:  \"{installer}\" --topic");
        }
    }

    println!();
    for line in restart_lines(&restart_because, code_read_now) {
        println!("{line}");
    }
    // For the assistant, not for the person: they never run anything themselves.
    let bin_dir = serve_exe.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| PathBuf::from("."));
    println!("FOR THE ASSISTANT (not for the person to read out or run): afterwards, check the setup yourself with THOR's health check, the program doctor in {} with --db {} added. Read its first lines to the person in your own words.", bin_dir.display(), db.display());
    ExitCode::SUCCESS
}

#[cfg(test)]
mod restart_tests {
    use super::restart_lines;

    #[test]
    fn a_restart_is_asked_for_only_when_something_a_restart_loads_changed() {
        let none = restart_lines(&[], false).join("\n");
        assert!(none.contains("do not need to restart"), "{none}");
        assert!(!none.contains("ONE THING LEFT") && !none.contains("Searching this project's code"), "{none}");
        let read = restart_lines(&[], true).join("\n");
        assert!(read.contains("do not need to restart") && read.contains("Searching this project's code starts the next time"), "{read}");
        let some = restart_lines(&["THOR's connections in Claude Code's settings changed"], true).join("\n");
        assert!(some.contains("ONE THING LEFT FOR YOU: close Claude Code completely"), "{some}");
        assert!(some.contains("connections in Claude Code's settings changed"), "{some}");
    }
}
