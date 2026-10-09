# THOR 2.0

A memory for a coding agent, and a gate that can actually stop a wrong write.

THOR stores what you want an agent to keep knowing: rules, orientation, reports,
lookups and code. It hands the relevant part back at the start of a session and
again just before a tool runs. What makes 2.0 different from a pile of notes is
the second half: a rule can carry a machine-runnable proof of its own currency,
and a rule whose proof runs and holds right now is allowed to block a write
outright. Nothing else may block. Prose can inform, never forbid.

There is no API key, no external model and nothing extra to install. Everything
runs on this machine, in one process, next to the agent.

## The doctrine, in four lines

1. Only a rule whose check runs and holds right now may block a write.
2. A rule backed by prose alone may warn, never block.
3. If a check cannot run (the file is gone, the path does not resolve), nothing
   is blocked. It is reported as needing review.
4. Never widen a trigger to buy a catch. A rule that fires on everything is a
   rule nobody reads.

`CONTRACT.md` is the full version: nine requirements, each naming the failure it
makes structurally impossible and the test that enforces it.

## What is in this directory

| where | what |
|---|---|
| `core/`, `model/`, `intent/`, `serve/`, `mcp/`, `library/`, `codeindex/`, `ops/` | the eight crates. This is the product. |
| `CONTRACT.md` | the standard the build is judged against. Read this first. |
| `SPEC-ENFORCEMENT.md` | how the enforcement layer is specified. |
| `JUDGE-TRANSPORT.md` | the write-up of the judge transport experiment. |
| `deploy/` | the container builds: a replica for a NAS or a server, and the stand-alone memory server that seeds its own store on first start. |
| `eval/` | the measurement record and the one-off scaffolding behind it. Ignored by git in full. |

`eval/` is ignored deliberately and not as an oversight. It holds measurement
data taken from a live memory - real notes, real project names - and that is
private by definition, however neutral any individual file looks. The same goes
for `target/`: several gigabytes that one `cargo build` reproduces.

This directory sits inside the THOR repository rather than beside it. Every
numbered version of THOR is the same project rebuilt, so it belongs in the same
place; the number says a real rebuild happened, not that a new project started.

## The eight crates

| crate | job |
|---|---|
| `model` | the item model, the write gate, and the check runner. The gate is what refuses a declaration that cannot be honoured. |
| `core` | the append-only event log the whole thing is built on. |
| `intent` | reads what an agent is about to do and turns it into a moment a rule can bind to. |
| `serve` | everything the agent sees: session start, the pre-tool gate, the write guard, lookup. |
| `mcp` | the sixteen tools an agent calls: seven write and state operations (remember, revise, retract, mark, pin, unpin, resolve), four read operations (get, history, status, lookup), three code analysis tools (search_code, where_used, outline), and two library tools (library, shelve). |
| `library` | the everyday-knowledge library behind the two library tools - shelves and entries, kept apart from the event log above. This crate cannot reach that log at all: it depends on nothing from that side. |
| `codeindex` | a map of every symbol in your source, rebuilt from the code itself. It is what answers "who calls this" and "what breaks if I change it". |
| `ops` | install, doctor, backup, sync. |

## Build it

You need a Rust toolchain. Nothing else.

```bash
cd thor2 && cargo build --release --features semantic
```

**The `--features semantic` flag is not optional, and leaving it off fails
silently.** Without it the binaries still build, still run, and still answer
every literal-text query correctly. What stops working is meaning-based lookup:
it returns nothing at all, with no error anywhere. This has cost real debugging
time more than once.

The reliable way to tell the two builds apart is size. A semantic `serve.exe` or
`mcp.exe` is over 20 MB because it carries the embedding model runtime. A build
without the flag is a few megabytes:

```bash
ls -la target/release/serve.exe
```

Under 20 MB means you built the wrong thing. Build again with the flag.

A release build is reproducible on Windows: building the same commit twice
produces the same binary, so a published program can be checked by building
that commit again.

## Install it

One command does the whole setup, and in the common case it takes no arguments:

```bash
target/release/install.exe
```

It creates the store if there is not one yet, wires in the four hooks, and
registers the tool server the agent writes through. The two files it writes are
found on their own - the per-user `settings.json` for the hooks and
`~/.claude.json` for the tool server. Both are backed up to `<path>.bak` before
anything touches them, nothing this tool did not put there is ever removed, and
a second run reports everything as already present and writes nothing.

A store it just created also gets twenty-two pinned notes. Ten are on how to
write a fact that comes back: anchoring it to what it is really about,
correcting instead of duplicating, giving a new project its own scope in one
command, keeping life and work in separate places, never inventing a place to
file something, keeping one entry to one thing, what a refusal actually is,
that words inform while only a proof forbids, saying what actually happened,
and answering whether a rule can refuse. Eleven more are the honesty,
agent-spawning and memory-hygiene habits that hold on any project regardless
of who is running it: never fabricating a measured value, counting coverage
instead of remembering it, verifying a claim against the real source, never
verifying a change with the mechanism it just touched, naming a model on
every spawned agent, keeping a mechanical brief free of sub-agents and
self-review, never blocking a turn on a notification, treating a mid-task
message as one that will not redirect a running agent, matching a check's
literal to the target file's own words, reasoning every fix for whoever
installs this next, and serving a fact as a constraint rather than a command.
One more is the odd one out: it walks the owner through his whole first
session once, before any other work - not only the answer guard - and the
session is held until it is retracted; retracting it is refused until his
answers are on record. That note carries the first-session defaults in its own words (short replies,
the owner's language, work lane only, every starting note kept), and so does the
turn-end reminder that holds the session, so a fresh agent that never read
AGENTS.md finishes the setup in one sentence without asking anything.

Fifteen more notes make up the workflow starter pack, tagged
`workflow-starter` (`ops::install::workflow_starter_pack`): general
coding-workflow habits that were marked useful again and again in the store of
the person who built this, rewritten to hold on any project - a changelog line
with every user-visible change when the project keeps a changelog, tests run
and the real result quoted before "done", no secret in a commit, push hygiene,
a scan before anything goes public, complete agent briefs, and a few more.
None is pinned: each is bound to the moment or command it is about (commit,
push, publish, delete, deploy, install, an agent being spawned, a `cd`, a
shell one-liner, `Start-Process`) and arrives right before it, once per
conversation. Only two carry a check, and only on a Windows install: no
`Start-Process` without a hidden form unless it opens something the user asked
to see, and no `schtasks /create` with `/it`, so an agent never flashes a
console window. The other candidates were replayed through the real guard over
145,976 recorded tool calls and every one refused a legitimate call (or only
looked safe because the call it targets never occurred), so each stays a note:
a new user must never get a false block from a rule they did not write. They all go in through
`model::store::declare`, the same gate every other write uses, and a refusal
is reported rather than worked around - a memory whose own gate rejects the
notes it ships with is worth seeing. An EXISTING store is never seeded, so
upgrading never pushes anything into someone's real notes.

It also seeds a starting rulebook for the answer guard described below, the
first time it finds none sitting next to the store: the same five example
rules, so the `Stop` hook has something to check from the first session
instead of silently checking nothing. An existing rulebook - the owner's
own, or one an earlier install already wrote - is left exactly as it is,
whether the store itself is brand new or not.

The two written files default to Claude Code's own per-user locations - not a
guess, but the one documented place each lives, printed before it is used and
backed up first. `--settings` and `--mcp-json` send them elsewhere (a project's
own `.mcp.json`, say), `--no-mcp` installs a read-only memory on purpose, and
`--db`, `--serve-exe`, `--mcp-exe` and `--code-index-root` override the rest.

It also writes `thor-eval.md`, the end-of-session evaluation routine, to
Claude Code's own commands folder, the first time it finds none there: a
generic template with this run's own programs folder and store filled in,
so running `/thor-eval` settles whatever the memory has been served without
a verdict. Written even under `--no-mcp`, since the file should already be
there the day writes are turned on. An existing copy - the owner's own
edited version, or one an earlier install already wrote - is left exactly
as it is.

Opening that routine starts an evaluation, and so does the `Stop` hook's ask
for one. The start is a record in the memory's own log (`evaluation-start-<project>-<seq>`,
written by the reserved actor `eval-machine`), never a file beside the store:
only the hook knows which session is which, so it writes the record, and the
tool server finds it again by the session id it runs under. The record
freezes three lists, none of which can grow afterwards: the judgement
list (what fired in that session and in 40 sessions or more since its last verdict,
a fact counting once per session),
the batch (at most 8 old rules only an agent can fix) and the A4 list (at
most 16 rules whose check bites and whose text only has to name its literal).
Right after it, THOR's machine pass tries the project's old rules itself, in a
time box of 20 seconds (`THOR_MACHINE_PASS_SECS`, 1 to 200), and writes a
rule, a note or an open point of its own for each; what it could not decide
goes to the batch or the A4 list, and the start note shows both with the exact
call for each item. A pass the hook did not finish (it was killed, or the box
ran out) is finished by the next `Stop`. Keep the routine under a name of your
own as well? List that name, one per line, in `eval-command-names.txt` beside
the store, and opening it under that name starts an evaluation too.

An evaluation report is filed through `mcp__thor__remember` only. The hook
stamps the call in flight (its `tool_use_id` and the input's digest), the
tool server stores them with the write, and THOR writes the report's MACHINE
SECTION itself in front of the agent's text: what was judged, what the
machine decided, which rules were refused, which writes came from outside an
attested call (side doors), which open points were carried. It is a pure
function of the log up to the filing, and a later `revise` that edits it is
refused; only the TLDR and remarks below it can change. The model command
line, a second client and an older binary cannot file one at all. A filing is
also refused while the batch or the A4 list is not settled, while THOR is still
deciding, while the verdicts given in the same turn are not checked yet, and
when a frozen item was changed outside an attested call of the evaluation (that
voids it; the next `Stop` starts a fresh one).

At the first main-session `Stop` after a filing, THOR validates it once and
records the answer in the log (`eval-validator`, `valid` or not, with the
reason and the carried open points): a filing that is a THOR call of this
session, for an evaluation THOR started in it, with the machine section exactly
as the log gives it and nothing on the lists changed behind it. A report that is
not valid counts for nothing, the next ask names it and why, and every project
therefore owes one fresh evaluation after the upgrade that introduced this (the
older reports carry no stamp). A missing session record makes a report invalid;
a record whose tool calls THOR cannot read (Claude Code changed its format) is
drift: nothing counts, no evaluation is asked for, and every turn says so until
THOR is updated. A long turn with no tool call at all is not drift. `eval-debt-state.json` beside the store is a copy for the hooks' speed
only: the `Stop` overwrites its facts from the log every time.

Who is asked, and when. The `Stop` hook asks for the whole routine by name,
per project and not per folder, when:

- the project's own work clock (every session of the project together, parallel
  minutes counted once, in `eval-debt-state.json`) shows 60 minutes since the
  newest VALID evaluation, in the rhythm of the user's plan (`cadence_in`; one
  evaluation per week per project: a valid report covers the next 7 days on a
  smaller plan, strictly, and on Max the next 7 days too but only 24 hours while
  the project shows rot - a defect found, a false-block sweep hit, an owed batch
  or dispute item, a report that did not count, or an open THOR bug candidate -
  until a counting evaluation finds none; there is no repeat after a few hours of
  work; the plan is read from Claude Code's own `~/.claude.json`
  (`organizationRateLimitTier`, undocumented, failing soft) and kept with its
  source in the mode sidecar, `install --set-plan` records the user's word, and
  `install --set-weekly-check off` switches the automatic ask off, `install
  --set-backup local|<place>|off` chooses the backup, which a fresh install does
  not make, and `install --set-recording off` stops the record of real calls);
- this session has at least 10 minutes of its own work in that project since
  then (the larger of the clock file's number and the session record's own
  count, so zeroing the file delays the ask at most until the record is read);
- it is not this session's first `Stop` in that project;
- no other session runs that project's evaluation right now (an open, unfiled
  start whose session showed activity on the log in the last 30 minutes,
  however old the start is): then the session gets one line naming that
  session, no hold, and no second evaluation starts. Once that session has
  been quiet for 30 minutes, the next ask takes the evaluation over and closes
  the old one as superseded (never void: its verdicts stay). A session's own
  open evaluation is asked for first, whatever the clock says.
- ONE HOLDER PER PROJECT (2.5): per project the holder of the evaluation ask is
  the session with the open evaluation, else the one holding the lease the ask
  took (`debt-holders.json` beside the store, released at a `Stop` that holds
  nothing, and at the retry `Stop` once the evaluation is no longer open),
  else the first one asked. Another session is not asked while the holder
  showed activity on the log in the last 30 minutes; it gets one line naming
  the holder. Holds about the session's own writes (a fact on a full place,
  setup, a stated decision, the Response Guard) stay per session.
- NO JUDGEMENT ASK AND NO RULE-UPKEEP ASK AT A TURN'S END (2.5): the owner's
  decision, 2026-10-02 - one block, in the evaluation, instead of something to
  judge every quarter of an hour. What used to hold a `Stop` for the items that
  fired repeatedly since they were last judged, or for the old rules THOR's
  trial has not decided, is asked inside the project's evaluation only (its
  frozen judgement list, its batch and its A4 list), which runs once a week
  per project. A fact counts ONCE PER SESSION toward the 40 that make a verdict
  owed, however often it was shown there, so 40 means about 40 sessions. The
  stale-rule guard still speaks once per session at most.

Inside a turn the ask already held, the `Stop` of that turn holds again while
the session's evaluation is open and its report not filed - only once the
session did something since the last hold, and at most four times - asking to
file in that same turn; after a void it starts the fresh evaluation the void
promised. With nothing done since the hold it lets go with one line saying the
report is still owed; that line, and a void notice (shown once per session),
are messages for you only and never ask the assistant to answer. An item another session changed through an attested
THOR call counts as settled by that session (credited in the machine section)
and never voids the evaluation.

The weekly floor holds regardless of the clock: a project with activity in
the last 7 days (an item served or a write from a Claude Code session) and no
valid evaluation in 7 days is due. The ask follows the work, not the folder: a
session that worked in project A is asked about A from another folder, and a
leading line says so. It is not once per session: it blocks the first `Stop` of
EVERY turn, naming how many times it has already asked and since when, until a
valid report is on file. A folder that belongs to no project is never asked on
its own, since there would be no collection to file that report under. A second
cadence, `WeeklyUnlessRot` (once a week, daily while the project shows rot), is
built and off: it is a constant in the code, not a setting, and flipping it is
the owner's decision; `doctor` prints which one runs. "Accrued" means real
activity - `SessionStart`, a prompt, a tool call, or `Stop` - with a gap of
half an hour or more between two of them counting as a pause, never as work.
Only a new valid evaluation, read from the log at a `Stop`, resets the clocks.
When something else holds that `Stop` first - a reply that breaks a reply rule,
the first-session setup, a decision the owner stated that was not stored, or a
note stored onto a full place - the ask is not lost: it comes in the same block,
after that reason, and the `Stop` is counted once.

A FULL PLACE IS THE AGENT'S (2.5): a rule whose check needs a place it is
not anchored to, when that place is full, goes to the agent's batch with a line
naming every occupant (severity, and whether its check can refuse) and the
three honest exits - fold, move to a narrower anchor, retract a stale one with
a reason - then the rebind. Only when every occupant is a heavier rule whose
check can refuse is it THOR's own open point (`anchor-full`). Retracting a rule
whose check holds warns in the reply and is listed under the machine section's
checks.

THE OWNER'S GO (2.5): a `requires` check tagged `owner-approval` is answered
only by the owner's own words - a real user turn of the session's main
transcript, this turn or the one before, with quoted and fenced text left out;
never the call itself, a tool result or a subagent's hand-back. An unreadable
transcript refuses (fail closed). Its marker is exempt from A4, so the text
need not publish it. At `SessionStart` THOR marks, once, every rule whose
requires answer is only approval markers (`go-<name>`).

FACTS FOR THE OWNER'S CHAT ONLY (2.5): a fact tagged `interactive-only` is left
out of the session start block and the prompt facts of a headless session, such
as `claude -p`. THOR reads how the session was started from
`CLAUDE_CODE_ENTRYPOINT` (else from the head of the session's transcript);
every entrypoint that starts with `sdk` is headless, `claude-desktop` and `cli`
are not, and a session of unknown kind is served as before. Facts bound to a
file or a command still fire at the tool call in every session. Tag a fact with
`revise` and its `tags`. The session start block is ranked: this project's own
facts first, then the global heavy rules (irreversible, costly), then house
style, then the global facts with no weight at all; inside each step, what this
project's own sessions were shown in the last 14 days goes first. A headless
start also leaves out the facts whose only binding besides "always" is the
moment of answering or of claiming something done (the Stop guard watches the
reply), says how many and which in `volume-log.jsonl` (`lo`/`li` for
interactive-only, `ao`/`ai` for reply-only, on the `entry` and `start` lines),
and carries neither the working copy line nor the open-points lines. Those two
lines show once a day per project in the owner's own sessions.

A CALL THAT ONLY LOOKS (2.5): when every part of a Bash call only reads
(`cat`, `grep`, `rg`, `head`, `tail`, `ls`, `find` without `-delete` or
`-exec`, `sed` without `-i`, `git log`, `git status`, `git diff`, `git show`, no
file redirect; `2>/dev/null`, `>/dev/null`, `NUL` and `2>&1` are not one), nothing is pushed, published or installed, so such a call derives no
push, publish, deploy, install, commit, delete, send, spend or configure moment
even when its text mentions one, and a fact that was reached ONLY through a file
it names is served only when its text names the tool that is running (a fact
about reading with `grep`, like a trap in a log file). Facts reached through a
command anchor, a host or another moment are served as before, and so is every
call that writes or that cannot be read in full.

THE LENGTH HOLD QUOTES THE OWNER (2.5): a response-rulebook reminder may carry
`{owner_request}`; the hold replaces it with the sentence(s) of the owner's last
prompt that name one of the rule's `asked_for_any_of` terms or ask for a report
back (geef, vertel, rapporteer, tell, give, report, "wat je veranderde", "hoe je
het controleerde", ...), else the LAST sentence, between quotes and cut at a
word boundary near 240 characters, so the agent keeps every item the prompt
asked for instead of a guess of it. A rule may also carry a `short_reminder`:
when two rules of one tier fire on the same reply, each says its short form once,
and one closing line keeps any note that a guard refused an edit or forced a change.

A CLAIM OF CHECKING BACKED BY A REAL CHECK (2.5): the rule that wants a path:line
or a commit for a "checked" claim counts the claim as evidenced when the same
turn ran a check through Bash or PowerShell (`sh -n`, `bash -n`, `node --check`,
`node --test`, `npm test`, `cargo test|check|build|clippy`, `pytest`,
`py_compile`, `go test`, `shellcheck`, `git diff` ...) or when the answer names
one. It is a broad test on purpose: it does not match the claim to the command.
The shipped reminder is English, like every other hold.

A CONTAINS CHECK IS PROVEN ONLY FOR A RULE ABOUT THAT FILE (2.5): the trial's
evidence that a literal sits in the committed file counts only when the rule's
text or falsifier names the file. A rule about a click in a web UI whose check
freezes a name in a script is wired, never proven. A refusal of a removal also
says what to do when the owner asked for it: tell him, revise the rule (clear or
change its check, with a because) or retract it, then edit - never keep the text
in a comment to get past the guard.

A HEADLESS START CARRIES AT MOST EIGHT GLOBAL FACTS (2.5): besides the project's
own facts, the irreversible and costly global facts first, then house style, then
the unweighted ones; `go`/`gi` on the volume `entry` and `start` lines count and
name what was cut.

Stop notices that do not hold the turn are also queued (at most 3, once each)
and handed to the same session as context at its next prompt.

HOW OFTEN ONE AGENT SEES A FACT (2.5): one context - a session's main
conversation, or one subagent, which starts fresh - is shown a fact in full
ONCE, then never again: no reminder line, and a prompt the owner types opens no
new showing. The count starts again only at a new session start (also a resume
or a compaction, which lose what was shown). Two facts that say the same in one
block are shown once. Only what is shown is limited: refusals run before it and
are never hidden, and only a full showing is recorded as a serving, so a
repeat adds nothing to the judgement debt. A path, directory or route anchor is
named by a prompt only through its path (`site/dev`) or a distinctive segment,
never by a bare generic word such as `dev`, `src`, `test`, `docs` or `server`
(no dot, at most four letters or on a short list); a real tool call on the path
is unchanged. The log keeps no prompt text: where a prompt fired a fact it
records "(prompt)" and the anchor or moment word that matched. `doctor` shows
the volume for the last 24 hours and 7 days: facts shown in full, repeats left
out, the five most shown facts and the average Stop holds per session. A
prompt that is a message from another session, a hand-back, a task
notification or a local-command echo draws no facts, and a negated action
("niet pushen", "no deploy") is no moment. The `SessionStart` block lists the
heaviest rules first and carries the open points as one line with their count.

An open point is THOR's own: the machine pass writes one per blocker
(`open-no-working-copy-*` when a project has no registered working copy,
`open-working-tree-differs-*` when the checkout is mid-change,
`open-anchor-full-*` when a rule's file check cannot be anchored because that
file's place is full of the owner's other facts) and closes it
itself, with a derived reason, at the first start after the blocker is gone.
Rules with no project whose check reads a file have no working copy anywhere;
they share ONE point for the whole memory, `open-no-working-copy--global-rules`,
which every project's evaluation holds them by (never on its batch) and which
belongs to no project.
`remember`, `revise` and `retract` refuse an agent's attempt to file, tag or
remove one. One still open after a valid evaluation was filed is in the owner's
queue (the point's text says so, and names each carrier); over a budget of 8 per
project, the `SessionStart` block opens with a banner for the owner, never for a
subagent, and the filing receipt repeats it. The one point for the whole memory
is carried by every project's valid evaluations together, counted once in the
queue, and against no project's budget. A `mark` given from a shell with
`serve mark` still writes a verdict, but it is outside an attested call: it is
not counted as judgement debt settled, it does not shrink a judgement list, and
the machine section lists it as a side door.

`--project <key>` additionally writes a `.thor-project` marker in the current
directory to override the project name. By default, a checkout is scoped to the
name of its repository folder. Pass `--project` only when you want a different
name. It refuses to change a key that is already there: re-scoping strands
every item filed under the old one while leaving them in the store, which is
invisible from every surface.

Either way the project's name was decided, `install` also reads that project's
code once (so `search_code`, `where_used` and `outline` can answer for it,
refreshed automatically on every later commit), records that same name in the
memory itself the moment the read succeeds - so the project's first `remember`
never has to ask which collection it belongs to - and registers the checkout it
runs in as that project's working copy. The working copy is the one folder THOR
tries a rule's file check in when the rule is written (see "One thing worth
knowing before you write a rule" below), whatever folder a session happens to
stand in. All three are idempotent: a later run from the same checkout reads
nothing again and writes nothing, and one that finds the code already read but
no working copy registered - a project set up by an earlier release - registers
it and changes nothing else.

`install --register-root --db "<store>"` does the registration alone. Run from
inside a checkout, it records that checkout as the working copy of the project
the checkout resolves to, and touches no settings, no hooks and no code index.
It refuses to run without `--db`, so a registration never lands in a default
memory your assistant does not use. One project has one working copy. A second
copy of a registered project - a clone, a copied folder - is refused by both
commands before anything is written, naming the registered folder. A
registration moves only once its old folder no longer exists; while it still
does, moving it is the owner's call, made by removing or renaming the old
folder. The registration record takes events from the installer only, so no
retract or revise can move it or remove it. For seven days after a registration
is made or moved, the project's session start names it in one line, and
`doctor` lists every one.

The four hooks are `SessionStart` (what the agent is handed at the start),
`PreToolUse` (the gate that can block a write), `UserPromptSubmit`, and `Stop`
(the check on the reply itself). All four run the same `serve hook` command and
tell themselves apart by the payload. When a `SessionStart` payload's own
`source` field reads `"compact"` - right after a context compaction - the
agent is handed one further line: a reminder that the summary is not proof,
so anything it calls done, tested, flashed, deployed or live should be
checked again first.

A hook pointing at a binary that is not there fails OPEN: the agent carries on
and the memory simply never speaks again, with no error anywhere. So the
installer refuses rather than write such a hook - if the `serve` or `mcp` binary
it would point at is not present, it stops and says which, at the one moment
that is cheap to notice.

## The answer guard

The `Stop` hook above is what makes this work: after every reply, before the
owner sees it, a small check reads the reply back against a short list of
house rules - open with a plain summary before the jargon, do not ask him to
check something you can check yourself, back up a "checked" claim with a real
file or commit - and if one fires, the agent is nudged to fix the reply
before it goes out. That list lives in one file next to the store,
`guard-response-rulebook.json`, and `install` writes a working example there
the first time it finds none (see above).

That example is neutral, not a choice anyone made, so a seeded note walks the
owner through his whole first session once, before other work: what the
guard checks, how long a reply may be, which of its five rules he wants, what
language he wants his own rules written in, and the rest of that first
session's setup besides (the full list is in `AGENTS.md`). His answers get
applied where they belong and stored as a record next to the note, and only
then is the note retracted - the session is held open until both are done,
and retracting the note without that record is refused rather than allowed
through. Saying "he is not interested" still counts as an answer and still
clears it; the point is that something was actually asked and recorded, not
that a particular answer was given.

To change the settings later by hand, open `guard-response-rulebook.json` and
edit the rule you want to change, or delete it outright to turn it off. Each
rule is one entry with:

- `id` - a short name for the rule, for your own reference.
- `any_of` - words or phrases; the rule is a candidate to fire when a reply
  contains at least one of them.
- `none_of` - words or phrases that cancel the rule even when `any_of`
  matched - the escape hatch for a false alarm.
- `min_chars` - the reply has to be at least this many characters long
  before the rule is considered at all.
- `list_request_any_of` - on the length rule only: if you asked for a list
  with one of these words and the reply really is one, the length rule
  stands aside instead of firing on it.
- `none_of_patterns` - shape-based escapes rather than exact words; the
  evidence rule uses these to recognise a commit hash or a `file:line`
  citation however it happens to be written. A rule that names `path_line`
  or `commit_sha` here has its evidence looked up whenever the `Stop` payload
  says which directory the session works in: a `file:line` counts only when
  that file exists under that directory and has that line; a commit hash, and
  the word `commit` in `none_of`, count only when the hash names a commit that
  exists in that directory's git repository; the words `line` and `regel` in
  `none_of` count only for a line number that a file named on the same line
  of the reply really has ("line 3 of src/lib.rs"); and a literal such as
  `.rs:` counts only inside a citation that checked out. Evidence that does
  not check out counts for nothing, and the block reason names it. Where no
  git repository answers for that directory, a commit hash is judged by its
  shape, as it was before. A commit of a different repository than the one
  the session works in does not count either: name a real line there, or say
  plainly that it was not checked.
- `own_claims_only` - `true` marks a rule about the agent's OWN claims, such
  as "do not say you cannot reach something without trying". Such a rule does
  not fire on words the reply reports or quotes from someone else ("the other
  model said it had no access", a quote, a blockquote, a code block), only on
  what the agent says itself; a first-person word ("I", "ik", "we") in the
  sentence that frames the report keeps it the agent's own. Left out, the
  rule reads every word of the reply, whoever said it.
- `affirmative_only` - `true` marks a rule about something the agent CLAIMS
  is so, such as "whenever you say it is pushed, name the version". Such a
  rule does not fire on a claim that a negation just before it turns into the
  opposite ("niets gepusht of gedeployd", "nothing was pushed"): a negation
  word ("niet", "niets", "geen", "nooit", "not", "never", "without", ...)
  within the six words before the trigger, in the same sentence and with no
  "maar"/"but" between them, makes that occurrence no claim. Left out, the
  rule reads every occurrence.
- `backed_by_turn_tools` - `true` marks a rule about a claim of having looked
  at something ("checked", "verified", "I cannot reach it"). Such a rule does
  not fire when the turn itself looked at something the claim is about: a Bash
  or PowerShell command, a Read, Grep or Glob, a web fetch or search, or a
  browser or screen tool, run after the owner's last prompt, whose file name,
  command words, address or stated purpose shares a word with the paragraph of
  the claim (and the one before and after it). Launching an agent, writing or
  editing, and THOR's own memory tools do not count, so a claim relayed from an
  agent's report, or made in a turn that looked at nothing, or only at
  something else, is still held. A reply never has to name a file or a line for
  this: the owner's short-answer rule forbids it. Known limit: a shared word is
  not proof that the call checked the claim. Left out, the rule reads every
  claim.
- `quotes_exempt` - `true` marks a rule whose trigger is a phrase the agent
  must not USE ("nog te controleren"): where the phrase sits inside quotation
  marks or inline code it is mentioned, not used, even in the agent's own
  first-person sentence. Left out, a quote is read as before.
- `only_if_prompt_any_of` - words of which the owner's own last prompt must
  carry at least one for the rule to apply at all. "Do not ask him to commit
  what he asked you to commit" is only a defect when he asked for a commit;
  asking first is the right thing when he did not. Left out, or a prompt that
  cannot be read: the rule applies as ever.
- `tier` - `block` holds the reply back until it is fixed; `warn` only
  leaves a note behind. Every rule seeded here is `block`.
- `owner_reading_only` - `true` marks a rule about how a reply reads to the
  owner (its length, a summary first, a tone), not about whether the agent
  told the truth. Such a rule steps aside for an agent's reply, which the
  session that spawned the agent reads, not the owner. Left out, the rule
  applies to everyone.
- `reminder` - the plain-language nudge the agent sees when the rule fires.

Who reads a reply is decided from the `Stop` payload. Claude Code adds
`agent_id` only when the reply comes from a subagent, and then the
`owner_reading_only` rules step aside - and so do all of the memory-upkeep
questions the `Stop` hook asks, since those belong to the owner's own session.
A program that drives a local model for the owner sends `agent_id` too, so its
calls never count toward an assistant session's upkeep, but the owner reads those
answers himself. Such a program adds one more field to its `Stop` payload:

```json
{"hook_event_name": "Stop", "agent_id": "local-model", "owner_reads_answer": true, "...": "..."}
```

`owner_reads_answer: true` brings the `owner_reading_only` rules back for that
one answer and changes nothing else: the call is still an agent's, so no
upkeep question of the owner's session is asked of it. Only a literal `true`
counts, and without `agent_id` the field changes nothing.

## Check it

`doctor` reports one plain-language line per component and touches nothing:

```bash
target/release/doctor.exe --db "<store>"
```

Its first line always names the build it was run from (`doctor 2.5.0`), so a
report you paste somewhere says which version made it - every program in this
directory answers the same way to `--version` or `-V`, on its own, with no
store needed.

It tells you whether the store is healthy, whether searching by meaning is on,
how many rules carry a runnable proof, how many anchors point at nothing, how
many rules still lack a falsifier, and how many live items are bound only to a
moment that nothing in `serve` actually fires - `answer` and `claim_done` are
the two that exist in the schema but nothing produces yet. It works on a store
with nothing in it yet, which is what a first run looks like.

When a rule or orientation has fired many times with no verdict either way,
doctor also names each one that applies to this checkout - how often, what
kind, and where it fires - so a review has a list to settle instead of only a
count. A rule the owner has pinned to fire at every session start never
appears on that list: pinning it already answered whether it belongs there,
and a verdict on it would change nothing else in the memory. A verdict only
clears that debt once a `Stop` has checked that it was made with the `mark`
tool inside the Claude Code session the item fired in; the ones that were not
(a `serve mark` from a shell, every verdict given before the upgrade that
introduced the check) are kept, counted on a line of their own, and clear
nothing.

An `evaluations` line names the newest evaluation report that COUNTS for this
checkout's project (valid, and filed by which session), the last five valid
ones with how their verdicts split (`eval-<id>: <u> useful, <n> noise, <s>
skipped (<p>% useful)`), a flag when ten or more verdicts all went one way (a
blind pass looks exactly like that, and nothing can tell whether the items were
read), every report on file that does not count with the reason, and a warning
when Claude Code's session record could not be read. The judgement debt line
says whether the project's evaluation is due and, when it is not, how many times
the `Stop` hook has already asked and since when. A `cadence` line always names
which rhythm this store runs (once a week, or daily while rot is found on Max,
with the plan and who said so), and a `work clock` line speaks up when the work-clock file disagrees
with the session records by more than a quarter of an hour, since that file is
one any local process can write. An `open points` line counts THOR's own open
points apart from the legacy ones an agent filed before THOR wrote them itself,
and an `owner queue` line counts, per project, the ones waiting for the owner
against the budget of 8 - and, once, the point for the whole memory.

Doctor also lists the working copy each project is registered at, since when,
and where it was before a move - or `NOT USABLE` with the reason, when that
folder is gone, is no longer the top of a checkout, or no longer resolves to
that project - and it names every project that owns rules but has no working
copy registered, with the command that registers one.

Its `teeth` lines say where the rules stand, read through the same answer the
`Stop` hook and the evaluation filing gate use: how many live rules and
orientations have no current verdict from THOR's trial (the rot, whatever their
weight, with the heavy ones beside it, each named as `undecided` with its
reason); how many are proven, wired, notes, notes by dispute, notes with a
pending check, kept by `accept_refusing`, or blocked; which refusals on the log
count as proof, and why each of the others does not; which rules were taken off
the rule track in the last 30 days; and which were refused for a tooth and then
stored as a note that no longer names it. `--full` names every item instead of
the first few.

Two of those checks - whether an old reference still points at a real file,
and whether some facts never win a place - need to know where your other
checkouts live, normally via `--checkouts <dir>`. Leave that flag off and
doctor now guesses: it looks at the folder just above the repo you ran it
from. The line right after its version always says which folder it ended up
using - the one you gave it, its own guess, or, if it could not find either, a
plain note that those two checks did not run this time and how to make them
run.

One line only speaks up when there is something to say: `wal`, the size of the
store's own write-ahead log. It stays silent for the ordinary case of a log
that grows and shrinks as you use the store, and reports only once the log has
grown past both 64 MB and the size of the store itself - which means a
checkpoint is not landing, because something (a long-running reader, a stuck
repair) is holding the log open. If you see it: close anything else that has
the store open, make sure nothing is stuck mid-repair, and open the store
again - opening it also caps how large the log is allowed to grow back to once
a checkpoint does land, so this should not recur.

`verify` is the slower, deeper check beside `doctor`: read-only, it replays
the whole event log and confirms the hash chain, the derived heads projection
and the search index all still agree with a from-scratch fold.

```bash
target/release/verify.exe "<store>"
```

A step that fails is reported and left alone. Add `--rebuild-fts` or
`--rebuild-heads` to have that one step rebuilt from the log and re-checked -
both are safe regardless of what caused the drift, because each is a
projection the append-only log can always reproduce losslessly, and neither
flag does anything unless the matching step actually failed.

## Writing to it from somewhere else

The machine that holds the store is the authority: it is the only one allowed
to append to the log. A second machine (a NAS, a server you can reach from
your phone) can hold a copy and answer reads from it, but if it ever wrote to
its own copy the two logs would fork, and the next replication would be
refused with no way back except rebuilding the copy.

Keeping that copy up to date is `sync ship`'s job, usually run on a schedule
(once an hour, for example): it sends only the events the copy is still
missing. A receiver that refuses a batch or simply never answers now fails
that run loudly - a bounded timeout so it cannot hang, one line on stderr
naming what went wrong, and a non-zero exit code - instead of leaving a
scheduled task stuck and silent for weeks; and `doctor` names a copy whose
last successful ship has gone stale, so a broken schedule shows up there even
if nobody is watching the task itself. That line also tells a sleeping
machine apart from a real problem: events waiting behind an old success are
named as waiting, never as a failure, while an attempt that actually failed
is named with its own reason. The install does not set a backup up: it is a
choice the user makes at the first session. Once chosen, the off-site copy the
`backup` command keeps in git behaves the same way about its own commits - one that could not reach the
remote is pushed again on the very next run, before that run's own
once-a-day schedule is even checked, instead of sitting unpushed for up to a
day. The log is exported in numbered files (`events-0001.jsonl`, ...) of at
most 50 MB each, so no file passes the size GitHub accepts, and
`backup --restore-from` takes the folder that holds them. A backup that fails
is not tried again at every session start, only after an hour, and `doctor`
says since when it has been failing and why. Where the copy goes is chosen
without a setting: a remote called `nas` in the backup folder wins, else the
remote the branch already follows, else `origin`; `backup --remote <name>` names
one on purpose, and a name the folder does not have is refused with the fix. A
private copy can move from a website to a bare repository on a NAS this way,
and neither the hook nor a scheduled task has to change. `doctor` prints one line with the place and the age of the last copy even
while all is well.

So a write arriving at the copy is not applied there. It is queued, and the
authority applies it later. Three commands, in the order you set them up:

Start the copy as a receiver that also accepts writes:

```bash
sync recv --db /srv/thor/thor.db --bind 0.0.0.0:5556 --inbox /srv/thor/inbox.jsonl
```

Point a remote session at it by running the tool server over HTTP on that same
machine. It refuses to open the port without an inbox, because a reachable
machine that applies writes is exactly the fork described above:

```bash
mcp --db /srv/thor/thor.db --http 0.0.0.0:5557 --capture-inbox /srv/thor/inbox.jsonl
```

This tool-server connector carries no authentication of its own. `--allowed-host`
limits which `Host` header it will answer, but that is a routing check, not a
login - anyone who can reach the port and send that header can call every tool.
Put something in front of it that actually authenticates (a tunnel such as
Cloudflare Access, or a strict private network) before a phone or a second
machine reaches it, and never bind it to the open internet directly.

On the main machine, empty the queue before every replication:

```bash
sync drain --db "C:\Users\dev\thor2\thor.db" --from http://10.0.0.50:5556
```

The drain prints one line per queued write: `OK` with the item it stored, or
`LOST` with the reason the write gate refused it, and it exits non-zero when
anything was lost. That matters more than it looks: the gate runs at the
authority, so a rule written on a phone can still be refused there, and the
drain report is the only place that ever says why. The drain tries a rule
against its project's registered working copy, like any write on the main
machine, and `doctor` there counts the queued writes it refused. What the copy
can judge without a working copy - a check literal the text never names, a
hand-written `teeth:` label, an unexplained weakening - it refuses on the spot
instead of queueing, and an evaluation report is always refused there: it is
filed on the main machine, in the session that ran it.

The `sync recv`/`sync drain` pair shares the `THOR_SYNC_TOKEN` secret, and
there is no other protection on that transport either. Run both transports -
this one and the tool-server connector above - on a LAN or behind a private
tunnel, never on the open internet.

## Asking it what applies, from your own program

Something that is not an assistant can use this memory too - a local model
behind a program of your own, a deploy script, a job that runs unattended.
There are two doors, and picking the wrong one is the most common mistake
this project sees.

**The moment door is for steering an action.** You are about to run a
command, or touch a file, and you want the facts that apply to it. You do not
search for those: you hand over the command or the path, and the memory
answers with what is bound there. No keywords to guess, nothing to phrase.

```bash
serve --db /path/thor.db check --command "ssh user@host 'cp printer.cfg /tmp/x'"
serve --db /path/thor.db check --file server/db.js
serve --db /path/thor.db why --command "git push --force origin main"
```

`check` prints what a real block would show. `why` prints everything that
applies, including what the block would withhold, which is what you want when
something you expected did not appear. Both are previews: they do not count
as a delivery. The production boundary is `hook` - JSON on stdin, JSON on
stdout, exit 0 always, silent on any failure - and it branches on the
payload's own `hook_event_name`: `SessionStart`, `UserPromptSubmit`, and
anything else is treated as a tool call about to happen. Only `hook` records
that an item actually fired, so the counts you later read in `doctor` come
from that door alone. A tool call names its place in `file_path` (Read, Write,
Edit) or, for a search, in `path` (Grep, Glob): a search that names a file or
folder brings up the facts anchored there - a folder's own rules, or a file's
and those of the folders it sits in - but only to inform. A search is never
refused, and it never counts as touching every file under the folder it
searches.

One trap costs an evening if you meet it blind: a payload carrying a Windows
path in single backslashes is not valid JSON, and this program then prints
nothing and exits 0, which reads exactly like "no rule applies". Use forward
slashes in the payload, and when a hook is silent, check the payload before
you conclude the memory is empty.

A fact that exists to REFUSE arrives in one of two shapes, and a program has to
read both. When the call carries a command line and a rule's own forbidden
fragment occurs in it - as whole words, in a part of the call that runs the
command the rule is bound to - the answer is a refusal naming the rule and the
fragment.
When the call carries structured arguments instead, and it is the TOOL NAME that
the rule is bound to, nothing is refused: the ordinary answer comes back with a
prohibition row in its data, naming the same fragment. The second shape is the
only one a program whose tools carry no command line will ever see - measured
against a real tool list in September 2026 - so treating a prohibition as
"something that would have been refused already" is how a rail quietly stops
being a rail.

**The search door is for looking something up on purpose**, by a person or an
assistant that then reads the answer. It is not a way to fetch the one fact
that answers a question, and it is worth knowing why before you build on it.
A search first tries your words literally - the whole phrase, and failing
that, every single word has to appear somewhere in the item. A ten-word
question therefore matches nothing literally, and everything you get back
comes from meaning search instead. That leg scores an item by its best
chunk, so a long report, split into many chunks, gets many attempts while a
short rule gets one. Measured on a real store in September 2026: three
natural-language questions returned eight or nine long reports out of every
ten hits, and the short rule that actually answered the question was not in
the results at all.

When a search surprises you, `search --explain` prints, per hit, whether it
matched literally or by meaning and with what score, plus the floor and the cap
that decide what survives; `search --explain-id <id>` answers the harder
question, which is why one particular item is NOT there - under the floor, or
over it and cut by the cap. That is the difference between "the memory does not
know this" and "the memory knows it and did not show it".

So, from a program: two or three distinctive words, not a sentence, and
`--kind rule` when it is a rule you want. And when what you really need is
"tell me what applies to what I am about to do", use the moment door above
instead - that is the one built for it.

## Run the tests

```bash
cd thor2 && cargo test --workspace --all-targets
```

1623 tests across 96 binaries, all green as of 2026-09-08. Every refusal the
write gate can produce has a test named after the defect it prevents. A refusal
reason with no test does not exist, by the contract's own rule.

## How much of your memory can actually stop you

Run the health check and look at the `provable rules` line. It counts how
many of your rules carry a runnable check, and that number is the ceiling on
what this tool can ever refuse: a rule backed by prose alone can inform, but
it can never block a write.

The first time this was measured on a real store, on 2026-08-06, the answer
was 2 rules out of 2999. Every other line of the health check was green.
That is the failure this line exists to make visible: a capability nothing is
wired into looks exactly like a capability that works.

A day of deliberate work took the same store to 256 of 2979. The number moves
by hand and only by hand, which is the design rather than a shortcoming: each
proof is a judgement about one rule, and attaching them wholesale is exactly
how the noise gets back in.

Doctor's `teeth` lines ask the sharper question: not whether a rule carries a
check, but whether THOR's own trial has decided it - on the rule track, a note,
or undecided. The undecided ones are the worklist, each named with its reason
(all of them with `--full`), and one command decides them:

```bash
serve --db "<store>" retest --project <name>
```

It tries every rule and orientation of that project that has no current
verdict, against the project's registered working copy and the real calls in
the log, prints every form it tried with what the hook did with it and the
verdict, and writes nothing. Read that first - above all every rule it would
turn into a note - then run it again with `--apply` to write the verdicts it
can: a rule whose check still bites stays on the rule
track, and one whose check no longer bites becomes a note without it - unless
a tooth built from its own text still bites, which it names as the fix. `--id
<id>` does one rule, `--all` the whole store. The exit code is 0 for a verdict
it can write, 3 when a rule is blocked (its project has no registered working
copy, or that checkout is mid-change while the default branch still has the
line), and 4 when only a writer can make the fix, which it names.

A verdict is only as good as the matcher that made it, so every verdict records
the matcher revision of the build that wrote it (a hash of the sources that
decide whether a call is refused; comments and blank lines do not count). After
an upgrade, see what the new build would decide differently:

```bash
serve --db "<store>" retest --all --compare
```

It tries every rule that ALREADY has a verdict again, skips none, writes nothing,
and prints only the rules whose verdict would change: a note that now bites
(`PROMOTE`), a rule that bit and would now be a note (`DEMOTION`, listed loudly),
and a rule this build cannot decide (`UNDECIDED`). It exits 5 when something
would change. `--compare --apply` writes the promotions; a demotion is written
only with `--compare --apply --allow-demote`, and otherwise waits for the
evaluation. The evaluation start does the same for rules an older matcher
decided, inside its time box: promotions are written, an unchanged verdict is
NOT rewritten (the revision it was compared at is remembered in
`matcher-tried.json` beside the store, a cache whose loss only costs one more
trial), and a demotion goes to the agent's batch with its reason, never to the
log.

The `Stop` hook does not ask about these at all any more (2.5). The rules THOR
could not decide alone are asked inside the project's evaluation - its batch of
at most eight that need a fix only an agent can make, and its list of at most
sixteen that only have to name their check's literal - which is asked at most
once a week per project, of a session that has done at least 10 minutes
of its own work in the project and never at its first `Stop` there. A short task
is never held for old rules it did not touch.

### The false-block sweep: every rule against what really happened

A rule can be right about the forms its writer thought of and wrong about the
calls that really get made. THOR keeps the calls itself and tries every rule
against them, so nobody has to hunt for a rule that refuses what it should not.

- **The record.** At every end of a turn the hook appends the turn's tool calls
  to `real-calls.jsonl` and the reply the answer guard judged to
  `real-replies.jsonl`, both beside the store: the tool, its input (redacted the
  way the log redacts a command, long texts cut), the folder and project, whether
  THOR's hook refused it and under which rules, and whether it ran and succeeded.
  Thirty days are kept, and each file stays under a size cap (144 MB and 32 MB,
  the oldest lines go first). Only what is new since the last turn is read, within
  one second (about 15 milliseconds when little is new). On a machine that starts
  with nothing, `serve sweep --backfill` reads the last 30 days of transcripts,
  calls and replies, in about 15 seconds; a call read back that way has no known
  build, so it is never evidence (below).
- **The sweep.** `serve --db "<store>" sweep` tries every rule that can refuse a
  call, and every entry of the answer rulebook, against that record through the
  functions the hook itself uses, and prints per rule: it would refuse K of N real
  calls, R of them ran fine, with examples. "Ran fine" is evidence, not a verdict:
  either the hook let the call through after the rule last changed, in a session
  whose hook was alive, and it ran; or the hook refused it under this rule and
  the same call later ran unchanged. Only calls the hook judged with the
  running build count, so an install never flags rules for what an older
  engine decided. A rule that waits for the owner's go (`owner-approval`) is
  never flagged. It is incremental: a run replays only the
  rows written since the last one, plus what a changed rule can reach, and stops
  at its time box (`--budget-secs`, 600 by default) to carry on next time. A full
  replay of 156,000 real calls takes two to four minutes; a run with nothing new
  takes under half a second, and 3,000 new calls about three seconds. A new build
  starts a full replay, while the earlier findings stay in view until the new pass
  replaces them.
- **Where it shows.** `doctor` and `doctor --eval` print one line, silent at zero:
  `false-block sweep: R rule(s) would refuse real calls that ran fine: <ids> (run
  serve sweep for detail)`. The evaluation start note lists the same rules as
  item d of section 2, each with its example and the call that settles it, and its
  refusal-outcomes line points there because that proxy cannot see a false block
  that was walked around without weakening the rule. `remember` and `revise` of a
  rule that can refuse say `real calls: this rule would refuse K of N real calls
  in the last 30 days`, with up to three examples; they inform and never refuse.

There is a second gate that keeps the number from rotting once you start
using it. Adding a fact to a target that already holds one whose own proof
has gone false is refused, and the refusal names what to settle. It is
deliberately narrow: only the target you are touching, never a backlog
elsewhere; only on a new fact, never on a correction or a removal, because
those are the maintenance; and only when a check actually ran and came out
false, never when it could not run at all.

## One thing worth knowing before you write a rule

A rule needs a binding (a moment, a target, or `always`), a falsifier (one
sentence naming what would prove it wrong), and it has to fit in 300 characters.
The gate refuses anything else and tells you exactly what to fix. That refusal
is the gate working, not a bug.

A rule or an orientation with no binding at all is refused: it could never
fire. So is a costly or irreversible one whose only binding is a project name:
that binding reaches a prompt in which you type the name, and never a tool call,
a command or a file. Bind it to the file or command it is about, to the moment
it applies at, or pin it (`always`, which costs tokens at every session start).
A light note (`house_style` or no severity) may be bound to a project name alone;
it then fires only when you type the name. Items stored before these two
refusals stay correctable, and `doctor` counts them (the `no binding` and
`project-only` lines) instead of saying `dead moments: none`.

A note may not carry a real secret. The text and the falsifier of every kind of
note are scanned for a short list of unmistakable token shapes (Stripe-style
`sk_`/`pk_`/`rk_` live and test keys, GitHub `ghp_`/`gho_`/`ghu_`/`ghs_`/`ghr_`
and `github_pat_`, Slack `xox`, AWS `AKIA`, `tskey-`, `sk-ant-`, `AIza`, a JWT,
a PEM private key header), each with a minimum length so a sentence that merely
mentions `ghp_` is fine. The words password or key, `name=value` and long hex
(a commit sha) are not scanned. The refusal names the shape, shows the first six
and last two characters and never the value, and there is no way around it: the
log is append-only and its daily backup copies every write, so a stored secret
can never be removed, only rotated. Write where the secret lives (a file or
variable name) instead. If a live note already carries one, `doctor` prints a
`secrets in notes` line (silent when there is none).

A moment binding only works for a moment something actually produces: `push`,
`commit`, `deploy` and the rest that `intent` derives from a real command or
file, plus `remember` itself. `answer` and `claim_done` exist in the schema but
nothing fires them yet, so binding a NEW rule only to one of those two is
refused; a rule that already carried one from before this was enforced stays
correctable for anything else about it.

A target binding can also name a tool directly - `Agent`, `Artifact`,
`SendUserFile` and so on, not only a shell command string - and now reaches you
at the moment that tool itself is called, the same way a shell command reaches
you when it runs.

If you want the rule to be able to block rather than just inform, give it a
check as well: `contains` and `absent` for text that must stay in or out of a
named file, `absent_all` for a set of literals in one file, `path_exists` for a
file that must be there, and `forbidden` for something self-contained that has
no file to anchor to at all, like a punctuation character that is banned
wherever it might be written. A check anchored to a file also catches a shell
command that deletes it, empties it out, or overwrites it (`rm`, `truncate`,
`sed -i`, `tee`, a redirect, `mv`/`cp` onto it) and not only a direct Edit or
Write, in any part of the command - after a `cd`, on a later line, inside
`$(...)` or `bash -c '...'`, with a relative path read against the folder that
part runs in - though a glob in the command is never expanded to guess which
files it might touch, and a command sent to another machine over `ssh` never
counts as a write to a file here. `contains`, `absent` and `absent_all` can
also point at a FOLDER instead of one file: then they look at every file
sitting directly inside it,
but never a file in a folder inside that one. Use this when the same fact
lives in more than one file in the same place - a setting that has to match
between two config files, say. Pointing the check at the folder they share
covers both of them with one rule, without also reaching into an old backup
copy of one of those files kept somewhere else in the project, which is what
would happen with `forbidden`.

A `forbidden` check bound to a command is looked for in every part of a call
that runs that command, the way a shell splits it: after `cd x &&`, after a
`;` or a new line, behind a pipe, inside `ssh host "..."`, `bash -c '...'`,
`$(...)` or a loop, and past `sudo`, `timeout` and the like in front. Its
literal matches whole words only: `--force` does not match inside
`--force-with-lease`, and `ssh` does not match inside `sshKey.mjs`, while
`--force=true` and `/usr/bin/ssh` still match. A command written with `.exe` in
a binding reaches only a call that names that `.exe`. Text that only mentions
the command - `echo "..."`, `grep "..."`, a heredoc of data, a comment - is not
a call of it. When one part of a call runs in another project's checkout (`cd
../other && git push`, `git -C ../other push`), that part is judged by that
project's rules. The checks on a file's content keep matching plain text.

Whatever check you give, the rule is tried the moment it is written. `remember`,
`revise`, `pin` and `unpin` run every rule and orientation through the same
guard the hook uses, and the reply says which of three things happened: a
RULE (the check refused the call or write it forbids and let the ordinary ones
through, so it is stored), a NOTE (no tooth holds, so it is stored without a
check and can only inform), or REFUSED (nothing written, one fix named). A file
check is tried in the project's registered working copy (see "Install it"
above). The check's literal has to be named in the rule's own text. And THOR
builds teeth from that text itself - a flag in a backtick span after the
command the rule is bound to, say - so a rule whose own words compose a tooth
that bites is refused until it hangs that tooth. `AGENTS.md`, under "How to
write a note that actually comes back", has the whole of it: the labels each
outcome writes, a whole-command ban, a check kept pending until its project is
registered, and the three arguments that answer a refusal (`legitimate_calls`,
`may_change`, `accept_refusing`).

`requires` is the odd one out: every check above asks whether some text is
present or absent, which cannot see a forgotten field - forgetting leaves no
fragment to look for. It carries a trigger plus a set of acceptable answers (in
`check_literals`, trigger first): once a call reaches one of the item's own
Command bindings, at least one answer has to appear in it too, or the call is
refused. It only ever binds to a Command target naming the exact command or
tool it is about, never `always` (which names nothing for a trigger to compare
against), and the trigger itself either equals one of those bindings or is a
literal that sits INSIDE the bound command. For example: a rule bound to the
commands `Agent` and `Workflow`, with literals `["Agent", "haiku", "sonnet",
"opus"]` - an `Agent` call naming none of the three cheap models is refused,
and so is a `Workflow` call, because the rule watches both bindings it is bound
to, not only the one spelled out as the trigger.

A trigger inside the command is for the case where the command that runs says
too little on its own. A rule bound to `ssh` with literals `["-p my-dev",
"-f docker-compose.dev.yml"]` asks for the dev compose file only on an `ssh`
call that carries `-p my-dev`; every other `ssh` call goes through untouched,
and so does `-p my-dev-old`, because the trigger has to appear as whole words.
Such a trigger has to be specific: at least four characters, and two or more
words or a flag, name or path with a digit or punctuation in it. One plain
word such as `dev` is refused, since inside a command it would pick out far
more calls than the ones meant.

Whole words cut both ways: the trigger `/thor` never sees `/thor-probe` or
`/thor2`, because a name that goes on after it is a longer word. For a family
of names, end the trigger with one `*` - `/thor*` catches `/thor-probe`,
`/thor2` and `/tmp/thor_new`, and, the price of a prefix, `build/thorough`. The
`*` is allowed only as the last character of a trigger inside the command, and
the part before it must be specific on its own. A call whose only words after
the command are `--help`, `-h`, `-?`, `/?`, `help`, `--version` or `-V` asks the
program about itself and runs nothing, so a `requires` check never refuses it.
A `forbidden` check does not refuse it either: a call that does nothing is
exempt from `forbidden`, `requires` and owner-approval rules alike.

One more thing `revise` refuses without asking first: weakening a rule or an
orientation, checked or not - clearing or changing its check, lowering its
severity, dropping or replacing a command, path or folder binding, taking a
flag, a quoted call or a backtick span out of its text, narrowing a checked
rule from every project to one, or turning it into a report (which takes it off
the rule track exactly like a retract) - needs a `because`, the same way
`retract` has always needed a reason, and it lands in the item's own history
right beside what changed. Clear a field
(severity, project, expires, key, falsifier, or the check) with its own
`clear_*` flag on `revise` - `clear_severity`, `clear_check`, and so on -
rather than the empty string a JSON-RPC caller would otherwise send: an
assistant's own tool-call layer drops an empty-string argument before it
ever reaches this server, so the flag is the one spelling that actually
works from one.

The store is the source. Every document, this one included, is a mirror of it.
