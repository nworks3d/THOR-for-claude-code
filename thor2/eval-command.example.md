---
description: THOR session evaluation - judge what fired, repair what has rotted, give teeth to what nothing could stop, capture what was missing, report what the gate did and where it chafed, and close with a plain-language summary
allowed-tools: mcp__thor__mark, mcp__thor__get, mcp__thor__lookup, mcp__thor__remember, mcp__thor__revise, mcp__thor__retract, mcp__thor__status, mcp__thor__history, Read, Grep, Glob, Bash(git status:*), Bash(git diff:*), Bash(git log:*), Bash(cargo test:*), Bash(cargo build:*), Bash(npm test:*), Bash(npm run verify:*), Bash(node --test:*), Bash(pio run:*), Bash(pio test:*), {{THOR_DOCTOR_ALLOW}}, {{THOR_RETEST_ALLOW}}
---

THOR session evaluation. Work through steps 1 to 8, then the report. You need
no prior knowledge for this: everything is below. Do not skip a step because
it seems like there is "nothing to report" - write "nothing" for that step
explicitly instead. An empty step is a finding; a skipped step is a gap.

Invent nothing. If a step turned up nothing, say so. A made-up finding costs
more than a missed one.

Do the work yourself. The session that runs this evaluation makes every THOR
call, every check and every test itself, and does not hand the evaluation to a
subagent: verdicts and filings count only from the evaluating session's own
calls, so work a subagent does is not counted.

## What a mark means

A useful mark is not a dead signal. The latest verdict is what counts:
- a useful mark clears whatever noise came before it and resets the count to
  zero;
- a noise mark after that counts again, on its own;
- two noise marks since the last useful mark take a fact out of the
  injection channels - it stays findable through lookup regardless.

No Stop hook asks for a verdict on a single item any more (since THOR
2.5): the items that fired in many sessions without a verdict wait for this
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
been quiet longer, the next ask takes the evaluation over: the old one is
closed as superseded and its verdicts stay. A project with activity in the
last seven days and no evaluation that counts in seven days is due whatever
the clock says.
Since THOR 2.5 the judgement debt ("N item(s) have fired repeatedly") and the question about old rules are not
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
start note (the batch and the A4 list), not at a turn-end. Where a line prints
a command, it is `serve ... retest --id '<id>'` - a DEMOTION line prints it in
full, with `--apply`, and so does the gate when it refuses a report - written
with the real program and store path, forward slashes and no `&`: that is
exactly the form this routine's `allowed-tools` pre-approves, so run it as
printed. Do not retype it in another shell form - a PowerShell form is not
pre-approved and a headless session is refused.

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
for up to the time box that the start note names (20 seconds by default) (the rules it did not reach in that time are counted in
the note as "not reached this time: N" - they are not owed in this evaluation
and they lead the next start), and the note it shows you afterwards names four
things, ordered by what you must act on, with what needs no action last:
1. JUDGEMENT - the items you give a verdict on (step 2);
2. TEETH - what THOR decided by itself, then the lists that are yours: the
   batch (at most eight rules), the A4 list (at most sixteen) and the
   disputes to verify (item c), each item with its reason and the exact call
   to make (step 4);
3. SIGNALS BEFORE YOU FILE - THOR's health and the refusal outcomes THOR
   already knows at the start, each with its meaning, its calls and when it is
   done (step 6). Act on them before you file; the machine section of your
   report records the same two signals again for the evaluation's own window;
4. OPEN POINTS - THOR's own, written and closed by THOR. Nothing for you to
   do; the note names how many carried each one and only the last three.

That note is the one source for your work. If you missed it, the gate names
the same lists when it refuses the report. If the note says THOR is still
deciding the old rules, end this turn once: the next Stop shows the finished
note. Claude Code shows a hook text whole only up to about 9 KB, so when the
lists are long the note leaves the least actionable items out, from the far
end of each list, and every cut says "N more item(s) of this list are NOT shown
here" with the call that reads the whole note: `mcp__thor__get(id: "<the record
id named in the note's first line>")` shows the start record, whose text is the
whole note. Never act on a cut list as if it were complete: read the whole note
first.

---

## Done, or cannot - the bar for steps 4, 7 and 8

WHAT IT MEANS. Three steps below ask you to finish something: teeth (4),
friction (7) and verification (8). Every point in them ends in one of two
outcomes, never in anything between:

1. **Done**, with the proof in the form the step names: an id with its before
   and after, an anchor from and to, a check with its kind, literal and place,
   or the command and its result.
2. **Cannot here.** The rare exception, and it costs something: you write down
   what you tried and how that ended (the call and the refusal, or the command
   and its error), and the ONE thing that is missing - a decision of the owner
   (then the question goes in the report, worded so it can be answered in one
   sentence), or a machine, tool or access this session does not have (say
   which). If you cannot fill in both, it can be done after all, and you still
   owe outcome 1. "No time", "later", "still to do" and "too big for now" do
   not exist as outcomes.

WHAT TO DO. For each point, make the call or run the command the step names,
then write the outcome in the report under that step's number: outcome 1 with
its proof, or outcome 2 with the attempt and the one missing thing.

WHEN IT IS DONE. A point is closed once it carries outcome 1 or outcome 2.
"I did not do this" is a gap, and a gap closes nothing. "I could not, because"
carries an attempt and a reason and closes the point as well as outcome 1
does: honestly finding that something cannot be done here blocks nothing.
What you leave lying does not come back to you but to the owner, which is
exactly what this evaluation is meant to prevent. Measured 2026-09-23: two
evaluations on one day reported the same friction, predicted it would return
and left it; three new checks went on a list for later and nobody looked at it
again. When the owner raised it, the repair took two minutes and the proof a
single command.

EXAMPLE. Outcome 1: "rule `no-force-push`: revise accepted, THOR answered
'teeth: wired, not proven', check forbidden on the literal `--force`". Outcome
2: "rule `deploy-needs-vpn`: I tried a `contains` check on `deploy.sh` and THOR
refused it because this checkout is not registered as the project's working
copy; what is missing is the owner's decision on which folder is the working
copy".

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
  paraphrase of one. Run it in a plain form that `allowed-tools` pre-approves:
  the command itself from the project root (for example `cargo test
  --manifest-path <dir>/Cargo.toml`), never prefixed with `cd x &&`, which is
  not pre-approved.
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
(`mcp__thor__lookup(scope: "<project>", kind: "report", query:
"evaluation-report")`; take the newest one, then open it with
`mcp__thor__get(id: "<its id>")`). Read its remarks - what it left unjudged with a reason, the
gaps it left open, every point it reported in step 4, 7 or 8 as "cannot here",
and everything it listed as not verified. For each: settle it now, or write it under "not settled" in your own
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
pinning it. Never `mark` an item that is not on your frozen list, even if it
fired in the wrong place: a wrong-place firing off the list goes to step 7 as
friction (anchor), not to a verdict.

Judge each item by WHERE it fired, never by its text alone. Under every item
the start note shows where it fired in this session - how often, and its last
places (the command or file); `mcp__thor__get(id: "<id>", fired: true)` shows
an item's recent firings. With a list of more than about 50 items, build a
firing map FIRST: one pass over this session's transcript (a subagent may only
READ it, to build the map) that says, per id, on which call it fired - then
judge. Every verdict, THOR call and filing stays the session's own. Never
get-then-mark blocks of items without looking where they fired: measured
2026-10-01, one session gave 205 verdicts in four minutes that way, and three
were provably wrong.

Every item on your list gets one of two answers. A verdict, given with the
THOR tool itself: useful if it belonged where it fired, noise if it did not.
A verdict from a shell, a script, another session or another program settles
nothing (`serve mark` in a terminal included), and is listed for the owner under "side doors". Or, when you cannot
honestly judge it - "when in doubt, skip" above - an explicit skip, written
as a tag on the report itself: `not-judgeable:<id>:<reason>`, with a reason
of at least twelve characters that says why, for example
`not-judgeable:a-1f2e3d:it fired on a grep of a log I never opened`. A skip
settles that item for this report only; it stays owed store-wide, and the
next session that sees it fire is asked again. Do not re-mark something that
already carries a verdict - that would count twice.

GIVE YOUR VERDICTS, THEN END THE TURN ONCE, THEN FILE - but only when you gave
verdicts in this turn. THOR checks that every verdict of this evaluation was a
real THOR call when the turn ends, and the report is refused while the verdicts
of the turn you are in are not checked yet. So when you gave verdicts: answer in
a line and let the turn end; the Stop hook checks them and holds the turn again,
asking you to file NOW - file the report in that same turn (see "The report").
When you gave no verdict in this turn (your list was empty, or every item got a
skip tag), there is nothing to wait for: file right away, in this turn. If no
THOR message held you, file right away too. Never tell the owner "next turn" or
"I will file it later" unless the Stop actually held you.

The health check's `judgement debt` line names the debt of the whole store
for this checkout, including items that only fired in other sessions. Those
are not this report's to settle and never hold it; judge one of them only if
you saw it fire yourself and know where. Step 2 is done once every
item on your list carries a verdict or a skip tag with a reason.

## 3. Repair what has rotted (this is not noise)

Did a fact come up that was still RELEVANT but no longer matches the
artifact - the code, the file, the current state? That is not noise, and
not "missing" either. Verify it against the artifact and revise it NOW.

Check explicitly every fact that carries a VALUE (a number, a line number, a
percentage, a date) rather than a pointer. These rot silently: measured 2026-08-08, 1 in 10 served facts was untrue, and
three of the nine untrue ones were exactly this shape. The fix is
not a new value but a pointer: "read this from the health check" instead of
a number that will drift the moment the store changes.

Also check every fact that describes a problem as still open when it has
since been fixed (two of those nine were that).

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
you start this step and again once you finish it - the report names both. Use
the fast form (`--eval`, see "The report") for both: it takes seconds, so wait
for it each time.

So you do NOT walk the store's heavy facts, and you do NOT pick your own. You
handle only what the start note handed you under TEETH:

- **The batch**: at most eight rules that need a fix only you can make, each
  line giving the reason and the call. Make the fix the line names, then run
  the call shown: a `mcp__thor__revise(id: "<id>", ...)` that carries that
  change - the printed call has `<the change its line names>` where the line
  says what to change (a check, an anchor, the text), and a `because` of at
  least twenty characters that the gate REQUIRES whenever the change takes a
  quoted form out of the text or weakens the rule (clears or changes its check,
  lowers its severity, drops an anchor). THOR tries the rule again as it is
  written now and answers rule, note, or what is still missing. A bare
  `mcp__thor__revise(id: "<id>")` changes nothing a trial reads, so it settles
  nothing: THOR answers it "NOTHING WAS SETTLED, '<id>' IS STILL OWED" and
  prints the line's exits again. A `no-literal:` tag answers nothing: when a
  batch line names a rule that carries one, send `tags` without that tag in the
  same `revise` as the check ("Send the tags without that no-literal tag in the
  same revise as the check", as THOR words it), or THOR refuses the call because
  a check that refuses and a no-literal tag contradict. A `revise` that hangs a
  check and sends no `tags` at all has the tag dropped by `revise` itself, and
  the reply says so. A DEMOTION line prints its own two exits, described next.
  The report cannot be filed until every rule of the batch is settled, and the
  gate counts what is left. Rules that did not fit stay owed and lead the next
  evaluation. To look at the file or command a rule is about,
  use `Read`, `Grep` and `Glob` (they are allowed here); never a shell.
- **The A4 list**: at most sixteen rules whose check bites and whose text only
  has to name the check's literal. Reword the text so it names that literal,
  using the exact call the note prints (a `revise` with the reworded text) -
  nothing else has to change. Each line says how many characters the text has
  left (300 minus its length now): reword within that budget, never append.
- **The disputes to verify** (item c of the note): at most eight rules that
  were stored by dispute (`legitimate_calls`, `may_change` or
  `accept_refusing`) since the previous report and that nobody has checked
  yet. WHAT IT MEANS: the writer told THOR that the tooth THOR built from the
  rule's own words is wrong (with `accept_refusing` the rule keeps its check,
  and the writer says that refusing those calls is the intent). A dispute turns
  a rule that bites into a note that can never refuse anything; an audit on 2026-10-07 found 7 of 11 such disputes wrong, because
  the disputed literal was the rule's real subject or a better check existed.
  WHAT TO DO, per item: each line shows what was disputed, the writer's reason
  (or "no reason recorded") and two exact calls. Open the rule and the file or
  command it is about and decide whether the disputed literal IS what the rule is about.
  If it is, or a better check exists, make call 1: a `revise` that rewords the
  text so it names the real subject and hangs the check (for a disputed span
  of a file the line names the rule's file in `check_path`). If it is only an
  incidental span (the plain test: the span may change while the rule's own
  claim stays true, like a version number written next to the rule's real
  subject), make call 2: `mcp__thor__revise(id: "<id>", because: "<why
  the span may change while the rule stays true, at least twenty
  characters>")` - THOR refuses a reason shorter than that, and a filler such
  as "ok" settles nothing. Say in the reason which file or call you read and
  what you found there (for example: "read sensor.cfg line 12: the word delay
  only labels a comment; the unit is on line 14"), because a reason written
  without opening them is what the audit found wrong. An item is done when one of the two calls was
  accepted, or when you retract the rule with a reason because it is simply
  wrong; nothing else settles it (not a tag, not a note, not a line in the
  report), and the report cannot be filed until every item is done. Example: a
  rule saying "the sensor delay is in microseconds, not nanoseconds", disputed
  on the loose word "delay": the subject is "microseconds", so call 1 hangs a
  `contains` check on it. The machine section of your report lists each item of
  this list under DISPUTES TO VERIFY, one line each: "verified by call 1" (a
  check was hung), "verified by call 2" (with your reason), "settled by a
  retract", or "NOT verified". The machine section has THREE dispute lists, and
  only the first is yours: (1) "DISPUTES TO VERIFY (frozen at the start)" is
  item c of the start note, the rules you verify, one line each; (2) "notes by
  dispute written in the window" and (3) "rules kept with accept_refusing
  written in the window" list disputes that were written DURING this
  evaluation, by anyone (another session included), as information for the
  owner and for the next evaluation's item c. You do not verify lists 2 and 3,
  nothing there is owed by you, and a rule of item c does not need to appear in
  them.
- **A DEMOTION in the batch**: a batch line that starts with "DEMOTION, nothing
  written". WHAT IT MEANS: THOR's matcher (the code that reads a command and
  decides whether a rule refuses it) changed between builds. This rule bit when
  the older matcher tried it, and the newer one would turn it into a note that
  can never refuse anything. THOR writes nothing on its own for this: the rule
  keeps its tooth until YOU decide - the owner is not asked. WHAT TO DO: open
  the rule (`mcp__thor__get`) and the file or command it is about, read the
  reason on the line (it names the forms the check refuses and the forms it
  lets through), and take one of the two exits the line prints. A bare
  `mcp__thor__revise(id: "<id>")` is NOT an exit: it changes nothing a trial
  reads, so it writes nothing and the rule stays owed.
  (a) ACCEPT the demotion, only when you checked that no wording or anchor can
  make the check bite under the new reading: run the retest command the line
  prints, as printed - the real `serve` path, `--db` with the real store,
  forward slashes, `retest --id '<id>' --apply`. It writes the note and
  nothing else; it needs no `--allow-demote` (that flag belongs to `--compare`
  runs). Your report says that you accepted the demotion and why.
  (b) KEEP THE TOOTH by narrowing the rule to the forms its check really
  catches: send the Call the line prints. It is a `revise` with the three
  things THOR's trial needs - `text` reworded to name only the caught forms (a
  quoted form taken out of the text is a weakening), `check_kind: "forbidden"`
  with `check_literals` narrowed to those forms (the line lists them), and a
  `because` of at least twenty characters of your own saying why. A `revise`
  with only the text is refused, and its refusal steers toward `clear_check`,
  which is a weakening and never the way here. Write down in the report the
  forms the narrowed rule no longer covers: that gap stays open.
  WHEN IT IS DONE: the rule is no longer on the batch - THOR answers rule for
  (b), or note for (a); the filing gate accepts either. EXAMPLE: a ban on
  resetting two named deploy checkouts lists four refused forms, and the new
  matcher lets the two joined with `&&` through. (b) is `mcp__thor__revise(id:
  "ban", text: "Never hard-reset the deploy checkouts alpha-dev and beta-prod.
  Refused: alpha-dev reset, beta-prod reset.", check_kind: "forbidden",
  check_literals: ["alpha-dev reset", "beta-prod reset"], because: "the two
  forms joined with && are not refused by the matcher, so the text now names
  only what the check catches")` and THOR answers "teeth: wired, not proven";
  (a) leaves the text as it is and turns the rule into a note.
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

Two facts about literals. A check's literal matches WHOLE WORDS only:
`--force` does not match inside `--force-with-lease`, and `ssh` does not match
inside `sshKey.mjs` (it is found in every part of a command, though: after
`cd x &&`, on a later line, inside `ssh host "..."`). So a literal catches
exactly the form you wrote and no longer one, and you never stretch it to catch
a neighbour. And a literal comes from the rule's own text or from a real call on
record (`mcp__thor__history`, the log) - never one you made up to get a check
accepted.

A rule can be on the batch AND named by the refusal outcomes (step 6) at once -
it was refused, then weakened, and now it is owed. Decide the refusal outcome
FIRST: open its history (`mcp__thor__history(id: "<id>")`) and decide whether
the refused call was legitimate. THAT decision tells you the batch call: if the
refusal was wrong (the weakening was right), the batch fix is the narrower rule
or the confirmed dispute; if the refusal was right, the batch fix puts the
tooth back (the restore call under the refusal outcome is the same revise).
Never make the batch call first and decide the refusal afterwards.

Three arguments exist for a refusal that is right about the tooth but wrong
about your rule: `legitimate_calls`, `may_change` and `accept_refusing`.
Doctor lists every rule saved with one of them for the owner, so use one only
when you can say why - and each one needs `because`: a sentence of at least
twenty characters that THOR keeps in the rule's history and prints beside the
dispute in the report's machine section and in doctor. Without it the write is
refused. Try the better path first: hang a check on the literal that IS the
rule's subject, and dispute only a span that is incidental. They are not a way
to get a rule off the batch. A `legitimate_calls` entry must be a
call that really occurs - in the log (THOR's reply says how many real calls a
tooth reaches) or in the project. Never a made-up path or a template with
`<...>` in it: measured 2026-10-01, a session named a test file that did not
exist.

When a revise reply says bind, anchor or rebind (for example "bind it to
<file>"), make that rebind as a revise of its own, or write "could not" with
the refusal. Since THOR 2.5 a rewording never clears a true check: a reply
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
rule with no project whose check reads a file) and refuses yours. A point says
itself what happens and closes itself once the anchor has room. A rule held
by one of those is not on your lists and is never yours to clear: clearing a
check that is true is never the way past a blocker. A full anchor that does
have an honest exit (a lighter fact, or a fact with no check that can refuse)
is no point but your work: the batch line names the occupants and the three
exits, and you carry one out. A check you clear,
move to another file or change during this evaluation is listed in the
report's machine section for the owner. A legacy open point
an older routine filed settles nothing and holds nothing; retract one, with a
reason, only when what it names is resolved.

Working in a git worktree? Every check is judged against the MAIN checkout -
the health check and the commit gates read that one, never your worktree. A
check whose literal or file exists only in your worktree is refused at the
write: it would read as a false memory on main and block unrelated commits
there (measured 2026-09-26: eleven minutes of blocked commits). Attach it once your change is merged; until then the fact stays
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
builds, and its answer to your revise (which calls it refused and which it let
through) IS the proof that counts here; only a real refusal at real work
counts beyond that.

Step 4 is done once every rule of the batch and of the A4 list has its call
made and THOR's answer to it (for a DEMOTION: one of its two exits taken),
every dispute to verify has its call 1 or call 2 accepted (or its rule
retracted with a reason), and nothing is left hanging on a refusal.
Retracting a heavy rule takes a reason that says what changed about THAT
rule; "no longer relevant" is refused. This is outcome 1 of the bar above: per
rule the call, with THOR's answer to it. For a rule that truly cannot be done
here - the fix needs a decision of the owner, or a machine this session does
not have - outcome 2 applies, with the attempt and the one thing missing; the
rule stays owed and goes under "not settled".

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
- **REFUSAL OUTCOMES (probable false blocks).** Two places show it: section 3b
  of the start note (what THOR knew at the start, since the previous report) and
  the machine section of your report, which you read in the reply to your
  filing (the refusals of the evaluation's own window), in a line that starts
  "REFUSAL OUTCOMES in the window". WHOSE REFUSALS: section 3b counts the
  verified refusals of EVERY session since the previous report, yours included
  (a refusal you met earlier in this session is there). The machine-section
  line covers only the evaluation itself, a few minutes, and counts OTHER
  sessions' refusals only: a call you made inside your own evaluation proves
  nothing and is not counted, and the line says so at its end. So if the gate
  refused one of your own calls during the evaluation, list it yourself:
  `mcp__thor__history(id: "<rule id>")` shows the refusal, and point 6 of the
  report names the rule id and the refused call. DONE WHEN: each of your own
  refusals is named there, or the report says there were none. EXAMPLE: "my
  own `Start-Process` was refused by `no-visible-window` during the evaluation;
  the call was a harmless URI launch, so I report it as a probable false
  block". WHAT IT MEANS: THOR looked at every verified
  refusal, found what happened to the refusing rule afterwards, and counts a
  refusal as a PROBABLE false block when someone other than THOR then retracted
  that rule or weakened it (cleared or changed its check, lowered its severity,
  dropped a binding). It is a proxy, not a verdict: a rule can change for other
  reasons, a refusal no session has verified yet is not counted, a noise mark is
  shown in `history` but never counted, and a rule refused twice counts twice.
  Under the line, each probable false block is named: the rule id, the call
  that was refused (cut short and redacted the way the log keeps it) and what
  followed - at most eight, then "and N more". WHAT TO DO, for each rule so
  named: open its history (`mcp__thor__history(id: "<id>")`) - the refusal step
  shows the refused call, the later steps show the change, and every step shows
  the rule as it stood (check, bindings, severity) - and decide, from that call
  and the file or command the rule is about, whether the refused call was
  legitimate. If it WAS (the refusal was wrong), the weakening was right: say so
  in the report, with the rule id, the call that was refused and why it was
  legitimate (this is the false block above). If it was NOT (the refusal was
  right and the weakening was a mistake), restore the rule with the call the
  start note prints under it ("If the refusal was right, restore: ..."): for a
  weakened rule a `mcp__thor__revise(id: "<id>", ...)` that sets back what the
  weakening took away, with a `because` of at least twenty characters of your
  own saying why the weakening was a mistake. The restore puts back what the
  weakening removed. If that old check was itself too broad (it also refuses the
  legitimate call that was refused), narrow it in the same revise to a literal
  that refuses the harmful form but not the legitimate one. Never widen it. For
  a RETRACTED rule a
  `mcp__thor__remember(...)` of the whole rule under a NEW id (`<id>-restored`,
  with a tag `restored-from:<id>`) - never under the old id, because a second
  `remember` on a retracted id leaves it with two heads (`get` then refuses to
  read it until a `resolve`), and a retraction is never undone by a `revise`.
  If you cannot tell, say so for that rule and why, and restore nothing. WHEN
  IT IS DONE: every named rule has one of those outcomes written in the report,
  and a restore you made was accepted by THOR (rule or note, never a refusal). A
  rule that refused a call and was NOT weakened or retracted afterwards is a
  refusal that stood. Name it in the report (the rule id and the refused call, read
  via history) as a right refusal. Nothing else is owed. A
  line that says "no verified refusal" needs nothing. For a window longer than
  eight blocks, work through the ones the start note and the section name, and
  say in the report how many more there were. EXAMPLE: the line names rule
  `no-force-push`, refused `git push --force-with-lease origin main`, then
  check cleared. The refused call is safe, so the weakening stands: write
  "no-force-push refused git push --force-with-lease origin main, which is
  safe; clearing its check was right". `mcp__thor__history` shows the refusal
  as `gate_refused by hook [...]: refused call: '...'`.
- **THOR health.** Section 3a of the start note and the machine section of the
  report each have a line "THOR health ...: N hook launch failure(s) (...), M
  version-skew event(s) (...)". WHAT IT MEANS: a hook launch failure is a Claude
  Code hook (THOR's guard or Stop hook) that could not start, so nothing from
  THOR reached that call - the brackets name the hook event, its exit code, the
  session whose hook failed and the time of the failure in UTC (the count
  covers every session in the window, not only yours); a version-skew event is a filing that THOR refused because
  the tool server that wrote the report was built from another THOR than the
  checker ("written by a tool server of another build than the checker"), which
  is nobody's edit - the brackets name the report and the session. There is
  nothing to fix in any note. WHAT TO DO: when both are zero there is nothing to
  do. Otherwise put both numbers and the names in the report, and tell the owner
  in the summary: for a skew, to restart Claude so the tool server is the
  current one, and for a launch failure, that the named hook needs his
  attention. You cannot restart or repair either yourself. WHAT HAPPENS WITH A
  SKEW, as THOR really does it: the tool server you file through and the checker
  the Stop hook runs are two programs. When the installed checker is of another
  build than your tool server, the tool server refuses the filing ITSELF, before
  it writes anything: "REFUSED: this tool server renders the evaluation machine
  section in format N, but the installed checker (serve.exe) reads format M".
  WHAT TO DO: tell the owner to restart Claude (the tool server loads only when
  Claude starts), end your turn, and file nothing more in this session. DONE
  WHEN: the owner restarted Claude and the next end-of-turn ask came. EXAMPLE
  (for the summary): "the tool server is an older build than the checker; I did
  not file; restart Claude and the evaluation is asked again". If a skewed
  report did get on file (from a tool server that predates that refusal), it is
  on file but does not count, and filing it ended THAT evaluation; THOR then
  says one line at each Stop ("Restart Claude; the evaluation is asked again
  after the restart") and does NOT start a fresh evaluation or hold the turn in
  that session, so do not file a second report: it would be dead the same way.
  After the owner restarts Claude (the same session id resumes), the next
  end-of-turn ask starts a FRESH evaluation (a new start note, new lists and a
  new log position - it does not reopen the old one), and in that turn you file
  its report under the id `eval-<project>-<YYYY-MM-DD>-2` (`-3` if `-2` is
  taken). That new start note's THOR health line lists the skew (it
  counts everything since the previous report); the new report's machine section
  does not, because its window starts at the new start. WHEN IT IS DONE: both
  numbers and the names are in the report, and the owner was told. EXAMPLE:
  "THOR health: 1 hook launch failure (PreToolUse, exit 126, session 0de7dc52, at
  2026-10-06T04:18:30Z) - the owner should look at that hook; 1 version-skew
  event - the owner was told to restart Claude". A THIRD PART may follow the two counts: "; also N hook(s)
  failed while the machine was going to sleep or shutting down - not THOR (...)".
  WHAT IT MEANS: in the same session transcript a hook of ANOTHER program
  (not THOR's own) failed within about two seconds of THOR's hook with the same
  exit code, which is what a PC suspending or shutting down looks like (nothing
  can start any more). THOR did not fail, so those are NOT in the launch-failure
  count. WHAT TO DO: nothing - do not tell the owner it is a THOR problem, and do
  not add it to the launch-failure number. You may name the count in the report
  as "not THOR". `doctor` says the same in its hook line ("N hook(s) failed while
  the machine was going to sleep or shutting down - not THOR"). WHEN IT IS DONE:
  there is nothing left to do. EXAMPLE: "THOR health: 0 hook launch failures; 3
  hooks failed while the machine was going to sleep or shutting down - not THOR,
  nothing to do". A failure recorded before THOR classified them is looked up in
  the session transcript again when the start note is made, so it is already
  marked "not THOR" whenever the transcript still holds the sleep. THOR does
  this sorting itself, never you: a failure counts as "not THOR" when a hook of
  another program failed in the same transcript within two seconds of it with
  the same exit code. So a failure still listed as a THOR launch failure was
  not matched: report it as listed. Add "possibly the machine" only when its time
  (each failure names it) is within a minute of another hook failure with the
  same exit code in the same session (exit 66 with no output is the usual
  look of a suspend); otherwise tell the owner the hook needs attention.
  WHEN IT IS DONE: the report says "possibly the machine" for it, with the
  time. EXAMPLE: "PreToolUse exit 66 at 2026-10-06T04:18:30Z, another hook
  failed with exit 66 at 04:18:50: possibly the machine".
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
     thing? Fold yours into it with `revise` and retract yours. Two half
     copies take two places for one piece of knowledge.
  2. **Anchor it narrower.** Is it hanging off a broad moment or a busy file
     when it is really about a more specific file or command? Re-anchor it.
     Measured 2026-08-08: re-anchoring 35 facts took the number of unreachable
     facts from 52 to 20.
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
report, every rule the REFUSAL OUTCOMES lines name (in the start note and in the
machine section of the filing reply) has its outcome written (and a wrongly
weakened check restored), the THOR health numbers and the hook events they name
are in the report, and every displacement notice is either resolved or
deliberately tagged - reporting alone is never enough here.

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

For each point of friction, first look for a rule that should have prevented
it: `mcp__thor__lookup(query: "<the subject of the friction>")`, then name the
id you found, or write "none". If one existed, the question is why it did not
reach the session - one of three: wrong anchor (it fired at another moment or
file than where the mistake fell: re-anchor it, step 4), displaced (it fired
nowhere: step 6, the three exits), or no teeth (it stood there and stopped
nothing: try to give it a tooth that holds, step 4). If none existed, step 5
captures it. Skipping this look-up is what let two evaluations on 2026-09-23
miss it: the repair turned out to be two rules at the wrong moment and a third
without teeth, and nothing in the routine asked for the look.

Then every point of friction gets one outcome of the bar above - done (what
changed: the id with before and after, the anchor from and to, the new id from
step 5, or the file) or cannot here (what you tried and the one thing that is
missing). A point that cannot be done here goes under "not settled" in your
remarks, and step 2 of the next evaluation picks it up.

When the fix for a friction point lives in THOR's own code or tool texts, not
in the memory, the outcome is "cannot here": name the one missing thing as "a
THOR code change", describe the change in one sentence, and write it in the
report so the next review picks it up. And a rule that fired in the wrong place
but is not on your frozen judgement list is friction (anchor) here, never a
`mark` (step 2).

This step has a third outcome: **one-off**, when it will not come back and you
say which circumstance caused it and why that circumstance is gone. If the
same point was in the previous report, one-off is no longer available. Writing
"it will come back" and moving on is no outcome but proof that you still owe
one.

Step 7 is done once every point of friction carries one of those three
outcomes, with the id of the rule found or "none" beside it. For example:
"a revise was refused because the anchor was full; rule found: none; done -
folded into `anchor-full-fold-first` (step 5)".

## 8. What you did NOT verify

Name explicitly what you are assuming but have not checked, then settle each
point with the bar above: checked, with the result, or cannot here, with the
reason. A list for later does not exist any more - nobody read it, and the work
landed on the owner.

If you added a fact bound to a target, this evaluation only ever sees what
FIRED - a silently displaced binding shows up nowhere here. THOR's own trial
is the proof you have: its answer to your revise says "tried ... (refused)"
or "(let through)". What the trial cannot show is whether the binding ever
comes up in real work, and that is the ONE thing missing then: put the tooth
under "not verified" with the command and what should make it refuse. A
refusal at real work counts as a bite by itself, once THOR has checked it at
the end of a turn. A call you plant here is recorded as proof of nothing (step
4). Never test with a file check or with free prose - that proves nothing, and writing the mistake into a file only risks
it. A check on a file is tried by THOR itself against the
project's registered working copy; one it cannot try yet is kept as a note
with a pending check, and doctor lists it.

Step 8 is done once every point carries one of the two outcomes: the command
or doctor line with its result, or the reason it could not be done here - for
an untested tooth, the command and what should make it refuse. For example:
"rule `no-force-push`, bound to the command `git push`: not verified in real
work; it should refuse `git push --force origin main`".

---

## The report

Open with a plain-language summary: what happened, does it work, what does
it mean for the owner. No jargon, no hashes, no test names. The detail
follows below, in the order of the steps above.

Every claim in the report is re-run in THIS turn, or marked "not verified" (a
health check, what origin holds, a version, what is still running). Counts -
verdicts, useful, noise, rules - come from THOR's replies and the machine
section, never from memory.

WHAT YOU SEE WHEN. The start note's section 3 and the lists of step 4 are what
THOR knows BEFORE you file: act on those lines first. The machine section
exists only once the report is filed, and THOR prints it in full in the reply to
your filing - its health line, its REFUSAL OUTCOMES lines, its DISPUTES TO
VERIFY lines. Read that reply. Anything it names that the start note did not
(a refusal or a hook failure of the evaluation's own window) you handle after
filing, and you correct your own text with `mcp__thor__revise(id: "<report
id>", text: "<TLDR and remarks, no section header>")`: THOR keeps the machine
section byte for byte and stores your new text below it. Verified: such a
revise, made before or after the Stop validated the filing, leaves the report
counting (the end-of-turn check reads the event that FILED the report and the
digest of its machine block, and a text revise changes neither), so do not file
a second time for a text fix. Never write numbers into your first filing that
you have not seen: take them from the start note, your own replies and, after
filing, the section.

Reading the machine section, line by line, so none of it is a guess:
- Its first lines say "started by ... (THOR's record '...', itself written at log
  seq 18); N = 17 is the log position the start note names": the start note's
  "log position" is N, and the start record itself is the next event, so the two
  numbers differ by one on purpose.
- Under TEETH, every item of the frozen batch and A4 list has one line saying how
  it was settled: "a rule (wired, not proven) by THOR's trial at log seq N,
  written by an attested THOR call" (your call worked), "settled without a write -
  its check already holds in the registered working copy; nothing to do" (nothing
  was written and nothing is owed: the gate read the check in the working copy and
  it holds), "retracted ...", "taken off the rule track ...", or "tried at log seq
  N and rewritten since - no verdict stands". Only a line that says the rule is
  still owed (or has no verdict) is open work.
- The REFUSED line counts the hook's refusals in the window and names the rule of
  each ("Rules the hook refused: 'a', 'b' (2x)"), so none is anonymous; under
  REFUSAL OUTCOMES every probable false block carries the same Read and restore
  calls as the start note, for you to use as step 6 says.

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

Open questions in the report are only for what is truly the owner's (money,
publishing, deleting his data, security settings) - never for memory
maintenance that THOR's rules let you decide yourself.

1. the state of the work: uncommitted changes and who made them, the actual
   test/build result you ran, background jobs and their state, and any
   promise you found made without proof;
2. what you judged as noise and why, and what belonged where it fired;
3. what you revised, with before and after, and which artifact you checked
   it against;
4. the health check's `teeth` line before and after step 4; the batch and the
   A4 list: per rule, the call you made and THOR's answer (rule, note, or what
   was still missing), and for a DEMOTION which exit you took and why;
   every dispute you verified, with the call (1 or 2, or the retract) and your
   reason; and every rule you saved with `legitimate_calls`, `may_change` or
   `accept_refusing`, with the reason;
5. what you stored that had been missing;
6. what the gate did - including every false block, every rule the REFUSAL
   OUTCOMES line names with its outcome, the THOR health numbers with the hook
   events and sessions it names, and every refusal of your own write actions;
7. the friction, per point with its outcome - done (what changed), cannot here
   (what you tried, what is missing, and the question to the owner if there is
   one) or one-off (why) - and the id of the rule that should have prevented
   it, or "none";
8. what you checked, with the result, and what could not be done here, with
   the reason.

Get the numbers about the state of the store from the health check, never
from memory and never from a fact. That is a SEPARATE program, not a
subcommand, and it is not on the PATH - run it with its full path. Use its
FAST form, `--eval`: it prints only the lines this routine asks for, each
exactly as the full check prints it, and it ends in seconds (about 7 seconds
on a store of 270,000 events, measured 2026-10-07). RUN IT IN THE FOREGROUND
and wait for it to end: do not start it in the background, and do not note,
quote or file a line before the command has returned. A line you did not
read is "not verified", and for a line this form prints that is never an
acceptable answer.

    {{THOR_DOCTOR_EVAL}}

In a Bash tool (Git Bash on Windows) use this form instead, the one this
file's `allowed-tools` pre-approves, so no permission prompt interrupts it:

    {{THOR_DOCTOR_EVAL_BASH}}

BEFORE YOU FILE, read the first lines it prints. If one starts `tool server:` or
`installed programs:`, the tool server you file through is older than the
installed programs, or the programs were not built from one source. WHAT IT
MEANS: a report filed now would be refused as version skew (see THOR health
above) and be a dead report. WHAT TO DO: do not file; tell the owner in your
summary to restart Claude (and to run the installer again for `installed
programs:`), and end the turn. The next end-of-turn ask files the report.
STEPS 1 TO 8 STAY: you still do them all, and the verdicts, batch calls and
other THOR calls you make are written and count; only the filing is withheld.
With no report filed, the summary for the owner goes in your chat reply. The
installer is not a command you can name here: say "the owner re-runs the THOR
installer". DONE WHEN: the summary says so and no report was filed. EXAMPLE:
the output holds "tool server: 1 running mcp.exe process(es) started before
mcp.exe was replaced" -> summary: "the tool server is stale; restart Claude, then the
evaluation is asked again; I filed nothing". When neither line is printed,
nothing is wrong and you file as usual.

The last line it prints says what it left out. For one of those lines (the
store and chain line, the command anchors, the hook launch failures, the
refusal outcomes, the open points) run the full check, the same command
without `--eval`. It takes longer (about 20 seconds on that same store), so
run it only for such a line, wait for it to end, and never file the report
while it is still running:

    {{THOR_DOCTOR}}

and in a Bash tool:

    {{THOR_DOCTOR_BASH}}

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

Example of reading the output: for step 4 you note the one line that starts
`teeth: N of M rule(s) marked irreversible or costly carry a check shaped to
refuse` - write down N and M before the step and again after it. The other
`teeth ...` lines under it are the detail of the same check. A line that is
missing from the output is not an error: `ship` and `contradictions` are
silent when there is nothing to say, and the report says so instead of
inventing a number.

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

The reply to a filed report summarises what the machine section holds, prints
the whole section ("THOR's machine section of this report, in full"), and says
that THOR checks the filing call when the turn ends. Read the section in the
reply: its health, refusal-outcome and dispute lines are the final window of
this evaluation (see "What you see when" above). The report counts from the
end-of-turn check on: the Stop hook reads the answer back from THOR's log, and
only a report that counts lets a blocked turn end and stays quiet for the next
16 hours. If the next ask says your report is on file but does not count, it
names why; follow that.

Do not store or change anything else unless the owner asks for it. The
exceptions are the work the steps themselves call for: the verdicts, the
revisions of rotten facts, the calls on the batch and the A4 list, the
captures of what was missing, and filing the report itself.
