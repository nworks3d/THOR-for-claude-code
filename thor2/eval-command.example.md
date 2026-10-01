---
description: THOR session evaluation - judge what fired, repair what has rotted, give teeth to what nothing could stop, capture what was missing, report what the gate did and where it chafed, and close with a plain-language summary
allowed-tools: mcp__thor__mark, mcp__thor__get, mcp__thor__lookup, mcp__thor__remember, mcp__thor__revise, mcp__thor__retract, mcp__thor__status, mcp__thor__history, Bash(git status:*), Bash(git diff:*), Bash(git log:*), Bash(cargo test:*), Bash(cargo build:*), Bash(npm test:*), Bash(npm run verify:*), Bash(node --test:*), Bash(pio run:*), Bash(pio test:*), {{THOR_DOCTOR_ALLOW}}, {{THOR_RETEST_ALLOW}}
---

THOR session evaluation. Work through steps 1 to 8, then the report. You need
no prior knowledge for this: everything is below. Do not skip a step because
it seems like there is "nothing to report" - write "nothing" for that step
explicitly instead. An empty step is a finding; a skipped step is a gap.

Invent nothing. If a step turned up nothing, say so. A made-up finding costs
more than a missed one.

## What a mark means

A useful mark is not a dead signal. The latest verdict is what counts:
- a useful mark clears whatever noise came before it and resets the count to
  zero;
- a noise mark after that counts again, on its own;
- two noise marks since the last useful mark take a fact out of the
  injection channels - it stays findable through lookup regardless.

No Stop hook asks for a verdict on a single item any more (since THOR
2.5.6): the items that fired in many sessions without a verdict wait for this
routine, and a fact counts once per session however often it was shown there.
Here you settle them deliberately, over the whole session, in one block.

## When THOR asks for this, and how it starts

THOR's Stop hook asks for this evaluation at the end of a turn, in a project
a session really works in, when all of this holds:
- the project has had an hour of work since its last evaluation that counts.
  The hour is the project's own clock, every session added together, and two
  sessions working at the same moment count once;
- this session has put in at least ten minutes of its own work in that
  project since then;
- it is not this session's first turn-end in that project.

The ask follows the work, not the folder: a session that worked in project A
and then moved to another folder is still asked about A, and the ask says so.
One project runs one evaluation at a time: if another session is already
running this project's evaluation and was active in the last 30 minutes, you
are told that in one line, naming that session, and are not held. Once it has
been quiet longer, the next ask takes the evaluation over. A project with activity in the last seven days and no
evaluation that counts in seven days is due whatever the clock says.
Since THOR 2.5.6 the judgement debt and the question about old rules are not
asked at a turn-end at all: this evaluation is the one place, per project at
most once every 16 hours, so a session is never handed something to judge
every quarter of an hour. A verdict counts per item for everyone. What is
about your OWN writes (a fact stored onto a full place, setup, a decision the
owner stated) stays per session.
If THOR says this routine changed since you last read it, read it again
before you go on: never work from an old version.
When something else holds the same turn-end first - a reply rule, the
first-session setup, a decision the owner stated that was not stored, or a
note stored onto a full place - the ask comes in the same message, after it.
Settle both.

The old rules whose trial THOR has not decided yet are asked here, in the
start note (the batch and the A4 list), not at a turn-end. Each line prints
ONE command to run, `serve ... retest --id '<id>'`, written with forward
slashes and no `&`: that is exactly the form this routine's `allowed-tools`
pre-approves, so run it as printed (add `--apply` to write the verdict). Do
not retype it in another shell form - a PowerShell form is not pre-approved
and a headless session is refused.

The ask does not go quiet after one turn: it holds the end of every turn,
naming how many times it has already asked and since when, until a report
that COUNTS is on file. A report covers the next 16 hours. Once three more
hours of work go by since it, AND a real risk has shown up too - three or
more code changes with no test or build run in between, or a context summary
- the obligation returns as a repeat covering only what happened since that
report (see the SCOPE note under step 2). Three quiet hours with neither risk
stays silent. It only ever asks inside a project: a checkout that resolves
to no project is never asked, since there would be no way to file the report
that silences it.

A report counts only when THOR has checked how it was filed (see "The
report" below). One that was filed any other way is on record but counts for
nothing, and the next ask says why.

THE EVALUATION STARTS IN THOR'S LOG, never in a file and never at filing. It
starts when you open this routine (as a skill, or when the owner types the
command) or when the Stop hook asks for it. At that moment THOR writes a
start record and freezes three lists, which cannot grow: nothing that fires
or is written afterwards is added, and nothing that fired only in another
session is on them. Then THOR decides the old rules of the project ITSELF,
for up to 20 seconds, and the note it shows you afterwards names three
things:
1. JUDGEMENT - the items you give a verdict on (step 2);
2. TEETH - what THOR decided by itself, then two lists that are yours: the
   batch (at most eight rules) and the A4 list (at most sixteen), each item
   with its reason and the exact call to make (step 4);
3. OPEN POINTS - THOR's own, written and closed by THOR. Nothing for you to
   do.

That note is the one source for your work. If you missed it, the gate names
the same lists when it refuses the report. If the note says THOR is still
deciding the old rules, end this turn once: the next Stop shows the finished
note.

---

## 1. State of the work

Before anything else: what does this project's own working copy actually
look like right now, and can you back every claim below with something you
just ran, not with something you remember saying earlier in this
conversation?

- **Uncommitted changes.** Run `git status` and `git diff --stat`. Name
  every changed file, and, where you can tell, which agent or session made
  it - a background agent, a delegated task, or the session running this
  evaluation itself. No changes at all is a finding too: say so explicitly.
- **The project's own tests or build, run now.** Do not quote a result from
  earlier in the session - a build can go stale the moment one more file
  changes after it. Run it again, right now, and quote the actual result
  line (a pass/fail count, or the compiler's own summary), never a
  paraphrase of one.
- **Agents or background jobs.** Anything still running, or that stopped
  partway through its own work, gets named: what it was doing, and whether
  it finished, failed, or is still going right now.
- **Promises made without proof.** A claim that something is "done",
  "deployed", "fixed", or "working" with nothing run this session to back it
  up - a machine still busy elsewhere, a deploy nobody has verified, a fix
  nobody has re-tested. Name each one plainly; repeating the claim is not
  the same as checking it.

Every item above gets a decision: settle it now if you can (run the missing
test, check the machine, ask what a background job actually did), or write
it under "not settled" in your remarks with the reason you could not settle
it here. Step 1 is done once every item above has one of those two outcomes
- never left as a bare observation with nothing decided about it.

## 2. Judge what fired

Before any of that: look up the previous evaluation report for this project
(`mcp__thor__lookup` for the tag `evaluation-report` and this project; take
the newest one). Read its remarks - what it left unjudged with a reason, the
gaps it left open, and everything it listed as not verified or still to be
probed. For each: settle it now, or write it under "not settled" in your own
remarks with the reason it is still open. No previous report? Say so and move
on. THOR's own open points are not yours: they are written and closed by THOR
(see step 4), and nothing you write carries one.

SCOPE: if an evaluation report for this project was already filed within the
last 16 hours, this evaluation covers only what happened SINCE that report -
not the whole session, and not the whole 16 hours. Judge only what fired
since then; everything the previous report already covers is not this
evaluation's business again.

The test is not "was it useful" but **did it belong where it fired**.
- `mcp__thor__mark(id: "<id>")` - it belonged there.
- `mcp__thor__mark(id: "<id>", noise: true)` - it did not.

Two patterns, and how to tell them apart:
- A fact anchored to a FILE you edited often fires dozens of times. That is
  not noise: the anchor is right, and you were there.
- A fact bound to a broad MOMENT (`remember`, `commit`, `deploy`) that
  advises on a narrow subclass fires on every action of that kind. That is
  noise. If no narrower moment exists, moving it to lookup is the correct
  repair - let the system do that; do not force it.

When in doubt: skip it. A guessed verdict pollutes the one signal that
maintenance depends on. This is the ONE place in this evaluation where doubt
is a reason NOT to act. Everywhere else below, the opposite holds: doubt is
never a reason to leave a step undone. Here it is, because a verdict cannot
be taken back once it counts.

The argument is called `id`, not `entity_id`. On an "unknown entity" error,
retry the call, first with and then without a project prefix - the local
mcp__thor__ server is the authority.

Your list is the one the start note printed under JUDGEMENT, frozen when the
evaluation started: exactly the items that fired in THIS session and had
fired in 40 sessions or more since their last verdict (a fact counts once per
session). Judge only those. A pinned
item is never on it: the owner already answered whether it belongs there by
pinning it.

Judge each item by WHERE it fired, never by its text alone. Under every item
the start note shows where it fired in this session - how often, and its last
places (the command or file); `mcp__thor__get(id: "<id>", fired: true)` shows
an item's recent firings. With a list of more than about 50 items, build a
firing map FIRST: one pass over this session's transcript (a subagent may read
it) that says, per id, on which call it fired - then judge. Never get-then-mark
blocks of items without looking where they fired: measured 2026-10-01, one
session gave 205 verdicts in four minutes that way, and three were provably
wrong.

Every item on your list gets one of two answers. A verdict, given with the
THOR tool itself: useful if it belonged where it fired, noise if it did not.
A verdict from a shell, a script, another session or another program settles
nothing, and is listed for the owner under "side doors". Or, when you cannot
honestly judge it - "when in doubt, skip" above - an explicit skip, written
as a tag on the report itself: `not-judgeable:<id>:<reason>`, with a reason
of at least twelve characters that says why, for example
`not-judgeable:a-1f2e3d:it fired on a grep of a log I never opened`. A skip
settles that item for this report only; it stays owed store-wide, and the
next session that sees it fire is asked again. Do not re-mark something that
already carries a verdict - that would count twice.

GIVE YOUR VERDICTS, THEN END THE TURN ONCE, THEN FILE. THOR checks that every
verdict of this evaluation was a real THOR call when the turn ends, and the
report is refused while the verdicts of the turn you are in are not checked
yet. So give all your verdicts, answer in a line, and let the turn end; the
Stop hook checks them and holds the turn again, asking you to file NOW - file
the report in that same turn (see "The report"). If no THOR message held you,
file right away, in this turn. Never tell the owner "next turn" or "I will
file it later" unless the Stop actually held you.

The health check's `judgement debt` line names the debt of the whole store
for this checkout, including items that only fired in other sessions. Those
are not this report's to settle and never hold it. Step 2 is done once every
item on your list carries a verdict or a skip tag with a reason.

## 3. Repair what has rotted (this is not noise)

Did a fact come up that was still RELEVANT but no longer matches the
artifact - the code, the file, the current state? That is not noise, and
not "missing" either. Verify it against the artifact and revise it NOW.

Check explicitly every fact that carries a VALUE (a number, a line number, a
percentage, a date) rather than a pointer. These rot silently. The fix is
not a new value but a pointer: "read this from the health check" instead of
a number that will drift the moment the store changes.

Also check every fact that describes a problem as still open when it has
since been fixed.

THOR wins for decisions; the artifact wins for the current state of the
build.

Does the gate refuse your revision - the text too long, the anchor full?
That is not a finding but work for the same turn: keep the text short and
put the reasoning in a separate Report as step 5 describes, or move it to
the right anchor with a route from step 6. Step 3 is done once every fact
you recognised as rotten has actually been revised - no revision is left
hanging on a refusal.

## 4. Give teeth to what nothing could stop

Step 3 asks whether a fact is still TRUE. This step asks whether it can DO
anything.

Every rule is tried by THOR the moment it is written, and the old rules were
tried by THOR itself at the start of this evaluation. A rule whose check
bites is on the rule track (`teeth: wired, not proven`, `teeth: proven guard`
once reality backs it too) and can refuse a wrong change. A rule whose tooth
cannot hold is a note (`teeth: note`): it still arrives at its moment and
can only inform. Nothing in this step turns a note into a rule by argument,
and nothing here accepts "it is a judgement rule" as an answer: the burden of
proof is on the rule, so for every rule you handle you TRY to give it a tooth
that holds before anything else. A tag, a reason, an open point or a refusal
on record settles nothing: only the trial decides, and the trial reads the
rule as it is written now. Note the `teeth` line from the health check before
you start this step and again once you finish it - the report names both.

So you do NOT walk the store's heavy facts, and you do NOT pick your own. You
handle only what the start note handed you under TEETH:

- **The batch**: at most eight rules that need a fix only you can make, each
  line giving the reason and the call. Make the fix the line names, then run
  the call shown - normally `mcp__thor__revise(id: "<id>")` with no other
  argument, which makes THOR try the rule again as it is written now and
  answer rule, note, or what is still missing. The report cannot be filed
  until every rule of the batch is settled, and the gate counts what is
  left. Rules that did not fit stay owed and lead the next evaluation.
- **The A4 list**: at most sixteen rules whose check bites and whose text only
  has to name the check's literal. Reword the text so it names that literal,
  using the exact call the note prints (a `revise` with the reworded text) -
  nothing else has to change. Each line says how many characters the text has
  left (300 minus its length now): reword within that budget, never append.
- **Everything THOR settled itself** (rules, notes, pending checks hung) is
  only reported, in the machine section of the report. You do not redo it.

Work out, for a batch rule, which literal fragment marks the mistake, at
which file or command it belongs (a rule hanging on a broad moment gets
re-anchored to the exact command or file first), and which check kind fits -
`forbidden`, `absent`, `absent_all`, `contains`, or `requires`. Name that
literal in the rule's own text, or the tool refuses the check: a tooth has to
say in its own words what it refuses. One exception: an approval marker of the
owner (a `requires` check tagged `owner-approval`, such as `go-owner`) is NOT
named in the text and never typed by you in a call - it counts only when the
owner writes it in his own message, this turn or the one before; a subagent's
hand-back or quoted text does not count. The text says the rule needs the
owner's explicit go instead. When THOR refuses a call for it, ask the owner
and wait for his message. Then `revise`. If the tool refuses, its
reply names the one thing to do; do it and call again - the refusal is the
gate working, not a bug.

Three arguments exist for a refusal that is right about the tooth but wrong
about your rule: `legitimate_calls`, `may_change` and `accept_refusing`.
Doctor lists every rule saved with one of them for the owner, so use one only
when you can say why, in the report, in a sentence the owner can check. They
are not a way to get a rule off the batch. A `legitimate_calls` entry must be a
call that really occurs - in the log (THOR's reply says how many real calls a
tooth reaches) or in the project. Never a made-up path or a template with
`<...>` in it: measured 2026-10-01, a session named a test file that did not
exist.

When a revise reply says bind, anchor or rebind (for example "bind it to
<file>"), make that rebind as a revise of its own, or write "could not" with
the refusal. Since THOR 2.5.2 a rewording never clears a true check: a reply
saying the check "is kept" leaves the rule owed until the rebind is made - or,
when that place is full, you make room first and rebind after.

A full place is yours to solve, never a question for the owner: THOR names
every fact on it with its severity and whether it carries a check that can
refuse, and the three honest exits - FOLD two that say almost the same (revise
one to carry the other's point, retract the other with a reason naming the
survivor), MOVE one to the narrower file or command it is really about, or
RETRACT one that is stale, with a reason. A fact with no check gives way before
a rule whose check can refuse, and a lighter fact before a heavier one. Never
clear a true check to get past it. When you retract a rule whose check held,
THOR warns and the machine section names that check: say in the report where
its point and its check live now, or that they went on purpose. Only when no
exit is honest, write "could not" with the refusal and the occupants.

Never file an open point, and never tag a rule with `open-point`, `about:` or
`blocker:`: THOR writes and closes its own open points (a missing working
copy, a file the checkout is changing, a file whose anchor is full of heavier
rules that can each refuse, and ONE point for the whole memory that holds every
rule with no project whose check reads a file) and refuses yours. A rule held
by one of those is not on your lists and is never yours to clear: clearing a
check that is true is never the way past a blocker, and a check you clear,
move to another file or change during this evaluation is listed in the
report's machine section for the owner. A legacy open point
an older routine filed settles nothing and holds nothing; retract one, with a
reason, only when what it names is resolved.

Working in a git worktree? Every check is judged against the MAIN checkout -
the health check and the commit gates read that one, never your worktree. A
check whose literal or file exists only in your worktree is refused at the
write: it would read as a false memory on main and block unrelated commits
there. Attach it once your change is merged; until then the fact stays
without that check.

What you NEVER do here: stretch the literal to catch more. If it catches one
form and not another, that is the honest answer - write down the gap you
left open. A broader rule that blocks legitimate work is the most expensive
outcome this system knows (see step 6), and the doctrine forbids widening a
trigger to buy a catch. Never write the mistake into a real file, and never
run the dangerous command itself, to "prove" a check: if the check does not
catch your form of it, the mistake is now in the code, or the command ran.
A refusal your own session meets while this evaluation is open proves
nothing: THOR records it as "refused during the session's own evaluation -
not proof", it never turns a rule from wired into proven, and it is listed
apart in the report. So do not plant a forbidden call, however harmless its
form, to show that a tooth works. THOR tries every tooth itself with forms it
builds; only a real refusal at real work counts.

Step 4 is done once every rule of the batch and of the A4 list has its call
made and THOR's answer to it, and nothing is left hanging on a refusal.
Retracting a heavy rule takes a reason that says what changed about THAT
rule; "no longer relevant" is refused.

## 5. Capture what was missing

Did you find a gotcha, a contract, or a behaviour detail that should have
been in THOR and that you can state concretely? Store it NOW with
`remember`. Reporting a missed lesson without storing it is half the work.

Where the gate will refuse you, so you get it right immediately:
- at most 300 characters for a Rule or Orientation - put the reasoning in a
  separate Report, do not lengthen the sentence;
- at least one binding (a moment, a target, or `always`) and a falsifier;
- a fact with no project scope may not name or anchor to a source file;
- anchor it to the file or command it is REALLY about, never to a path that
  merely happens to appear in the sentence;
- every binding already full of heavier rivals: solve it with a route from
  step 6 (fold it into an existing fact, anchor it narrower, or deliberately
  leave it with the label `crowded-on-purpose`) and name the occupants in
  your report - dropping it is not a route;
- a NEW place - a project, scope or purpose that does not exist yet: choose
  an existing place that fits, or ask the owner for a name for the new one;
- a check whose literal the text never names.

Want a fact to be able to REFUSE a wrong action instead of only informing:
- forbidden text in files: `always` plus check_kind `forbidden`;
- a forbidden COMMAND: a command target plus check_kind `forbidden`, with
  the dangerous fragment as the literal. Do not give a rule like this an
  `always` binding unless those words must also never be written - otherwise
  it refuses its own documentation;
- text that must not appear in ONE file: check_kind `absent`.

The reply to a write says which of three things THOR found: RULE, NOTE, or
REFUSED (nothing written, and the one thing to do). All other forms only
inform, and that is usually the right choice.

Step 5 is done once every fact you tried to capture is actually stored - no
attempt is left hanging on a refusal from the gate.

## 6. What the gate did

This is the signal that counts here, not a mark.

- Did the gate refuse something, rightly? Name the rule id and what was
  refused.
- Was there a FALSE BLOCK: something refused that should have gone through?
  **This is the most expensive outcome this system knows**, and it always
  belongs in the report - with the rule id, what you tried, and why it was
  legitimate. A false block teaches people to work around the gate, and
  that cannot be undone.
- Did the gate refuse one of your OWN write actions (`remember`/`revise`)?
  That is not an annoyance but a signal about the shape of the store. Name
  the reason.
- Did a displacement notice show up on a write ("this may well never be
  shown there", "no automatic serving surface can reach")? That notice
  means you just stored something that will probably never be read.
  **Do not just report it: resolve it now, before you continue.** This is
  the ONLY signal this evaluation ever gets about the invisible side of the
  memory - a displaced fact does not fire, so it never comes up to be
  judged. Reporting it and leaving it means it is never looked at again.

  Three ways out, in this order:
  1. **Fold it in.** Does an existing fact already say almost the same
     thing? Fold yours into it with `revise` and retract yours.
  2. **Anchor it narrower.** Is it hanging off a broad moment or a busy file
     when it is really about a more specific file or command? Re-anchor it.
  3. **Leave it, and record why.** Sometimes it genuinely belongs there and
     the place is legitimately full of heavier things. Then that is the
     answer - name which facts occupy the place in your report, and put the
     label `crowded-on-purpose` on the fact. Without that label it is not a
     decision but an oversight, and the debt comes back every turn until
     someone throws away a true fact just to be rid of it.

  What you NEVER do: raise the severity to make it visible. That pushes a
  heavier warning out of the way to show a lighter one, which is exactly
  the compensating trick this design forbids.

Step 6 is done once every refusal is named, every false block is in the
report, and every displacement notice is either resolved or deliberately
tagged - reporting alone is never enough here.

## 7. Friction - where it chafed

This is a full outcome, not an afterthought. Name everything that cost you
time or confidence, even what you fixed yourself:

- **Tooling.** A tool that refused, an unclear error, an argument that did
  not do what its description promised, an id shape that did not work.
- **The gate.** A refusal whose text did not say what to do instead. A rule
  that fired where it did not belong. A repeat handled differently than you
  expected.
- **The memory itself.** A fact that contradicted another fact. A
  near-duplicate you only noticed after writing yours. A fact whose anchor
  you could not trust.
- **The shape of the record.** Something you wanted to capture but could not
  fit within the rules (too long, no honest anchor, no honest test). This is
  the most important kind of friction, because it is where the store loses
  information.

For each point of friction: is this a one-off, or will it happen again next
session? If it will recur, THOR should hold a fact about it - capture that
under step 5 instead of only reporting it.

Step 7 is done once you have decided that for every point of friction - not
every point needs to become a fact, but every point needs a decision.

## 8. What you did NOT verify

Name explicitly what you are assuming but have not checked. If you added a
fact bound to a target, this evaluation only ever sees what FIRED - a
silently displaced binding shows up nowhere here. Flag such a fact as
"still to be probed": the probe is not made in this evaluation - a call you
plant here is recorded as proof of nothing (step 4) - but by the real work of
a later session, or by the owner. Never test with a file check or with free
prose - that proves nothing, and writing the mistake into a file only risks
it. A check on a file is tried by THOR itself against the
project's registered working copy; one it cannot try yet is kept as a note
with a pending check, and doctor lists it.

Step 8 is done once every untested addition from this session is on that
list, named with what should make it refuse.

---

## The report

Open with a plain-language summary: what happened, does it work, what does
it mean for the owner. No jargon, no hashes, no test names. The detail
follows below, in the order of the steps above.

Every claim in the report is re-run in THIS turn, or marked "not verified" (a
health check, what origin holds, a version, what is still running). Counts -
verdicts, useful, noise, rules - come from THOR's replies and the machine
section, never from memory.

Report what you DID, not what you noticed - evidence, not assertion. The
owner cannot see into your session: "I repaired a rotten fact" is, to him,
indistinguishable from doing nothing. So for every fact you changed: its id,
what it said BEFORE, what it says NOW, and the artifact you checked it
against. For every re-anchored fact: from which anchor to which anchor. For
every displacement notice: how you resolved it, or that you deliberately
left it, with the occupants named.

If you deliberately left something alone, say why in one sentence - that is
an outcome too, the same as a change; if you changed nothing at all, the
same holds. If a revision, check or capture could not get past the gate,
resolve it before you report: "I could not save this" does not belong in
this report - if it is still there when you write the report, the work is
not done.

1. the state of the work: uncommitted changes and who made them, the actual
   test/build result you ran, background jobs and their state, and any
   promise you found made without proof;
2. what you judged as noise and why, and what belonged where it fired;
3. what you revised, with before and after, and which artifact you checked
   it against;
4. the health check's `teeth` line before and after step 4; the batch and the
   A4 list: per rule, the call you made and THOR's answer (rule, note, or what
   was still missing), and every rule you saved with `legitimate_calls`,
   `may_change` or `accept_refusing`, with the reason;
5. what you stored that had been missing;
6. what the gate did - including every false block and every refusal of
   your own write actions;
7. the friction, and which of it will happen again next session;
8. what you did not verify.

Get the numbers about the state of the store from the health check, never
from memory and never from a fact. That is a SEPARATE program, not a
subcommand, and it is not on the PATH - run it with its full path:

    {{THOR_DOCTOR}}

The lines you need: `provable rules` (how many can refuse anything at all),
`gate` (how often it did), `teeth` (how many heavy rules have teeth),
`pinned` (how much of the memory is never re-read), `decay`, `crowding`,
`ship` (how long ago the copy on another machine last succeeded; this line
stays silent when there is no copy, and speaks up when one has gone stale),
`judgement debt` (how many items still owe a verdict, store-wide and for
this project), and `contradictions` (how many pairs of notes carry a proof
that cannot both be true right now; this line stays silent when there are
none). Do not copy those numbers into a Rule or Orientation - point to them
instead; they may appear in your remarks in the dated evaluation report
itself (below).

## Filing the report

File the report in THOR when you are done, so a later session can find it and
hunt it for real THOR defects. THOR writes the facts of the evaluation
itself, from its log, and puts them in front of your text:

- Call `mcp__thor__remember` with kind `report`, the project you evaluated,
  tags `["evaluation-report"]`, id `eval-<project>-<YYYY-MM-DD>` (a second
  one the same day gets `-2`), and a text that is ONLY your TLDR and remarks:
  the plain-language summary and then the eight points above, exactly as you
  gave them to the owner.
- Do not write a section of your own for what was judged, the teeth, the open
  points, the refusals or the side doors, and do not start a line with
  `=== `. THOR writes that machine section in front of your text, checks it
  against the log, and refuses any edit to it later. Your text below it can
  be revised; the machine section cannot.
- Add a `not-judgeable:<id>:<reason>` tag for every item of your list you
  skipped (step 2).
- If the THOR tools are not loaded in this session, load them first with
  ToolSearch, query `select:mcp__thor__remember,mcp__thor__revise,mcp__thor__mark`,
  then call the tool with the same arguments.
- File ONLY through that tool call, in the session that ran the evaluation.
  Never through a script, a second client, the model command line or an older
  program: such a report is refused, and if one slips through it does not
  count - the Stop hook checks the filing call against this session's own
  record of the conversation.

If the gate refuses the report, it names what is still open and the one
thing to do: each item is either settled by the work it names or honestly
skipped; the lists it holds you to cannot grow between attempts. Three
refusals are plain waiting, not work: "THOR is still deciding the old rules"
and "the verdicts you gave in this turn are checked when the turn ends" both
mean end this turn once and file again - THOR then holds the turn and you file
in it; "no evaluation is open in this session" means there is nothing to file,
so file nothing. A refusal that calls the evaluation VOID (an item on its
lists was changed outside an attested THOR call) means: end the turn, and the
next Stop starts the fresh evaluation or names the session that runs it. An
attested change by ANOTHER session never voids an evaluation: that item counts
as settled by that session.

The reply to a filed report says what the machine section holds and that THOR
checks the filing call when the turn ends. The report counts from that check
on: the Stop hook reads the answer back from THOR's log, and only a report
that counts lets a blocked turn end and stays quiet for the next 16 hours. If
the next ask says your report is on file but does not count, it names why;
follow that.

Do not store or change anything else unless the owner asks for it. The
exceptions are the work the steps themselves call for: the verdicts, the
revisions of rotten facts, the calls on the batch and the A4 list, the
captures of what was missing, and filing the report itself.
