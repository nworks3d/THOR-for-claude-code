# PLAN: route coverage (collect item 71)

A rule anchored to one route of a capability stays silent on another route.
This is a design only. No code was changed. Every number below was counted by
a script over the live store (opened read-only) and the 30-day record of real
calls (`real-calls.jsonl`, 105 MB, read-only), on 2026-11-24. How each number
was counted is given next to it, and what was NOT verified is listed at the end.

## 1. The problem (the incident, and what is already fixed)

2026-11-09, project acme-shop. Rule `fusion-geen-nieuwe-bodies-maken` was
bound to the tool `mcp__Autodesk_Fusion__fusion_mcp_execute` (Command target)
and, from revision 298884, to the host `127.0.0.1:27182`. The session drove
Fusion through `\\nas\Business-projects\hulptools\fusion-mcp-aansturen\fusion-script.ps1`.
The rule never fired and unrequested bodies were made.

Three facts the log and the disk show, which change the shape of the fix:

1. The incident script does NOT contain the host. Its first line is
   `param([Parameter(Mandatory)][string]$Py, [switch]$ReadOnly, [Parameter(Mandatory)][string]$Url)`.
   The host arrives on the command line as `-Url "http://127.0.0.1:27182/mcp"`.
   What the script does contain is the bare MCP tool name `fusion_mcp_execute`
   (`name = "fusion_mcp_execute"`), which is the last part of the bound tool name.
2. The host on the command line was NOT reached at the time, because a URL lost
   its port in `model::normalize::hosts_in_command` (normalize.rs:143). That was
   fixed in 2.6.5 (2026-11-09, CHANGELOG line 151: "A Host binding that names a
   port now fires"). The log confirms it: `item_served` for this rule has 17
   rows; none between 2026-10-29 and 2026-11-12, then the first script-line
   serving on 2026-11-12 (seq 319969, trigger `powershell ... -File "\\nas...\fusion-script.ps1" -Url "http://127.0.0.1:27182/mcp" ...`).
   So the exact incident route is closed for any script that takes the host as
   an argument. Five servings of this rule carry `fusion-script` in their trigger.
3. The rule is `teeth:note` (it informs, it cannot refuse). The gate never had
   anything to refuse with. Route coverage here is about informing.

What remains open is the general class: a script that holds the host, URL or
tool name INSIDE the file, so the command line names none of them.

## 2. Measured size of the gap

Counted with Python scripts in the session scratchpad (deleted afterwards).

How the live rules were found: replayed `event` rows of kind `fact_created`,
`fact_revised`, `fact_retracted` in `seq` order, last one per entity wins,
retracted = gone; kept `kind` in (`rule`, `orientation`) (the kinds
`Kind::can_fire` accepts). Bindings read from the `bindings` JSON of that head.

| # | What | Count | How counted |
|---|------|-------|-------------|
| 1a | Live rules + orientations | 1917 (1127 + 790) | as above |
| 1b | with a Host target | 60 (35 distinct values; 27 are a dotted non-email name or IP) | binding kind `host` |
| 1c | with a tool-name Command target | 56 (36 distinct tools) | Command value starting `mcp__` or one of `model::shell::TOOL_NAMES` (shell.rs:2579) |
| 1d | with either | 111 | union of 1b and 1c |
| 1e | with a non-tool Command target | 409 (358 distinct values) | for the git push question |
| 2a | Shell calls in the last 30 days | 60,703 | `Bash` + `PowerShell` rows, `t` within 30 days |
| 2b | Calls that run a script file | 11,049 (18 percent of shell calls), 13,871 script slots | port of `route_probe::executed_scripts` (route_probe.rs:184) to Python: interpreter + file, `-File`, `&`/`.`/`source`, bare path; script extensions as route_probe.rs:138 |
| 2c | Distinct script paths | 6070; 4420 without `$ * ? % { }` or backtick; 65 on UNC (548 slots) | path resolved against the call's `cwd`, lowercased |
| 2d | Slots whose path holds a variable or glob | 3571 of 13,871 (26 percent) | cannot be resolved at fire time |
| 3a | Scripts whose content is known | 1192 of 4420 paths, covering 3317 of 10,300 slots | 227 still on disk, 1110 have a Write/Edit in the record (229 of those were clipped at 6000 chars, corpus.rs:74), 145 both |
| 3b | Scripts naming a bound HOST literal | 59 scripts, 510 slots; in 487 of those slots the command line does not name the host, so **487 slots are a gap** (4.7 percent of all variable-free script slots, 14.7 percent of those with known content) | substring of a dotted non-email host binding in script text |
| 3c | Rules affected by 3b | 34 distinct rules/orientations | host to rule ids |
| 3d | Scripts naming a full `mcp__` tool name | 1 script (1 slot) | exact |
| 3e | Scripts naming the SUFFIX of a bound mcp tool (text after the last `__`, 8+ chars, with an underscore; 16 suffixes) | 2 scripts: `fusion-script.ps1` (330 slots) and one other (1 slot) | substring |
| 3f | How many rules a gap call would inherit | 3 calls inherit 1 rule, 122 inherit 2, 3 inherit 3, 254 inherit 5, 1 inherits 6, 72 inherit 8, 32 inherit 9 or more. **359 of 487 inherit more than 4** | rules of the host literals the script holds and the line does not |
| 3g | Scripts naming a bound multi-word COMMAND literal (disk only) | 46 scripts, 724 slots, examples `docker inspect`, `git grep`, `powershell 5.1` | very noisy, not a target of this design |
| 4a | Read time of a script, NAS online | UNC (64 files) median 27.7 ms, p95 54.6 ms, max 86.3 ms; local (163) median 20.8 ms, p95 34.0, max 61.2 | Python open+read in a thread, includes thread spawn and a cold cache, so an overestimate for Rust |
| 4b | Script size | median 7,478 bytes, p95 53,890, max 530,007 (`pc-chat.cjs`); the incident script is 1,588 bytes | readable files |
| 4c | Scan of the largest script | 68 ms for 285 literal searches plus 3 regexes in Python | a single generic pass in Rust will be far less; NOT measured in Rust |

What these numbers mean, honestly:

- The gap is real but modest: a lower bound of 487 slots (the content of two
  thirds of the script slots is unknown, because 95 percent of script paths are
  gone from disk by now: scratch scripts and worktrees).
- Whether each of the 487 slots was a rule that mattered was NOT judged. Many
  are likely noise: site build tools that mention `nworks3d.be`
  (311 slots) or `shop.example.org` (149 slots), test files that mention an IP.
  Treat 487 as "the route exists", not "487 missed warnings".
- The only script that matches by tool name is the incident script.

## 3. What the code proves wrong or costly in the proposed direction

1. **A host look-through would NOT have caught the incident script.** The
   script holds no host (section 1). What reaches the incident rule is (a) the
   host on the command line, fixed in 2.6.5, and (b) the tool-name fragment
   `fusion_mcp_execute` inside the script. (b) needs a new equivalence: a bound
   `mcp__<server>__<tool>` is reached by a script that holds `<tool>`. Measured:
   that suffix rule matches exactly one real script (330 slots), so it is
   precise. Design A therefore extracts hosts AND words, not hosts only.
2. **B already exists in part.** `serve::route_probe` (route_probe.rs, 868
   lines, in `serve sweep` only, serve.rs:8110) already finds a rule bound to a
   host that a script reaches without a binding, and prints the `revise` that
   adds the script path. It was taken out of `doctor` in 2.6.6 because it was
   noisy ("named things that are no host at all and scripts that merely live on
   a share named like the host", CHANGELOG line 128). Extend it; do not build a
   second one.
3. **The probe's content source is weak.** It reads the Write/Edit record only
   (clipped at 6000 chars, 229 of 1110 written scripts clipped; heredoc or
   redirect-written scripts are invisible; route_probe.rs:438-450). The new
   shared reader (section 4.1) reads the disk with a deadline and falls back to
   the Write record.
4. **A per-script Path binding is the wrong remedy at scale.** 4420 variable-free
   script paths in 30 days; 227 still exist. And the `revise` the probe prints
   can be REFUSED: `full_reliable_anchor` (store.rs:3307, called at 2680)
   refuses a Path, Dir or Command binding on a place already holding
   `MAX_ITEMS` (4) facts at any weight. That is exactly the incident's "the
   script's place already held 4 facts". Design A removes the need for a per-
   script binding for host and tool routes; B must preflight its suggested
   call with `model::store::capacity` and, when it would be refused or the
   script sits in a tools folder, suggest the Dir of the script's folder
   instead of the file.
5. **The 4-per-block cap bites the inherited facts hard.** 359 of 487 gap calls
   would inherit more than 4 rules (three NAS or printer hosts carry 5 rules
   each). Without a rule for route facts, inherited notes would crowd out the
   direct ones. See 4.4.
6. **A Host place can crowd silently.** The write gate and the evaluation list
   count only Path, Dir and Command places (store.rs:3316-3320 in
   `full_reliable_anchor`, store.rs:3657-3659 in `outranked_places`). Host is
   not counted. The host `127.0.0.1:27182` holds 5 live facts, `10.0.0.61`
   holds 5, `10.0.0.150` holds 5 (counted from live bindings, without
   project scoping): each more than a block shows, and nobody is ever told.
   This session's own hook context showed it: for a Bash call that named the
   fusion tools folder the block showed 4 facts and listed
   `fusion-geen-nieuwe-bodies-maken` among "5 more apply here". So the incident
   rule can lose its place at its own host even when it is reached.
7. **The cache keyed by path plus mtime plus size buys little.** The key needs
   a stat, which on a share costs about as much as a small read (4a), and the
   parse of one script in Rust is milliseconds (not measured). The hook is a
   fresh process for every call, so a cache has to live in a sidecar file with
   its own read/write cost and races. Ship without a cache; add it only if the
   hook timing measurement (section 7) says so.
8. **git push (the closing question): informing by moment is all noise today.**
   Scripts containing `git push` run by a call whose line has no `git push`:
   5 scripts, 35 slots, counting disk and Write record. All five are
   test files or gates that mention the words as data (`pc-chat.cjs`,
   `readonly-commands.test.cjs`, `thor.test.cjs`, `precommit-gate.js`,
   `momentgate.test.mjs`). Zero real wrappers. See 4.6.

## 4. Design

### 4.1 A: the fire-time script look-through (informs only)

New module `thor2/serve/src/script_routes.rs`, registered in
`thor2/serve/src/lib.rs` beside `route_probe` (lib.rs:154).

**Which scripts.** Move `executed_scripts`, `script_argument`, `INTERPRETERS`,
`PASS_THROUGH`, `SCRIPT_EXTENSIONS`, `POWERSHELL_VALUE_FLAGS` from
route_probe.rs:129-217 into the new module and make `route_probe` import them
(one implementation, the test `model/tests/single_normalization.rs` style).
Input is `model::shell::call_segments(tool, command)` (shell.rs:173), already
what the probe uses. Differences the hook needs:

- Keep only paths ending in a script extension (the probe also keeps any first
  word containing `/`, e.g. an `.exe`, which is not a script).
- Skip a path containing `$ % * ? { }` or a backtick (2d: 26 percent of slots).
- Resolve a relative path against the hook payload's `cwd` (`session_cwd`,
  serve.rs:2878), and against a `cd <dir> &&` in an earlier segment when the
  segment parser already tracks it (`input::shell_writes_in` carries the folder
  of a segment, input.rs:104; reuse that, do not re-derive).
- At most 3 scripts per call.

**What is read.** One level: the script file itself, never what it calls.
First 256 KB (covers every measured script except `pc-chat.cjs` at 530 KB;
a bigger file is read partly and that is fine for an informing path). Decode
with `work_folders::decode_text` (work_folders.rs:244, currently private: make
it `pub(crate)`), which handles a UTF-8 BOM and the UTF-16 a PowerShell
redirect writes. Skip a file with a NUL byte in the first 8 KB that is not
UTF-16. Drop full-line comments (`#`, `//`, `REM`, `::`) before extracting:
a host in a comment is text about the host, the same stance as route_probe.rs:17.

**What is extracted** (`Routes { script, hosts, words }`):

- `hosts`: `model::normalize::hosts_in_command` over each remaining line
  (normalize.rs:143), which already yields `host` and `host:port` for URLs and
  bare `host:port` tokens and drops file names. At most 64.
- `words`: the lowercased identifier-like words of 8+ chars that contain `_`
  or are a full `mcp__a__b` name, at most 500. They exist only to be compared
  with the suffix of a bound `mcp__<server>__<tool>` (finding 3.1). They are
  not matched against any other binding.

**Time and size budget (hard).** The whole look-through (stat + read + parse)
runs inside `work_folders::within(Duration::from_millis(400), ...)`
(work_folders.rs:235): a worker thread and `recv_timeout`. A timeout, a missing
file, a permission error, a share that is off: no routes, no message, the call
goes on exactly as before. The hook process exits after the call, so an abandoned
read thread cannot leak. UNC works because the read is an ordinary
`std::fs::read` of a path; the measured NAS-online read is 28 ms median,
86 ms max (4a). NAS OFFLINE WAS NOT MEASURED (see "not verified").
It runs only in the informing block (serve.rs:2875 onward), after
`pre_tool_guard_block` (serve.rs:2844) has had its say, so a refused call never
pays for it.

**Where it plugs in.**

- `ServeInput` (input.rs:235) gets `routes: Vec<RouteTargets>` (script path,
  hosts, words). Filled by a new `ServeInput::add_script_routes` called in
  serve.rs right after `input.add_command(command)` (serve.rs:2883). NOT
  inside `add_command` (input.rs:298), because `add_command` is also used by
  `check`, `why`, `places_of_trigger` and the probes; they must stay pure.
- Candidate pool: `live::candidate_pool_key` (live.rs:177) must add the Host
  kind when `input.routes` holds a host, or host-bound rules are never even
  candidates for a script call that names no host. (This is the sneaky part:
  without it A silently does nothing.)
- Match: `rank::binding_matches` (rank.rs:84) gets two route arms after the
  existing ones: a Host binding matches a route host through the same
  `target_matches`; a Command binding for which `names_a_tool` holds and that
  starts with `mcp__` matches a route word equal to its text after the last
  `__`. A new function `route_of(binding, input) -> Option<&RouteTargets>`
  tells which. `matched_places` (rank.rs:134) must include a route-matched
  binding, or the owner cannot name its place in a `mark` verdict.
- `rank::select` (rank.rs:258): a hit with only route matches gets
  `RankedItem.via: Option<String>` (rank.rs:22) = "script.ps1 names <literal>".
- Rendering (render.rs:143): the line for such a fact says
  `[id] (reached through <script file name>, which names <literal>; the command line does not)`.
- Recording: `deliver::record_delivery_around_anchors` (deliver.rs:189) is given
  a synthetic trigger for route facts, `<command head> (via <script>, names <literal>)`,
  so the evaluation's "where it fired" shows the route and
  `rank::places_of_trigger` (rank.rs:153: it reads the trigger back through
  `add_command` and `add_file`) resolves the Host binding as the place.
  `deliver::redact_secrets` already runs over it (deliver.rs:157). Because the
  fact is served through the ordinary `item_served` event, it owes an ordinary
  verdict in the evaluation's verdict list: that is the visibility for facts
  that WERE reached. No new event kind.
- `check_rows` (serve.rs:806): route-only facts get verdict `advice`, never
  `prohibition`, so no consumer of `verdicts` reads an inferred route as a
  refusal basis.

**It never refuses.** Structurally, not by flag: the refusing path is
`pre_tool_guard_block` (serve.rs:4630) to `guard_verdict::pre_tool`
(guard_verdict.rs:150) to `command` (:353) to `absent_guard::find_command_violation_in`
over the call's own segments. It builds its own inputs and never sees
`ServeInput`. `routes` is read only in rank.rs, render.rs and the recording
call. A test (section 6) pins this.

### 4.2 Depth

One level. A script that runs another script is not followed. Reason: no
measurement says wrappers of wrappers exist, and each level adds a stat and a
read (28 ms on a share) to a hook. Revisit only if the served-via-route count
after shipping (4.3) shows misses a second level would have caught.

### 4.3 B: visibility

Three parts, smallest first.

**B0 (free with A).** A fact reached through a route is an ordinary serving,
so it appears in the evaluation's existing verdict list with its route in the
"where it fired" text, and `mark(place: "<host>")` resolves. Nothing to build.

**B1 (extend what exists).** `serve sweep` already prints `route probe:`
lines (route_probe.rs:530-547, serve.rs:8110). Changes:
- Use the shared reader (disk with deadline, then the Write record) instead of
  the Write record alone (route_probe.rs:438-450), and use the call's `cwd`
  (CallRow.cwd, corpus.rs:130) to resolve a relative script, instead of the
  suffix guess in `Written::find` (route_probe.rs:248).
- Also report the tool-suffix route (finding 3.1) next to the host route.
- Label each gap "A reaches this now" (it will fire as informing) or
  "A cannot reach this" with the reason (path has a variable, file gone,
  content not on record). Only the second kind is a true remaining gap.
- Preflight the printed `revise` with `model::store::capacity`
  (store.rs:2656). If it would be refused, print the Dir of the script's
  folder instead, and say so. The call text itself comes from the existing
  `revise_for` (route_probe.rs:350).

**B2 (the evaluation list, owed).** Only for the gaps A cannot reach, and only
for rules whose check can refuse or whose severity is costly or worse
(`sweep::can_refuse`, sweep.rs; `severity_rank`), because for the others a
missed note is not worth holding a report. Shape, copied from the sweep item
(`d.`):
- Fill: `eval_session.rs:1140` next to `crate::sweep::for_evaluation`, a
  `crate::route_probe::for_evaluation(store, SWEEP_BOX_SECS)` with the same
  time box, frozen into `SignalsSoFar` (model/src/store.rs:8812 area) as a new
  `route_gaps: Vec<RouteGapHit>` (serde default, like `sweep`), capped like
  `EVALUATION_DISPUTE_LIST` (8) with a `route_gaps_more` count.
- Render: a new `route_section` after `sweep_section` (eval_session.rs:2022)
  as item `g.`, in the same four-part voice the file demands: what it means,
  the exact call, when it is done, an example. The call is the preflighted
  `revise`.
- Done: a pure function of store state like `evaluation_outranked_owed`
  (store.rs:3715): the rule's bindings now include a Path or Dir target that
  reaches the example script (`target_matches`). A tag, a reason or a line in the
  report settles nothing. This is "settled only by that real call".
- Cost: a new field in the machine section means a bump of
  `MACHINE_RENDERER_REV` (gate.rs:1718, now 12), which makes an older running
  tool server refuse to file an evaluation until Claude Code restarts (the
  "tool server is an older build" rule in AGENTS.md). Plan one bump for the
  whole change.
- Recommendation: ship B0 and B1 first. Ship B2 only after B1 has run for a
  week and its precision is known, because this list was taken out of doctor
  once already for noise, and an owed item with no honest exit blocks every
  report. See open question 1.

### 4.4 Ranking and the 4-per-block cap for route-inherited facts

The cap is `MAX_ITEMS = 4` (model/src/item.rs:969) applied in `render::cap`
(render.rs:105) to the list `serve_from_candidates` (lib.rs:261) builds.
Measured need: 359 of 487 gap calls would inherit more than 4 rules.

Proposed rule, in `serve_from_candidates` (not in `select`, so `why` still
sees one full list):
1. Direct hits keep their order and their slots. A route-only fact is sorted
   after all direct hits (a new key in `rank::select`, ahead of the severity
   keys but behind nothing else; irreversible direct hits are not displaced by
   a route-only irreversible fact, because the route is an inference).
2. Route-only facts fill only the slots direct hits leave empty, at most 2
   route-only facts per block. If more than 2 apply, the block's existing
   "N more apply here" line names them and says "(through the script)".
3. One exception, open question 2: a route-only fact that can refuse
   (`sweep::can_refuse`) or is irreversible gets one guaranteed slot, taking
   the lowest direct fact's place. Default if undecided: no exception.

This is a ranking change, and AGENTS.md says to ship none without a fresh
measurement on real data: the measurement is a replay of the 487 gap calls
through `serve_from_candidates` before and after (use the `serve sweep` call
loader, corpus.rs:415).

### 4.5 C: how a crowded script place shows up

- A script path (or its folder) with more than 4 Path/Dir-bound facts already
  appears as a crowded place: `outranked_places` (store.rs:3651), listed in
  the evaluation by `crowded_section` (eval_session.rs:2059), at most
  `EVALUATION_OUTRANKED_LIST` = 3 (store.rs:4188). No change.
- Route-inherited facts are bound to a host or tool, so they never count toward
  the script place, and they are held to 2 per block (4.4). They cannot make a
  script place newly crowded.
- New: include `TargetKind::Host` in the anchor filter of `outranked_places`
  (store.rs:3657-3659) so a crowded host place is listed too (finding 3.6:
  three hosts today). Do NOT add Host to the write gate's refusal
  (`full_reliable_anchor`, store.rs:3316) in this change: it would start
  refusing writes on 3 places that already exist, which is a separate decision.
  `place_has_an_honest_exit` (store.rs:3640) already decides which places are
  listed; most host facts are notes, so most are listable.

### 4.6 Does a script that wraps `git push` dodge the push rules today?

Yes, structurally. Two things read only the command line: the moments
(`intent::from_command`, intent/src/lib.rs:296, fed by `ServeInput::add_command`,
input.rs:315) and the refusing checks (`find_command_violation_in` over the
call's segments). A call `./ship.sh` that runs `git push --force` inside has no
`Publish` moment and no `--force` literal. A `pre-push` git hook in
`thor2/githooks` is the only thing that sees the push itself.

Should the informing path cover it? Recommendation: no, not now.
- The measurement is 5 scripts, 35 slots, all five scripts mention `git push` as
  test data or documentation, none runs it. 100 percent noise on the only
  evidence there is.
- A host or a tool name is a noun. A moment is derived from a verb by a regex
  over free text (`from_command`), and a whole file is free text; the same file
  that documents "never run git push" would fire the push rules.
- If a real wrapper ever shows up, the right tool is a Command/Path binding on
  that wrapper (an ordinary rule about `ship.sh`), which the owner can write
  once a wrapper exists. The refusal stays on the real command only, as the
  doctrine says; nothing in this design changes that.

## 5. What it costs at hook time

Per PreToolUse call that runs a script with a resolvable path (about 18 percent
of shell calls minus 26 percent unresolvable, so roughly 13 percent):
one stat plus one read of at most 256 KB, in a worker thread with a 400 ms
ceiling. Measured Python read: 21 to 28 ms median, 34 to 55 ms p95, 61 to 86 ms
max (4a, includes thread spawn, so an overestimate). Parse: one pass; not
measured in Rust. Every other call pays nothing: the look-through starts only
when `executed_scripts` finds a script path. Memory: bounded by 256 KB.

## 6. What it never does

- Never refuses, blocks or holds a turn on an inferred route. Pinned by test.
- Never executes, imports or sources the script. It only reads bytes.
- No network beyond reading a file the OS can open (a share the machine
  already has mounted); no DNS, no connect, no request to a host it finds.
- Never writes the store. B1 and B2 read the store and the record only; the
  `revise` text is a suggestion nothing applies (route_probe.rs:23).
- Never follows a second level, never reads a variable path, never guesses a
  path.
- Never makes a route fact outrank a direct fact.

## 7. Edge cases

| Case | Behaviour |
|------|-----------|
| Script written by redirect or heredoc in the same command (`echo ... > x.ps1; ./x.ps1`) | File absent when the hook runs: no routes. The next call that runs it is covered. Silent. |
| Missing file, permission denied, directory | No routes, no message. |
| Huge file | First 256 KB only. |
| Binary or UTF-16 without BOM | NUL bytes in the first 8 KB: skipped. |
| UTF-16 with BOM (PowerShell redirect) | Decoded by `decode_text`. (Of 227 readable scripts today, 0 are UTF-16; kept because PowerShell redirects write it.) |
| UNC share off or slow | 400 ms ceiling, then no routes. A dead share can block a `CreateFile` far longer, but only the worker thread blocks and the process exits after the call. NOT measured offline. |
| Variable or glob in the path (`$w\list.py`, `*.test.cjs`) | Skipped (26 percent of slots). Not guessed. |
| Relative path | Resolved against the payload `cwd` and a preceding `cd`; if that cannot be told, skipped. |
| Same script twice in one call | Read once. |
| Script path equals a Write in the same turn that is not on disk yet | Not read from the record at fire time (the hook has no record access); B1 uses the record offline. |
| `--no-record` / held store dir | The look-through still runs (it reads only); the synthetic trigger is recorded exactly as any serving is, so nothing extra is written. |
| A host in a comment | Dropped with full-line comments; an inline trailing comment is not stripped (accepted noise). |
| A host that is a variable (`http://$host:27182`) | `hosts_in_command` yields nothing useful; no route. The `-Url` argument form is covered by the command line. |

## 8. Tests to add

Unit (script_routes.rs): the interpreter table moved from the probe keeps
every existing probe test green (route_probe.rs:655-855 stays as is);
comments are dropped; UTF-8 BOM and UTF-16 decode; NUL file skipped; 256 KB
cap; path with a variable is not read; relative path resolves against cwd and
`cd`; word extraction keeps `fusion_mcp_execute` and ignores short or
underscore-free words.

Matching (rank.rs): a Host binding is reached by a route host and by
`host:port` exactly (same port rule as input.rs:484 tests); a binding with
another port is not reached; `mcp__Autodesk_Fusion__fusion_mcp_execute` is
reached by the word `fusion_mcp_execute` and NOT by `execute` or a shorter
suffix; a route-only fact gets `via`; a direct hit has no `via`.

Pool (live.rs): `candidate_pool_key` includes `host` when only `routes` carry a
host (the silent-failure test).

Cap (lib.rs): 3 direct + 5 route-only: 4 shown, route facts only in empty
slots, at most 2, the rest named in the "more apply" line; direct hits never
displaced; with the exception of open question 2 chosen, the guaranteed slot.

Never-refuses (serve/tests): a hook payload for `powershell -File x.ps1` whose
file holds a host, with a live rule bound to that host that has a REFUSING
`forbidden` check whose literal is in the script: the hook output has the
fact as context and no `decision: block`. Same payload with the host in the
command line: unchanged behaviour (it already refuses by the guard path).

Recording: the `item_served` trigger carries `via <script>`;
`places_of_trigger` resolves the Host place; `mark(place:)` accepts it.

Hook budget: a script on a path that never answers (a closed local port is not
a file; use a path in a directory the test makes unreadable, or a fake
`Job` that never replies) returns within the ceiling and the call output is
unchanged.

Probe (B1): disk content found when the Write record is clipped; relative path
resolved by `cwd`; the printed `revise` is replaced by a Dir suggestion when
`capacity` would refuse (fixture: 4 facts on the script path).

Evaluation (B2, if shipped): a frozen `route_gaps` round-trips with an old
record that lacks the field; the item is done only when the binding exists;
`MACHINE_RENDERER_REV` bump covered by the skew test.

Crowded host (C): a host with 5 facts is listed by `outranked_places`; a host
with 4 is not; the gate still accepts a 5th Host binding.

Replay measurement (before shipping the cap): the 487 gap calls through
`serve_from_candidates`, direct-hit slots unchanged in every case.

## 9. Open questions (real choices left)

1. B2 exit. The evaluation needs an honest "this route is not meant" exit, or
   an owed gap with a false positive blocks the report. The existing exits are
   all store-state changes (`revise`, `retract`, a verdict mark). There is no
   field that records "route X is not covered on purpose". Options: (a) none,
   so B2 stays informational until precision is measured (recommended);
   (b) `mark` with a noise verdict at the route's Host place (reuses the place
   verdict machinery, but it says "the note did not belong", which is not the
   same claim); (c) a new `revise` field. Owner to choose.
2. A guaranteed slot for a route-only fact that can refuse or is irreversible
   (4.4 point 3). Default: no.
3. Tool-suffix matching (finding 3.1) is the only way the incident class is
   caught by content. It ties to a convention (the script uses the server's
   own tool name). Accept that, or restrict A to hosts and leave the incident
   class to the Path/Dir binding (4.3 B1 suggests the tools folder).
4. Host in `outranked_places` only, or also in the write gate (4.5)? Default
   here: places list only.

## 10. What was NOT verified

- Nothing was built or run in Rust: no test, no timing of the Rust parse, no
  compile of any of this. All hook-time numbers are Python reads.
- A UNC share that is OFF was not measured (the NAS was online).
- The 487 gap slots were counted, not judged: how many were a rule that
  mattered is unknown. Content is known for 3317 of 10,300 script slots only.
- Script content was read as it is now, not as it was at the time of each
  call; a script edited since may differ. The Write record is the content at
  write time, clipped at 6000 chars.
- The Python port of `executed_scripts` is an approximation (quote-aware split
  on `; | && ||` and newline, no heredoc or `$()` handling). The Rust version
  (route_probe.rs:184 on `call_segments`) is more exact; counts in 2b to 3g
  may differ by a few percent.
- `test_signal.rs` and `sweep.rs` internals (`can_refuse`, `for_evaluation`)
  were read only by their call sites, not line by line.
- The Host-place crowding counts (5 facts on three hosts) ignore project
  scoping and the `kind`/decay filters the live pool applies.
- The claim that the hook runs the look-through after the guard relies on the
  order read at serve.rs:2844 and 2875; not exercised.
