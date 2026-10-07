# SPEC - enforcement (Lane B safety catches + Lane C capture guard)

Implementable contract for `PLAN-NEXT.md` sections 0.1, 4 (B3) and 4b (C1-C4).
Written against the code as it stands: `serve/src/respond.rs` (the response
guard) and `serve/src/bin/serve.rs::hook_once` (the hook dispatcher).

Scope of this spec: the SAFETY MODEL (B3) and the CAPTURE GUARD (Lane C).
B1/B2 (the action guard with a judge model) build on the same safety model and
are specified only far enough to make B3 binding on them. Section 6 adds the
write side of the safety model: how a check gets onto a blocking surface at
all. Section 7 adds the evaluation: how it starts, how its report is filed and
checked, and who is asked for it.

---

## 0. What already exists (do not rebuild)

- `respond::Rule` + `respond::evaluate` - a dependency-free, case-insensitive
  substring matcher: `all_of` AND (`any_of` OR empty) AND NOT `none_of`, with a
  `min_chars` floor. Rulebook is JSON, read from beside the store.
- `hook_once` dispatches by `hook_event_name`: `Stop` (guard, handled first and
  WITHOUT opening the store), `SessionStart`, `UserPromptSubmit`, everything
  else = PreToolUse-shaped.
- `Stop` already honours `stop_hook_active` - a turn that was already blocked
  once is never blocked again. That is the anti-loop guarantee and every rule
  below inherits it.
- Every hook payload carries `session_id` and `cwd`.

The capture guard REUSES this matcher and this rulebook format. It does not get
a matcher of its own. A second rulebook file is the only new data.

---

## 1. B3 - the two safety catches (binding on every enforcement surface)

### 1.1 Three verdicts, one default

```
Verdict = BLOCK | WARN | ALLOW
```

- **BLOCK** only on a conclusive, CITED violation: the surface must be able to
  print WHICH rule fired and WHAT text triggered it. A block that cannot name
  its own cause is a bug, not a strict setting.
- **WARN** = emit text (context/reminder), never stop the action. The cheap
  outcome. A false WARN costs a line; a false BLOCK costs trust.
- **ALLOW** = silence. This is the DEFAULT for every path that is not
  conclusive, including every error path.

Hard rule (mirrors R5 and the existing guard doc): any error - unreadable
rulebook, malformed JSON, missing field, store that will not open, judge that
does not answer - is ALLOW. Never BLOCK, never WARN. An enforcement layer must
never itself become the reason work cannot proceed.

### 1.2 Freshness (anti-stale), default closed

A fact may only BLOCK if its currency is proven AT THAT INSTANT:

1. **Pointer facts** (preferred): the fact stores a rule plus a pointer to the
   canonical source, never a copied value. The value is read LIVE at
   enforcement time. A pointer fact cannot be stale; if the pointer does not
   resolve, the verdict is ALLOW (not BLOCK) and the fact is flagged for revise.
2. **Stored-value facts**: may only BLOCK if they carry a machine-runnable
   currency check that passes at that instant. Check fails -> ALLOW + flag for
   revise (auto-demote). Check missing or unrunnable -> ALLOW.
3. **Code facts**: never a stored snapshot. Enforce against live code through
   the code index, which already carries the commit and the working-copy drift
   on every hit. Drift present -> ALLOW (cannot prove current).

Restated as one invariant, and this is the non-negotiable from section 0.1:

> **There is no path on which a fact whose currency is not proven at this
> instant emits a BLOCK.**

Every enforcement surface must have a test named after that invariant.

### 1.3 What this costs

Soft-fail means the catch rate is bounded by what can be proven current. That
is the intended trade: a missed catch is invisible, a false block is not.

---

## 2. Lane C - the capture guard

Goal: a durable decision stated by the owner cannot end the turn unrecorded.

### 2.1 C1 - flag the decision (UserPromptSubmit)

New rulebook `guard-capture-rulebook.json`, beside the store, same JSON shape as
`guard-response-rulebook.json`, matched with `respond::evaluate` against the
prompt text.

Two confidence tiers, expressed as two rulebooks or one field `tier`:

- **tier `block`** - unambiguous normative statements. Examples of `any_of`
  terms (Dutch and English, the owner works in both): "vanaf nu", "voortaan",
  "nooit meer", "altijd ", "de nieuwe ", "nieuwe huisstijl", "from now on",
  "never again", "the new rule", "always ", "never ".
- **tier `warn`** - weaker signals ("ik wil liever", "beter is", "prefer",
  "should").

`none_of` carries the usual escapes plus the ones this guard needs: quoting or
discussing a rule ("any_of", "for example", "bijvoorbeeld", "als voorbeeld") and
any text that is clearly a question rather than a statement.

On a match, write a marker keyed by `session_id` into a small state file beside
the store (`capture-owed.json`):

```json
{"<session_id>": {"tier": "block", "quote": "<the matched line, trimmed to 200 chars>",
                  "seq_at_flag": <max event seq at flag time>, "blocked_once": false}}
```

`seq_at_flag` is the whole satisfaction mechanism: it is the store's max event
sequence number at the moment of flagging. Nothing else needs to be tracked.

Failure policy: any error writing the marker = no marker. Never fail the prompt.

### 2.2 C2 - enforce at turn end (Stop)

In `hook_once`'s `Stop` branch, AFTER the response guard has had its say (the
response guard stays first and keeps its no-store guarantee; the capture check
is the one Stop path that may open the store, read-only):

1. `stop_hook_active == true` -> return None. Unchanged, inherited anti-loop.
2. No marker for this `session_id` -> return None.
3. Marker present: read the store's max event seq now. If any item-declaring or
   item-revising event exists with seq > `seq_at_flag`, the debt is paid ->
   clear the marker, return None.
4. Debt unpaid and tier is `block` and `blocked_once == false`:
   set `blocked_once = true`, emit
   `{"decision":"block","reason":"[THOR] You were told a durable decision (\"<quote>\") and did not record it in THOR. Record it now with remember or revise."}`
5. Debt unpaid and tier is `warn`, or `blocked_once == true`: clear the marker
   and return None. **One block per flag, ever.** This is what keeps it from
   becoming a nag.

Store cannot be opened, marker file unreadable, JSON malformed -> return None
(ALLOW). Same fail-open as everything else on this surface.

### 2.3 C3 - block the wrong sink (PreToolUse)

Narrow on purpose, so it cannot become a false-block generator. It fires ONLY
when BOTH hold:

- a capture is owed this session (a tier `block` marker exists and is unpaid),
  AND
- the tool call is a Write/Edit whose `file_path` basename is a known mirror
  sink: `CLAUDE.md`, `BRAND.md`, `AGENTS.md`, or a path segment `decisions/`.

Then BLOCK with: "this is a durable decision; record it in THOR first - the .md
is a mirror, never the source." Outside that intersection: ALLOW, silently.

Rationale for the narrowness: without an owed capture, an edit to CLAUDE.md is
ordinary work and blocking it would be exactly the kind of nag this project
exists to remove.

### 2.4 C4 - house style captures the pointer, not the values

When the flagged decision concerns house style / brand / colors, the block
reason must additionally say: "store the POINTER (which source file is
authoritative) plus the rule, never a copied value." This is the write-side of
section 1.2 layer 1 - it is what makes the resulting fact stale-free by
construction.

Detection: the same rulebook, a rule whose id starts with `style-`.

---

## 3. Tests that must exist (each named after the defect it prevents)

Capture guard:
1. `a_stated_decision_that_was_never_recorded_blocks_turn_end`
2. `a_stated_decision_that_was_recorded_does_not_block` (seq moved past
   `seq_at_flag`)
3. `the_same_flag_never_blocks_twice` (`blocked_once`)
4. `a_warn_tier_signal_never_blocks`
5. `a_question_about_a_rule_is_not_a_decision` (none_of escape)
6. `a_missing_or_broken_capture_rulebook_blocks_nothing` (fail-open)
7. `a_marker_from_another_session_is_ignored` (session_id keying)
8. `an_edit_to_claude_md_without_an_owed_capture_is_not_blocked` (C3 narrowness)

Safety model (B3), independent of surface:
9. `a_fact_whose_currency_cannot_be_proven_never_blocks`
10. `a_failed_currency_check_demotes_instead_of_blocking`
11. `every_block_reason_names_the_rule_and_the_trigger`

A refusal ground without a test does not exist (the project's own P2 rule).

---

## 4. Acceptance, and it must be POWERED

The lesson from the underpowered behaviour test (n=14) is binding here.

- Build two sets: (a) prompts that DO state a durable decision, (b) prompts that
  do not but look like it (questions about rules, quoted rules, hypotheticals,
  ordinary work). Minimum 60 per set, drawn from real session transcripts where
  possible, not invented in one sitting.
- Report **false-block rate** (target ~0) and **catch rate**. Report both, always
  together. A catch rate with no false-block rate is not a result.
- The measurement runs against a COPY of the store, never the live one.

---

## 5. Deployment - NOT part of the build

Nothing here is deployed to `C:\Users\dev\thor2\bin` or wired into
`settings.json` without the owner saying so in-session. A surface that can block
turn completion changes his daily work; the build and the numbers come first,
the switch is his call.

---

## 6. The two tracks - how a check reaches a blocking surface

Section 1.2 says a fact may only BLOCK when its currency is proven at that
instant. This section is the write side of the same rule:

> **A new or changed check reaches the store only after it refused the call or
> write it forbids and let every clean form through, in the same pipeline the
> hook runs.**

A check stored before this trial existed stays live until `retest` decides it
(6.5): a guard that works is never switched off only because nothing has tried
it yet.

### 6.1 Every write is a trial

- `remember`, `revise`, `pin` and `unpin` run every Rule/Orientation, whatever
  its severity, through `serve::teeth::trial`. A revise that changes only tags,
  falsifier, expires or key runs none and keeps the verdict it had.
- Every attempt runs through `serve::guard_verdict` - the one decision
  pipeline `serve hook` runs - on a one-item in-memory store, with synthetic
  payloads. The trial never writes a file and never appends to a real store.
- Three outcomes. RULE: the forbidden form was refused and every clean form let
  through; stored with its check. NOTE: stored with no check (`teeth:note`); it
  keeps its severity, so it still ranks and informs as before. REFUSE: nothing
  written; one `GateRefused` by actor `teeth-write` records it and never
  changes the item.
- Clean forms are built by THOR, never taken from the log: the anchor alone,
  the listing forms (`git tag -l`, `git branch --list`, ...), the help forms
  (`<anchor> --help`, `-h`, `--version`), and for `contains` the file as it
  stands, an edit that keeps the literal, and the file minus its first line
  that overlaps no occurrence of the literal (when it has one). A collapsed
  check (its anchor contains its literal) is tried as a whole-command ban: the
  call itself must be refused, and a mention (`echo <anchor>`) must pass - for
  an anchor of two or more words also `<program> --version`, and for three or
  more the anchor without its last word.
- A tooth derived from the rule's own text - a backtick or quoted span, or a
  flag, after a Command anchor; a span that stands in a file the rule is
  anchored to - FORCES the write when it bites, passes every clean form, and
  refuses no real call from the log that another session made before the id's
  first refused write. The refusal names both polarities when both bite and
  both exits: hang the tooth, or dispute it (`legitimate_calls`,
  `may_change`), which lands a note by dispute. Each of the three arguments
  needs `because` (one sentence, at least 20 characters): the write is refused
  without it, the reason lands in the item's history, the evaluation lists
  every dispute with it and hands the unverified ones to the agent, and doctor
  prints it beside the dispute ("no reason recorded" for an older one).
- A check that would refuse more than half of at least five real calls it
  reaches in the log is refused as too broad; `accept_refusing`, naming one of
  the listed calls, keeps it on the rule track.
- A4: a check whose literal the rule's own text never names is refused when the
  check is attached or changed. A pre-existing A4 failure never blocks a write
  that leaves the check alone; the item stays undecided.
- The verdict rides in the write's own event as a `teeth_trial` record.
  `model::store::current_verdict` counts it only while its fingerprint (kind,
  text, bindings, check, project, severity) equals the head's. A `teeth:` tag
  is a readable copy only: written only together with a record that implies
  it, dropped by any write that changes the fingerprint without one, and never
  read to decide anything.
- Proven versus wired is a label, never a settlement: proven needs evidence
  from reality - a field refusal (6.4), the `contains` literal in the
  committed file (`HEAD`), or an `absent` literal the file's git history once
  held.

### 6.2 The root a check is judged under

- A file check is tried in the project's registered working copy
  (`model::checkout::registered_checkout`, re-exported by `serve::project`).
  It is read only from events by actor `installer` and validated at every
  read: the folder exists, is the top of a main checkout (not a linked
  worktree) and still resolves to the project. There is no fallback to the
  session's folder, the tool server's startup folder or the code index.
- No registered working copy: REFUSE, naming `install.exe --register-root
  --db "<store>"`, when the writing session is inside that project; otherwise
  NOTE with the check kept pending (`NoWorkingCopy`), never live. False in the
  registered checkout but true in the session's own checkout: NOTE pending
  (`NotTrueInRegisteredCheckout`). False in both: REFUSE, naming what is
  missing. An OLD file check whose project has no registered working copy is
  `Blocked(NoWorkingCopy)`: it keeps its check and is counted apart.
- The hook's own root is the top of the checkout (`checkout_top`); a relative
  path is read against the session's folder, and in a shell write against the
  folder its segment runs in (6.3). The guards' project is the folder's
  project, else the session's last project
  (`serve::project::session_project`); a session that drifted out of its
  project resolves its path guards under that project's registered working
  copy (`serve::project::guard_root`).

### 6.3 The door - command checks

- `serve::shell::segments` is the one definition of "this anchor reaches this
  call", used by every guard arm, serving, the trial and the real-call corpus.
  It splits the way a shell does (separators, new lines, pipes, quoting,
  heredocs, `$(...)`, backticks, loops) and descends into `ssh`, `sh -c`,
  `eval`, `powershell -Command`, `cmd /c` and `Invoke-Expression`, in the Sh
  and the PowerShell dialect.
- A Command-bound forbidden literal is looked for in the reached segment's
  unit: its pipeline, its heredoc bodies and the last `cd` before it on the
  same line. An Always-bound forbidden literal reads the whole text.
- Token boundaries: a forbidden literal and a `requires` trigger match only
  where each word-character edge has a neighbour that is absent, whitespace, a
  quote or one of `; | & ( ) { } < > = , : / \`; on the right, `.` counts only
  when no word character follows it. The content checks (`contains`, `absent`,
  `absent_all`) and a `requires` check's answers keep substring matching.
- An anchor written with `.exe` reaches only a call that names that `.exe`. A
  `requires` trigger ending in one `*` is a prefix. A call whose only words
  after the anchor are help or version words is never refused by `requires`
  (a `forbidden` check is not exempted).
- Each reached segment is judged under the project of the folder it runs in
  (the `cd` chain, and for git its `-C`, `--git-dir` or `GIT_DIR=`; never
  `--work-tree`, which names no repository); a folder of no project, one
  written with `$` or `~`, or a network path keeps the session's project.
- The shell-write guard reads the same segments, and never treats a segment
  behind `ssh` as a write to a local file.

### 6.4 Field evidence

`serve::attest::is_field_refusal`: a `gate_refused` by actor `hook`, from a
UUID-shaped session with served activity, after the check was set, carrying a
`tool_use_id`, AND a recorded `attester` verdict that the session's own
transcript holds that tool_use and a tool_result for it with `is_error` true
whose content starts "PreToolUse:<Tool> hook error: [THOR]" and names the rule
id. The transcript is found by session id, never by a path a payload names. It
is checked once, at that session's `Stop`, and the verdict is recorded in the
log; a refusal never verified whose transcript is gone is never proof. A field
refusal sets the proven label and nothing else.

Self-proof is never proof: a refusal the evaluating session met inside its own
evaluation (from its start to its filing), and a refusal in the session that
wrote the rule's current check, are recorded with `field: false` and the reason,
and doctor counts both shapes apart. What this cannot close is named in 7.8
(N15).

### 6.5 One reader, and retest

- `model::store::settlement` is the one answer for the evaluation filing gate,
  the `Stop` hook and `doctor`: Settled on a current RULE or NOTE verdict,
  while a blocker holds (`NoWorkingCopy`, `WorkingTreeDiffers`, `AnchorFull`), or when
  retracted or archived; otherwise Owed. Rot is every live Rule/Orientation
  that is Owed, whatever its severity. A tag, a reason, an open point or a
  refusal on record settles nothing. Read for one evaluation (`Some(start)`), a
  rule handed to the agent (the batch, the A4 list) is settled only by a verdict
  written after that start, by a retract or a move off the rule track, or, for a
  `contains` check whose literal was gone, by the literal being back.
- `serve retest --id <id> | --project <p> | --all [--apply]` runs the same
  trial with no session. Exit 0 a verdict it can write, 3 blocked, 4 needs the
  agent, 1 error, 2 usage. `--apply` writes one `fact_revised` by actor
  `retest`, only while the head is still the one tried (compare-and-swap).

### 6.6 Tests that pin section 6

`serve/tests/hook_and_trial_share_one_pipeline.rs` (golden outputs recorded
before the pipeline moved), `mcp/tests/two_tracks_at_the_write.rs`,
`mcp/tests/every_refusal_names_a_way_on.rs` (every refusal names a way on the
agent can run itself, or routes to the owner),
`serve/tests/command_tooth_reaches_every_segment.rs`,
`serve/tests/requires_star_trigger.rs`, `serve/tests/shell_write_guard.rs`,
`serve/tests/hard_guards_stay_in_their_project.rs`,
`ops/tests/install_registers_root.rs`,
`serve/tests/stop_records_refusal_verifications.rs`,
`serve/tests/retest_cli.rs`,
`serve/tests/stop_hook_and_filing_gate_read_one_settlement.rs`,
`mcp/tests/replica_refuses_what_it_can_judge.rs`,
`model/tests/cli_cannot_forge_a_box.rs`.

## 7. The evaluation - started in the log, filed through an attested call

Part B of the same build. The evaluation is the one place where an agent's own
report decides whether a turn may end, so every step of it is read from the log
and from Claude Code's own record of the session, never from a file or from the
agent's word.

### 7.1 Write identity

Every write tool (`remember`, `revise`, `retract`, `mark`, `pin`, `unpin`,
`resolve`) stores `att` in its event body: the call's `tool_use_id`, the digest
of its canonical input, the server's session hint, and whether the `PreToolUse`
hook saw the call in flight (`seen`, `unseen`, `side-door`). The hook appends
`[THOR] write attested: <tool_use_id> <first 16 hex of the digest>` to a write it
lets through. At the session's `Stop`, each new write is checked once against
the session's own record (a tool call of that name, that digest, with a result
naming the event) and the answer is written to the log as an `attester` event.
A verdict counts toward the judgement debt, and toward an evaluation's frozen
list, only when it was verified that way.

### 7.2 The start, and the three frozen lists

An evaluation starts with a start record in the log (`eval-machine`), written
when the routine is opened or the `Stop` hook asks. It freezes the judgement
list, the batch (at most `EVALUATION_TEETH_BATCH`) and the A4 list (at most
`EVALUATION_A4_LIST`) - none of which grows - and is followed, inside a time box
(`MACHINE_PASS_BOX_SECS`), by the machine pass: THOR tries the project's old rules
itself and writes a rule or a note as `teeth-eval` (the one old-stock verdict as
`retest`), or one open point of its own per blocker. A pass cut off is finished
by the next `Stop`. A write to a frozen item outside an attested call of that
evaluation voids it (D10); the next `Stop` starts a fresh one.

### 7.3 The filing

An evaluation report is stored only by `remember` or `revise`, through an
attested call of the evaluating session, for an open, non-void evaluation of the
same project. THOR puts a machine section - a pure function of the log up to the
filing - in front of the agent's text, and refuses a later edit of it. It is
refused while THOR is still deciding, while the frozen judgement list, batch or
A4 list is not settled, while the turn's own writes are not verified yet, and
when a frozen item was changed behind the evaluation. The model command line,
a second client and an older binary are refused outright.

### 7.4 The validator

The first main-session `Stop` after a filing validates it once
(`serve::eval_filing::validate`) and records an `eval-validator` event: a THOR
call in the session's record, an evaluation started in that session with its
start note shown to it, the machine section byte for byte as the log renders it,
no D10 write in the window, the filing gate holding on the log as it stood.
Only a valid report counts: for the ask, the clocks, the carry, and `doctor`. A
missing record of the session is invalid; a record in a shape THOR cannot read
is drift: nothing counts, no evaluation is asked for, every turn says so. Drift
is only THOR failing to read tool calls it expects (a large record with a
`tool_use` structure no known line matches, or with no readable call although an
attested THOR write of the session proves one was made), never size plus "no
tool call this turn": a chat-only turn is large through injected context alone.

### 7.5 Who is asked

Per project: its own work clock (every session together, parallel minutes once)
under the cadence the build runs (`EVAL_CADENCE`; `WeeklyUnlessRot` is built and
off), AND at least `EVAL_OWN_WORK_MINUTES` of the session's own work there (the
larger of the clock file and the session record), AND not its first `Stop` in the
project, AND no other session running that project's evaluation for less than
`EVAL_ELSEWHERE_MAX_MINUTES`. The weekly floor (`EVAL_FLOOR_DAYS`) reads the log
alone. Only a new valid evaluation resets the clocks.

The ask rides with every block that holds a first-pass `Stop` before its own
turn - the Response Guard, setup, the capture check, the crowding question - in
the same block after that reason, so none of them starves it; the `Stop` is
counted once, before any verdict. Since step 28 (2.5) no `Stop` holds a turn
for the teeth question (rules with no current verdict from THOR's trial) or for
the judgement debt: both are asked inside the evaluation (its batch, its A4 list
and its frozen judgement list), at most once per 16 hours per project. The
judgement debt counts a fact once per session.

### 7.6 Open points and the owner queue

Open points are THOR's own (`open-no-working-copy-*`,
`open-working-tree-differs-*`, `open-anchor-full-*`), reserved to `eval-machine`
at the store, written and closed by the machine pass. `Blocker::AnchorFull` holds
a `path_exists` check on a file the rule is not anchored to when anchoring it
there is refused by the crowding cap (the place already holds the most facts it
ever shows): its point names the full place and every occupant for the owner, the
rule keeps its true check and is never on the agent's batch, and the point closes
when the place has room. A check an agent clears, moves or changes in an
evaluation's window is counted and listed in the machine section and in `doctor`. Rules with no project whose check reads a file
(`Blocker::global_rules`) share ONE store-wide point,
`open-no-working-copy--global-rules` (`GLOBAL_RULES_POINT_ID`), in no project:
every project's evaluation holds them by it (never on its batch), its carry is
counted across every project's valid evaluations together, and it is counted
once in the owner queue (`store_wide_owner_queue`), against no project's budget. A subject still open after a valid evaluation was
filed is carried and is in the owner queue; over `OPEN_POINTS_OWNER_BUDGET` a
project's session start opens with a banner. No agent can file, tag or remove
one.

### 7.7 Tests that pin section 7

`mcp/tests/evaluation_end_to_end.rs`, `model/tests/cli_cannot_file_an_evaluation.rs`,
`serve/tests/write_attestation_marker.rs`, `serve/tests/evaluation_handover_hook.rs`,
`serve/tests/evaluation_debt_stop_hook.rs`, `serve/tests/owner_queue_banner.rs`,
`serve/src/eval_session/pass_tests.rs`, `serve/src/eval_filing/tests.rs`,
`serve/src/usefulness/clock_tests.rs`,
`serve/tests/the_teeth_instructions_agree.rs` (the routine and AGENTS.md say what
the code does), `ops/tests/install_eval_command.rs` (the seeded routine files a
TLDR and remarks only and never asks for an open point),
`serve/tests/teeth_debt_rotates_and_follows_the_session.rs`.

### 7.8 What stays open (named limits)

| # | What stays open | Why | Fallback |
|---|---|---|---|
| N15 | A session that plants a refused call OUTSIDE its own evaluation window (before its evaluation starts, or after it filed), on a rule it did not write, looks exactly like a real call and can set that rule's proven label | Nothing tells a planted call from a real one; excluding every refusal of a session that later evaluates would also drop real refusals | Doctor counts the self-proof shapes apart; the record of every field refusal names its session and log seq; a proven label settles nothing (proven versus wired only) |
