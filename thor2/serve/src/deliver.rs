//! R3: delivery is observable. Every item actually shown in a block writes an
//! `ItemServed` event (the variant already exists in `thor_core::event_store`,
//! unwired until this crate) so "when did this last fire" and "how often" are
//! answerable, and "declared but never delivered" is a query (see `audit.rs`).
//!
//! This is called ONLY from the `hook` channel - the real production
//! delivery boundary. `check`/`why` call the same select+render path to
//! PREVIEW a block but must not count as a real firing, or "how often did
//! this actually fire" would be inflated by every dry run a human tried.
//! This split is a design choice the brief itself does not spell out (it says
//! "elke levering", every delivery) - flagged here rather than assumed silently.
//!
//! Fails open exactly like the rest of the read/serve boundary: a failed
//! write here must never stop the block that was already decided from
//! reaching stdout, and must never make `hook` speak or exit non-zero.

use model::served::ItemServed;
use thor_core::event_store::{EventKind, EventStore};

/// What a likely secret is replaced by.
pub const REDACTED: &str = "[redacted]";

/// `text` with likely secrets replaced by `[redacted]` (step 31). The log keeps
/// the command a fact fired on, and so do its backups: a token typed in a
/// command line must not go with it. Four shapes, in this order:
///
/// 1. an `Authorization` header's value;
/// 2. the value after `=` or `:` in a name that holds `token`, `secret`,
///    `password`, `passwd`, `pwd` or `key` (`API_TOKEN=...`, `?key=...`), and
///    the value after a `--password`, `--token`, `--secret` or `--api-key` flag;
/// 3. well-known token shapes (`sk_live_`, `ghp_`, `github_pat_`, `xoxb-`,
///    `AKIA...`, `tskey-`, `sk-ant-`, `AIza`, a JWT, a PEM private key
///    header): the ONE list the write gate refuses in a note too, kept in
///    `model::secrets` so the two can never drift apart;
/// 4. any other long run of 32 or more letters and digits that is all hex, or
///    mixes upper case, lower case and digits (a hash or base64 blob). A long
///    hyphenated name or a path is neither.
///
/// It looks for likely secrets and never claims to find all of them. What a
/// rule's trial needs from a command (the program, its flags, the paths)
/// stays as it was.
pub fn redact_secrets(text: &str) -> String {
    use regex::Regex;
    use std::sync::OnceLock;
    static RULES: OnceLock<Vec<(Regex, String)>> = OnceLock::new();
    static LONG_RUN: OnceLock<Regex> = OnceLock::new();
    let rules = RULES.get_or_init(|| {
        let keep_then = |pattern: &str| (Regex::new(pattern).expect("a fixed pattern"), format!("${{1}}{REDACTED}"));
        vec![
            keep_then(r#"(?i)(authorization\s*[:=]\s*)(?:(?:bearer|basic|token)\s+)?[^\s'"]+"#),
            keep_then(
                r#"(?i)([A-Za-z0-9_.-]*(?:token|secret|passw(?:or)?d|pwd|key)[A-Za-z0-9_.-]*\s*[=:]\s*)("[^"]*"|'[^']*'|[^\s'"&;|]+)"#,
            ),
            keep_then(r#"(?i)(--?(?:password|passwd|pwd|token|secret|api-?key|apikey)\s+)("[^"]*"|'[^']*'|[^\s'"-][^\s'"]*)"#),
        ]
    });
    let mut out = text.to_string();
    for (pattern, replacement) in rules {
        out = pattern.replace_all(&out, replacement.as_str()).into_owned();
    }
    let mut kept = String::with_capacity(out.len());
    let mut from = 0;
    for found in model::secrets::find(&out) {
        kept.push_str(&out[from..found.start]);
        kept.push_str(REDACTED);
        from = found.end;
    }
    kept.push_str(&out[from..]);
    let out = kept;
    let long_run = LONG_RUN.get_or_init(|| Regex::new(r"[A-Za-z0-9+_-]{32,}={0,2}").expect("a fixed pattern"));
    long_run
        .replace_all(&out, |found: &regex::Captures| {
            let run = &found[0];
            let hex = run.chars().all(|c| c.is_ascii_hexdigit());
            let mixed = run.chars().any(|c| c.is_ascii_digit())
                && run.chars().any(|c| c.is_ascii_uppercase())
                && run.chars().any(|c| c.is_ascii_lowercase());
            if hex || mixed {
                REDACTED.to_string()
            } else {
                run.to_string()
            }
        })
        .into_owned()
}

/// Record one `ItemServed` event per shown item id, with no `trigger` - the
/// plain entry point for every caller that either has none to give (`hook`'s
/// own `UserPromptSubmit` arm - a prompt is not a command, a file, or the
/// pinned `SessionStart` block) or does not care (every test in this
/// workspace that seeds servings without meaning to prove anything about
/// where they fired). Strictly a thin call onto `record_delivery_with_
/// trigger` below, never a second copy of the write - see that function's
/// own doc comment for what a trigger is and where the three real
/// production call sites (`bin/serve.rs`) get theirs.
pub fn record_delivery(
    store: &mut EventStore,
    session_id: &str,
    lineage_id: &str,
    actor: &str,
    served_at: &str,
    ids: &[String],
) {
    record_delivery_with_trigger(store, session_id, lineage_id, actor, served_at, ids, None);
}

/// The session id of the `i`-th of several firings of one fact in a fixture:
/// the first is `base` itself, every other one a session of its own. The
/// judgement debt counts a fact once per session (step 28), so a fixture that
/// wants "fired N times" has to fire it in N sessions.
pub fn nth_session(base: &str, i: usize) -> String {
    match i {
        0 => base.to_string(),
        _ => format!("{base}-{i}"),
    }
}

/// The longest `trigger` a served event records verbatim - a command line
/// can run to any length, and the point of this field is naming the general
/// shape of where an item fired (see `ItemServed::trigger`'s own doc
/// comment), not reproducing a whole shell pipeline byte for byte. Chosen to
/// comfortably hold a realistic command or file path while keeping the body
/// small; truncated by CHARACTER count, not by byte-slicing, so a multi-byte
/// UTF-8 command (a path with a non-ASCII directory name, say) is never cut
/// mid-codepoint.
///
/// 400 since step 30 (field test 5, defect D2): at 120 most real `ssh ...
/// docker exec ... sh -c "..."` calls were cut, the trial of a rule could not
/// see the literal in them, and "would refuse 0" was read as safe. Events an
/// older build wrote stay cut at `LEGACY_TRIGGER_LIMIT`.
pub const TRIGGER_LIMIT: usize = 400;

/// What a build before step 30 kept of a trigger. Only read, to recognise a
/// cut call in an old event.
pub const LEGACY_TRIGGER_LIMIT: usize = 120;

/// Record one `ItemServed` event per shown item id, each carrying `trigger`
/// - see `ItemServed::trigger`'s own doc comment for what the three shapes
/// (a command, a file path, or `"session start"`) mean and the defect naming
/// them closes. Every other behaviour (fail-open on a broken log, one event
/// per id) is identical to `record_delivery` above, which is now simply this
/// function called with `None`.
///
/// `trigger` is truncated to `TRIGGER_LIMIT` HERE, once, rather than trusted
/// to every caller: `bin/serve.rs`'s own call sites pass a raw command line
/// or file path straight through, and a single shared truncation point is
/// the only way that limit can never quietly drift between them.
pub fn record_delivery_with_trigger(
    store: &mut EventStore,
    session_id: &str,
    lineage_id: &str,
    actor: &str,
    served_at: &str,
    ids: &[String],
    trigger: Option<&str>,
) {
    let trigger: Option<String> = trigger.map(redact_secrets).map(|t| {
        if t.chars().count() > TRIGGER_LIMIT {
            t.chars().take(TRIGGER_LIMIT).collect()
        } else {
            t
        }
    });
    for id in ids {
        append_served(store, session_id, lineage_id, actor, served_at, id, trigger.clone());
    }
}

fn append_served(
    store: &mut EventStore,
    session_id: &str,
    lineage_id: &str,
    actor: &str,
    served_at: &str,
    id: &str,
    trigger: Option<String>,
) {
    let Ok(body) = serde_json::to_string(&ItemServed { served_at: served_at.to_string(), trigger }) else { return };
    let _ = store.append_event(session_id, lineage_id, actor, EventKind::ItemServed, id, None, &body);
}

/// `record_delivery_with_trigger`, keeping for each item the part of `trigger`
/// AROUND the item's own anchors (`anchors_of` gives them per id) instead of
/// the first `TRIGGER_LIMIT` characters (step 27, D9). Field case 2026-10-01:
/// a long command named an item's file far from its start, the head was all
/// that was kept, and the evaluation was shown "...ESTIMATOR.md CL" as the
/// place the item fired. The excerpt carries its own "..." marks, as
/// `model::store::trigger_excerpt` writes them.
pub fn record_delivery_around_anchors(
    store: &mut EventStore,
    session_id: &str,
    lineage_id: &str,
    actor: &str,
    served_at: &str,
    anchors_of: &[(String, Vec<String>)],
    trigger: Option<&str>,
) {
    // Redacted whole BEFORE it is cut, so a secret cannot survive in the part kept.
    let trigger = trigger.map(redact_secrets);
    for (id, anchors) in anchors_of {
        let excerpt = trigger.as_deref().map(|t| model::store::trigger_excerpt_to(t, anchors, TRIGGER_LIMIT));
        append_served(store, session_id, lineage_id, actor, served_at, id, excerpt);
    }
}

/// Record what the write guard actually DID: refused a call, or stood aside
/// with something outstanding it chose not to raise again this session.
///
/// The stand-aside used to mean "a call it had already refused once", which is
/// what the file and command arms did until 2026-08-08. Neither does any more -
/// no prohibition stands aside at all. What is left is the stale-rule nudge at
/// Stop, which holds off for the rest of a session after it has been paid once.
///
/// WHY BOTH, AND WHY THE SECOND ONE MATTERS MORE. A refusal is visible to the
/// person it refused. A gate that quietly declines to look is visible to
/// nobody, which is exactly how the once-per-session marker survived from the
/// first day of 2.0 until four separate reviews read it out loud on
/// 2026-08-08. Until this function existed, nothing in the log recorded that
/// enforcement had happened at all: the gate was the whole reason this
/// version was built and the one capability with no measurement, which is the
/// third time this project has been caught by that exact shape.
///
/// Fail-silent like every other telemetry write here: a log that cannot take
/// a measurement must never cost a refusal.
pub fn record_gate_outcome(
    store: &mut EventStore,
    session_id: &str,
    lineage_id: &str,
    actor: &str,
    at: &str,
    refused: bool,
    entity_id: &str,
    target: &str,
) {
    record_gate_outcome_for_call(store, session_id, lineage_id, actor, at, refused, entity_id, target, None);
}

/// `record_gate_outcome`, also carrying the id of the tool call the outcome
/// was about (`tool_use_id` in the hook's payload), so the call can later be
/// found in the session's own transcript (`serve::attest`). A refusal with no
/// id can never be checked there, and is never counted as field evidence.
#[allow(clippy::too_many_arguments)]
pub fn record_gate_outcome_for_call(
    store: &mut EventStore,
    session_id: &str,
    lineage_id: &str,
    actor: &str,
    at: &str,
    refused: bool,
    entity_id: &str,
    target: &str,
    tool_use_id: Option<&str>,
) {
    let mut body = serde_json::json!({ "at": at, "target": redact_secrets(target) });
    if let Some(id) = tool_use_id.filter(|id| !id.trim().is_empty()) {
        body["tool_use_id"] = serde_json::Value::String(id.to_string());
    }
    let body = match serde_json::to_string(&body) {
        Ok(b) => b,
        Err(_) => return,
    };
    let kind = if refused { EventKind::GateRefused } else { EventKind::GateStoodAside };
    let _ = store.append_event(session_id, lineage_id, actor, kind, entity_id, None, &body);
}


/// COPY MODE twin of the delivery records: what `record_delivery*` would have
/// appended to the log goes to the per-machine telemetry sidecar instead
/// (`thor_core::copy_mode`). One line per call, the trigger redacted and cut
/// like the log's own. Never touches the store.
pub fn copy_telemetry_served(db: &std::path::Path, session_id: &str, actor: &str, ids: &[String], trigger: Option<&str>) {
    if ids.is_empty() {
        return;
    }
    let trigger: Option<String> = trigger.map(|t| redact_secrets(t).chars().take(TRIGGER_LIMIT).collect());
    thor_core::copy_mode::telemetry(
        db,
        "item_served",
        serde_json::json!({ "session": session_id, "actor": actor, "ids": ids, "trigger": trigger }),
    );
}

/// COPY MODE twin of `record_gate_outcome_for_call`.
pub fn copy_telemetry_gate(db: &std::path::Path, session_id: &str, actor: &str, refused: bool, rule: &str, target: &str, tool_use_id: Option<&str>) {
    thor_core::copy_mode::telemetry(
        db,
        if refused { "gate_refused" } else { "gate_stood_aside" },
        serde_json::json!({
            "session": session_id, "actor": actor, "rule": rule,
            "target": redact_secrets(target).chars().take(TRIGGER_LIMIT).collect::<String>(),
            "tool_use_id": tool_use_id,
        }),
    );
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::serving::serving_stats;

    #[test]
    fn recording_delivery_is_queryable_immediately_after_the_write() {
        let mut store = EventStore::in_memory().unwrap();
        record_delivery(&mut store, "s", "l", "serve", "2026-08-02T12:00:00Z", &["x1".to_string()]);
        let stats = serving_stats(&store);
        let s = stats.get("x1").expect("must be queryable right after the write, per R3");
        assert_eq!(s.times_served, 1);
        assert_eq!(s.last_served_at.as_deref(), Some("2026-08-02T12:00:00Z"));
    }

    /// `record_delivery` itself never carries a trigger (see its own doc
    /// comment) - proven here so the plain, no-trigger path this crate's
    /// many other tests already depend on stays pinned down alongside the
    /// new one below.
    #[test]
    fn recording_delivery_carries_no_trigger() {
        let mut store = EventStore::in_memory().unwrap();
        record_delivery(&mut store, "s", "l", "serve", "2026-08-02T12:00:00Z", &["x1".to_string()]);
        assert_eq!(model::served::last_trigger(&store, "x1"), None);
    }

    /// THE DEFECT THIS CLOSES: an evaluation could not see WHERE an owed
    /// item had actually been firing (measured 2026-09-17, acme-shop
    /// eval 1 and 2 - see `model::served::ItemServed::trigger`'s own doc
    /// comment). `record_delivery_with_trigger` is the half of the fix that
    /// writes it; `model::served::last_trigger` (proven in `model`'s own
    /// tests) is the half that reads it back.
    #[test]
    fn record_delivery_with_trigger_carries_the_trigger_in_the_body() {
        let mut store = EventStore::in_memory().unwrap();
        record_delivery_with_trigger(
            &mut store,
            "s",
            "l",
            "hook",
            "2026-09-17T00:00:00Z",
            &["x1".to_string()],
            Some("git push --force origin main"),
        );
        assert_eq!(model::served::last_trigger(&store, "x1").as_deref(), Some("git push --force origin main"));
    }

    /// THE TRUNCATION: a command line can run to any length, and the point
    /// of this field is naming the general shape of where an item fired, not
    /// reproducing a whole shell pipeline verbatim - so a trigger longer than
    /// `TRIGGER_LIMIT` is cut down to it, at this one write site, rather than
    /// trusting every future caller to remember to.
    #[test]
    fn a_trigger_longer_than_the_limit_is_truncated() {
        let mut store = EventStore::in_memory().unwrap();
        let long = "x".repeat(TRIGGER_LIMIT + 50);
        record_delivery_with_trigger(&mut store, "s", "l", "hook", "2026-09-17T00:00:00Z", &["x1".to_string()], Some(&long));
        let trigger = model::served::last_trigger(&store, "x1").expect("a trigger was given");
        assert_eq!(trigger.chars().count(), TRIGGER_LIMIT, "must be cut down to the limit, not left full length");
        assert_eq!(trigger, "x".repeat(TRIGGER_LIMIT));
    }

    /// Field test 5 (D2): the place a command fired is kept to a few hundred
    /// characters, not 120, so a rule's trial can read the call it is judged on.
    #[test]
    fn a_command_of_a_few_hundred_characters_is_recorded_whole() {
        let mut store = EventStore::in_memory().unwrap();
        let call = format!("ssh nas 'docker exec app sh -c \"{}\"'", "echo hello && ".repeat(15));
        assert!(call.chars().count() > 200);
        record_delivery_around_anchors(
            &mut store,
            "s",
            "l",
            "hook",
            "2026-10-02T00:00:00Z",
            &[("x1".to_string(), vec!["ssh".to_string()])],
            Some(&call),
        );
        assert_eq!(model::served::last_trigger(&store, "x1").as_deref(), Some(call.as_str()));
        let huge = format!("ssh nas {}", "y".repeat(3000));
        record_delivery_around_anchors(
            &mut store,
            "s",
            "l",
            "hook",
            "2026-10-02T00:00:01Z",
            &[("x2".to_string(), vec!["ssh".to_string()])],
            Some(&huge),
        );
        let kept = model::served::last_trigger(&store, "x2").unwrap();
        assert!(kept.chars().count() <= TRIGGER_LIMIT + 6, "a huge call is still cut: {}", kept.chars().count());
    }

    #[test]
    fn recording_delivery_never_touches_the_items_head_set() {
        // ItemServed is head-neutral (core::cas/auditor already treat it that
        // way); this pins it from the serve side too: recording a delivery
        // must never change what `live_items` reports for that entity.
        use model::item::{Binding, Item, Kind};
        let mut store = EventStore::in_memory().unwrap();
        let item = Item {
            id: "x2".to_string(),
            kind: Kind::Rule,
            text: "do the thing".to_string(),
            bindings: vec![Binding::Always],
            severity: None,
            project: None,
            tags: vec![],
            expires: None,
            key: None,
            falsifier: Some("this item turns out to be wrong for this synthetic fixture".to_string()),
            check: None,
        };
        model::store::declare(&mut store, "s", "l", "a", &item).unwrap();
        record_delivery(&mut store, "s", "l", "serve", "2026-08-02T12:00:00Z", &["x2".to_string()]);
        let live = crate::live::live_items(&store);
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].item, item);
    }

    #[test]
    fn recording_delivery_for_an_unknown_id_never_panics() {
        // hook is fail-open at the boundary; the delivery write for an id
        // that was never declared must be a harmless no-op-ish event, not a
        // crash (defense in depth even though the caller only ever passes
        // ids that select() just returned).
        let mut store = EventStore::in_memory().unwrap();
        record_delivery(&mut store, "s", "l", "serve", "2026-08-02T12:00:00Z", &["ghost".to_string()]);
        let stats = serving_stats(&store);
        assert_eq!(stats.get("ghost").map(|s| s.times_served), Some(1));
    }

    /// Step 31: a command that carries a secret never lands in the log with it.
    /// The tokens below are made up for this test.
    #[test]
    fn a_secret_in_a_command_is_redacted_before_it_is_stored() {
        let fake_token = "ghp_0123456789abcdefghijABCDEFGHIJ012345";
        let calls = [
            format!("curl -H 'Authorization: Bearer {fake_token}' https://example.test/api"),
            format!("deploy.sh --env prod API_TOKEN={fake_token}"),
            "ssh nas 'PASSWORD=hunter2hunter2 ./run.sh'".to_string(),
            "tool --password hunter2hunter2 --verbose".to_string(),
            "export DB_SECRET: \"s3cr3t-value-here\"".to_string(),
            "curl https://example.test/x?key=abcdef123456".to_string(),
            "echo 5d41402abc4b2a76b9719d911017c5925d41402abc4b2a76b9719d911017c592".to_string(),
            "echo QUJDREVGR0hJSktMTU5PUFFSU1RVVldYWVowMTIzNDU2Nzg5YWJjZGVm".to_string(),
        ];
        for call in &calls {
            let mut store = EventStore::in_memory().unwrap();
            record_delivery_with_trigger(&mut store, "s", "l", "hook", "2026-10-02T00:00:00Z", &["x1".to_string()], Some(call));
            let kept = model::served::last_trigger(&store, "x1").unwrap();
            for secret in [
                fake_token,
                "hunter2hunter2",
                "s3cr3t-value-here",
                "abcdef123456",
                "5d41402abc4b2a76b9719d911017c592",
                "QUJDREVGR0hJSktMTU5PUFFSU1RVVldY",
            ] {
                assert!(!kept.contains(secret), "{secret} survived in {kept}");
            }
            assert!(kept.contains("[redacted]"), "{kept}");
            let mut again = EventStore::in_memory().unwrap();
            let anchors = vec![("x1".to_string(), vec!["ssh".to_string()])];
            record_delivery_around_anchors(&mut again, "s", "l", "hook", "2026-10-02T00:00:00Z", &anchors, Some(call));
            assert!(!model::served::last_trigger(&again, "x1").unwrap().contains(fake_token));
        }
    }

    /// The refusal's own record carries the refused command, so it is redacted too.
    #[test]
    fn a_refused_command_is_stored_without_its_secret() {
        let mut store = EventStore::in_memory().unwrap();
        record_gate_outcome(&mut store, "s", "l", "hook", "2026-10-02T00:00:00Z", true, "r1", "tool --password hunter2hunter2 --force");
        let events = store.get_events_by_entity("r1").unwrap();
        assert_eq!(events.len(), 1);
        assert!(!events[0].body.contains("hunter2hunter2"), "{}", events[0].body);
        assert!(events[0].body.contains("--force"), "{}", events[0].body);
    }

    /// The known-token list is the write gate's own (`model::secrets`), so a
    /// shape it refuses in a note is scrubbed from a served command too.
    #[test]
    fn the_shapes_the_write_gate_refuses_are_scrubbed_from_a_command_too() {
        let anthropic = format!("{}{}", ["sk-", "ant-"].concat(), "x".repeat(30));
        let kept = redact_secrets(&format!("curl --data {anthropic} https://example.test"));
        assert!(!kept.contains(&anthropic), "{kept}");
        let body = "MIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQC7VJTUt9Us8cKj";
        let pem = redact_secrets(&format!("printf '-----BEGIN PRIVATE KEY-----\n{body}\n-----END PRIVATE KEY-----' > key.pem"));
        assert!(!pem.contains("BEGIN") && !pem.contains(body), "{pem}");
        // The header alone is public: a command that only names it stays readable.
        let header_only = "grep -c '-----BEGIN PRIVATE KEY-----' key.pem";
        assert_eq!(redact_secrets(header_only), header_only);
    }

    /// What the trial of a rule reads stays readable: ordinary commands, paths
    /// and long hyphenated names are not touched.
    #[test]
    fn an_ordinary_command_is_left_alone() {
        for call in [
            "git push --force origin main",
            "node --test --test-timeout=120000 C:/work/acme/serve/src/deliver.rs",
            "cat mechanische-agentbrief-verbiedt-subagenten-in-een-lange-naam.md",
            "ssh nas 'docker exec app sh -c \"echo hello\"'",
            "npm run keyboard-test --key-count 3",
        ] {
            assert_eq!(redact_secrets(call), call, "{call}");
        }
    }
}
