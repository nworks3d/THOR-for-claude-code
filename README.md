![THOR for Claude Code](assets/banner.png)

# THOR - a memory for your AI coding assistant

**Most memory tools can only remind your assistant. THOR can refuse.**

Give it a note with a proof attached - a small check it can run right now,
like "this file still contains that line." From then on the note stops being
advice. THOR refuses the matching edit or command before it runs. Not a
suggestion your assistant is free to skip. A hard stop.

That proof runs live, at the exact moment it matters, against your actual
project. A note that has gone stale, that no longer describes what is really
there, quietly stops counting as proof - so it can never block you by
mistake.

Other local memories can refuse a write too. What is different is where
the block comes from: a proof, checked live, never a rule taken on faith.
And the whole loop that produces it, learning included, runs in one place:
this machine.

Most notes only inform: pinned so they are handed over at the start of
every session, or surfaced the first time your assistant reaches for the
file or command they are about.

A note that keeps being served without ever being judged is asked about in
one block, not one note at a time: the end-of-session review (`/thor-eval`),
which THOR asks for about once a week per project. THOR does not stop an
ordinary turn to ask whether one note helped or got in the way. The review
only names a note your own session actually saw fire - never one that only
ever showed up during other, unrelated work. Two "this did not belong"
verdicts since the note was last rewritten stop a plain note from being served
again, though it stays there if you go looking for it. A rewrite of the note's
text, check or bindings starts that count again, and a note whose check can
refuse is never hidden this way.

A health check names what has rotted, out loud: proofs that now come out
false, notes pointing at nothing, notes too crowded to ever be shown.

Everything that is not code - a recipe, a book, a training log - lives in
its own library, kept apart, so it never competes with a deploy rule for
room.

Everything stays on your machine by default: no account, no key, no cloud -
including what it learns about which of your notes are worth keeping. The only
two things that can leave it are listed under "Stays on your machine" below.

**Get started.** Send this repository's link to your AI assistant (Claude Code)
and say "install THOR". It does the whole setup itself; the only thing it asks
of you is to close Claude Code and open it again, once.
The full step-by-step is under [Getting started](#getting-started) further
down. If you are the assistant, read [AGENTS.md](AGENTS.md).

---

THOR is free and GPLv3. If it earns its keep, you can support the work:
**[buy me a Ko-fi](https://ko-fi.com/noizieworks)** or
**[become a YouTube member](https://www.youtube.com/channel/UCrEZc_oJR9mywNjqY115mRg/join)** -
no pressure, no paywall, everything stays open either way.

> **Are you an AI assistant, setting THOR up for the person you work with?** Go
> straight to **[AGENTS.md](AGENTS.md)**. It is written for you.

[![Watch: THOR explained on YouTube](https://img.youtube.com/vi/tCQOUdkhErQ/maxresdefault.jpg)](https://youtu.be/tCQOUdkhErQ)

*Prefer watching? The video above walks through what THOR is and why it exists.*

## What it does

**Remembers what you tell it.** A rule, a gotcha, a decision, the shape of the
project. Once, in your own words. It stays until you change it.

**Hands it back at the right moment.** Not a search box you have to remember.
The note arrives while you are touching the file or running the command it is
about, in a conversation that never heard it.

**Stops a wrong change, not just warns about it.** A note carrying something
checkable can refuse the write outright. Most notes only inform, and that is
deliberate: a rule that blocks honest work is the most expensive thing this
system can do.

**Keeps projects apart.** Every project has its own memory. One repo's rules
never leak into another.

**Says when it has rotted.** It counts its own dead ends: notes pointing at
files that moved, notes nothing ever reads, notes crowded out by louder ones.
Out loud, in plain language, so you can fix them.

**Stays on your machine.** No account, no key, no server. THOR never sends
anything on its own. Exactly two things can leave your machine, and only
because you say so: a bug report about THOR itself, sent to its GitHub page only
after your explicit yes to the exact text shown (file paths, project and
computer names, addresses, keys and passwords are removed first), and a backup
of your memory to a place you name, if you choose one - a backup kept on this
machine sends nothing. A short record of recent commands and replies, kept
beside the memory so THOR can test its own rules, never leaves the machine and
can be switched off (`install --set-recording off`).

## What that looks like in practice

Months ago you found out the hard way that this project is pinned to an older
Node, and anything newer breaks the build. You said so once, and moved on.

Today a fresh conversation opens `package.json` to add a dependency, sees an
engine range that looks out of date, and is one helpful edit away from bumping
it. Right then, before it types, your own sentence is in front of it.

That is the whole idea. Not a search box you remember to use. A memory that
shows up on time.

### "I already have a CLAUDE.md for that"

Most assistants read a rules file at startup - `CLAUDE.md`, `AGENTS.md`,
`.cursorrules`. It helps, and it runs out of road quickly.

Past a certain size it becomes a phone book, and nobody reads a phone book front
to back. Your assistant skims it, takes the gist and moves on. The rule was in
there. It got skipped. Nothing looks wrong afterwards, because the line is still
sitting in the file, so you go on believing you are covered.

Here is the part that is genuinely different. Picture a fresh agent, no history,
no idea what this project has already cost you, one keystroke away from the
exact write that broke production last spring. A rules file would have mentioned
it somewhere on page four. THOR stops the keystroke. The write does not happen -
and what stops it is the note *you* wrote, the day it broke.

That is the whole promise: not better advice, but a wrong change that does not
land.

Getting there is not free, and it is worth knowing before you start. A note only
earns that power if it is written to earn it: tied to a real file or command,
carrying something checkable that shows it still applies. THOR ships with a
set of starting notes that teach exactly that, and refuses the ones that
cannot work. [AGENTS.md](AGENTS.md) spells out the rules of the game in full.

Three things make that work, and they all live in one file on your machine:

**1. Nothing is ever lost.** Every note is kept forever. Change your mind and
the old version stays too, so you can always look back at what you used to
think and when it changed. If two versions of a note ever conflict, THOR keeps
both and tells you, rather than quietly picking one and throwing the other away.
It is the same care you would give your source code, given to the things you
know.

**2. It arrives at the right moment.** THOR checks your memory on every message
you send. And the first time your assistant reaches for a file or runs a command
that one of your rules is about, that rule gets put in front of it right then.
Before the mistake, not after.

**3. Your assistant looks after it.** It is not a notebook you have to fill in
by hand. Your assistant can add notes, correct them, retire ones that stopped
being true, and flag which ones actually helped. A THOR that is used well is a
THOR your assistant is quietly tidying as you work.

It reads your code too. Point it at a project and it takes in the source and the
documentation, so "how does this bit work here" gets answered from your actual
project instead of a guess.

Everything stays on your machine by default. No cloud, no account, no
subscription, and nothing to sign up for. If some optional piece is missing, THOR quietly falls
back to a simpler way of working instead of breaking.

## A note that can actually stop you

Showing a warning at the right moment is worth a lot, and for a long time that
was all THOR could do. A note could speak. It could not refuse.

Version 2 lets a note carry a **proof of its own currency**: a small check THOR
can run right now to see whether the note is still true of your project. "This
file still contains that line." "That file is still there." "This character
never appears in anything we write." Or, for catching something left out
rather than something wrong: "every agent I spawn names which model to do the
work with."

That changes what a note is allowed to do:

- A note whose proof **runs and holds right now** may stop a wrong change
  outright.
- A note backed by words alone may warn, and only warn. It can inform your
  assistant; it can never forbid.
- If the proof **cannot run** - the file moved, the path is gone - nothing is
  blocked. It is reported as needing a look.

That first kind of stop reaches further than an edit made through your
assistant's own tools: deleting the protected file, emptying it out, or
overwriting it from a command your assistant runs counts as the same wrong
change, and is stopped the same way.

The reason for the split is uncomfortable and worth saying out loud. Notes rot.
You write one, the project moves on, and the note quietly becomes wrong. A tool
that let any old note block your work would spend most of its time blocking you
for reasons that stopped being true months ago. So THOR only hands that power to
notes that can prove, at that exact second, that they still describe your
project.

Which is why, as the top of this page already said, most of your notes will
never block anything. The health check prints how many can, and you should look
at it. On the author's own memory, when this was first measured, 2 notes out of
2999 could prove themselves; a day of deliberate work took that to 256. It moves
by hand, because deciding what proves a note is a judgement about that one note.

That the number is printed at all is the point: a safety net nothing is attached
to looks exactly like a safety net that works.

### THOR asks, so you do not have to remember to

Leaving that to whoever thinks of it means it never happens. So THOR does it
by itself, in two places.

**When a note is written.** You are not asked whether the note can be checked:
THOR tries it. A rule that spells out a command, a flag or a filename is run
through the same guard that watches your files and commands. If a check built
on its own words really refuses the wrong change and lets ordinary changes
through, the note is stored as a rule that can refuse. If no check holds, it is
stored as a plain note: it still comes back at the right moment, it just cannot
refuse anything. A note that is turned down says what to write instead.

**For the notes you already had.** THOR sorts the older notes itself at the
start of each review (below). Only what needs your assistant's own judgement is
handed over, in one block, so a memory written before any of this existed still
gets worked through instead of being declared hopeless.

A caution worth stating plainly: THOR can prove that a note is *wired* so a
matching change would be stopped. It cannot know whether the text you typed is
the text the real command uses. A misspelled fragment is wired perfectly and
guards nothing. That is why the health check reports two different numbers - how
many notes could refuse something, and how many ever actually did. Trust the
second one.

The review is the broom: run `/thor-eval` at the end of a session. It walks
through everything the memory has been served without a verdict, repairs what
has quietly drifted out of date, and settles the debt in one pass instead of
one note at a time. A session that never runs it simply leaves that debt for
the next one to inherit.

You do not have to remember to run it. Once a project has had an hour of work
since its last review, and your assistant has put in at least ten minutes of
its own work there, THOR holds the end of the assistant's turn until a review
is filed and checked. THOR sorts your older notes itself at the start of the
review; your assistant only handles what needs its own judgement, and the part
of the review report that says what was judged and what was refused is written
by THOR from its own record, so it cannot be made up. A review that was filed
any other way than through THOR's own tool does not count, and THOR says why.

**How often the review comes depends on your plan.** The review draws on the
same usage limits as your own work, so THOR keeps it to one a week per project.
On a smaller plan (Pro and anything below Max) that is strict: once a week, and
never daily, whatever it finds. On Max it is once a week too, but it comes back
daily while something is found wrong (a defect, a rule that would refuse real
work, a check left undone, a review that did not count, a bug in THOR itself),
and goes back to weekly the first time a review finds nothing. THOR reads your
plan from Claude Code's own settings file when it installs and asks you to
confirm it in the first-session questions; `install --set-plan max` or
`install --set-plan small` records your answer. The review's request says in
one line which of the two applies.

## A memory with a spine

Version 1 remembered well and never argued. It would hand your assistant a note
at the right moment and hope. Version 2 is the same memory with a spine.

- **A note can refuse.** The headline, and the rest of this section is about it.
  Version 1 could only speak.
- **Bad notes no longer get in.** A note that cannot ever fire, has nothing that
  would prove it wrong, runs on for a page, or simply repeats one you already
  have: turned away at the door, with the reason and the fix. Version 1 stored
  whatever it was given, and you found out months later that half of it was
  unreachable.
- **It counts what it actually did.** How many notes can refuse something, how
  many ever have, how much of your memory nothing re-reads. Version 1 had no
  number for the one thing it was built to do, which is how a safety net stays
  broken for a year.
- **Maintenance is no longer optional, and it does not nag.** Judging the notes
  that keep firing and cleaning up old rules is asked once, inside the project's
  evaluation (once a week per project), never at the end of every turn. A turn
  is held only for something the session did itself: a note it just filed where
  it will never be seen, a setup left undone, a decision you stated that was not
  stored. It will not be waved off with a promise to do it later.
- **A copy of the memory can be kept somewhere else, and you choose.** A backup
  is a choice you make at the first session; the install does not switch one on
  for you, so until you choose, nothing is backed up. Once you choose one, THOR
  writes the whole memory into a private git repository about once a day.
  `install --set-backup local` keeps that repository beside the memory on this
  machine and sends nothing anywhere (it protects against a damaged memory
  file, not against the disk dying). `install --set-backup <folder or address>`
  also adds the place you named (a folder on a NAS or another computer you own,
  or a private repository) as a remote called `nas`, and the copy goes there and
  nowhere else. `install --set-backup off` stops it. `doctor` says where the
  last copy went and how long ago, or that it has been failing and why. Getting
  it back is one command.
- **It costs almost nothing to carry.** A note is capped at 300 characters and a
  block at four notes and 1200 characters, so what lands in the conversation is
  a few hundred tokens, not a whole rules file re-read on every turn. A memory
  of ten thousand notes costs the same per turn as one of fifty. Nothing calls
  out to a model to decide what to send - it is a local program reading a local
  file, so there is no network round trip in front of your keystroke. The one
  exception is the pinned notes: those are read out in full at the start of a
  conversation (a new memory starts with 22 of them, and that block is capped at
  about 9,500 bytes), so pin sparingly and the cap does the rest.
- **Replies are checked too.** A new memory comes with a small file of six reply
  rules, kept beside it: open a result-like reply with a plain summary, do not
  ask you to check what the assistant can check itself, no reflexive
  disclaimers, keep an answer under about 1000 characters, show evidence when
  saying something was checked, and never say everything was checked without
  saying what was covered. A reply that breaks one is sent back once to be
  rewritten. A summary or overview you ask for may run longer, and the
  first-session list is exempt from the length rule. Edit the file, delete one
  entry, or delete the file to change or switch them off.
- **Stale notes are hunted, not left to rot.** A note whose proof comes out
  false is reported instead of quietly going on being wrong. A note that keeps
  firing without anyone ever saying whether it belonged gets asked about, and
  two verdicts of "this did not belong" since it was last rewritten stop a plain
  note from being shown while leaving it findable (a rewrite of its text, check
  or bindings starts that count again, and a rule whose check can refuse is never
  hidden this way). In version 1 a note that went wrong simply stayed.
- **Crowding is visible and refused.** Only a few notes fit in a block, so notes
  compete. Version 2 counts that competition, tells you when you have just
  stored something onto a spot too crowded to ever show it, names what is
  holding the place, and refuses the write outright when every spot the note
  could take is already full of heavier ones. A note tied to one file, folder
  or command is refused the moment that single place already holds as many as
  it can ever show - lighter rivals count too, so a heavier note can no longer
  bump a lighter one out of sight unnoticed. Version 1 accepted it and said
  nothing, which is how a memory fills up with advice nobody will ever see.
  Making room is the assistant's own job: the refusal names every note on the
  place, how heavy it is and whether it can refuse anything, and the three
  honest ways out - merge two that say the same, move one to the narrower file
  or command it is really about, or remove one that is out of date with a
  reason. It never becomes a question for you, and a true check is never
  thrown away to get past it.
- **One assistant at a time owes a project its upkeep.** When two
  conversations work in the same project, only one of them is stopped to judge
  notes, fix rules or write the project's evaluation; the other gets one line
  saying which conversation is doing it. If that one goes quiet for half an
  hour, the next one asked takes over.
- **Your go cannot be typed by the assistant.** A rule can say a command needs
  your explicit go (for example sending an e-mail, or touching a live
  server). Then only your own message counts - what you typed yourself, in
  this turn or the one before. The assistant writing the word into its own
  command, a helper's report, or text you pasted as a quote does not count.
  If THOR cannot read the conversation to check, it refuses rather than
  guess.

## A second memory, for everything that is not code

The memory above is built for work. It has a gate, notes that interrupt you,
and a hard cap on how much ever reaches the conversation - all of which is
exactly wrong for a recipe.

So 2.1 adds a library, and it is a genuinely separate thing: its own file, its
own two commands, and no way to reach the first memory at all. Nothing you put
in it can ever interrupt you, compete with a note, or take up room in a block.
You only ever see it because you asked.

It works the way a shelf works.

- **Everything lives on a shelf**, and shelves do not nest. Books, recipes, a
  training log, what you spent. Filing something without naming a shelf is
  refused, and the refusal lists the shelves you have, so your assistant picks
  from real ones instead of inventing a name.
- **Only you create a shelf.** If nothing fits, your assistant has to ask you
  what the new one should be called - and once you answer, it repeats your own
  word back and the shelf is created, with the entry filed onto it, in that
  same reply. It can never invent a name or skip the question. This is the
  rule that stops a tidy list of eight from becoming a sprawl of sixty.
- **A shelf that grows gets labels, never a split.** Two hundred recipes on one
  shelf, filtered by "bbq" or "dessert", stays one shelf. That is what keeps the
  list of shelves short enough to hold in your head.
- **You get an index, not a wall of text.** Open a shelf and you see one line
  per entry. Ask for one by number to read it whole.
- **The same thing twice is refused**, pointing at the entry you already have.
- **Nothing is ever deleted.** Your assistant can retire an entry that no
  longer belongs - it comes out of the listing and out of search, and stays
  fully readable by its own number for as long as the library exists.
- **A search never answers "nothing".** If your words miss - and they will, since
  the words you ask with are rarely the words you wrote - it hands you the shelf
  to read instead. Asking for "ribbetjes" when you wrote "ribben" finds it.

## Getting started

**The shortest way - let your assistant do it.** Send your assistant the link to
this repository and ask it to "install THOR". It reads [AGENTS.md](AGENTS.md),
which is written for it, runs the one command below for you, checks that it
worked, and asks you for exactly one thing: restart it once. Nothing to
download, no paths to type, no Rust to install, no questions to answer - the
first conversation after the restart finishes the setup on its own, with
defaults you can change just by saying so.

That first conversation also shows you a numbered list of everything the install
arranged, one line each. Everything on it is already on, except two things that
stay off until you choose: helper-agent mode (your main assistant judges while
cheaper models do the work in helpers; `install --set-helpers on` switches it
on) and backups (nothing is backed up until you pick `local` or a place). The
changelog item creates a `CHANGELOG.md` in your project only if it has none and
you do not say no. Answer by number ("1 yes, 3 no, 10 local"), or say
"defaults"; anything you do not mention keeps its default.

### The easy way: five steps, no programming tools

You need Claude Code, installed and opened at least once. Nothing else to
prepare: no account, no key. The paste-one-line route below needs neither git nor
Python. When you let your assistant install THOR instead, it checks your computer
for git and Python (which its own tools use), installs what is missing through your
system's package manager, and asks you for at most one permission click.

1. **Open a terminal.** On Windows press the Windows key, type `PowerShell`
   and press Enter. On Linux open your Terminal.
2. **Paste ONE line and press Enter.** It downloads the latest release from this
   project's GitHub page, checks the download against the checksum published next
   to it, unpacks it into a `thor2` folder in your home folder, and sets
   everything up. It prints what it did, in plain words. It needs no
   administrator rights and installs nothing outside your own user folder.

   Windows, in PowerShell:

   ```powershell
   irm https://raw.githubusercontent.com/nworks3d/THOR-for-claude-code/main/install.ps1 | iex
   ```

   Linux:

   ```sh
   curl -fsSL https://raw.githubusercontent.com/nworks3d/THOR-for-claude-code/main/install.sh | sh
   ```

   (On Windows, if a THOR program refuses to start, install Microsoft's "Visual
   C++ Redistributable" and paste the line again. Pasting it again is always
   safe.)
3. **Close Claude Code completely and open it again.** Claude Code only learns
   about THOR when it starts, so until you do this nothing is active.
4. **Make a folder for your topic, and open Claude Code in it.** For example a
   folder called `Kitchen renovation` in your Documents, with your notes, quotes
   and PDFs inside. It does not have to be a programming project. Then tell your
   assistant: *"Give this folder its own THOR memory, as a topic."*
5. **Just work.** Tell your assistant things once ("the tiles come from
   Tilehouse", "we chose the oak worktop") and in any later conversation in that
   folder it already knows. Each topic folder has its own memory, so nothing from
   one topic leaks into another. To check that all is well, ask your assistant:
   *"Is THOR working?"* The first lines of its answer are a plain summary.

The setup touches two of Claude Code's own files - its settings and the list of
tools it may use - and backs up both before it does. Rather read the script
before you run it? Open `install.ps1` or `install.sh` from this repository in a
browser first. There is no macOS build yet, so on a Mac take the route below.
Using a different assistant? See
[Using it from another assistant](#using-it-from-another-assistant) below.

### Troubleshooting: "THOR program missing" (an antivirus took a file)

Antivirus programs sometimes remove a small unsigned program such as THOR's
`sync.exe` by mistake (a "false positive"; Windows Defender did exactly this on
2026-10-11). THOR then tells you, at the start of a conversation and in
`doctor`, in a line that begins `PROBLEM: THOR program missing` and names the
file. Nothing else breaks silently any more: the hourly ship records its
failures, and `doctor` says whether it never ran, ran and failed, or the
program is gone. To get it back, step by step:

1. Open **Windows Security** (press the Windows key, type `Windows Security`).
2. Choose **Virus & threat protection**, then **Protection history**.
3. Look for an entry that names the THOR file, open it, and choose
   **Actions > Restore**.
4. No such entry? Run THOR's install again (the same command as in the easy way
   above): it puts the programs back.

If it keeps happening, your assistant may **offer** to add a Windows Defender
exclusion for THOR's program folder. It only does so after you say yes. What
that does: Defender stops scanning the files in that folder, so a bad file
placed there would not be caught. Windows shows one administrator prompt.
Saying no is fine, and THOR never changes a Windows security setting by
itself. You can remove the exclusion any time under Windows Security > Virus &
threat protection > Manage settings > Exclusions.

### For developers: build it yourself

You need a Rust toolchain. Nothing else: no key to get, no model to download
first, no account.

```sh
cd thor2 && cargo build --release --features semantic
```

> **`--features semantic` is not optional, and leaving it off fails silently.**
> Without it everything still builds, still runs, and still answers every
> word-for-word search correctly. What stops working is searching by meaning: it
> returns nothing at all, with no error anywhere. The reliable way to tell the
> two apart is size. Look at `thor2/target/release/serve.exe` - over 20 MB is
> the right build, a few MB is the wrong one. Build it again with the flag.

Then run the setup yourself. That is the same step the one-command install ends
with, and in the normal case you type no paths at all:

```sh
thor2/target/release/install.exe
```

It finds your assistant's own two configuration files by itself, creates your
memory if you do not have one yet, wires THOR into your assistant, and registers
the part your assistant writes through. It also installs a shared set of git
hooks that run in every repository on this machine (unless another tool already
set git's global hooks folder, in which case THOR leaves that alone and says so),
which keeps the reading of your code fresh after each commit, and each one hands
control back to your repository's own hooks so nothing already there stops
working. It backs up both
files before it touches
them, it never removes anything it did not put there, and running it twice
changes nothing the second time.

A brand new memory does not arrive empty. It gets 22 short pinned notes - how to
write a note that comes back to you later, plus honesty and hygiene habits - and
your assistant is handed them at the start of every conversation from then on. That matters more than it
sounds: an assistant with nothing in front of it writes notes in a shape that
never fires again, and neither of you would notice for weeks. They are ordinary
notes - unpin one, rewrite it in your own words, or throw it out. An existing
memory is never seeded, so upgrading never pushes anything into your own notes.

It also gets a starter set of general work rules for coding with an assistant,
the habits the author learnt the hard way: a changelog line with every
user-visible change (when the project keeps one), run the tests and quote the
real result before saying "done", never commit a secret, no force-push of a
shared branch, check where you push, scan before anything goes public, rehearse
a destructive step on a copy, and a few more (13 in all). Each one arrives right
before the step it is about - the commit, the push, the delete - and
they only inform, so a rule written for someone else's project cannot block
yours. The one exception is on Windows: two of them refuse a call that would
flash a console window (a `Start-Process` that is not hidden, and a scheduled
task made with `/it`). Ask your assistant to remove any you
do not want. The two notes that carry helper-agent mode are not among them: that
mode is off in a new memory.

You only reach for a flag if your setup is unusual: `--settings` and `--mcp-json`
send it at other files, `--db` and `--serve-exe` override where it looks, and
`--no-mcp` sets up a memory your assistant can read but not write, on purpose.
If a program it needs is missing, it stops and says so rather than installing
something that would sit silent.

**Then restart your assistant.** The part it writes through only comes alive
after a restart. Until then, it can already read the memory but not add to it.

**Step 3 - check it.**

```sh
thor2/target/release/doctor.exe --db "C:\Users\you\AppData\Local\thor2\thor.db"
```

One plain-language line per part: which build you are running,
whether your memory is healthy, whether searching by meaning is switched on,
how many of your notes can prove themselves, how many point at files that are
no longer there, and how many are bound to something that can never happen. It
changes nothing.

**To see just the build number** - useful when you are reporting a problem and
need to say exactly which version you have - run `doctor` as above and read its
first line, or run any of the programs in `thor2/target/release/` with
`--version`, for example `thor2/target/release/serve.exe --version`.

One of those lines only speaks up when it has something to report: if it ever
says your memory's own log file has outgrown the memory itself, something is
stopping a save from ever finishing - close any other program that might have
that same memory open, and run the check again. Newer builds also cap how big
that log file is allowed to grow, so this should be rare.

**Step 4 - give each project its own memory.** From that project's folder, with
the full path to the installer and the same memory as in step 3:

```sh
"<path to thor2>/target/release/install.exe" --db "C:\Users\you\AppData\Local\thor2\thor.db"
```

This matters more than it sounds. Skip it and THOR gets *worse* the more you use
it, because every search starts competing with projects you were not asking
about. It files the project under the name of its folder - add `--project
"my-project"` for another name, which it keeps in a small file called
`.thor-project` - reads the project's code, and registers this folder as the
project's working copy: the one place THOR tries a rule's check on a file. It
refuses to change a name that is already there, because renaming a project's
scope would strand every note already filed under the old one.

**A folder that is not a code repository** - a topic with notes and PDFs, no git
anywhere - works the same way. Run the installer from inside it with `--topic`
added. The folder's own name becomes the project's name, one small hidden file
(`.thor-project`) is added to the folder, and nothing else in it is touched.
There is no code to read, so searching inside files is not available there; your
assistant reads the files itself and THOR remembers what you told it. Your home
folder and a whole drive are refused as a topic, because they are far too broad.

```sh
"<path to thor2>/target/release/install.exe" --topic --db "C:\Users\you\AppData\Local\thor2\thor.db"
```

Set a project up with an earlier release? Run this once from its folder
instead: it registers the working copy and changes nothing else.

```sh
"<path to thor2>/target/release/install.exe" --register-root --db "C:\Users\you\AppData\Local\thor2\thor.db"
```

If you are the assistant doing the setup, [AGENTS.md](AGENTS.md) is the
walkthrough for the steps above.

## Use THOR on a second machine

You can use the same memory on a second computer, for example a laptop, and
choose how much of it that computer gets. The second computer gets a **read-only
copy**: your assistant there is reminded of your rules, a rule can still refuse
a wrong step, and search works, but nothing is ever written into the copy. Notes
you want to keep are saved on your main computer and show up on the laptop the
next time the copy is refreshed. The copy refreshes by itself.

**You choose what the copy holds.** Name the projects the laptop needs, and say
whether it also gets your general work rules (the notes that belong to no
project). Every other project, and your personal library, is never copied: it
is not hidden, it is not in the copy's file at all. That makes it fit for a
company laptop. Choose nothing and you get the whole memory.

What stays on the laptop is a filtered copy with its own chain of checks, so
`doctor` can verify it like any memory. Two honest limits: a note you did copy
may mention a project you did not copy by name (it is only words there, nothing
breaks), and a note that was ever filed under a project you did not choose stays
out entirely, even if it was later made general.

### What you need first (on your main computer)

The copy is made from your backup, so the laptop must be able to read it. This
part is the one thing THOR cannot do for you.

1. **Find your backup.** On the main computer, run THOR's health check (ask your
   assistant "Is THOR working?"). Look for the line that starts with `backup:`.
   It says where the last copy went, for example `backup: last copy went to
   backup-repo (remote nas: //nas/backups/thor), 3h ago`. The place after
   `remote nas:` is your backup's address. If there is no such line, you have not chosen a backup yet: tell your
   assistant "set up a backup to" followed by a folder on your NAS or a private
   repository address, then run the health check again.
2. **Make sure the laptop can open that place.**
   - A folder on a NAS: on the laptop, open File Explorer (Windows) or your file
     manager (Linux), type the folder's path in the address bar and press Enter.
     If it asks for a user name and password, enter them and tick "Remember my
     credentials". If the folder opens and you can see a folder called `thor2`
     in it (or the backup's own repository files), you are done.
   - A private repository on a website: on the laptop, open a terminal and run
     `git ls-remote "<the address>" HEAD`. If it prints a line starting with a
     long code, you are done. If it asks you to sign in, sign in. If it says
     "not found" or "access denied", the laptop's account has no access yet:
     add it on the website (Settings, then Collaborators), or ask whoever
     manages that repository.
   - A laptop that belongs to a company: its network or its security software
     may block the NAS or the website. If the check above fails, ask the
     company's IT people to allow the laptop to read that one address. Tell
     them it is read-only.
3. **Keep git on the laptop** only when the backup is a repository address. A
   plain folder on a share needs no git at all.

### The one command

Open a terminal **inside your project's folder** on the laptop (on Windows:
open the folder in File Explorer, click the address bar, type `powershell` and
press Enter). Then paste ONE of these, after replacing the three parts in
capitals. `YOUR-PROJECT` is the project's name exactly as the main computer
knows it (the name of its folder, unless you changed it).

Windows, in PowerShell:

```powershell
$env:THOR_COPY_FROM = "THE-BACKUP-ADDRESS"; $env:THOR_COPY_SCOPES = "YOUR-PROJECT"; $env:THOR_COPY_GLOBAL = "1"; irm https://raw.githubusercontent.com/nworks3d/THOR-for-claude-code/main/install.ps1 | iex
```

Linux:

```sh
curl -fsSL https://raw.githubusercontent.com/nworks3d/THOR-for-claude-code/main/install.sh | THOR_COPY_FROM="THE-BACKUP-ADDRESS" THOR_COPY_SCOPES="YOUR-PROJECT" THOR_COPY_GLOBAL=1 sh
```

- `THE-BACKUP-ADDRESS` is the place from step 1 above, for example
  `\\nas\backups\thor` or `/mnt/nas/thor`.
- Several projects: separate them with commas, `"project-one,project-two"`.
- Leave out `THOR_COPY_SCOPES` and you get the project of the folder you are in.
- Leave out `THOR_COPY_GLOBAL` and the general work rules are not copied.
- On Windows the `$env:` values only live in this window. Close it when you are
  done.

THOR is already unpacked on this laptop (from an earlier install)? Then run the
installer directly instead, with the same choices as options:

```powershell
& "$HOME\thor2\install.exe" --copy-from "THE-BACKUP-ADDRESS" --scope YOUR-PROJECT --include-global
```

```sh
~/.thor2/install --copy-from "THE-BACKUP-ADDRESS" --scope YOUR-PROJECT --include-global
```

(Repeat `--scope` for more than one project. Add `--whole-memory` instead of
the choices to copy everything.)

### Or copy the whole memory

Only do this on a computer you trust, such as your own second laptop. A full copy
holds everything in your memory, including anything personal in it, so it does
not belong on a work or company laptop (use the project choice above there).

You do not need to name a project. Open any terminal on the laptop (on Windows:
press the Windows key, type `powershell`, press Enter), replace
`THE-BACKUP-ADDRESS` as above, and paste ONE of these:

Windows, in PowerShell:

```powershell
$env:THOR_COPY_FROM = "THE-BACKUP-ADDRESS"; $env:THOR_COPY_WHOLE = "1"; irm https://raw.githubusercontent.com/nworks3d/THOR-for-claude-code/main/install.ps1 | iex
```

Linux:

```sh
curl -fsSL https://raw.githubusercontent.com/nworks3d/THOR-for-claude-code/main/install.sh | THOR_COPY_FROM="THE-BACKUP-ADDRESS" THOR_COPY_WHOLE=1 sh
```

`THOR_COPY_WHOLE` does the same as `--whole-memory`. If you also give
`THOR_COPY_SCOPES` or `THOR_COPY_GLOBAL`, the full copy wins: nothing is left
out, and the command tells you that those two were not needed. The copy is still
read-only, and it refreshes by itself like any other copy.

It downloads THOR, checks the download, connects THOR to Claude Code, makes the
copy, and tells you what you have. This is what you see at the end (your own
folders and numbers will differ):

```
Setting THOR up as a read-only copy of another machine's memory.

= you did not name a project, so this copy holds the project of the folder you ran this in: 'acme-shop' (add --scope <name> to choose another)
+ made the copy from the backup: 3 notes in it, 3 records written (15 ms)
  what it holds: project acme-shop, general rules (no project)
  project 'acme-shop': 2 notes
  general work rules: 1 notes
  every other project, and your personal library, were left out of the copy file on purpose
+ connected THOR to Claude Code (4 of 4 connections), so the memory speaks up by itself
+ the small file that keeps your assistant's replies tidy is in place

What you have now
  the copy:       C:\Users\you\AppData\Local\thor2\thor.db
  refreshed:      by itself, in the background, at the start of a Claude Code session (at most once an hour)
What is read-only here
  - nothing in this copy can be changed from this machine: your assistant reads it, and rules still fire and still refuse
  - notes are saved on the main machine, and show up here after the next refresh
  - the weekly check runs on the main machine only

Done. ONE THING LEFT FOR YOU: close Claude Code completely and open it again.
Claude Code only learns about THOR when it starts, so until you do this nothing above is active.
```

(The first lines, "Downloading THOR", "Checksum matches." and "Unpacked into
...", come from the download step.) The line `! the project 'x' has no notes in
the backup` means the project name is spelled differently on the main computer:
run the command again with the right name. It is safe to run the command again
at any time; a second run changes nothing it does not have to.

### If it stops

The command either finishes or says plainly that nothing was set up and why:

- `The copy could not be made, so nothing was set up. Reason: the backup at ...
  could not be reached` - the laptop cannot read the backup. Go back to step 2
  of "What you need first". Nothing was left behind; run the same command again
  when it works.
- `This machine already has a memory of its own ... and it is not a copy, so I
  will not replace it` - THOR was installed here the normal way first and made
  its own empty memory. If you never saved a note here, rename the file named
  in the message (do not delete it) and run the command again. If it holds
  notes you wrote, keep it, and ask for help before you continue.

### After the command

1. **Close Claude Code completely and open it again** (not just the
   conversation). The first session start says: *"This machine runs a copy of
   the memory ... It holds only: project acme-shop, general rules (no project).
   Notes of other projects are not on this machine on purpose."*
2. **Check it.** Ask your assistant "Is THOR working?", or run this yourself
   (Windows: `& "$HOME\thor2\doctor.exe" --db "$env:LOCALAPPDATA\thor2\thor.db"`,
   Linux: `~/.thor2/doctor --db ~/.local/share/thor2/thor.db`). The short summary
   at the top says, among other lines: `OK: this is a copy, selection: project
   acme-shop, general rules (no project), last pull: 5 minutes ago, 3 items`.
   The notes of the projects you did not choose are not missing by mistake, and
   the check never counts them as a problem. It warns you if the copy has not
   been refreshed for three days.
3. **Optional: tell the main computer where the project lives on the laptop**,
   so rules that look at a file can be checked here. From inside the project's
   folder run the installer with `--register-root --per-machine` and
   `--db` naming the copy. It writes nothing on the laptop and prints one line;
   run that line on the main computer.

### Change what the copy holds, or refresh it by hand

```powershell
& "$HOME\thor2\sync.exe" pull --db "$env:LOCALAPPDATA\thor2\thor.db" --scope project-one --scope project-two --include-global
```

```sh
~/.thor2/sync pull --db ~/.local/share/thor2/thor.db --scope project-one --scope project-two --include-global
```

The choice is remembered, so a later `sync pull --db ...` with no options
repeats it (this is what the automatic refresh does). A narrower choice removes
what it no longer covers from the copy at the next pull. `--whole` goes back to
the whole memory. A refresh builds a new copy next to the old one and swaps it
in only when it is complete and checks out; if anything goes wrong (no network,
the NAS is off) the old copy keeps working and the next session start says so
in one line.

One thing to know about a company laptop: if you point the command at a
repository folder that already sits on the laptop (a "clone"), that folder holds
the **complete** memory, and the health check says so. Give it the backup's
address instead, or a plain folder on the NAS; then the whole memory is only
read in passing and nothing of it stays on the laptop.

Not covered here: saving notes from the laptop (they are saved on the main
computer; the longer setup in [thor2/README.md](thor2/README.md), "Second
machine (laptop)", queues them for you, but that route works on the whole
memory), and searching by meaning. Also not yet done: a run on a real second
computer. The steps above are covered by tests and by a full run on one machine.

## Using it from another assistant

The automated setup above is for Claude Code specifically. Any other assistant
that can be pointed at an external tool server can still use THOR, through the
container published alongside every release instead of a local build.

A generic client config, the shape most tool-calling assistants expect:

```json
{
  "mcpServers": {
    "thor": {
      "command": "docker",
      "args": [
        "run", "-i", "--rm",
        "-v", "thor-data:/data",
        "ghcr.io/nworks3d/thor-mcp:2.6.0"
      ]
    }
  }
}
```

### Run it as a container

```bash
docker run -i --rm -v thor-data:/data ghcr.io/nworks3d/thor-mcp:2.6.0
```

On an empty volume, the container creates a memory and seeds it with THOR's
own set of starting notes before it starts answering - the same first
step the setup above takes. Point `-v` at a folder or named volume that
already holds a `thor.db` instead (a copy of the one your own build created,
say), and that one is opened untouched. `-i` keeps input open, which is what
talking to it over stdio needs; drop `--rm` if you would rather keep the
stopped container around than have it clean up after itself.

## Stay in one conversation

The old advice was to start a fresh chat often, because long ones got worse and
you lost everything anyway. With THOR that advice is out of date. **One long
conversation is now the better habit.**

When a conversation gets long, the assistant's tools squeeze out the older parts
to make room. THOR covers that moment: your standing rules come straight back,
and it nudges your assistant to write down anything important that was never
saved. Starting a fresh chat is covered too - your rules and your project's
background are loaded in from the start.

So stay in one conversation while you are on one piece of work. Start a fresh
one on purpose - because you have moved on to something else, or because this
one has talked itself into a corner - not because it is getting long.

## What the first week actually looks like

Worth knowing before you start, because the beginning is the least impressive
part and it is easy to conclude too early that nothing is happening.

**Day one, it stops almost nothing.** A fresh memory holds 22 pinned starting
notes (how to write one that comes back, plus honesty, agent and hygiene habits
that hold on any project) and 14 general work rules that arrive right before
the step they are about. The part of THOR that can refuse a
wrong change only works on notes that carry a proof, and you have not written
any yet. So on the first day you get those notes at the start of a conversation
and the first-session list described under Getting started, and no refusals,
with one exception: on Windows, two of the work rules refuse a call that would
flash a console window. That is not a fault; there is simply nothing yet to
refuse with.

**Your first note will probably be turned down.** It asks for two things most
people leave out: when the note should come back to you, and what would show it
had gone wrong. If the note names a command or a filename, THOR also tries to
build a check from your own words; when none holds, the note is stored as a
plain note that can inform but not refuse, and that is fine. The refusal names
everything that is missing at once and says what to write instead, so the
second attempt usually lands. It is strict on purpose - a note nobody can ever
prove wrong is a note that quietly stops being true.

**The value arrives once you have notes about real places.** A note tied to a
file, a folder or a command comes back exactly when you touch that thing. A
handful of those is worth more than fifty general ones, and after a week or two
of writing them down as you go, your assistant stops asking you the same
questions.

Then check what you have built with `doctor`. It tells you plainly how much of
your memory can actually stop a wrong change, how often it has, and which parts
nothing ever re-reads.

### If you arrive with a memory you already have

Everything above describes a memory that starts empty. If you are coming from
version 1, or from any pile of notes written before proofs existed, the shape is
different and worth saying plainly, because the obvious plan does not work.

THOR's weekly review handles a limited batch of old notes each time. That is a
brake, not a broom. It keeps the pile from growing while you work, and it was
never meant to clear one: a backlog of thousands outlives you at that pace.

The broom is the health check, pointed at the folder your projects actually live
in:

```bash
doctor --db <your thor.db> --checkouts <the folder holding your projects> --full
```

That names every note whose anchor points at nothing, every proof that now comes
out false, and every note stored somewhere too crowded to ever be shown. Set
aside an afternoon rather than a coffee, and go through it in one sitting. The
list is long because the memory is old, not because anything is broken.

Two things make that afternoon safe to be decisive in. Correcting a note keeps
the old version, so nothing you wrote is lost and you can always read back what
it used to say. And removing one does not delete it either: it stops being
handed to anyone, stays findable, and the reason you gave stays attached to it.

## Does it work?

Use it for a week and see whether your assistant stops asking you the same
things. That is the only test that answers the question you actually have.

THOR was measured head to head against another memory tool for months, and those
numbers are not here any more. Not because they were bad - they were good - but
because a score measured on someone else's notes tells you about them, not about
you. The tool is here. The verdict is yours.

## The sixteen tools

What your assistant actually calls, grouped the same way the memory itself is
split in two.

**Code lane** - projects, rules, how to work:

- `remember` - store a new rule, note, report or lookup entry
- `revise` - correct one that already exists, instead of a near-duplicate;
  taking teeth away from a rule that can already stop a mistake - dropping
  its check, lowering its severity, or narrowing where it applies - needs a
  reason too, kept in its history
- `retract` - remove one that turned out wrong (a reason is required; nothing
  is deleted)
- `resolve` - settle two versions of the same fact that diverged apart
- `pin` - make an existing note a standing rule, served at every session start
- `unpin` - stop it being one
- `get` - show one item whole, by id
- `lookup` - search everything - every project, by scope, by key, or by free
  text
- `history` - walk one item's whole past, oldest first
- `mark` - judge whether something you were served actually helped
- `status` - how much of the memory can actually stop a mistake, and how much
  has gone stale
- `search_code` - search the project's own indexed source code
- `where_used` - find every place one function, struct or variable is defined
  and used
- `outline` - see everything one file declares, in line order

**Library lane** - recipes, books, training, expenses, kept apart from the
code lane:

- `library` - read a shelf's index, one entry, or search across shelves
- `shelve` - file a new entry, correct one that already exists, or retire one

## Remove THOR completely

Everything the install put on your computer is listed here, with how to undo
each piece. Nothing is hidden anywhere else. Your own projects and their code
are never changed, apart from the one small file in step 9. Your assistant can
do all of this for you if you ask; these are the steps in plain words.

1. **Close Claude Code completely.** It reads its settings once, when it opens.
2. **Keep a copy of your notes first, if you might want them again.** Copy the
   memory folder from step 7 somewhere safe. Deleting it in step 7 cannot be
   undone.
3. **Take the four THOR lines out of Claude Code's settings.** Open
   `settings.json` in the `.claude` folder inside your home folder. Under
   `hooks`, delete the entries for `SessionStart`, `PreToolUse`,
   `UserPromptSubmit` and `Stop` whose command names THOR's `serve` program and
   ends in `hook`. If you chose a backup, there is one more `SessionStart` entry:
   the one whose command runs the `backup` program (`backup.exe` on Windows)
   with `--db` naming your memory and `--repo` naming the `backup-repo` folder
   (or the folder you chose). Delete that one as well. Leave every other entry
   alone.
4. **Take THOR out of the tool list.** Open `.claude.json` in your home folder
   and delete the entry called `thor` under `mcpServers`.
5. **Delete the `/thor-eval` command.** It is the file `thor-eval.md` in the
   `commands` folder inside the `.claude` folder in your home folder.
6. **Undo THOR's git setting, only if it is THOR's.** Run
   `git config --global --get core.hooksPath`. If the folder it prints ends in
   `thor2/githooks` (or `thor2\githooks`), run
   `git config --global --unset core.hooksPath`. If it prints some other
   folder, or nothing, leave it alone: it is not THOR's.
7. **Delete the memory folder.** On Windows that is `%LOCALAPPDATA%\thor2`, on
   Linux `~/.local/share/thor2` (the install printed it on its `memory:` line).
   It holds your notes, the small files beside them (the reply rules, the modes,
   the weekly-check clock, and the recording switch, which lives in
   `thor-mode.json`), the record of real calls (`real-calls.jsonl` and
   `real-replies.jsonl`), THOR's reading of your code, its git helpers, and the
   `backup-repo` folder if you chose a local backup.
8. **Delete the programs.** On Windows that is `thor2` in your home folder, on
   Linux `~/.thor2`.
9. **Delete the hidden `.thor-project` file** in each folder where you ran the
   install with `--topic` or `--project`. It is the only thing THOR ever put
   inside one of your own folders.
10. **Open Claude Code again.** A new conversation should start without any THOR
    block, and `/thor-eval` should be gone. The `settings.json.bak` and
    `.claude.json.bak` copies hold your two files as they were just before THOR
    first changed them (a later run never replaces them). They can be deleted;
    do not restore them over the current files, they predate anything else you
    changed since.

## Documentation

| page | what it answers |
|---|---|
| [AGENTS.md](AGENTS.md) | for your AI assistant: how to set THOR up |
| [thor2/README.md](thor2/README.md) | the version 2 program: how it is built and what each part does |
| [thor2/CONTRACT.md](thor2/CONTRACT.md) | the standard version 2 is judged against, and the test enforcing each rule |
| [thor2/SPEC-ENFORCEMENT.md](thor2/SPEC-ENFORCEMENT.md) | how a note proves itself, in detail |
| [CONTRIBUTING.md](CONTRIBUTING.md) | changing THOR: the bar for a pull request |
| [CHANGELOG.md](CHANGELOG.md) | what changed in each release: the section every release opens with |

## Thanks

- **MakerViking** - for the inspiration and the great fight. This project would
  not exist without the spark, and it would not be half as good without someone
  worth pushing against. Skål!
- **mimir** ([MakerViking/mimir](https://github.com/MakerViking/mimir)) - the
  reason THOR exists at all. In the old Norse stories, Mimir guards the well of
  knowledge; here it set the bar THOR had to clear, and for a long stretch it
  cleared plenty of its own. Every early comparison in this project was against
  mimir, wins and losses both counted honestly, because a rival that good
  deserves honest numbers (they are no longer published here; see "Does it
  work?").
- **Ideas borrowed, both ways.** Two things THOR does came from mimir's own
  work and were rebuilt here in THOR's own way, and mimir in turn credits THOR
  for reading code into memory and for checking memory on every message -
  exactly the kind of exchange open source is for. Thanks, MakerViking.

## Support this project

THOR is built by [N-Works 3D](https://www.youtube.com/channel/UCrEZc_oJR9mywNjqY115mRg). If it has
earned its keep - saved you an explanation, caught a mistake before it cost you,
or just meant you did not have to start from scratch - there are two easy ways
to help keep it going:

- **[Buy me a Ko-fi](https://ko-fi.com/noizieworks)** - a one-off, whenever you
  feel like it.
- **[Become a YouTube member](https://www.youtube.com/channel/UCrEZc_oJR9mywNjqY115mRg/join)** -
  monthly, if you want to keep it going.

No pressure and no paywall - it all stays open either way. Skål, and thanks for
reading this far.

## Contributing

Bug reports and pull requests welcome. THOR is a memory your assistant is
supposed to trust, so the bar is being right rather than having more features.
The checklist is in **[CONTRIBUTING.md](CONTRIBUTING.md)**.

## License

GPLv3.
