//! THE FIRST-SESSION QUESTIONNAIRE: everything the install arranges or switches
//! on, numbered, each in plain user language - what it is, WHY it exists, what
//! THOR chose, and how to say no. One catalogue, rendered twice: into the
//! lookup item `first-session-questions` that the seeded setup note points the
//! assistant at, and into the "First session with a new owner" section of
//! `AGENTS.md` (a test holds the page equal to the render).
//!
//! WHY A CATALOGUE AND NOT PROSE IN TWO PLACES. The assistant must be able to
//! push back on a bad answer ("no, switch it all off") by stating, once, the
//! real reason the thing exists, then respect the choice. That reason has to be
//! somewhere the assistant can read at the moment it is needed, and it must not
//! drift from the page a person reads. So every item carries its `why` here,
//! in one place, and both surfaces are rendered from it. The starter work rules
//! are listed from `install::workflow_starter_pack()` itself, so a note added
//! to the pack appears in the list without anyone remembering to add it.
//!
//! LAYERED, BECAUSE FOUR PARTS PER ITEM OVERWHELMED. The owner found a ten-item
//! list with what, why, default and how-to-say-no under every item too much to
//! read. So each rendering has two layers:
//!
//! 1. What the USER is shown (`render_list`): an intro of two lines, then ONE
//!    line per item (what it is, and the default), then how to answer. Nothing
//!    else.
//! 2. What the ASSISTANT keeps (`ASSISTANT_RULES` and `render_detail`): the full
//!    what, why, default and way to say no per item. The assistant states an
//!    item's why once, only when the user asks about that number or refuses it,
//!    and then respects the choice.
//!
//! NON-BLOCKING BY DESIGN. The defaults are applied first, in the same turn;
//! the list is then shown with "tell me by number what you want different".
//! Nothing waits for an answer. A bad answer is answered with the reason once
//! and the user's final word is respected (`ASSISTANT_RULES`).
//!
//! THE LIST MUST NOT CLAIM WHAT THE INSTALL DOES NOT DO. A fresh install wires
//! no backup, so item 10 says nothing is backed up until the user answers (a
//! test in `ops/tests/install_backup_and_recording.rs` holds that the standard
//! hooks carry none, and another that every `--set-...` command the list names
//! exists in the installer).

use crate::install::workflow_starter_pack;
use serve::plan::{Detection, Plan};
use std::path::PathBuf;

/// The id and the key of the lookup item that carries the rendered list.
pub const LOOKUP_ID: &str = "first-session-questions";

/// The notes that make up helper-agent mode (question 6). They are not in the
/// workflow pack (`install::helper_mode_notes`): a new memory stores
/// them too (the mode is on by default) and `install --set-helpers on|off`
/// stores or retracts them. The whole workflow pack
/// is the plain work rules (question 5).
pub const HELPER_MODE_NOTES: &[&str] = &["orchestrator-mode-saves-quota", "an-agent-brief-stands-alone"];

/// Where the rendered list sits inside `AGENTS.md`, so a test can cut it out.
pub const BEGIN_MARKER: &str = "<!-- questionnaire:begin -->";
pub const END_MARKER: &str = "<!-- questionnaire:end -->";

/// One item of the list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Question {
    pub n: usize,
    /// A stable word for the assistant ("plan", "hooks", ...).
    pub id: &'static str,
    pub title: &'static str,
    /// The ONE line the user is shown: what it is, and the default. It may
    /// carry `{PLAN_SHORT}`, which `render_list` fills in.
    pub line: String,
    /// What it is, in plain words (the assistant's detail).
    pub what: String,
    /// WHY it exists: the real reason or the evidence, one sentence.
    pub why: &'static str,
    /// What THOR chose.
    pub default: &'static str,
    /// How to say no, and what that changes.
    pub say_no: String,
}

/// The plain name of a starter note: its id with the hyphens taken out.
pub fn plain_name(id: &str) -> String {
    id.replace('-', " ")
}

/// The one-line list of the plain work rules (question 5), derived from the
/// workflow pack so it can never be out of date.
pub fn work_rule_ids() -> Vec<String> {
    workflow_starter_pack().into_iter().map(|n| n.id).collect()
}

/// The plain work rules as one line of names.
pub fn work_rule_line() -> String {
    work_rule_ids().iter().map(|id| plain_name(id)).collect::<Vec<_>>().join("; ")
}

/// A small count as a word, so the reply-rules item follows the shipped
/// rulebook instead of a number typed twice. Above twelve the digits stand.
fn count_word(n: usize) -> String {
    const WORDS: [&str; 13] =
        ["no", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten", "eleven", "twelve"];
    WORDS.get(n).map_or_else(|| n.to_string(), |w| (*w).to_string())
}

/// The whole list, in the order the user sees it.
pub fn questions() -> Vec<Question> {
    let rules = work_rule_ids();
    let teaching = crate::install::working_contract().len() - 1;
    let ids = rules.join(", ");
    let reply_rules = count_word(crate::install::response_rulebook_rule_count());
    let list = vec![
        Question {
            n: 1,
            id: "plan",
            title: "Your plan",
            line: "Your Claude plan: I noticed which Claude subscription you have ({PLAN_SHORT}). That way THOR's weekly self-check does not use up your usage allowance. Now: taken from Claude's settings.".to_string(),
            what: "I looked at Claude Code's own settings to find out which plan you have: Max, or Pro and smaller. Pro and any plan below Max count as \"small\" (install --set-plan small); Max counts as \"max\" (install --set-plan max). {PLAN_LINE} Never ask him about it and never mention it unless he asks.".to_string(),
            why: "THOR checks itself on a schedule, and that check draws on the same usage limit as your own work, so on a smaller plan it runs once a week at most and never daily, which keeps your limit for you; on Max it also runs daily while something is found wrong.",
            default: "The plan the settings say; small (Pro or smaller) when they do not say, until you say otherwise.",
            say_no: "If he says the plan is wrong, I record the right one, so the schedule follows what he really pays for.".to_string(),
        },
        Question {
            n: 2,
            id: "hooks",
            title: "The connections to Claude Code",
            line: "Link to Claude: THOR is connected to Claude Code, the program you talk to me in. So the memory speaks up by itself when a conversation starts, before I take an action, and when I finish an answer. Now: on.".to_string(),
            what: "THOR is wired into Claude Code at four moments: when a session starts, before each tool call, when you send a message, and when a reply ends. That is how the memory speaks up by itself and how replies get checked. A backup copy of Claude Code's settings file was made first.".to_string(),
            why: "A memory that must be asked for stays silent, and a missing connection fails without any message: the memory simply never says a word.",
            default: "On.",
            say_no: "Then THOR is only a notebook your assistant searches when you ask. I remove only THOR's four hook entries (SessionStart, PreToolUse, UserPromptSubmit and Stop, the ones whose command runs the serve program with hook at the end) from {SETTINGS}, and touch no other hook there; the backup copy stays next to it. The thor tool server in {CLAUDE_JSON} stays, so notes can still be searched and saved. It takes effect after you restart Claude Code. This also ends the weekly check, the reply rules and the bug offers, since they all run through those connections.".to_string(),
        },
        Question {
            n: 3,
            id: "git-hooks",
            title: "The git hooks in every repository",
            line: "Code tracking: for software projects, THOR keeps its map of your code current. It also stops video files or files over 25 MB being saved by mistake. Now: on.".to_string(),
            what: "THOR set one git setting for the whole machine so that every repository refreshes THOR's code index after a commit, a merge or pull, a rebase and a branch switch. The same setting also makes git refuse to commit video files (mp4, mov, mkv, webm, avi, m4v) and files above 25 MB. That check is settable for the whole machine or for one repository (git settings thor.mediagate.maxbytes and thor.mediagate.extensions), and one file can be allowed on purpose with a line '<path> thor-media-ok' in .gitattributes. Each repository's own git hooks still run; if another tool already owned that setting, THOR left it alone, and then neither the refresh nor the check is active.".to_string(),
            why: "An index three days behind made 83 percent of the 'where is this used' answers name a wrong line while a fresh one made none wrong (measured at the maker, 2026-08-15), and a 26.7 MB 4K video plus slides got committed and pushed with a plain git add of everything in one real repository on 2026-10-20, which a check on typed commands cannot see because it never shows what such an add stages.",
            default: "On (the video and size check included), unless something else already owned the setting.",
            say_no: "I undo all of it with one git command (git config --global --unset core.hooksPath): searching the code then goes stale after a commit, a merge or pull, a rebase and a branch switch, and the video and size check stops too. To turn off only the video and size check I run git config --global thor.mediagate.enabled false (without --global, inside one repository, for that repository only).".to_string(),
        },
        Question {
            n: 4,
            id: "weekly-check",
            title: "The weekly check",
            line: "Weekly tidy-up: about once a week I tidy THOR's memory for you. You can also ask me to do it at any time. Now: on.".to_string(),
            what: "About once a week per project (daily on Max while something is found wrong) THOR asks your assistant to run a short tidy-up of the memory before it finishes a turn: judge the notes that fired, repair stale ones and file a report. The command /thor-eval was added so you can also run it yourself. The ask runs through the connections of item 2, so it only happens while those are on.".to_string(),
            why: "A memory nobody maintains fills with stale notes and quietly stops helping, and a check that depends on someone remembering it does not happen.",
            default: "On, at the rhythm of question 1.",
            say_no: "I switch the weekly ask off (install --set-weekly-check off); you can still ask me for it any time (I run /thor-eval), and nothing else changes.".to_string(),
        },
        Question {
            n: 5,
            id: "work-rules",
            title: "The starter work rules",
            line: format!(
                "Work habits: {} good working habits are stored as reminders I follow. For example, I check my work before I say it is done. I never save a password where others can see it. They only remind me. Now: all on.",
                rules.len()
            ),
            what: format!(
                "A set of {} general work habits, stored as notes that arrive right before the step they are about, once per session: {}. {teaching} more notes teach your assistant how to write notes that come back and how to work with THOR. None of them blocks a normal step; on Windows two of them, about hidden console windows, refuse the one call that would flash a window.",
                rules.len(),
                work_rule_line()
            ),
            why: "They are the habits that were marked useful again and again in a memory built over months of real work, so a new memory starts with that experience instead of learning it the hard way.",
            default: "All on.",
            say_no: format!("Name the ones you do not want, or say none: I remove just those notes ({ids}), and a removed note simply stops arriving. If a note refuses to be retracted because its check bites (the two Windows notes about hidden console windows can), I first clear its check with a revise that says why, then retract it."),
        },
        Question {
            n: 6,
            id: "helper-agents",
            title: "Helper assistants",
            line: "Helper assistants: I can hand big jobs to cheaper helper assistants that work in the background. That can cost less and keeps our conversation free. Now: on - say \"helpers off\" to stop.".to_string(),
            what: "When it is on, your main assistant (the strongest model) answers, judges and checks, while cheaper models do the real work in helper assistants, one helper per task. No helper is used for a question or an answer, a trivial one-step job, work that needs this conversation's context or quick back-and-forth with you, or a check of a helper's own result: the main assistant does those itself. Tests are run narrowly while working and in full once at the end, and a helper that goes quiet is judged dead after about three checks. Why you may not want it: you do not see the work happen and cannot steer a helper halfway; each helper starts with a fixed context cost, so small tasks cost more, not less; a helper knows nothing of the conversation beyond its brief; parallel helpers can collide in the same files; and checking the result costs an extra turn. It is on in a new memory: the two notes that carry it (orchestrator-mode-saves-quota and an-agent-brief-stands-alone) are stored. To switch it off, say \"helpers off\" and I run: install --set-helpers off --db <store>; to switch it on again, install --set-helpers on --db <store>.".to_string(),
            why: "Measured at the maker, helpers were likely 12 to 62 percent cheaper than doing everything in the main conversation, and the chat stays free and answers at once while they work, instead of you waiting 5 to 15 minutes; it is on by default because that holds for big tasks, and you can switch it off.",
            default: "On.",
            say_no: "Say \"helpers off\" and I run install --set-helpers off --db <store>, which retracts the two notes that carry it (orchestrator-mode-saves-quota and an-agent-brief-stands-alone); your assistant then does the work itself in the main conversation. To get it back, install --set-helpers on --db <store>.".to_string(),
        },
        Question {
            n: 7,
            id: "bug-reports",
            title: "Reporting THOR's own bugs",
            line: "Reporting faults: if THOR finds a fault in itself, I may offer to report it to its makers. I always show you the exact text first, and nothing is sent without your yes. Now: on.".to_string(),
            what: "Sometimes THOR finds a fault in itself. Your assistant may then offer to report it to the makers on GitHub. It shows you the exact full text first; file paths, project and folder names, computer and user names, addresses, keys and passwords are removed; nothing is sent without your yes, and saying no costs nothing. The offer runs through the connections of item 2, so it only happens while those are on.".to_string(),
            why: "THOR sees faults in itself that nobody reading a report would notice, and the makers can only fix what they hear about.",
            default: "On: it may ask you, once per fault, and sends nothing without your yes.",
            say_no: "I switch it off (install --set-mode off): nothing is offered and nothing is sent, ever.".to_string(),
        },
        Question {
            n: 8,
            id: "replies",
            title: "Language, reply length and reply rules",
            line: format!("Language and replies: I talk to you in the language you write in and keep my answers short, with a summary first. I also follow {reply_rules} reply rules that my answers are checked against. Now: on."),
            what: format!("Two things. First, the language: you can choose the one your assistant talks to you in (nl, en, de, fr or es). I set it automatically to the language you write in (when it is one of those five) and the first reply says so; a reply meant for you that clearly reads as another language is then sent back once to be rewritten in yours, also right after a machine message such as a helper's report. Short replies, code, file names and quoted text are never judged, and a reply the detector is unsure about is left alone. If you say it is the wrong language, I switch it. Second, the shape: your assistant answers short, with a plain-language summary first. A small file next to the memory holds {reply_rules} reply rules (a summary first, no reflexive disclaimers, check before asking you to look something up, keep an answer under about 1000 characters, show evidence when saying something was checked, and never say everything was checked without saying what was covered); a reply that breaks one is sent back once to be rewritten. A summary or overview you ask for may run longer."),
            why: "Replies drift into English right after English machine input in real sessions of the maker (measured 2026-11-18), so a written rule is not enough and the choice is checked at the end of every reply, while the reply rules keep a reply short and to the point and were checked against about 6,600 real replies with their false triggers removed.",
            default: "The language you write in, set automatically and checked; short replies and all the reply rules on.",
            say_no: "Name the language you want and I run install --set-language <code> --db <store> (for example nl); to stop checking it again, install --set-language off --db <store>. For the length or a single reply rule, tell me and I change the file or remove just that rule.".to_string(),
        },
        Question {
            n: 9,
            id: "topic-folders",
            title: "Folders for a topic",
            line: "Folders for a subject: an ordinary folder without code, such as recipes or travel plans, can get a memory of its own. Just tell me which folder. Now: none yet.".to_string(),
            what: "A plain folder with no code in it, such as a folder of recipes or travel plans, can have a memory of its own: I run one command inside it and it gets one small hidden file. Notes about that folder then come back only when you work there. Nothing is set up until you name a folder, except one offer after the welcome: when you worked with Claude before, I look at those earlier conversations and the files they left loose (home folder, Desktop, Documents, Downloads), propose one fixed folder per subject with its own memory, and only after your yes move the files into it and keep what the old conversations taught. Nothing is ever deleted, files of a programming project stay where they are, and every move can be put back (install --tidy-undo).".to_string(),
            why: "Notes about one subject stay out of the way of the others, so nothing from your recipe folder interrupts your code.",
            default: "No folders are set up yet.",
            say_no: "Nothing to undo: do not name a folder. A folder you set up has one hidden file, .thor-project, that you can delete.".to_string(),
        },
        Question {
            n: 10,
            id: "backups",
            title: "Backups of the memory",
            line: "Safety copy: NOTHING is backed up until you choose. You can choose a copy on this machine. Or a place you name: a network drive (a NAS), another computer or a private online storage place. Or none. Now: none yet.".to_string(),
            what: "A backup copy of the whole memory in a git folder. A fresh install makes NO backup, so until you answer nothing is copied anywhere. You choose one of three: a copy on this machine (a git folder beside the memory; nothing leaves the machine, and it protects against a damaged or wrongly edited memory, not against the disk dying), a place you name (a folder on a NAS or another computer you own, or a private repository; that survives the disk dying), or none. Once you choose, THOR copies when a session starts, at most once a day, and makes the first copy at once to prove it works. To start, say \"backup local\", or name the place.".to_string(),
            why: "The memory is a single file, and if the disk dies everything it learned dies with it.",
            default: "None: nothing is backed up until you answer.",
            say_no: "Choose none, or simply do not answer: nothing is copied anywhere. If you change your mind later I run install --set-backup off, which removes only the connection that makes the copies; the copies already made stay where they are.".to_string(),
        },
        Question {
            n: 11,
            id: "recording",
            title: "A record of real calls",
            line: "Record of my work: for 30 days, only on this machine, THOR keeps the instructions I carried out and my answers. Passwords and secret codes are hidden. This helps to find its own rules that wrongly stop normal work. Now: on.".to_string(),
            what: "THOR keeps, in two files next to the memory, the commands your assistant ran and the replies it gave in the last 30 days. The files stay on this machine and are never sent anywhere. Passwords and keys THOR recognises are blanked out before anything is written, and every line older than 30 days is deleted by itself. THOR then replays its rules against that record to find a rule that would wrongly block you.".to_string(),
            why: "Replaying 146,000 real calls found 12 rules that blocked calls which were fine, and no review of the reports had shown them (measured at the maker, 2026-10-09).",
            default: "On.",
            say_no: "I switch it off (install --set-recording off): nothing new is recorded and the check for wrongly blocking rules then says it has no data; what was already recorded disappears by itself after 30 days.".to_string(),
        },
        Question {
            n: 12,
            id: "changelog",
            title: "A changelog and version number",
            line: "Change log: in a software project that has none, I create CHANGELOG.md, a file that lists each change, and keep it up to date. If you say no, I do not create it. Now: yes, in such projects.".to_string(),
            what: "A changelog is a file, CHANGELOG.md, with one line for every change you can see, and a version number says which state of the project is running. Only in a project that is a git repository and has none do I create the file and add a line with each change from then on; a project that already keeps one is left as it is, and a folder that is not a git repository gets none.".to_string(),
            why: "Every later session, and you, know exactly what changed and which version runs, without digging through history.",
            default: "Yes, in a git project that has none.",
            say_no: "Then I create no file and do not ask again; the starter work rule about changelogs (item 5) stays, and it only applies to a project that keeps one.".to_string(),
        },
    ];
    debug_assert!(list.iter().enumerate().all(|(i, q)| q.n == i + 1));
    list
}

/// What the assistant does with the answers. Not read out to the user.
pub const ASSISTANT_RULES: &str = "\
FOR THE ASSISTANT (do not read this part out)
- This text is the whole job; you need nothing else. In your first reply, in this order: say the welcome of SAY FIRST (in the person's own language, warm and plain, every point kept, nothing technical added), make THE TWO CALLS, and apply the defaults. Every call, command and path you need is written out below (see PATHS ON THIS MACHINE), so never ask him for a path or a command. If the THOR tools remember and retract are not loaded in this session yet, load them first (in Claude Code: ToolSearch with select:mcp__thor__remember,mcp__thor__retract).
- Apply the defaults NOW, in this same turn, without waiting for an answer, so nothing waits: store the Report 'owner-setup-answers' with them and retract the setup note. Never ask the same question twice and never hold up his work for an answer.
- The welcome is ONE question over three numbered proposals. Apply nothing of proposals 1 to 3 before he answers, except that meanwhile you speak the language he writes in. His answer is ONE reply: a plain \"yes\" (or any plain agreement) applies all three proposals; a number with a new value (for example \"2 English\" or \"3 figure it out yourself\") changes only that item; a number he leaves out keeps its proposal. Then revise 'owner-setup-answers' with what was applied. Every other default is applied at once, in this same turn, and never mentioned.
- Proposal 1 (a daily safety copy): when applied run install --set-backup local --db <store> and say the result in one plain sentence (it makes a copy on this computer and a first copy at once to prove it works; if it failed, say so and why in plain words, and when git is missing say that the copy needs git). Until it is applied nothing is backed up.
- Proposal 2 (the language): <language> is the language he writes in. When applied and it is nl, en, de, fr or es, run install --set-language <code> --db <store> and say in one sentence that it is set; for any other language set nothing and say plainly that it cannot be checked but that you will still write in it. A number 2 with another language switches to that one by the same rule.
- Names for a person: name <folder> by its real name on this computer (for example \"Documenten\" on a Dutch Windows, not \"Documents\"), and in the person's language call a session \"a conversation\".
- Proposal 3 (the folder): <folder> is your proposal. Propose the folder this session runs in when it is a specific folder; never the home folder, the Documents, Desktop or Downloads folder itself, a drive root, or anything under AppData or Program Files. Otherwise propose a new folder inside Documents, named after what the person said they work on, or \"My work\" in their language. When applied: when the folder is not a git repository, create it if needed and run the installer with --topic from inside it (install --topic --db <store>); when it is a git repository it is already a project (run install --db <store> from inside it if it is not registered). Say the result in one plain sentence. On \"3 figure it out yourself\": choose by the same rule, create and register it the same way, and say in one plain sentence which folder it is. On \"3\" plus a folder: use that folder by the same rule; a home, Documents, Desktop, Downloads or drive root answer gets a subfolder inside it proposed instead, in one sentence. The person only says yes or no: they never open or pick a folder. Whenever the folder is not the one this session runs in, and you have the Claude desktop app tools, move this conversation there yourself: call mcp__ccd_directory__request_directory with the folder path, then mcp__ccd_directory__change_directory with the same path, and rename the conversation with mcp__ccd_session_mgmt__set_session_title (the word self means this conversation). The move takes effect at the end of the turn and was seen to be late or not to happen at all, so at the first tool call of your next turn check the working folder (for example pwd); if it did not move, call change_directory once more; if it still did not move, prepare a new conversation in that folder with mcp__ccd_session__spawn_task (cwd and title) and tell the person in one plain sentence to click the card. A resumed conversation can come back in the old folder: move it again the same way. Without these tools (a terminal), give plain click steps for opening Claude Code in that folder. Never mention git, topic markers, scopes or registration to the person.
- THE TIDY STEP (right after his answer to the welcome was applied, in the same session, as a SECOND message; not part of the first reply): put the work he did with Claude before in order, so that new files stop landing loose everywhere. Run `serve --db <store> inventory` (the serve program sits next to the install program; it only reads, except that it writes THOR's own record when it finds nothing to order). When it says there is nothing to order (no earlier sessions, and no loose files in the home folder, Desktop, Documents or Downloads), say NOTHING about this and stop. Otherwise read what you need: the listing names a transcript per session, and `serve --db <store> inventory --digest <transcript>` prints one session as short plain text with no secrets; read digests only as far as you need to know what a session was about. Group the sessions and loose files BY SUBJECT (you judge: CV and job hunting, a renovation, a trip ...); sessions that ran in a git repository or in a folder that already has its own memory are already in order and are left out. The listing's heading CONVERSATIONS WITHOUT A FOLDER holds the conversations he had in the Claude desktop app with no folder chosen (they ran in a temporary workspace of the app): group them by subject like the others, move their files to the subject folder, and propose the files marked AT RISK first; tell him in plain words that those files were in a temporary place of the app, which may clean it up, and are now safe in his own folder. A conversation marked NOTHING TO MOVE left no file that still exists: it does not stand in the way, but you may still store what it taught. Then say, in his language, plainly, in ONE message, with the real folder names of his computer: \"I also looked at what you did with Claude before. Here is my proposal to put it in order: 1. <Subject>: a fixed folder <real folder name, for example Documenten\\Cv en werk zoeken>. I put <n> files in it (<a few names>), and I remember what was in <n> earlier conversations, for example <one or two concrete facts>. 2. ... Shall I do it like this? Just say \"yes\". Don't want one of them? Say for example \"2 no, because ...\".\" Leave out the 'I put files in it' part for a subject that has no files. Never ask him to type, paste, run or install anything, and never use the words git, memory store, scope, topic marker or plan. Show the proposal in the message right after the one in which you ran `serve inventory` (without --digest), as a numbered list that starts with a line `1.`; THOR recognises the proposal from the record of the session (the first message with visible text after that inventory call that holds such a list), so add nothing technical to it and do not show a numbered list to him before the inventory ran. THOR HOLDS THE SESSION until this step is settled (the pinned note `order-earlier-work-once` carries it, and you cannot retract, rewrite or unpin that note yourself): the end of every turn is held until it is settled, except the turn that ends on the proposal itself while you wait for his answer. It is settled ONLY by what THOR checks itself, never by your word: (1) `serve --db <store> inventory` finds nothing to order (it writes that record itself; run it plainly, with no folder options, because a folder you name proves nothing); (2) `install --tidy` ran AFTER he answered the proposal (also when every file already lay in its folder and nothing had to move); (3) for what he refused, you file a Report with id `order-earlier-work-answer` (kind report, no project) whose text quotes, exactly and between double quotes, ONE WHOLE message he typed after the proposal (not a piece of it; any words, any language: THOR keeps no list of refusal words, it only checks that the quote really is in a message of his; a helper's text, a hook's text and your own words do not count); a quote that names item numbers refuses only those items, any other quote refuses all of it. A question of his is not his answer: his answer is the first message after the proposal that is not a question (unless the tidy ran right after a message of his that ends in a question mark: then that message was his answer). A tidy run only counts when at least one folder of its plan was accepted (a drive root, the home folder, Documents, a git repository or a folder outside the home folder leave no record), so when every folder of a plan was refused, make a plan with a real subject folder (a new folder inside Documents, named after the subject) or, if he refused it all, file the Report that quotes his whole answer. A loose file that was there when THOR was installed and has vanished without a tidy run keeps the inventory from saying nothing to order; the proposal and his answer are then the way. Name the files listed as NO LONGER THERE in the proposal as one item (files that are no longer there), so his answer covers them; the tidy then settles it as usual. A proposal that holds only such files has no folder to tidy into: then the honest way out is the Report that quotes his whole answer; when he said yes and there are other items, the tidy of those items settles it. An answer that refuses only some items needs both (2) for the accepted ones and (3) for the refused ones. A tag, a report without his words or a quote that is not in his message settles nothing. When it is settled THOR retracts the note itself. On his answer (a plain yes accepts all; a number with no applies all the others): write the plan as a JSON file in your own scratch folder, `[{\"folder\": \"<absolute path>\", \"files\": [\"<absolute path>\", ...]}]` with only the accepted subjects, and run `install --tidy --db <store> --plan <that file>` (it makes each folder, gives it its own memory exactly as --topic does, moves the files, never deletes and never overwrites, refuses a home folder, Desktop, Documents, Downloads, a drive root and a program's own folders with a plain reason, leaves files of a git repository and files outside his home folder where they are, and writes an undo log). For each accepted subject store what the old sessions taught with the THOR remember tool under that subject's project (project = the folder's name): kind orientation, always true, one fact per note, under 300 characters, each with a falsifier; never a secret, and nothing that only mattered inside that one old conversation; plus ONE Report per subject that summarises what was found. When you have these tools (Claude desktop app), also group the old conversations per subject in the sidebar (mcp__ccd_sidebar__create_group, then mcp__ccd_sidebar__move_sessions); skip that without them. An item he refused stays exactly as it is; record the reason in 'owner-setup-answers' (revise it). For each accepted subject other than the one this conversation is in, prepare a conversation with mcp__ccd_session__spawn_task (title = the subject, cwd = its folder) and tell the person in one plain sentence to click the card; they never open a folder by hand. Report the result in one or two plain sentences, including that every file can be put back (you run `install --tidy-undo --db <store>` when he asks). Never delete a file, never move a file outside his home folder, never move the files of a git repository.
- NEVER ask about the plan and never look it up yourself: the installer already read it from Claude Code's settings, and THE TWO CALLS below carry it. Record that word with install --set-plan max|small --db <store> (Pro and any plan below Max count as small, and Max counts as max; a plan the settings did not name is small) and never mention it.
- NEVER mention in the first reply: the plan, git, hooks, helper assistants, the weekly check, the work rules, the record of real calls, bug reports, changelogs, topic markers or scopes. They are on the full numbered list and come up only when he asks.
- The full numbered list (THE FULL LIST below) is shown ONLY when he asks what is switched on (for example \"what is switched on?\"), never in the first reply. Show him the short list only: the intro, one line per item and the way to answer; never paste the detail per item (the last part of this text) at him. Its numbers 1 to 12 are stable and in order.
- The detail per item is yours: state an item's Why once, only when he asks about a number (\"why 7\") or refuses it, in one or two plain sentences, then do what he decided. If he asks and has not refused, add how to say no.
- Anything he does not mention keeps the default. Item 6 (helper assistants) is on by default (a new memory stores its two notes) and is never mentioned in the first reply: when he says \"helpers off\" run install --set-helpers off --db <store> and tell him it is off.
- Item 10 (backups) is off until he accepts proposal 1 of the welcome; a place he names (a folder or an address) is install --set-backup <place> --db <store>.
- Item 12 (changelog): only in a project that is a git repository and has no CHANGELOG.md, create CHANGELOG.md in the project this session runs in (a '# Changelog' heading with an 'Unreleased' section, nothing invented) and keep a version number only if the project already has one or he asks for one; do not mention it in the first reply. In a folder that is not a git repository create no CHANGELOG.md and never mention one. If he says no, create nothing and do not ask again.
- When he answers, apply each number where it belongs, then REVISE 'owner-setup-answers' (never store a second copy): 1 install --set-plan; 2 remove only THOR's four hook entries from the settings file (item 2 below says which and what stays), then tell him it takes effect after he restarts Claude Code (to switch them back on, run the installer again with `install --db <store>`: it wires the four entries again, a second run changes nothing, and a restart of Claude Code is needed again); 3 git config --global --unset core.hooksPath (turns off all of it), or only the video and size check: git config --global thor.mediagate.enabled false; 4 install --set-weekly-check off|on; 5 retract the named notes (a retracted note stops arriving; a note whose refusing check bites, such as windowless-processes-on-windows or scheduled-task-not-interactive once THOR has tried it, is refused at the retract, so first clear its check with mcp__thor__revise(id, clear_check: true, because: <why, 20+ characters>) and then retract it); 6 install --set-helpers on|off --db <store> (on stores the two helper notes, off retracts them; running it again changes nothing); 7 install --set-mode off (or release); 8 the language: install --set-language <code>|off --db <store> (nl, en, de, fr or es; an unknown code is refused with the list), and for the reply rules the reply file beside the store (guard-response-rulebook.json); 9 install --topic, run from inside the folder; 10 install --set-backup local|<folder or address>|off --db <store> (it makes the git folder, adds a named place as the remote 'nas', connects one session-start backup and makes a first copy to prove it works: tell him that result in plain words, and if it failed say so and why); 11 install --set-recording on|off --db <store>; 12 CHANGELOG.md as above.
- Off-topic work gets its own session: the starter note off-topic-work-gets-its-own-session says when to propose one, and a line from THOR saying that a message is about another project, or that this session runs in a folder of no project, means the same. Propose it in one plain sentence; on a yes, make the folder (inside Documents, named after the subject), run install --topic --db <store> from inside it, and then, when you have these tools (Claude desktop app), move the conversation there exactly as described under proposal 3 (request_directory, change_directory, set_session_title, and the check at the first tool call of the next turn). Move and rename this session when it has no work of its own yet; otherwise prepare a new one with mcp__ccd_session__spawn_task (cwd and title) and tell the person in one plain sentence to click the card. When you do not have these tools, give plain click steps. The person types nothing.
- When THOR holds or refuses (a held end of a turn, a refused call, a reply sent back), tell the person in one short plain sentence what you are doing and that nothing is needed from them; never the jargon words (rule, falsifier, evaluation, scope, teeth, hook, MCP, collection, store).
- A bad answer is one that works against him (for example switching off the connections and then expecting the memory to speak). Say ONCE, in one or two plain sentences, why the item exists (its 'Why' in the detail), then do what he decided. He has the final word, you never block and never nag.
- A THOR line that says a THOR program is missing (an antivirus may have quarantined it): tell him in one plain sentence what it found and the Windows Security steps it names. You may OFFER, once and never unasked, to add a Windows Defender exclusion for the THOR program folder: say what it does and its risk (Defender stops scanning every file in that folder), that Windows shows one administrator prompt, and that saying no is fine. Only after his clear yes run the exact command the line gives (Start-Process powershell -Verb RunAs -WindowStyle Hidden, running Add-MpPreference -ExclusionPath on the folder) and check it with (Get-MpPreference).ExclusionPath. Never change any other Windows security setting.
- A fact he tells you whose anchor you do not know (for example \"staging needs the VPN\" and he has not named the host): never invent a hostname, and never leave the fact unstored or ask him first. Store it with the best real anchor you do have (a file or a command it is about); when there is none, store it as kind orientation with always true (pinned, so it still comes back), with its falsifier. Then offer once, in one sentence, to attach it to the hostname if he gives it, and when he does, revise that note with the target (kind host, value the hostname).
- \"He does not want any of this\" is itself a valid answer: record it as the answers and apply nothing.";

/// What the assistant SAYS in its first reply (English source; the assistant
/// says it in the person's own language). `<language>` is filled by the
/// assistant with the language the person writes in. This replaces the old
/// "say first" sentences and the numbered list in the first reply: the full
/// list is shown only when the person asks what is switched on.
const WELCOME: &str = "\
Your memory helper THOR is set up. From now on I remember what you tell me about your work. I bring it back by myself when you need it, also in a new conversation next week.
You do not have to learn anything. Just talk to me as you always do. For example:
- \"Remember that the contractor is called Jan and comes on Tuesdays.\"
- \"What do you know about the kitchen?\"
- \"That is no longer right: the tiles now come from another shop.\"
This is what I will set up for you:
1. Once a day, a safety copy of everything I remember, on this computer.
2. I talk to you in <language>.
3. Everything for this work goes in the folder <folder>. That is where we work together.
Shall I set it up like this? Just say \"yes\".
Want something different? Tell me the number and what you want, for example \"2 English\" or \"3 figure it out yourself\".
Want to see everything that is switched on? Ask me: \"what is switched on?\"";

/// The welcome, as the assistant says it in its first reply.
pub fn render_welcome() -> String {
    WELCOME.to_string()
}

/// What to say before anything else.
fn say_first() -> String {
    format!("SAY FIRST (before anything else, in his own language, warm and plain, every point kept and nothing added; put the language he writes in where <language> stands and your proposed folder (see question 3) where <folder> stands (the proposals of THE WELCOME rules below)):\n{}", render_welcome())
}

/// Three facts to give in your own words, without a question.
const THREE_FACTS: &str = "\
THREE MORE FACTS (give them in your own words, without a question):
- Lanes. The code-and-work memory is the default. The personal lane for their own life - recipes, books, a training log - exists anyway and needs no setup; anything personal goes to the library with `shelve`, never into the code memory with `remember`.
- Project names. Every checkout is scoped by its folder name automatically - facts stay inside that project and do not compete across projects. If a folder name does not match how they call the project, they can run `install --project <name> --db <store>` from inside the checkout to override it. Only when they tell you a name, use it - never invent one.
- The memory is theirs to correct. A changed fact is corrected with `revise`, never stored a second time - though weakening any rule (clearing or changing its check, lowering its severity, dropping or replacing a binding, narrowing a checked rule's reach, taking a quoted call or a flag out of its text, or turning it into a report) needs a reason too, the same as removing one. Clearing one of those fields outright uses its own `clear_*` flag on `revise` (`clear_check`, `clear_severity`, and so on), not an empty value. A fact that is simply wrong is removed with `retract` and a reason. A true fact that fired in the wrong place is marked as noise with `mark`, instead of being deleted.";

/// The two calls of the first reply. `{PLAN}` is filled by `two_calls`.
const TWO_CALLS: &str = "\
THE TWO CALLS (make them in this order, in your first reply, whatever he answers; use the THOR tools remember and retract):
remember(
  id:   \"owner-setup-answers\",
  kind: \"report\",
  text: \"Setup finished with the defaults, nothing asked first. Plan: {PLAN}. The defaults on the numbered list apply (helper assistants are on), except backup, which stays off until the owner chooses (changelog: yes, only in a git project that has none). Backup, language and work folder follow the owner's answer to the welcome (a plain yes accepts the proposals). Replies: short and plain, in the owner's own language. Lane: work only. Projects: named after their folder. Any of these changes when the owner says so.\"
)
retract(id: \"walk-through-the-answer-guard-once\", reason: \"Setup finished: the owner's answers are on record as owner-setup-answers.\")
Give the report NO project: a store this fresh has not named a scope yet, so do not invent one and do not ask him to name one. The retract is refused until the remember call has landed, so make them in this order. When he later answers the two questions or by number, apply each answer where it belongs (the rules above), then revise 'owner-setup-answers' with what he chose; a second copy is worse than none. \"He is not interested\" is also a valid answer to record.";

/// The line above the detail per item.
const DETAIL_HEADER: &str = "DETAIL PER ITEM (for the assistant: state an item's Why once, only when he asks about a number or refuses it; never read all of this out)";

/// The words that stand for the plan in item 1, for the detail
/// (`detected`: what Claude Code's settings said at install, when known).
pub fn plan_line(detected: Option<Detection>) -> String {
    match detected {
        Some(Detection { plan: Plan::Max, tier_found: true }) => "The settings say: Max, so record max.".to_string(),
        Some(Detection { plan: Plan::Small, tier_found: true }) => "The settings say: Pro or smaller, so record small.".to_string(),
        Some(_) => "The settings do not say, so record small.".to_string(),
        None => "Record what the settings say, and small when they say nothing.".to_string(),
    }
}

/// The same question, short enough for the one line the user is shown.
pub fn plan_short(detected: Option<Detection>) -> String {
    match detected {
        Some(Detection { plan: Plan::Max, tier_found: true }) => "Max, as Claude's settings say".to_string(),
        Some(Detection { plan: Plan::Small, tier_found: true }) => "Pro or smaller, as Claude's settings say".to_string(),
        Some(_) => "the settings do not say, so I assume Pro or smaller".to_string(),
        None => "as Claude's settings say".to_string(),
    }
}

fn preface() -> &'static str {
    "Here is everything THOR has switched on for you, so nothing happens silently - one line each.\nNothing here waits for you: ask me about any number and I explain it, say no to any number and I switch it off."
}

fn closing() -> &'static str {
    "Answer by number (for example \"1 yes, 3 no, 10 local\"), ask \"why 7\" about any number, or say \"defaults\" to accept all of it; you can change any of it later just by asking."
}

/// The short list the user sees: an intro, one line per item, the way to
/// answer. Nothing else (the why and the way to say no are `render_detail`).
pub fn render_list(detected: Option<Detection>) -> String {
    let mut out = String::new();
    out.push_str(preface());
    out.push_str("\n\n");
    for q in questions() {
        out.push_str(&format!("{}. {}\n", q.n, q.line.replace("{PLAN_SHORT}", &plan_short(detected))));
    }
    out.push('\n');
    out.push_str(closing());
    out.push('\n');
    out
}

/// The real paths of this machine, known when the installer seeds the
/// memory. The user's own project does not hold the THOR repository or its
/// AGENTS.md, so the served text alone has to carry them. A path that is not
/// known renders as a plain description instead (`fill`), which is also what
/// the page in AGENTS.md shows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Paths {
    /// The installer program itself.
    pub installer: Option<PathBuf>,
    /// The memory (the store), for every `--db`.
    pub db: Option<PathBuf>,
    /// Claude Code's settings file, which holds THOR's four hook entries.
    pub settings: Option<PathBuf>,
    /// The file that holds the `thor` tool server (`~/.claude.json` by default).
    pub tool_server_file: Option<PathBuf>,
}

fn shown(path: &Option<PathBuf>, unknown: &str) -> String {
    path.as_ref().map_or_else(|| unknown.to_string(), |p| format!("\"{}\"", p.display()))
}

impl Paths {
    fn installer_text(&self) -> String {
        shown(&self.installer, "the install program in the thor2 folder (install.exe on Windows)")
    }
    fn db_text(&self) -> String {
        shown(&self.db, "<store>")
    }
    fn settings_text(&self) -> String {
        shown(&self.settings, "Claude Code's settings file (~/.claude/settings.json)")
    }
    fn tool_server_text(&self) -> String {
        shown(&self.tool_server_file, "Claude Code's tool-server file (~/.claude.json)")
    }
}

/// Put the paths into a text: the tokens `{INSTALL}`, `{DB}`, `{SETTINGS}`,
/// `{CLAUDE_JSON}`, and the older `<store>` placeholder.
pub fn fill(text: &str, paths: &Paths) -> String {
    text.replace("{INSTALL}", &paths.installer_text())
        .replace("{DB}", &paths.db_text())
        .replace("{SETTINGS}", &paths.settings_text())
        .replace("{CLAUDE_JSON}", &paths.tool_server_text())
        .replace("<store>", &paths.db_text())
}

/// The detail per item, for the assistant: what, why, default and how to say no.
pub fn render_detail(detected: Option<Detection>) -> String {
    render_detail_with(detected, &Paths::default())
}

pub fn render_detail_with(detected: Option<Detection>, paths: &Paths) -> String {
    let mut out = String::new();
    out.push_str(DETAIL_HEADER);
    out.push('\n');
    for q in questions() {
        out.push_str(&format!(
            "\n{}. {}\n   What it is: {}\n   Why it exists: {}\n   What I chose: {}\n   To say no: {}\n",
            q.n,
            q.title,
            fill(&q.what.replace("{PLAN_LINE}", &plan_line(detected)), paths),
            q.why,
            q.default,
            fill(&q.say_no, paths)
        ));
    }
    out
}

/// The plan as the answers report names it.
fn plan_for_answers(detected: Option<Detection>) -> &'static str {
    match detected {
        Some(Detection { plan: Plan::Max, tier_found: true }) => "Max (read from Claude Code's settings; he can correct it)",
        Some(Detection { plan: Plan::Small, tier_found: true }) => "Pro or smaller (read from Claude Code's settings; he can correct it)",
        Some(_) => "Pro or smaller by default (the settings did not say)",
        None => "<as detected>",
    }
}

/// The two calls of the first reply, with the plan filled in.
pub fn two_calls(detected: Option<Detection>) -> String {
    TWO_CALLS.replace("{PLAN}", plan_for_answers(detected))
}

/// Every path the assistant needs, so it never has to guess one or ask.
pub fn paths_section(paths: &Paths) -> String {
    fill(
        "PATHS ON THIS MACHINE (use them exactly; never ask him for a path)
- The installer program: {INSTALL}. Wherever this text says `install ...`, run this program with those arguments (a quoted path runs as a command in Bash; in PowerShell put & in front).
- The memory (the store), for every --db: {DB}
- Claude Code's settings file, which holds THOR's four hook entries: {SETTINGS}
- The file that holds the `thor` tool server: {CLAUDE_JSON}",
        paths,
    )
}

/// What the assistant does, everything it needs written out: the rules, what
/// to say first, the two calls, the paths and the three facts.
pub fn assistant_section(detected: Option<Detection>, paths: &Paths) -> String {
    format!(
        "{}\n\n{}\n\n{}\n\n{}\n\n{}",
        fill(ASSISTANT_RULES, paths),
        say_first(),
        two_calls(detected),
        paths_section(paths),
        fill(THREE_FACTS, paths)
    )
}

/// The text of the lookup item: the short list, what the assistant does, then
/// the detail per item.
pub fn lookup_text(detected: Option<Detection>) -> String {
    lookup_text_with(detected, &Paths::default())
}

pub fn lookup_text_with(detected: Option<Detection>, paths: &Paths) -> String {
    format!(
        "{}\n\nTHE FULL LIST (show it ONLY when he asks what is switched on, never in the first reply):\n{}\n{}",
        assistant_section(detected, paths),
        render_list(detected),
        render_detail_with(detected, paths)
    )
}

/// The block that sits between the markers in `AGENTS.md`: the same render,
/// with the plan line written for any user.
pub fn agents_md_block() -> String {
    format!("{BEGIN_MARKER}\n{}{END_MARKER}", lookup_text(None))
}

/// The lookup item the installer seeds into a new memory.
pub fn lookup_item(detected: Option<Detection>) -> model::item::Item {
    lookup_item_with(detected, &Paths::default())
}

pub fn lookup_item_with(detected: Option<Detection>, paths: &Paths) -> model::item::Item {
    model::item::Item {
        id: LOOKUP_ID.to_string(),
        kind: model::item::Kind::Lookup,
        text: lookup_text_with(detected, paths),
        bindings: vec![],
        severity: None,
        project: None,
        tags: vec!["working-contract".to_string()],
        expires: None,
        key: Some(LOOKUP_ID.to_string()),
        falsifier: None,
        check: None,
    }
}
