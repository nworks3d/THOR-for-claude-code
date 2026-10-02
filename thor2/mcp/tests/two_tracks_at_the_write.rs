//! Every Rule/Orientation write goes through THOR's trial (step 8 of the two
//! tracks), end to end through the real compiled `mcp` binary - the door an
//! assistant uses. One test per probe of the step: each red is tried and
//! each green beside it, so none of them can only come out one way.
//!
//! RULE: the check bites (its forbidden form refused, every clean form let
//! through) and is stored, labelled `teeth:wired-not-proven` (or proven).
//! NOTE: no tooth holds; stored with no check and `teeth:note`.
//! REFUSED: nothing written on the item, the reply names the one fix, and one
//! `GateRefused` by `teeth-write` is recorded.

mod common;

use common::*;
use model::item::{Binding, Check, Item, Kind, Severity, TargetKind};
use model::store::CurrentVerdict;
use serde_json::json;

fn heavy(v: serde_json::Value) -> serde_json::Value {
    let mut v = v;
    v["severity"] = json!("irreversible");
    v
}

fn command_target(value: &str) -> serde_json::Value {
    json!([{ "kind": "command", "value": value }])
}

/// R8.1 / G8.1: a server started in alpha tries a beta rule against BETA's
/// registered working copy, never its own folder: refused while NOTES.md in
/// beta lacks the literal (naming beta's root), proven once it holds it at
/// HEAD there.
#[test]
fn r8_1_and_g8_1_a_rule_is_tried_in_its_own_projects_registered_checkout() {
    let tmp = tempfile::tempdir().unwrap();
    let db = store(tmp.path(), &["alpha", "beta"]);
    let alpha = checkout(tmp.path(), "alpha", &[("NOTES.md", "keep me\n")], true);
    let beta = checkout(tmp.path(), "beta", &[("NOTES.md", "nothing here yet\n")], true);
    register(&db, "alpha", &alpha);
    register(&db, "beta", &beta);
    let mut mcp = Mcp::start(&db, &alpha);
    let mut args = heavy(rule("co-rule", "NOTES.md must keep the line `keep me`.", Some("beta")));
    args["targets"] = json!([{ "kind": "path", "value": "NOTES.md" }]);
    args["check_kind"] = json!("contains");
    args["check_path"] = json!("NOTES.md");
    args["check_literal"] = json!("keep me");

    let reply = mcp.call("remember", args.clone());
    println!("R8.1: {reply}");
    let beta_shown = beta.display().to_string();
    assert!(reply.starts_with("REFUSED:"), "{reply}");
    assert!(reply.contains(&format!("your check is false in {beta_shown} right now: NOTES.md does not contain 'keep me'")), "{reply}");
    assert!(common::events_of(&db, "co-rule").iter().all(|e| e.kind == thor_core::event_store::EventKind::GateRefused));

    std::fs::write(beta.join("NOTES.md"), "keep me\nsecond line keep me\nplain line\n").unwrap();
    commit_all(&beta);
    let reply = mcp.call("remember", args);
    println!("G8.1: {reply}");
    assert!(reply.starts_with("stored 'co-rule'"), "{reply}");
    assert!(reply.contains("teeth: proven guard - NOTES.md in the working copy registered for beta"), "{reply}");
    assert!(reply.contains(&beta_shown), "{reply}");
    assert_eq!(verdict(&db, "co-rule"), CurrentVerdict::RuleProven);
}

/// R8.2 / G8.2: a light rule with NO check whose text composes a command
/// with a flag is refused naming the tooth THOR built from its own words;
/// the same rule hanging that tooth is stored wired, with the forms tried
/// and the real calls replayed. A light rule with nothing to build from is a
/// note.
#[test]
fn r8_2_and_g8_2_a_checkless_rule_whose_text_composes_a_tooth_is_refused_and_hanging_it_lands() {
    let tmp = tempfile::tempdir().unwrap();
    let db = store(tmp.path(), &["demo"]);
    served(&db, OTHER_SESSION, &["git push origin main", "git push origin topic", "git push"]);
    let mut mcp = Mcp::start(&db, tmp.path());
    let mut args = rule("a1-light", "Never run `git push --force` on main.", Some("demo"));
    args["targets"] = command_target("git push");
    let reply = mcp.call("remember", args.clone());
    println!("R8.2: {reply}");
    assert!(reply.starts_with("REFUSED: THOR built a tooth from your own text and it bites"), "{reply}");
    assert!(reply.contains("forbidden '--force' on 'git push'"), "{reply}");
    assert!(reply.contains("check_kind forbidden, check_literals ['--force']"), "{reply}");
    let refusals = common::events_of(&db, "a1-light");
    assert_eq!(refusals.len(), 1, "R8.17: one refused-write record, nothing else");
    assert_eq!(refusals[0].actor, "teeth-write");
    assert!(!mcp.call("get", json!({"id": "a1-light"})).contains("\"id\""), "the item does not exist");

    args["check_kind"] = json!("forbidden");
    args["check_literals"] = json!(["--force"]);
    let reply = mcp.call("remember", args);
    println!("G8.2: {reply}");
    assert!(reply.starts_with("stored 'a1-light'"), "{reply}");
    assert!(reply.contains("teeth: wired, not proven - tried git push --force (refused), git push (let through)"), "{reply}");
    assert!(reply.contains("reaches 3 whole real call(s) in the log"), "{reply}");
    assert!(reply.contains("would refuse 0"), "{reply}");
    assert!(get(&mut mcp, "a1-light").tags.contains(&"teeth:wired-not-proven".to_string()));

    let mut light = rule("subjects-short", "Keep commit subjects under 60 characters.", Some("demo"));
    light["moments"] = json!(["commit"]);
    let reply = mcp.call("remember", light);
    assert!(
        reply.contains(
            "teeth: note - nothing in the text or anchors to build a tooth from (no flag, quoted call, backtick span or path)"
        ),
        "{reply}"
    );
    let note = get(&mut mcp, "subjects-short");
    assert_eq!(note.tags, vec!["teeth:note".to_string()]);
    assert!(note.check.is_none());
}

/// R8.3 (K20): a file check whose project has no registered working copy.
/// From inside that project's checkout the write is refused naming the one
/// command to run there; from another project it is a note with the check
/// pending.
#[test]
fn r8_3_an_unregistered_projects_file_check_is_refused_inside_it_and_a_note_elsewhere() {
    let tmp = tempfile::tempdir().unwrap();
    let db = store(tmp.path(), &["alpha", "gamma"]);
    let gamma = checkout(tmp.path(), "gamma", &[("README.md", "the marker-x1\n")], false);
    let alpha = checkout(tmp.path(), "alpha", &[], false);
    let mut args = heavy(rule("gamma-rule", "README.md must keep the marker-x1 line.", Some("gamma")));
    args["targets"] = json!([{ "kind": "path", "value": "README.md" }]);
    args["check_kind"] = json!("contains");
    args["check_path"] = json!("README.md");
    args["check_literal"] = json!("marker-x1");

    let mut inside = Mcp::start(&db, &gamma);
    let reply = inside.call("remember", args.clone());
    println!("R8.3 inside: {reply}");
    assert!(
        reply.starts_with("REFUSED: THOR cannot try this file check: 'gamma' has no registered working copy. Run this from inside the checkout: install.exe --register-root --db \""),
        "{reply}"
    );
    assert!(reply.contains("then write this again"), "{reply}");
    drop(inside);

    let mut elsewhere = Mcp::start(&db, &alpha);
    let reply = elsewhere.call("remember", args);
    println!("R8.3 elsewhere: {reply}");
    assert!(reply.starts_with("stored 'gamma-rule'"), "{reply}");
    assert!(
        reply.contains("teeth: note - this file check could not be tried here because 'gamma' has no registered working copy on this machine; THOR tries it again"),
        "{reply}"
    );
    let item = get(&mut elsewhere, "gamma-rule");
    assert!(item.check.is_none());
    assert!(item.tags.contains(&"teeth:note".to_string()));
}

/// R8.4 as K8(d) resolves it: a writer's check that bites but would refuse
/// more than half of at least five real calls it reaches is refused, listing
/// them and the two ways on; `accept_refusing` naming one keeps it (G8.14's
/// mechanism), and a made-up call is refused.
#[test]
fn r8_4_a_check_refusing_most_real_calls_needs_the_writer_and_accept_refusing_keeps_it() {
    let tmp = tempfile::tempdir().unwrap();
    let db = store(tmp.path(), &["demo"]);
    let calls = [
        "git push origin main",
        "git push origin topic",
        "git push origin fix-1",
        "git push origin fix-2",
        "git push origin release",
        "git push origin docs",
    ];
    served(&db, OTHER_SESSION, &calls);
    let mut mcp = Mcp::start(&db, tmp.path());
    let mut args = heavy(rule("force-origin", "Never force-push to origin: `git push --force` rewrites shared history.", Some("demo")));
    args["targets"] = command_target("git push");
    args["check_kind"] = json!("forbidden");
    args["check_literals"] = json!(["origin"]);
    let reply = mcp.call("remember", args.clone());
    println!("R8.4: {reply}");
    assert!(reply.starts_with("REFUSED: your check would refuse 6 of the 6 real calls it reaches in the log"), "{reply}");
    assert!(reply.contains("'git push origin main'") && reply.contains("accept_refusing"), "{reply}");
    assert!(reply.contains("narrow the check"), "{reply}");
    // F4: the narrower tooth THOR built from the text is named with it.
    assert!(reply.contains("forbidden '--force' on 'git push' refuses 'git push --force'"), "{reply}");
    assert!(reply.contains("Hang it: check_kind forbidden, check_literals ['--force']"), "{reply}");

    args["accept_refusing"] = json!(["git push origin made-up"]);
    let reply = mcp.call("remember", args.clone());
    assert!(reply.starts_with("REFUSED:") && reply.contains("which is not one of the listed calls"), "{reply}");

    args["accept_refusing"] = json!(["git push origin main"]);
    let reply = mcp.call("remember", args.clone());
    println!("R8.4 accepted: {reply}");
    assert!(reply.starts_with("stored 'force-origin'"), "{reply}");
    assert!(reply.contains("teeth: wired, not proven"), "{reply}");
    assert!(reply.contains("refusing these real calls is accepted: git push origin main"), "{reply}");

    // K8(d): one real call is no majority (it takes at least five) - the
    // check is stored and the call it would refuse is listed, never hidden.
    let tmp = tempfile::tempdir().unwrap();
    let db = store(tmp.path(), &["demo"]);
    served(&db, OTHER_SESSION, &["git push origin main"]);
    let mut mcp = Mcp::start(&db, tmp.path());
    args.as_object_mut().unwrap().remove("accept_refusing");
    let reply = mcp.call("remember", args);
    println!("R8.4 one call: {reply}");
    assert!(reply.starts_with("stored 'force-origin'") && reply.contains("would refuse 1, e.g. 'git push origin main'"), "{reply}");
}

/// G8.3 / G8.6: with only ordinary calls in the log, forbidden '--force'
/// bites and lets them through (wired); a writer's check that reaches
/// nothing, on a rule whose text builds nothing, is dropped into a note that
/// quotes it and the form that failed.
#[test]
fn g8_3_and_g8_6_a_biting_check_is_wired_and_a_check_that_reaches_nothing_becomes_a_note() {
    let tmp = tempfile::tempdir().unwrap();
    let db = store(tmp.path(), &["demo"]);
    served(&db, OTHER_SESSION, &["git push origin main"]);
    let readme = checkout(tmp.path(), "demo", &[("README.md", "hello\n")], true);
    register(&db, "demo", &readme);
    let mut mcp = Mcp::start(&db, tmp.path());
    let mut args = heavy(rule("force-push", "Never force-push to origin: `git push --force` rewrites shared history.", Some("demo")));
    args["targets"] = command_target("git push");
    args["check_kind"] = json!("forbidden");
    args["check_literals"] = json!(["--force"]);
    let reply = mcp.call("remember", args);
    println!("G8.3: {reply}");
    assert!(reply.contains("teeth: wired, not proven"), "{reply}");

    let mut reach = rule("readme-marker", "Keep the readme free of the marker-x1 line.", Some("demo"));
    reach["always"] = json!(true);
    reach["check_kind"] = json!("absent");
    reach["check_path"] = json!("README.md");
    reach["check_literal"] = json!("marker-x1");
    let reply = mcp.call("remember", reach);
    println!("G8.3 note: {reply}");
    assert!(reply.starts_with("stored 'readme-marker'"), "{reply}");
    assert!(reply.contains("teeth: note - your check (absent 'marker-x1' in README.md) was not stored"), "{reply}");
    assert!(get(&mut mcp, "readme-marker").check.is_none());
}

/// R8.5 (K19): a collapsed check is a whole-command ban, stored wired with
/// the cleaner anchor as advice; the retired anchor-collapsed label is never
/// written.
#[test]
fn r8_5_a_collapsed_check_is_stored_as_a_whole_command_ban() {
    let tmp = tempfile::tempdir().unwrap();
    let db = store(tmp.path(), &["demo"]);
    let mut mcp = Mcp::start(&db, tmp.path());
    let mut args = heavy(rule("ban", "Never run `git push --force` here.", Some("demo")));
    args["targets"] = command_target("git push --force");
    args["check_kind"] = json!("forbidden");
    args["check_literals"] = json!(["--force"]);
    let reply = mcp.call("remember", args);
    println!("R8.5: {reply}");
    assert!(
        reply.contains(
            "teeth: wired, not proven - a whole-command ban: refuses 'git push --force x' (refused), lets 'echo git push --force', 'git --version' and 'git push' through. Advice: bind to 'git push' with the literal '--force' - THOR tried exactly that and it bites"
        ),
        "{reply}"
    );
    let item = get(&mut mcp, "ban");
    assert!(item.tags.contains(&"teeth:wired-not-proven".to_string()) && !item.tags.iter().any(|t| t == "teeth:anchor-collapsed"));
}

/// R8.6 / G8.4 (A4 through remember): a check whose literal the text never
/// names is refused with the sentence; named, it is wired - a glyph named by
/// its code point.
#[test]
fn r8_6_and_g8_4_a_literal_must_be_named_in_the_rules_own_text() {
    let tmp = tempfile::tempdir().unwrap();
    let db = store(tmp.path(), &["demo"]);
    let mut mcp = Mcp::start(&db, tmp.path());
    let mut args = heavy(rule("diskpart", "Never repartition the data disk.", Some("demo")));
    args["targets"] = command_target("diskpart");
    args["check_kind"] = json!("forbidden");
    args["check_literals"] = json!(["zqxj-never-typed"]);
    let reply = mcp.call("remember", args.clone());
    assert!(
        reply.starts_with(
            "REFUSED: this check's literal 'zqxj-never-typed' is not named anywhere in the rule's own text - a rule's tooth has to say in its own words what it refuses"
        ),
        "{reply}"
    );
    assert!(reply.contains("put it in the text, or drop the check"), "{reply}");

    args["text"] = json!("Never run `diskpart clean` on the data disk.");
    args["check_literals"] = json!(["clean"]);
    let reply = mcp.call("remember", args);
    assert!(reply.contains("teeth: wired, not proven"), "{reply}");

    let mut dash = rule("no-em-dash", "Never write an em dash (U+2014) into any file.", Some("demo"));
    dash["always"] = json!(true);
    dash["check_kind"] = json!("forbidden");
    dash["check_literals"] = json!(["\u{2014}"]);
    let reply = mcp.call("remember", dash);
    println!("G8.4 glyph: {reply}");
    assert!(reply.contains("teeth: wired, not proven"), "{reply}");
    assert!(reply.contains("writing 'U+2014' into a file (refused)") && reply.contains("writing an empty file (let through)"), "{reply}");
}

/// R8.7 / R8.8 / R8.9 (A5, K15, F4): a heavy rule with no check and a
/// no-literal reason, whose text composes the force-push tooth, is refused
/// naming it and both exits; bound only to a moment, the refusal also names
/// the command binding to add; a "legitimate call" that is THOR's own
/// forbidden form is refused as a dispute.
#[test]
fn r8_7_to_r8_9_a_tooth_built_from_the_text_forces_and_a_reason_no_longer_stands_in() {
    let tmp = tempfile::tempdir().unwrap();
    let db = store(tmp.path(), &["demo"]);
    served(&db, OTHER_SESSION, &["git push origin main"]);
    let mut mcp = Mcp::start(&db, tmp.path());
    let mut args = heavy(rule("force-reason", "Never force-push: `git push --force` rewrites shared history.", Some("demo")));
    args["targets"] = command_target("git push");
    args["tags"] = json!(["no-literal:force pushing leaves no fixed text in the command"]);
    let reply = mcp.call("remember", args.clone());
    println!("R8.7: {reply}");
    assert!(reply.starts_with("REFUSED: THOR built a tooth from your own text and it bites: forbidden '--force' on 'git push' refuses 'git push --force'"), "{reply}");
    assert!(reply.contains("Hang it: check_kind forbidden, check_literals ['--force']"), "{reply}");
    assert!(reply.contains("legitimate_calls: ['git push --force <the rest of the real call>']"), "{reply}");
    assert!(reply.contains("A no-literal reason no longer stands in for a tooth that works."), "{reply}");

    let mut moment = heavy(rule("force-moment", "Never force-push: `git push --force` rewrites shared history.", Some("demo")));
    moment["moments"] = json!(["push"]);
    let reply = mcp.call("remember", moment);
    println!("R8.8: {reply}");
    assert!(reply.starts_with("REFUSED:") && reply.contains("and bind it to the command 'git push'"), "{reply}");

    args["legitimate_calls"] = json!(["git push --force"]);
    let reply = mcp.call("remember", args);
    println!("R8.9: {reply}");
    assert!(reply.contains("'git push --force' is the forbidden form THOR built itself, not a legitimate call"), "{reply}");
}

/// G8.5 (K22): a valid dispute neutralises the tooth it names, and a rule
/// whose every forcing tooth is disputed is stored as a note by dispute,
/// listed for the owner. This text composes THREE teeth - forbidden
/// 'list-heads' on 'git bundle', and both polarities of '--mirror' on the
/// command it recommends, `git clone` (derive (b), proposing that binding) -
/// so each needs its own real legitimate call (or one of them is hung). A
/// dispute of one leaves the others forcing. A flag only in prose, away from
/// the anchor, derives nothing.
#[test]
fn g8_5_a_valid_dispute_is_a_note_by_dispute_and_prose_derives_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let db = store(tmp.path(), &["demo"]);
    let mut mcp = Mcp::start(&db, tmp.path());
    let mut args = rule("bundle", "Prove a bundle with `git clone --mirror` plus fsck, never with `git bundle list-heads`.", Some("demo"));
    args["targets"] = command_target("git bundle");
    let reply = mcp.call("remember", args.clone());
    println!("G8.5 red: {reply}");
    assert!(reply.starts_with("REFUSED:") && reply.contains("forbidden 'list-heads' on 'git bundle'"), "{reply}");
    assert!(reply.contains("forbidden '--mirror' on 'git clone'") && reply.contains("requires '--mirror' after 'git clone' on 'git clone'"), "{reply}");
    assert!(reply.contains("Both polarities bite on the same fragment"), "{reply}");

    args["legitimate_calls"] = json!(["git bundle list-heads backup.bundle"]);
    let reply = mcp.call("remember", args.clone());
    println!("G8.5 one dispute: {reply}");
    assert!(reply.starts_with("REFUSED:") && !reply.contains("'list-heads' on 'git bundle' refuses"), "the disputed tooth is gone: {reply}");

    args["legitimate_calls"] = json!(["git bundle list-heads backup.bundle", "git clone --mirror origin.git backup.git", "git clone origin.git work"]);
    let reply = mcp.call("remember", args);
    println!("G8.5: {reply}");
    assert!(reply.starts_with("stored 'bundle'"), "{reply}");
    assert!(
        reply.contains("teeth: note by dispute - a tooth THOR built bites, and you named 'git bundle list-heads backup.bundle'"),
        "{reply}"
    );
    assert!(reply.contains("doctor lists this for the owner"), "{reply}");
    assert!(get(&mut mcp, "bundle").check.is_none());

    let mut prose = rule("encoding", "Pass -Encoding when a script writes a file; cscript does the rest.", Some("demo"));
    prose["targets"] = command_target("cscript");
    let reply = mcp.call("remember", prose);
    println!("G8.5 prose: {reply}");
    assert!(reply.starts_with("stored 'encoding'") && reply.contains("teeth: note"), "{reply}");
}
/// R8.10 / G8.6 (legit, listing forms): a requires check that refuses the
/// listing forms is not stored, and the refusal says so first. The text
/// RECOMMENDS `git tag -a`, and the teeth it composes forbid it (A5, K22):
/// so the rule is refused with the tooth and both exits - here the writer
/// names the real call that carries it, and it is stored as a note by
/// dispute. An inner trigger that passes every listing form stays wired.
#[test]
fn r8_10_and_g8_6_a_check_refusing_a_listing_form_is_never_stored() {
    let tmp = tempfile::tempdir().unwrap();
    let db = store(tmp.path(), &["demo"]);
    served(&db, OTHER_SESSION, &["git tag -l \"backup-checkpoint*\""]);
    let mut mcp = Mcp::start(&db, tmp.path());
    let mut args = rule("tag-annotated", "Tag every release annotated: `git tag -a v1 -m msg`.", Some("demo"));
    args["targets"] = command_target("git tag");
    args["check_kind"] = json!("requires");
    args["check_literals"] = json!(["git tag", "-a"]);
    let reply = mcp.call("remember", args.clone());
    println!("R8.10: {reply}");
    assert!(
        reply.starts_with("REFUSED: Your check (requires '-a' after 'git tag' on 'git tag') was not stored: it refuses 'git tag' and 'git tag -l' and 'git tag --list', which a check must let through."),
        "{reply}"
    );
    assert!(reply.contains("forbidden '-a v1' on 'git tag'") && reply.contains("legitimate_calls"), "{reply}");
    assert!(reply.contains("would refuse 1, e.g. 'git tag -l \"backup-checkpoint*\"'"), "the replay line: {reply}");

    args["legitimate_calls"] = json!(["git tag -a v1 -m release"]);
    let reply = mcp.call("remember", args);
    println!("R8.10 disputed: {reply}");
    assert!(reply.starts_with("stored 'tag-annotated'") && reply.contains("teeth: note by dispute"), "{reply}");
    assert!(get(&mut mcp, "tag-annotated").check.is_none(), "the listing-refusing check is never stored");

    let mut inner = rule("dev-compose", "On the dev host, `ssh -p my-dev` always runs compose.dev.yml.", Some("demo"));
    inner["targets"] = command_target("ssh");
    inner["check_kind"] = json!("requires");
    inner["check_literals"] = json!(["-p my-dev", "compose.dev.yml"]);
    let reply = mcp.call("remember", inner);
    println!("G8.6 inner: {reply}");
    assert!(reply.contains("teeth: wired, not proven"), "{reply}");
}
/// R8.12 (DR M8, K8/K18): the receipt replays the real calls a writer's
/// check reaches. A requires check that would refuse a listing form is never
/// stored; a real call it would refuse is listed; a blind inner trigger is
/// named.
#[test]
fn r8_12_the_receipt_names_the_real_calls_a_check_would_refuse() {
    let tmp = tempfile::tempdir().unwrap();
    let db = store(tmp.path(), &["demo"]);
    served(&db, OTHER_SESSION, &["git tag -l", "git tag v1.2 -m x", "pip show cartographer3d-plugin", "mkdir /c/tmp/thor-probe-eval"]);
    let mut mcp = Mcp::start(&db, tmp.path());

    let mut tag = rule("tag-annotated", "Tag every release annotated: `git tag -a v1 -m msg`.", Some("demo"));
    tag["targets"] = command_target("git tag");
    tag["check_kind"] = json!("requires");
    tag["check_literals"] = json!(["git tag", "-a"]);
    let reply = mcp.call("remember", tag);
    println!("R8.12 tag: {reply}");
    assert!(reply.starts_with("REFUSED:"), "{reply}");
    assert!(reply.contains("reaches 2 whole real call(s) in the log") && reply.contains("would refuse 2, e.g."), "{reply}");
    assert!(reply.contains("'git tag -l'") && reply.contains("'git tag v1.2 -m x'"), "{reply}");

    let mut pip = rule("pip-no-deps", "Install a printer plugin with `pip install --no-deps`.", Some("demo"));
    pip["targets"] = command_target("pip");
    pip["check_kind"] = json!("requires");
    pip["check_literals"] = json!(["pip", "--no-deps"]);
    let reply = mcp.call("remember", pip);
    println!("R8.12 pip: {reply}");
    // No listing form of pip, and one whole call is no K8(d) majority: the
    // check is stored, and the real call it would refuse is listed.
    assert!(reply.starts_with("stored 'pip-no-deps'") && reply.contains("teeth: wired, not proven"), "{reply}");
    assert!(reply.contains("would refuse 1, e.g. 'pip show cartographer3d-plugin'"), "the real call is listed: {reply}");

    let mut mk = rule("mkdir-thor", "A folder under /thor is made with `mkdir -p` so a rerun never fails.", Some("demo"));
    mk["targets"] = command_target("mkdir");
    mk["check_kind"] = json!("requires");
    mk["check_literals"] = json!(["/thor", "-p"]);
    let reply = mcp.call("remember", mk);
    println!("R8.12 mkdir: {reply}");
    assert!(reply.contains("it occurs in 1 reached call(s) and the matcher finds it in 0"), "{reply}");
    assert!(common::events_of(&db, "tag-annotated").iter().all(|e| e.kind == thor_core::event_store::EventKind::GateRefused));
}

/// R8.11 (D5): '--force' is not a whole token of '--force-with-lease', so a
/// planted over-match call can never drop the writer's check.
#[test]
fn r8_11_an_over_match_never_drops_a_check() {
    let tmp = tempfile::tempdir().unwrap();
    let db = store(tmp.path(), &["demo"]);
    served(&db, OTHER_SESSION, &["git push --force-with-lease origin main"]);
    let mut mcp = Mcp::start(&db, tmp.path());
    let mut args = heavy(rule("no-force", "Never run `git push --force` on main.", Some("demo")));
    args["targets"] = command_target("git push");
    args["check_kind"] = json!("forbidden");
    args["check_literals"] = json!(["--force"]);
    let reply = mcp.call("remember", args);
    println!("R8.11: {reply}");
    assert!(reply.contains("teeth: wired, not proven"), "{reply}");
    assert!(reply.contains("reaches 1 whole real call(s) in the log") && reply.contains("would refuse 0"), "{reply}");
    assert!(get(&mut mcp, "no-force").check.is_some());
}

/// R8.13 / R8.15 / G8.8 (A6): removing the tooth's words from the text is a
/// weakening that needs a because; with one, the check's literal is no
/// longer named and A4 refuses. A tags-only revise runs no trial and keeps
/// the record it had; on an undecided item it stays undecided.
#[test]
fn r8_13_r8_15_and_g8_8_revise_the_words_a_tooth_stands_on() {
    let tmp = tempfile::tempdir().unwrap();
    let db = store(tmp.path(), &["demo"]);
    let mut mcp = Mcp::start(&db, tmp.path());
    let mut args = rule("force-push", "Never `git push --force` on main.", Some("demo"));
    args["targets"] = command_target("git push");
    args["check_kind"] = json!("forbidden");
    args["check_literals"] = json!(["--force"]);
    assert!(mcp.call("remember", args).contains("teeth: wired, not proven"));

    let reply = mcp.call("revise", json!({"id": "force-push", "text": "Never rewrite shared history."}));
    println!("R8.13 no because: {reply}");
    assert!(reply.starts_with("REFUSED:") && reply.contains("this revise removes 'git push --force', '--force' from the text, and THOR builds teeth from it"), "{reply}");
    let reply = mcp.call("revise", json!({"id": "force-push", "text": "Never rewrite shared history.", "because": "fixture: the words move to a report"}));
    println!("R8.13 because: {reply}");
    assert!(reply.starts_with("REFUSED:") && reply.contains("the check's literal '--force' is no longer named in the text"), "{reply}");

    let reply = mcp.call("revise", json!({"id": "force-push", "tags": ["gotcha"]}));
    println!("G8.8: {reply}");
    assert!(
        reply.starts_with("revised 'force-push' (event seq ")
            && reply.contains(") - teeth unchanged: the trial at log seq ")
            && reply.contains(" still matches its text, anchors, check, project and severity"),
        "{reply}"
    );
    assert_eq!(verdict(&db, "force-push"), CurrentVerdict::RuleWired);

    // R8.15: a no-literal reason on an item no trial decided changes nothing.
    seed_raw(
        &db,
        &Item {
            id: "undecided-x".to_string(),
            kind: Kind::Rule,
            text: "A fixture rule that was never tried.".to_string(),
            bindings: vec![Binding::Always],
            severity: Some(Severity::Costly),
            project: Some("demo".to_string()),
            tags: vec![],
            expires: None,
            key: None,
            falsifier: Some("it turns out to have been tried".to_string()),
            check: None,
        },
    );
    let reply = mcp.call("revise", json!({"id": "undecided-x", "tags": ["no-literal:a judgement with no text to catch at all"]}));
    println!("R8.15: {reply}");
    assert!(reply.starts_with("revised") && reply.contains("teeth: undecided - it has never been tried"), "{reply}");
    assert!(matches!(verdict(&db, "undecided-x"), CurrentVerdict::Undecided { .. }));
}

/// R8.16 / G8.9: a writer may not set a note's label; a revise hanging a
/// check that bites moves the note onto the rule track.
#[test]
fn r8_16_and_g8_9_a_note_moves_to_the_rule_track_only_through_the_trial() {
    let tmp = tempfile::tempdir().unwrap();
    let db = store(tmp.path(), &["demo"]);
    let mut mcp = Mcp::start(&db, tmp.path());
    let mut args = rule("n1", "Never run `sshpass` here; use keys.", Some("demo"));
    args["severity"] = json!("costly");
    args["always"] = json!(true);
    let reply = mcp.call("remember", args);
    println!("G8.9 first: {reply}");
    let reply = mcp.call("revise", json!({"id": "n1", "tags": ["teeth:proven-guard"]}));
    assert!(reply.starts_with("invalid 'tags': 'teeth:proven-guard' starts with 'teeth:'"), "{reply}");

    let reply = mcp.call("revise", json!({"id": "n1", "check_kind": "forbidden", "check_literals": ["sshpass"]}));
    println!("G8.9: {reply}");
    assert!(reply.contains("teeth: wired, not proven"), "{reply}");
    assert!(get(&mut mcp, "n1").tags.contains(&"teeth:wired-not-proven".to_string()));
}

/// R8.18 (Z2 b): a contains literal present twice - the forbidden attempt
/// removes BOTH occurrences and is refused, so a working tooth is never
/// judged toothless.
#[test]
fn r8_18_the_forbidden_attempt_removes_every_occurrence() {
    let tmp = tempfile::tempdir().unwrap();
    let db = store(tmp.path(), &["beta"]);
    let beta = checkout(tmp.path(), "beta", &[("NOTES.md", "keep me\nsecond line keep me\nplain line\n")], true);
    register(&db, "beta", &beta);
    let mut mcp = Mcp::start(&db, tmp.path());
    let mut args = rule("twice", "NOTES.md must keep the line `keep me`.", Some("beta"));
    args["targets"] = json!([{ "kind": "path", "value": "NOTES.md" }]);
    args["check_kind"] = json!("contains");
    args["check_path"] = json!("NOTES.md");
    args["check_literal"] = json!("keep me");
    let reply = mcp.call("remember", args);
    println!("R8.18: {reply}");
    assert!(reply.contains("tried an Edit removing 'keep me' (refused)"), "{reply}");
    assert!(reply.contains("the file without line 3 (let through)"), "{reply}");
}

/// G8.11 (R7): a refused write, then the write that hangs the named tooth -
/// the item's own head chain holds only the stored write; the refusal is on
/// the log, head-neutral.
#[test]
fn g8_11_a_refused_write_is_head_neutral() {
    let tmp = tempfile::tempdir().unwrap();
    let db = store(tmp.path(), &["demo"]);
    let mut mcp = Mcp::start(&db, tmp.path());
    let mut args = heavy(rule("refused-then-hung", "Never force-push: `git push --force` rewrites shared history.", Some("demo")));
    args["targets"] = command_target("git push");
    assert!(mcp.call("remember", args.clone()).starts_with("REFUSED:"));
    args["check_kind"] = json!("forbidden");
    args["check_literals"] = json!(["--force"]);
    assert!(mcp.call("remember", args).starts_with("stored"));
    let events = common::events_of(&db, "refused-then-hung");
    let facts = events.iter().filter(|e| e.kind == thor_core::event_store::EventKind::FactCreated).count();
    let refusals = events.iter().filter(|e| e.kind == thor_core::event_store::EventKind::GateRefused).count();
    assert_eq!((facts, refusals), (1, 1));
    let heads = thor_core::cas::compute_head_sets(&events);
    assert_eq!(heads["refused-then-hung"].heads.len(), 1);
    assert_eq!(verdict(&db, "refused-then-hung"), CurrentVerdict::RuleWired);
}

/// G8.12 (F12, K23): unpin of a pinned rule whose literal is partly unnamed
/// lands, undecided with the reason; a revise naming it gives the verdict.
#[test]
fn g8_12_an_old_a4_failure_never_blocks_an_unpin() {
    let tmp = tempfile::tempdir().unwrap();
    let db = store(tmp.path(), &["demo"]);
    seed_raw(
        &db,
        &Item {
            id: "pinned-old".to_string(),
            kind: Kind::Rule,
            text: "Never run `git push --force` on main.".to_string(),
            bindings: vec![Binding::Always, Binding::Target { kind: TargetKind::Command, value: "git push".to_string() }],
            severity: None,
            project: Some("demo".to_string()),
            tags: vec![],
            expires: None,
            key: None,
            falsifier: Some("A force push to main turns out harmless.".to_string()),
            check: Some(Check::Forbidden { literals: vec!["--force".to_string(), "--mirror".to_string()] }),
        },
    );
    let mut mcp = Mcp::start(&db, tmp.path());
    let reply = mcp.call("unpin", json!({"id": "pinned-old"}));
    println!("G8.12 unpin: {reply}");
    assert!(reply.starts_with("unpinned 'pinned-old'"), "{reply}");
    assert!(reply.contains("teeth: undecided - A4: '--mirror' not named in the text"), "{reply}");
    assert!(matches!(verdict(&db, "pinned-old"), CurrentVerdict::Undecided { .. }));
    let reply = mcp.call("revise", json!({"id": "pinned-old", "text": "Never run `git push --force` or `git push --mirror` on main."}));
    println!("G8.12 named: {reply}");
    assert!(reply.contains("teeth: wired, not proven"), "{reply}");
    assert_eq!(verdict(&db, "pinned-old"), CurrentVerdict::RuleWired);
}

/// G8.7 (DR M8): a check that refuses none of the real calls it reaches
/// says so, and the stored item is the same with and without them.
#[test]
fn g8_7_the_real_call_replay_changes_words_never_state() {
    let with_calls = {
        let tmp = tempfile::tempdir().unwrap();
        let db = store(tmp.path(), &["demo"]);
        served(&db, OTHER_SESSION, &["git push origin main", "git push origin topic"]);
        let mut mcp = Mcp::start(&db, tmp.path());
        let mut args = rule("replay", "Never run `git push --force` on main.", Some("demo"));
        args["targets"] = command_target("git push");
        args["check_kind"] = json!("forbidden");
        args["check_literals"] = json!(["--force"]);
        let reply = mcp.call("remember", args);
        assert!(reply.contains("reaches 2 whole real call(s) in the log") && reply.contains("would refuse 0"), "{reply}");
        get(&mut mcp, "replay")
    };
    let without = {
        let tmp = tempfile::tempdir().unwrap();
        let db = store(tmp.path(), &["demo"]);
        let mut mcp = Mcp::start(&db, tmp.path());
        let mut args = rule("replay", "Never run `git push --force` on main.", Some("demo"));
        args["targets"] = command_target("git push");
        args["check_kind"] = json!("forbidden");
        args["check_literals"] = json!(["--force"]);
        mcp.call("remember", args);
        get(&mut mcp, "replay")
    };
    assert_eq!(with_calls, without, "the replay changes the receipt, never the item");
}

/// G8.14 (K8(d), K19): a whole-command ban of a "never push until the owner
/// says so" rule refuses every real push; naming one in accept_refusing keeps
/// it on the rule track.
#[test]
fn g8_14_a_ban_meant_to_refuse_every_push_is_kept_by_accept_refusing() {
    let tmp = tempfile::tempdir().unwrap();
    let db = store(tmp.path(), &["demo"]);
    served(
        &db,
        OTHER_SESSION,
        &["git push origin main", "git push origin topic", "git push", "git push origin fix", "git push -u origin feature"],
    );
    let mut mcp = Mcp::start(&db, tmp.path());
    let mut args = heavy(rule("no-push-yet", "Never run `git push` in this repository until the owner says go.", Some("demo")));
    args["targets"] = command_target("git push");
    args["check_kind"] = json!("forbidden");
    args["check_literals"] = json!(["git push"]);
    let reply = mcp.call("remember", args.clone());
    println!("G8.14 red: {reply}");
    assert!(reply.starts_with("REFUSED:") && reply.contains("accept_refusing"), "{reply}");
    args["accept_refusing"] = json!(["git push origin main"]);
    let reply = mcp.call("remember", args);
    println!("G8.14: {reply}");
    assert!(reply.contains("teeth: wired, not proven - a whole-command ban"), "{reply}");
    assert!(reply.contains("refusing these real calls is accepted: git push origin main"), "{reply}");
}

/// Field test 5 (D3): THOR's "Hang it" line was refused whenever the rule also
/// carried a no-literal reason tag - four of five first tries. Hanging a
/// check that refuses on a rule whose tags the call does not touch now drops
/// the reason tag itself, and the reply says so. A call that names the tags
/// and keeps the reason is still held to the contradiction.
#[test]
fn d3_hanging_a_check_drops_the_no_literal_reason_tag_by_itself() {
    let tmp = tempfile::tempdir().unwrap();
    let db = store(tmp.path(), &["demo"]);
    seed_raw(
        &db,
        &Item {
            id: "force-old".to_string(),
            kind: Kind::Rule,
            text: "Never force-push: `git push --force` rewrites shared history.".to_string(),
            bindings: vec![Binding::Target { kind: TargetKind::Command, value: "git push".to_string() }],
            severity: Some(Severity::Costly),
            project: Some("demo".to_string()),
            tags: vec!["no-literal:force pushing leaves no fixed text in the command".to_string()],
            expires: None,
            key: None,
            falsifier: Some("A force push turns out to be harmless here.".to_string()),
            check: None,
        },
    );
    let mut mcp = Mcp::start(&db, tmp.path());

    // Naming the tags and keeping the reason is still the contradiction.
    let reply = mcp.call(
        "revise",
        json!({"id": "force-old", "check_kind": "forbidden", "check_literals": ["--force"],
               "tags": ["no-literal:force pushing leaves no fixed text in the command"]}),
    );
    assert!(reply.starts_with("REFUSED:") && reply.contains("AND the tag"), "{reply}");

    // The call THOR's own "Hang it" line names: no tags in it.
    let reply = mcp.call("revise", json!({"id": "force-old", "check_kind": "forbidden", "check_literals": ["--force"]}));
    println!("D3: {reply}");
    assert!(reply.starts_with("revised 'force-old'"), "{reply}");
    assert!(reply.contains("dropped the tag 'no-literal:force pushing leaves no fixed text in the command'"), "{reply}");
    let item = get(&mut mcp, "force-old");
    assert!(!item.tags.iter().any(|t| t.starts_with("no-literal:")), "{:?}", item.tags);
    assert!(item.check.is_some());
}
