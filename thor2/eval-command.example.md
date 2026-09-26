---
description: THOR session evaluation - judge what fired, repair what has rotted, give teeth to what nothing could stop, capture what was missing, report what the gate did and where it chafed, and close with a plain-language summary
allowed-tools: mcp__thor__mark, mcp__thor__get, mcp__thor__lookup, mcp__thor__remember, mcp__thor__revise, mcp__thor__retract, mcp__thor__status, mcp__thor__history, Bash(git status:*), Bash(git diff:*), Bash(git log:*), Bash(cargo test:*), Bash(cargo build:*), Bash(npm test:*), Bash(npm run verify:*), Bash(node --test:*), Bash(pio run:*), Bash(pio test:*), {{THOR_DOCTOR_ALLOW}}
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

The Stop hook already asks for a verdict on the single most-served unjudged
item, once per turn. That is the same debt this routine pays; here you
settle it deliberately, over the whole session, instead of one item at a
time.

In every project a session actually works in, once that session has put in
at least an hour there, the Stop hook requires this evaluation before a turn
can end - not once per session: it blocks the FIRST Stop of EVERY turn,
regardless of how many notes currently owe a verdict, for as long as no
evaluation report filed for this project still covers it (within the last
16 hours). It names how many times it has already asked, and since when.
Filing the report below is what goes quiet - but not necessarily for the
full 16 hours: once three more hours of accrued work go by since that
report, AND a real risk has shown up since then too - three or more code
changes with no test or build run in between, or a context summary - the
obligation returns while the report still covers, as a repeat covering only
what happened since that report (see the SCOPE note under step 2). Three
quiet hours with neither risk stays silent. It only ever asks inside a
project: a checkout that resolves to no project is never asked, since there
would be no way to file the report that silences it.

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
test, check the machine, ask what a background job actually did), or carry
it into this report's own open points with the reason you could not settle
it here. Step 1 is done once every item above has one of those two outcomes
- never left as a bare observation with nothing decided about it.

## 2. Judge what fired

Before any of that: look up the previous evaluation report for this project
(`mcp__thor__lookup` for the tag `evaluation-report` and this project; take
the newest one). Handle its open points first - what it left unjudged with a
reason, the gaps it left open, and everything it listed as not verified or
still to be probed. For each: settle it now, or carry it into this report's
own open points with the reason it is still open. No previous report? Say so
and move on.

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

Your list is fixed the moment this evaluation starts. It starts when you
open this routine (as a skill, or when the owner types the command), when
the Stop hook asks for it - or, if none of those happened, at your first
attempt to file the report. From that moment the list holds exactly the
items that fired in THIS session and had fired 40 times or more since their
last verdict, and it never grows: nothing that fires after the start (your
own remember and revise calls fire the rules bound to the `remember` moment
on every write) and nothing that fired only in another session is added.
The note that starts the evaluation names the list; if you missed it, the
gate names it when it refuses the report. A pinned item is never on it: the
owner already answered whether it belongs there by pinning it.

Every item on your list gets one of two answers. A verdict: useful if it
belonged where it fired, noise if it did not. Or, when you cannot honestly
judge it - "when in doubt, skip" above - an explicit skip, written as a tag
on the report itself: `not-judgeable:<id>:<reason>`, with a reason of
at least twelve characters that says why, for example
`not-judgeable:a-1f2e3d:it fired on a grep of a log I never opened`. A skip
settles that item for this report only; it stays owed store-wide, and the
next session that sees it fire is asked again. On top of your list: every
fact you recognised as noise this session, and every useful fact the Stop
hook itself never asked about. Do not re-mark something that already
carries a verdict - that would count twice.

The health check's `judgement debt` line names the debt of the whole store
for this checkout, including items that only fired in other sessions. Those
are not this report's to settle and never hold it; judge one only if you
saw it fire yourself and know where. Step 2 is done once every item on your
list carries a verdict or a skip tag with a reason.

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

A fact about something irreversible or costly that carries no check only
informs: it sits there while the mistake happens and says nothing. Walk the
facts that fired this session and are heavy (`irreversible` or `costly`),
and decide for each. `get` shows you `severity`; a fact with no `check`
field can stop nothing. Note the `teeth` line from the health check before
you start this step and again once you finish it - the report names both.

Start with this report's batch: at most eight heavy facts that fired since
the previous report without a real tooth or a real reason, the ones written
longest ago first. You do not pick them: the health check names them on its
`evaluation batch` line, each on its own `batch: <id>` line, and the ask for
this evaluation names the same ones. The report cannot be filed until each
one is settled; until then the gate refuses it and counts what is left.
Settled means what the gate counts, and nothing else: a check that bit
reality (the real hook refused a call under the check the rule carries now -
that counts by itself, even while the label still reads wired); a rule that
hangs a check carried by an open point that names its id (see below for what
an open point is); or, only for a rule whose check cannot refuse (none,
`contains` or `path_exists`), a no-literal reason that quotes the legitimate
call or names the trigger, after a real try. A reason never settles a rule
whose check can refuse, and the gate refuses the two side by side. A label
the tool wrote itself, such as `teeth:informing`, settles nothing. The heavy facts on the `owed later:`
lines stay owed and lead the next report's batch - settle more of them when
there is time, but never fewer than the batch.

The burden of proof is reversed: for every heavy fact with no check, TRY to
give it one before anything else. Work out, in writing, which literal
fragment marks the mistake, at which file or command it belongs (a fact
hanging on a broad moment gets re-anchored to the exact command or file
first), and which check kind fits - `forbidden`, `absent`, `absent_all`,
`contains`, or `requires`. Then attach it with `revise`. Measured
2026-08-09: this is how a line about deleting a pushed remote tag went from
"fires on every `rm`, stops nothing" to "refuses exactly that form" - by
re-anchoring, not by writing a new rule.

Only after that attempt may you answer "no literal", and only with one of
two reasons written into the tag: the fragment also appears in legitimate
use of that command - QUOTE that legitimate call between quotes, for
example `no-literal:the same fragment is in 'git push origin main' which is
allowed` - or the mistake leaves no text at all, an omission or a judgement -
then NAME what the rule is about: its command, its file, or the moment it
fires at. The gate refuses a reason on a heavy fact that does neither: "it
is a judgement rule" fits every rule in the store, so it says nothing about
this one. A reason that quotes the call or names the trigger is a settled
answer; the Stop hook and the report gate stop asking about that fact.

A reason counts only after a real try, and the log is what shows the try: a
no-literal reason settles a heavy fact only when that fact's own history
carries a check at some point. So the order is not optional - but the try
never hangs live. Send the reason tag and the check you worked out in ONE
`revise`, with `trial_check: true`: the tool runs its own write-time proof on
that check, records the check and what the proof found in the fact's
history, and never stores it. If the tried check turns out to work, the
revise is refused and tells you to hang it for real instead - then that is
your answer, not the reason. Never hang a check you already expect to be
wrong just to earn a reason: measured 2026-09-27, one such check refused
every write that mentioned the file it named for about a minute. A reason on
a fact that never carried or tried a check settles nothing, however well it
is written: two such reasons slipped through on 2026-09-26, one of them
saying in so many words that the syntax was never found.

A heavy fact that already carries a no-literal tag is re-checked, not waved
through: read the reason again against the fact. An older reason that
neither quotes a call nor names the trigger does not count any more - the
debts keep naming that fact until you rewrite it. If the fact names a
command or a file, the reason is suspect and the attempt above applies
again. Say per fact whether the reason held.

A label the tool writes is never an answer. `teeth:informing` only says
nothing could be tried; `teeth:wired-not-proven` says the check reaches the
right place both ways, but was tried only with attempts built from its own
literal, which an invented literal passes just as well. A check counts as a
real tooth - `teeth:proven-guard` - only once the log shows it biting
reality: the real hook refused a real call under it, or a real call the rule
already fired on carries its literal while the rule's ordinary calls still
pass. Never run the dangerous command itself to "prove" a check: if the
check does not catch your form of it, the command runs.

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
trigger to buy a catch.

Never write the mistake into a real file to make a check bite: if the check
misses, the mistake is now in the code. A command check may be proven with a
harmless form of its command (step 8); a refusal that happened on its own
this session counts by itself. So every check here that has not bitten -
`teeth: wired, not proven`, or one you could not make work here - goes on an
open point, one per fact. An open point is a separate small Report, never a
tag on the rule (the gate refuses a tag such as `open:<id>` for exactly that
reason). File it with `remember`: kind `report`, the same project as this
report, tags `["open-point"]`, an id such as `open-<rule id>`, and one
sentence that names the rule's id and the real call that would prove it, or
what blocks the fix (a full anchor, say). That open point is what lets the
report through while the check has not met reality yet, and the next
evaluation has to close it - retract it with a reason once the check bit, or
once the check itself was retracted - or carry it forward once. A check that
has never met a real call is an assumption.

Does the gate refuse the check you are adding - does it not hold, or is the
anchor full after moving it? Correct the check until it holds. A full anchor
is solved with a route from step 6; if that cannot happen in this session,
the open point names the full anchor and its occupants - never a no-literal
reason that hides the real cause. That happens in the same turn, not as a
line in the report. Step 4 is done once every heavy fact in this report's
batch, and every other one you took on, has a decision - a check that bit
reality, a wired check carried by a named open
point, or a tried-and-justified no-literal tag - every fact that already
carried a no-literal tag has been re-checked this session, and nothing is
left hanging on a refusal. Retracting a heavy fact takes a reason that says
what changed about THAT fact; "no longer relevant" is refused.

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
  an existing place that fits, or ask the owner for a name for the new one.

Want a fact to be able to REFUSE a wrong action instead of only informing:
- forbidden text in files: `always` plus check_kind `forbidden`;
- a forbidden COMMAND: a command target plus check_kind `forbidden`, with
  the dangerous fragment as the literal. Do not give a rule like this an
  `always` binding unless those words must also never be written - otherwise
  it refuses its own documentation;
- text that must not appear in ONE file: check_kind `absent`.

All other forms only inform, and that is usually the right choice.

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
"still to be probed": the probe itself happens outside this evaluation, by
putting the forbidden fragment literally into a harmless form of the bound
command and seeing whether the gate responds. Never test with a file check
or with free prose - that proves nothing, and writing the mistake into a
file only risks it; a check on a file waits on an open point (step 4).

Step 8 is done once every untested addition from this session is on that
list, named with what should make it refuse.

---

## The report

Open with a plain-language summary: what happened, does it work, what does
it mean for the owner. No jargon, no hashes, no test names. The detail
follows below, in the order of the steps above.

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
4. the doctor's `teeth` line before and after this step; each fact that
   gained a check, with its id, the check kind, the literal, and where it
   is anchored; each fact that got or kept a no-literal reason, with the
   reason; and the gaps you left open;
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
instead; they may appear in the dated evaluation report itself (below).

Save the whole report in THOR when you are done, so a later session can find
it and hunt it for real THOR defects: `mcp__thor__remember` with kind
report, id `eval-<project>-<YYYY-MM-DD>` (a second one the same day gets
`-2`), project set to the project you evaluated, tags `evaluation-report`,
and text the full report exactly as you gave it to the owner. This is a
dated record, not a rule - the health-check numbers may appear in it, and it
never fires on its own. Add a `not-judgeable:<id>:<reason>` tag for every
item of your list you skipped (step 2). If the gate refuses the report, it
names what is still open - each item is either settled by the work it names
or honestly skipped; the list it holds you to cannot grow between attempts.
The reply to a filed report says how many items of your list you settled
and skipped, and how many stay owed store-wide. It is also what tells THOR
the evaluation happened:
the Stop hook reads it back, and filing it is what lets a blocked turn end
and stays quiet for the next 16 hours - a report filed now never buys more
than that in advance.

Do not store or change anything else unless the owner asks for it. The
exceptions are the work the steps themselves call for: the verdicts, the
revisions of rotten facts, the checks you attach to heavy facts, the
captures of what was missing, and filing the report itself.
