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
//! NON-BLOCKING BY DESIGN. The defaults are applied first, in the same turn;
//! the list is then shown with "tell me by number what you want different".
//! Nothing waits for an answer. A bad answer is answered with the reason once
//! and the user's final word is respected (`ASSISTANT_RULES`).

use crate::install::workflow_starter_pack;
use serve::plan::{Detection, Plan};

/// The id and the key of the lookup item that carries the rendered list.
pub const LOOKUP_ID: &str = "first-session-questions";

/// The starter notes that make up helper-agent mode (question 6): everything
/// else in the workflow pack is the plain work rules (question 5).
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
    /// What it is, in plain words.
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
    workflow_starter_pack().into_iter().map(|n| n.id).filter(|id| !HELPER_MODE_NOTES.contains(&id.as_str())).collect()
}

/// The plain work rules as one line of names.
pub fn work_rule_line() -> String {
    work_rule_ids().iter().map(|id| plain_name(id)).collect::<Vec<_>>().join("; ")
}

/// The whole list, in the order the user sees it.
pub fn questions() -> Vec<Question> {
    let rules = work_rule_ids();
    let teaching = crate::install::working_contract().len() - 1;
    let ids = rules.join(", ");
    let list = vec![
        Question {
            n: 1,
            id: "plan",
            title: "Your plan",
            what: "I looked at Claude Code's own settings to find out which plan you have: Max, or Pro and smaller. {PLAN_LINE}".to_string(),
            why: "THOR checks itself on a schedule, and that check draws on the same usage limit as your own work, so on a smaller plan it runs once a week at most and never daily, which keeps your limit for you; on Max it also runs daily while something is found wrong.",
            default: "The plan the settings say; a smaller plan when they do not say.",
            say_no: "Tell me the right plan and I record it, so the schedule follows what you really pay for.".to_string(),
        },
        Question {
            n: 2,
            id: "hooks",
            title: "The connections to Claude Code",
            what: "THOR is wired into Claude Code at four moments: when a session starts, before each tool call, when you send a message, and when a reply ends. That is how the memory speaks up by itself and how replies get checked. A backup copy of Claude Code's settings file was made first.".to_string(),
            why: "A memory that must be asked for stays silent, and a missing connection fails without any message: the memory simply never says a word.",
            default: "On.",
            say_no: "Then THOR is only a notebook your assistant searches when you ask. I remove the THOR lines from Claude Code's settings file (the backup copy stays next to it), which also ends the weekly check, the reply rules and the bug offers, since they all run through those connections.".to_string(),
        },
        Question {
            n: 3,
            id: "git-hooks",
            title: "The git hooks in every repository",
            what: "THOR set one git setting for the whole machine so that every repository refreshes THOR's code index after each commit. Each repository's own git hooks still run; if another tool already owned that setting, THOR left it alone.".to_string(),
            why: "An index three days behind made 83 percent of the 'where is this used' answers name a wrong line; a fresh index made none wrong (measured at the maker, 2026-08-15).",
            default: "On, unless something else already owned the setting.",
            say_no: "I undo it with one git command (git config --global --unset core.hooksPath). Searching the code then goes stale after each commit.".to_string(),
        },
        Question {
            n: 4,
            id: "weekly-check",
            title: "The weekly check",
            what: "About once a week per project (daily on Max while something is found wrong) THOR asks your assistant to run a short tidy-up of the memory before it finishes a turn: judge the notes that fired, repair stale ones and file a report. The command /thor-eval was added so you can also run it yourself.".to_string(),
            why: "A memory nobody maintains fills with stale notes and quietly stops helping, and a check that depends on someone remembering it does not happen.",
            default: "On, at the rhythm of question 1.",
            say_no: "I switch the weekly ask off (install --set-weekly-check off); you can still type /thor-eval yourself, and nothing else changes.".to_string(),
        },
        Question {
            n: 5,
            id: "work-rules",
            title: "The starter work rules",
            what: format!(
                "A set of {} general work habits, stored as notes that arrive right before the step they are about, once per session: {}. {teaching} more notes teach your assistant how to write notes that come back and how to work with THOR. None of them can block anything.",
                rules.len(),
                work_rule_line()
            ),
            why: "They are the habits that were marked useful again and again in a memory built over months of real work, so a new memory starts with that experience instead of learning it the hard way.",
            default: "All on.",
            say_no: format!("Name the ones you do not want, or say none: I remove just those notes ({ids}), and a removed note simply stops arriving."),
        },
        Question {
            n: 6,
            id: "helper-agents",
            title: "Helper-agent mode",
            what: "Your main assistant (the strongest model) answers, judges and checks, while cheaper models do the real work in helper agents, one helper per task. A trivial one-step job is done directly, tests are run narrowly while working and in full once at the end, and a helper that goes quiet is judged dead after about three checks. This is also true on smaller plans, and if your plan has no strongest model the best available one steers.".to_string(),
            why: "Measured at the maker, helpers were likely 12 to 62 percent cheaper than doing everything in the main conversation, and the chat stays free and answers at once while they work, instead of you waiting 5 to 15 minutes.",
            default: "On.",
            say_no: "I remove the two notes that carry it (orchestrator-mode-saves-quota and an-agent-brief-stands-alone); your assistant then does the work itself in the main conversation.".to_string(),
        },
        Question {
            n: 7,
            id: "bug-reports",
            title: "Reporting THOR's own bugs",
            what: "Sometimes THOR finds a fault in itself. Your assistant may then offer to report it to the makers on GitHub. It shows you the exact full text first; file paths, project and folder names, computer and user names, addresses, keys and passwords are removed; nothing is sent without your yes, and saying no costs nothing.".to_string(),
            why: "THOR sees faults in itself that nobody reading a report would notice, and the makers can only fix what they hear about.",
            default: "On: it may ask you, once per fault, and sends nothing without your yes.",
            say_no: "I switch it off (install --set-mode off): nothing is offered and nothing is sent, ever.".to_string(),
        },
        Question {
            n: 8,
            id: "replies",
            title: "Reply language and length",
            what: "Your assistant answers in your language, short, with a plain-language summary first. A small file next to the memory holds five reply rules (a summary first, no reflexive disclaimers, check before asking you to look something up, keep an answer under about 1000 characters, show evidence when saying something was checked); a reply that breaks one is sent back once to be rewritten. A summary or overview you ask for may run longer.".to_string(),
            why: "Long, technical replies are hard to act on; the rules keep a reply to the point, and they were checked against about 6,600 real replies at the maker, and the false triggers found there were removed.",
            default: "Your language, short replies, all five reply rules on.",
            say_no: "Tell me the language and the length you want. I change the file, or remove a single rule to turn just that one off.".to_string(),
        },
        Question {
            n: 9,
            id: "topic-folders",
            title: "Folders for a topic",
            what: "A plain folder with no code in it, such as a folder of recipes or travel plans, can have a memory of its own: I run one command inside it and it gets one small hidden file. Notes about that folder then come back only when you work there. Nothing is set up until you name a folder.".to_string(),
            why: "Notes about one subject stay out of the way of the others, so nothing from your recipe folder interrupts your code.",
            default: "No folders are set up yet.",
            say_no: "Nothing to undo: do not name a folder. A folder you set up has one hidden file, .thor-project, that you can delete.".to_string(),
        },
        Question {
            n: 10,
            id: "backups",
            title: "Backups of the memory",
            what: "About once a day, and when a session starts, THOR writes a copy of the whole memory into a git repository of its own on this machine. It goes somewhere else only if you name a place: a folder on a NAS or another computer you own, or a private repository.".to_string(),
            why: "The memory is a single file, and if the disk dies everything it learned dies with it.",
            default: "A daily copy on this machine; nothing leaves the machine.",
            say_no: "Nothing leaves your machine unless you name a place. The local copy is one folder you can delete.".to_string(),
        },
    ];
    debug_assert!(list.iter().enumerate().all(|(i, q)| q.n == i + 1));
    list
}

/// What the assistant does with the answers. Not read out to the user.
pub const ASSISTANT_RULES: &str = "\
FOR THE ASSISTANT (do not read this part out)
- Apply the defaults NOW, in this same turn, without waiting for an answer, so nothing waits: store the Report 'owner-setup-answers' with them and retract the setup note. Then show the list above in the user's own language, once, and say that everything on it is already on and that he can tell you by number what he wants different (for example \"1 yes, 3 no, the rest as you like\"; \"defaults\" or \"doe maar\" accepts all of it).
- Anything he does not mention keeps the default. Never ask the same question twice and never hold up his work for an answer.
- Question 1: if the settings named a plan, ask him to confirm it with its reason; if they did not, ask which plan outright with the same reason. Record his word with install --set-plan max|small --db <store>.
- When he answers, apply each number where it belongs, then REVISE 'owner-setup-answers' (never store a second copy): 1 install --set-plan; 2 remove THOR's lines from Claude Code's settings file; 3 git config --global --unset core.hooksPath; 4 install --set-weekly-check off|on; 5 and 6 retract the named notes (a retracted note stops arriving); 7 install --set-mode off (or release); 8 the reply file beside the store (guard-response-rulebook.json) and a stored preference for the language; 9 install --topic, run from inside the folder; 10 the backup folder and its remote.
- A bad answer is one that works against him (for example switching off the connections and then expecting the memory to speak). Say ONCE, in one or two plain sentences, why the item exists (its 'Why' above), then do what he decided. He has the final word, you never block and never nag.
- \"He does not want any of this\" is itself a valid answer: record it as the answers and apply nothing.";

/// The words that stand for the plan in item 1, for the list the user sees
/// (`detected`: what Claude Code's settings said at install, when known).
pub fn plan_line(detected: Option<Detection>) -> String {
    match detected {
        Some(Detection { plan: Plan::Max, tier_found: true }) => "The settings say: Max. Is that right?".to_string(),
        Some(Detection { plan: Plan::Small, tier_found: true }) => "The settings say: Pro or smaller. Is that right?".to_string(),
        Some(_) => "The settings do not say, so which is it: Max, or Pro and smaller?".to_string(),
        None => "Ask: 'The settings say <Max | Pro or smaller>. Is that right?' - or, when they do not say, 'Which is it: Max, or Pro and smaller?'".to_string(),
    }
}

fn preface() -> &'static str {
    "THOR is set up. Here is everything the install arranged or switched on, so nothing happens silently. For each one: what it is, why it exists, what was chosen for you, and how to say no.\n\nAnswer by number, for example \"1 yes, 3 no, the rest as you like\". Whatever you do not mention keeps the choice made for you, and \"defaults\" accepts all of it. You can change any of it later just by asking."
}

/// The numbered list the user sees, with the plan line for `detected`.
pub fn render_list(detected: Option<Detection>) -> String {
    let mut out = String::new();
    out.push_str(preface());
    out.push('\n');
    for q in questions() {
        out.push_str(&format!(
            "\n{}. {}\n   What it is: {}\n   Why it exists: {}\n   What I chose: {}\n   To say no: {}\n",
            q.n,
            q.title,
            q.what.replace("{PLAN_LINE}", &plan_line(detected)),
            q.why,
            q.default,
            q.say_no
        ));
    }
    out
}

/// The text of the lookup item: the list, then what the assistant does.
pub fn lookup_text(detected: Option<Detection>) -> String {
    format!("{}\n{}\n", render_list(detected), ASSISTANT_RULES)
}

/// The block that sits between the markers in `AGENTS.md`: the same render,
/// with the plan line written for any user.
pub fn agents_md_block() -> String {
    format!("{BEGIN_MARKER}\n{}{END_MARKER}", lookup_text(None))
}

/// The lookup item the installer seeds into a new memory.
pub fn lookup_item(detected: Option<Detection>) -> model::item::Item {
    model::item::Item {
        id: LOOKUP_ID.to_string(),
        kind: model::item::Kind::Lookup,
        text: lookup_text(detected),
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
