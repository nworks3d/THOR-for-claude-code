//! The door, end to end through the real compiled `serve hook`: a command
//! tooth reaches every segment of a call, reads its literal in that segment's
//! unit on token boundaries, and judges each segment under the project it
//! runs in.
//!
//! THE DEFECT THIS CLOSES, measured on the owner's own transcripts
//! (2026-09-28): a rule bound to `docker exec` refused only the bare
//! `docker exec <container> node -e ...`, and let the same call through
//! after `cd /srv &&`, behind `ssh host "..."`, in `$(...)`, in a loop or
//! under `sudo -u root` - about half of the real calls it was written for.
//! Commit trailers fed on a heredoc passed every attribution rule. And a
//! literal matched anywhere, so `--force` refused `--force-with-lease`.
//!
//! RED = the hook prints a block naming the rule and the literal; GREEN = no
//! decision on stdout. Every fixture rule is stored through the real write
//! gate. The hook runs with `--no-record`, so no case leaves a marker that
//! changes the wording of the next.

use model::item::{Binding, Check, Item, Kind, Severity, TargetKind};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn rule(id: &str, anchors: &[&str], project: Option<&str>, check: Check) -> Item {
    Item {
        id: id.to_string(),
        kind: Kind::Rule,
        text: format!("door fixture rule {id}"),
        bindings: anchors.iter().map(|a| Binding::Target { kind: TargetKind::Command, value: a.to_string() }).collect(),
        severity: Some(Severity::Irreversible),
        project: project.map(str::to_string),
        tags: vec![],
        expires: None,
        key: None,
        falsifier: Some(format!("the door fixture rule {id} turns out wrong")),
        check: Some(check),
    }
}

fn forbidden(literals: &[&str]) -> Check {
    Check::Forbidden { literals: literals.iter().map(|l| l.to_string()).collect() }
}

fn requires(when: &str, required: &[&str]) -> Check {
    Check::Requires { when: when.to_string(), required: required.iter().map(|r| r.to_string()).collect() }
}

/// A checkout folder of `project`: a `.thor-project` marker and an empty
/// `.git` folder (never a real repository: no git runs here).
fn checkout(base: &Path, project: &str) -> PathBuf {
    let dir = base.join(project);
    std::fs::create_dir_all(dir.join(".git")).unwrap();
    std::fs::write(dir.join(".thor-project"), format!("{project}\n")).unwrap();
    dir
}

struct Fixture {
    _tmp: tempfile::TempDir,
    db: PathBuf,
    alpha: PathBuf,
    beta: PathBuf,
}

/// A store holding `items` (through the real write gate) beside two
/// checkouts, `alpha` and `beta`.
fn fixture(items: &[Item]) -> Fixture {
    let tmp = tempfile::Builder::new().prefix("door").tempdir().unwrap();
    let alpha = checkout(tmp.path(), "alpha");
    let beta = checkout(tmp.path(), "beta");
    let store_dir = tmp.path().join("store");
    std::fs::create_dir_all(&store_dir).unwrap();
    let db = store_dir.join("thor.db");
    let mut store = thor_core::event_store::EventStore::new(&db).unwrap();
    for item in items {
        model::store::declare(&mut store, "door", "door", "door-fixture", &named(item))
            .unwrap_or_else(|e| panic!("the write gate refused fixture {}: {e:?}", item.id));
    }
    Fixture { _tmp: tmp, db, alpha, beta }
}

/// Run the hook on one call; returns stdout. The payload goes through a file,
/// never inline.
fn hook(fx: &Fixture, cwd: &Path, tool: &str, command: &str) -> String {
    let payload = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "session_id": "door-session",
        "cwd": cwd.to_string_lossy(),
        "tool_name": tool,
        "tool_input": {"command": command},
    });
    let file = fx.db.with_file_name(format!("payload-{}.json", std::process::id()));
    std::fs::write(&file, payload.to_string()).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_serve"))
        .arg("--db")
        .arg(&fx.db)
        .arg("hook")
        .arg("--no-record")
        .stdin(Stdio::from(std::fs::File::open(&file).unwrap()))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("spawn serve");
    assert!(out.status.success(), "the hook must always exit 0");
    String::from_utf8(out.stdout).unwrap()
}

fn block_reason(out: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(out.trim()).ok()?;
    (v.get("decision") == Some(&serde_json::json!("block"))).then(|| v["reason"].as_str().unwrap_or("").to_string())
}

fn red(fx: &Fixture, cwd: &Path, tool: &str, command: &str, id: &str, literal: &str) {
    let out = hook(fx, cwd, tool, command);
    let reason = block_reason(&out).unwrap_or_else(|| panic!("must be refused: {command:?}\n{out}"));
    assert!(reason.contains(&format!("[THOR] rule {id} ")), "must name {id}: {reason}");
    assert!(reason.contains(literal), "must name the matched {literal:?}: {reason}");
}

fn green(fx: &Fixture, cwd: &Path, tool: &str, command: &str) {
    let out = hook(fx, cwd, tool, command);
    assert!(block_reason(&out).is_none(), "must pass: {command:?}\n{out}");
}

// ------------------------------------------------------------------ 5a

#[test]
fn the_prod_container_rule_reaches_every_compound_shape() {
    let fx = fixture(&[rule(
        "door-prod",
        &["docker exec"],
        None,
        forbidden(&["acme-prod node -e", "acme-prod node --input-type=module"]),
    )]);
    let a = fx.alpha.clone();
    for cmd in [
        "cd /srv && docker exec acme-prod node -e \"1\"",
        "docker ps; docker exec acme-prod node -e \"1\"",
        "cd /srv\ndocker exec acme-prod node -e \"1\"",
        "echo x | docker exec -i acme-prod node -e \"1\"",
        "ssh nas \"docker exec acme-prod node -e 'console.log(1)'\"",
        "ssh -p 2222 admin@nas 'sudo docker exec acme-prod node -e \"1\"'",
        "bash -lc 'cd /x && docker exec acme-prod node -e 1'",
        "ssh nas bash -s <<'EOF'\ncd /srv\ndocker exec acme-prod node -e 1\nEOF",
        "x=$(docker exec acme-prod node -e 1)",
        "if docker exec acme-prod node -e 1; then echo ok; fi",
        "(cd /srv && docker exec acme-prod node -e 1)",
        "timeout 30 docker exec acme-prod node -e 1",
        "sudo -u root docker exec acme-prod node -e 1",
    ] {
        red(&fx, &a, "Bash", cmd, "door-prod", "acme-prod node -e");
    }
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
        "python - <<'EOF'\nprint('docker exec acme-prod node -e')\nEOF",
        "docker logs acme-prod | grep 'node -e'",
    ] {
        green(&fx, &a, "Bash", cmd);
    }
}

#[test]
fn the_reset_rule_reaches_every_spelling_of_git() {
    let fx = fixture(&[rule("door-reset", &["git reset"], None, forbidden(&["--hard"]))]);
    let a = fx.alpha.clone();
    red(&fx, &a, "PowerShell", "\"C:/Program Files/Git/git.exe\" reset --hard x", "door-reset", "--hard");
    red(&fx, &a, "Bash", "git -C /srv/x reset --hard", "door-reset", "--hard");
    red(&fx, &a, "Bash", "GIT_DIR=/srv/x git reset --hard origin/main", "door-reset", "--hard");
    // An unquoted path with a space is two words to bash too.
    green(&fx, &a, "Bash", "C:/Program Files/Git/git.exe reset --hard x");
    green(&fx, &a, "Bash", "sudo git pull --ff-only");
}

#[test]
fn the_attribution_rule_reaches_the_real_commit_shapes() {
    let fx = fixture(&[rule(
        "door-attribution",
        &["git commit", "gh pr create"],
        None,
        forbidden(&["Co-Authored-By", "Co-authored-by", "Generated with"]),
    )]);
    let a = fx.alpha.clone();
    for (cmd, literal) in [
        (
            "git add -A && git commit -m \"$(cat <<'EOF'\nIt's fixed (see #3)\n\nCo-Authored-By: C\nEOF\n)\"",
            "Co-Authored-By",
        ),
        ("git add . && git commit -F - <<'EOF'\nfix\n\nCo-Authored-By: C\nEOF", "Co-Authored-By"),
        ("printf 'fix\\n\\nCo-Authored-By: C' | git commit -F -", "Co-Authored-By"),
        ("cd repo && gh pr create --title t --body \"Generated with X\"", "Generated with"),
        ("git commit -m \"fix\n\nCo-Authored-By: C\"", "Co-Authored-By"),
    ] {
        red(&fx, &a, "Bash", cmd, "door-attribution", literal);
    }
    for cmd in [
        "git add -A && git commit -m \"fix: parser\"",
        "git log --grep \"Co-Authored-By\"",
        // Refused before the door (the literal sat in a later, separate
        // command); the pipeline unit lets it through.
        "git commit -m \"fix\" && git log -1 --format=%B | grep -c \"Co-Authored-By\"",
    ] {
        green(&fx, &a, "Bash", cmd);
    }
}

#[test]
fn the_loop_rule_reaches_the_loop_and_nothing_after_it() {
    let fx = fixture(&[rule("door-loop", &["until", "while", "for"], None, forbidden(&["ssh"]))]);
    let a = fx.alpha.clone();
    red(&fx, &a, "Bash", "cd /x && until ssh nas test -f /x; do sleep 2; done", "door-loop", "ssh");
    red(&fx, &a, "Bash", "for h in a b; do for i in 1 2; do echo $i; done; ssh $h uptime; done", "door-loop", "ssh");
    red(&fx, &a, "PowerShell", "while ($true) { ssh nas uptime; Start-Sleep 5 }", "door-loop", "ssh");
    // Token boundaries: a full path still counts.
    red(&fx, &a, "Bash", "for h in a b; do /usr/bin/ssh $h uptime; done", "door-loop", "ssh");
    green(&fx, &a, "Bash", "for f in *.log; do grep -c ERR \"$f\"; done");
    // Refused before the door: the ssh merely follows the loop.
    green(&fx, &a, "Bash", "for f in a b; do echo $f; done; ssh nas uptime");
    green(&fx, &a, "PowerShell", "foreach ($f in $files) { Get-Item $f }; ssh nas uptime");
    // Token boundaries: `ssh` inside a longer name or a hidden folder.
    green(&fx, &a, "Bash", "for f in sshKey.mjs b.mjs; do node $f; done");
    green(&fx, &a, "Bash", "for k in ~/.ssh/*.pub; do cat $k; done");
}

#[test]
fn a_place_named_by_the_last_cd_travels_with_the_command() {
    let fx = fixture(&[rule("door-place", &["git reset"], None, forbidden(&["acme-prod", "nas-share"]))]);
    let a = fx.alpha.clone();
    red(&fx, &a, "Bash", "cd //nas-share/docker/acme-prod && git reset --hard origin/main", "door-place", "acme-prod");
    green(&fx, &a, "Bash", "cd C:/x/dev/repo && git reset --hard origin/main");
    green(&fx, &a, "Bash", "cd //nas-share/x && cp a b; cd C:/repo && git reset --hard");
}

#[test]
fn the_push_after_a_commit_heredoc_is_its_own_segment() {
    let fx = fixture(&[rule("door-tags", &["git push"], None, forbidden(&["push --tags"]))]);
    let a = fx.alpha.clone();
    red(
        &fx,
        &a,
        "Bash",
        "git add -A && git commit -m \"$(cat <<'EOF'\nIt's fixed (see #3\nEOF\n)\" && git push --tags",
        "door-tags",
        "push --tags",
    );
    // A PowerShell here-string is data.
    green(&fx, &a, "PowerShell", "$m = @'\nit's done; git push --tags\n'@\nSet-Content notes.md $m");
}

#[test]
fn a_literal_matches_only_on_token_boundaries() {
    let fx = fixture(&[rule("door-force", &["git push"], None, forbidden(&["--force"]))]);
    let a = fx.alpha.clone();
    red(&fx, &a, "Bash", "git push --force origin main", "door-force", "--force");
    red(&fx, &a, "Bash", "git push --force=true origin main", "door-force", "--force");
    green(&fx, &a, "Bash", "git push --force-with-lease origin main");
    // Not refused, but still SERVED: serving reads the same segments, so the
    // rule is put in front of a push after a `cd` as well as a bare one.
    for cmd in ["git push --force-with-lease origin main", "cd repo && git push origin main"] {
        let out = hook(&fx, &a, "Bash", cmd);
        assert!(block_reason(&out).is_none() && out.contains("[door-force]"), "must be served, not refused: {cmd}\n{out}");
    }
    let out = hook(&fx, &a, "Bash", "echo git push --force");
    assert!(!out.contains("door-force"), "a mention is not a push: {out}");
}

#[test]
fn an_exe_anchor_reaches_only_its_own_program() {
    let fx = fixture(&[rule("door-exe", &["install.exe"], None, forbidden(&["--all"]))]);
    let a = fx.alpha.clone();
    red(&fx, &a, "Bash", "\"C:/x/bin/install.exe\" --all", "door-exe", "--all");
    green(&fx, &a, "Bash", "install -m 644 a b --all");
}

#[test]
fn an_anchor_written_with_quotes_still_reaches_its_own_command() {
    let fx = fixture(&[rule("door-quoted", &["bws run -- 'node server.js'"], Some("alpha"), forbidden(&["--watch"]))]);
    let a = fx.alpha.clone();
    red(&fx, &a, "Bash", "bws run -- 'node server.js' --watch", "door-quoted", "--watch");
    red(&fx, &a, "Bash", "cd /srv && bws run -- 'node server.js' --watch", "door-quoted", "--watch");
}

/// The measured false block before the door, in a neutral form: the
/// upload rule refused a PowerShell line whose `gh release view` was only a
/// look, because a LATER command of the line named the release folder.
#[test]
fn the_upload_rule_no_longer_refuses_a_look_followed_by_a_local_copy() {
    let fx = fixture(&[rule("door-upload", &["gh release"], None, forbidden(&["target/release", "target\\release"]))]);
    let a = fx.alpha.clone();
    green(
        &fx,
        &a,
        "PowerShell",
        "gh release view v0.9.11 --repo acme/tool --json tagName; $new = \"C:\\x\\target\\release\\tool.exe\"; Copy-Item $new $dst",
    );
    // The upload itself, in both spellings, is still refused.
    red(&fx, &a, "PowerShell", "gh release upload v1 C:\\x\\target\\release\\tool.exe", "door-upload", "target\\release");
    red(&fx, &a, "Bash", "gh release upload v1 ./target/release/tool.exe", "door-upload", "target/release");
}

// ------------------------------------------------------------------ 5b

#[test]
fn a_requires_rule_reaches_the_compound_and_ssh_shapes() {
    let fx = fixture(&[
        rule("door-go", &["docker exec acme-prod"], None, requires("docker exec acme-prod", &["go-owner"])),
        rule("door-node-test", &["node --test"], None, requires("node --test", &["--test-reporter", "--test-timeout"])),
        rule("door-dev-compose", &["ssh"], None, requires("-p acme-dev", &["-f docker-compose.dev.yml"])),
    ]);
    let a = fx.alpha.clone();
    red(&fx, &a, "Bash", "ssh nas \"docker exec acme-prod ls /app\"", "door-go", "go-owner");
    red(&fx, &a, "Bash", "cd x && docker exec acme-prod ls", "door-go", "go-owner");
    red(&fx, &a, "Bash", "cd server && node --test test/x.test.js", "door-node-test", "--test-reporter");
    red(&fx, &a, "Bash", "ssh admin@host \"cd /srv/app && docker compose -p acme-dev up -d\"", "door-dev-compose", "-p acme-dev");
    for cmd in [
        "ssh nas \"docker exec acme-dev ls /app\"",
        "ssh nas \"docker exec acme-prod ls\" # go-owner 2026-09-28",
        "docker logs acme-prod",
        "echo 'docker exec acme-prod ls'",
        "ssh admin@host \"docker compose -p acme-dev-old logs\"",
        "scp -p acme-dev host:/x .",
        "ssh admin@host uptime",
    ] {
        green(&fx, &a, "Bash", cmd);
    }
}

#[test]
fn a_trigger_inside_a_url_keeps_its_boundary() {
    let fx = fixture(&[rule("door-health", &["curl"], None, requires("/health", &["cb="]))]);
    let a = fx.alpha.clone();
    red(&fx, &a, "Bash", "curl -s http://10.0.0.5:3002/health", "door-health", "/health");
    green(&fx, &a, "Bash", "curl -s http://10.0.0.5:3002/healthz");
}

#[test]
fn a_pure_help_call_is_never_refused_by_a_requires_rule() {
    let fx = fixture(&[rule("door-install-db", &["install.exe"], None, requires("install.exe", &["--db"]))]);
    let a = fx.alpha.clone();
    red(&fx, &a, "Bash", "\"C:/x/bin/install.exe\" --register-root", "door-install-db", "--db");
    red(&fx, &a, "Bash", "cd C:/x && ./install.exe --project demo", "door-install-db", "--db");
    // A call with --help on its own segment runs nothing; the flag on another segment answers nothing.
    green(&fx, &a, "Bash", "install.exe --help --all");
    red(&fx, &a, "Bash", "install.exe --help && install.exe --all", "door-install-db", "--db");
    red(&fx, &a, "Bash", "install.exe --all -- --help", "door-install-db", "--db");
    green(&fx, &a, "Bash", "\"C:/x/bin/install.exe\" --help");
    green(&fx, &a, "Bash", "install.exe -h");
    green(&fx, &a, "Bash", "install -m 644 a b");
}

// ------------------------------------------------------------------ 5c

#[test]
fn a_segment_run_in_another_checkout_is_judged_by_that_checkouts_rules() {
    let fx = fixture(&[
        rule("alpha-no-push", &["git push"], Some("alpha"), forbidden(&["git push"])),
        rule("alpha-no-share", &["git reset"], Some("alpha"), forbidden(&["acme-prod", "nas-share"])),
        // beta is a project this store knows: it holds a rule of its own.
        rule("beta-no-force", &["git push"], Some("beta"), forbidden(&["--force"])),
        rule("door-attribution", &["git commit"], None, forbidden(&["Co-Authored-By"])),
    ]);
    let a = fx.alpha.clone();
    let b = fx.beta.clone();
    // Session in alpha: every way of pushing alpha is alpha's business.
    red(&fx, &a, "Bash", "cd ../beta && git -C ../alpha push", "alpha-no-push", "git push");
    red(&fx, &a, "Bash", &format!("cd /tmp && git -C {} push", a.to_string_lossy().replace('\\', "/")), "alpha-no-push", "git push");
    if cfg!(windows) {
        // Git Bash's own spelling of the same folder - from a session in
        // beta, so only reading `/c/...` as `C:/...` finds alpha.
        let unix = a.to_string_lossy().replace('\\', "/");
        let drive = unix[..1].to_lowercase();
        let gitbash = format!("/{drive}{}", &unix[2..]);
        red(&fx, &a, "Bash", &format!("cd /tmp && git -C {gitbash} push"), "alpha-no-push", "git push");
        red(&fx, &fx.beta.clone(), "Bash", &format!("git -C {gitbash} push"), "alpha-no-push", "git push");
        green(&fx, &fx.beta.clone(), "Bash", "git -C . push");
    }
    red(&fx, &a, "Bash", "cd /tmp && git push", "alpha-no-push", "git push");
    red(&fx, &a, "Bash", "cd ../beta && GIT_DIR=../alpha/.git git push", "alpha-no-push", "git push");
    // A network path is never walked: the session's project stays.
    red(&fx, &a, "Bash", "cd //nas-share/docker/acme-prod && git reset --hard", "alpha-no-share", "acme-prod");
    // Pushing beta from an alpha session is beta's business - and beta's
    // own rules apply there.
    green(&fx, &a, "Bash", &format!("cd {} && git push -q origin main", b.to_string_lossy().replace('\\', "/")));
    green(&fx, &a, "Bash", "cd ../beta && git push -q origin main");
    red(&fx, &a, "Bash", "cd ../beta && git push --force origin main", "beta-no-force", "--force");
    // ...and a session in beta pushing beta is too.
    green(&fx, &b, "Bash", "git push -q origin main");
    // A global rule applies wherever the segment runs.
    red(
        &fx,
        &a,
        "Bash",
        &format!("cd {} && git commit -m \"x\n\nCo-Authored-By: C\"", b.to_string_lossy().replace('\\', "/")),
        "door-attribution",
        "Co-Authored-By",
    );
}

/// The stated price of keeping a `cd` from being an exit, pinned: a command
/// whose effect depends on its folder, run in a folder of no known project,
/// is judged under the session's project. A scratch repository is a real one
/// here (`git init` makes a `.git`, and its folder name then resolves as a
/// project of its own), and so is a folder whose marker names a project this
/// store has never heard of: neither is a project the memory knows, so
/// neither silences alpha's rules - alpha's irreversible container rule still
/// refuses there. Since 2026-10-09 the price is lower for `git` alone, which
/// acts on the repository that holds its folder: a push in such a repository
/// is not a push of alpha (a scratch repository, or another repository the
/// memory knows nothing of). A folder in no repository at all keeps the
/// session's project for git too.
#[test]
fn the_stated_price_a_folder_of_no_known_project_is_judged_under_the_session() {
    let fx = fixture(&[
        rule("alpha-no-push", &["git push"], Some("alpha"), forbidden(&["git push"])),
        rule("alpha-no-prod-node", &["docker exec"], Some("alpha"), forbidden(&["acme-prod node -e"])),
    ]);
    let base = fx.db.parent().unwrap().parent().unwrap().to_path_buf();
    let plain = base.join("plain-folder");
    std::fs::create_dir_all(&plain).unwrap();
    let scratch = base.join("scratch-repo");
    std::fs::create_dir_all(scratch.join(".git")).unwrap();
    let stranger = checkout(&base, "gamma");
    let a = fx.alpha.clone();
    for folder in [&plain, &scratch, &stranger] {
        let folder = folder.to_string_lossy().replace('\\', "/");
        if folder.ends_with("plain-folder") {
            red(&fx, &a, "Bash", &format!("cd {folder} && git push"), "alpha-no-push", "git push");
        } else {
            green(&fx, &a, "Bash", &format!("cd {folder} && git push"));
        }
        red(
            &fx,
            &a,
            "Bash",
            &format!("cd {folder} && docker exec acme-prod node -e 1"),
            "alpha-no-prod-node",
            "acme-prod node -e",
        );
    }
    // A push run in gamma's repository is not alpha's, known or not - and gamma's own rules apply once known.
    let mut store = thor_core::event_store::EventStore::open_existing(&fx.db).unwrap();
    model::store::declare(&mut store, "door", "door", "door-fixture", &named(&rule("gamma-note", &["make"], Some("gamma"), forbidden(&["--gamma"]))))
        .unwrap();
    drop(store);
    green(&fx, &a, "Bash", &format!("cd {} && git push -q origin main", stranger.to_string_lossy().replace('\\', "/")));
}

/// A `cd` that did not take effect never moves a command out of the
/// session's project - the measured exit (2026-09-28): `(cd ../beta) && git
/// push` pushed alpha, was judged as run in beta, and alpha's push ban only
/// advised. A `cd` in a subshell ends with it, a `cd` in a pipeline or sent
/// to the background never takes effect, a `cd` after `&&`/`||` or inside an
/// `if` or a loop may not have run, and a `cd` into a folder that is not
/// there fails and leaves the command where it was. Each keeps alpha's rule.
#[test]
fn a_cd_that_did_not_take_effect_never_moves_a_command_out_of_the_session() {
    let fx = fixture(&[
        rule("alpha-no-push", &["git push"], Some("alpha"), forbidden(&["git push"])),
        rule("alpha-no-prod-node", &["docker exec"], Some("alpha"), forbidden(&["acme-prod node -e"])),
        rule("beta-no-force", &["git push"], Some("beta"), forbidden(&["--force"])),
    ]);
    let a = fx.alpha.clone();
    for cmd in [
        "(cd ../beta) && git push",
        "(cd ../beta && ls); git push origin main",
        "cd ../beta | cat; git push",
        "cd ../beta & git push",
        "false && cd ../beta; git push",
        "false && cd ../beta || git push",
        "if false; then cd ../beta; fi; git push",
        "cd ../beta/not-there; git push",
    ] {
        red(&fx, &a, "Bash", cmd, "alpha-no-push", "git push");
    }
    red(&fx, &a, "Bash", "(cd ../beta) && docker exec acme-prod node -e 1", "alpha-no-prod-node", "acme-prod node -e");
    // A cd that certainly took effect still moves the command to beta.
    for cmd in [
        "cd ../beta && git push -q origin main",
        "cd ../beta; git push -q origin main",
        "cd ../beta\ngit push -q origin main",
        "(cd ../beta && git push -q origin main)",
        "cd ../beta || exit 1; git push -q origin main",
    ] {
        green(&fx, &a, "Bash", cmd);
    }
}

/// The place a `cd` names travels with what runs there, into a wrapper too,
/// and ends with the subshell it was typed in: the measured false block was
/// `(cd //nas-share/... && ls) && git reset --hard`, refused although the
/// reset runs outside the share.
#[test]
fn the_place_of_a_cd_reaches_into_a_wrapper_and_ends_with_its_subshell() {
    let fx = fixture(&[rule("door-place", &["git reset"], None, forbidden(&["acme-prod", "nas-share"]))]);
    let a = fx.alpha.clone();
    for cmd in [
        "cd //nas-share/docker/acme-prod && bash -c 'git reset --hard origin/main'",
        "cd //nas-share/docker/acme-prod && x=$(git reset --hard origin/main)",
        "cd //nas-share/docker/acme-prod && bash <<'EOF'\ngit reset --hard origin/main\nEOF",
    ] {
        red(&fx, &a, "Bash", cmd, "door-place", "acme-prod");
    }
    green(&fx, &a, "Bash", "(cd //nas-share/docker/acme-prod && ls) && git reset --hard origin/main");
}

/// The measured crash (2026-09-28): two thousand `$(` after a forbidden
/// command overflowed the hook's stack, the hook exited with a crash code,
/// and a hook that crashes lets the call through. Through the real hook: it
/// exits 0 and refuses, with the nest before or after the command, and a
/// long run of `$(:)` does not run it anywhere near its timeout.
#[test]
fn a_crafted_nest_never_crashes_the_hook() {
    let fx = fixture(&[rule("door-prod", &["docker exec"], None, forbidden(&["acme-prod node -e"]))]);
    let a = fx.alpha.clone();
    let bad = "docker exec acme-prod node -e 1";
    let nest = format!(": {}{}", "$(".repeat(20_000), ")".repeat(20_000));
    red(&fx, &a, "Bash", &format!("{bad}; {nest}"), "door-prod", "acme-prod node -e");
    red(&fx, &a, "Bash", &format!("{nest}; {bad}"), "door-prod", "acme-prod node -e");
    red(&fx, &a, "Bash", &format!("{bad}; : {}{}", "<(".repeat(20_000), ")".repeat(20_000)), "door-prod", "acme-prod node -e");
    let started = std::time::Instant::now();
    red(&fx, &a, "Bash", &format!(": {}; {bad}", "$(:)".repeat(40_000)), "door-prod", "acme-prod node -e");
    assert!(started.elapsed() < std::time::Duration::from_secs(10), "took {:?}", started.elapsed());
}

/// A `cd` the shell refuses moves nothing, and a work tree names no
/// repository - each form measured against Git Bash 5.3 and Windows
/// PowerShell 5.1 (2026-09-28): every call here pushes alpha, so alpha's
/// push ban refuses it. Before, each was judged as run in beta. The same
/// folder named the way the shell takes it still moves the push into beta.
#[test]
fn a_cd_the_shell_refuses_or_a_work_tree_never_moves_a_command_out_of_the_session() {
    let fx = fixture(&[
        rule("alpha-no-push", &["git push"], Some("alpha"), forbidden(&["git push"])),
        rule("beta-no-force", &["git push"], Some("beta"), forbidden(&["--force"])),
    ]);
    let a = fx.alpha.clone();
    for cmd in [
        "cd ../beta extra; git push -q origin main",
        "cd -x ../beta; git push -q origin main",
        "cd -@ ../beta; git push -q origin main",
        "chdir ../beta; git push -q origin main",
        "sudo cd ../beta; git push -q origin main",
        "pushd ../beta extra; git push -q origin main",
        "git --work-tree=../beta push -q origin main",
        "GIT_WORK_TREE=../beta git push -q origin main",
        "git --git-dir=.git --work-tree=../beta push -q origin main",
    ] {
        red(&fx, &a, "Bash", cmd, "alpha-no-push", "git push");
    }
    for cmd in [
        "Set-Location ../beta extra; git push -q origin main",
        "Set-Location -x ../beta; git push -q origin main",
        "cd -Path ../beta extra; git push -q origin main",
    ] {
        red(&fx, &a, "PowerShell", cmd, "alpha-no-push", "git push");
    }
    for cmd in [
        "cd -P ../beta; git push -q origin main",
        "cd -- ../beta; git push -q origin main",
        "pushd ../beta; git push -q origin main",
        "git -C ../beta push -q origin main",
        "git --git-dir=../beta/.git --work-tree=. push -q origin main",
    ] {
        green(&fx, &a, "Bash", cmd);
    }
    for cmd in ["Set-Location -Path ../beta; git push -q origin main", "cd -ea Stop ../beta; git push -q origin main"] {
        green(&fx, &a, "PowerShell", cmd);
    }
}

/// Arithmetic hides no command from the hook (measured against Git Bash
/// 5.3, 2026-09-28): a `<<` in `((...))`, `$[...]` or `for ((...))` is a
/// shift, never a heredoc, a command inside arithmetic runs, and `$((cmd) )`
/// is a command substitution. Before, the first three hid the next line.
#[test]
fn arithmetic_hides_no_command_from_the_hook() {
    let fx = fixture(&[rule("door-prod", &["docker exec"], None, forbidden(&["acme-prod node -e"]))]);
    let a = fx.alpha.clone();
    let bad = "docker exec acme-prod node -e 1";
    for cmd in [
        format!("(( x = 1 << 2 ))
{bad}"),
        format!("echo $[1<<2]
{bad}"),
        format!("for ((i=0; i<<1; i++)); do :; done
{bad}"),
        format!("(( $({bad}) ))"),
        format!("echo $(( $({bad}) + 1 ))"),
        format!("echo $(({bad}) )"),
    ] {
        red(&fx, &a, "Bash", &cmd, "door-prod", "acme-prod node -e");
    }
    green(&fx, &a, "Bash", &format!("(({bad}))"));
    green(&fx, &a, "Bash", "(( x = 1 << 2 ))
ls");
}

/// `item` with every check literal its text does not name appended to the
/// text (in `U+XXXX` form for a glyph): since step 8 of the two tracks the
/// write gate refuses a check whose literal the rule's own words never name
/// (A4), and these fixtures are about the hook, not about naming.
fn named(item: &model::item::Item) -> model::item::Item {
    let mut out = item.clone();
    let unnamed = model::gate::unnamed_literals(&out);
    if !unnamed.is_empty() {
        let shown: Vec<String> = unnamed.iter().map(|l| format!("'{}'", model::gate::shown_literal(l))).collect();
        out.text = format!("{} (names {})", out.text, shown.join(" "));
    }
    out
}
