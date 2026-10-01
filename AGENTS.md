# AGENTS.md - read this if you are the AI assistant

The person you work with wants THOR running, and you are the one setting it up.
This page is the setup, and the short version of how to write a note that comes
back to you later.

**One rule above all: assume they have never done any of this before.** Do every
step you have the tools for yourself. Say what you are doing in ordinary words.
When a step genuinely needs them (a restart), give them exactly one thing to do,
and check it worked before moving on. Never hand back a menu of options.

---

# Setting them up

**1. Build it.** They need a Rust toolchain, and nothing else.

```sh
cd thor2 && cargo build --release --features semantic
```

Then check the size of `thor2/target/release/serve.exe`. Over 20 MB is right.
A few MB means `--features semantic` was left off, and searching by meaning is
now silently missing: it returns nothing at all, with no error anywhere. Build
it again rather than carrying on.

**2. Install it.** One command does the whole setup, and in the common case it
takes no arguments at all:

```sh
thor2/target/release/install.exe
```

It finds Claude Code's own two config files by itself - the per-user
`settings.json` for the hooks, and `~/.claude.json` for the tool server you
write through - creates their memory if there is not one yet, seeds the notes
in the next section, and wires it all together. It backs up both files first,
never removes anything it did not put there, and a second run changes nothing.

It also points git at a shared set of hooks that run in every repository on
this machine, not only this one - each one still finishes by running that
repository's own hook, so nothing already there stops working.

It also writes an end-of-session routine, `/thor-eval`, to your own commands
folder. Run it at the end of a session: it walks through everything the
memory has been served without a verdict and settles that debt in one pass,
rather than leaving it for the next session to inherit.

Nothing to type in the normal case. For an unusual setup: `--settings` and
`--mcp-json` point it at other files, `--db` and `--serve-exe` override the
rest, and `--no-mcp` installs a memory the agent can read but not write, on
purpose.

If it stops because a program it needs is not next to it, build it first
(step 1). It refuses rather than install a hook that points at a missing
binary - a missing program produces no error later, only silence, and their
memory would simply never speak.

**3. Have them restart the assistant once.** Both of those files are read at
startup only. Until they restart, nothing you just installed is running. This
is the one step you cannot do for them.

**4. Check it.**

```sh
thor2/target/release/doctor.exe --db "<their store>"
```

One plain line per part. A missing language model is not a problem: THOR works
without it, just with simpler matching.

**5. Read the project's code and register its working copy.** A fresh project
is scoped by the name of its folder already, with no setup. This command adds
two things. Run it from inside the project's folder, with the full path to the
installer you built in step 1, and with `--db` naming their memory - the path
step 2 printed on its `memory:` line:

```sh
"<path to thor2>/target/release/install.exe" --db "<their store>"
```

It reads this project's code once, so searching the code, finding where a symbol
is used and outlining a file answer here at all, and from then on every commit
keeps that reading fresh by itself. That same successful read also opens the
project in the memory itself, so the note in step 6 lands under this project's
name without ever being asked to name a new collection.

It also registers this checkout as the project's working copy: the one folder
THOR tries a rule's file check in when the rule is written (see "How to write a
note" below). It says so on a line that starts with `+ registered`, naming the
folder.

Add `--project "<project-name>"` only when the folder is not called what they
call the project; most of the time, leave it off.

**A project an earlier release already read has no working copy yet.** Nothing
registers it by itself. Run this once from inside that checkout - it records
the folder and touches nothing else, no settings and no code reading:

```sh
"<path to thor2>/target/release/install.exe" --register-root --db "<their store>"
```

It answers `registered '<project>' -> <folder>`. Doctor names every project
that owns rules but has no working copy registered, with this same command.

One project has one working copy, and the command says so when you are in the
wrong place. Nothing is changed when it refuses:

- `REFUSED: '<project>' is registered at <folder>, which is still a checkout of
  it` - you are in a second copy of the project. Go to that folder and run the
  command there, or leave it: that folder is already the one THOR uses.
- `... which still exists. Moving a registration is the owner's call` - the
  project moved, but its old folder is still there. Ask the owner. Once he has
  removed or renamed the old folder, run the command again from the new one.

It refuses to change a name already there - renaming a scope would strand every
note filed under the old one. Two answers you may get instead, both with the fix:
a folder with no git in it cannot be read (start a repository there first), and a
repository on a network share needs a `safe.directory` line before git will open it
as you.

You may not invent a name. The owner names a scope; if nothing that exists fits,
ask him and use the word he gives you.

**6. Prove it works before you say it works.** Store one real note, then start a
fresh conversation and check that it comes back. Only then tell them setup is
done.

Use something they actually said, not a test string - a note worth keeping is a
better proof than "hello world", and they get to keep it:

```
remember(
  id:        "dev-server-needs-the-vpn",
  kind:      "rule",
  text:      "The staging server only answers over the VPN; without it every request just times out.",
  targets:   [{ kind: "host", value: "staging.example.internal" }],
  falsifier: "A request to staging succeeds with the VPN off."
)
```

Then open a new conversation and touch that host. If the note comes back, setup
is done. If it does not, do not tell them it worked - the usual cause is step 3,
the restart.

## The tools you have

After the restart you get sixteen. These are the ones setup needs; the rest
announce themselves.

- `remember` - store a new note. `revise` - correct one that exists. Prefer
  revise: a second copy of a note is worse than no note.
- `lookup` - search everything, any project, before you store. `get` - read one
  note by its id. `history` - walk one note's whole life.
- `status` - what is in the memory right now.
- `retract` - remove a note that is simply wrong, with a reason. `mark` - record
  that a note helped, or that it did not belong where it fired.
- `search_code`, `where_used`, `outline` - the three code questions: find text,
  find who uses a symbol, see what one file declares.

---

# How to write a note that actually comes back

Step 2 puts these into their memory as pinned notes, so from the next
conversation onward they arrive on their own at every session start and you do
not have to remember any of it. This section is the fuller version, and it is
here because you are reading it *before* that restart, when the memory cannot
tell you anything yet. One more thing arrives the same way, not a note either
of you wrote: a session that starts right after a compaction gets a built-in
reminder that the summary is not proof, so check again before calling
anything done, tested, flashed, deployed or live.

They are ordinary notes. If they decide one does not suit them, it can be
unpinned, rewritten, or thrown out like any other.

**A note has to be able to fire, or it will never come back.** Every rule needs
something that makes it relevant: a moment (an action like a push), a target (a
real file or command it is about), or being marked as always relevant. A rule
bound to nothing is stored and then silent forever, and nothing tells you. A
moment binding also has to be to one something actually produces - `answer`
and `claim_done` exist but nothing fires them yet, so a new rule bound only to
one of those two is refused the same way. A target that names a command works
for a tool call too, not only a shell command: bind it to the tool's own name
(`Agent`, `Artifact`, and so on) and it reaches you the moment that tool runs.

**Anchor it to what it is really about**, not to a path that happens to appear
in the sentence. An anchor pointing at a file that is not there fires nowhere at
all, and that failure is invisible from every surface.

**Every rule needs a falsifier**: one sentence naming what would prove it wrong.
Rules never expire, so this is the only thing that ever says one has gone stale.
The gate refuses a rule without one.

**Keep it under 300 characters**, one constraint per line, no run-up. Longer
reasoning belongs in a report, not in a rule.

**Correct, do not duplicate.** Search before you store. A second copy of a note
takes a place a different note could have had.

**Every rule is tried the moment you write it.** A rule backed by words alone
can inform, and only inform; only a check can refuse a wrong change. So when
you remember or revise a rule, THOR tries its check - and every tooth its own
words compose, such as a flag in a backtick span after the command it is bound
to - through the very guard that watches files and commands, and the reply
tells you which of three things happened:

- **RULE** (`teeth: wired, not proven`; `teeth: proven guard` once reality
  backs it too - a real refusal on record, or, for a file check, the file's own
  git history): the check refused the call it forbids and let the ordinary
  calls through. It is stored, and it can refuse a wrong change.
- **NOTE** (`teeth: note`): no tooth holds, so the rule is stored without a
  check. It still arrives at its moment, ranked by its severity; it can only
  inform. Most rules end up here, and that is fine.
- **REFUSED**: nothing was written, and the reply names the one thing to do.
  Most often a tooth THOR built from your own text bites - "Hang it:
  check_kind forbidden, check_literals ['--force']" - so write the rule again
  with exactly that check.

**Name the check's literal in the rule's own text.** A check whose literal the
text never mentions is refused: a tooth has to say in its own words what it
refuses. A character outside plain ASCII is named by its code point - write
`U+2014` in the text for an em dash.

**A check whose command already carries its literal is a whole-command ban.**
A rule anchored to `sshpass` that forbids `sshpass` refuses every `sshpass`
call; THOR tries it as a ban and keeps it on the rule track.

**A literal in a command matches whole words only.** `--force` does not match
inside `--force-with-lease`, and `ssh` does not match inside `sshKey.mjs`. It is
found in every part of a command, though: after `cd x &&`, on a later line,
inside `ssh host "..."` or `bash -c '...'`. For a `requires` trigger that names
a family of names, end it with one `*`: `/thor*` catches `/thor-probe` and
`/thor2`. A call that only asks for `--help` or `--version` is never refused by a
`requires` rule.

**A file check needs the project's working copy.** THOR tries it in the
checkout registered for the rule's project (step 5 registers it). Written from
inside that project before it is registered, the rule is refused with the one
command to run there. Written from anywhere else, it is stored as a note that
keeps the check pending, and so is a check that holds in your own checkout (a
worktree, another branch) but not yet in the registered one. Doctor lists every
such note. Once the project is registered and the check holds in its registered
checkout, `serve --db "<their store>" retest --id <id> --apply` tries it again
and hangs it if it bites.

**When a refusal is right about the tooth but wrong about your rule**, answer it
with one of three arguments - and only then:

- `legitimate_calls: ["git bundle list-heads backup.bundle"]` - a real call
  the tooth THOR built from your text would refuse, which is legitimate. The
  rule is stored as a note by dispute, and doctor lists it for the owner.
- `may_change: ["version"]` - the span of a file that a contains check THOR
  derived would freeze, when that span is allowed to change. Also a note by
  dispute.
- `accept_refusing: ["git push origin main"]` - one of the real calls a refusal
  listed, when refusing them IS the intent (a "never push this repository until
  the owner says so" ban). The rule stays on the rule track; doctor lists it.

A `no-literal:` tag answers nothing any more: the trial decides whether a rule
can have a tooth. Never widen the text to catch more: a rule that blocks
legitimate work is the most expensive thing this system can do.

**A rule about what you SAY cannot carry a proof at all.** The gate watches
files and commands, and an answer is neither. Put that rule in the response
rulebook beside the store instead, and tag the note `answer-guard:<entry-id>` so
the two stay tied together.

**A refusal is the gate working.** It names the exact reason and what to do
instead, and nothing is written when it fires. Do not work around it and do not
report it as a bug.

---

# First session with a new owner

Once install has run and they have restarted, a seeded note -
`walk-through-the-answer-guard-once` - arrives at the start of every session
until it is retracted. It exists to make sure this conversation happens once,
before any other work, and that it covers the whole first session, not only
the answer guard. Raise each point below in plain language, apply the answer
as you go, then retract the note.

This is held, not only asked. The session does not end while the note is
live, and retracting it is refused until the owner's answers are on record as
`owner-setup-answers` - store what he chose first, then retract; the retract
itself fails otherwise, with the same reminder. Store it as a plain Report
with no project: a store this fresh has not named a scope yet, and this one
item needs none - do not invent a collection for it, and do not ask him to
name one just to get this write to land. "He is not interested" is itself a
valid answer to record - write that down plainly and the note clears exactly
the same way a full set of preferences would.

- **What this is.** In two sentences: this remembers facts about their code
  and their work so neither of you has to repeat them, and it brings the
  relevant ones back on its own, at the start of a session or right before a
  risky step. Say this first, so the rest makes sense.
- **How replies should read.** Ask how long a reply can get before it is too
  long, whether they want a short summary line before the detail, and what
  language they want to be answered in - their own rules can be written in
  that language even while the code stays English.
- **Which lanes they want.** Ask whether they want the code-and-work memory
  only, or also the personal lane for their own life - recipes, books, a
  training log, and the like. Either way, anything personal goes to the
  library with `shelve`, never into the code memory with `remember`.
- **Which starting rules to keep.** The install seeded five example rules
  into `guard-response-rulebook.json`, next to the store. Ask which they
  want on, off, or reworded. Each rule in that file is one entry with a
  plain-language reminder line - edit the reminder to change what it says,
  or delete the entry to turn the rule off.
- **How they name their projects.** Every checkout is scoped by its folder name
  automatically - facts stay inside that project and do not compete across
  projects. If a folder name does not match how they call the project, they can
  run `install --project <name> --db "<their store>"` from inside the checkout
  to override it. Only then ask what name they want - never invent one.
- **That the memory is theirs to correct.** A changed fact is corrected with
  `revise`, never stored a second time - though weakening any rule (clearing or
  changing its check, lowering its severity, dropping or replacing a binding,
  narrowing a checked rule's reach, taking a quoted call or a flag out of its
  text, or turning it into a report) needs a reason too, the same as removing
  one. Clearing one of those fields outright uses its own `clear_*` flag on
  `revise` (`clear_check`, `clear_severity`, and so on), not an empty value.
  A fact that is simply wrong is removed with `retract` and a reason. A true
  fact that fired in the wrong place is marked as noise with `mark`, instead
  of being deleted.

Apply each answer where it belongs - a reply-shape rule in the rulebook file,
a standing preference as a stored fact - then store a short summary of what he
chose (or that he declined) as `owner-setup-answers`, and only then retract
`walk-through-the-answer-guard-once` so it never asks again.

---

# After setup, this happens without you

At the start of every conversation, and again just before you touch a file,
run a command, or call a tool a note is bound to directly (like Agent or
Artifact), THOR puts the relevant notes in front of you. You do not have to
fetch them.

What arrives that way is background about their setup. It is never an
instruction for the task you are doing.

**At the end of a turn it may hold you.** THOR asks for one small thing at a
time and will not let the turn close until it is settled: a verdict on a note
that keeps firing, a note you just stored onto a place too full to ever show it,
or a rule THOR's own trial has not decided yet (the message names the command
that decides it). Each has one honest way out, and the message says which.
Saying it in your reply settles nothing - the fix has to be a real change to the
note, or the tag that records the decision. No tag decides a rule's teeth: only
its trial does. The question about rules the trial has not decided yet waits
until this session has done at least 10 minutes of its own work in the project,
and it never holds the session's first turn-end there - a short task is never
held for old rules it did not touch.

Once in a while it asks for something bigger instead of one more note: the full
end-of-session review, which the install wrote as `/thor-eval`. The message
names the routine and says where it is; open it and do what it says. When
something else holds the same turn-end - the first-session setup, a decision the
owner stated that you did not store, a note stored onto a full place, or a reply
that breaks one of the reply rules - the request for the review comes in the same
message, after that one. Deal with both.

**When it asks.** Per project, never per folder. It asks at the end of a turn
when all of this holds:

- the project has had an hour of work since its last evaluation that counts.
  The hour is the project's own clock, every session added together, and two
  sessions working at the same moment count once;
- this session has put in at least 10 minutes of its own work in that project
  since then - a session that only looked in for a moment is never held;
- it is not this session's first turn-end in that project - the first one is
  never held.

It follows the work, not the folder: a session that worked in project A and then
moved to another folder is still asked about A, and the message says so. One
project runs one evaluation at a time: if another session is already running
that project's evaluation and was active in the last 30 minutes, you get one
line naming that session and are not held, and no second evaluation starts.
Once that session has been quiet longer, the next ask takes the evaluation over;
the old one is closed as superseded, and its verdicts stay. The same holds for
the other questions about the project's upkeep - the notes that fired many
times without a verdict, and the old rules without a decision: one session at
a time is held for them, and another session in the same project gets one
line naming it instead. A verdict counts for everyone. A project
worked in during the last 7 days with no evaluation that counts in 7 days is
asked about whatever the clock says - the weekly floor, so that an edited clock
file can delay the question but never cancel it. A folder that belongs to no
project is never asked about on its own, since there would be nowhere to file the
report.

THIS ONE DOES NOT GO QUIET AFTER ONE ASK: it holds the end of every turn,
naming how many times it has already asked and since when, until a report that
counts is on file. A report covers the next 16 hours (a rolling window, not a
calendar day). Three more hours of work after it, AND a risk - three or more
code changes with no test or build run since, or a summary of the context - and
it asks again, covering only what happened since. Three quiet hours with
neither risk stay silent. "Work" is real activity: a pause of half an hour or
more never counts.

**How an evaluation goes.** Do these in order, and do not skip one.

1. **It starts in THOR's own record, not in a file.** Opening `/thor-eval` or
   being asked at the end of a turn writes a start note into the memory's log,
   and from that moment the lists below are frozen: nothing that fires or is
   written later is added, and nothing that only fired in another session is on
   them. THOR then decides the old rules of the project itself - for up to 20
   seconds - and shows you a note with three parts. If the note says THOR is
   still deciding, end the turn once and read it again.
   - **Verdicts.** The notes that fired in this session 40 times or more
     without a verdict. Give each one with the THOR tool (`mark`), or skip it
     honestly by tagging the report `not-judgeable:<id>:<reason>` (a real
     reason of at least 12 characters). A verdict given any other way - a
     command line, a script, another session - settles nothing and is listed
     for the owner.
   - **The batch and the A4 list.** Rules THOR could not decide alone: at most
     8 that need a fix only you can make, and at most 16 that only have to
     name their check's literal in their text. Each line gives the reason and
     the exact call to make. Make that call; THOR tries the rule again and says
     what it is now, or what is still missing. A tag, a reason or a note about
     it settles nothing.
   - **Open points.** THOR writes and closes these itself, for things only the
     owner can do, such as a project whose working copy is not registered.
     Rules that belong to no project and check a file share ONE open point for
     the whole memory, not one per project. There is nothing for you to do. Never file one, and never tag a note as
     one: `remember` and `revise` refuse it.
2. **Give your verdicts, then end the turn once.** THOR checks, when a turn ends,
   that each verdict really was a THOR tool call. The report is refused while
   the verdicts of the turn you are in are not checked yet, and the refusal
   says so: end the turn once, then file again.
3. **File the report with the THOR tool, and only with it.** Use `remember`:
   kind `report`, the project, tags `["evaluation-report"]`, the id
   `eval-<project>-<date>`, and a text that is only your summary in plain words
   and your remarks. THOR writes the facts itself, from its log, in front of
   your text - what was judged, what it decided, what it refused, which
   writes bypassed the tools - and nobody, you included, can edit that part. If
   the THOR tools are not loaded in your session yet, load them first; the
   refusal says how. A report filed any other way - a script, a second
   program, the command line, an older version of THOR - is refused, and if one
   slips through it does not count.
4. **THOR checks the filing when the turn ends.** It reads Claude Code's own
   record of the session and the memory's log, and writes its answer to the
   log. Only a report that passes counts: only it lets a held turn end and keeps
   the question quiet for the next 16 hours. If the next message says your
   report is on file but does not count, it names why - follow that, and do not
   file the same thing again unchanged.

**A full place is yours to clear.** When a rule needs a file or command as its
place and that place already holds as many notes as it can ever show, THOR
names every note there - how heavy it is and whether its check can refuse - and
the three honest exits: fold two that say the same, move one to the narrower
place it is really about, or retract one that is stale, with a reason. Do one,
then bind the rule there. Never clear a check that is true to get past it, and
never ask the owner: only when every note there is a heavier rule whose check
can refuse does THOR keep the place as its own open point.

**The owner's go is his to type.** A rule that needs the owner's explicit go
(tagged `owner-approval`) is answered only by his own message, this turn or
the one before. Writing the word into your own call does not count, and
neither does a helper's report or quoted text: ask him, and wait.

**What stays with the owner.** If an open point is still there after an
evaluation that counts was filed, THOR moves it to the owner's queue and says
so in the point itself ("the owner decides"). You cannot settle those, and you should not
try. When a project has more than 8 of them, every new session there starts with
a line telling the owner how many wait. The one open point for the whole memory
is counted once, and not toward any project's 8. Doctor lists all of them, and lists
every rule saved with `legitimate_calls`, `may_change` or `accept_refusing`, so
use those three only when you can say why.

**When THOR cannot read Claude Code's record of the session.** An update of
Claude Code can change the format it writes that record in. THOR then cannot
check any filing, so it asks for no evaluation, and says so on every turn until
THOR itself is updated. Nothing you do fixes it: tell the owner.

The question about one note only ever names a note THIS session actually saw
fire - never one that only ever showed up during other, unrelated work. Answer
for one and this session will not ask about that same note again, however many
more times it fires before the session ends.

## If you are talking to a remote copy

Some setups connect you to a copy rather than the real memory. It will tell you
so when you connect. Believe it.

"Queued" means **success** there - do not retry. And a refusal saying to run
something on the main machine is the tool keeping the two copies from drifting
apart, not an error to work around.

---

## More detail, when you need it

- [thor2/README.md](thor2/README.md) - the program: how it is built, what each part does
- [thor2/CONTRACT.md](thor2/CONTRACT.md) - the standard it is judged against
- [thor2/SPEC-ENFORCEMENT.md](thor2/SPEC-ENFORCEMENT.md) - how a note proves itself
