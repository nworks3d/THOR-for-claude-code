//! What the owner said: the part of an evaluation that reads the messages the
//! owner typed since the project's previous evaluation, so the evaluating
//! assistant can store the wishes among them that should still hold later.
//!
//! WHY. A beginner never says "this is a rule". They just talk: "always ask me
//! before you delete anything", "keep the answers short". The assistant has to
//! find such a wish itself, in whatever language it is written, and THOR has no
//! word list to find it with. So THOR hands the evaluation the owner's own
//! messages and holds the report until the assistant has printed them.
//!
//! WHAT THIS MODULE DOES, in the order the evaluation uses it:
//! 1. `freeze` - at the evaluation start, collects the owner-typed prompts of
//!    the project's main sessions in a window (after the project's cursor, or
//!    the last 7 days when it has none) and keeps only the window and the
//!    counts (`model::store::SaidFrozen`). The words themselves are never
//!    written to the log.
//! 2. `render` and `plan` - what `serve said --project <name>` prints: the same
//!    window read again from the transcripts, oldest first, one line each with
//!    its date, capped at `PRINT_CAP_CHARS`; the last line says how many newer
//!    messages wait for the next evaluation.
//! 3. `record_print_evidence` - when the filing is made, THOR reads this
//!    session's own transcript and, if the print command ran after the start,
//!    writes `model::store::SAID_PRINTED_KIND` on the log. The filing gate and
//!    the machine section read that record, never a live transcript.
//! 4. `advance_cursor` - only when a report that counts is filed, the cursor
//!    moves to the last message that was printed, so nothing is dropped.
//!
//! WHAT THIS CANNOT PROVE. THOR can show that the list existed and was printed.
//! It cannot tell whether the assistant read it well or stored the right
//! wishes: that judgement is the assistant's own and is only written down in
//! its own text.
//!
//! A transcript is somebody else's file: one that cannot be read is skipped
//! and counted, never an error.

use model::store::{EvaluationStart, SaidFrozen};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use thor_core::event_store::EventStore;

/// A message shorter than this many characters (after white space is folded
/// to single spaces) is left out: "yes", "go", "ok".
pub const MIN_CHARS: usize = 20;

/// The window of a project that has no cursor yet, in days.
pub const FIRST_WINDOW_DAYS: i64 = 7;

/// `serve said` prints at most about this many characters.
pub const PRINT_CAP_CHARS: usize = 20_000;

/// One message is cut at this many characters.
pub const MESSAGE_CLIP_CHARS: usize = 2_000;

/// Room kept below the cap for the heading and the last line.
const PRINT_RESERVE_CHARS: usize = 400;

/// One message the owner typed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Said {
    pub at_ms: i64,
    pub text: String,
}

/// What `collect_in` found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Collected {
    /// Oldest first; short and repeated messages left out; secrets blanked.
    pub list: Vec<Said>,
    /// Transcript files in the window that could not be read.
    pub unreadable: usize,
}

// ---------------------------------------------------------------- time

/// The time of a transcript timestamp in milliseconds (`2026-03-02T10:00:30.123Z`).
pub fn ms_of_timestamp(at: &str) -> Option<i64> {
    let seconds = crate::eval_filing::unix_of_timestamp(at)?;
    let fraction = at.split_once('.').map(|(_, rest)| rest.chars().take_while(|c| c.is_ascii_digit()).take(3).collect::<String>()).unwrap_or_default();
    let ms = format!("{fraction:0<3}").parse::<i64>().unwrap_or(0);
    Some(seconds * 1000 + ms)
}

pub fn date_of_ms(ms: i64) -> String {
    crate::time::iso8601_from_unix(ms / 1000).chars().take(16).collect::<String>().replace('T', " ")
}

// ------------------------------------------------------------- reading

/// The owner's own text on one transcript line, or `None` when the line is not
/// a message he typed. Built on `attest::owner_text` (a user line whose origin
/// is human, not meta, not a sidechain, not a tool result, not a hook, task
/// notification, cross-session message or compaction summary, quoted and
/// fenced lines taken out) and drops what is left of system text: a reminder or
/// a command wrapper starts with a tag, a compaction summary with its own words.
fn owner_prompt(line: &str) -> Option<String> {
    let text = crate::attest::owner_text(line)?;
    let start = text.trim_start();
    if start.starts_with('<') || start.starts_with("This session is being continued") || start.starts_with("Caveat:") {
        return None;
    }
    Some(text)
}

fn tidy(text: &str) -> String {
    let one_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let blanked = crate::inventory::redact(&one_line);
    if blanked.chars().count() <= MESSAGE_CLIP_CHARS {
        return blanked;
    }
    let cut: String = blanked.chars().take(MESSAGE_CLIP_CHARS).collect();
    format!("{cut}...")
}

/// The first working folder a transcript names, read from its first lines only
/// (a transcript can be large, and most belong to other projects).
fn cwd_of(file: &Path) -> std::io::Result<Option<String>> {
    use std::io::BufRead;
    let reader = std::io::BufReader::new(std::fs::File::open(file)?);
    for line in reader.split(b'\n').take(200) {
        let line = line?;
        if let Some(cwd) = serde_json::from_slice::<serde_json::Value>(&line).ok().and_then(|v| v.get("cwd").and_then(|c| c.as_str()).map(str::to_string)) {
            return Ok(Some(cwd));
        }
    }
    Ok(None)
}

/// The main transcripts under `root`: `<root>/<folder>/<session>.jsonl`, never
/// the helpers' files in the folders below them.
fn main_transcripts(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(folders) = std::fs::read_dir(root) else { return out };
    for folder in folders.flatten().map(|e| e.path()).filter(|p| p.is_dir()) {
        let Ok(files) = std::fs::read_dir(&folder) else { continue };
        out.extend(files.flatten().map(|e| e.path()).filter(|p| p.is_file() && p.extension().is_some_and(|e| e == "jsonl")));
    }
    out.sort();
    out
}

/// Every message the owner typed in the main sessions of `project` after
/// `since_ms` and at or before `until_ms`: 20 characters or longer, identical
/// texts once (the oldest), secrets blanked, oldest first. A session belongs to
/// the project its first working folder resolves to.
pub fn collect_in(root: &Path, project: &str, since_ms: i64, until_ms: i64) -> Collected {
    let mut found: Vec<Said> = Vec::new();
    let mut unreadable = 0;
    let mut belongs: HashMap<String, bool> = HashMap::new();
    for file in main_transcripts(root) {
        // A file nobody wrote to since the window opened holds nothing in it.
        let modified = std::fs::metadata(&file)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64);
        if modified.is_some_and(|m| m < since_ms) {
            continue;
        }
        let cwd = match cwd_of(&file) {
            Ok(Some(cwd)) => cwd,
            Ok(None) => continue,
            Err(_) => {
                unreadable += 1;
                continue;
            }
        };
        let ours = *belongs.entry(cwd.clone()).or_insert_with(|| {
            crate::project::resolve_project(Path::new(&cwd)).is_some_and(|p| crate::project::same_project_name(&p, project))
        });
        if !ours {
            continue;
        }
        let Ok(bytes) = std::fs::read(&file) else {
            unreadable += 1;
            continue;
        };
        let content = String::from_utf8_lossy(&bytes);
        for line in content.lines() {
            let Some(text) = owner_prompt(line) else { continue };
            let Some(at_ms) = serde_json::from_str::<serde_json::Value>(line)
                .ok()
                .and_then(|v| v.get("timestamp").and_then(|t| t.as_str()).and_then(ms_of_timestamp))
            else {
                continue;
            };
            if at_ms <= since_ms || at_ms > until_ms {
                continue;
            }
            let said = tidy(&text);
            if said.chars().count() >= MIN_CHARS {
                found.push(Said { at_ms, text: said });
            }
        }
    }
    found.sort_by(|a, b| a.at_ms.cmp(&b.at_ms).then_with(|| a.text.cmp(&b.text)));
    let mut seen = std::collections::HashSet::new();
    found.retain(|s| seen.insert(s.text.clone()));
    Collected { list: found, unreadable }
}

// ------------------------------------------------------------ printing

fn render_line(said: &Said) -> String {
    format!("[{}] {}", date_of_ms(said.at_ms), said.text)
}

/// How many of `list` the print shows (oldest first, at least one when there
/// is one) and the time of the last of them in milliseconds (0 for none).
pub fn plan(list: &[Said]) -> (usize, i64) {
    let mut used = 0;
    let mut shown = 0;
    for said in list {
        used += render_line(said).chars().count() + 1;
        if shown > 0 && used > PRINT_CAP_CHARS - PRINT_RESERVE_CHARS {
            break;
        }
        shown += 1;
    }
    (shown, list.get(shown.wrapping_sub(1)).map_or(0, |s| s.at_ms))
}

/// The text `serve said` prints for `list`: a heading, the messages shown,
/// oldest first, one line each with its date, and - when some do not fit - a
/// last line with how many newer messages wait for the next evaluation.
pub fn render(project: &str, list: &[Said]) -> String {
    let (shown, _) = plan(list);
    let mut out = format!(
        "What the owner typed in '{project}' since the previous evaluation, oldest first ({} message(s) on the list, {shown} printed). \
         Read them all. For every message that says how the owner wants something done in a way that should still hold in a \
         later conversation, look it up first and then store it or correct the note that holds it.\n",
        list.len()
    );
    for said in &list[..shown] {
        out.push_str(&render_line(said));
        out.push('\n');
    }
    if shown < list.len() {
        out.push_str(&format!("{} newer message(s) wait for the next evaluation.\n", list.len() - shown));
    }
    out
}

// -------------------------------------------------------------- cursor

/// The small state file beside the store that holds each project's cursor.
pub fn cursor_path(db: &Path) -> PathBuf {
    db.parent().unwrap_or_else(|| Path::new(".")).join("evaluation-said-cursors.json")
}

fn cursor_key(project: &str) -> String {
    crate::project::project_match_key(project)
}

fn read_cursors(db: &Path) -> BTreeMap<String, i64> {
    std::fs::read_to_string(cursor_path(db)).ok().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default()
}

/// The project's cursor in milliseconds, when it has one.
pub fn cursor_of(db: &Path, project: &str) -> Option<i64> {
    read_cursors(db).get(&cursor_key(project)).copied()
}

/// Move the project's cursor forward to `through_ms` (never back).
pub fn advance_cursor(db: &Path, project: &str, through_ms: i64) {
    if through_ms <= 0 {
        return;
    }
    let mut cursors = read_cursors(db);
    let entry = cursors.entry(cursor_key(project)).or_insert(0);
    if *entry >= through_ms {
        return;
    }
    *entry = through_ms;
    let path = cursor_path(db);
    let temp = path.with_extension("json.tmp");
    if let Ok(text) = serde_json::to_string_pretty(&cursors) {
        if std::fs::write(&temp, text).is_ok() {
            let _ = std::fs::rename(&temp, &path);
        }
    }
}

/// Move the cursor of the evaluation that `record` (a validation that counts)
/// is about, to the last message its list printed.
pub fn advance_for_valid(store: &EventStore, record: &model::store::EvaluationValidation) {
    let (Some(project), Some(started)) = (record.project.as_deref(), record.started_seq) else { return };
    let Some(db) = store.path() else { return };
    let start = model::store::evaluation_starts_of_session(store, &record.session)
        .into_iter()
        .filter(|s| s.frozen.project == project && s.frozen.started_seq == started)
        .max_by_key(|s| s.seq);
    if let Some(said) = start.and_then(|s| s.frozen.said) {
        advance_cursor(&db, project, said.through_ms);
    }
}

// -------------------------------------------------------------- freezing

/// What the evaluation of `project` starting at `now` (unix seconds) freezes:
/// the window and the counts. `None` when the list is empty and every
/// transcript could be read: such an evaluation asks nothing about it.
pub fn freeze(db: &Path, root: Option<&Path>, project: &str, now: i64) -> Option<SaidFrozen> {
    let until_ms = now * 1000;
    let since_ms = cursor_of(db, project).unwrap_or(until_ms - FIRST_WINDOW_DAYS * 86_400_000);
    let collected = match root {
        Some(root) => collect_in(root, project, since_ms, until_ms),
        None => Collected::default(),
    };
    if collected.list.is_empty() && collected.unreadable == 0 {
        return None;
    }
    let (shown, through_ms) = plan(&collected.list);
    Some(SaidFrozen {
        since_ms,
        until_ms,
        total: collected.list.len(),
        shown,
        through_ms,
        unreadable: collected.unreadable,
        command: model::store::said_command_for(Some(db), project),
    })
}

/// What `serve said --project <project>` prints: the list of the newest open
/// evaluation of `project` that froze one, read again inside the frozen window.
pub fn print_for(store: &EventStore, root: Option<&Path>, project: &str) -> Result<String, String> {
    let now = crate::time::now_unix();
    let start = model::store::open_evaluation_starts_of_project(store, project, now)
        .into_iter()
        .filter(|s| s.frozen.said.is_some())
        .max_by_key(|s| s.seq);
    let Some(start) = start else {
        return Err(format!(
            "no open evaluation of '{project}' holds a list of what the owner said: either none is open, or nothing was typed \
             since the previous one"
        ));
    };
    let said = start.frozen.said.as_ref().expect("filtered above");
    let Some(root) = root else { return Err("could not find the folder with the session transcripts".to_string()) };
    let collected = collect_in(root, project, said.since_ms, said.until_ms);
    Ok(render(project, &collected.list))
}

// ------------------------------------------------------------- evidence

/// Splits a shell command into words the way Bash and PowerShell do for the
/// forms THOR prints: white space separates words except inside single or
/// double quotes, the quotes themselves are dropped (`--project='a b'` is one
/// word `--project=a b`), and a backslash before a space or, outside single
/// quotes, before a quote keeps that character (`a\ b`). Any other backslash is
/// literal, so Windows paths such as `C:\thor2\serve.exe` stay whole. PowerShell
/// (`powershell`) has no backslash escape at all: there a backslash is always literal.
fn shell_words(command: &str, powershell: bool) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut started = false;
    let mut quote: Option<char> = None;
    let mut chars = command.chars().peekable();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), _) if c == q => quote = None,
            (Some('\''), _) => word.push(c),
            (Some(_), '\\') if !powershell && matches!(chars.peek(), Some('"')) => word.push(chars.next().unwrap()),
            (Some(_), _) => word.push(c),
            (None, '"' | '\'') => {
                quote = Some(c);
                started = true;
            }
            (None, '\\') if !powershell && matches!(chars.peek(), Some(' ' | '"' | '\'')) => {
                word.push(chars.next().unwrap());
                started = true;
            }
            (None, _) if c.is_whitespace() => {
                if started || !word.is_empty() {
                    words.push(std::mem::take(&mut word));
                }
                started = false;
            }
            (None, _) => word.push(c),
        }
    }
    if started || !word.is_empty() {
        words.push(word);
    }
    words
}

/// Whether a shell command runs THOR's `serve` with the `said` subcommand and
/// `--project <project>` (the program is `serve` or `serve.exe`, bare or at the
/// end of a path; the project may be quoted).
pub fn runs_said(command: &str, project: &str) -> bool {
    runs_said_in(command, project, false)
}

/// `runs_said` for a command of the PowerShell tool (`powershell`) or of Bash.
pub fn runs_said_in(command: &str, project: &str, powershell: bool) -> bool {
    let clean = |t: String| t.trim_matches(|c| c == '&' || c == ';' || c == '|' || c == '(' || c == ')').to_string();
    let tokens: Vec<String> = shell_words(command, powershell).into_iter().map(clean).filter(|t| !t.is_empty()).collect();
    let is_serve = |t: &str| {
        let base = t.rsplit(['/', '\\']).next().unwrap_or("").to_lowercase();
        base == "serve" || base == "serve.exe"
    };
    let Some(at) = tokens.iter().position(|t| is_serve(t)) else { return false };
    let rest = &tokens[at + 1..];
    if !rest.iter().any(|t| t == "said") {
        return false;
    }
    let named = rest.iter().enumerate().find_map(|(i, t)| match t.strip_prefix("--project=") {
        Some(value) => Some(value.to_string()),
        None if t == "--project" => rest.get(i + 1).cloned(),
        None => None,
    });
    named.is_some_and(|name| crate::project::same_project_name(&name, project))
}

/// Whether `transcript` shows a shell call, made after `started_unix`, that
/// runs the print command for `project`. Lines of helpers never count.
pub fn printed_in_transcript(transcript: &Path, started_unix: i64, project: &str) -> bool {
    let Ok(bytes) = std::fs::read(transcript) else { return false };
    String::from_utf8_lossy(&bytes).lines().any(|raw| {
        let Ok(line) = serde_json::from_str::<serde_json::Value>(raw) else { return false };
        if line.get("type").and_then(|t| t.as_str()) != Some("assistant") || line.get("isSidechain").and_then(|b| b.as_bool()) == Some(true) {
            return false;
        }
        let at = line.get("timestamp").and_then(|t| t.as_str()).and_then(crate::eval_filing::unix_of_timestamp).unwrap_or(0);
        if at < started_unix {
            return false;
        }
        let items = line.get("message").and_then(|m| m.get("content")).and_then(|c| c.as_array());
        items.into_iter().flatten().any(|item| {
            let tool = item.get("name").and_then(|n| n.as_str());
            let shell = item.get("type").and_then(|t| t.as_str()) == Some("tool_use") && matches!(tool, Some("Bash") | Some("PowerShell"));
            shell && runs_said_in(item.get("input").and_then(|i| i.get("command")).and_then(|c| c.as_str()).unwrap_or(""), project, tool == Some("PowerShell"))
        })
    })
}

/// When `start` froze a list and the session's own transcript shows the print
/// command run after the start, write that down on the log (once). Returns
/// whether the evidence is on the log afterwards.
pub fn record_print_evidence(store: &mut EventStore, root: Option<&Path>, start: &EvaluationStart) -> bool {
    if start.frozen.said.as_ref().is_none_or(|s| s.total == 0) {
        return true;
    }
    if model::store::said_printed(store, start) {
        return true;
    }
    let Some(file) = crate::attest::main_transcript_of(None, root, &start.frozen.session) else { return false };
    if !printed_in_transcript(&file, start.frozen.started_unix, &start.frozen.project) {
        return false;
    }
    model::store::record_said_printed(store, start).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(at: &str, text: &str, extra: &str) -> String {
        format!(
            r#"{{"type":"user","timestamp":"{at}","cwd":"CWD","message":{{"role":"user","content":{}}}{extra}}}"#,
            serde_json::to_string(text).unwrap()
        )
    }

    /// A transcripts folder holding one main session of `cwd` with `lines`.
    fn session(root: &Path, folder: &str, id: &str, cwd: &Path, lines: &[String]) {
        let dir = root.join(folder);
        std::fs::create_dir_all(&dir).unwrap();
        let body = lines.iter().map(|l| l.replace("CWD", &cwd.to_string_lossy().replace('\\', "\\\\"))).collect::<Vec<_>>().join("\n");
        std::fs::write(dir.join(format!("{id}.jsonl")), body).unwrap();
    }

    fn project_folder(base: &Path, name: &str) -> PathBuf {
        let dir = base.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(crate::project::MARKER_FILE_NAME), format!("{name}\n")).unwrap();
        dir
    }

    const T0: i64 = 1_790_000_000_000;

    fn at(offset_s: i64) -> String {
        crate::time::iso8601_from_unix(T0 / 1000 + offset_s)
    }

    #[test]
    fn the_list_holds_only_owner_typed_prompts_of_the_right_project_and_window() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("projects");
        let mine = project_folder(dir.path(), "demo");
        let other = project_folder(dir.path(), "elsewhere");
        let sub = mine.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        session(
            &root,
            "a",
            "s1",
            &mine,
            &[
                user(&at(10), "please always answer me in short sentences", ""),
                // Not typed by the owner: a tool result, a helper, a meta line, text from another origin, a reminder.
                r#"{"type":"user","timestamp":"2026-03-02T10:00:00Z","cwd":"CWD","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"x","content":"a result that is long enough to count"}]}}"#.to_string(),
                user(&at(11), "a helper typed this long enough line", r#","isSidechain":true"#),
                user(&at(12), "a meta line that is long enough to count", r#","isMeta":true"#),
                user(&at(13), "a task notification that is long enough", r#","origin":{"kind":"task-notification"}"#),
                user(&at(14), "<system-reminder>a reminder that is long enough</system-reminder>", ""),
                user(&at(15), "This session is being continued from a previous conversation that ran out", ""),
                // Outside the window: before it and after it.
                user(&at(-5000), "an old message that is long enough to count", ""),
                user(&at(5000), "a later message that is long enough to count", ""),
            ],
        );
        // A session typed in a subfolder of the project counts too.
        session(&root, "b", "s2", &sub, &[user(&at(20), "keep every commit message plain and short", "")]);
        // Another project does not.
        session(&root, "c", "s3", &other, &[user(&at(30), "this belongs to some other project entirely", "")]);
        // A helper's file below the main session does not.
        let helper = root.join("a").join("s1");
        std::fs::create_dir_all(&helper).unwrap();
        std::fs::write(helper.join("agent-1.jsonl"), user(&at(40), "a helper file that is long enough to count", "").replace("CWD", &mine.to_string_lossy().replace('\\', "\\\\"))).unwrap();

        let got = collect_in(&root, "demo", T0, T0 + 1_000_000);
        let texts: Vec<&str> = got.list.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(texts, vec!["please always answer me in short sentences", "keep every commit message plain and short"]);
        assert_eq!(got.unreadable, 0);
    }

    #[test]
    fn short_and_repeated_messages_are_left_out_and_a_secret_is_blanked() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("projects");
        let mine = project_folder(dir.path(), "demo");
        let token = format!("{}{}", "ghp_", "a".repeat(30));
        session(
            &root,
            "a",
            "s1",
            &mine,
            &[
                user(&at(1), "yes", ""),
                user(&at(2), "go ahead and do it", ""),
                user(&at(3), "never push to the main branch without asking", ""),
                user(&at(4), "never   push to the main\nbranch without asking", ""),
                user(&at(5), &format!("my token is {token} so use it for the api"), ""),
            ],
        );
        let got = collect_in(&root, "demo", T0, T0 + 1_000_000);
        let texts: Vec<&str> = got.list.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(texts.len(), 2, "{texts:?}");
        assert_eq!(texts[0], "never push to the main branch without asking");
        assert!(texts[1].contains("[secret removed]") && !texts[1].contains(&token), "{texts:?}");
        assert_eq!(got.list[0].at_ms, T0 + 3000, "the repeat keeps the oldest");
    }

    #[test]
    fn the_print_is_capped_and_says_how_many_wait() {
        let list: Vec<Said> = (0..40).map(|n| Said { at_ms: T0 + n * 1000, text: format!("{n:02} {}", "word ".repeat(150)) }).collect();
        let (shown, through) = plan(&list);
        assert!(shown > 0 && shown < list.len(), "{shown}");
        assert_eq!(through, list[shown - 1].at_ms, "the cursor goes to the last message shown");
        let printed = render("demo", &list);
        assert!(printed.chars().count() <= PRINT_CAP_CHARS, "{}", printed.chars().count());
        assert!(printed.contains("00 word"), "the oldest part is printed");
        assert!(!printed.contains(&format!("\n{:02} word", 39)), "the newest part is not");
        assert!(printed.trim_end().ends_with(&format!("{} newer message(s) wait for the next evaluation.", list.len() - shown)), "{printed}");
        // A short list prints whole, with no waiting line.
        let few = &list[..3];
        assert_eq!(plan(few).0, 3);
        assert!(!render("demo", few).contains("wait for the next"));
        // One message alone is always shown.
        assert_eq!(plan(&list[..1]).0, 1);
    }

    #[test]
    fn the_cursor_moves_forward_only_and_per_project() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("thor.db");
        assert_eq!(cursor_of(&db, "Demo App"), None);
        advance_cursor(&db, "Demo App", 5_000);
        advance_cursor(&db, "Demo App", 3_000);
        assert_eq!(cursor_of(&db, "demo-app"), Some(5_000), "spelling does not make a second project");
        assert_eq!(cursor_of(&db, "other"), None);
        advance_cursor(&db, "Demo App", 0);
        assert_eq!(cursor_of(&db, "Demo App"), Some(5_000));
    }

    #[test]
    fn only_the_print_command_of_the_right_project_counts() {
        assert!(runs_said("serve --db x said --project demo", "demo"));
        assert!(runs_said("\"C:/thor2/serve.exe\" --db \"C:/x y/thor.db\" said --project 'Demo'", "demo"));
        assert!(runs_said("cd x && ~/.thor2/serve --db y said --project=demo | head", "demo"));
        assert!(!runs_said("serve --db x said --project other", "demo"));
        assert!(!runs_said("serve --db x said", "demo"));
        assert!(!runs_said("serve --db x status --project demo", "demo"));
        assert!(!runs_said("echo said --project demo", "demo"));
    }

    #[test]
    fn a_project_name_with_a_space_passes_in_every_quoting_form() {
        for command in [
            "serve --db x said --project 'Trade bot'",
            "serve --db x said --project \"Trade bot\"",
            "serve --db x said --project=\"Trade bot\"",
            "serve --db x said --project='Trade bot'",
            "serve --db x said --project Trade\\ bot",
            "& \"C:\\thor2\\serve.exe\" --db \"C:\\my store\\thor.db\" said --project 'Trade bot'",
            "cd x && serve --db x said --project 'Trade bot' | head",
            "serve --db x said --project trade-bot",
        ] {
            assert!(runs_said(command, "Trade bot"), "{command}");
        }
        assert!(!runs_said("serve --db x said --project 'Trade'", "Trade bot"));
        assert!(!runs_said("serve --db x said --project Trade bot", "Trade bot"), "an unquoted space ends the name");
        assert!(!runs_said("serve --db x said --project 'Other bot'", "Trade bot"));
        assert!(!runs_said("serve --db x said --project \"Trade bot", "Trade bot x"));
    }

    #[test]
    fn a_backslash_before_a_quote_is_literal_in_powershell_and_an_escape_in_bash() {
        let ps = "Set-Location \"C:\\x\\\"; & C:/Users/a/thor2/serve.exe --db y said --project 'Trade bot'";
        assert!(runs_said_in(ps, "Trade bot", true), "PowerShell closes the word at the quote");
        assert!(!runs_said_in(ps, "Trade bot", false), "Bash reads the backslash as an escape");
        assert!(runs_said_in("serve --db \"a \\\"b\\\" c\" said --project demo", "demo", false));
        // Through the transcript reader: the tool name decides.
        let dir = tempfile::tempdir().unwrap();
        let line = |tool: &str| {
            serde_json::json!({ "type": "assistant", "timestamp": "2026-11-20T10:00:00Z",
                "message": { "content": [{ "type": "tool_use", "name": tool, "input": { "command": ps } }] } })
            .to_string()
        };
        std::fs::write(dir.path().join("p.jsonl"), line("PowerShell")).unwrap();
        std::fs::write(dir.path().join("b.jsonl"), line("Bash")).unwrap();
        assert!(printed_in_transcript(&dir.path().join("p.jsonl"), 0, "Trade bot"));
        assert!(!printed_in_transcript(&dir.path().join("b.jsonl"), 0, "Trade bot"));
    }

    #[test]
    fn the_start_notes_own_printed_command_passes_for_a_spaced_name() {
        let db = std::path::Path::new("C:/my store/thor.db");
        for program in [std::path::Path::new("C:/thor2/serve.exe"), std::path::Path::new("serve"), std::path::Path::new("C:/Program Files/thor/serve.exe")] {
            let printed = model::store::said_command_with(program, Some(db), "Trade bot");
            assert!(runs_said(&printed, "Trade bot"), "{printed}");
            // PowerShell form of the same command: call operator in front.
            assert!(runs_said(&format!("& {printed}"), "Trade bot"), "& {printed}");
            assert!(!runs_said(&printed, "Other bot"), "{printed}");
        }
    }
}
