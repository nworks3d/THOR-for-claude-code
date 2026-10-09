//! The write-up of a THOR bug candidate: the scrubber, the issue template and
//! the nudge text (`thor_bugs` finds the candidates; this says them).
//!
//! WHAT THE NUDGE IS. THOR finds some of its own bugs from signals it already
//! has (`thor_bugs`). Once per session it hands ONE of them to the session's
//! agent as a soft demand: a nudge that holds nothing, asking the agent to
//! draft an issue (or a pull request) for the public THOR repository from
//! THOR's evidence. Nothing is published without the user's own yes, each
//! time. On the owner's development setup (mode `test`) the same text runs
//! with one more paragraph: this is a test environment, so the agent shows
//! the write-up and reports it instead of pushing.
//!
//! WHAT THE SCRUBBER PROMISES. Everything that goes into a write-up passes
//! `scrub` first: a path, a project name, a host, an address, a user name, a
//! session id, an e-mail address and a secret token each become a placeholder.
//! It works from SHAPES (a drive letter, `/home/...`, four dotted numbers, a
//! `.internal` name) and from NAMES THE CALLER KNOWS (`Known`: the memory's
//! project names and this machine's user and host names). `residue` re-reads
//! the scrubbed text and names anything that still looks private, so the
//! nudge can warn the agent instead of vouching for a text it cannot vouch for.

use regex::Regex;
use std::sync::OnceLock;

/// The public repository bug write-ups go to.
pub const UPSTREAM_REPO: &str = "nworks3d/thor-for-claude-code";

/// Where a person without the `gh` program opens a new issue.
pub fn new_issue_url() -> String {
    format!("https://github.com/{UPSTREAM_REPO}/issues/new")
}

/// What the scrubber knows about THIS machine and memory that no shape gives
/// away: names to replace wherever they appear, whole words, any case.
#[derive(Debug, Clone, Default)]
pub struct Known {
    /// Project names (the memory's scopes).
    pub projects: Vec<String>,
    /// User and machine names.
    pub people_and_hosts: Vec<String>,
}

impl Known {
    /// This machine's user and host names, from the environment and the home
    /// folder's name, plus `projects`.
    pub fn gather(projects: Vec<String>) -> Self {
        let mut names: Vec<String> = ["USERNAME", "USER", "LOGNAME", "COMPUTERNAME", "HOSTNAME"]
            .iter()
            .filter_map(|key| std::env::var(key).ok())
            .collect();
        for key in ["USERPROFILE", "HOME"] {
            if let Some(home) = std::env::var_os(key) {
                if let Some(name) = std::path::Path::new(&home).file_name() {
                    names.push(name.to_string_lossy().into_owned());
                }
            }
        }
        names.retain(|n| n.chars().count() >= 3);
        names.sort();
        names.dedup();
        Known { projects, people_and_hosts: names }
    }
}

fn re(cell: &'static OnceLock<Regex>, pattern: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pattern).expect("a fixed pattern"))
}

static URL: OnceLock<Regex> = OnceLock::new();
static EMAIL: OnceLock<Regex> = OnceLock::new();
static WIN_PATH: OnceLock<Regex> = OnceLock::new();
static UNC_PATH: OnceLock<Regex> = OnceLock::new();
static UNIX_PATH: OnceLock<Regex> = OnceLock::new();
static REL_WIN_PATH: OnceLock<Regex> = OnceLock::new();
static IPV4: OnceLock<Regex> = OnceLock::new();
static HOST: OnceLock<Regex> = OnceLock::new();
static SESSION: OnceLock<Regex> = OnceLock::new();

/// THOR's own program names: a path ending in one keeps that name, because
/// "which program" is evidence and the folder it sits in is not.
const PROGRAMS: &[&str] = &["serve", "mcp", "doctor", "install", "backup", "sync", "library"];

fn keep_program_name(matched: &str) -> String {
    // Punctuation that ends a sentence is not part of the path.
    let path = matched.trim_end_matches(['.', ',', ';', ':', ')']);
    let tail = &matched[path.len()..];
    let last = path.rsplit(['/', '\\']).next().unwrap_or("");
    let stem = last.strip_suffix(".exe").unwrap_or(last);
    if PROGRAMS.contains(&stem) && last != path {
        format!("<path>/{last}{tail}")
    } else {
        format!("<path>{tail}")
    }
}

/// Replace every word-bounded, case-insensitive occurrence of each name.
fn replace_names(text: &str, names: &[String], placeholder: &str) -> String {
    let mut out = text.to_string();
    for name in names {
        let name = name.trim();
        if name.chars().count() < 3 {
            continue;
        }
        let pattern = format!(r"(?i)\b{}\b", regex::escape(name));
        if let Ok(rx) = Regex::new(&pattern) {
            out = rx.replace_all(&out, placeholder).into_owned();
        }
    }
    out
}

/// `text` with everything private replaced by a placeholder: secrets, URLs
/// (the public THOR repository's own address stays), e-mail addresses, paths,
/// IPv4 addresses, host names, session ids, then the names `known` lists.
pub fn scrub(text: &str, known: &Known) -> String {
    // Secrets first, from the end so offsets stay valid. `find` is the one
    // definition the write gate uses.
    let mut out = text.to_string();
    let mut found = model::secrets::find(&out);
    found.sort_by_key(|f| std::cmp::Reverse(f.start));
    for f in found {
        out.replace_range(f.start..f.end, "<secret>");
    }

    // The public THOR repository's own addresses are kept: they are swapped
    // for a marker no other pattern matches, and put back at the end.
    let public = format!("https://github.com/{UPSTREAM_REPO}");
    let mut kept: Vec<String> = Vec::new();
    out = re(&URL, r#"https?://[^\s)>"'`]+"#)
        .replace_all(&out, |caps: &regex::Captures| {
            if caps[0].starts_with(&public) {
                kept.push(caps[0].to_string());
                format!("\u{1}{}\u{1}", kept.len() - 1)
            } else {
                "<url>".to_string()
            }
        })
        .into_owned();
    out = re(&EMAIL, r"[A-Za-z0-9._%+\-]+@[A-Za-z0-9.\-]+\.[A-Za-z]{2,}").replace_all(&out, "<email>").into_owned();
    out = re(&UNC_PATH, r"\\\\[A-Za-z0-9_.\-]+(?:\\[^\s\\]+)*").replace_all(&out, "<path>").into_owned();
    out = re(&WIN_PATH, r#"[A-Za-z]:[\\/][^\s"'<>|*?`]*"#)
        .replace_all(&out, |caps: &regex::Captures| keep_program_name(&caps[0]))
        .into_owned();
    out = re(&UNIX_PATH, r#"(^|[\s"'(=`])((?:~|/[A-Za-z0-9_.\-]+)(?:/[A-Za-z0-9_.\-]+)+/?)"#)
        .replace_all(&out, |caps: &regex::Captures| format!("{}{}", &caps[1], keep_program_name(&caps[2])))
        .into_owned();
    out = re(&REL_WIN_PATH, r"[A-Za-z0-9_.\-]+(?:\\[A-Za-z0-9_.\-]+)+")
        .replace_all(&out, |caps: &regex::Captures| keep_program_name(&caps[0]))
        .into_owned();
    out = re(&IPV4, r"\b\d{1,3}(?:\.\d{1,3}){3}\b").replace_all(&out, "<ip>").into_owned();
    out = re(
        &HOST,
        r"(?i)\b[a-z0-9\-]+(?:\.[a-z0-9\-]+)*\.(?:internal|local|lan|home|corp|localdomain|localhost|test|com|net|org|io|dev|nl|be|de|eu|app|cloud|ts\.net)\b",
    )
    .replace_all(&out, "<host>")
    .into_owned();
    out = re(&SESSION, r"(?i)\b[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\b")
        .replace_all(&out, "<session>")
        .into_owned();
    out = replace_names(&out, &known.projects, "<project>");
    out = replace_names(&out, &known.people_and_hosts, "<user/host>");
    for (i, url) in kept.iter().enumerate() {
        out = out.replace(&format!("\u{1}{i}\u{1}"), url);
    }
    out
}

/// What in an ALREADY SCRUBBED `text` still looks private - a short list of
/// kinds, empty when nothing does. The nudge warns the agent when this is
/// not empty instead of vouching for the text.
pub fn residue(text: &str, known: &Known) -> Vec<&'static str> {
    let mut left = Vec::new();
    if !model::secrets::find(text).is_empty() {
        left.push("a secret");
    }
    if re(&WIN_PATH, r#"[A-Za-z]:[\\/][^\s"'<>|*?`]*"#).is_match(text) || re(&UNC_PATH, r"\\\\[A-Za-z0-9_.\-]+(?:\\[^\s\\]+)*").is_match(text) {
        left.push("a path");
    }
    if re(&IPV4, r"\b\d{1,3}(?:\.\d{1,3}){3}\b").is_match(text) {
        left.push("an address");
    }
    let named = |names: &[String]| {
        names.iter().any(|n| {
            n.trim().chars().count() >= 3
                && Regex::new(&format!(r"(?i)\b{}\b", regex::escape(n.trim()))).is_ok_and(|rx| rx.is_match(text))
        })
    };
    if named(&known.projects) {
        left.push("a project name");
    }
    if named(&known.people_and_hosts) {
        left.push("a user or host name");
    }
    left
}

/// One candidate as the write-up needs it. `thor_bugs::Candidate` fills it.
#[derive(Debug, Clone)]
pub struct WriteUp<'a> {
    pub fingerprint: &'a str,
    pub signal: &'a str,
    /// A short title, without the signal.
    pub title: &'a str,
    pub summary: &'a str,
    pub why_thor: &'a str,
    pub evidence: &'a [String],
    pub expected: &'a str,
    pub actual: &'a str,
    pub fix_area: &'a str,
    /// "2.5.0 (build <id>, renderer <n>)".
    pub version: &'a str,
}

/// The issue title.
pub fn title(w: &WriteUp<'_>, known: &Known) -> String {
    let title = scrub(w.title, known);
    format!("{} [{}]", title.trim().trim_end_matches('.'), w.signal)
}

/// The issue (or pull request) body: version and build, the signal, redacted
/// evidence, expected and actual behaviour, a suggested fix area. Scrubbed
/// as a whole, so a field a detector filled carelessly cannot leak.
pub fn body(w: &WriteUp<'_>, known: &Known) -> String {
    let mut out = String::new();
    out.push_str(&format!("**THOR version and build:** {}\n", w.version));
    out.push_str(&format!("**Signal:** {} (fingerprint {})\n", w.signal, w.fingerprint));
    out.push_str(&format!("**Platform:** {}\n\n", std::env::consts::OS));
    out.push_str(&format!("**What THOR saw:** {}\n\n", w.summary));
    out.push_str(&format!("**Why this looks like a THOR bug and not a user's rule or work:** {}\n\n", w.why_thor));
    out.push_str("**Evidence (redacted):**\n");
    for line in w.evidence {
        out.push_str(&format!("- {line}\n"));
    }
    out.push_str(&format!("\n**Expected:** {}\n\n**Actual:** {}\n\n", w.expected, w.actual));
    out.push_str(&format!("**Suggested fix area:** {}\n\n", w.fix_area));
    out.push_str("_Drafted from THOR's own signals and redacted of paths, project names, hosts, addresses, user names and secrets before it was shown to anyone._\n");
    scrub(&out, known)
}

/// How the nudge is worded for a mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// An install of the official release: the agent asks the user for a go,
    /// then opens the issue.
    Release,
    /// The owner's development setup: the agent shows the write-up and
    /// reports it, and pushes nothing.
    Test,
}

/// The text a session's first prompt opens with. `report_id` is the id of the
/// outcome report the agent records (`thor-bug-<fingerprint>`).
pub fn nudge(w: &WriteUp<'_>, mode: Mode, known: &Known) -> String {
    let title = title(w, known);
    let body = body(w, known);
    let fp = w.fingerprint;
    let short: String = scrub(w.title, known).chars().take(100).collect();
    let report_id = format!("{}{fp}", model::store::THOR_BUG_ID_PREFIX);
    let tag = format!("{}{}", crate::thor_bugs::REPORT_TAG_PREFIX, w.fingerprint);
    let mut out = String::new();
    out.push_str(&format!(
        "[THOR] THOR may have found a bug in ITSELF. This is a soft nudge: it holds nothing, it is not part of the \
         owner's task, and you may finish what the owner asked first. It will not come again in this session.\n\n\
         WHAT THOR SAW: {}\n\
         WHY THIS LOOKS LIKE A THOR BUG: {}\n\n",
        scrub(w.summary, known),
        scrub(w.why_thor, known)
    ));
    out.push_str(&format!(
        "WHAT TO DO (meaning: turn this into a report the THOR makers can act on, only if the user says yes):\n\
         1. The TITLE and BODY below are the text that would be sent. Keep them in ENGLISH, so the makers can read them. \
         They are already stripped of file paths, project and folder names, computer and user names, addresses, keys and \
         passwords. Read them once more yourself and remove anything private that is left.\n\
         2. If you can reach the THOR source and confirm the cause, add it under 'Suggested fix area'. Write a pull request \
         only if you also have a tested fix; otherwise an issue.\n\
         3. When the owner's task is at a stopping point, ask the user ONE question, in the user's own language, \
         with the exact full TITLE and BODY shown above it. The question says: what THOR found (an error in itself); \
         that the text shown is exactly what would be sent; what was removed (file paths, project and folder names, \
         computer and user names, addresses, keys and passwords); that nothing is sent without their yes; and that \
         saying no is fine. For example, in Dutch: \"THOR vond een fout in zichzelf. Hieronder staat precies wat ik naar de \
         makers zou sturen; namen, paden, adressen en wachtwoorden zijn eruit gehaald, er gaat niets anders mee. Mag ik het \
         versturen? Nee zeggen kan gewoon.\" Ask for nothing but yes or no: never ask the user to type, copy or edit anything.\n\
         4. After a yes: if `gh auth status` succeeds, run gh issue create --repo {UPSTREAM_REPO} --title \"<the title>\" \
         --body-file <a file holding the body>. Otherwise open {} in the user's browser with ?title=<title>&body=<body> \
         added, both URL-encoded (PowerShell [uri]::EscapeDataString, Python urllib.parse.quote(text, safe=\"\")), the \
         whole address within about 7000 characters: if the body is too long, cut it and end it with (shortened). Open it with \
         Start-Process on Windows, open on macOS, xdg-open on Linux; the user then only clicks Submit. After a no, send nothing.\n\
         5. Record what happened with the THOR tool: remember(id: \"{report_id}\", kind: \"report\", \
         text: \"OUTCOME for {fp} ({short}): <one of: opened <address of the issue> | owner declined | shown to the owner, not pushed \
         (test environment)>. <one sentence of anything you learned>\", tags: [\"{tag}\"]). Add no project and no path \
         to that text. DONE WHEN: that remember call is accepted; THOR then stops raising this bug.\n\n",
        new_issue_url()
    ));
    if mode == Mode::Test {
        out.push_str(&format!(
            "THIS IS A TEST ENVIRONMENT (the owner's own development setup). Do NOT do step 3 or 4: push nothing - no \
             gh call, no issue, no pull request, and do not ask for a go. Instead say to the owner, in your own words, \
             \"normally I would push this to the THOR repo\", show the finished write-up (title and body below) in full, \
             and report it to the owner. Then do step 5 with the outcome: shown to the owner, not pushed (test \
             environment). This lets the owner see that the text and the mechanism work without publishing anything.\n\n"
        ));
    }
    out.push_str(&format!("TITLE: {title}\nBODY:\n{body}"));
    let left = residue(&format!("{title}\n{body}"), known);
    if !left.is_empty() {
        out.push_str(&format!(
            "\nTHOR'S SCRUB IS NOT SURE: the text above may still hold {}. Remove it before anyone sees the text.\n",
            left.join(", ")
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn known() -> Known {
        Known { projects: vec!["Acme-Widgets".to_string()], people_and_hosts: vec!["someone".to_string(), "workstation-7".to_string()] }
    }

    #[test]
    fn a_windows_path_becomes_a_placeholder_but_a_thor_program_keeps_its_name() {
        let out = scrub(r#"ran "C:\Users\someone\thor2\bin\serve.exe" --db C:\Users\someone\thor2\thor.db"#, &known());
        assert!(out.contains("<path>/serve.exe"), "{out}");
        assert!(!out.contains("someone") && !out.contains("C:\\"), "{out}");
        assert!(out.contains("--db <path>"), "{out}");
    }

    #[test]
    fn unix_paths_unc_paths_and_the_home_shorthand_are_replaced() {
        let out = scrub("see /home/someone/repo/src/main.rs and ~/notes/x.md and \\\\nas01\\share\\thor2\\a.db", &known());
        assert_eq!(out, "see <path> and <path> and <path>");
    }

    #[test]
    fn hosts_addresses_urls_emails_and_sessions_are_replaced() {
        let out = scrub(
            "host staging.corp.example.internal at 10.20.30.40, see https://wiki.example.com/page?x=1 mail me@example.org \
             session 0a8aa202-cf49-4bf1-a39d-c7177a6a81af",
            &known(),
        );
        assert_eq!(out, "host <host> at <ip>, see <url> mail <email> session <session>");
    }

    #[test]
    fn the_public_thor_repository_address_is_kept() {
        let out = scrub(&format!("open {} now", new_issue_url()), &known());
        assert!(out.contains(UPSTREAM_REPO), "{out}");
    }

    #[test]
    fn project_user_and_host_names_are_replaced_as_whole_words_in_any_case() {
        let out = scrub("Acme-Widgets failed for SOMEONE on workstation-7 (someones is not a name)", &known());
        assert_eq!(out, "<project> failed for <user/host> on <user/host> (someones is not a name)");
    }

    #[test]
    fn a_secret_token_becomes_a_placeholder() {
        let token = format!("ghp_{}", "a1B2c3D4e5F6g7H8i9J0k1L2m3N4o5P6q7R8");
        let out = scrub(&format!("header token {token} leaked"), &known());
        assert_eq!(out, "header token <secret> leaked");
    }

    #[test]
    fn plain_evidence_survives_untouched() {
        let text = "validator record 12 of 40: exit code 126, event SessionStart, renderer 7 vs 6, build ec4708ca2b22, v2.5.0";
        assert_eq!(scrub(text, &known()), text);
        let words = "serve.exe, thor.db and eval_filing.rs are file names, not hosts";
        assert_eq!(scrub(words, &known()), words);
    }

    #[test]
    fn residue_names_what_a_scrub_missed_and_is_empty_after_a_good_scrub() {
        let dirty = r"left over: D:\data\x and 198.51.100.5 and Acme-Widgets";
        let kinds = residue(dirty, &known());
        assert!(kinds.contains(&"a path") && kinds.contains(&"an address") && kinds.contains(&"a project name"), "{kinds:?}");
        assert!(residue(&scrub(dirty, &known()), &known()).is_empty());
    }

    fn write_up<'a>(evidence: &'a [String]) -> WriteUp<'a> {
        WriteUp {
            fingerprint: "hook-launch-1a2b3c4d",
            signal: "hook-launch",
            title: "Hook launch failure: SessionStart hook, exit code 126 in C:\\Users\\someone\\thor2",
            summary: "Claude Code could not start THOR's hook (SessionStart, exit code 126) in C:\\Users\\someone\\thor2.",
            why_thor: "No other program's hook failed at the same moment, so the machine was not shutting down.",
            evidence,
            expected: "The hook starts.",
            actual: "Claude Code reported a launch failure and went on without THOR.",
            fix_area: "the installer's hook command",
            version: "2.5.0 (build ec4708ca2b22, renderer 7)",
        }
    }

    #[test]
    fn the_body_carries_every_template_part_and_no_private_text() {
        let evidence = vec!["command ran in /home/someone/work/Acme-Widgets".to_string()];
        let text = body(&write_up(&evidence), &known());
        for part in [
            "**THOR version and build:** 2.5.0 (build ec4708ca2b22, renderer 7)",
            "**Signal:** hook-launch (fingerprint hook-launch-1a2b3c4d)",
            "**What THOR saw:**",
            "**Evidence (redacted):**",
            "**Expected:** The hook starts.",
            "**Actual:**",
            "**Suggested fix area:** the installer's hook command",
        ] {
            assert!(text.contains(part), "missing {part:?} in {text}");
        }
        assert!(residue(&text, &known()).is_empty() && !text.contains("someone") && !text.contains("Acme"), "{text}");
        let title = title(&write_up(&evidence), &known());
        assert!(title.ends_with("[hook-launch]") && !title.contains("someone"), "{title}");
    }

    #[test]
    fn the_test_nudge_is_the_release_nudge_plus_the_test_paragraph() {
        let evidence = vec!["exit code 126".to_string()];
        let w = write_up(&evidence);
        let release = nudge(&w, Mode::Release, &known());
        let test = nudge(&w, Mode::Test, &known());
        assert!(!release.contains("TEST ENVIRONMENT"));
        assert!(test.contains("THIS IS A TEST ENVIRONMENT") && test.contains("normally I would push this to the THOR repo"));
        let without_paragraph: String = {
            let start = test.find("THIS IS A TEST ENVIRONMENT").unwrap();
            let end = test.find("TITLE: ").unwrap();
            format!("{}{}", &test[..start], &test[end..])
        };
        assert_eq!(without_paragraph, release, "the two modes differ by the test paragraph alone");
        for text in [&release, &test] {
            assert!(text.contains("remember(id: \"thor-bug-hook-launch-1a2b3c4d\", kind: \"report\""), "{text}");
            assert!(text.contains("tags: [\"thor-bug:hook-launch-1a2b3c4d\"]"), "{text}");
            assert!(text.contains("DONE WHEN"), "{text}");
        }
        assert!(release.contains("gh issue create --repo nworks3d/thor-for-claude-code"));
        // The one question reassures: exact text, what was removed, no send without a yes, no is fine.
        for part in [
            "THOR vond een fout in zichzelf",
            "exactly what would be sent",
            "file paths, project and folder names, computer and user names, addresses, keys and passwords",
            "nothing is sent without their yes",
            "saying no is fine",
            "ENGLISH",
            "issues/new",
            "7000 characters",
            "(shortened)",
            "never ask the user to type, copy or edit anything",
        ] {
            assert!(release.contains(part), "missing {part:?}: {release}");
        }
    }

    #[test]
    fn nothing_in_the_nudge_is_longer_than_a_hook_context_can_carry() {
        let evidence: Vec<String> = (0..5).map(|i| format!("evidence line {i} {}", "x".repeat(250))).collect();
        let text = nudge(&write_up(&evidence), Mode::Test, &known());
        assert!(text.len() < 6_000, "{}", text.len());
    }
}
