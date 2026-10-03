//! RED and GREEN for the door, ported from the prototype that measured it
//! (neutral names). RED = the real guard must refuse; GREEN = it must let the
//! call through. Where a case says what the matcher before this module did,
//! that is read off `head`, a frozen copy of it kept for exactly that purpose.

use super::*;
use crate::absent_guard;
use crate::live::LiveItem;
use model::item::{Binding, Check, Item, Kind, TargetKind};
use std::path::Path;

/// The matcher before segments and token boundaries, copied verbatim: the
/// anchor against the first words of the whole line, the literal anywhere in
/// it, an inner trigger as whole words. Test-only: it pins what changed.
mod head {
    pub fn strip_invocation_wrapper(command: &str) -> String {
        let mut words: Vec<&str> = command.split_whitespace().collect();
        while let Some(first) = words.first() {
            if *first == "sudo" || (first.contains('=') && !first.starts_with('-')) {
                words.remove(0);
            } else {
                break;
            }
        }
        if let Some(first) = words.first_mut() {
            let base = first.rsplit(['/', '\\']).next().unwrap_or(first);
            *first = base.strip_suffix(".exe").or_else(|| base.strip_suffix(".EXE")).unwrap_or(base);
        }
        words.join(" ")
    }

    pub fn command_anchor_matches(anchor: &str, command: &str) -> bool {
        let anchor_words: Vec<String> = anchor.split_whitespace().map(str::to_lowercase).collect();
        if anchor_words.is_empty() {
            return false;
        }
        let command_words: Vec<String> =
            command.split_whitespace().take(anchor_words.len()).map(str::to_lowercase).collect();
        command_words == anchor_words
    }

    pub fn command_anchor_names(anchor: &str, command: &str) -> bool {
        command_anchor_matches(anchor, command) || command_anchor_matches(anchor, &strip_invocation_wrapper(command))
    }

    pub fn contains_as_words(haystack: &str, needle: &str) -> bool {
        let needle = needle.trim();
        if needle.is_empty() {
            return false;
        }
        let joins = |c: char| c.is_alphanumeric() || "-_.".contains(c);
        haystack.match_indices(needle).any(|(at, _)| {
            let before = haystack[..at].chars().next_back();
            let after = haystack[at + needle.len()..].chars().next();
            let starts_clean = before.is_none_or(|c| !joins(c) || !needle.starts_with(joins));
            let ends_clean = after.is_none_or(|c| !joins(c) || !needle.ends_with(joins));
            starts_clean && ends_clean
        })
    }

    pub fn same_trigger_words(when: &str, binding: &str) -> bool {
        let when_words: Vec<String> = when.split_whitespace().map(str::to_lowercase).collect();
        !when_words.is_empty() && when_words == binding.split_whitespace().map(str::to_lowercase).collect::<Vec<_>>()
    }

    pub fn forbidden_refuses(anchors: &[String], literals: &[String], command: &str) -> bool {
        let normalised = strip_invocation_wrapper(command);
        if !anchors.iter().any(|a| command_anchor_matches(a, command) || command_anchor_matches(a, &normalised)) {
            return false;
        }
        literals.iter().any(|l| command.contains(l.as_str()))
    }

    pub fn requires_refuses(anchors: &[String], when: &str, required: &[String], command: &str) -> bool {
        if !anchors.iter().any(|a| command_anchor_names(a, command)) {
            return false;
        }
        let inner = !anchors.iter().any(|a| same_trigger_words(when, a));
        if inner && !contains_as_words(command, when) {
            return false;
        }
        !required.iter().any(|r| command.contains(r.as_str()))
    }
}

fn s(v: &[&str]) -> Vec<String> {
    v.iter().map(|x| x.to_string()).collect()
}

fn rule(anchors: &[&str], check: Check) -> Vec<LiveItem> {
    let item = Item {
        id: "door-rule".to_string(),
        kind: Kind::Rule,
        text: "a door fixture rule".to_string(),
        bindings: anchors.iter().map(|a| Binding::Target { kind: TargetKind::Command, value: a.to_string() }).collect(),
        severity: None,
        project: None,
        tags: vec![],
        expires: None,
        key: None,
        falsifier: Some("the fixture turns out wrong".to_string()),
        check: Some(check),
    };
    vec![LiveItem { id: item.id.clone(), item }]
}

/// (HEAD today, the real guard now) for a forbidden rule, in `d`.
fn fb_in(anchors: &[&str], lits: &[&str], cmd: &str, d: Dialect) -> (bool, bool) {
    let items = rule(anchors, Check::Forbidden { literals: s(lits) });
    let segs = segments(cmd, d);
    let now = absent_guard::find_command_violation_in(&items, &segs, None, Some(Path::new("."))).is_some();
    (head::forbidden_refuses(&s(anchors), &s(lits), cmd), now)
}

fn fb(anchors: &[&str], lits: &[&str], cmd: &str) -> (bool, bool) {
    fb_in(anchors, lits, cmd, Dialect::Sh)
}

fn fb_ps(anchors: &[&str], lits: &[&str], cmd: &str) -> (bool, bool) {
    fb_in(anchors, lits, cmd, Dialect::PowerShell)
}

/// (HEAD today, the real guard now) for a requires rule, the call's content
/// being the command itself (a Bash call's only input value).
fn rq(anchors: &[&str], when: &str, req: &[&str], cmd: &str) -> (bool, bool) {
    let items = rule(anchors, Check::Requires { when: when.to_string(), required: s(req) });
    let now = absent_guard::find_requires_violation(&items, "Bash", Some(cmd), cmd).is_some();
    (head::requires_refuses(&s(anchors), when, &s(req), cmd), now)
}

const PROD: &[&str] = &["acme-prod node -e", "acme-prod node --input-type=module"];

#[test]
fn z4_red_prod_container_code_in_every_compound_shape() {
    let cases = [
        // (command, refused by HEAD today)
        ("docker exec acme-prod node -e \"1\"", true),
        ("cd /srv && docker exec acme-prod node -e \"1\"", false),
        ("docker ps; docker exec acme-prod node -e \"1\"", false),
        ("cd /srv\ndocker exec acme-prod node -e \"1\"", false),
        ("echo x | docker exec -i acme-prod node -e \"1\"", false),
        ("ssh nas \"docker exec acme-prod node -e 'console.log(1)'\"", false),
        ("ssh -p 2222 admin@nas 'sudo docker exec acme-prod node -e \"1\"'", false),
        ("bash -c \"docker exec acme-prod node -e 1\"", false),
        ("bash -lc 'cd /x && docker exec acme-prod node -e 1'", false),
        ("ssh nas bash -s <<'EOF'\ncd /srv\ndocker exec acme-prod node -e 1\nEOF", false),
        ("x=$(docker exec acme-prod node -e 1)", false),
        ("sudo -u root docker exec acme-prod node -e 1", false),
        ("if docker exec acme-prod node -e 1; then echo ok; fi", false),
        ("(cd /srv && docker exec acme-prod node -e 1)", false),
        ("timeout 30 docker exec acme-prod node -e 1", false),
    ];
    for (cmd, today) in cases {
        let (o, n) = fb(&["docker exec"], PROD, cmd);
        assert_eq!(o, today, "HEAD today for {cmd:?}");
        assert!(n, "must be refused: {cmd:?}");
    }
}

#[test]
fn z4_green_mentions_and_other_containers_pass() {
    for cmd in [
        "docker exec acme-dev node -e \"1\"",
        "echo \"docker exec acme-prod node -e\"",
        "grep -n \"docker exec acme-prod node -e\" notes.md",
        "git commit -m \"doc: never docker exec acme-prod node -e; use scripts\"",
        "cat > notes.md <<'EOF'\nNever: docker exec acme-prod node -e x; ever\nEOF",
        "ssh nas \"grep 'docker exec acme-prod node -e' /var/log/x\"",
        "# docker exec acme-prod node -e\nls",
        "ls # ; docker exec acme-prod node -e x",
        "ssh nas \"echo 'docker exec acme-prod node -e'\"",
        "python - <<'EOF'\nimport subprocess\nprint('docker exec acme-prod node -e')\nEOF",
        "docker logs acme-prod | grep 'node -e'",
    ] {
        let (_, n) = fb(&["docker exec"], PROD, cmd);
        assert!(!n, "must pass: {cmd:?}");
    }
}

const ATTR: &[&str] = &["Co-Authored-By", "Co-authored-by", "Generated with"];

#[test]
fn z4_red_attribution_in_the_real_commit_shapes() {
    let cases = [
        ("git commit -m \"x\n\nCo-Authored-By: C <n@a.com>\"", true),
        ("git add -A && git commit -m \"$(cat <<'EOF'\nIt's fixed (see #3)\n\nCo-Authored-By: C\nEOF\n)\"", false),
        ("git add . && git commit -F - <<'EOF'\nfix\n\nCo-Authored-By: C\nEOF", false),
        ("cd repo && gh pr create --title t --body \"Generated with X\"", false),
        ("printf 'fix\\n\\nCo-Authored-By: C' | git commit -F -", false),
    ];
    for (cmd, today) in cases {
        let (o, n) = fb(&["git commit", "gh pr create"], ATTR, cmd);
        assert_eq!(o, today, "HEAD today for {cmd:?}");
        assert!(n, "must be refused: {cmd:?}");
    }
}

#[test]
fn z4_green_attribution_mentions_pass_and_one_false_block_of_today_goes_away() {
    for cmd in [
        "git add -A && git commit -m \"fix: parser\"",
        "git log --grep \"Co-Authored-By\"",
        "git add -A && git commit -m \"$(cat <<'EOF'\nIt's fixed (see #3)\nEOF\n)\" && git push",
    ] {
        assert!(!fb(&["git commit", "gh pr create"], ATTR, cmd).1, "must pass: {cmd:?}");
    }
    // HEAD refuses this today (the literal sits in a later, separate command);
    // the pipeline unit lets it through.
    let (o, n) = fb(&["git commit"], ATTR, "git commit -m \"fix\" && git log -1 --format=%B | grep -c \"Co-Authored-By\"");
    assert!(o && !n);
}

#[test]
fn z4_the_commit_heredoc_does_not_swallow_the_push_after_it() {
    let cmd = "git add -A && git commit -m \"$(cat <<'EOF'\nIt's fixed (see #3\nEOF\n)\" && git push --tags";
    let (o, n) = fb(&["git push"], &["push --tags"], cmd);
    assert!(!o && n, "the push after the commit heredoc is its own segment");
}

#[test]
fn z4_loop_rule_keeps_its_reach_and_gains_the_compound_shape() {
    let a = &["until", "while", "for"];
    for (cmd, today) in [
        ("while true; do ssh nas uptime; sleep 5; done", true),
        ("for i in 1 2 3; do ssh nas \"ls /srv\"; done", true),
        ("cd /x && until ssh nas test -f /x; do sleep 2; done", false),
    ] {
        let (o, n) = fb(a, &["ssh"], cmd);
        assert_eq!(o, today, "{cmd:?}");
        assert!(n, "must be refused: {cmd:?}");
    }
    for cmd in ["for f in *.log; do grep -c ERR \"$f\"; done", "ssh nas uptime", "echo \"while true; do ssh nas; done\""] {
        assert!(!fb(a, &["ssh"], cmd).1, "must pass: {cmd:?}");
    }
    // HEAD refuses a single ssh that merely FOLLOWS a loop (the literal is
    // looked for in the whole line); the loop unit now ends at its `done`.
    let (o, n) = fb(a, &["ssh"], "for f in a b; do echo $f; done; ssh nas uptime");
    assert!(o && !n);
    // Nested loops still close at the right `done`.
    assert!(fb(a, &["ssh"], "for h in a b; do for i in 1 2; do echo $i; done; ssh $h uptime; done").1);
    // PowerShell: the body is the braces.
    assert!(fb_ps(a, &["ssh"], "while ($true) { ssh nas uptime; Start-Sleep 5 }").1);
    assert!(!fb_ps(a, &["ssh"], "foreach ($f in $files) { Get-Item $f }; ssh nas uptime").1);
}

#[test]
fn z4_requires_reaches_the_compound_and_ssh_shapes() {
    let a = &["docker exec acme-prod"];
    let w = "docker exec acme-prod";
    for (cmd, today) in [
        ("docker exec acme-prod ls /app", true),
        ("ssh nas \"docker exec acme-prod ls /app\"", false),
        ("cd x && docker exec acme-prod ls", false),
    ] {
        let (o, n) = rq(a, w, &["go-owner"], cmd);
        assert_eq!(o, today, "{cmd:?}");
        assert!(n, "must be refused: {cmd:?}");
    }
    for cmd in [
        "ssh nas \"docker exec acme-dev ls /app\"",
        "ssh nas \"docker exec acme-prod ls\" # go-owner 2026-09-28",
        "docker logs acme-prod",
        "echo 'docker exec acme-prod ls'",
    ] {
        assert!(!rq(a, w, &["go-owner"], cmd).1, "must pass: {cmd:?}");
    }
}

#[test]
fn z4_existing_inner_trigger_tests_still_hold() {
    let a = &["ssh"];
    let w = "-p acme-dev";
    let r = &["-f docker-compose.dev.yml"];
    assert!(rq(a, w, r, "ssh admin@host \"cd /srv/app && docker compose -p acme-dev up -d\"").1);
    assert!(!rq(a, w, r, "ssh admin@host \"cd /srv/app && docker compose -f docker-compose.dev.yml -p acme-dev up -d\"").1);
    for cmd in [
        "ssh admin@host \"docker ps\"",
        "ssh admin@host \"cd /srv/app && docker compose -p acme-prod up -d\"",
        "ssh admin@host \"docker compose -p acme-dev-old logs\"",
        "ssh admin@host uptime",
        "scp -p acme-dev host:/x .",
    ] {
        assert!(!rq(a, w, r, cmd).1, "{cmd:?}");
    }
    // /health inside a URL, /healthz not
    let a = &["curl"];
    assert!(rq(a, "/health", &["cb="], "curl -s http://10.0.0.5:3002/health").1);
    assert!(!rq(a, "/health", &["cb="], "curl -s http://10.0.0.5:3002/healthz").1);
}

/// An inner trigger narrows the REACHED segment: the literal has to sit in
/// the unit of a segment the binding reaches, never in another command of
/// the same line.
#[test]
fn an_inner_trigger_must_sit_in_the_reached_segment() {
    let a = &["ssh"];
    let w = "-p acme-dev";
    let r = &["-f docker-compose.dev.yml"];
    assert!(!rq(a, w, r, "ssh admin@host uptime; docker compose -p acme-dev up -d").1, "the trigger is in a local command, not the ssh one");
    assert!(rq(a, w, r, "ls; ssh admin@host \"docker compose -p acme-dev up -d\"").1);
}

/// The measured misses, and the corrected diagnosis: HEAD DOES find '/thor'
/// inside an argument, at a path separator; it is the character AFTER the
/// trigger ('-', '_', '.', a letter or a digit) that hides it. The token
/// boundaries keep that decision, and the star is the one way to ask for a
/// prefix.
#[test]
fn z6_today_the_trigger_is_blind_to_a_name_that_continues_after_it() {
    let a = &["mkdir"];
    let r = &["thor2", "scratchpad"];
    assert!(rq(a, "/thor", r, "mkdir /thor").0);
    assert!(rq(a, "/thor", r, "mkdir /c/x/thor/y").0, "inside an argument, at a separator: found today");
    for cmd in ["mkdir /thor-probe-eval", "mkdir foo /thor-x", "mkdir /c/Users/dev/AppData/Local/Temp/thor-probe-eval", "mkdir Temp/thor-probe-eval"] {
        assert!(!rq(a, "/thor", r, cmd).0, "HEAD misses {cmd:?}");
        assert!(!rq(a, "/thor", r, cmd).1, "without a star the boundaries keep missing it: {cmd:?}");
    }
    assert!(rq(a, "/thor", r, "mkdir /c/x/thor/y").1, "and keep finding it at a separator");
}

#[test]
fn z6_red_and_green_for_a_prefix_trigger() {
    let a = &["mkdir"];
    let r = &["thor2", "scratchpad"];
    let w = "/thor*";
    for cmd in [
        "mkdir /c/tmp/thor-probe-eval",
        "mkdir Temp/thor-probe-eval",
        "mkdir foo /thor-x",
        "mkdir -p C:/x/thor3",
        "cd /tmp && mkdir -p /tmp/thor_new",
        "ssh nas 'mkdir /srv/thor-copy'",
    ] {
        assert!(rq(a, w, r, cmd).1, "must be refused: {cmd:?}");
    }
    for cmd in [
        "mkdir C:/x/thor2/code-index",
        "mkdir -p /c/tmp/claude/x/scratchpad/thor-x",
        "mkdir /c/tmp/authority",
        "mkdir thor-notes",
        "echo mkdir /thor-x",
    ] {
        assert!(!rq(a, w, r, cmd).1, "must pass: {cmd:?}");
    }
    // The price of a prefix, stated not hidden: a longer word on the same stem.
    assert!(rq(a, w, r, "mkdir build/thorough").1);
    // Without the star nothing changes for existing whole-word triggers.
    assert!(!rq(&["ssh"], "-p acme-dev", &["-f x"], "ssh h \"docker compose -p acme-dev-old up\"").1);
}

#[test]
fn powershell_call_operator_and_here_string() {
    // `& "C:\...\tool.exe" import` is a ban in PowerShell form.
    let (o, n) = fb_ps(&["tool import", "tool.exe import"], &["tool import", "tool.exe import"], "cd C:\\x; & \"C:\\bin\\tool.exe\" import --all");
    assert!(!o && n, "HEAD misses the call operator; the head of the called program reaches it now");
    let (_, n) = fb_ps(&["git push"], &["push --tags"], "Set-Location C:\\x; git push --tags");
    assert!(n);
    // A here-string body is data, even when it has an apostrophe and a command in it.
    let (_, n) = fb_ps(&["git push"], &["push --tags"], "$m = @'\nit's done; git push --tags\n'@\nSet-Content notes.md $m");
    assert!(!n);
}

#[test]
fn a_place_named_by_a_cd_travels_with_the_command_that_runs_there() {
    let lits = &["acme-dev", "acme-prod", "nas-share"];
    let (o, n) = fb(&["git reset"], lits, "cd //nas-share/docker/acme-prod && git reset --hard origin/main");
    assert!(!o && n, "HEAD misses it; the cd text is part of the unit");
    assert!(fb(&["git reset"], lits, "git -C //nas-share/docker/acme-prod reset --hard").1);
    // A cd that names no such place changes nothing.
    assert!(!fb(&["git reset"], lits, "cd C:/x/dev/repo && git reset --hard origin/main").1);
    // Only the LAST cd before the command counts.
    assert!(!fb(&["git reset"], lits, "cd //nas-share/x && cp a b; cd C:/repo && git reset --hard").1);
}

#[test]
fn an_anchor_written_with_quotes_still_matches_as_it_does_today() {
    let a = &["bws run -- 'node server.js'"];
    let (o, n) = fb(a, &["--watch"], "bws run -- 'node server.js' --watch");
    assert!(o && n);
    assert!(fb(a, &["--watch"], "cd /srv && bws run -- 'node server.js' --watch").1);
}

#[test]
fn the_quoted_windows_path_now_reaches_and_the_unquoted_one_still_does_not() {
    let a = &["git reset"];
    let (o, n) = fb(a, &["--hard"], r#""C:\Program Files\Git\git.exe" reset --hard x"#);
    assert!(!o && n, "quoted: the program is git; HEAD misses it");
    let (o, n) = fb(a, &["--hard"], r"C:\Program Files\Git\git.exe reset --hard x");
    assert!(!o && !n, "unquoted: bash itself would split it; still not reached");
    for c in [
        "GIT_DIR=/srv/x git reset --hard origin/main",
        "sudo git reset --hard origin/main",
        "/usr/bin/git reset --hard origin/main",
        "git -C /srv/x reset --hard",
    ] {
        assert!(fb(a, &["--hard"], c).1, "{c}");
    }
    assert!(!fb(a, &["--hard"], "sudo git pull --ff-only").1);
    // The PowerShell tool reads the same program by its forward-slash path.
    assert!(fb_ps(a, &["--hard"], r#""C:/Program Files/Git/git.exe" reset --hard x"#).1);
}

#[test]
fn spacing_or_quoting_a_word_does_not_hide_a_literal() {
    for c in [
        "docker exec acme-prod  node -e 1",
        "docker exec acme-prod \"node\" -e 1",
        "docker exec acme-prod node \\\n  -e 1",
    ] {
        assert!(fb(&["docker exec"], PROD, c).1, "{c:?}");
    }
}

// ------------------------------------------------------------ token boundaries

#[test]
fn literal_at_boundary_left_and_right() {
    // The over-match that is gone (D5).
    assert!(!literal_at_boundary("git push --force-with-lease origin main", "--force"));
    assert!(literal_at_boundary("git push --force origin main", "--force"));
    assert!(literal_at_boundary("git push --force=true origin main", "--force"));
    // `ssh` inside a longer name, a hidden folder or a snake name.
    for text in ["node sshKey.mjs", "cat ~/.ssh/config", "rm id_ssh", "sshd -t"] {
        assert!(!literal_at_boundary(text, "ssh"), "{text}");
    }
    for text in ["/usr/bin/ssh nas", "ssh nas", "x;ssh nas", "\"ssh\"", "(ssh nas)", "C:\\bin\\ssh nas"] {
        assert!(literal_at_boundary(text, "ssh"), "{text}");
    }
    // A colon after, and a sentence's full stop.
    assert!(literal_at_boundary("fix\n\nCo-Authored-By: C", "Co-Authored-By"));
    assert!(literal_at_boundary("signed Co-Authored-By.", "Co-Authored-By"));
    assert!(!literal_at_boundary("see ssh.config", "ssh"), "a dot followed by a letter is not a boundary");
    // Both sides: a literal that is itself a word on each end.
    assert!(literal_at_boundary("acme-prod node -e 1", "acme-prod node -e"));
    assert!(!literal_at_boundary("xacme-prod node -e 1", "acme-prod node -e"));
    assert!(!literal_at_boundary("acme-prod node -eval", "acme-prod node -e"));
    // The first occurrence is not always the one on a boundary.
    assert!(literal_at_boundary("--force-with-lease --force", "--force"));
    assert_eq!(find_at_boundary("--force-x --force", "--force"), Some(10));
    // An escaped whitespace stands for the whitespace it writes: printf and
    // `echo -e` (`\n`), PowerShell (`` `n ``).
    assert!(literal_at_boundary("printf 'fix\\n\\nCo-Authored-By: C'", "Co-Authored-By"));
    assert!(literal_at_boundary("\"fix`n`nCo-Authored-By: C\"", "Co-Authored-By"));
    assert!(literal_at_boundary("\"x Co-Authored-By`n\"", "Co-Authored-By"));
    assert!(!literal_at_boundary("printf 'fix\\qCo-Authored-By'", "Co-Authored-By"), "only whitespace escapes count");
    assert!(!literal_at_boundary("printf 'fixnCo-Authored-By'", "Co-Authored-By"), "a plain n is a letter");
}

#[test]
fn a_non_word_edge_needs_no_boundary() {
    // A literal whose edges are not word characters matches as before.
    assert!(literal_at_boundary("xx/usr/local/ yy", "/usr/local/"));
    assert!(literal_at_boundary("a\u{2014}b", "\u{2014}"));
    assert!(literal_at_boundary("--x-y", "-x-"));
    // Only a word-character edge asks for a boundary.
    assert!(literal_at_boundary("abc-", "-"));
}

#[test]
fn required_answers_and_file_checks_keep_substring_matching() {
    // A requires ANSWER is a plain substring of the call's input: `--no-deps`
    // inside `--no-deps-please` still satisfies it (strict matching there
    // would refuse more, never less).
    let items = rule(&["pip install"], Check::Requires { when: "pip install".to_string(), required: s(&["--no-deps"]) });
    let cmd = "pip install --no-deps-please x";
    assert!(absent_guard::find_requires_violation(&items, "Bash", Some(cmd), cmd).is_none());
    // A file-content check (an always-bound forbidden literal in content)
    // still matches inside a longer word.
    let mut always = rule(&["unused"], Check::Forbidden { literals: s(&["TODO"]) });
    always[0].item.bindings = vec![Binding::Always];
    assert!(absent_guard::find_forbidden_violation(&always, "a TODOLIST line").is_some());
}

#[test]
fn an_exe_anchor_needs_the_exe_in_the_call() {
    let a = &["install.exe"];
    for c in ["\"C:/x/bin/install.exe\" --all", "./install.exe --all", "C:/x/bin/install.exe --all"] {
        assert!(fb(a, &["--all"], c).1, "{c}");
    }
    assert!(fb_ps(a, &["--all"], "& \"C:\\x\\bin\\install.exe\" --all").1);
    assert!(!fb(a, &["--all"], "install -m 644 a b --all").1, "coreutils install is another program");
    // An anchor written without .exe reaches both spellings, as before.
    assert!(fb(&["install"], &["--all"], "install --all").1);
    assert!(fb(&["install"], &["--all"], "./install.exe --all").1);
}

#[test]
fn a_pure_help_or_version_call_is_never_refused_by_requires() {
    let a = &["install.exe"];
    let w = "install.exe";
    let r = &["--db"];
    for c in ["\"C:/x/bin/install.exe\" --help", "install.exe -h", "install.exe --version", "install.exe -V", "install.exe /?"] {
        assert!(!rq(a, w, r, c).1, "a pure help call must pass: {c}");
    }
    assert!(rq(a, w, r, "install.exe --help --all").1, "not a pure help call");
    assert!(rq(a, w, r, "install.exe").1, "the bare call is not a help call");
    assert!(rq(a, w, r, "\"C:/x/bin/install.exe\" --register-root").1);
    // A forbidden check is not exempted.
    assert!(fb(&["install.exe"], &["install.exe"], "install.exe --help").1);
}

#[test]
fn a_git_call_carries_its_own_folder_and_a_cd_chain_is_followed() {
    let dirs = |c: &str| {
        segments(c, Dialect::Sh).into_iter().map(|s| (s.head_text(), s.dir.as_deref().map(str::to_string))).collect::<Vec<_>>()
    };
    let got = dirs("cd ../B && git -C ../A push");
    assert_eq!(got[1], ("git push".to_string(), Some("../B/../A".to_string())));
    let got = dirs("cd a && cd b && ls");
    assert_eq!(got[2].1.as_deref(), Some("a/b"));
    let got = dirs("cd ../B && GIT_DIR=../A/.git git push");
    assert_eq!(got[1].1.as_deref(), Some("../B/../A/.git"));
    // The repository decides, never the work tree (`--work-tree` names only
    // where the files are).
    let got = dirs("git --work-tree=/srv/x --git-dir /srv/x/.git status");
    assert_eq!(got[0].1.as_deref(), Some("/srv/x/.git"));
    let got = dirs("cd /tmp && git push");
    assert_eq!(got[1].1.as_deref(), Some("/tmp"));
    let got = dirs("pushd /x && popd && ls");
    assert_eq!(got[2].1, None, "popd returns to where pushd started");
    let got = dirs("cd && ls");
    assert_eq!(got[1].1.as_deref(), Some("~"), "a bare cd is the home folder, which no reader resolves");
}

#[test]
fn an_ssh_command_is_remote_and_a_local_wrapper_is_not() {
    let remote = |c: &str| segments(c, Dialect::Sh).into_iter().map(|s| (s.head_text(), s.remote)).collect::<Vec<_>>();
    let got = remote("ssh nas 'rm /srv/x' && bash -lc 'rm y'");
    assert!(got.contains(&("rm /srv/x".to_string(), true)), "{got:?}");
    assert!(got.contains(&("rm y".to_string(), false)), "{got:?}");
}

#[test]
fn cmd_and_invoke_expression_are_descended() {
    assert!(fb_ps(&["docker exec"], PROD, "cmd /c \"docker exec acme-prod node -e 1\"").1);
    assert!(fb_ps(&["docker exec"], PROD, "Invoke-Expression \"docker exec acme-prod node -e 1\"").1);
    assert!(fb_ps(&["docker exec"], PROD, "iex 'docker exec acme-prod node -e 1'").1);
    assert!(!fb_ps(&["docker exec"], PROD, "cmd /c \"echo docker exec acme-prod node -e 1\"").1);
}

/// The hook runs this on every shell call, on text nobody controls: no input,
/// however malformed (unclosed quotes, stray parentheses, heredocs with no
/// end, multi-byte characters next to operators), may make it panic. A fixed
/// pseudo-random corpus over the characters the scanner treats specially.
#[test]
fn the_scanner_never_panics_on_arbitrary_text() {
    let alphabet: Vec<char> = "\"'`$(){}[]<>|&;#\n\\ @-=*~!/:.acdegilnoprstu0129\u{e9}\u{2014}".chars().collect();
    let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    for _ in 0..20_000 {
        let len = (next() % 40) as usize;
        let text: String = (0..len).map(|_| alphabet[(next() % alphabet.len() as u64) as usize]).collect();
        for d in [Dialect::Sh, Dialect::PowerShell] {
            for seg in segments(&text, d) {
                let _ = anchor_reaches("git push", &seg);
                let _ = anchor_reaches("a.exe x", &seg);
                let _ = literal_in(&seg, "--force");
                let _ = trigger_in_segment(&seg, "/thor*");
                let _ = is_pure_help_call("git", &seg);
            }
        }
        let _ = literal_at_boundary(&text, "a");
        let _ = trigger_in(&text, "e*");
    }
}

#[test]
fn a_redirect_is_not_a_word_and_is_recorded_with_its_producer() {
    let segs = segments("echo x > a.txt; : > b.txt; cat c >> d.txt; > e.txt; cmd 2>/dev/null 2>&1", Dialect::Sh);
    let shown: Vec<(Vec<String>, Vec<Redirect>)> = segs.iter().map(|s| (s.words.clone(), s.redirects.clone())).collect();
    let r = |t: &str, append: bool, producer: bool| Redirect { target: t.to_string(), append, producer };
    assert_eq!(shown[0], (s(&["echo", "x"]), vec![r("a.txt", false, true)]));
    assert_eq!(shown[1], (s(&[":"]), vec![r("b.txt", false, false)]));
    assert_eq!(shown[2], (s(&["cat", "c"]), vec![r("d.txt", true, true)]));
    assert_eq!(shown[3], (Vec::<String>::new(), vec![r("e.txt", false, false)]));
    assert_eq!(shown[4], (s(&["cmd"]), vec![r("/dev/null", false, true)]), "a file descriptor is never a word or a target");
}

// ------------------------------------------------------------ where reading stops

/// The measured exit (2026-09-28): two thousand `$(` after a forbidden
/// command overflowed the hook's stack, the hook crashed, and a crashed hook
/// lets the call through. Nesting is now followed `MAX_NEST` deep, the call
/// is then also read flat, and the command beside the nest is refused
/// before it or after it, in every nesting form.
#[test]
fn a_deep_nest_never_crashes_and_the_command_beside_it_is_still_refused() {
    let bad = "docker exec acme-prod node -e 1";
    let n = 20_000;
    for (open, close) in [("$(", ")"), ("<(", ")"), ("\"$(", ")\"")] {
        let nest = format!(": {}{}", open.repeat(n), close.repeat(n));
        for cmd in [format!("{bad}; {nest}"), format!("{nest}; {bad}")] {
            assert!(fb(&["docker exec"], PROD, &cmd).1, "must be refused: {open} nest, {} bytes", cmd.len());
        }
    }
    let ps = format!("{bad}; {}{}", "$(".repeat(n), ")".repeat(n));
    assert!(fb_ps(&["docker exec"], PROD, &ps).1, "PowerShell subexpressions take the same road");
}

/// A command nested deeper than the reader descends is no way past a rule:
/// the call is then read flat as well, and the flat reading finds the
/// command wherever it stands. One level less is read in full, and gets no
/// flat reading at all.
#[test]
fn a_command_nested_past_the_depth_cap_is_read_flat_and_refused() {
    let bad = "docker exec acme-prod node -e 1";
    let deep = format!("x={}{bad}{}", "$(".repeat(MAX_DEPTH), ")".repeat(MAX_DEPTH));
    assert!(fb(&["docker exec"], PROD, &deep).1, "{deep}");
    assert!(segments(&deep, Dialect::Sh).iter().any(|s| s.flat), "read flat as well");
    let hops = format!("{}{bad}", "ssh h ".repeat(MAX_DEPTH + 2));
    assert!(fb(&["docker exec"], PROD, &hops).1, "{hops}");
    let inside = format!("x={}{bad}{}", "$(".repeat(MAX_DEPTH - 1), ")".repeat(MAX_DEPTH - 1));
    let segs = segments(&inside, Dialect::Sh);
    assert!(segs.iter().all(|s| !s.flat), "read in full: no flat reading");
    assert!(segs.iter().any(|s| s.head_text() == bad && s.depth == MAX_DEPTH - 1), "{segs:?}");
}

/// Only a call that could not be read in full is read flat: an ordinary
/// call is never refused on a word it merely mentions.
#[test]
fn an_ordinary_call_is_never_read_flat() {
    for cmd in ["echo \"docker exec acme-prod node -e\"", "git commit -m \"docker exec acme-prod node -e 1\""] {
        assert!(segments(cmd, Dialect::Sh).iter().all(|s| !s.flat), "{cmd}");
        assert!(!fb(&["docker exec"], PROD, cmd).1, "{cmd}");
    }
}

/// Reading costs time linear in the call's length. Before, twenty thousand
/// `$(:)` in a row took 15 seconds in the hook (each `$(` copied the whole
/// call), a line of unclosed `if`s searched the rest of the line once per
/// `if`, a long pipeline copied its whole text into every one of its
/// segments, and a chain of `cd a` built ever longer folder names - and a
/// call that runs the hook past its timeout goes through as surely as a
/// crash. Each shape here is about 160 KB; the bound is far above what a
/// linear reading takes, even in a debug build.
#[test]
fn reading_a_crafted_call_costs_time_linear_in_its_length() {
    let bad = "\ndocker exec acme-prod node -e 1";
    let shapes = [
        ("$(:)".repeat(40_000), Dialect::Sh),
        ("echo a | ".repeat(18_000), Dialect::Sh),
        ("if a; ".repeat(27_000), Dialect::Sh),
        ("for x in a; do ".repeat(10_000), Dialect::Sh),
        ("cd a && ".repeat(20_000), Dialect::Sh),
        ("cat <<E\nx\nE\n".repeat(14_000), Dialect::Sh),
        ("foreach ($x in $y) { ".repeat(8_000), Dialect::PowerShell),
        ("Get-Item a | ".repeat(12_000), Dialect::PowerShell),
    ];
    for (shape, d) in shapes {
        let cmd = format!("{shape}{bad}");
        let started = std::time::Instant::now();
        let refused = fb_in(&["docker exec"], PROD, &cmd, d).1;
        let spent = started.elapsed();
        assert!(refused, "the command after {:?}... must still be refused", &shape[..12]);
        assert!(spent < std::time::Duration::from_secs(5), "{:?}... took {spent:?}", &shape[..12]);
    }
}

/// A line of unclosed `if`s opens one group per `if`, all of them closing at
/// the line's end. Every command used to search the whole stack of open
/// groups for one closing right there, which cost the hook 12 seconds on a
/// 600 KB line; a closer now only searches when a group of its kind is open.
/// About 900 KB here; the bound is far above a linear reading, debug build
/// included.
#[test]
fn a_line_of_unclosed_openers_costs_no_search_per_command() {
    let cmd = format!("{}\ndocker exec acme-prod node -e 1", "if a; ".repeat(150_000));
    let started = std::time::Instant::now();
    assert!(fb(&["docker exec"], PROD, &cmd).1, "the command after the line must still be refused");
    let bound = std::time::Duration::from_secs(if cfg!(debug_assertions) { 60 } else { 5 });
    assert!(started.elapsed() < bound, "took {:?}", started.elapsed());
}

/// Every line of a heredoc body used to be found by walking the call's
/// characters from the very start, so a call of many small heredocs cost
/// the square of its length. About 720 KB here; the bound is far above a
/// linear reading, debug build included.
#[test]
fn a_call_of_many_heredocs_is_read_in_one_pass() {
    let cmd = format!("{}docker exec acme-prod node -e 1", "cat <<E\nx\nE\n".repeat(60_000));
    let started = std::time::Instant::now();
    assert!(fb(&["docker exec"], PROD, &cmd).1, "the command after the heredocs must still be refused");
    let bound = std::time::Duration::from_secs(if cfg!(debug_assertions) { 60 } else { 5 });
    assert!(started.elapsed() < bound, "took {:?}", started.elapsed());
}

// ------------------------------------------------------------ the folder

/// The folder of a call's last `git push` segment, certain and as if every
/// `cd` took effect.
fn push_dirs(c: &str) -> (Option<String>, Option<String>) {
    let segs = segments(c, Dialect::Sh);
    let s = segs.iter().rev().find(|s| s.head_text().starts_with("git push")).expect("a git push segment");
    (s.dir.as_deref().map(str::to_string), s.any_cd_dir.as_deref().map(str::to_string))
}

/// A `cd` moves the folder of what follows only when it certainly ran and
/// took effect in this shell. The measured exit (2026-09-28): `(cd ../beta)
/// && git push` pushed from the session's folder but was judged as run in
/// beta, so the session's push ban only advised.
#[test]
fn a_cd_that_cannot_take_effect_never_moves_what_follows() {
    let beta = Some("../beta".to_string());
    for c in [
        "cd ../beta && git push",
        "cd ../beta; git push",
        "cd ../beta\ngit push",
        "(cd ../beta && git push)",
        "cd ../beta || exit 1; git push",
    ] {
        assert_eq!(push_dirs(c), (beta.clone(), beta.clone()), "certain: {c}");
    }
    // A subshell of its own: no effect after it at all.
    for c in [
        "(cd ../beta) && git push",
        "(cd ../beta && ls); git push origin main",
        "cd ../beta | cat; git push",
        "cd ../beta & git push",
    ] {
        assert_eq!(push_dirs(c), (None, None), "no effect: {c}");
    }
    // It may not have run: the folder stays, and a write is judged in both.
    for c in [
        "false && cd ../beta; git push",
        "false && cd ../beta || git push",
        "if false; then cd ../beta; fi; git push",
        "for d in a; do cd ../beta; done; git push",
        "{ cd ../beta; }; git push",
    ] {
        assert_eq!(push_dirs(c), (None, beta.clone()), "may not have run: {c}");
    }
    // In PowerShell `( )` is no subshell: the cd holds after it.
    let segs = segments("(cd ../beta); git push", Dialect::PowerShell);
    assert_eq!(segs.last().map(|s| s.dir.as_deref().map(str::to_string)), Some(beta));
}

/// The place a `cd` names travels with the commands that run there - into a
/// wrapper too - and ends with the subshell it was typed in (the measured
/// false block: `(cd //nas-share/... && ls) && git reset --hard` was refused
/// by a place rule although the reset runs outside the share).
#[test]
fn the_place_a_cd_names_travels_into_wrappers_and_ends_with_its_subshell() {
    let lits = &["acme-prod", "nas-share"];
    for c in [
        "cd //nas-share/docker/acme-prod && bash -c 'git reset --hard origin/main'",
        "cd //nas-share/docker/acme-prod && x=$(git reset --hard origin/main)",
        "cd //nas-share/docker/acme-prod && bash <<'EOF'\ngit reset --hard origin/main\nEOF",
        "cd //nas-share/docker/acme-prod\ngit reset --hard origin/main",
    ] {
        assert!(fb(&["git reset"], lits, c).1, "must be refused: {c}");
    }
    for c in [
        "(cd //nas-share/docker/acme-prod && ls) && git reset --hard origin/main",
        "cd //nas-share/docker/acme-prod | cat; git reset --hard origin/main",
        "cd //nas-share/docker/acme-prod && ssh nas 'git reset --hard origin/main'",
    ] {
        assert!(!fb(&["git reset"], lits, c).1, "must pass: {c}");
    }
}

/// sh's `$'...'` quoting: a backslash escapes the quote inside it. Read as
/// two plain quotes, `$'it\'s'` opened a string that is not there and hid
/// the command after it.
#[test]
fn ansi_c_quoting_hides_no_command() {
    assert!(fb(&["docker exec"], PROD, "echo $'it\\'s'; docker exec acme-prod node -e 1; echo 'x'").1);
    assert!(!fb(&["docker exec"], PROD, "echo $'docker exec acme-prod node -e 1\\n'").1);
    assert_eq!(segments("echo $'a\\'b' c", Dialect::Sh)[0].words, s(&["echo", "a'b", "c"]));
}

/// A chain of `cd`s is followed to `MAX_DIR_TEXT` bytes; past that its
/// folder is one no reader resolves (so its segments keep the session's
/// project), and no chain builds ever longer folder names.
#[test]
fn a_cd_chain_past_the_length_cap_names_no_folder() {
    let c = format!("{}ls", "cd abcdefgh && ".repeat(MAX_DIR_TEXT / 8));
    let segs = segments(&c, Dialect::Sh);
    let last = segs.last().expect("segments");
    assert_eq!(last.head_text(), "ls");
    assert!(last.dir.as_deref().is_some_and(|d| d.contains('$')), "{:?}", last.dir);
    assert_eq!(segments("cd a && cd b && ls", Dialect::Sh)[2].dir.as_deref(), Some("a/b"));
}

// ------------------------------------------------------------ a cd the shell refuses

/// The folder of a call's last `git push` segment in `d`, certain and as if
/// every `cd` took effect.
fn push_dirs_in(c: &str, d: Dialect) -> (Option<String>, Option<String>) {
    let segs = segments(c, d);
    let s = segs.iter().rev().find(|s| s.head_text().starts_with("git push")).expect("a git push segment");
    (s.dir.as_deref().map(str::to_string), s.any_cd_dir.as_deref().map(str::to_string))
}

/// A `cd` the shell refuses moves nothing, as in the shell itself - each
/// form measured against Git Bash 5.3 and Windows PowerShell 5.1
/// (2026-09-28). The measured exit: `cd ../beta extra; git push` was judged
/// as run in beta, while bash refused the `cd` ("too many arguments") and
/// pushed from the session's folder.
#[test]
fn a_cd_the_shell_refuses_moves_nothing() {
    let beta = Some("../beta".to_string());
    for c in [
        "cd ../beta extra; git push",
        "cd -x ../beta; git push",
        "cd -Lx ../beta; git push",
        "cd -@ ../beta; git push",
        "cd ../beta --; git push",
        "cd ''; git push",
        "pushd ../beta extra; git push",
        "pushd -x ../beta; git push",
        "pushd +x; git push",
        "pushd -n; git push",
        "pushd ''; git push",
        // Not the shell's own cd at all: bash has no chdir, sl or
        // Set-Location, and a prefix or a folder runs a program of that name.
        "chdir ../beta; git push",
        "sl ../beta; git push",
        "Set-Location ../beta; git push",
        "sudo cd ../beta; git push",
        "env cd ../beta; git push",
        "nohup cd ../beta; git push",
        "/usr/bin/cd ../beta; git push",
    ] {
        assert_eq!(push_dirs_in(c, Dialect::Sh), (None, None), "moves nothing: {c}");
    }
    for c in [
        "cd ../beta; git push",
        "cd -P ../beta; git push",
        "cd -LP ../beta; git push",
        "cd -e ../beta; git push",
        "cd -L -P ../beta; git push",
        "cd -- ../beta; git push",
        "command cd ../beta; git push",
        "builtin cd ../beta; git push",
        "X=1 cd ../beta; git push",
        "pushd ../beta; git push",
        "pushd -- ../beta; git push",
    ] {
        assert_eq!(push_dirs_in(c, Dialect::Sh), (beta.clone(), beta.clone()), "moves: {c}");
    }
    for c in [
        "Set-Location ../beta extra; git push",
        "Set-Location -x ../beta; git push",
        "Set-Location -Pa ../beta; git push",
        "Set-Location -P ../beta; git push",
        "Set-Location -e Stop ../beta; git push",
        "Set-Location -StackName s ../beta; git push",
        "Set-Location -UseTransaction ../beta; git push",
        "cd -Path ../beta extra; git push",
        "cd -LiteralPath ../beta -Path ../alpha; git push",
        "pushd ../beta extra; git push",
        "Set-Location -Path; git push",
    ] {
        assert_eq!(push_dirs_in(c, Dialect::PowerShell), (None, None), "moves nothing: {c}");
    }
    for c in [
        "Set-Location ../beta; git push",
        "Set-Location -Path ../beta; git push",
        "Set-Location -Pat ../beta; git push",
        "Set-Location -Li ../beta; git push",
        "Set-Location -PSPath ../beta; git push",
        "Set-Location -PassThru ../beta; git push",
        "Set-Location -Verbose ../beta; git push",
        "Set-Location -Verb ../beta; git push",
        "Set-Location -ea Stop ../beta; git push",
        "Set-Location -ErrorAction:Stop ../beta; git push",
        "Set-Location -OutB 1 ../beta; git push",
        "Set-Location -Verbose:$false ../beta; git push",
        "Set-Location -Path:../beta; git push",
        "Set-Location -- ../beta; git push",
        "Set-Location ../beta --; git push",
        "cd ../beta -PassThru; git push",
        "chdir ../beta; git push",
        "sl ../beta; git push",
        "Microsoft.PowerShell.Management\\Set-Location ../beta; git push",
        "pushd ../beta; git push",
    ] {
        assert_eq!(push_dirs_in(c, Dialect::PowerShell), (beta.clone(), beta.clone()), "moves: {c}");
    }
}

/// The directory stack, as bash and PowerShell keep it: `pushd -n` and
/// `popd -n` leave the folder, a bare `pushd` swaps in sh and saves in
/// PowerShell, and a rotation or a named stack leaves a folder no reader
/// resolves - never one this reader only guessed.
#[test]
fn the_directory_stack_is_followed_as_the_shell_keeps_it() {
    let beta = Some("../beta".to_string());
    let unknown = Some(UNKNOWN_DIR.to_string());
    let sh = |c: &str| push_dirs_in(c, Dialect::Sh).0;
    assert_eq!(sh("pushd ../beta; popd -n; git push"), beta, "popd -n keeps the folder");
    assert_eq!(sh("pushd ../beta; popd extra; git push"), beta, "a popd bash refuses keeps the folder");
    assert_eq!(sh("pushd ../beta; popd -- x; git push"), None, "after -- bash reads nothing more");
    assert_eq!(sh("pushd -n ../beta; git push"), None, "pushd -n does not move");
    assert_eq!(sh("cd ../beta; pushd .; pushd -n ../alpha; popd; git push"), unknown, "what pushd -n saved is no known folder");
    assert_eq!(sh("pushd ../beta; pushd; git push"), None, "a bare pushd swaps back");
    assert_eq!(sh("pushd ../beta; pushd; popd; git push"), beta);
    assert_eq!(sh("pushd ../beta; pushd +1; git push"), unknown, "a rotation");
    assert_eq!(sh("pushd ../beta; pushd +1; popd; git push"), unknown, "and after it");
    assert_eq!(sh("pushd ../beta; popd +0; git push"), unknown);
    let ps = |c: &str| push_dirs_in(c, Dialect::PowerShell).0;
    assert_eq!(ps("cd ../beta; pushd; cd ../gamma; popd; git push"), beta, "a bare pushd saves the folder");
    assert_eq!(ps("pushd ../beta; popd extra; git push"), beta, "a popd PowerShell refuses keeps the folder");
    assert_eq!(ps("pushd ../beta; popd -StackName s; git push"), unknown, "a named stack");
    assert_eq!(ps("pushd -StackName s ../beta; git push"), unknown, "a named stack");
    assert_eq!(ps("pushd ../beta; Pop-Location; git push"), None);
}

/// The repository git acts on decides the folder, never its work tree:
/// `git --work-tree=../beta push` pushes the repository of the folder it
/// runs in (`git rev-parse --git-dir` says so, measured 2026-09-28).
#[test]
fn the_repository_decides_the_folder_never_the_work_tree() {
    let dir = |c: &str| push_dirs_in(c, Dialect::Sh).0;
    assert_eq!(dir("git --work-tree=../beta push"), None);
    assert_eq!(dir("git --work-tree ../beta push"), None);
    assert_eq!(dir("GIT_WORK_TREE=../beta git push"), None);
    assert_eq!(dir("cd ../beta && git --work-tree=../alpha push").as_deref(), Some("../beta"));
    assert_eq!(dir("git --git-dir=.git --work-tree=../beta push").as_deref(), Some(".git"));
    assert_eq!(dir("git -C ../beta --work-tree=../gamma push").as_deref(), Some("../beta"));
    assert_eq!(dir("GIT_DIR=../beta/.git GIT_WORK_TREE=. git push").as_deref(), Some("../beta/.git"));
    assert_eq!(dir("GIT_DIR=../gamma/.git git --git-dir=../beta/.git push").as_deref(), Some("../beta/.git"), "the option wins");
    let segs = segments("git --work-tree=../beta --git-dir ../beta/.git push --force", Dialect::Sh);
    assert_eq!(segs[0].head_text(), "git push --force", "git's own options still leave the head");
}

// ------------------------------------------------------------ arithmetic

/// Arithmetic is read as bash reads it (each form measured against Git Bash
/// 5.3, 2026-09-28): a `<<` in `((...))`, `$[...]`, `for ((...))` or
/// `$((...))` is a shift, never a heredoc, so every line after it is still a
/// command; a `$(...)` or backtick command inside arithmetic still runs;
/// and `((cmd) )` and `$((cmd) )` are commands, as bash runs them.
#[test]
fn arithmetic_hides_no_command() {
    let bad = "docker exec acme-prod node -e 1";
    for c in [
        format!("(( x = 1 << 2 ))\n{bad}"),
        format!("((x=1<<2)); {bad}"),
        format!("echo $[1<<2]\n{bad}"),
        format!("for ((i=0; i<<1; i++)); do :; done\n{bad}"),
        format!("if ((1<<2)); then :; fi\n{bad}"),
        format!("while (( i << 1 )); do :; done\n{bad}"),
        format!("echo $((1<<2))\n{bad}"),
        format!("echo \"$((1<<2))\"\n{bad}"),
        format!("(( $({bad}) ))"),
        format!("(( x = $({bad}) << 2 ))"),
        format!("echo $(( $({bad}) + 1 ))"),
        format!("echo $[ $({bad}) + 1 ]"),
        format!("(( `{bad}` ))"),
        format!("(( \"$({bad})\" ))"),
        format!("echo $(( $(( $({bad}) )) ))"),
        format!("(({bad}) )"),
        format!("echo $(({bad}) )"),
    ] {
        assert!(fb(&["docker exec"], PROD, &c).1, "must be refused: {c:?}");
    }
    // Arithmetic is data: what it merely holds runs nowhere, and bash
    // refuses `((docker exec ...))` as an arithmetic error.
    for c in [format!("(({bad}))"), format!("echo $(({bad}))"), format!("echo $[{bad}]"), "(( x << 2 ))".to_string()] {
        assert!(!fb(&["docker exec"], PROD, &c).1, "must pass: {c:?}");
    }
    assert_eq!(segments("(( x = 1 << 2 ))\nls", Dialect::Sh).last().map(|s| s.head_text()), Some("ls".to_string()));
}

/// Deciding whether `((` is arithmetic reads ahead to its matching `)`, and
/// a nested command read ahead is met again after it: each is found once per
/// text, so a run of `((` or a nest of `$((` never reads the same text again
/// at every step. About 200 KB each; the bound is far above a linear
/// reading, debug build included.
#[test]
fn deciding_what_is_arithmetic_costs_time_linear_in_the_call() {
    let bad = "\ndocker exec acme-prod node -e 1";
    let shapes = [
        format!("{}x{}", "(".repeat(60_000), " )".repeat(60_000)),
        format!("{}x{}", "((".repeat(40_000), ") ".repeat(80_000)),
        "((x) ".repeat(40_000),
        format!(": {}{}", "$((".repeat(30_000), ") )".repeat(30_000)),
        format!(": {}{}", "$( (( ".repeat(20_000), " )) )".repeat(20_000)),
        format!(": {}", "$[".repeat(60_000)),
    ];
    for shape in shapes {
        let cmd = format!("{shape}{bad}");
        let started = std::time::Instant::now();
        let refused = fb(&["docker exec"], PROD, &cmd).1;
        let spent = started.elapsed();
        assert!(refused, "the command after {:?}... must still be refused", &shape[..12]);
        let bound = std::time::Duration::from_secs(if cfg!(debug_assertions) { 60 } else { 5 });
        assert!(spent < bound, "{:?}... took {spent:?}", &shape[..12]);
    }
}

// ------------------------------------------------------------ memory

/// A folder is shared, never copied: every segment in the same folder, and
/// every group saved open around it, holds one copy of it. The measured
/// exit (2026-09-28): a 200 KB `cd` before ten thousand open `(` cost the
/// hook 1.9 GB (a 1 KiB `pushd` stack 4 GB), and under a memory cap the
/// hook aborted with no decision - a crashed hook lets the call through.
#[test]
fn a_folder_is_shared_by_every_segment_never_copied() {
    let long = format!("cd /{}", "a".repeat(900));
    let segs = segments(&format!("{long}; ( : ); {{ :; }}; :; x=$(:)"), Dialect::Sh);
    let dirs: Vec<&std::sync::Arc<str>> = segs.iter().filter(|s| s.head_text() == ":").filter_map(|s| s.dir.as_ref()).collect();
    assert_eq!(dirs.len(), 4, "{segs:?}");
    assert!(dirs.iter().all(|d| std::sync::Arc::ptr_eq(d, dirs[0])), "one copy of the folder for every segment");
    let any: Vec<&std::sync::Arc<str>> = segs.iter().filter_map(|s| s.any_cd_dir.as_ref()).collect();
    assert!(any.iter().all(|d| std::sync::Arc::ptr_eq(d, dirs[0])), "the any-cd folder is the same copy");
}

/// A stack of three hundred thousand `pushd`s is dropped frame by frame:
/// dropped by recursion it overflowed the stack, and a crashed hook lets
/// the call through.
#[test]
fn a_deep_directory_stack_never_crashes_the_reader() {
    let cmd = format!("{}docker exec acme-prod node -e 1", "pushd /x; ".repeat(300_000));
    assert!(fb(&["docker exec"], PROD, &cmd).1);
}

// ------------------------------------------------ PowerShell paren groups

/// Whether a requires rule on `Start-Process` refuses `cmd`, a PowerShell
/// call, for want of one of `req`.
fn ps_requires_refuses(req: &[&str], cmd: &str) -> bool {
    let items = rule(&["Start-Process"], Check::Requires { when: "Start-Process".to_string(), required: s(req) });
    absent_guard::find_requires_violation(&items, "PowerShell", Some(cmd), cmd).is_some()
}

fn ps_words(cmd: &str) -> Vec<String> {
    segments(cmd, Dialect::PowerShell).into_iter().map(|g| g.words.join(" ")).collect()
}

/// The measured false refusal (2026-10-03, 33 and 35 of 289 real
/// `Start-Process` calls): the `)` that closed `@( ... )` ended the command,
/// so the flags after the array stood in another segment and two rules that
/// require them refused a call that carries exactly what they ask for.
#[test]
fn a_flag_after_a_powershell_array_is_still_a_flag_of_its_command() {
    let hidden = &["-WindowStyle", "-NoNewWindow"];
    let wait = &["-Wait"];
    for cmd in [
        "Start-Process -FilePath powershell.exe -ArgumentList @('-NoProfile','-File','x.ps1') -WindowStyle Hidden -Wait",
        "Start-Process -FilePath powershell.exe -ArgumentList @('-NoProfile', '-File', 'x.ps1') -WindowStyle Hidden -Wait",
        "Start-Process -FilePath powershell.exe -ArgumentList @( '-File', 'x.ps1' ) -WindowStyle Hidden -Wait",
        "Start-Process -WindowStyle Hidden -Wait -FilePath powershell.exe -ArgumentList @('-File','x.ps1')",
        "Start-Process -FilePath p.exe -ArgumentList \"-File x.ps1\" -WindowStyle Hidden -Wait",
        "Start-Process -FilePath p.exe -ArgumentList \"a $(Get-Date -Format yyyy)\" -WindowStyle Hidden -Wait",
        "Start-Process -FilePath p.exe -ArgumentList $(Get-Item a | Where-Object { $_ } ) -WindowStyle Hidden -Wait",
        "Start-Process -FilePath p.exe -ArgumentList @(Get-ChildItem | ForEach-Object { $_.Name }) -WindowStyle Hidden -Wait",
        "Start-Process -FilePath p.exe -ArgumentList $args.Join(',') -WindowStyle Hidden -Wait",
        "Start-Process -FilePath p.exe -ArgumentList @('a', @('b', ('c' + 'd'))) -WindowStyle Hidden -Wait",
        "Start-Process -FilePath p.exe -ArgumentList @('a)b', \"c(d\", 'e') -WindowStyle Hidden -Wait",
        "Start-Process -FilePath p.exe -ArgumentList $($x.Trim()) -WindowStyle Hidden -Wait",
        "$p = Start-Process -FilePath p.exe -ArgumentList @('a') -NoNewWindow -Wait -PassThru",
    ] {
        let has_hidden = cmd.contains("-WindowStyle") || cmd.contains("-NoNewWindow");
        assert_eq!(ps_requires_refuses(hidden, cmd), !has_hidden, "-WindowStyle rule: {cmd}");
        assert!(!ps_requires_refuses(wait, cmd), "-Wait rule: {cmd}");
    }
}

/// The other side of it: a flag that is not on the command is still missing,
/// whether the array is there or not, and a flag after a real `;` or a real
/// `( ... )` group belongs to another command.
#[test]
fn a_missing_flag_is_still_refused_with_an_array_in_the_call() {
    for cmd in [
        "Start-Process -FilePath p.exe -ArgumentList @('-File','x.ps1')",
        "Start-Process -FilePath p.exe -ArgumentList @('-File','x.ps1'); Get-Item x -Wait",
        "Start-Process -FilePath p.exe -ArgumentList $a.Join(',')",
        "Start-Process -FilePath p.exe; (Get-Item x; Get-Item y) -Wait",
    ] {
        assert!(ps_requires_refuses(&["-Wait"], cmd), "must be refused: {cmd}");
    }
}

/// A group in a word is part of the word, a group that starts a statement is
/// still a group of its own, and what a group runs is still read.
#[test]
fn a_powershell_paren_group_is_a_word_only_when_a_word_is_open() {
    assert_eq!(ps_words("(Get-Foo; Get-Bar) -Wait"), ["Get-Foo", "Get-Bar", "-Wait"]);
    assert_eq!(ps_words("Start-Process p @('a','b') -Wait"), ["Start-Process p @('a','b') -Wait", "a,b"]);
    let nested = ps_words("Start-Process p -ArgumentList @('a', ('b')) -Wait");
    assert_eq!(nested[0], "Start-Process p -ArgumentList @('a', ('b')) -Wait", "{nested:?}");
    // What the group runs is its own segment, so a rule on it still reaches it.
    for cmd in [
        "Write-Host @(docker exec acme-prod node -e 1) -Wait",
        "Write-Host x.Add(docker exec acme-prod node -e 1) -Wait",
        "Start-Process p -ArgumentList @('a', $(docker exec acme-prod node -e 1)) -Wait",
    ] {
        assert!(fb_ps(&["docker exec"], PROD, cmd).1, "must be refused: {cmd}");
    }
    // Bash is untouched: there a `)` always ends the command.
    let sh: Vec<String> = segments("echo a (b) -Wait", Dialect::Sh).into_iter().map(|g| g.words.join(" ")).collect();
    assert_eq!(sh, ["echo a", "b", "-Wait"], "{sh:?}");
}

/// A deep nest of word-attached groups is read to the depth cap and then
/// counted, like `$(`: never a crash, and the command beside it still read.
#[test]
fn a_deep_nest_of_powershell_word_groups_never_crashes() {
    let bad = "docker exec acme-prod node -e 1";
    let n = 20_000;
    let nest = format!("x {}{}", "f(".repeat(n), ")".repeat(n));
    for cmd in [format!("{bad}; {nest}"), format!("{nest}; {bad}")] {
        assert!(fb_ps(&["docker exec"], PROD, &cmd).1, "must be refused: {} bytes", cmd.len());
    }
}
