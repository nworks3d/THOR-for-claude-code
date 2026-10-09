//! The Response Guard: the one surface that watches the ASSISTANT, not the
//! store. It reads a rulebook of the owner's standing rules about how a reply
//! should look ("open with a TLDR when the answer is technical", "do not ask
//! me to do what you can do yourself", "no reflexive AI disclaimers") and, on
//! the Stop hook, blocks a reply that breaks one so the model reconsiders
//! before yielding.
//!
//! THE DEFECT THIS EXISTS TO UNDO, and it is the sharpest one of the whole
//! rebuild (found by the owner, 2026-08-03). 1.0 ran this guard on a Stop
//! hook. 2.0 was built with three of 1.0's four hook surfaces and this one -
//! the only one that watches the assistant's own behaviour - was left out
//! entirely. So for a full session of building 2.0, replies full of commit
//! hashes and "PASS" went out with no TLDR and nothing corrected them,
//! because the thing that would have was the thing not built. That is exactly
//! the failure class the whole project set out to make impossible - a surface
//! silently doing less than 1.0 - introduced by my own hand, on the surface
//! that guards my hand. It is restored here, byte-for-byte in behaviour: the
//! rulebook is the owner's own, ported unchanged.
//!
//! Matching is dependency-free, case-insensitive substring logic, identical to
//! 1.0's `guard::evaluate` so the same rulebook behaves the same. A rule fires
//! when: the haystack is at least `min_chars` long (a floor so a rule about the
//! SHAPE of a long report does not fire on a two-line status note that happens
//! to contain one technical word), AND every `all_of` term is present, AND
//! (`any_of` is empty OR at least one is present), AND no `none_of` term is
//! present (the escape hatch that lets a false-positive twin through).
//!
//! Failure policy: this is the ONE surface allowed to emit a block, but ONLY on
//! a genuine match. Every ERROR is hard fail-open - a missing or malformed
//! rulebook, unreadable stdin, anything - prints nothing and blocks nothing.
//! A guard that watches replies must never itself become the reason a reply
//! cannot be given.

use regex::Regex;
use serde_json::Value;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// One response rule, parsed from the rulebook JSON.
#[derive(Debug, Clone)]
pub struct Rule {
    pub id: String,
    pub all_of: Vec<String>,
    pub any_of: Vec<String>,
    /// If ANY of these is present the rule does NOT fire - the false-positive
    /// escape (e.g. "false positive", "als voorbeeld", "any_of" so the rule's
    /// own definition text never trips it).
    pub none_of: Vec<String>,
    /// Length floor in characters. 0 = no floor. The TLDR rule uses 600 so a
    /// short status line carrying one technical word is never blocked.
    pub min_chars: usize,
    pub reminder: String,
}

/// The rulebook next to the store, so the Stop hook needs no path argument.
/// Never a CWD-relative name: a project directory could plant one and inject
/// reminders. `db` is the store path; the rulebook sits beside it.
pub fn default_rulebook_path(db: &Path) -> PathBuf {
    db.parent()
        .unwrap_or_else(|| Path::new("."))
        .join("guard-response-rulebook.json")
}

/// Read a JSON array field as a list of strings, dropping any non-string
/// entries silently (same fail-soft stance as everything else in this
/// parser). Shared by every rule shape this file parses (`Rule` and
/// `OptInRule` further down) so the field-reading logic exists exactly once.
fn str_list(v: &Value, key: &str) -> Vec<String> {
    v.get(key)
        .and_then(|x| x.as_array())
        .map(|a| a.iter().filter_map(|s| s.as_str().map(str::to_string)).collect())
        .unwrap_or_default()
}

/// Parse one rulebook entry into a `Rule`. `None` (entry dropped, fail-open)
/// when `reminder` is missing or not a string - the one field every rule
/// must carry. Factored out of `parse_rules` so `OptInRule` (further down)
/// can build its own base `Rule` from the exact same logic instead of a
/// second copy that could silently drift from this one.
fn parse_one_rule(r: &Value) -> Option<Rule> {
    let reminder = r.get("reminder").and_then(|v| v.as_str())?.to_string();
    Some(Rule {
        id: r.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string(),
        all_of: str_list(r, "all_of"),
        any_of: str_list(r, "any_of"),
        none_of: str_list(r, "none_of"),
        min_chars: r
            .get("min_chars")
            .and_then(|v| v.as_u64())
            .unwrap_or(0)
            .try_into()
            .unwrap_or(usize::MAX),
        reminder,
    })
}

/// Parse the rulebook. Any malformed JSON yields no rules (fail-open): a broken
/// rulebook silences the guard, never errors.
pub fn parse_rules(text: &str) -> Vec<Rule> {
    let Ok(Value::Array(arr)) = serde_json::from_str::<Value>(text) else {
        return vec![];
    };
    arr.iter().filter_map(parse_one_rule).collect()
}

/// Normalise the assistant's message into one lowercase haystack: collapse
/// intra-line whitespace to a single space so multi-word terms ("git commit")
/// match regardless of extra spaces, but keep newlines so a term cannot span
/// two unrelated lines. Same shape as 1.0's `tool_input_text`.
pub fn haystack(message: &str) -> String {
    message
        .to_lowercase()
        .lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect::<Vec<_>>()
        .join("\n")
}

/// How Claude Code opens the user line it writes for a Stop hook's text.
const STOP_HOOK_FEEDBACK_PREFIX: &str = "Stop hook feedback:";

/// The text of the OWNER'S OWN last prompt, read out of a Claude Code
/// transcript (the JSONL `transcript_path` a Stop payload always names -
/// `serve/src/bin/serve.rs` reads that file and hands its raw text here;
/// this function does no I/O of its own, same stance as every other function
/// in this file). `None` when the transcript is empty, unparseable line by
/// line, or simply never contains a real user turn - fail-open, because the
/// only thing that ever reads this (the list/overview exemption on
/// `OptInRule::list_request_any_of`, see the opt-in section below) treats
/// "no signal" exactly like "the prompt did not ask for a list": the rule it
/// guards keeps firing exactly as it did before this function existed.
///
/// THE DEFECT THIS PREVENTS (FALSE BLOCK A, reported by the owner from other
/// sessions, fixed 2026-09-07): the length rule
/// (`checked-claim-needs-evidence`'s sibling `answer-is-too-long`) blocked a
/// reply that was a list the owner had explicitly asked for. There was no
/// way for the guard to see the PROMPT at all - only the reply
/// (`last_assistant_message`) ever reached it - so a rule about the shape of
/// the reply could never take the owner's own request into account.
///
/// Claude Code's transcript is one JSON object per line. A line the owner
/// actually typed has `"type":"user"` and a message whose `content` is a
/// plain STRING. A line with an ARRAY `content` is Claude Code's own shape
/// for a tool result being fed back as a "user" turn - not the owner
/// speaking - UNLESS that array itself also carries a plain
/// `{"type":"text",...}` block (a real prompt that also attaches an image,
/// say), so an array is read that far and no further. Scanned from the END
/// and returns the FIRST real prompt found that way, because the guard only
/// ever cares about the prompt that led to the reply it is judging right
/// now, not the first message of the whole session. Any line that is not
/// valid JSON, or carries neither shape, is skipped rather than treated as a
/// parse failure - one malformed line must never hide every real prompt
/// before it.
pub fn last_user_prompt(transcript_jsonl: &str) -> Option<String> {
    for line in transcript_jsonl.lines().rev() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(entry) = serde_json::from_str::<Value>(line) else { continue };
        if let Some(prompt) = owner_prompt_of(&entry) {
            return Some(prompt);
        }
    }
    None
}

/// The text the owner typed in one transcript entry, `None` for every entry
/// that is not him typing (an assistant line, a tool result, Claude Code's own
/// note, a Stop hold's text, a background task's notification or a system
/// reminder: `prompt::is_owners_own_text`) - see `last_user_prompt` for the
/// shapes. Field test 12, R6: the length hold quoted `</task-notification>` as
/// the owner's words because the last user-role entry was only a notification.
pub fn owner_prompt_of(entry: &Value) -> Option<String> {
    if entry.get("type").and_then(|t| t.as_str()) != Some("user") {
        return None;
    }
    // Claude Code's own notes to the model (`isMeta`), such as the text of
    // a Stop hold fed back after the hold, are not the owner typing.
    if entry.get("isMeta").and_then(|m| m.as_bool()) == Some(true) {
        return None;
    }
    let content = entry.get("message").and_then(|m| m.get("content"))?;
    if let Some(s) = content.as_str() {
        return (!s.trim().is_empty() && !s.starts_with(STOP_HOOK_FEEDBACK_PREFIX) && crate::prompt::is_owners_own_text(s))
            .then(|| s.to_string());
    }
    let blocks = content.as_array()?;
    let text = blocks
        .iter()
        .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("text"))
        .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
        // A block the harness wrote (a task notification, a system reminder)
        // is not the owner typing, even beside a block he did type.
        .filter(|t| crate::prompt::is_owners_own_text(t))
        .collect::<Vec<_>>()
        .join("
");
    // A content array with no plain text block at all (a pure tool_result
    // turn) is not something the owner typed.
    (!text.trim().is_empty()).then_some(text)
}

/// Tool calls that LOOK at something (a file, a page, the web, a screen), by
/// name. Together with the shell, these are what a rule marked
/// `backed_by_turn_tools` reads as "the turn looked for itself". Not in it, on
/// purpose: launching an agent (its report is someone else's look), writing or
/// editing, and THOR's own memory tools (a note is not the thing it is about).
fn is_observation_tool(name: &str) -> bool {
    matches!(name, "Read" | "Grep" | "Glob" | "WebFetch" | "WebSearch" | "NotebookRead")
        || name.starts_with("mcp__Claude_Browser__")
        || name.starts_with("mcp__claude-in-chrome__")
        || name.starts_with("mcp__computer-use__")
}

/// What the assistant did in the CURRENT turn that looked at something, after
/// the owner's last prompt in the transcript: the command of every Bash or
/// PowerShell tool call, and a `[tool] Name` marker for every other tool call
/// that looks (`is_observation_tool`). Empty when the transcript is unreadable
/// (fail-open: no evidence is claimed).
pub fn turn_commands(transcript_jsonl: &str) -> Vec<String> {
    let entries: Vec<Value> = transcript_jsonl.lines().filter_map(|l| serde_json::from_str::<Value>(l.trim()).ok()).collect();
    let start = entries.iter().rposition(|e| owner_prompt_of(e).is_some()).map_or(0, |i| i + 1);
    let mut commands = Vec::new();
    for entry in &entries[start..] {
        if entry.get("type").and_then(|t| t.as_str()) != Some("assistant") {
            continue;
        }
        let Some(blocks) = entry.get("message").and_then(|m| m.get("content")).and_then(|c| c.as_array()) else { continue };
        for block in blocks {
            if block.get("type").and_then(|t| t.as_str()) != Some("tool_use") {
                continue;
            }
            let name = block.get("name").and_then(|n| n.as_str()).unwrap_or("");
            let is_shell = matches!(name, "Bash" | "PowerShell");
            if let (true, Some(command)) = (is_shell, block.get("input").and_then(|i| i.get("command")).and_then(|c| c.as_str())) {
                commands.push(command.to_string());
                // What the call says it is for names what it looked at as well.
                if let Some(why) = block.get("input").and_then(|i| i.get("description")).and_then(|d| d.as_str()).filter(|d| !d.is_empty()) {
                    commands.push(format!("[desc] {why}"));
                }
            } else if is_observation_tool(name) {
                // The marker carries what the call looked at (a file, a pattern,
                // a page), so a claim can be tied to it (`turn_backs_claim`).
                match observation_target(block.get("input")) {
                    target if target.is_empty() => commands.push(format!("[tool] {name}")),
                    target => commands.push(format!("[tool] {name} {target}")),
                }
            }
        }
    }
    commands
}

/// What an observation tool call looked at, as words: its file, folder,
/// pattern, page address or query, whichever the input carries.
fn observation_target(input: Option<&Value>) -> String {
    let Some(input) = input else { return String::new() };
    ["file_path", "path", "notebook_path", "pattern", "url", "query"]
        .iter()
        .filter_map(|key| input.get(key).and_then(|v| v.as_str()))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Words too common to tie a claim to a call, in Dutch and English, on top of
/// the rule's own trigger words.
const COMMON_WORDS: &[&str] = &[
    "alle", "alles", "drie", "orde", "goed", "klopt", "staat", "zelf", "niet", "geen", "ook", "maar", "met", "voor", "naar", "nog", "wel",
    "dus", "zijn", "wordt", "worden", "heb", "heeft", "hebben", "kan", "zou", "dat", "die", "dit", "het", "een", "van", "the", "and", "that",
    "this", "with", "have", "has", "for", "are", "was", "were", "all", "http", "https", "www", "com", "exe", "tool",
    // The programs and verbs of a command line: they name how a look was made,
    // not what it looked at.
    "git", "curl", "node", "npm", "npx", "cargo", "python", "bash", "echo", "cat", "head", "tail", "grep", "sed", "awk", "find", "get",
    "set", "select", "object", "write", "read", "powershell", "item", "content", "path", "file", "files",
];

/// Whether a word of a command line is a path: it starts like one, or has a
/// slash and nothing of a regular expression or a script in it.
fn looks_like_path(word: &str) -> bool {
    let drive = word.len() > 2 && word.as_bytes()[1] == b':' && matches!(word.as_bytes()[2], b'/' | b'\\');
    let code = word.contains(['(', ')', '[', ']', '{', '}', '<', '>', '*', '?', '|', '=', '$', '^']);
    !code && (drive || word.starts_with(['/', '\\', '~']) || word.starts_with("./") || word.contains(['/', '\\']))
}

/// The words that name something, from `text`: lowercased runs of letters and
/// digits of at least three characters, a plural `s` dropped, common words and
/// `skip` left out. A word with a path or address in it keeps only what names
/// the thing: a file's own name (not the folders above it), a page's host and
/// last part.
fn naming_words(text: &str, skip: &HashSet<String>) -> HashSet<String> {
    let mut words = HashSet::new();
    for raw in text.split_whitespace() {
        let raw = raw.trim_matches(|c: char| matches!(c, '"' | '\'' | '`' | '(' | ')' | ';' | ',' | '<' | '>'));
        // `R=/c/Users/me/proj` names the folder, not the variable.
        let raw = match raw.split_once('=') {
            Some((name, value)) if !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') => {
                value.trim_matches(|c: char| matches!(c, '"' | '\''))
            }
            _ => raw,
        };
        let parts: Vec<&str> = match raw.split_once("://") {
            Some((_, address)) => {
                let host = address.split('/').next().unwrap_or("");
                vec![host, address.rsplit('/').next().unwrap_or("")]
            }
            None if looks_like_path(raw) => vec![raw.rsplit(['/', '\\']).next().unwrap_or("")],
            None => vec![raw],
        };
        for part in parts {
            for word in part.split(|c: char| !c.is_alphanumeric()).filter(|w| w.chars().count() >= 3) {
                let lower = word.to_lowercase();
                let stem = lower.strip_suffix('s').filter(|s| s.chars().count() >= 3).unwrap_or(&lower).to_string();
                if !COMMON_WORDS.contains(&lower.as_str()) && !skip.contains(&lower) && !skip.contains(&stem) {
                    words.insert(stem);
                }
            }
        }
    }
    words
}

/// The paragraphs of `lower` that carry one of the rule's trigger terms, with
/// the one before each (what the claim is about is usually said there) and the
/// one after (a claim "Gecontroleerd:" is followed by its list). The whole
/// reply when no paragraph carries a term.
fn claim_text(lower: &str, triggers: &[String]) -> String {
    let paragraphs: Vec<&str> = lower.split("\n\n").collect();
    let triggers: Vec<String> = triggers.iter().map(|t| t.to_lowercase()).collect();
    let hits: Vec<usize> =
        paragraphs.iter().enumerate().filter(|(_, p)| triggers.iter().any(|t| p.contains(t.as_str()))).map(|(i, _)| i).collect();
    if hits.is_empty() {
        return lower.to_string();
    }
    let keep = |i: usize| hits.iter().any(|h| i + 1 >= *h && i <= h + 1);
    paragraphs.iter().enumerate().filter(|(i, _)| keep(*i)).map(|(_, p)| *p).collect::<Vec<_>>().join(" ")
}

/// Whether what the turn looked at is what a claim of checking is about (the
/// rules marked `backed_by_turn_tools`): some call of the turn - a command, a
/// read, a search, a page - shares a naming word with the claim (its file name,
/// its command words, its address). A call that has nothing to do with the claim
/// does not back it (field test 14, collect 17b: any look in the turn used to).
/// The trigger words of the rule themselves do not count as naming anything.
fn turn_backs_claim(rule: &Rule, haystack_lower: &str, turn_commands: &[String]) -> bool {
    let skip: HashSet<String> = rule
        .any_of
        .iter()
        .chain(&rule.all_of)
        .flat_map(|t| t.to_lowercase().split(|c: char| !c.is_alphanumeric()).map(str::to_string).collect::<Vec<_>>())
        .collect();
    let claim = naming_words(&claim_text(haystack_lower, &rule.any_of), &skip);
    turn_commands.iter().any(|call| {
        // A marker is "[tool] Name <target>": the tool's name names nothing.
        let call = match (call.strip_prefix("[tool] "), call.strip_prefix("[desc] ")) {
            (Some(marker), _) => marker.split_once(' ').map_or("", |(_, target)| target),
            (None, Some(why)) => why,
            (None, None) => call,
        };
        !naming_words(call, &skip).is_disjoint(&claim)
    })
}

/// Whether `rule`'s AND/OR/NOT/min_chars conditions hold against this
/// haystack - the whole non-positional matcher, built from the two halves
/// below so `evaluate_opt_in` (further down) reuses them rather than
/// reimplementing AND/OR/NOT next to a second copy that could drift from
/// this one.
fn rule_matches(rule: &Rule, haystack_lower: &str) -> bool {
    trigger_holds(rule, haystack_lower, &|term| haystack_lower.contains(term)) && !none_of_escaped(rule, haystack_lower)
}

/// The trigger side: `min_chars`, then every `all_of` term and (when there
/// are any) one `any_of` term present, where `present` answers for one
/// lowercased term - a plain substring test, or `occurs_as_claimed` for a rule
/// that counts only the assistant's own claims (see "reported speech").
fn trigger_holds(rule: &Rule, haystack_lower: &str, present: &dyn Fn(&str) -> bool) -> bool {
    if rule.min_chars > 0 && haystack_lower.chars().count() < rule.min_chars {
        return false;
    }
    if !rule.all_of.iter().all(|s| present(&s.to_lowercase())) {
        return false;
    }
    rule.any_of.is_empty() || rule.any_of.iter().any(|s| present(&s.to_lowercase()))
}

/// The `none_of` side: any escape term present in `text`.
fn none_of_escaped(rule: &Rule, text: &str) -> bool {
    rule.none_of.iter().any(|s| text.contains(&s.to_lowercase()))
}

/// The pure matcher: every rule's reminder that fires for this haystack, in
/// rulebook order. Identical semantics to 1.0's `guard::evaluate`.
pub fn evaluate(rules: &[Rule], haystack_lower: &str) -> Vec<String> {
    rules.iter().filter(|r| rule_matches(r, haystack_lower)).map(|r| r.reminder.clone()).collect()
}

/// The block reason for an assistant message, or `None` if nothing fires.
/// `rulebook_text` is the raw rulebook file contents (or None if it could not
/// be read - then nothing fires, fail-open).
pub fn block_reason(rulebook_text: Option<&str>, message: &str) -> Option<String> {
    let rules = rulebook_text.map(parse_rules).unwrap_or_default();
    if rules.is_empty() {
        return None;
    }
    let fired = evaluate(&rules, &haystack(message));
    if fired.is_empty() {
        None
    } else {
        Some(format!("[THOR] {}", fired.join("  ||  ")))
    }
}

// ---------------------------------------------------------------- opt-in
//
// Two capabilities `Rule`/`parse_rules`/`evaluate`/`block_reason` above
// cannot express, added here without changing one byte of them: a
// POSITION-aware escape, and a WARN tier. Not wired into `block_reason`
// itself and not used by any caller yet - a rulebook has to opt in by
// adding a `before` and/or `tier` key before either does anything, and
// today's live rulebook adds neither (proven, not assumed - see
// `serve/tests/response_guard_live_rulebook_unchanged.rs`).
//
// WHY A NEW TYPE (`OptInRule`) INSTEAD OF NEW FIELDS ON `Rule` ITSELF: this
// module is not the only place a `Rule` gets built. `capture.rs`'s
// `to_respond_rule` constructs one with a plain struct literal naming
// exactly today's six fields, to run its OWN, unrelated fallback rulebook
// through this matcher (SPEC-ENFORCEMENT.md section 0: "the capture guard
// REUSES this matcher... it does not get a matcher of its own"). A Rust
// struct literal must name every field of the type; a new REQUIRED field on
// `Rule` stops that call site compiling, and `capture.rs` is out of this
// task's edit scope. A second, separate type that WRAPS a `Rule` sidesteps
// this: `Rule`, `parse_rules`, `evaluate` and `block_reason` are untouched
// below this line, so every existing caller - `capture.rs`,
// `serve/src/bin/serve.rs`, and this file's own pre-existing tests above -
// keeps compiling and behaving exactly as it does today.
//
// POSITION - the shape chosen (`before: Vec<String>`, anchored to the
// rule's own `any_of`), and why over two alternatives considered:
//
// - A fixed "opening window" (does term X occur within the first N
//   characters), independent of where the trigger term sits. Rejected: it
//   cannot tell "TLDR: ... commit ..." (compliant) apart from "commit ...
//   TLDR: ..." (not compliant) when both markers land inside the same
//   window - it checks presence-in-a-region, not ORDER, and the defect this
//   exists to fix (RESPONSE-GUARD-BLIND.md's b01 false block) and this
//   task's own required tests are both specifically about order: the same
//   two words, swapped, must flip the verdict. A window check cannot
//   express that on its own.
// - A fully general "A before B" primitive taking two independently-named
//   term lists, rather than reusing `any_of`. More reusable in the
//   abstract, but it is a second vocabulary to read, validate and default
//   on every rule, for a relation this rulebook shape already has half of:
//   `any_of` already IS "the trigger vocabulary" for any rule that has one.
//   Anchoring `before` to `any_of`'s own earliest hit reuses that instead of
//   naming the same terms twice, and is the smaller shape for the one case
//   this task asks to fix. A fully general two-list version could still be
//   added later without breaking this one - the same way this one was added
//   without breaking `Rule`.
//
// `before` empty (every rulebook shipped so far, since the field does not
// exist in any of them) means no positional constraint at all - the rule
// evaluates exactly as `evaluate` would score it. Byte offsets from
// `str::find` (not char offsets) are compared throughout: correct for
// ordering purposes, since UTF-8 byte order and char order agree for any
// valid string, and `min_chars` above is the only place that ever needed a
// true char COUNT rather than an offset.
//
// WARN TIER - `tier: Tier`, defaulting to `Tier::Block`: SPEC-ENFORCEMENT.md
// section 1.1's three verdicts (BLOCK/WARN/ALLOW), restricted to the two a
// rulebook rule can choose between (ALLOW is simply "did not fire", which
// needs no field of its own). Missing OR unrecognised -> `Block`,
// deliberately stricter defaulting than `capture::parse_rulebook`'s (which
// DROPS a rule that carries no valid tier): every rule shipped so far has no
// `tier` key at all and blocks when it fires, and this parser has to keep
// meaning that, byte for byte - dropping instead of defaulting would
// silently turn every one of today's six live rules into dead weight the
// moment this parser is used at all.
//
// HOW A WARN VERDICT REACHES THE CALLER: `guard_verdict` below returns it as
// `GuardVerdict::warn_reason`, formatted exactly like `block_reason`'s own
// return value. That is as far as this file can take it. Actually
// surfacing that text to the owner without stopping the reply needs a
// change in `serve/src/bin/serve.rs`, outside this task's edit scope - see
// the task report for precisely what and where.

/// BLOCK (today's only behaviour, and the default) or WARN
/// (SPEC-ENFORCEMENT.md 1.1: emit text, never stop the reply).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    Block,
    Warn,
}

impl Tier {
    fn from_json(s: &str) -> Option<Self> {
        match s {
            "block" => Some(Tier::Block),
            "warn" => Some(Tier::Warn),
            _ => None,
        }
    }
}

/// A `Rule` plus the two capabilities it cannot express on its own - see
/// this section's own doc comment for why this is a separate type from
/// `Rule` rather than new fields on it. Parsed by `parse_opt_in_rules`,
/// matched by `evaluate_opt_in`.
#[derive(Debug, Clone)]
pub struct OptInRule {
    pub base: Rule,
    /// Terms whose earliest occurrence, if strictly before the earliest
    /// occurrence of whichever `any_of` term matched, suppresses firing -
    /// the rule is then treated as compliant. Empty = no positional
    /// constraint (today's behaviour).
    pub before: Vec<String>,
    /// Defaults to `Block` when the rulebook carries no `tier` key, or one
    /// this parser does not recognise.
    pub tier: Tier,
    /// Named STRUCTURAL escapes - recognised by shape, not by literal
    /// substring - that suppress firing exactly like a `none_of` hit. See
    /// `pattern_matches` for the recognised names (today: `"commit_sha"`,
    /// `"path_line"`) and FALSE BLOCK B in this section's own doc comment
    /// for the defect this exists to fix. An unrecognised name matches
    /// nothing (fail-soft, same stance as everywhere else in this parser) -
    /// it never errors and never grants an escape "by accident". Empty
    /// (every rulebook before this field existed) means no such escape,
    /// unchanged from today.
    pub none_of_patterns: Vec<String>,
    /// Terms whose presence in the OWNER'S OWN LAST PROMPT (never in the
    /// reply being judged - see `last_user_prompt` above), combined with the
    /// reply itself actually reading as a list (`is_mostly_list_lines`),
    /// suppress firing. See FALSE BLOCK A in this section's own doc comment.
    /// Empty (every rulebook before this field existed) means no such
    /// exemption, unchanged from today.
    pub list_request_any_of: Vec<String>,
    /// Terms whose presence in the OWNER'S OWN LAST PROMPT mean he explicitly
    /// asked for a summary or an overview ("samenvatten", "overzicht",
    /// "summarize", "overview", "list"). Unlike `list_request_any_of` this
    /// does not ask the reply to be list-shaped: a summary is often prose. It
    /// raises the length at which the rule fires to `overview_max_chars`, so a
    /// reply that IS that overview may run longer, but not without end. Empty
    /// or `overview_max_chars` 0 (every rulebook before this field existed)
    /// means no such allowance.
    pub overview_request_any_of: Vec<String>,
    /// The length (in characters) up to which a reply to an explicit request
    /// for a summary or overview is left alone - see `overview_request_any_of`.
    pub overview_max_chars: usize,
    /// `true` marks this rule as being about the OWNER's own reading of a
    /// reply, not about whether the agent was honest or diligent - skipped
    /// for a subagent payload (`evaluate_opt_in`'s own `is_subagent`
    /// parameter) as if the rule had not matched at all. Missing, `false`,
    /// or a malformed value all mean the same thing: this rule keeps
    /// applying to everyone, subagent included. See this section's own
    /// "READER SCOPE" doc comment, further down, for why that is the
    /// direction this field fails in, and `parse_opt_in_rules` for exactly
    /// how a malformed value is read as `false` rather than rejected.
    pub owner_reading_only: bool,
    /// `true` marks this rule as being about the assistant's OWN claims: a
    /// trigger term counts only where the assistant says it in its own voice,
    /// never where it reports or quotes what another party said. See this
    /// file's own "reported speech" section. Missing, `false` or malformed all
    /// mean the rule reads every occurrence, as before this field existed.
    pub own_claims_only: bool,
    /// `true` marks this rule as being about something the assistant CLAIMS
    /// is so ("pushed", "deployed", "now running"): a trigger term does not
    /// count where a negation just before it turns it into the opposite claim
    /// ("niets gepusht of gedeployd"). See this file's own "negated claims"
    /// section. Missing, `false` or malformed all mean the rule reads every
    /// occurrence, as before this field existed.
    pub affirmative_only: bool,
    /// `true` marks a rule whose trigger words are a PHRASE the assistant must
    /// not use ("nog te controleren"): where the phrase sits inside quotation
    /// marks or inline code it is mentioned, not used, whoever is speaking -
    /// even in the assistant's own first-person sentence ('what I would once
    /// have written as "nog te controleren"'). Missing, `false` or malformed
    /// all mean quotes are read as before (field test 15, collect 17c).
    pub quotes_exempt: bool,
    /// Terms whose presence in the OWNER'S OWN LAST PROMPT means he asked for
    /// what the rule's reminder tells the writer to leave out (step 29: the
    /// length rule says "no test counts", and the prompt may be "give me the
    /// real test result"). Then `reminder_if_asked` is said instead of the
    /// reminder. Empty, or no prompt that could be read: the reminder as ever.
    pub asked_for_any_of: Vec<String>,
    /// What is said instead of the reminder when `asked_for_any_of` matches the
    /// owner's prompt.
    pub reminder_if_asked: Option<String>,
    /// What this rule says when another rule of the same tier fires on the same
    /// reply (step 33): each requirement once, short, instead of two full
    /// reminders that repeat each other. Absent: the reminder as ever.
    pub short_reminder: Option<String>,
    /// `true` marks a rule about a claim of having looked at something ("checked",
    /// "cannot reach"): it does not fire when the turn itself looked at
    /// something THE CLAIM NAMES (`turn_backs_claim`: a shell command, a read, a
    /// search or a page of the turn shares a naming word with the claim - a file
    /// name, a command word, an address). Launching an agent or writing does not
    /// count, so a claim relayed from an agent's report, or made in a turn that
    /// looked at nothing, is still held; so is one whose turn looked only at
    /// something else. Known limit, stated here rather than rediscovered: a
    /// shared word is not proof that the call checked the claim.
    /// Missing, `false` or malformed all mean the rule reads every claim.
    pub backed_by_turn_tools: bool,
    /// Words of which the OWNER'S OWN LAST PROMPT must carry at least one for
    /// the rule to apply at all: "do not bounce his commit request back at him"
    /// is only a defect when he asked for a commit. Empty, or a prompt that
    /// could not be read: the rule applies as ever.
    pub only_if_prompt_any_of: Vec<String>,
    /// Single words that, standing earlier in the SAME sentence as a trigger
    /// term, turn that occurrence into a mention of a future or conditional
    /// state ("controleer ik of alles klopt", "once everything is verified"),
    /// not a claim. Whole words, case-insensitive. Empty: no such reading.
    pub hedge_words_before: Vec<String>,
    /// Single words that, standing later in the same clause as a trigger term
    /// (up to the next full stop, colon, semicolon or line break), say what
    /// the claim covers or leaves out ("alles klopt op de totalen", "all
    /// verified except the build"): a claim that names its scope is not an
    /// absolute one. Empty: no such reading.
    pub scope_words_after: Vec<String>,
    /// `true` reads a trigger term on a list line that opens with a label and
    /// a colon ("- **Deploy config:** alles klopt") as scoped by that label.
    /// Missing, `false` or malformed all mean the label scopes nothing.
    pub list_label_scopes: bool,
}

/// Parse the rulebook into `OptInRule`s: every field `parse_rules` reads,
/// plus `before`, `tier`, `none_of_patterns`, `list_request_any_of` and
/// `owner_reading_only`. Same fail-open stance as `parse_rules` - malformed
/// JSON, or an entry missing `reminder`, drops that entry rather than
/// erroring.
pub fn parse_opt_in_rules(text: &str) -> Vec<OptInRule> {
    let Ok(Value::Array(arr)) = serde_json::from_str::<Value>(text) else {
        return vec![];
    };
    arr.iter()
        .filter_map(|r| {
            let base = parse_one_rule(r)?;
            let before = str_list(r, "before");
            let tier =
                r.get("tier").and_then(|v| v.as_str()).and_then(Tier::from_json).unwrap_or(Tier::Block);
            let none_of_patterns = str_list(r, "none_of_patterns");
            let list_request_any_of = str_list(r, "list_request_any_of");
            let overview_request_any_of = str_list(r, "overview_request_any_of");
            let overview_max_chars = r.get("overview_max_chars").and_then(|v| v.as_u64()).map_or(0, |n| n as usize);
            // `.as_bool()` is `None` for anything that is not a literal JSON
            // `true`/`false` - absent, a string, a number, an object, an
            // array - and `unwrap_or(false)` reads every one of those the
            // SAME way the field's total absence already reads: this rule is
            // NOT owner-reading-only, so it keeps applying to everyone. See
            // this file's own "READER SCOPE" doc comment, above
            // `evaluate_opt_in`, for why a malformed value must fail toward a
            // rule applying rather than toward it going quiet.
            let owner_reading_only =
                r.get("owner_reading_only").and_then(|v| v.as_bool()).unwrap_or(false);
            // Same direction as `owner_reading_only`: anything but a literal
            // `true` keeps the rule reading every occurrence of its terms.
            let own_claims_only = r.get("own_claims_only").and_then(|v| v.as_bool()).unwrap_or(false);
            let affirmative_only = r.get("affirmative_only").and_then(|v| v.as_bool()).unwrap_or(false);
            let quotes_exempt = r.get("quotes_exempt").and_then(|v| v.as_bool()).unwrap_or(false);
            let asked_for_any_of = str_list(r, "asked_for_any_of");
            let reminder_if_asked = r.get("reminder_if_asked").and_then(|v| v.as_str()).map(str::to_string);
            let short_reminder = r.get("short_reminder").and_then(|v| v.as_str()).map(str::to_string);
            let backed_by_turn_tools = r.get("backed_by_turn_tools").and_then(|v| v.as_bool()).unwrap_or(false);
            let only_if_prompt_any_of = str_list(r, "only_if_prompt_any_of");
            let hedge_words_before = str_list(r, "hedge_words_before");
            let scope_words_after = str_list(r, "scope_words_after");
            let list_label_scopes = r.get("list_label_scopes").and_then(|v| v.as_bool()).unwrap_or(false);
            Some(OptInRule {
                base,
                before,
                tier,
                none_of_patterns,
                list_request_any_of,
                overview_request_any_of,
                overview_max_chars,
                owner_reading_only,
                own_claims_only,
                affirmative_only,
                quotes_exempt,
                asked_for_any_of,
                reminder_if_asked,
                short_reminder,
                backed_by_turn_tools,
                only_if_prompt_any_of,
                hedge_words_before,
                scope_words_after,
                list_label_scopes,
            })
        })
        .collect()
}

/// The byte offset (see this section's own doc comment for why that is the
/// right unit here) of the earliest occurrence of any of `terms` in
/// `haystack_lower`, or `None` if none occur at all.
fn earliest_index(terms: &[String], haystack_lower: &str) -> Option<usize> {
    terms.iter().filter_map(|t| haystack_lower.find(&t.to_lowercase())).min()
}

/// Whether `rule.before` positionally excuses a match that `rule_matches`
/// already confirmed: true when at least one `before` term occurs strictly
/// earlier than the earliest matched `any_of` term. `before` empty, or no
/// `any_of` term to measure against (nothing for a bare `all_of` rule to be
/// "before"), both mean the constraint does not apply - never an escape,
/// exactly as if `before` were empty.
fn positionally_escaped(rule: &OptInRule, haystack_lower: &str) -> bool {
    if rule.before.is_empty() {
        return false;
    }
    let Some(trigger_at) = earliest_index(&rule.base.any_of, haystack_lower) else {
        return false;
    };
    rule.before.iter().any(|t| haystack_lower.find(&t.to_lowercase()).is_some_and(|i| i < trigger_at))
}

// ------------------------------------------------------- pattern escapes
//
// FALSE BLOCK B, reported by the owner from other sessions, fixed
// 2026-09-07: `checked-claim-needs-evidence` refused an answer that named
// five real commit shas ("commits a2770f8e (...), be1e8b3b (...), ...") and
// two real files, because its `none_of` escape is pure literal-substring
// matching and named the trigger word as "commit " (a trailing space) - this
// answer said "commits" (plural), so the literal never matched, and none of
// the ten hand-listed file extensions (`.py:`, `.rs:`, ...) matched either
// since neither file mentioned was followed by a colon at all. The rule's
// OWN reminder text already states what should have counted: "Noem het
// bestand met regelnummer (pad:regel) of de commit" - a file WITH a line
// number (path:line), or the commit (a sha) - so that is exactly, and only,
// what these two patterns detect. A bare path with an extension but no line
// number, or a path merely sitting near an unrelated concrete token (a
// version number, a quoted identifier), deliberately does NOT count: the
// rule's own reminder never asked for that, and accepting it would let
// nearly any answer that names a filename in passing through unverified,
// which is the opposite of what this rule exists to catch. This is a
// decision, not an oversight - see this task's own report for the fixture
// that would have needed it and why it was left out.

/// A commit sha anywhere in the text: 7 to 40 ASCII hex characters (case
/// insensitive), word-bounded (so this can only ever match a WHOLE token -
/// a 6-character or a 41+-character run of hex characters never matches at
/// any length inside it, because `\b` cannot land in the middle of a longer
/// run of word characters), containing at least one digit AND at least one
/// letter. The digit+letter mix is checked in plain Rust rather than the
/// regex itself: the `regex` crate (this workspace's only regex engine,
/// deliberately - see its own README) has no lookaround, so "at least one of
/// each" cannot be expressed as part of the pattern; checked this way it is
/// exact rather than approximated, and needs no lookaround to begin with.
/// This is what tells "a2770f8e" (a real short sha: five digits, three
/// letters) apart from an all-digit run (a count, a version, a phone number -
/// no letters at all) or an all-letter run (an ordinary word - `facade` and
/// `deedbead` are both valid hex-alphabet words with no digit at all).
fn commit_sha_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)\b[0-9a-f]{7,40}\b").unwrap())
}

fn contains_commit_sha(haystack_lower: &str) -> bool {
    commit_sha_regex().find_iter(haystack_lower).any(|m| {
        let s = m.as_str();
        s.bytes().any(|b| b.is_ascii_digit()) && s.bytes().any(|b| b.is_ascii_alphabetic())
    })
}

/// A `path:line` citation anywhere in the text: one or more path-shaped
/// characters (letters, digits, `_ . / \ -`), a short alphanumeric
/// extension, then `:` and a run of digits - `server/public/dashboard.html:42`,
/// `gate.rs:610`. Deliberately a SHAPE check rather than the fixed,
/// hand-enumerated extension list `checked-claim-needs-evidence`'s own
/// `none_of` already carries (`.py:`, `.rs:`, ... ten languages) so a real
/// citation in any OTHER extension - `.yaml:12`, `.toml:3`, one nobody
/// thought to list by hand - is recognised too. Additive, not a replacement:
/// the existing fixed list stays exactly as it was.
fn path_line_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"[a-z0-9_./\\-]+\.[a-z0-9]{1,8}:\d+").unwrap())
}

/// One `none_of_patterns` name resolved against the haystack. An
/// unrecognised name matches nothing - see `OptInRule::none_of_patterns`'s
/// own doc comment for why that is fail-soft rather than an error.
fn pattern_matches(name: &str, haystack_lower: &str) -> bool {
    match name {
        "commit_sha" => contains_commit_sha(haystack_lower),
        "path_line" => path_line_regex().is_match(haystack_lower),
        _ => false,
    }
}

/// Whether any of `rule`'s `none_of_patterns` is present - same suppressing
/// effect as a literal `none_of` hit, just resolved by shape instead of by
/// substring. Empty `none_of_patterns` always yields `false`, exactly as if
/// the field did not exist.
fn pattern_escaped(rule: &OptInRule, haystack_lower: &str) -> bool {
    rule.none_of_patterns.iter().any(|name| pattern_matches(name, haystack_lower))
}

/// ------------------------------------------------------- checked evidence
//
// THE ESCAPE THIS CLOSES (found 2026-09-26). `checked-claim-needs-evidence`
// accepts a `path:line` citation as evidence, twice over: the `path_line`
// pattern above, and literal `none_of` terms such as ".rs:". Both only look at
// the SHAPE, so an answer could type any file name and any line number, never
// having opened either, and walk through a rule whose whole job is to catch a
// claim of checking that nobody checked.
//
// So when the caller can look (it knows the session's working directory and
// hands over an `EvidenceLookup`), a citation only counts when it names a real
// file under that directory and a line that file really has. A citation that
// does not check out counts for nothing: it is blanked from the text the
// rule's escapes are read in, so neither the pattern nor a literal term inside
// it (".rs:", a hex-looking file name the `commit_sha` shape would take for a
// sha) can let the answer through.
//
// THE SAME DAY, THE REST OF THE ESCAPES. The rule's other evidence words were
// still read by shape: the literals "commit ", "line " and "regel " (anywhere -
// "pipeline " carries "line ", "vuistregel " carries "regel "), any typed
// sha-shaped token through `commit_sha`, and a bare "name.rs:" with no line at
// all through the ".rs:" literal. When the caller can look, each of them now
// counts only when it is real, the way a citation does:
// - a sha-shaped token, and the literal "commit", count only when a token names
//   a commit that exists in the working directory's git repository. Where no
//   repository answers (no git, no repository there), a sha cannot be looked up
//   and is judged by its shape, as before - the same stance as a citation with
//   no working directory to look in;
// - the literals "line"/"regel" count only for a line number that a file named
//   on the same line of the answer really has ("line 3 of src/lib.rs", "regel 4
//   van src/lib.rs", "src/lib.rs, line 3");
// - an extension literal such as ".rs:" counts only inside a citation that
//   checked out.
// Everything else stays as it was: the honest escapes ("not checked"), and a
// rule that names neither `path_line` nor `commit_sha` among its
// `none_of_patterns` - it never treated a citation or a sha as evidence, so
// there is nothing to check. Without a way to look, every escape is judged by
// its shape exactly as before.
//
// KNOWN LIMIT, stated rather than left to be rediscovered: a commit of ANOTHER
// repository than the working directory's does not count, the same way a file
// outside the working directory does not. An answer about another repository
// names a real line under the working directory, or says plainly that it was
// not checked.
//
// This file does no I/O of its own: the lookups are the caller's (see
// `serve::evidence`, `cited_line_exists` and `real_commits`: the Stop hook and the sweep use the same two).

/// How the caller looks evidence up, handed in because this file does no I/O.
pub struct EvidenceLookup<'a> {
    /// Whether `path` names a real file under the session's working directory
    /// with at least `line` lines.
    pub line_exists: &'a dyn Fn(&str, u64) -> bool,
    /// Which of `candidates` (lowercase sha-shaped tokens) name a real commit
    /// in the working directory's git repository - `None` when no repository
    /// answers, so the caller cannot look.
    pub real_commits: &'a dyn Fn(&[String]) -> Option<HashSet<String>>,
}

/// At most this many distinct sha-shaped tokens, and this many line-number
/// lookups, are tried for one answer: enough for any real report, and a bound
/// on the work one hook call can be made to do.
const MAX_SHA_LOOKUPS: usize = 16;
const MAX_LINE_LOOKUPS: usize = 24;

/// A `path:line` citation as written in the ORIGINAL reply (a file name keeps
/// its case, which matters on most systems): the same shape as `path_line`,
/// plus an optional drive letter in front so an absolute Windows path is read
/// whole.
fn citation_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)(?:\b[a-z]:)?[a-z0-9_./\\-]+\.[a-z0-9]{1,8}:\d+").unwrap())
}

/// "line 3", "lines 3-9", "regel 4", "regels 4 tot 9": the number a line
/// reference names.
fn line_reference_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)\b(?:lines?|regels?)\s+(\d{1,12})\b").unwrap())
}

/// A file name with an extension, as a line reference names it: the citation
/// shape without its `:line`.
fn file_name_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)(?:\b[a-z]:)?[a-z0-9_./\\-]*[a-z0-9_]\.[a-z0-9]{1,8}\b").unwrap())
}

/// What a reply's evidence turned out to prove.
struct CheckedEvidence {
    /// The citations that name a real line of a real file, lowercased.
    verified_citations: Vec<String>,
    /// Whether a sha-shaped token names a real commit: `Some` when the caller
    /// could look, `None` when it could not (judged by shape then).
    real_sha: Option<bool>,
    /// Whether a "line N"/"regel N" names a line a file on the same line of
    /// the answer really has.
    real_line_reference: bool,
    /// What did not check out, as written, for the reason.
    unverified: Vec<String>,
    /// The reply with every unverified citation blanked, as a haystack - what
    /// a verifying rule's escapes are read in.
    escapes_lower: String,
}

fn check_evidence(message: &str, lookup: &EvidenceLookup) -> CheckedEvidence {
    let mut verified_citations = Vec::new();
    let mut unverified = Vec::new();
    let mut kept = String::with_capacity(message.len());
    let mut last = 0;
    for m in citation_regex().find_iter(message) {
        let (path, line) = m.as_str().rsplit_once(':').expect("the pattern always ends in ':' and digits");
        // A number too long for u64 names no real line either.
        if (lookup.line_exists)(path, line.parse::<u64>().unwrap_or(u64::MAX)) {
            verified_citations.push(m.as_str().to_lowercase());
            continue;
        }
        unverified.push(m.as_str().to_string());
        kept.push_str(&message[last..m.start()]);
        kept.push(' ');
        last = m.end();
    }
    kept.push_str(&message[last..]);
    let escapes_lower = haystack(&kept);

    let real_line_reference = check_line_references(message, lookup, &mut unverified);

    let mut candidates: Vec<String> = Vec::new();
    for m in commit_sha_regex().find_iter(&escapes_lower) {
        let s = m.as_str();
        let shaped = s.bytes().any(|b| b.is_ascii_digit()) && s.bytes().any(|b| b.is_ascii_alphabetic());
        // A real sha can be all digits (its first eight characters are, one
        // commit in forty-three); written next to "commit" or "sha" it is
        // asked of the repository, which is the judge. Bare, it is a number.
        if (shaped || named_as_a_commit(&escapes_lower, m.start())) && !candidates.iter().any(|c| c == s) && candidates.len() < MAX_SHA_LOOKUPS {
            candidates.push(s.to_string());
        }
    }
    // Asked even with no candidates: whether a repository answers at all
    // decides how the bare word "commit" is read.
    let real_sha = (lookup.real_commits)(&candidates).map(|real| {
        for c in candidates.iter().filter(|c| !real.contains(*c)) {
            unverified.push(format!("{c} (no such commit in this repository)"));
        }
        !real.is_empty()
    });
    CheckedEvidence { verified_citations, real_sha, real_line_reference, unverified, escapes_lower }
}

/// Whether the text just before `start` is the word commit, sha, hash, rev or
/// revision (white space, a colon, a hash sign, quotes, backticks and an open
/// bracket allowed in between), as a whole word.
fn named_as_a_commit(haystack_lower: &str, start: usize) -> bool {
    let before = haystack_lower[..start].trim_end_matches(|c: char| c.is_whitespace() || ":#`'\"(".contains(c));
    ["commit", "sha", "hash", "revision", "rev"].iter().any(|word| {
        before.strip_suffix(word).is_some_and(|rest| !rest.chars().next_back().is_some_and(char::is_alphanumeric))
    })
}

/// Whether some "line N"/"regel N" in `message` names a line that a file named
/// on the same line of the answer really has. A reference that names no file
/// there, or none that has that line, goes into `unverified`.
fn check_line_references(message: &str, lookup: &EvidenceLookup, unverified: &mut Vec<String>) -> bool {
    let mut lookups = 0usize;
    for m in line_reference_regex().captures_iter(message) {
        let whole = m.get(0).expect("group 0 always exists");
        let line: u64 = m[1].parse().unwrap_or(u64::MAX);
        let text_line_start = message[..whole.start()].rfind('\n').map_or(0, |i| i + 1);
        let text_line_end = message[whole.end()..].find('\n').map_or(message.len(), |i| whole.end() + i);
        for file in file_name_regex().find_iter(&message[text_line_start..text_line_end]) {
            if lookups >= MAX_LINE_LOOKUPS {
                return false;
            }
            lookups += 1;
            if (lookup.line_exists)(file.as_str(), line) {
                return true;
            }
        }
        unverified.push(whole.as_str().to_string());
    }
    false
}

/// Whether this rule takes a citation or a sha as evidence, which is what
/// makes its evidence worth checking.
fn takes_evidence(rule: &OptInRule) -> bool {
    rule.none_of_patterns.iter().any(|p| p == "path_line" || p == "commit_sha")
}

/// `pattern_escaped` for a rule whose evidence was checked: `path_line` holds
/// only for a citation that checked out, `commit_sha` only for a real commit
/// (by shape when no repository answers), and every other shape is read in
/// the reply with the unverified citations blanked.
fn pattern_escaped_checked(rule: &OptInRule, checked: &CheckedEvidence) -> bool {
    rule.none_of_patterns.iter().any(|name| match name.as_str() {
        "path_line" => !checked.verified_citations.is_empty(),
        "commit_sha" => checked.real_sha.unwrap_or_else(|| contains_commit_sha(&checked.escapes_lower)),
        other => pattern_matches(other, &checked.escapes_lower),
    })
}

/// The evidence a literal `none_of` term stands for, when it stands for any:
/// the word "commit", a line word, or a file extension followed by a colon.
enum EvidenceWord {
    Commit,
    Line,
    ExtensionColon,
}

fn evidence_word(term_lower: &str) -> Option<EvidenceWord> {
    match term_lower.trim() {
        "commit" | "commits" => Some(EvidenceWord::Commit),
        "line" | "lines" | "regel" | "regels" => Some(EvidenceWord::Line),
        t if t.len() >= 3
            && t.starts_with('.')
            && t.ends_with(':')
            && t[1..t.len() - 1].chars().all(|c| c.is_ascii_alphanumeric()) =>
        {
            Some(EvidenceWord::ExtensionColon)
        }
        _ => None,
    }
}

/// The `none_of` side of a rule whose evidence was checked: an evidence word
/// counts only when what it stands for is real (see this section's own
/// comment); every other term is read in the reply with the unverified
/// citations blanked, as before.
fn none_of_escaped_checked(rule: &Rule, checked: &CheckedEvidence) -> bool {
    rule.none_of.iter().any(|term| {
        let t = term.to_lowercase();
        match evidence_word(&t) {
            Some(EvidenceWord::Commit) => checked.real_sha.unwrap_or_else(|| checked.escapes_lower.contains(&t)),
            Some(EvidenceWord::Line) => checked.real_line_reference,
            Some(EvidenceWord::ExtensionColon) => checked.verified_citations.iter().any(|c| c.contains(&t)),
            None => checked.escapes_lower.contains(&t),
        }
    })
}

// -------------------------------------------------------- reported speech
//
// THE FALSE BLOCK THIS CLOSES (reported by the owner, 2026-09-26). The rule
// that demands "verify before claiming you cannot" held an answer in which the
// assistant QUOTED what another model had said ("het model meldde geen toegang
// te hebben") and claimed nothing itself. Its trigger is a plain substring, so
// it cannot tell the assistant's own "I cannot" from a report of someone
// else's.
//
// A rule opts in with `own_claims_only: true` (no rule changes by itself: a
// rule about the SHAPE of a reply, such as the summary or the length rule,
// still reads every word, whoever said it). For such a rule, one occurrence of
// a trigger term does not count when it sits in another party's words:
// - REPORTED: earlier in the same sentence a reporting word ("meldde", "zei",
//   "volgens", "said", "reported", "according to", ...) introduces it, with no
//   clause boundary (a comma, a semicolon, "en", "maar", "and", "but", ...)
//   between that word and the trigger, and no first-person word ("ik", "we",
//   "I", "my", ...) in the clause of the reporting word - nor in the trigger
//   itself, unless a colon after the reporting word has opened the other
//   party's own words ("the agent said: I don't have access");
// - QUOTED: it sits inside quotation marks, inline code, a blockquote or a
//   fenced block, and the words framing the quote (the clause before it, or an
//   attribution right after it such as '"...", zei ik') carry no first-person
//   word.
// Everything else is the assistant's own voice, and the rule fires on it as
// before. The rule fires when at least one occurrence is in the assistant's
// own voice, so a report next to a real claim still fires.
//
// Deliberately biased toward firing: a first-person word anywhere in the
// frame keeps the occurrence the assistant's own, even where a person would
// read it as a report ("het model zei dat ik geen toegang had"), and a
// reporting word cut off by a comma no longer governs what follows. A false
// block costs a rewrite; a real "I cannot" slipping through is what the rule
// exists to stop. One gap the other way, also deliberate: a quotation with no
// frame at all ('de NAS gaf "geen toegang"') counts as a quote, because a
// quotation mark is itself the sign of a mention.

/// The marks a quote is opened and closed with: straight and curly double
/// quotes, the low opening quote, guillemets, and a backtick.
const QUOTE_MARKS: &[char] = &['"', '\u{201c}', '\u{201d}', '\u{201e}', '\u{ab}', '\u{bb}', '`'];

/// Words that make the speaker the assistant itself (or a group it is in).
const FIRST_PERSON: &[&str] =
    &["ik", "mij", "me", "mijn", "m'n", "wij", "we", "ons", "onze", "i", "i'm", "i've", "i'd", "i'll", "my", "mine", "us", "our"];

/// One-word reporting words: another party's words follow.
const REPORTING_WORDS: &[&str] = &[
    "meldde", "meldden", "meldt", "melden", "vermeldde", "vermeldt", "zei", "zeiden", "zegt", "zeggen", "schreef",
    "schreven", "schrijft", "antwoordde", "antwoordden", "antwoordt", "beweerde", "beweerden", "beweert", "claimde",
    "claimt", "rapporteerde", "rapporteert", "verklaarde", "verklaart", "volgens", "aldus", "concludeerde",
    "concludeert", "dacht", "denkt", "klaagde", "waarschuwde", "said", "says", "reported", "reports", "claimed",
    "claims", "wrote", "writes", "replied", "replies", "answered", "answers", "stated", "states", "told", "tells",
    "concluded", "concludes", "thought", "thinks", "believed", "believes", "complained", "insisted", "responded",
    "responds", "mentioned", "mentions", "warned",
];

/// Two-word reporting phrases.
const REPORTING_PHRASES: &[(&str, &str)] =
    &[("gaf", "aan"), ("geeft", "aan"), ("liet", "weten"), ("laat", "weten"), ("according", "to")];

/// Reporting words that open an attribution phrase, closed by a comma that is
/// no clause boundary ("Volgens de agent, ...", "According to the log, ...").
const ATTRIBUTION_WORDS: &[&str] = &["volgens", "aldus", "according"];

/// Words that open a new clause, cutting a reporting word off from what follows.
const CLAUSE_WORDS: &[&str] = &["en", "maar", "want", "dus", "and", "but", "so"];

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '\'' || c == '\u{2019}'
}

/// The words of `text` with their byte offsets (start, end), a curly
/// apostrophe read as a straight one.
fn words(text: &str) -> Vec<(usize, usize, String)> {
    let mut out = Vec::new();
    let mut start: Option<usize> = None;
    for (i, c) in text.char_indices() {
        match (is_word_char(c), start) {
            (true, None) => start = Some(i),
            (false, Some(s)) => {
                out.push((s, i, text[s..i].replace('\u{2019}', "'")));
                start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = start {
        out.push((s, text.len(), text[s..].replace('\u{2019}', "'")));
    }
    out
}

fn has_first_person(text: &str) -> bool {
    words(text).iter().any(|(_, _, w)| FIRST_PERSON.contains(&w.as_str()))
}

/// The byte span of the LAST reporting word or phrase in `text`.
fn last_reporting_word(text: &str) -> Option<(usize, usize)> {
    let ws = words(text);
    let mut found = None;
    for (i, (start, end, w)) in ws.iter().enumerate() {
        if REPORTING_WORDS.contains(&w.as_str()) {
            found = Some((*start, *end));
        }
        if let Some((_, next_end, next)) = ws.get(i + 1) {
            if REPORTING_PHRASES.iter().any(|(a, b)| a == w && b == next) {
                found = Some((*start, *next_end));
            }
        }
    }
    found
}

fn starts_with_reporting_word(text: &str) -> bool {
    let ws = words(text);
    match ws.first() {
        Some((_, _, w)) if REPORTING_WORDS.contains(&w.as_str()) => true,
        Some((_, _, w)) => {
            ws.get(1).is_some_and(|(_, _, next)| REPORTING_PHRASES.iter().any(|(a, b)| a == w && b == next))
        }
        None => false,
    }
}

/// The byte offset just past the last clause boundary in `text` (a comma, a
/// semicolon, or a clause word), or 0.
fn last_clause_start(text: &str) -> usize {
    let punct = text.rfind([',', ';']).map_or(0, |i| i + 1);
    let word =
        words(text).iter().rev().find(|(_, _, w)| CLAUSE_WORDS.contains(&w.as_str())).map_or(0, |(_, end, _)| *end);
    punct.max(word)
}

fn has_clause_boundary(text: &str) -> bool {
    text.contains([',', ';']) || words(text).iter().any(|(_, _, w)| CLAUSE_WORDS.contains(&w.as_str()))
}

/// The frame the last reporting word in `lead` opens: the words from the start
/// of its clause up to where the other party's words begin, and whether a
/// colon opened them. `None` when there is no reporting word, or a clause
/// boundary after it cuts it off from the end of `lead` - except the one comma
/// that closes an attribution phrase ("According to the agent, ...").
fn reporting_frame(lead: &str) -> Option<(&str, bool)> {
    let (word_start, word_end) = last_reporting_word(lead)?;
    let after_word = &lead[word_end..];
    let opens_attribution = words(&lead[word_start..word_end])
        .first()
        .is_some_and(|(_, _, w)| ATTRIBUTION_WORDS.contains(&w.as_str()));
    let cut_off =
        if opens_attribution { has_clause_boundary(&after_word.replacen(',', " ", 1)) } else { has_clause_boundary(after_word) };
    if cut_off {
        return None;
    }
    let clause_start = last_clause_start(&lead[..word_start]);
    Some(match after_word.find(':') {
        Some(colon) => (&lead[clause_start..word_end + colon], true),
        None => (&lead[clause_start..], false),
    })
}

/// Whether a reporting word in `lead` (the sentence up to the trigger) hands
/// `trigger` to another party - see this section's own comment.
fn reported_by_another_party(lead: &str, trigger: &str) -> bool {
    match reporting_frame(lead) {
        Some((frame, speech_opened)) => !has_first_person(frame) && (speech_opened || !has_first_person(trigger)),
        None => false,
    }
}

/// Whether the words framing a quote leave it another party's: no
/// first-person word in the clause before it (from the reporting word's clause
/// when there is one), and no first-person word in an attribution right after
/// it ('"...", zei ik').
fn quote_frame_is_another_party(before: &str, after: &str) -> bool {
    let frame = reporting_frame(before).map_or(&before[last_clause_start(before)..], |(frame, _)| frame);
    if has_first_person(frame) {
        return false;
    }
    let attribution = after.trim_start_matches(|c: char| c.is_whitespace() || ",.:;!?".contains(c));
    let attribution = &attribution[..attribution.find([',', ';', '.', '!', '?', '\n']).unwrap_or(attribution.len())];
    !(starts_with_reporting_word(attribution) && has_first_person(attribution))
}

/// `text` with the inside of every closed quote left out, so a quote's own
/// words are never read as its frame's.
fn blank_closed_quotes(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut open: Option<usize> = None;
    for c in text.chars() {
        if QUOTE_MARKS.contains(&c) {
            match open {
                None => {
                    out.push(c);
                    open = Some(out.len());
                }
                Some(at) => {
                    out.truncate(at);
                    out.push(c);
                    open = None;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// The start of the sentence byte `at` sits in, never before `floor`.
fn sentence_start(hay: &str, floor: usize, at: usize) -> usize {
    ["! ", "? ", ". "]
        .iter()
        .filter_map(|end| hay[floor..at].rfind(end).map(|i| floor + i + end.len()))
        .max()
        .unwrap_or(floor)
}

/// Whether the occurrence of a trigger term at byte `at` (length `len`) in
/// `hay` sits in another party's words - see this section's own comment.
fn occurrence_is_another_partys(hay: &str, at: usize, len: usize) -> bool {
    // A fenced block, or a blockquote line: its frame is the last plain line
    // before it.
    let mut in_fence = false;
    let mut fence_frame = "";
    let mut plain_before = "";
    let mut offset = 0usize;
    for line in hay.split('\n') {
        let end = offset + line.len();
        let trimmed = line.trim_start();
        let is_fence = trimmed.starts_with("```");
        if at >= offset && at <= end {
            if in_fence && !is_fence {
                return quote_frame_is_another_party(fence_frame, "");
            }
            if trimmed.starts_with('>') {
                return quote_frame_is_another_party(plain_before, "");
            }
            break;
        }
        if is_fence {
            if !in_fence {
                fence_frame = plain_before;
            }
            in_fence = !in_fence;
        } else if !in_fence && !trimmed.is_empty() && !trimmed.starts_with('>') {
            plain_before = line;
        }
        offset = end + 1;
    }
    let line_start = hay[..at].rfind('\n').map_or(0, |i| i + 1);
    let line_end = hay[at..].find('\n').map_or(hay.len(), |i| at + i);
    let quote_marks_before: Vec<usize> =
        hay[line_start..at].char_indices().filter(|(_, c)| QUOTE_MARKS.contains(c)).map(|(i, _)| line_start + i).collect();
    if quote_marks_before.len() % 2 == 1 {
        let open_at = *quote_marks_before.last().expect("an odd count is at least one");
        let before = blank_closed_quotes(&hay[sentence_start(hay, line_start, open_at)..open_at]);
        let rest = &hay[at + len..line_end];
        let after = rest.char_indices().find(|(_, c)| QUOTE_MARKS.contains(c)).map_or("", |(i, c)| &rest[i + c.len_utf8()..]);
        return quote_frame_is_another_party(&before, after);
    }
    let lead = blank_closed_quotes(&hay[sentence_start(hay, line_start, at)..at]);
    reported_by_another_party(&lead, &hay[at..at + len])
}

/// The trigger side of a rule (`trigger_holds`) with `own_claims_only`
/// honoured.
fn trigger_fires(rule: &OptInRule, haystack_lower: &str) -> bool {
    let scopes = !rule.hedge_words_before.is_empty() || !rule.scope_words_after.is_empty() || rule.list_label_scopes;
    if rule.own_claims_only || rule.affirmative_only || rule.quotes_exempt || scopes {
        trigger_holds(&rule.base, haystack_lower, &|term| occurs_as_claimed(haystack_lower, term, rule))
    } else {
        trigger_holds(&rule.base, haystack_lower, &|term| haystack_lower.contains(term))
    }
}

// -------------------------------------------------------- negated claims
//
// THE FALSE BLOCK THIS CLOSES (field test 2026-10-01). The rule that demands
// "name the VERSION whenever you say something is pushed, deployed or now
// running" held a reply that said "niets gecommit, gepusht of gedeployd" - a
// plain substring match cannot tell the claim from its opposite.
//
// A rule opts in with `affirmative_only: true`. One occurrence of a trigger
// term then does not count when a negation word ("niet", "niets", "geen",
// "nooit", "not", "never", "without", ...) stands within the six words before
// it in the same sentence, with no contrast word ("maar", "but", "wel", ...)
// between them. A comma does not end the reach: "niets gecommit, gepusht of
// gedeployd" negates all three. "no" alone is not a negation word here: it
// opens too many harmless sentences ("No problem, pushed to dev").
//
// Biased like the reported-speech reading: a negation further back than six
// words, or cut off by a full stop, leaves the claim a claim.

/// How many words before a trigger term a negation can reach.
const NEGATION_REACH: usize = 6;

/// Whether the words just before byte `at` of `hay` negate what follows.
fn negated_before(hay: &str, at: usize) -> bool {
    let lead = &hay[sentence_start(hay, 0, at)..at];
    let before = words(lead);
    for (_, _, word) in before.iter().rev().take(NEGATION_REACH) {
        let word = word.to_lowercase();
        if crate::prompt::CONTRASTS.contains(&word.as_str()) {
            return false;
        }
        if word != "no" && crate::prompt::NEGATORS.contains(&word.as_str()) {
            return true;
        }
    }
    false
}

/// Whether `term_lower` occurs in `hay` at least once as the assistant's
/// claim: in its own voice when the rule asks for that, not negated when the
/// rule is affirmative-only, outside quotes when quotes are exempt, and not
/// hedged or scoped by the words around it (`occurrence_is_scoped`).
fn occurs_as_claimed(hay: &str, term_lower: &str, rule: &OptInRule) -> bool {
    if term_lower.is_empty() {
        return true;
    }
    hay.match_indices(term_lower).any(|(at, t)| {
        (!rule.own_claims_only || !occurrence_is_another_partys(hay, at, t.len()))
            && (!rule.affirmative_only || !negated_before(hay, at))
            && (!rule.quotes_exempt || !inside_quotes(hay, at))
            && !occurrence_is_scoped(rule, hay, at, t.len())
    })
}

/// Whether the words around the occurrence at byte `at` (length `len`) make it
/// something other than an absolute claim: a hedge word earlier in its
/// sentence ("zodra", "of", "once"), a scope word later in its clause ("op",
/// "behalve", "except"), or - on a list line - a label and colon before it.
fn occurrence_is_scoped(rule: &OptInRule, hay: &str, at: usize, len: usize) -> bool {
    let line_start = hay[..at].rfind('\n').map_or(0, |i| i + 1);
    let has_word = |text: &str, list: &[String]| {
        words(text).iter().any(|(_, _, w)| list.iter().any(|l| l.to_lowercase() == *w))
    };
    if !rule.hedge_words_before.is_empty() {
        let lead = &hay[sentence_start(hay, line_start, at)..at];
        if has_word(lead, &rule.hedge_words_before) {
            return true;
        }
    }
    if !rule.scope_words_after.is_empty() {
        let tail = &hay[at + len..];
        let end = [". ", "! ", "? ", ": ", "; ", "\n"].iter().filter_map(|stop| tail.find(stop)).min().unwrap_or(tail.len());
        if has_word(&tail[..end], &rule.scope_words_after) {
            return true;
        }
    }
    let line = &hay[line_start..at];
    rule.list_label_scopes && is_list_line(line) && line.contains(':')
}

/// Whether byte `at` of `hay` sits inside quotation marks or inline code: an
/// odd number of quote marks before it on its line.
fn inside_quotes(hay: &str, at: usize) -> bool {
    let line_start = hay[..at].rfind('\n').map_or(0, |i| i + 1);
    hay[line_start..at].chars().filter(|c| QUOTE_MARKS.contains(c)).count() % 2 == 1
}

// --------------------------------------------------------- list exemption
//
// FALSE BLOCK A, reported by the owner from other sessions, fixed
// 2026-09-07: `answer-is-too-long` blocked a reply that was a list the owner
// had explicitly asked for - its only two escapes (a quoted blockquote, a
// code fence) have nothing to do with an ordinary markdown list, so a long,
// entirely compliant list had no way to pass. The fix is deliberately
// narrower than "never block a list": it stands the rule aside only when
// BOTH (a) the owner's own last prompt asked for one (`list_request_any_of`,
// matched the same case-insensitive substring way as every other term list
// in this file - a small explicit word list, never fuzzy, so a rulebook
// author can see exactly what triggers it) AND (b) the reply itself actually
// reads as a list. Requiring both matters: the exemption is for the LIST,
// not for a long prose answer that merely follows a prompt that happened to
// ask for one - that answer failed to do what was asked and must still be
// challenged for its length, same as before this field existed.

/// Whether trimmed line `line` opens with a markdown list marker: a bullet
/// (`-`, `*`, `+`) followed by a space (or nothing but the bullet itself),
/// or an ordered marker (one or more digits then `.` or `)`) followed by a
/// space or nothing. KNOWN NOT RECOGNISED, stated plainly rather than
/// silently missed: a glossary-style `**term**: description` line, a
/// markdown table row, or a numbered marker using a different closing
/// punctuation - see this section's own doc comment on the honest scope this
/// was kept to.
fn is_list_line(line: &str) -> bool {
    let t = line.trim();
    let mut chars = t.chars();
    match chars.next() {
        Some('-') | Some('*') | Some('+') => return matches!(chars.next(), Some(' ') | None),
        _ => {}
    }
    let after_digits = t.trim_start_matches(|c: char| c.is_ascii_digit());
    if after_digits.len() == t.len() {
        return false; // no leading digit at all
    }
    if let Some(rest) = after_digits.strip_prefix('.').or_else(|| after_digits.strip_prefix(')')) {
        return rest.starts_with(' ') || rest.is_empty();
    }
    false
}

/// Whether `haystack_lower` reads as a LIST: at least three non-blank lines
/// are list lines (`is_list_line`), and they are the MAJORITY of the
/// non-blank lines - never fooled by one bullet buried in five paragraphs of
/// prose, which is presence, not shape. Blank lines are not counted either
/// way (a blank line between list items is normal list formatting, not
/// prose diluting it).
fn is_mostly_list_lines(haystack_lower: &str) -> bool {
    let mut list_lines = 0usize;
    let mut total = 0usize;
    for line in haystack_lower.lines() {
        if line.trim().is_empty() {
            continue;
        }
        total += 1;
        if is_list_line(line) {
            list_lines += 1;
        }
    }
    list_lines >= 3 && list_lines * 2 >= total
}

/// Whether `rule.list_request_any_of` exempts this match: the prompt asked
/// for a list (a term is present in `prompt_lower`) AND the reply itself is
/// mostly list lines. `list_request_any_of` empty, or no prompt text at all
/// (`prompt_lower` empty - the transcript could not be read, see
/// `last_user_prompt`'s own doc comment), both mean the constraint never
/// applies - never an escape, exactly as if the field did not exist.
fn list_exempted(rule: &OptInRule, prompt_lower: &str, haystack_lower: &str) -> bool {
    if rule.list_request_any_of.is_empty() || prompt_lower.is_empty() {
        return false;
    }
    let asked_for_a_list = rule.list_request_any_of.iter().any(|t| prompt_lower.contains(&t.to_lowercase()));
    asked_for_a_list && is_mostly_list_lines(haystack_lower)
}

/// Whether the owner's prompt explicitly asked for a summary or an overview
/// (`overview_request_any_of`) and the reply is still within the longer
/// allowance that earns (`overview_max_chars`). Found 2026-10-09 by the
/// answer-guard sweep: the owner asked "Kan je eens samenvatten wat de nieuwe
/// gebruiker te zien krijgt", and the reply WAS that overview, about 1,100
/// characters of structured prose; the length rule held it because
/// "samenvatten" was no request word and the reply was not list-shaped. No
/// prompt text at all, an empty term list or a zero allowance never exempts.
fn overview_allowed(rule: &OptInRule, prompt_lower: &str, haystack_lower: &str) -> bool {
    if rule.overview_request_any_of.is_empty() || rule.overview_max_chars == 0 || prompt_lower.is_empty() {
        return false;
    }
    rule.overview_request_any_of.iter().any(|t| prompt_lower.contains(&t.to_lowercase()))
        && haystack_lower.chars().count() <= rule.overview_max_chars
}

// ------------------------------------------------- the first-session list
//
// THE DEFECT THIS CLOSES (blind test of a new user's first session, 2026-10-10).
// The first reply of a new install MUST show the numbered list of everything the
// install arranged (`ops::questionnaire::render_list`, twelve lines, about 2,900
// characters). The user did not ask for an overview, so the style rules about the
// owner's own reading (`owner_reading_only`: the length rule, the summary-first
// rule) would hold the very first reply the memory ever gives.
//
// The reply is recognised by SHAPE, in any language, because the assistant shows
// the list in the user's own language: numbered lines 1 to
// `SETUP_LIST_MIN_ITEMS`, and the two items a translation keeps verbatim
// (the `/thor-eval` command of item 4 and the `CHANGELOG.md` file of item 12).
// A test in `ops` renders the real list and holds that it is recognised.
// Capped at `SETUP_LIST_MAX_CHARS`, so the exemption is for the list reply and
// not for an essay that happens to contain one. It stands aside for
// `owner_reading_only` rules only: the honesty rules keep reading it.

/// How many numbered lines (1, 2, 3, ...) the first-session list reply must carry.
pub const SETUP_LIST_MIN_ITEMS: usize = 10;

/// The longest reply that still counts as the first-session list reply.
pub const SETUP_LIST_MAX_CHARS: usize = 6000;

/// Whether `haystack_lower` (see `haystack`) is the first-session list reply.
pub fn is_setup_list_reply(haystack_lower: &str) -> bool {
    if haystack_lower.chars().count() > SETUP_LIST_MAX_CHARS
        || !haystack_lower.contains("/thor-eval")
        || !haystack_lower.contains("changelog.md")
    {
        return false;
    }
    let mut next = 1usize;
    for line in haystack_lower.lines() {
        let item = line.trim_start_matches(|c: char| c == '*' || c == '_' || c == '#' || c == '-' || c == ' ');
        let number = format!("{next}");
        let numbered = item
            .strip_prefix(number.as_str())
            .is_some_and(|rest| rest.starts_with('.') || rest.starts_with(')'));
        if numbered {
            next += 1;
        }
    }
    next > SETUP_LIST_MIN_ITEMS
}

// ------------------------------------------------------- reader scope
//
// THE DEFECT THIS FIELD FIXES, decided by the owner 2026-09-09, the same day
// `serve/src/bin/serve.rs` first gave a subagent's Stop a blanket exemption
// from this whole guard. That exemption was all-or-nothing: EVERY rule in
// the rulebook stopped applying to a subagent's own reply, the moment the
// payload carried an `agent_id` at all. Two of the rulebook's rules are not
// about how a reply READS to the owner but about whether the agent told the
// truth - a claim that something was checked or verified with no evidence
// for it (the live rulebook's `checked-claim-needs-evidence`), and a claim
// that something could not be reached without having tried (the live
// rulebook's `claim-no-access-without-checking`) - and those must keep
// catching a subagent exactly as they catch the owner's own main session: a
// subagent that lies about having checked something is the same defect this
// whole project exists to catch, regardless of who reads the lie. The
// blanket exemption silenced them too. Measured the same day: the rules that
// ARE genuinely about the owner's own reading experience (the length rule,
// the plain-language-summary rule) refused 15 of 25 real agent reports - so
// the exemption was not wrong to exist, only wrong to cover every rule
// alike.
//
// THE FIX: `owner_reading_only`, one optional boolean per rule (parsed by
// `parse_opt_in_rules`, read here by `evaluate_opt_in`). `true` means this
// rule judges the SHAPE of a reply for the owner's own reading - length, a
// summary first, a tone he finds grating - rather than whether the agent was
// honest or diligent, and is skipped entirely for a subagent payload, the
// same as if the rule had not matched at all. Every other rule - the field
// absent, `false`, or any value that is not a literal JSON `true` - keeps
// applying to everyone, subagent included.
//
// THE DEFAULT MATTERS, and it is deliberately the OPPOSITE of `tier`'s own
// default a few sections up. `tier` defaults to `Block` (the stricter of its
// two choices) because every rulebook shipped before that field existed
// already meant Block, and the default has to preserve that history. This
// field carries no such history, so its default is chosen the other way: a
// gate going quiet is the worst failure class this whole project exists to
// catch (`serve/src/bin/serve.rs` has already measured that twice, under a
// different name - see its own `is_subagent` doc comment), so a rule that
// DECLARES NOTHING applies to EVERYONE, including a subagent, rather than to
// no one. That way a newly written honesty rule guards a subagent from the
// moment it exists, with no second step to remember, and a newly written
// style rule that turns out to annoy an agent is a LOUD, visible nuisance -
// caught, named, and marked - rather than a silent hole nobody notices. A
// malformed value (a string, a number, anything that is not literally
// `true`/`false`) falls back to this SAME default rather than crashing the
// guard or erroring: this parser already fails this way on every other
// field (`str_list` drops a non-string entry silently, `parse_one_rule`
// drops a whole rule that carries no `reminder` at all) and on the rulebook
// file itself (unreadable or malformed JSON yields zero rules, never an
// error - this module's own "Failure policy" doc comment at the top) - a
// rule whose scope cannot be read is a rule that keeps applying, not a rule
// that goes quiet. `parse_opt_in_rules`'s own `.as_bool().unwrap_or(false)`
// is where this is actually enforced.

/// `evaluate`'s own AND/OR/NOT/min_chars matcher (`trigger_fires` for the
/// trigger side, which also honours `own_claims_only`), plus the positional
/// escape, the pattern escape (`pattern_escaped`), the list exemption
/// (`list_exempted`) and the reader scope (this section's own doc comment
/// above) layered on top. Returns every fired rule's tier and reminder, in
/// rulebook order - the same order `evaluate` returns its reminders in.
/// `prompt_lower` is the owner's own last prompt, already lowercased, or an
/// empty string when it could not be read - `list_exempted` treats that
/// exactly like "the field is absent", never as a match. `is_subagent` is the
/// caller's own answer to "is this payload from a Task-tool subagent"
/// (`serve/src/bin/serve.rs`'s own `payload_is_from_a_subagent`) - `true`
/// drops every fired rule whose `owner_reading_only` is `true`, `false` drops
/// none of them.
pub fn evaluate_opt_in(
    rules: &[OptInRule],
    haystack_lower: &str,
    prompt_lower: &str,
    is_subagent: bool,
) -> Vec<(Tier, String)> {
    evaluate_opt_in_checked(rules, haystack_lower, prompt_lower, is_subagent, None, &[])
}

/// What a fired rule says: its `reminder_if_asked` when the owner's own prompt
/// asked for what the reminder tells the writer to leave out, else its
/// reminder - with any internal id in a bracket taken out (`without_internal_ids`)
/// and `{owner_request}` replaced by the owner's own words (`owner_request_quote`).
/// `prompt` is the owner's last prompt as he typed it, `prompt_lower` the same
/// lowercased (empty when it could not be read).
fn reminder_for(rule: &OptInRule, prompt: &str, prompt_lower: &str) -> String {
    let asked = !prompt_lower.is_empty()
        && !rule.asked_for_any_of.is_empty()
        && rule.asked_for_any_of.iter().any(|t| prompt_lower.contains(&t.to_lowercase()));
    let text = match (&rule.reminder_if_asked, asked) {
        (Some(alternative), true) => alternative,
        _ => &rule.base.reminder,
    };
    with_owner_request(&without_internal_ids(text), rule, prompt)
}

/// `text` with `{owner_request}` replaced by the owner's own words.
fn with_owner_request(text: &str, rule: &OptInRule, prompt: &str) -> String {
    if !text.contains(OWNER_REQUEST_PLACEHOLDER) {
        return text.to_string();
    }
    let quote = owner_request_quote(prompt, &rule.asked_for_any_of);
    text.replace(OWNER_REQUEST_PLACEHOLDER, &quote).replace("  ", " ")
}

/// Said once after the short reminders when two rules of one tier fire (step 33).
/// A guard's note about a refused edit or a change it forced is not padding, and
/// the length rule must not make a writer drop it.
const COMBINED_HOLD_TAIL: &str =
    "Keep any one-line note that a guard refused an edit or forced a change to your work: that is not cut.";

/// A reminder that cannot know what the owner asked for says so with this
/// placeholder; the hold then carries his own words instead of a guess (step 32:
/// "the owner asked for the test result" made an agent drop the "what you
/// changed" the same prompt also asked for).
const OWNER_REQUEST_PLACEHOLDER: &str = "{owner_request}";

/// The longest quote of the owner's prompt a hold carries.
const OWNER_REQUEST_QUOTE_MAX_CHARS: usize = 240;

/// Words that ask for a report back (English and Dutch), matched as whole words
/// of a sentence of the owner's prompt, beside the rulebook's own
/// `asked_for_any_of` terms.
const REPORT_VERBS: &[&str] = &[
    "geef", "vertel", "rapporteer", "meld", "toon", "report", "tell", "give", "summarise", "summarize", "explain",
    "describe",
];

/// Phrases that ask for a report back, matched anywhere in a sentence.
const REPORT_PHRASES: &[&str] = &["laat zien", "show me", "wat je veranderde", "hoe je het controleerde", "what you changed", "how you checked", "how you verified"];

fn asks_for_a_report(lower_sentence: &str) -> bool {
    REPORT_PHRASES.iter().any(|p| lower_sentence.contains(p))
        || lower_sentence
            .split(|c: char| !c.is_alphanumeric())
            .any(|word| REPORT_VERBS.contains(&word))
}

/// His own words, as a short clause for a hold: the sentence(s) of `prompt`
/// that name one of `asked_terms` or ask for a report back (`asks_for_a_report`),
/// else the LAST sentence (a report is asked for at the end); cut at a word
/// boundary after `OWNER_REQUEST_QUOTE_MAX_CHARS`. Empty when the prompt could
/// not be read, so the clause is simply left out.
fn owner_request_quote(prompt: &str, asked_terms: &[String]) -> String {
    let prompt = prompt.trim();
    if prompt.is_empty() {
        return String::new();
    }
    let terms: Vec<String> = asked_terms.iter().map(|t| t.to_lowercase()).collect();
    let sentences = prompt_sentences(prompt);
    let naming: Vec<&str> = sentences
        .iter()
        .copied()
        .filter(|sentence| {
            let lower = sentence.to_lowercase();
            terms.iter().any(|t| lower.contains(t)) || asks_for_a_report(&lower)
        })
        .collect();
    let picked = if naming.is_empty() { sentences.last().copied().unwrap_or(prompt).to_string() } else { naming.join(" ") };
    let one_line = picked.split_whitespace().collect::<Vec<_>>().join(" ");
    let cut = if one_line.chars().count() > OWNER_REQUEST_QUOTE_MAX_CHARS {
        let head: String = one_line.chars().take(OWNER_REQUEST_QUOTE_MAX_CHARS).collect();
        // Back to the last whole word, unless the cut already fell on a space.
        let whole = if one_line.chars().nth(OWNER_REQUEST_QUOTE_MAX_CHARS).is_some_and(char::is_whitespace) {
            head.as_str()
        } else {
            head.rsplit_once(char::is_whitespace).map_or(head.as_str(), |(words, _)| words)
        };
        format!("{}...", whole.trim_end())
    } else {
        one_line
    };
    format!("His own words were: \"{cut}\".")
}

/// `prompt` cut into sentences: after a `.`, `!` or `?` that is followed by
/// white space, and at every line break. A dot inside a name (`lib/pdf.js`)
/// is not an end.
fn prompt_sentences(prompt: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut chars = prompt.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        let ends_here = match c {
            '\n' => true,
            '.' | '!' | '?' => chars.peek().map_or(true, |(_, next)| next.is_whitespace()),
            _ => false,
        };
        if ends_here {
            let end = i + c.len_utf8();
            let sentence = prompt[start..end].trim();
            if !sentence.is_empty() {
                out.push(sentence);
            }
            start = end;
        }
    }
    let rest = prompt[start..].trim();
    if !rest.is_empty() {
        out.push(rest);
    }
    out
}

/// `text` without a store id in brackets: a block message is read by a writer
/// who has no use for "(mcp-0a1b2c3d)" or "(alpha:mem-0a1b2c3d)" - the
/// reminder says what to do, never which stored note says so (step 29). Only a
/// bracket holding nothing but such an id is removed, with the space before it.
pub fn without_internal_ids(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find('(') {
        let Some(close) = rest[open..].find(')') else { break };
        let inner = &rest[open + 1..open + close];
        if is_internal_id(inner) {
            out.push_str(rest[..open].trim_end_matches(' '));
            rest = &rest[open + close + 1..];
        } else {
            out.push_str(&rest[..open + close + 1]);
            rest = &rest[open + close + 1..];
        }
    }
    out.push_str(rest);
    out
}

/// `mcp-0a1b2c3d`, `mem-0a1b2c3d`, `alpha:mem-0a1b2c3d`: an optional
/// project and colon, then `mcp-` or `mem-` and eight hex digits.
fn is_internal_id(inner: &str) -> bool {
    let id = inner.rsplit(':').next().unwrap_or(inner);
    let hex = id.strip_prefix("mcp-").or_else(|| id.strip_prefix("mem-"));
    hex.is_some_and(|h| h.len() == 8 && h.chars().all(|c| c.is_ascii_hexdigit()))
}

/// Whether the owner's own last prompt lets `rule` apply at all
/// (`only_if_prompt_any_of`): no gate, or a prompt that could not be read, or a
/// prompt that carries one of its words.
fn prompt_gate_open(rule: &OptInRule, prompt_lower: &str) -> bool {
    rule.only_if_prompt_any_of.is_empty()
        || prompt_lower.is_empty()
        || rule.only_if_prompt_any_of.iter().any(|t| prompt_lower.contains(&t.to_lowercase()))
}

/// `evaluate_opt_in`, plus the reply's evidence checked when the caller could
/// look (see "checked evidence" above): a rule that takes a citation or a sha
/// as evidence then reads its escapes against what checked out, and its
/// reminder names what did not, so the writer sees why it did not count.
/// `evidence` is the original reply with the caller's lookup; the reply is
/// checked at most once, and only when a rule that takes evidence has
/// otherwise fired, so an answer no such rule is about costs no lookup at all.
fn evaluate_opt_in_checked(
    rules: &[OptInRule],
    haystack_lower: &str,
    prompt: &str,
    is_subagent: bool,
    evidence: Option<(&str, &EvidenceLookup)>,
    turn_commands: &[String],
) -> Vec<(Tier, String)> {
    let prompt_lower = prompt.to_lowercase();
    let prompt_lower = prompt_lower.as_str();
    let checked: std::cell::OnceCell<Option<CheckedEvidence>> = std::cell::OnceCell::new();
    let fired: Vec<(&OptInRule, String, usize)> = rules
        .iter()
        .filter_map(|r| {
            let fires_before_escapes = trigger_fires(r, haystack_lower)
                && !positionally_escaped(r, haystack_lower)
                && !list_exempted(r, prompt_lower, haystack_lower)
                && !overview_allowed(r, prompt_lower, haystack_lower)
                && !(r.owner_reading_only && is_subagent)
                && !(r.owner_reading_only && is_setup_list_reply(haystack_lower))
                && prompt_gate_open(r, prompt_lower)
                && !(r.backed_by_turn_tools && turn_backs_claim(&r.base, haystack_lower, turn_commands));
            if !fires_before_escapes {
                return None;
            }
            // A check the turn really ran, or one the answer names, is evidence of
            // a claim of checking (step 33): whoever reads the claim can test it.
            if takes_evidence(r) && (turn_commands.iter().any(|c| names_a_check(&c.to_lowercase())) || names_a_check(haystack_lower)) {
                return None;
            }
            let checked = if takes_evidence(r) {
                checked.get_or_init(|| evidence.map(|(message, lookup)| check_evidence(message, lookup))).as_ref()
            } else {
                None
            };
            let escaped = match checked {
                Some(c) => none_of_escaped_checked(&r.base, c) || pattern_escaped_checked(r, c),
                None => none_of_escaped(&r.base, haystack_lower) || pattern_escaped(r, haystack_lower),
            };
            if escaped {
                return None;
            }
            let said = reminder_for(r, prompt, prompt_lower);
            let said_len = said.len();
            let reminder = match checked.filter(|c| !c.unverified.is_empty()) {
                Some(c) => format!(
                    "{} (Evidence counts only when it checks out: a path:line, or a line number next to a file \
                     name, must name a line that file really has under the session's working directory, and a \
                     commit must exist in its git repository; these did not: {}.)",
                    said,
                    c.unverified.iter().take(3).map(String::as_str).collect::<Vec<_>>().join(", ")
                ),
                None => said,
            };
            Some((r, reminder, said_len))
        })
        .collect();
    // Two rules of one tier on one reply: each says its requirement once, in its
    // short form, and the tail is said once (step 33). A rule without a short
    // form keeps its full reminder.
    let mut out: Vec<(Tier, String)> = Vec::new();
    for (i, (rule, reminder, said_len)) in fired.iter().enumerate() {
        let together = fired.iter().filter(|(o, _, _)| o.tier == rule.tier).count() > 1;
        let text = match (&rule.short_reminder, together) {
            (Some(short), true) => {
                let short = with_owner_request(&without_internal_ids(short), rule, prompt);
                // The evidence note rides after the reminder in the full form; keep it.
                format!("{short}{}", &reminder[*said_len..])
            }
            _ => reminder.clone(),
        };
        out.push((rule.tier, text));
        let last_of_tier = !fired[i + 1..].iter().any(|(o, _, _)| o.tier == rule.tier);
        if together && last_of_tier {
            out.push((rule.tier, COMBINED_HOLD_TAIL.to_string()));
        }
    }
    out
}

/// Commands (or the words of an answer) that check something: a syntax check,
/// a test run, a build check, a diff. Matched as lowercase substrings that start
/// a word, so `bash -n` and `sh -n` both count and `ash -n` inside a longer word
/// does not.
const CHECK_COMMANDS: &[&str] = &[
    "sh -n", "bash -n", "dash -n", "zsh -n", "node --check", "node --test", "npm test", "npm run test", "pnpm test", "yarn test", "cargo test", "cargo check",
    "cargo build", "cargo clippy", "pytest", "py_compile", "unittest", "go test", "go vet", "dotnet test", "shellcheck", "tsc ",
    "git diff", "make test", "ctest", "mvn test", "gradle test",
];

fn names_a_check(lower: &str) -> bool {
    CHECK_COMMANDS.iter().any(|needle| {
        lower.match_indices(needle).any(|(at, _)| !lower[..at].chars().next_back().is_some_and(|c| c.is_alphanumeric() || c == '_'))
    })
}

/// SPEC-ENFORCEMENT.md section 1.1's three verdicts, restricted to the two
/// this matcher can ever produce - ALLOW is simply both fields `None`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuardVerdict {
    /// Same shape, same "[THOR] ... || ..." formatting, as `block_reason`'s
    /// own return value - built only from `Tier::Block` fires. `Some` means
    /// exactly what it always meant: stop the reply.
    pub block_reason: Option<String>,
    /// The same formatting, built only from `Tier::Warn` fires. Never a
    /// reason to stop anything (SPEC-ENFORCEMENT.md 1.1). See this
    /// section's own doc comment for what still has to change elsewhere
    /// before this can reach the owner without blocking.
    pub warn_reason: Option<String>,
}

/// `block_reason`'s richer sibling: same fail-open stance (no rulebook, or
/// zero parseable rules, yields both fields `None`), but reads a rulebook
/// that may carry `before`/`tier`/`none_of_patterns`/`list_request_any_of`/
/// `owner_reading_only`/`own_claims_only` and keeps a WARN-tier fire out of
/// the block reason entirely, instead of discarding the tier and treating
/// every fire alike. `last_user_prompt` is the owner's own last typed prompt
/// (see the `last_user_prompt` function above for where a caller reads this
/// from), or an empty string when it is not available - the ONLY thing that
/// ever reads it, the list exemption, treats an empty string exactly like
/// "the prompt did not ask for a list". `is_subagent` is threaded straight
/// through to `evaluate_opt_in` - see this file's own "READER SCOPE" doc
/// comment, right above it, for what it does and why its default (`false`,
/// from every existing caller that predates this parameter) must never
/// silently narrow what a rulebook already blocks or warns about for the
/// owner's own main session, only ever for a subagent's. `evidence` is the
/// caller's way to look a citation, a line number or a commit up (`Some` only
/// when it knows the session's working directory - see "checked evidence"
/// above); `None` judges all of them by their shape alone, as before that
/// existed.
pub fn guard_verdict(
    rulebook_text: Option<&str>,
    message: &str,
    last_user_prompt: &str,
    is_subagent: bool,
    evidence: Option<&EvidenceLookup>,
) -> GuardVerdict {
    guard_verdict_in_turn(rulebook_text, message, last_user_prompt, is_subagent, evidence, &[])
}

/// `guard_verdict` that also knows the shell commands the assistant ran in the
/// turn (`turn_commands`): a claim of having checked something counts as
/// evidenced when the turn ran a check (`names_a_check`) or the answer names
/// one. The claim is still judged only by its shape and the lookups; this
/// adds the one signal the answer text cannot carry, a real tool call.
pub fn guard_verdict_in_turn(
    rulebook_text: Option<&str>,
    message: &str,
    last_user_prompt: &str,
    is_subagent: bool,
    evidence: Option<&EvidenceLookup>,
    turn_commands: &[String],
) -> GuardVerdict {
    let rules = rulebook_text.map(parse_opt_in_rules).unwrap_or_default();
    if rules.is_empty() {
        return GuardVerdict { block_reason: None, warn_reason: None };
    }
    let fired =
        evaluate_opt_in_checked(&rules, &haystack(message), last_user_prompt, is_subagent, evidence.map(|e| (message, e)), turn_commands);
    let format_tier = |tier: Tier| -> Option<String> {
        let reasons: Vec<&str> = fired.iter().filter(|(t, _)| *t == tier).map(|(_, r)| r.as_str()).collect();
        if reasons.is_empty() {
            None
        } else {
            Some(format!("[THOR] {}", reasons.join("  ||  ")))
        }
    };
    GuardVerdict { block_reason: format_tier(Tier::Block), warn_reason: format_tier(Tier::Warn) }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RULEBOOK: &str = r#"[
      {"id":"tldr","any_of":["md5","commit ","gepusht","fsck","exit=0"],
       "none_of":["tldr","kort:","any_of"],"min_chars":600,
       "reminder":"This reads like results but has no TLDR - open with one."},
      {"id":"amnesia","any_of":["kun je checken","can you check","ik heb geen toegang"],
       "none_of":["any_of"],
       "reminder":"Check whether you can reach it yourself before asking."}
    ]"#;

    fn long(marker: &str) -> String {
        format!("{marker} {}", "x".repeat(700))
    }

    /// THE DEFECT THIS PREVENTS: a long technical answer with a commit hash and
    /// no TLDR goes out unchallenged. It was true for a whole session.
    #[test]
    fn a_long_technical_answer_without_tldr_is_blocked() {
        let msg = long("the commit fsck run is done, all gepusht");
        let reason = block_reason(Some(RULEBOOK), &msg).expect("must fire");
        assert!(reason.contains("TLDR"), "{reason}");
    }

    #[test]
    fn the_same_answer_with_a_tldr_passes() {
        let msg = long("TLDR: it works. the commit fsck run is done, all gepusht");
        assert!(block_reason(Some(RULEBOOK), &msg).is_none());
    }

    /// The min_chars floor: a two-line status note with one technical word is
    /// not a report and must not be blocked (measured defect in 1.0: a
    /// 230-char closing line blocked for the word "pushed").
    #[test]
    fn a_short_technical_note_is_below_the_floor_and_passes() {
        assert!(block_reason(Some(RULEBOOK), "commit done, gepusht").is_none());
    }

    /// The none_of escape: a message discussing the rule itself must not trip
    /// it. "any_of" appears in every rule's own none_of exactly for this.
    #[test]
    fn discussing_the_rulebook_does_not_trip_a_rule() {
        let msg = long("the tldr rule has any_of [md5, commit ] and fires on a commit ");
        assert!(block_reason(Some(RULEBOOK), &msg).is_none());
    }

    #[test]
    fn capability_amnesia_is_caught_regardless_of_length() {
        let reason = block_reason(Some(RULEBOOK), "kun je checken wat er in staat?").expect("must fire");
        assert!(reason.contains("yourself"), "{reason}");
    }

    /// Fail-open: a broken or absent rulebook blocks nothing.
    #[test]
    fn a_broken_or_absent_rulebook_blocks_nothing() {
        assert!(block_reason(None, &long("commit fsck gepusht")).is_none());
        assert!(block_reason(Some("{ not json"), &long("commit fsck gepusht")).is_none());
        assert!(block_reason(Some("[]"), &long("commit fsck gepusht")).is_none());
    }

    #[test]
    fn the_rulebook_sits_beside_the_store() {
        let p = default_rulebook_path(Path::new("C:/Users/x/thor2/thor.db"));
        assert!(p.ends_with("guard-response-rulebook.json"));
        assert_eq!(p.parent(), Some(Path::new("C:/Users/x/thor2")));
    }

    // ---------------------------------------------------------- position
    //
    // Deliberately does NOT put the summary marker ("tldr"/"kort:") in
    // `none_of` the way the live rulebook does: that would let the OLD,
    // position-blind escape pass these tests for the wrong reason. Here the
    // ONLY thing that can excuse a match is the NEW `before` field, so a
    // passing test actually proves the positional check works.
    const POSITIONED_RULEBOOK: &str = r#"[
      {"id":"tldr-positioned",
       "any_of":["commit ","fsck","gepusht"],
       "none_of":["any_of"],
       "before":["tldr","kort:"],
       "min_chars":600,
       "reminder":"open with a TLDR before the jargon"}
    ]"#;

    fn long_prefix(marker: &str) -> String {
        format!("{marker} {}", "detail ".repeat(100))
    }

    fn long_suffix(marker: &str) -> String {
        format!("{} {marker}", "detail ".repeat(100))
    }

    /// THE DEFECT THIS PREVENTS: a summary that genuinely comes first, before
    /// any jargon, still trips a summary-first rule - the shape of the b01
    /// false block (RESPONSE-GUARD-BLIND.md), fixed here at the position
    /// level instead of by growing the escape vocabulary.
    #[test]
    fn a_reply_that_opens_with_a_summary_then_jargon_does_not_trip_the_rule() {
        let msg = format!("TLDR: it works. {}", long_suffix("commit "));
        assert!(guard_verdict(Some(POSITIONED_RULEBOOK), &msg, "", false, None).block_reason.is_none());
    }

    /// Same words, reversed order: the jargon now comes first, so the rule
    /// must fire - proves the check is genuinely about ORDER, not merely
    /// presence of the summary marker somewhere in the text.
    #[test]
    fn the_same_words_in_the_other_order_trips_the_rule() {
        let msg = format!("{} TLDR: it works.", long_prefix("commit "));
        let reason = guard_verdict(Some(POSITIONED_RULEBOOK), &msg, "", false, None).block_reason.expect("must fire");
        assert!(reason.contains("before the jargon"), "{reason}");
    }

    /// No jargon at all: `any_of` never matches, so the positional escape is
    /// never even asked to decide anything - `before` must never fire a rule
    /// on its own.
    #[test]
    fn a_reply_with_no_jargon_at_all_never_trips_the_position_aware_rule() {
        let msg = format!("TLDR: it works. {}", "detail ".repeat(100));
        assert!(guard_verdict(Some(POSITIONED_RULEBOOK), &msg, "", false, None).block_reason.is_none());
    }

    /// A rulebook that never mentions `before` at all - every rule the owner
    /// has shipped so far - must evaluate through the new path exactly as it
    /// did through the old one. Reuses this file's own pre-existing
    /// `RULEBOOK`/`long` fixtures so the comparison is apples to apples.
    #[test]
    fn a_rulebook_without_the_before_field_behaves_exactly_as_before() {
        let blocked = long("the commit fsck run is done, all gepusht");
        assert_eq!(block_reason(Some(RULEBOOK), &blocked), guard_verdict(Some(RULEBOOK), &blocked, "", false, None).block_reason);

        let compliant = long("TLDR: it works. the commit fsck run is done, all gepusht");
        assert_eq!(
            block_reason(Some(RULEBOOK), &compliant),
            guard_verdict(Some(RULEBOOK), &compliant, "", false, None).block_reason
        );

        let short = "commit done, gepusht";
        assert_eq!(block_reason(Some(RULEBOOK), short), guard_verdict(Some(RULEBOOK), short, "", false, None).block_reason);
    }

    // ------------------------------------------------------------- tier
    const TIERED_RULEBOOK: &str = r#"[
      {"id":"warn-example","tier":"warn","any_of":["ik zou liever"],"none_of":["any_of"],
       "reminder":"consider phrasing this as a preference, not a directive"},
      {"id":"block-example","tier":"block","any_of":["nooit meer doen"],"none_of":["any_of"],
       "reminder":"this is a hard rule, not a suggestion"},
      {"id":"untiered-example","any_of":["mystery phrase"],"none_of":["any_of"],
       "reminder":"no tier key at all - must still behave like block"},
      {"id":"bogus-tier-example","tier":"maybe","any_of":["bogus phrase"],"none_of":["any_of"],
       "reminder":"an unrecognized tier value - must still behave like block"}
    ]"#;

    /// THE DEFECT THIS PREVENTS: a rule the owner has explicitly marked WARN
    /// (SPEC-ENFORCEMENT.md 1.1 - "emit text, never stop the action") still
    /// ends up stopping the reply, because `respond.rs` parsed the tier and
    /// then discarded it.
    #[test]
    fn a_warn_tier_rule_never_blocks() {
        let v = guard_verdict(Some(TIERED_RULEBOOK), "ik zou liever dit anders zien", "", false, None);
        assert!(v.block_reason.is_none(), "{:?}", v.block_reason);
        let warn = v.warn_reason.expect("the warn tier must still say something");
        assert!(warn.contains("preference"), "{warn}");
    }

    #[test]
    fn a_block_tier_rule_still_blocks() {
        let v = guard_verdict(Some(TIERED_RULEBOOK), "dat nooit meer doen, begrepen?", "", false, None);
        let reason = v.block_reason.expect("a block-tier match must still block");
        assert!(reason.contains("hard rule"), "{reason}");
    }

    /// Today, every rule in every shipped rulebook has no `tier` key at all
    /// and blocks when it fires. A rule with no tier, or with a tier value
    /// this parser does not recognise, must default to that SAME behaviour -
    /// never silently downgrade to non-blocking just because the field is
    /// absent or misspelled.
    #[test]
    fn an_unknown_or_missing_tier_defaults_to_block() {
        let missing = guard_verdict(Some(TIERED_RULEBOOK), "a mystery phrase appears here", "", false, None);
        assert!(missing.block_reason.is_some(), "no tier key at all must still block");

        let bogus = guard_verdict(Some(TIERED_RULEBOOK), "a bogus phrase appears here", "", false, None);
        assert!(bogus.block_reason.is_some(), "an unrecognised tier value must still block");
    }

    /// A message that trips both a block-tier and a warn-tier rule at once:
    /// the warn's text must never leak into the block reason (SPEC-
    /// ENFORCEMENT.md 1.1: BLOCK requires a conclusive, cited violation, not
    /// one diluted with a merely-possible one) and vice versa.
    #[test]
    fn block_and_warn_reasons_never_mix() {
        let msg = "ik zou liever dit anders zien, en dat nooit meer doen, begrepen?";
        let v = guard_verdict(Some(TIERED_RULEBOOK), msg, "", false, None);
        let block = v.block_reason.expect("block-example must still fire");
        let warn = v.warn_reason.expect("warn-example must still fire");
        assert!(!block.contains("preference"), "{block}");
        assert!(warn.contains("preference"), "{warn}");
        assert!(!warn.contains("hard rule"), "{warn}");
        assert!(block.contains("hard rule"), "{block}");
    }

    // ------------------------------------------------- FALSE BLOCK B: evidence
    //
    // Mirrors the live `checked-claim-needs-evidence` rule closely enough to
    // be a faithful regression test, without depending on the owner's own
    // installed rulebook (this crate's tests must stand on their own - same
    // stance as every other rulebook constant in this file).
    const EVIDENCE_RULEBOOK: &str = r#"[
      {"id":"checked-claim-needs-evidence","tier":"block",
       "any_of":["gecheckt","gecontroleerd","geverifieerd","nagekeken","verified","confirmed","checked against","gevalideerd"],
       "none_of":[".py:",".mjs:",".cfg:",".md:",".js:",".json:",".rs:",".ts:",".sh:",".ps1:","commit ","regel ","line ","niet gecheckt","not checked","niet geverifieerd","not verified","niet nagekeken","not looked up","niet gecontroleerd","niet gevalideerd","nog niet gecheckt","niet bevestigd","not confirmed","unverified"],
       "none_of_patterns":["commit_sha","path_line"],
       "reminder":"Je beweert dat iets gecheckt of geverifieerd is zonder bewijs. Noem het bestand met regelnummer (pad:regel) of de commit, of zeg letterlijk 'niet gecheckt'."}
    ]"#;

    /// THE EXACT ANSWER that was wrongly blocked (FALSE BLOCK B, reported by
    /// the owner from other sessions, fixed 2026-09-07): five real commit
    /// shas named after the plural "commits" (the rule's own escape said
    /// "commit " - singular, trailing space - so it never matched), and two
    /// real files with no line number attached. `none_of_patterns` must let
    /// this through on the shas alone.
    #[test]
    fn false_block_b_a_checked_claim_naming_real_commit_shas_now_passes() {
        let msg = "Business gecheckt met bewijs: git log in de business-repo toont commits a2770f8e \
                    (plaatkeuze naast filament, met clienttests), be1e8b3b (tab Fleet maintenance plus \
                    proxy-fix), 0dca539b (1.0.24), 9b3d5132 (onbekende plaat blokkeert dispatch niet), \
                    faa6d10a (1.0.25); werkmap schoon, server/package.json versie 1.0.25, en \
                    server/public/dashboard.html bevat de knop btn-nav-fleet. Niets verloren.";
        let v = guard_verdict(Some(EVIDENCE_RULEBOOK), msg, "", false, None);
        assert!(v.block_reason.is_none(), "{:?}", v.block_reason);
    }

    /// The other half of the same fix, kept honest: a claim with NO sha and
    /// no path at all must still block exactly as before - broadening the
    /// escape must never turn the rule into a no-op.
    #[test]
    fn a_bare_checked_claim_with_no_sha_or_path_still_blocks() {
        let msg = "Ja, dat heb ik gecheckt, het klopt allemaal.";
        let v = guard_verdict(Some(EVIDENCE_RULEBOOK), msg, "", false, None);
        assert!(v.block_reason.is_some(), "a bare claim with nothing to point at must still block");
    }

    /// A path WITH a line number, in an extension the rule's own fixed
    /// `none_of` list never enumerated, must also escape - proving
    /// `path_line` is a genuine shape check, not a repeat of the fixed list.
    #[test]
    fn a_path_line_citation_in_an_unlisted_extension_escapes_the_rule() {
        let msg = "Gecheckt: config/settings.yaml:42 bevat de verkeerde waarde.";
        let v = guard_verdict(Some(EVIDENCE_RULEBOOK), msg, "", false, None);
        assert!(v.block_reason.is_none(), "{:?}", v.block_reason);
    }

    // ------------------------------------------ checked citations (2026-09-26)

    /// The caller's lookup, stood in for: only `src/real.rs` exists, and it
    /// has ten lines.
    fn only_real_rs(path: &str, line: u64) -> bool {
        path == "src/real.rs" && (1..=10).contains(&line)
    }

    /// The repository, stood in for: `a2770f8e` is its one commit.
    fn only_a2770f8e(candidates: &[String]) -> Option<HashSet<String>> {
        Some(candidates.iter().filter(|c| c.as_str() == "a2770f8e").cloned().collect())
    }

    /// No repository answers for the working directory.
    fn no_repository(_: &[String]) -> Option<HashSet<String>> {
        None
    }

    fn lookup() -> EvidenceLookup<'static> {
        EvidenceLookup { line_exists: &only_real_rs, real_commits: &only_a2770f8e }
    }

    /// THE ESCAPE THIS CLOSES: a claim of having verified something, backed by
    /// a citation nobody opened - a line the file does not have, or a file
    /// that is not there - is held once the caller can look, and the reason
    /// names the citation that did not count.
    #[test]
    fn a_verified_claim_citing_a_line_that_is_not_there_is_held_when_the_caller_can_look() {
        for msg in ["Verified: src/real.rs:99 has the fix.", "Verified: src/made_up.rs:12 has the fix."] {
            let v = guard_verdict(Some(EVIDENCE_RULEBOOK), msg, "", false, Some(&lookup()));
            let reason = v.block_reason.unwrap_or_else(|| panic!("an unchecked citation must not count: {msg}"));
            assert!(reason.contains("these did not"), "{reason}");
        }
    }

    #[test]
    fn a_verified_claim_citing_a_real_line_passes() {
        let v = guard_verdict(Some(EVIDENCE_RULEBOOK), "Verified: src/real.rs:3 has the fix.", "", false, Some(&lookup()));
        assert!(v.block_reason.is_none(), "{:?}", v.block_reason);
    }

    /// Unchanged when the caller cannot look: the citation is judged by its
    /// shape, as before.
    #[test]
    fn without_a_lookup_a_citation_still_counts_by_its_shape() {
        let v = guard_verdict(Some(EVIDENCE_RULEBOOK), "Verified: src/made_up.rs:12 has the fix.", "", false, None);
        assert!(v.block_reason.is_none(), "{:?}", v.block_reason);
    }

    /// A made-up file name that looks like hex must not pass as a commit sha,
    /// while a real sha elsewhere in the answer, and saying plainly that it
    /// was not checked, both still count.
    #[test]
    fn an_unchecked_citation_counts_for_nothing_but_the_other_escapes_still_do() {
        let held = guard_verdict(Some(EVIDENCE_RULEBOOK), "Verified: src/deadbeef42.rs:7 has the fix.", "", false, Some(&lookup()));
        assert!(held.block_reason.is_some(), "a hex-looking made-up file name must not pass as a sha");
        for msg in [
            "Verified in a2770f8e, see src/made_up.rs:12.",
            "Verified: src/made_up.rs:12 looks right, but this is unverified.",
        ] {
            let v = guard_verdict(Some(EVIDENCE_RULEBOOK), msg, "", false, Some(&lookup()));
            assert!(v.block_reason.is_none(), "{msg}: {:?}", v.block_reason);
        }
    }

    /// An absolute Windows path is read whole, drive letter included.
    #[test]
    fn an_absolute_windows_citation_reaches_the_lookup_whole() {
        let line_exists = |path: &str, line: u64| path == r"C:\repo\src\real.rs" && line == 3;
        let lookup = EvidenceLookup { line_exists: &line_exists, real_commits: &no_repository };
        let v = guard_verdict(Some(EVIDENCE_RULEBOOK), r"Verified: C:\repo\src\real.rs:3 has the fix.", "", false, Some(&lookup));
        assert!(v.block_reason.is_none(), "{:?}", v.block_reason);
    }

    /// A rule that never took a citation as evidence (no `path_line` among its
    /// patterns) is judged exactly as before, lookup or not.
    #[test]
    fn a_rule_that_takes_no_citation_as_evidence_is_untouched_by_the_lookup() {
        let rulebook = r#"[{"id":"plain","any_of":["verified"],"none_of":[".rs:"],"reminder":"plain rule"}]"#;
        let v = guard_verdict(Some(rulebook), "Verified: src/made_up.rs:12 has the fix.", "", false, Some(&lookup()));
        assert!(v.block_reason.is_none(), "{:?}", v.block_reason);
    }

    // ---------------------------------- commits, line numbers (2026-09-26)

    /// A repository whose one commit's short form is all digits (about one
    /// commit in forty-three is like that).
    fn only_12345678(candidates: &[String]) -> Option<HashSet<String>> {
        Some(candidates.iter().filter(|c| c.as_str() == "12345678").cloned().collect())
    }

    fn held_by_digit_repository(msg: &str) -> Option<String> {
        let lookup = EvidenceLookup { line_exists: &only_real_rs, real_commits: &only_12345678 };
        guard_verdict(Some(EVIDENCE_RULEBOOK), msg, "", false, Some(&lookup)).block_reason
    }

    /// THE FLAKE (step 31 report): a real sha whose first eight characters are
    /// all digits was not looked up at all, so "commit 12345678" never counted.
    #[test]
    fn a_real_commit_written_as_eight_digits_counts_when_it_is_named_as_a_commit() {
        for msg in [
            "Verified: commit 12345678 has the fix.",
            "Verified: commit: 12345678 has the fix.",
            "Verified: commit `12345678` has the fix.",
            "Verified in sha 12345678.",
            "Verified in Commit 12345678.",
        ] {
            assert!(held_by_digit_repository(msg).is_none(), "a real commit must count: {msg}");
        }
        let fake = held_by_digit_repository("Verified: commit 87654321 has the fix.").expect("a made-up sha is held");
        assert!(fake.contains("87654321"), "the reason must name it: {fake}");
    }

    /// A number that is not named as a commit stays a number: it is not looked
    /// up, does not count as evidence and is not listed as a missing commit.
    #[test]
    fn a_bare_run_of_digits_is_still_not_a_commit() {
        let reason = held_by_digit_repository("Verified: ran 12345678 checks on 20260929.").expect("no evidence: held");
        assert!(!reason.contains("no such commit"), "{reason}");
        assert!(held_by_digit_repository("Verified: the 12345678 commit has the fix").is_some());
    }

    fn held(msg: &str) -> bool {
        guard_verdict(Some(EVIDENCE_RULEBOOK), msg, "", false, Some(&lookup())).block_reason.is_some()
    }

    /// THE ESCAPE THIS CLOSES: the word "commit" and a typed sha let a claim
    /// through by their shape; now only a commit the repository has counts.
    #[test]
    fn a_commit_counts_only_when_the_repository_has_it() {
        assert!(!held("Verified: commit a2770f8e has the fix."));
        assert!(!held("Gecontroleerd: a2770f8e bevat de fix."));
        assert!(held("Verified: commit 1b2c3d4e has the fix."));
        assert!(held("Verified: the last commit has the fix."));
        let reason = guard_verdict(Some(EVIDENCE_RULEBOOK), "Verified in 1b2c3d4e.", "", false, Some(&lookup()))
            .block_reason
            .expect("a made-up sha must not count");
        assert!(reason.contains("1b2c3d4e"), "the reason names the sha that did not count: {reason}");
    }

    /// Where no repository answers, a sha is judged by its shape, as before.
    #[test]
    fn without_a_repository_a_sha_and_the_word_commit_count_by_their_shape() {
        let lookup = EvidenceLookup { line_exists: &only_real_rs, real_commits: &no_repository };
        for msg in ["Verified in 1b2c3d4e.", "Verified: the last commit has the fix."] {
            let v = guard_verdict(Some(EVIDENCE_RULEBOOK), msg, "", false, Some(&lookup));
            assert!(v.block_reason.is_none(), "{msg}: {:?}", v.block_reason);
        }
    }

    /// THE ESCAPE THIS CLOSES: "line " and "regel " counted anywhere, even
    /// inside another word; now only a line number a named file really has.
    #[test]
    fn a_line_number_counts_only_when_a_named_file_has_that_line() {
        assert!(!held("Verified: line 3 of src/real.rs has the fix."));
        assert!(!held("Geverifieerd: regel 4 van src/real.rs klopt."));
        assert!(!held("Verified: src/real.rs, lines 2-5, has the fix."));
        assert!(held("Verified: line 30 of src/real.rs has the fix."));
        assert!(held("Verified: line 3 of src/made_up.rs has the fix."));
        assert!(held("Verified: regel 2 klopt."));
        assert!(held("Verified: the pipeline is green."));
        assert!(held("Geverifieerd: de vuistregel klopt."));
    }

    /// A file named on another line of the answer does not vouch for a line
    /// number here.
    #[test]
    fn a_line_number_needs_its_file_on_the_same_line_of_the_answer() {
        assert!(held("See src/real.rs.\nVerified: line 3 has the fix."));
    }

    /// THE ESCAPE THIS CLOSES: ".rs:" matched a file name followed by a colon
    /// with no line at all.
    #[test]
    fn an_extension_with_a_colon_counts_only_inside_a_citation_that_checked_out() {
        assert!(held("Verified: src/real.rs: the fix is there."));
        assert!(!held("Verified: src/real.rs:3 has the fix."));
    }

    /// The honest escapes are untouched.
    #[test]
    fn saying_it_was_not_checked_still_passes() {
        assert!(!held("Verified: the pipeline, although line 30 is not checked."));
    }

    // ------------------------------------------ reported speech (2026-09-26)

    /// Mirrors the owner's own `claim-no-access-without-checking` rule, with
    /// the new field set, and a twin without it.
    const OWN_CLAIMS_RULEBOOK: &str = r#"[
      {"id":"claim-no-access-without-checking","own_claims_only":true,
       "any_of":["kan ik van hieruit niet","geen tailnet","geen nas-toegang","geen toegang","ik heb geen toegang",
                 "i can't reach","i don't have access","no access to the nas"],
       "none_of":["read-only","alleen-lezen","vals alarm","false positive","any_of"],
       "reminder":"Verify before claiming you cannot."}
    ]"#;

    fn own_claim_held(msg: &str) -> bool {
        guard_verdict(Some(OWN_CLAIMS_RULEBOOK), msg, "", false, None).block_reason.is_some()
    }

    /// THE FALSE BLOCK THIS CLOSES, in the shapes a report takes.
    #[test]
    fn another_partys_reported_or_quoted_words_do_not_fire() {
        for msg in [
            "Het model meldde geen toegang te hebben tot de NAS.",
            "Volgens de agent is er geen toegang tot de share.",
            "Ik heb het model gevraagd en het meldde geen toegang te hebben.",
            "Het model meldde: ik heb geen toegang tot de NAS.",
            "The subagent said it had no access to the NAS.",
            "The subagent reported: I don't have access to the NAS.",
            "According to the agent, there is no access to the NAS.",
            "\"Ik heb geen toegang\", zei de agent.",
            "De agent schreef \"geen toegang\" terug.",
            "The tool answered `I don't have access` and stopped.",
            "Het model schreef:\n> ik heb geen toegang tot de NAS",
            "De uitvoer van het model:\n```\nik heb geen toegang\n```",
        ] {
            assert!(!own_claim_held(msg), "reported speech must not fire: {msg}");
        }
    }

    /// The other half: the assistant's own claim still fires, also next to a
    /// report, also when it reports its own words.
    #[test]
    fn the_assistants_own_claim_still_fires() {
        for msg in [
            "Ik heb geen toegang tot de NAS.",
            "Daar heb ik geen toegang toe.",
            "I don't have access to the NAS from here.",
            "Het model meldde niets bijzonders en ik heb geen toegang tot de NAS.",
            "Het model meldde niets, ik heb geen toegang.",
            "The agent said it was fine, but I can't reach the NAS.",
            "Ik meldde al: geen toegang.",
            "Volgens mij heb ik geen toegang.",
            "\"Geen toegang\", zei ik.",
            "Ik kreeg \"geen toegang\" terug.",
            "Ik kreeg deze melding:\n> geen toegang",
        ] {
            assert!(own_claim_held(msg), "the assistant's own claim must still fire: {msg}");
        }
    }

    /// No rule changes by itself: without the field, a report fires exactly
    /// as before, and a malformed value reads as absent.
    #[test]
    fn without_own_claims_only_a_report_still_fires() {
        for field in ["", r#""own_claims_only":"yes","#] {
            let rulebook = format!(
                r#"[{{"id":"r",{field}"any_of":["geen toegang"],"none_of":["any_of"],"reminder":"no access claim"}}]"#
            );
            let v = guard_verdict(Some(&rulebook), "Het model meldde geen toegang te hebben.", "", false, None);
            assert!(v.block_reason.is_some(), "{field}");
        }
    }

    /// The owner's `claim-no-access-without-checking` rule as narrowed on
    /// 2026-10-02: the bare "geen toegang" is gone, only first-person forms
    /// remain. (The repo ships no copy of this rule in its example rulebook,
    /// so this is an inline copy of the live rule.)
    const NARROWED_NO_ACCESS_RULEBOOK: &str = r#"[
      {"id":"claim-no-access-without-checking","own_claims_only":true,
       "any_of":["kan ik van hieruit niet","geen tailnet","geen nas-toegang","heb ik geen toegang",
                 "ik heb er geen toegang","ik heb geen toegang",
                 "i can't reach","i don't have access","no access to the nas"],
       "none_of":["read-only","alleen-lezen","vals alarm","false positive","use versus mention","als voorbeeld","any_of"],
       "reminder":"Verify before claiming you cannot."}
    ]"#;

    fn narrowed_no_access_held(msg: &str) -> bool {
        guard_verdict(Some(NARROWED_NO_ACCESS_RULEBOOK), msg, "", false, None).block_reason.is_some()
    }

    /// FALSE FIRE SEEN LIVE 2026-10-02: a sentence about a third party's
    /// permission scope ("De assistent ... geen toegang tot ...") is not the
    /// assistant claiming it cannot reach something.
    #[test]
    fn a_third_partys_permission_scope_does_not_fire_the_no_access_rule() {
        let msg = "Is het een kleine, duidelijke oplossing, dan bouwt hij die met een test. \
                   Die test moet eerst aantoonbaar falen en daarna slagen. \
                   De assistent blijft enkel lezen: geen toegang tot geld, bestellingen of klantgegevens. \
                   Hij zet niets op dev en commit niets.";
        assert!(!narrowed_no_access_held(msg));
    }

    /// The first-person claims still fire after the narrowing.
    #[test]
    fn first_person_no_access_claims_still_fire_after_narrowing() {
        for msg in [
            "Ik heb geen toegang tot de NAS.",
            "Daar heb ik geen toegang toe.",
            "Ik heb er geen toegang toe.",
        ] {
            assert!(narrowed_no_access_held(msg), "must still fire: {msg}");
        }
    }

    /// An ordinary Dutch/English word that happens to use only hex-alphabet
    /// letters (a-f) must never be misread as a sha - it carries no digit at
    /// all, which `contains_commit_sha` requires.
    #[test]
    fn an_all_letter_word_is_never_mistaken_for_a_commit_sha() {
        assert!(!contains_commit_sha("de gevel is af, geen bewijs nodig"));
        assert!(!contains_commit_sha("facade decade cabbage"));
    }

    /// A long run of plain digits (a count, a version-ish number) carries no
    /// LETTER at all, so it must never be mistaken for a sha either.
    #[test]
    fn a_pure_digit_run_is_never_mistaken_for_a_commit_sha() {
        assert!(!contains_commit_sha("er zijn 1234567890 regels gelezen"));
    }

    #[test]
    fn a_real_short_sha_is_detected_case_insensitively() {
        assert!(contains_commit_sha("commits a2770f8e en BE1E8B3B"));
    }

    // ------------------------------------------- step 29: the reminder's words

    #[test]
    fn an_internal_id_in_a_bracket_is_not_part_of_the_message() {
        assert_eq!(
            without_internal_ids("The owner standing rule (mcp-0a1b2c3d): open with a TLDR."),
            "The owner standing rule: open with a TLDR."
        );
        assert_eq!(without_internal_ids("rule (alpha:mem-0a1b2c3d): drop it"), "rule: drop it");
        // Anything else in a bracket stays.
        assert_eq!(without_internal_ids("no ids (hashes, ports) here"), "no ids (hashes, ports) here");
        assert_eq!(without_internal_ids("see (mcp-12) and (a-1234)"), "see (mcp-12) and (a-1234)");
        assert_eq!(without_internal_ids("an open ( bracket"), "an open ( bracket");
    }

    const ASKED_RULEBOOK: &str = r#"[
      {"id":"answer-is-too-long","tier":"block","owner_reading_only":true,
       "any_of":[],"none_of":["```"],"min_chars":1000,
       "asked_for_any_of":["testuitslag","test result"],
       "reminder":"Over 1000 characters (mcp-0a1b2c3d): no test counts, no ids.",
       "reminder_if_asked":"Over 1000 characters: he asked for the test result, so give that number, plainly, and stop."}
    ]"#;

    #[test]
    fn the_reminder_without_a_matching_prompt_names_no_id() {
        let long = "x".repeat(1200);
        let v = guard_verdict(Some(ASKED_RULEBOOK), &long, "Kun je het even uitleggen?", false, None);
        let reason = v.block_reason.expect("long answer blocks");
        assert!(reason.contains("no test counts"), "{reason}");
        assert!(!reason.contains("mcp-0a1b2c3d"), "{reason}");
    }

    #[test]
    fn a_prompt_that_asks_for_the_test_result_gets_the_reminder_that_allows_it() {
        let long = "x".repeat(1200);
        let v = guard_verdict(Some(ASKED_RULEBOOK), &long, "Geef me de echte testuitslag erbij", false, None);
        let reason = v.block_reason.expect("still too long: the length rule stays");
        assert!(reason.contains("he asked for the test result"), "{reason}");
        assert!(!reason.contains("no test counts"), "{reason}");
    }

    // ------------------------------- step 32: the length hold does not guess the request

    const QUOTING_RULEBOOK: &str = r#"[
      {"id":"answer-is-too-long","tier":"block","owner_reading_only":true,
       "any_of":[],"none_of":["```"],"min_chars":1000,
       "asked_for_any_of":["testuitslag","test result"],
       "reminder":"Over 1000 characters. Keep every item his own prompt asked for. {owner_request} No ids unless asked.",
       "reminder_if_asked":"Over 1000 characters. Keep every item his own prompt asked for, test result included. {owner_request} Plain words."}
    ]"#;

    const TWO_ITEM_PROMPT: &str = "Maak het lettertype in de widget een keer laadbaar. Draai daarna alle tests. \
                                   Geef op het einde kort wat je veranderde en de echte testuitslag. Verder niets.";

    /// THE DEFECT (field test 7): the hold said "give the test result and the
    /// count", so the agent dropped "what you changed", which the prompt also
    /// asked for. The hold now quotes the owner's own sentence instead.
    #[test]
    fn the_hold_quotes_the_sentence_that_names_everything_the_owner_asked_for() {
        let long = "x".repeat(1200);
        let v = guard_verdict(Some(QUOTING_RULEBOOK), &long, TWO_ITEM_PROMPT, false, None);
        let reason = v.block_reason.expect("long answer blocks");
        assert!(
            reason.contains("Geef op het einde kort wat je veranderde en de echte testuitslag."),
            "the owner's own sentence, in his own case, must be quoted: {reason}"
        );
        assert!(!reason.contains("{owner_request}"), "{reason}");
        assert!(!reason.contains("Maak het lettertype"), "only the asking sentence is quoted: {reason}");
    }

    #[test]
    fn a_prompt_without_a_known_term_is_quoted_from_its_first_words() {
        let long = "x".repeat(1200);
        let prompt = "Leg uit hoe de cache werkt en wat je zou aanpassen. ".repeat(10);
        let v = guard_verdict(Some(QUOTING_RULEBOOK), &long, &prompt, false, None);
        let reason = v.block_reason.expect("long answer blocks");
        assert!(reason.contains("Leg uit hoe de cache werkt"), "{reason}");
        assert!(!reason.contains("{owner_request}"), "{reason}");
        assert!(reason.len() < 700, "the quote is cut short: {} bytes", reason.len());
    }

    /// Field test 8: the prompt's report request is its LAST sentence and shares no
    /// word with the rulebook's terms; the hold quoted the first 240 characters
    /// instead, cut in the middle of a word.
    #[test]
    fn the_hold_quotes_the_sentence_with_a_report_verb_not_the_start_of_the_prompt() {
        let long = "x".repeat(1200);
        let prompt = "files/deploy.sh controleert bij een dev-herstart nog containers zoals alpha-a en alpha-b.                       Zoek in deze repo uit welke dev-containers echt bestaan (docker-compose-bestanden en documentatie),                       en pas de lijst in het script aan zodat een dev-herstart niet faalt op containers die er niet meer zijn.                       Doe het werk zelf. Controleer de syntax van het script. Alleen lokaal: niets committen.                       Geef op het einde kort wat je veranderde en hoe je het controleerde.";
        let v = guard_verdict(Some(QUOTING_RULEBOOK), &long, prompt, false, None);
        let reason = v.block_reason.expect("long answer blocks");
        assert!(reason.contains("Geef op het einde kort wat je veranderde en hoe je het controleerde."), "{reason}");
        assert!(!reason.contains("files/deploy.sh controleert"), "{reason}");
    }

    #[test]
    fn a_prompt_with_no_report_sentence_is_quoted_from_its_last_sentence() {
        let long = "x".repeat(1200);
        let prompt = "Leg uit hoe de cache werkt. Kijk vooral naar de eviction. Houd het bij de feiten.";
        let v = guard_verdict(Some(QUOTING_RULEBOOK), &long, prompt, false, None);
        let reason = v.block_reason.expect("long answer blocks");
        assert!(reason.contains("His own words were: \"Houd het bij de feiten.\""), "{reason}");
    }

    #[test]
    fn a_long_quote_is_cut_at_a_word_boundary() {
        let quote = owner_request_quote(&format!("Vertel {}.", "woordje ".repeat(60)), &[]);
        assert!(quote.ends_with("woordje...\"."), "{quote}");
        assert!(quote.chars().count() < 280, "{quote}");
    }

    #[test]
    fn an_unreadable_prompt_leaves_no_placeholder_and_no_quote() {
        let long = "x".repeat(1200);
        let v = guard_verdict(Some(QUOTING_RULEBOOK), &long, "", false, None);
        let reason = v.block_reason.expect("long answer blocks");
        assert!(!reason.contains("{owner_request}"), "{reason}");
        assert!(!reason.contains("  "), "no doubled space left behind: {reason}");
        assert!(reason.contains("Keep every item his own prompt asked for"), "{reason}");
    }

    #[test]
    fn a_dot_inside_a_file_name_does_not_cut_the_quoted_sentence() {
        let long = "x".repeat(1200);
        let prompt = "Pas lib/pdf.js aan. Vertel kort in de testuitslag wat er in lib/pdf.js veranderde.";
        let v = guard_verdict(Some(QUOTING_RULEBOOK), &long, prompt, false, None);
        let reason = v.block_reason.expect("long answer blocks");
        assert!(reason.contains("Vertel kort in de testuitslag wat er in lib/pdf.js veranderde."), "{reason}");
    }

    /// THE SECOND ROUND (found while replaying field test 7): after a hold, Claude
    /// Code writes the hold's own text into the transcript as a user line
    /// (`isMeta`, "Stop hook feedback: ..."). It is not something the owner
    /// typed, and must never be read as his prompt - or the next hold would
    /// quote THOR's own words as his request.
    #[test]
    fn a_stop_hook_feedback_line_is_not_the_owners_prompt() {
        let transcript = [
            r#"{"type":"user","message":{"role":"user","content":"Tel de tests en vertel wat je veranderde."}}"#,
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"een lang antwoord"}]}}"#,
            r#"{"type":"user","isMeta":true,"message":{"role":"user","content":"Stop hook feedback:\n[THOR] This answer is over 1000 characters."}}"#,
        ]
        .join("\n");
        assert_eq!(last_user_prompt(&transcript).as_deref(), Some("Tel de tests en vertel wat je veranderde."));
        let unflagged = transcript.replace("\"isMeta\":true,", "");
        assert_eq!(
            last_user_prompt(&unflagged).as_deref(),
            Some("Tel de tests en vertel wat je veranderde."),
            "the text alone names it, even without the flag"
        );
    }

    // ------------------------------- step 33: a claim backed by a tool call of the turn

    const CLAIM_RULEBOOK: &str = r#"[
      {"id":"checked-claim-needs-evidence","tier":"block",
       "any_of":["gecontroleerd:","checked:"],
       "none_of":[".sh:","commit ","niet gecheckt","not checked"],
       "none_of_patterns":["commit_sha","path_line"],
       "reminder":"You are claiming something was checked without evidence."}
    ]"#;

    fn transcript_with_commands(commands: &[&str]) -> String {
        let mut lines = vec![r#"{"type":"user","message":{"role":"user","content":"Pas het script aan en controleer het."}}"#.to_string()];
        for c in commands {
            lines.push(format!(
                r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"tool_use","name":"Bash","input":{{"command":"{c}"}}}}]}}}}"#
            ));
        }
        lines.join("
")
    }

    /// THE DEFECT (field test 8): "Gecontroleerd: de syntaxcontrole is geslaagd" after a
    /// real `sh -n` in the same turn was held for "no evidence".
    #[test]
    fn a_claim_backed_by_a_check_run_in_the_same_turn_passes() {
        let claim = "Gecontroleerd: de syntaxcontrole is geslaagd.";
        let bare = guard_verdict_in_turn(Some(CLAIM_RULEBOOK), claim, "", false, None, &[]);
        assert!(bare.block_reason.is_some(), "no tool call, no named command: the claim still needs evidence");
        let commands = turn_commands(&transcript_with_commands(&["sh -n files/deploy.sh && echo SYNTAX_OK"]));
        assert_eq!(commands, vec!["sh -n files/deploy.sh && echo SYNTAX_OK".to_string()]);
        let backed = guard_verdict_in_turn(Some(CLAIM_RULEBOOK), claim, "", false, None, &commands);
        assert!(backed.block_reason.is_none(), "{:?}", backed.block_reason);
        for command in ["node --test", "cargo test --release", "npm test", "git diff --stat", "bash -n x.sh", "python -m py_compile a.py"] {
            let v = guard_verdict_in_turn(Some(CLAIM_RULEBOOK), claim, "", false, None, &[command.to_string()]);
            assert!(v.block_reason.is_none(), "{command}");
        }
    }

    #[test]
    fn a_tool_call_that_checks_nothing_does_not_back_a_claim() {
        let v = guard_verdict_in_turn(Some(CLAIM_RULEBOOK), "Gecontroleerd: alles werkt.", "", false, None, &["ls -la".to_string(), "cat a".to_string()]);
        assert!(v.block_reason.is_some());
    }

    #[test]
    fn an_answer_that_names_the_command_it_ran_counts() {
        let v = guard_verdict_in_turn(Some(CLAIM_RULEBOOK), "Gecontroleerd: `sh -n` gaf geen fout.", "", false, None, &[]);
        assert!(v.block_reason.is_none(), "{:?}", v.block_reason);
    }

    #[test]
    fn only_the_current_turns_commands_are_read() {
        let transcript = [
            r#"{"type":"user","message":{"role":"user","content":"eerste vraag"}}"#,
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","name":"Bash","input":{"command":"cargo test"}}]}}"#,
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","content":"ok"}]}}"#,
            r#"{"type":"user","message":{"role":"user","content":"tweede vraag"}}"#,
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","name":"Read","input":{"file_path":"a"}},{"type":"tool_use","name":"Bash","input":{"command":"ls"}}]}}"#,
        ]
        .join("
");
        // The earlier turn's `cargo test` is not read; this turn's Read shows as a marker.
        assert_eq!(turn_commands(&transcript), vec!["[tool] Read a".to_string(), "ls".to_string()]);
    }

    // -------------- step 59: the turn's own tool calls back a claim, for a rule that opts in
    //
    // THE FALSE BLOCK (owner, 2026-10-09): "Zelf nagekeken ... ik heb de scripts van de
    // admin-pagina zelf gecontroleerd: alle drie in orde. Ingelogd heb ik het niet gezien"
    // was held for "no evidence" although the turn had run curl, git and a node check. The
    // reminder asked for a path:line, which the owner's short-answer rule forbids. A rule
    // with `backed_by_turn_tools` stands aside when the turn looked at something itself.

    const BACKED_RULEBOOK: &str = r#"[
      {"id":"checked","tier":"block","backed_by_turn_tools":true,
       "any_of":["gecontroleerd:","nagekeken"],"none_of":["niet gecontroleerd"],
       "reminder":"You are claiming something was checked without evidence."}
    ]"#;

    fn transcript_with_tool_calls(calls: &[(&str, &str)]) -> String {
        let mut lines = vec![r#"{"type":"user","message":{"role":"user","content":"Kijk na of de admin werkt."}}"#.to_string()];
        for (name, input) in calls {
            lines.push(format!(
                r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"tool_use","name":"{name}","input":{input}}}]}}}}"#
            ));
        }
        lines.join("\n")
    }

    #[test]
    fn the_turn_lists_the_tools_that_look_and_not_the_ones_that_only_launch_or_write() {
        let transcript = transcript_with_tool_calls(&[
            ("Read", r#"{"file_path":"a.html"}"#),
            ("Grep", r#"{"pattern":"x"}"#),
            ("Agent", r#"{"prompt":"do the work"}"#),
            ("Edit", r#"{"file_path":"a.html"}"#),
            ("mcp__thor__remember", r#"{"id":"x"}"#),
            ("mcp__thor__lookup", r#"{"query":"x"}"#),
            ("mcp__Claude_Browser__get_page_text", r#"{}"#),
            ("WebFetch", r#"{"url":"https://example.com"}"#),
        ]);
        assert_eq!(
            turn_commands(&transcript),
            vec![
                "[tool] Read a.html",
                "[tool] Grep x",
                "[tool] mcp__Claude_Browser__get_page_text",
                "[tool] WebFetch https://example.com"
            ]
        );
    }

    #[test]
    fn a_rule_that_opts_in_is_backed_by_a_read_or_a_command_of_the_same_turn() {
        let claim = "Zelf nagekeken. Ik heb de pagina admin.html zelf gecontroleerd: alle drie in orde.";
        let bare = guard_verdict_in_turn(Some(BACKED_RULEBOOK), claim, "", false, None, &[]);
        assert!(bare.block_reason.is_some(), "no tool call in the turn: the claim stays held");
        for calls in [
            vec![("Read", r#"{"file_path":"server/public/admin.html"}"#)],
            vec![("Grep", r#"{"pattern":"admin"}"#)],
            vec![("Bash", r#"{"command":"curl -s http://dev/admin"}"#)],
            vec![("PowerShell", r#"{"command":"Get-Content admin.html"}"#)],
            vec![("Bash", r#"{"command":"node check.mjs","description":"Syntax check of the admin page"}"#)],
        ] {
            let commands = turn_commands(&transcript_with_tool_calls(&calls));
            let v = guard_verdict_in_turn(Some(BACKED_RULEBOOK), claim, "", false, None, &commands);
            assert!(v.block_reason.is_none(), "{calls:?}: {:?}", v.block_reason);
        }
    }

    /// Collect 17b: any look in the turn used to back any claim of checking. A
    /// look at something the claim does not name - another file, a listing, a
    /// folder - does not.
    #[test]
    fn a_look_at_something_the_claim_does_not_name_does_not_back_it() {
        let claim = "Zelf nagekeken. Ik heb de pagina admin.html zelf gecontroleerd: alle drie in orde.";
        for calls in [
            vec![("Read", r#"{"file_path":"server/notes.md"}"#)],
            vec![("Grep", r#"{"pattern":"TODO","path":"C:\\Users\\me\\dev\\proj"}"#)],
            vec![("Bash", r#"{"command":"ls -la"}"#)],
            vec![("Bash", r#"{"command":"git -C C:/Users/me/dev/proj status -sb"}"#)],
            vec![("WebFetch", r#"{"url":"https://example.com/pricing"}"#)],
        ] {
            let commands = turn_commands(&transcript_with_tool_calls(&calls));
            assert!(!commands.is_empty(), "fixture sanity, the turn did look: {calls:?}");
            let v = guard_verdict_in_turn(Some(BACKED_RULEBOOK), claim, "", false, None, &commands);
            assert!(v.block_reason.is_some(), "{calls:?} named nothing of the claim, so the claim stays held");
        }
    }

    /// The words the rule itself triggers on name nothing, and a file's folders
    /// are not its name: only the file, the command words and the address count.
    #[test]
    fn the_trigger_words_and_the_folders_of_a_path_tie_nothing() {
        let claim = "Gecontroleerd: de pagina is in orde.";
        let commands = turn_commands(&transcript_with_tool_calls(&[("Bash", r#"{"command":"echo gecontroleerd"}"#)]));
        let v = guard_verdict_in_turn(Some(BACKED_RULEBOOK), claim, "", false, None, &commands);
        assert!(v.block_reason.is_some(), "the word the rule fires on is no tie");
        let claim = "Gecontroleerd: de dev-omgeving draait.";
        let commands = turn_commands(&transcript_with_tool_calls(&[("Read", r#"{"file_path":"C:\\Users\\me\\dev\\proj\\README.md"}"#)]));
        let v = guard_verdict_in_turn(Some(BACKED_RULEBOOK), claim, "", false, None, &commands);
        assert!(v.block_reason.is_some(), "a folder called dev is not the dev environment");
    }

    // ------------------------------------------- collect 17c: a quoted mention is no use

    const QUOTES_RULEBOOK: &str = r#"[
      {"id":"no-still-to-check","tier":"warn","own_claims_only":true,"quotes_exempt":true,
       "any_of":["nog te controleren"],"none_of":[],"reminder":"Check it now."}
    ]"#;

    #[test]
    fn a_quoted_mention_of_the_phrase_in_a_first_person_sentence_is_no_use_of_it() {
        let mention = "Dat was precies het soort ding dat ik eerder zou hebben opgeschreven als \"nog te controleren\".";
        let v = guard_verdict_in_turn(Some(QUOTES_RULEBOOK), mention, "", false, None, &[]);
        assert!(v.warn_reason.is_none() && v.block_reason.is_none(), "{:?}", v.warn_reason);
        let code = "Ik noemde het vroeger `nog te controleren`, maar dat doe ik niet meer.";
        let v = guard_verdict_in_turn(Some(QUOTES_RULEBOOK), code, "", false, None, &[]);
        assert!(v.warn_reason.is_none(), "{:?}", v.warn_reason);
        // The phrase used for real, next to a quote, still fires; so does a rule that does not opt in.
        let used = "Het script is nog te controleren. Eerder schreef ik \"nog te controleren\" op.";
        let v = guard_verdict_in_turn(Some(QUOTES_RULEBOOK), used, "", false, None, &[]);
        assert!(v.warn_reason.is_some());
        let plain = QUOTES_RULEBOOK.replace("\"quotes_exempt\":true,", "");
        let v = guard_verdict_in_turn(Some(&plain), mention, "", false, None, &[]);
        assert!(v.warn_reason.is_some(), "without the field a first-person quote is still the agent's own");
    }

    #[test]
    fn launching_an_agent_or_writing_a_note_does_not_back_a_claim() {
        let claim = "Gecontroleerd: alle drie in orde.";
        let transcript = transcript_with_tool_calls(&[
            ("Agent", r#"{"prompt":"check it"}"#),
            ("mcp__thor__remember", r#"{"id":"x"}"#),
            ("Edit", r#"{"file_path":"a"}"#),
        ]);
        let commands = turn_commands(&transcript);
        assert!(commands.is_empty(), "{commands:?}");
        let v = guard_verdict_in_turn(Some(BACKED_RULEBOOK), claim, "", false, None, &commands);
        assert!(v.block_reason.is_some(), "a claim relayed from an agent, with nothing looked at, stays held");
    }

    #[test]
    fn a_rule_that_does_not_opt_in_is_not_backed_by_a_read() {
        let v = guard_verdict_in_turn(Some(CLAIM_RULEBOOK), "Gecontroleerd: alles werkt.", "", false, None, &["[tool] Read".to_string()]);
        assert!(v.block_reason.is_some());
    }

    #[test]
    fn an_honest_exit_still_passes_without_any_tool() {
        let v = guard_verdict_in_turn(Some(BACKED_RULEBOOK), "Niet gecontroleerd: ik zag het niet.", "", false, None, &[]);
        assert!(v.block_reason.is_none());
    }

    const PROMPT_GATED_RULEBOOK: &str = r#"[
      {"id":"ask-commit","tier":"warn","only_if_prompt_any_of":["commit","push"],
       "any_of":["zal ik committen"],"none_of":[],"reminder":"Do not bounce his request back."}
    ]"#;

    #[test]
    fn a_rule_gated_on_the_prompt_fires_only_when_the_owner_asked_for_that() {
        let reply = "Alles groen. Zal ik committen?";
        let asked = guard_verdict(Some(PROMPT_GATED_RULEBOOK), reply, "Commit maar en push naar dev", false, None);
        assert!(asked.warn_reason.is_some(), "he asked for it: asking back is bouncing it");
        let not_asked = guard_verdict(Some(PROMPT_GATED_RULEBOOK), reply, "Ja", false, None);
        assert!(not_asked.warn_reason.is_none(), "he never asked for a commit: asking first is the standing rule");
        let unreadable = guard_verdict(Some(PROMPT_GATED_RULEBOOK), reply, "", false, None);
        assert!(unreadable.warn_reason.is_some(), "no prompt to read: the rule keeps applying");
    }

    // ------------------------------------- step 33: one short hold when two rules fire

    const TWO_FIRE_RULEBOOK: &str = r#"[
      {"id":"no-tldr","tier":"block","any_of":["gepusht"],"none_of":["tldr"],"min_chars":600,
       "reminder":"LONG-TLDR: open with a TLDR in plain human language, what happened, does it work, what it means, no jargon, detail below it, and keep the whole answer under 1000 characters because the length rule counts all of it.",
       "short_reminder":"SHORT-TLDR: open with a plain-language TLDR."},
      {"id":"too-long","tier":"block","any_of":[],"none_of":["```"],"min_chars":1000,
       "reminder":"LONG-LENGTH: this answer is over 1000 characters, shorten it to the outcome and keep every item his own prompt asked for. {owner_request} Say what happened, then stop.",
       "short_reminder":"SHORT-LENGTH: stay under 1000 characters, keep every item asked for. {owner_request}"}
    ]"#;

    #[test]
    fn two_firing_rules_say_each_requirement_once_in_their_short_form() {
        let msg = format!("gepusht {}", "x".repeat(1200));
        let v = guard_verdict(Some(TWO_FIRE_RULEBOOK), &msg, "Geef kort wat je veranderde.", false, None);
        let reason = v.block_reason.expect("both fire");
        assert!(reason.contains("SHORT-TLDR") && reason.contains("SHORT-LENGTH"), "{reason}");
        assert!(!reason.contains("LONG-"), "{reason}");
        assert!(reason.contains("Geef kort wat je veranderde."), "the quote still rides: {reason}");
        assert!(reason.contains("refused an edit"), "the note about a refused edit is kept on purpose: {reason}");
        assert_eq!(reason.matches("refused an edit").count(), 1, "{reason}");
    }

    #[test]
    fn one_firing_rule_keeps_its_full_reminder() {
        let msg = format!("{}", "x".repeat(1200));
        let v = guard_verdict(Some(TWO_FIRE_RULEBOOK), &msg, "", false, None);
        let reason = v.block_reason.expect("length fires alone");
        assert!(reason.contains("LONG-LENGTH") && !reason.contains("SHORT-"), "{reason}");
        assert!(!reason.contains("refused an edit"), "{reason}");
    }

    // --------------------------------------------------- FALSE BLOCK A: lists
    const LIST_RULEBOOK: &str = r#"[
      {"id":"answer-is-too-long","tier":"block",
       "any_of":[],
       "none_of":["\n> ","```"],
       "min_chars":1000,
       "list_request_any_of":["lijst","overzicht","opsomming","rapport","alle ","welke ","list","overview","report","all the"],
       "reminder":"This answer is over 1000 characters."}
    ]"#;

    /// A long, genuinely list-shaped reply: one intro line, forty bullet
    /// lines. Well past `min_chars`.
    fn long_list_reply() -> String {
        let mut s = String::from("Hier is de lijst met alle stappen:\n");
        for i in 1..=40 {
            s.push_str(&format!("- stap {i}: doe iets nuttigs met dit onderdeel van de taak\n"));
        }
        s
    }

    /// A long reply with NO list shape at all - plain running prose, also
    /// well past `min_chars`, used to prove the exemption is for the LIST,
    /// not merely for "a reply that follows a list-asking prompt".
    fn long_prose_reply() -> String {
        format!(
            "Dit is een lang antwoord zonder enige lijst erin. {}",
            "Nog wat extra uitleg in doorlopende tekst zonder opsomming. ".repeat(20)
        )
    }

    /// THE DEFECT THIS PREVENTS (FALSE BLOCK A): a list the owner explicitly
    /// asked for was blocked purely for its length. Prompt asks for one
    /// ("lijst", "alle"), reply genuinely is one - must pass.
    #[test]
    fn a_long_list_reply_after_a_prompt_that_asked_for_one_is_not_blocked() {
        let v = guard_verdict(Some(LIST_RULEBOOK), &long_list_reply(), "Kun je een lijst geven van alle stappen?", false, None);
        assert!(v.block_reason.is_none(), "{:?}", v.block_reason);
    }

    /// Same long list reply, but the prompt never asked for one - the
    /// exemption must not fire just because the reply happens to be
    /// list-shaped; it needs the owner to have asked.
    #[test]
    fn the_same_long_list_reply_still_blocks_when_the_prompt_never_asked_for_one() {
        let v = guard_verdict(Some(LIST_RULEBOOK), &long_list_reply(), "Hoe los ik dit probleem op?", false, None);
        assert!(v.block_reason.is_some(), "no list was ever requested, so the length rule must still apply");
    }

    /// The prompt DID ask for an overview, but the reply is long prose, not
    /// a list - the exemption is for the list, so this must still block.
    /// This is the fixture that proves the check is genuinely about the
    /// reply's SHAPE, not just about what the prompt said.
    #[test]
    fn a_long_prose_reply_still_blocks_even_when_the_prompt_asked_for_an_overview() {
        let v = guard_verdict(Some(LIST_RULEBOOK), &long_prose_reply(), "Geef je me nog een overzicht van de opties?", false, None);
        assert!(v.block_reason.is_some(), "the reply is prose, not a list, so it must still block");
    }

    // ------------------------------------- an explicit summary or overview
    const OVERVIEW_RULEBOOK: &str = r#"[
      {"id":"answer-is-too-long","tier":"block",
       "any_of":[],
       "none_of":["\n> ","```"],
       "min_chars":1000,
       "overview_request_any_of":["samenvat","summar","overzicht","overview","lijst","list","opsomming"],
       "overview_max_chars":2500,
       "reminder":"This answer is over 1000 characters."}
    ]"#;

    fn prose_of(chars: usize) -> String {
        let mut s = String::from("Dit is de samenvatting in lopende tekst. ");
        while s.chars().count() < chars {
            s.push_str("Nog een zin die uitlegt wat de gebruiker te zien krijgt. ");
        }
        s.chars().take(chars).collect()
    }

    /// THE CASE FOUND 2026-10-09: "Kan je eens samenvatten ..." answered with a
    /// ~1,100 character overview in prose. It was held; it must pass.
    #[test]
    fn an_overview_the_owner_asked_to_have_summarised_may_run_longer() {
        let prompt = "Kan je eens samenvatten wat de nieuwe gebruiker te zien krijgt?";
        let v = guard_verdict(Some(OVERVIEW_RULEBOOK), &prose_of(1100), prompt, false, None);
        assert!(v.block_reason.is_none(), "{:?}", v.block_reason);
        for ask in ["Geef me een overzicht van de opties", "Please summarize the change", "Give me an overview", "Maak een lijst van de stappen", "list the options"] {
            let v = guard_verdict(Some(OVERVIEW_RULEBOOK), &prose_of(1800), ask, false, None);
            assert!(v.block_reason.is_none(), "{ask}: {:?}", v.block_reason);
        }
    }

    /// Longer is not endless: past the allowance the rule applies again.
    #[test]
    fn an_overview_past_the_allowance_is_still_held() {
        let prompt = "Kan je eens samenvatten wat de nieuwe gebruiker te zien krijgt?";
        let v = guard_verdict(Some(OVERVIEW_RULEBOOK), &prose_of(2600), prompt, false, None);
        assert!(v.block_reason.is_some(), "2600 characters is beyond the 2500 allowance");
    }

    /// The same 1,100 characters after a prompt that asked for no summary or
    /// overview, or with no prompt text at all, are held as before.
    #[test]
    fn without_an_explicit_request_the_length_rule_is_unchanged() {
        let v = guard_verdict(Some(OVERVIEW_RULEBOOK), &prose_of(1100), "Hoe los ik dit probleem op?", false, None);
        assert!(v.block_reason.is_some());
        let v = guard_verdict(Some(OVERVIEW_RULEBOOK), &prose_of(1100), "", false, None);
        assert!(v.block_reason.is_some(), "an unreadable prompt never grants the allowance");
    }

    /// A rulebook from before the field existed behaves exactly as it did.
    #[test]
    fn a_rulebook_without_the_overview_fields_is_unchanged() {
        let v = guard_verdict(Some(LIST_RULEBOOK), &prose_of(1100), "Kan je eens samenvatten wat de nieuwe gebruiker te zien krijgt?", false, None);
        assert!(v.block_reason.is_some());
    }

    /// Fail-open direction: an empty last prompt (transcript unreadable) is
    /// treated exactly like "did not ask for a list", never as a match -
    /// same behaviour this rule had before the field existed.
    #[test]
    fn an_empty_last_prompt_never_grants_the_list_exemption() {
        let v = guard_verdict(Some(LIST_RULEBOOK), &long_list_reply(), "", false, None);
        assert!(v.block_reason.is_some());
    }

    #[test]
    fn is_mostly_list_lines_requires_a_genuine_majority_not_mere_presence() {
        assert!(is_mostly_list_lines(&long_list_reply().to_lowercase()));
        assert!(!is_mostly_list_lines(&long_prose_reply().to_lowercase()));
        // One bullet buried in otherwise-prose lines: presence, not shape.
        let mixed = format!("- one lonely bullet\n{}", "just an ordinary prose line here.\n".repeat(10));
        assert!(!is_mostly_list_lines(&mixed));
    }

    // --------------------------------------------------- last_user_prompt

    #[test]
    fn last_user_prompt_reads_a_plain_string_content_turn() {
        let jsonl = r#"{"type":"user","message":{"role":"user","content":"eerste vraag"}}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"antwoord"}]}}
{"type":"user","message":{"role":"user","content":"tweede vraag, de echte"}}"#;
        assert_eq!(last_user_prompt(jsonl).as_deref(), Some("tweede vraag, de echte"));
    }

    /// A tool result is ALSO written as a `"type":"user"` turn by Claude
    /// Code (the Anthropic Messages API shape: a tool result is a "user"
    /// message). That is not the owner speaking, so it must be skipped in
    /// favour of the real prompt further back.
    #[test]
    fn last_user_prompt_skips_a_pure_tool_result_turn() {
        let jsonl = r#"{"type":"user","message":{"role":"user","content":"echte vraag van de eigenaar"}}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","name":"Bash"}]}}
{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"x","content":[{"type":"text","text":"stdout hier"}]}]}}"#;
        assert_eq!(last_user_prompt(jsonl).as_deref(), Some("echte vraag van de eigenaar"));
    }

    /// An array-shaped `content` that DOES carry a plain text block (a real
    /// prompt that also attached an image, say) is read that far.
    #[test]
    fn last_user_prompt_reads_the_text_block_of_an_array_shaped_turn() {
        let jsonl = r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"wat staat er op deze foto?"},{"type":"image","source":{}}]}}"#;
        assert_eq!(last_user_prompt(jsonl).as_deref(), Some("wat staat er op deze foto?"));
    }

    /// FIELD TEST 12, R6: a background task's notification is written as a
    /// user-role entry after the owner's real prompt, and the length hold
    /// quoted its closing tag as "his words". A harness wrapper is skipped,
    /// as a whole entry and as one block beside a block he typed.
    #[test]
    fn last_user_prompt_skips_a_task_notification_and_a_system_reminder_entry() {
        let jsonl = r#"{"type":"user","message":{"role":"user","content":"controleer de build en zeg wat er stuk is"}}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"ik start een taak"}]}}
{"type":"user","message":{"role":"user","content":"<task-notification><task-id>t1</task-id><summary>Build finished</summary></task-notification>"}}
{"type":"user","message":{"role":"user","content":[{"type":"text","text":"<system-reminder>fixture reminder</system-reminder>"}]}}"#;
        assert_eq!(last_user_prompt(jsonl).as_deref(), Some("controleer de build en zeg wat er stuk is"));

        let mixed = r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"<system-reminder>fixture</system-reminder>"},{"type":"text","text":"en nu de tweede vraag"}]}}"#;
        assert_eq!(last_user_prompt(mixed).as_deref(), Some("en nu de tweede vraag"));

        // Only a wrapper in the whole transcript: no owner prompt at all.
        let only = r#"{"type":"user","message":{"role":"user","content":"<task-notification>x</task-notification>"}}"#;
        assert_eq!(last_user_prompt(only), None);
    }

    #[test]
    fn last_user_prompt_skips_malformed_lines_rather_than_giving_up() {
        let jsonl = "{ not json at all\n{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"nog steeds leesbaar\"}}";
        assert_eq!(last_user_prompt(jsonl).as_deref(), Some("nog steeds leesbaar"));
    }

    #[test]
    fn last_user_prompt_is_none_for_an_empty_or_userless_transcript() {
        assert_eq!(last_user_prompt(""), None);
        assert_eq!(
            last_user_prompt(r#"{"type":"assistant","message":{"role":"assistant","content":"hoi"}}"#),
            None
        );
    }

    // ------------------------------------------------------- reader scope
    //
    // `owner_reading_only` (2026-09-09): proves the matcher-level contract
    // this file's own "reader scope" doc comment (above `evaluate_opt_in`)
    // states, independent of the hook wiring - `serve/tests/
    // response_guard_subagent_gate.rs` and `serve/tests/
    // response_guard_warn_stop_hook.rs` prove the same contract end to end
    // through the compiled binary.

    const READER_SCOPE_RULEBOOK: &str = r#"[
      {"id":"owner-reading-rule","owner_reading_only":true,
       "any_of":["te formeel geschreven"],"none_of":["any_of"],
       "reminder":"this rule is about the owner's own reading"},
      {"id":"unmarked-rule",
       "any_of":["ik kon dit niet bereiken"],"none_of":["any_of"],
       "reminder":"this rule carries no owner_reading_only key at all"}
    ]"#;

    /// THE DEFECT THIS PREVENTS: a rule the owner has explicitly marked as
    /// being about his own reading of a reply must not catch a subagent -
    /// the field genuinely doing what it says, not merely existing.
    #[test]
    fn a_rule_marked_owner_reading_only_is_skipped_for_a_subagent() {
        let v = guard_verdict(Some(READER_SCOPE_RULEBOOK), "dit was te formeel geschreven voor mijn smaak", "", true, None);
        assert!(v.block_reason.is_none(), "{:?}", v.block_reason);
    }

    /// Scoped, not a kill switch: the SAME rule and the SAME message still
    /// fire for the owner's own main session (`is_subagent: false`).
    #[test]
    fn the_same_owner_reading_only_rule_still_fires_for_the_main_session() {
        let v = guard_verdict(Some(READER_SCOPE_RULEBOOK), "dit was te formeel geschreven voor mijn smaak", "", false, None);
        let reason = v.block_reason.expect("must still fire for the owner's own reading");
        assert!(reason.contains("own reading"), "{reason}");
    }

    /// THE DEFAULT MATTERS: a rule that declares no `owner_reading_only` key
    /// at all is NOT exempt for a subagent - it keeps applying to everyone,
    /// the fail-safe direction this file's own "reader scope" doc comment
    /// argues for (a gate going quiet is the worst failure class here).
    #[test]
    fn an_unmarked_rule_still_fires_for_a_subagent() {
        let v = guard_verdict(Some(READER_SCOPE_RULEBOOK), "ik kon dit niet bereiken vanaf hier", "", true, None);
        let reason = v.block_reason.expect("an unmarked rule must still catch a subagent");
        assert!(reason.contains("no owner_reading_only key"), "{reason}");
    }

    /// A malformed `owner_reading_only` (a JSON string, not a bool) must
    /// neither crash `parse_opt_in_rules` nor drop the rule - it must be
    /// read as `false`, the SAME value the field's total absence already
    /// produces (this file's own "reader scope" doc comment). Checked at TWO
    /// levels: the parsed struct field directly, and the end-to-end matcher
    /// still catching a subagent on it.
    #[test]
    fn a_malformed_owner_reading_only_value_is_read_as_not_marked() {
        const MALFORMED_RULEBOOK: &str = r#"[
          {"id":"malformed-owner-reading","owner_reading_only":"yes please",
           "any_of":["deze regel heeft een kapotte waarde"],"none_of":["any_of"],
           "reminder":"owner_reading_only is a string here, not a bool"}
        ]"#;

        let parsed = parse_opt_in_rules(MALFORMED_RULEBOOK);
        assert_eq!(parsed.len(), 1, "a malformed owner_reading_only must not drop the whole rule");
        assert!(!parsed[0].owner_reading_only, "a non-bool value must read as false, not panic or default true");

        let v = guard_verdict(Some(MALFORMED_RULEBOOK), "deze regel heeft een kapotte waarde erin", "", true, None);
        assert!(v.block_reason.is_some(), "a malformed value must be treated as not-marked, so it still catches a subagent");
    }

    // ------------------------------------------------- negated claims (step 27)

    const VERSION_RULE: &str = r#"[{"id":"no-version-number-on-ship","tier":"block","affirmative_only":true,
        "any_of":["gepusht","gedeployed","pushed","deployed"],"none_of":["versie","version"],
        "reminder":"name the version"}]"#;

    const VERSION_RULE_OLD: &str = r#"[{"id":"no-version-number-on-ship","tier":"block",
        "any_of":["gepusht","gedeployed","pushed","deployed"],"none_of":["versie","version"],
        "reminder":"name the version"}]"#;

    fn blocks(rulebook: &str, msg: &str) -> bool {
        guard_verdict(Some(rulebook), msg, "", false, None).block_reason.is_some()
    }

    /// THE FIELD CASE: "niets gecommit, gepusht of gedeployd" is the opposite of
    /// a claim that something was pushed.
    #[test]
    fn a_negated_ship_claim_does_not_trip_the_version_rule() {
        for msg in [
            "Klaar. Niets gecommit, gepusht of gedeployd.",
            "Nothing was pushed or deployed.",
            "Het is niet gepusht naar dev.",
            "Zonder iets te deployed te hebben.",
        ] {
            assert!(!blocks(VERSION_RULE, msg), "{msg}");
        }
    }

    #[test]
    fn an_affirmative_ship_claim_still_trips_it() {
        for msg in [
            "Alles is gepusht naar dev.",
            "Het is gepusht, niet gedeployed.",
            "No problem, pushed to dev.",
            "We hebben niet gewacht. Alles is gedeployed.",
        ] {
            assert!(blocks(VERSION_RULE, msg), "{msg}");
        }
    }

    #[test]
    fn a_rule_without_the_field_reads_every_occurrence_as_before() {
        assert!(blocks(VERSION_RULE_OLD, "Klaar. Niets gecommit, gepusht of gedeployd."));
    }

    #[test]
    fn a_negation_further_back_than_its_reach_leaves_the_claim_a_claim() {
        let msg = "Niet alles ging goed, maar daarna is het uiteindelijk toch allemaal netjes gepusht.";
        assert!(blocks(VERSION_RULE, msg), "{msg}");
    }
}
