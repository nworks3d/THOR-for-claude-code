//! `serve inventory`: the read-only look at earlier Claude Code sessions.
//!
//! Every transcript here is invented: a person called Ann, her CV and a
//! kitchen renovation. Nothing is read from a real home folder; the program
//! runs with `--home` and `--projects-dir` pointed at a temp folder.

use std::path::{Path, PathBuf};
use std::process::Command;

struct Fixture {
    root: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let f = Self { root: tempfile::tempdir().unwrap() };
        std::fs::create_dir_all(f.home().join("Desktop")).unwrap();
        std::fs::create_dir_all(f.home().join("Documents")).unwrap();
        std::fs::create_dir_all(f.projects()).unwrap();
        f
    }
    fn home(&self) -> PathBuf {
        self.root.path().join("ann")
    }
    fn projects(&self) -> PathBuf {
        self.root.path().join("claude-projects")
    }
    /// Write one transcript for a session that ran in `cwd`.
    fn session(&self, id: &str, cwd: &Path, day: &str, lines: &[String]) {
        let dir = self.projects().join("encoded-folder");
        std::fs::create_dir_all(&dir).unwrap();
        let mut body = String::new();
        for l in lines {
            body.push_str(&l.replace("@CWD@", &cwd.display().to_string().replace('\\', "\\\\")).replace("@DAY@", day));
            body.push('\n');
        }
        std::fs::write(dir.join(format!("{id}.jsonl")), body).unwrap();
    }
}

fn user(text: &str) -> String {
    format!(r#"{{"type":"user","cwd":"@CWD@","timestamp":"@DAY@T10:00:00Z","sessionId":"s","message":{{"role":"user","content":{}}}}}"#, serde_json::to_string(text).unwrap())
}

fn assistant_text(text: &str) -> String {
    format!(
        r#"{{"type":"assistant","cwd":"@CWD@","timestamp":"@DAY@T10:01:00Z","message":{{"role":"assistant","content":[{{"type":"text","text":{}}}]}}}}"#,
        serde_json::to_string(text).unwrap()
    )
}

fn tool(name: &str, key: &str, value: &Path) -> String {
    format!(
        r#"{{"type":"assistant","cwd":"@CWD@","timestamp":"@DAY@T10:02:00Z","message":{{"role":"assistant","content":[{{"type":"tool_use","name":"{name}","input":{{"{key}":{}}}}}]}}}}"#,
        serde_json::to_string(&value.display().to_string()).unwrap()
    )
}

fn tool_result() -> String {
    r#"{"type":"user","cwd":"@CWD@","timestamp":"@DAY@T10:03:00Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"x","content":"ok"}]}}"#.to_string()
}

fn serve(args: &[&str]) -> (String, bool) {
    let db = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_serve")).arg("--db").arg(db.path().join("thor.db")).args(args).output().unwrap();
    (String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr), out.status.success())
}

fn inventory_args<'a>(f: &'a Fixture, extra: &[&'a str], buf: &'a mut Vec<String>) -> Vec<&'a str> {
    buf.push(f.projects().display().to_string());
    buf.push(f.home().display().to_string());
    let mut v = vec!["inventory", "--projects-dir", buf[0].as_str(), "--home", buf[1].as_str()];
    v.extend_from_slice(extra);
    v
}

/// A fixture with a CV session on the Desktop, a repository session, and a
/// session with only tool results (which has no prompt of the person).
fn populated() -> Fixture {
    let f = Fixture::new();
    let desktop = f.home().join("Desktop");
    let cv = f.home().join("Documents").join("cv.docx");
    std::fs::write(&cv, "x").unwrap();
    let gone = desktop.join("letter-draft.txt");
    f.session(
        "aaaa1111",
        &desktop,
        "2026-03-02",
        &[
            user("Help me rewrite my CV for a job at a bakery"),
            assistant_text("Sure, here is a first version."),
            tool("Write", "file_path", &cv),
            tool("Edit", "file_path", &gone),
            tool("Write", "file_path", &cv),
            tool_result(),
            user("Now make it shorter"),
            assistant_text("Done, it is one page now."),
        ],
    );
    let repo = f.home().join("dev").join("site");
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    f.session("bbbb2222", &repo, "2026-03-05", &[user("Fix the header"), assistant_text("Fixed.")]);
    f.session("cccc3333", &desktop, "2026-03-06", &[tool_result()]);
    f
}

#[test]
fn lists_unordered_folders_first_with_files_and_loose_files() {
    let f = populated();
    let mut buf = Vec::new();
    let args = inventory_args(&f, &[], &mut buf);
    let (text, ok) = serve(&args);
    assert!(ok, "{text}");
    assert!(text.contains("Sessions with something typed by the person: 2"), "{text}");
    let desktop_at = text.find("NOT in order: Desktop").expect(&text);
    let repo_at = text.find("in order (a git repository").expect(&text);
    assert!(desktop_at < repo_at, "folders not in order come first: {text}");
    assert!(text.contains("2026-03-02 .. 2026-03-02  2 prompt(s)"), "{text}");
    assert!(text.contains("first prompt: Help me rewrite my CV for a job at a bakery"), "{text}");
    assert!(text.contains("exists") && text.contains("cv.docx"), "{text}");
    assert!(text.contains("gone") && text.contains("letter-draft.txt"), "{text}");
    assert_eq!(text.matches("cv.docx").count(), 2, "listed once under the session, once as a loose file: {text}");
    assert!(text.contains("LOOSE FILES"), "{text}");
}

#[test]
fn json_form_has_the_same_facts() {
    let f = populated();
    let mut buf = Vec::new();
    let args = inventory_args(&f, &["--json"], &mut buf);
    let (text, ok) = serve(&args);
    assert!(ok, "{text}");
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["sessions_found"], 2);
    assert_eq!(v["folders"][0]["ordered"], false);
    assert_eq!(v["folders"][1]["ordered"], true);
    assert_eq!(v["folders"][0]["sessions"][0]["prompts"], 2);
    assert_eq!(v["loose_files"].as_array().unwrap().len(), 2);
}

#[test]
fn many_sessions_are_capped_and_the_rest_counted() {
    let f = Fixture::new();
    let desktop = f.home().join("Desktop");
    for i in 0..45 {
        f.session(&format!("s{i:03}"), &desktop, "2026-04-01", &[user(&format!("question {i}"))]);
    }
    let mut buf = Vec::new();
    let args = inventory_args(&f, &[], &mut buf);
    let (text, _) = serve(&args);
    assert!(text.contains("listed: 40 (5 left out)"), "{text}");
}

#[test]
fn no_sessions_says_nothing_to_order() {
    let f = Fixture::new();
    let mut buf = Vec::new();
    let args = inventory_args(&f, &[], &mut buf);
    let (text, ok) = serve(&args);
    assert!(ok && text.contains("Nothing to order"), "{text}");
}

#[test]
fn digest_reads_cheaply_and_never_shows_a_secret() {
    let f = Fixture::new();
    let desktop = f.home().join("Desktop");
    let token = format!("{}{}", "ghp_", "b".repeat(30));
    f.session(
        "dddd4444",
        &desktop,
        "2026-05-01",
        &[
            user(&format!("My token is {token}, please keep it safe")),
            assistant_text("First thoughts"),
            tool("Write", "file_path", &desktop.join("plan.md")),
            assistant_text("Final answer: the plan is written."),
            user(&"long ".repeat(400)),
        ],
    );
    let file = f.projects().join("encoded-folder").join("dddd4444.jsonl");
    let file_arg = file.display().to_string();
    let (text, ok) = serve(&["inventory", "--digest", &file_arg]);
    assert!(ok, "{text}");
    assert!(!text.contains(&token), "{text}");
    assert!(text.contains("[secret removed]"), "{text}");
    assert!(text.contains("tool: Write") && text.contains("plan.md"), "{text}");
    assert!(text.contains("CLAUDE: Final answer: the plan is written."), "{text}");
    assert!(!text.contains("First thoughts"), "only the last reply of a turn is kept: {text}");
    let long_line = text.lines().find(|l| l.starts_with("PERSON") && l.contains("long")).unwrap();
    assert!(long_line.chars().count() < 700, "{}", long_line.len());
    assert!(text.chars().count() < 21_000);
}

#[test]
fn a_garbled_line_is_skipped_not_an_error() {
    let f = Fixture::new();
    let desktop = f.home().join("Desktop");
    f.session("eeee5555", &desktop, "2026-05-02", &["{not json".to_string(), user("Hello there")]);
    let mut buf = Vec::new();
    let args = inventory_args(&f, &[], &mut buf);
    let (text, ok) = serve(&args);
    assert!(ok && text.contains("first prompt: Hello there"), "{text}");
}
