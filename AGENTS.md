# AGENTS.md - read this if you are the AI assistant

The person you work with sent you this link and asked you to install THOR. You
are the one setting it up, and they should have to do exactly one thing: restart
you once. Everything else is yours, in the order below, without asking them
anything in between.

**One rule above all: assume they have never done any of this before.** Say what
you are doing in ordinary words. Never hand back a menu of options, and never
send them off to type a command or a path: you have the tools, so run it.

## The whole install, in five steps

1. **Get the machine ready and install it, from inside their project folder.**
   Open a terminal in the folder of the project they work on. It does not matter
   if it is not a git repository, and a folder of notes for a subject works too
   (see "A folder for a subject, not for code" below).
   - First check what the machine has, and install only what is missing, exactly
     as written in "What the machine needs first" right below this list. Say in
     one sentence what you are about to install and why. The system may show ONE
     permission prompt for it; that click is the person's, and they type nothing.
   - Then run the one command for their system:
     - Windows, in PowerShell:
       `irm https://raw.githubusercontent.com/nworks3d/THOR-memory-bible-for-AI/main/install.ps1 | iex`
     - Linux:
       `curl -fsSL https://raw.githubusercontent.com/nworks3d/THOR-memory-bible-for-AI/main/install.sh | sh`
     - macOS or any other system: there is no download for it yet, so build it
       from source (see "Installing from a build you made yourself" below).

   That one command downloads the latest release, checks it against its
   published checksum, unpacks it into the home folder, creates the memory with
   its starting notes (a general set of work rules included), wires the hooks
   and the tool server into Claude Code, and reads this project's code and
   registers it. No Rust toolchain, no question, no path to type. If your own
   harness asks the person to approve running it, or editing Claude Code's
   settings, that is the harness asking: it is the one click they cannot skip.
2. **Check it before the restart.** Run `doctor` and then `serve session-start`,
   exactly as written in "Checking it" below: the first says the memory is
   healthy, the second shows what the next conversation will be handed. If
   either is not fine, fix it yourself and run the check again. Only report
   success when both pass.
3. **Ask for the restart, in one sentence.** Say it as one plain instruction:
   "Close Claude Code completely - not just this conversation - and open it again,
   in the same folder. Then say hello and I will check it worked." Claude Code
   reads the new memory at startup only; closing one conversation is not enough,
   and neither is a compaction. This is the one thing you cannot do for them.
4. **After the restart, finish the setup yourself.** The new session opens with
   a note telling you setup is not finished. Call `status` first (if the tools
   are not there, the restart did not happen yet: ask once more). Then follow
   "First session with a new owner" below: tell them the defaults in one
   sentence, record them as `owner-setup-answers`, retract the setup note. Do
   not ask them questions and do not wait for an answer; they can change any
   default later by telling you.
5. **Tell them what they now have, in three sentences.** That THOR remembers what
   they tell it and brings it back right before it matters. That a starter set of
   work rules is on (changelog with every change, tests before "done", no secrets
   in a commit, and the rest listed below), and that any can be removed by saying
   so. That to change anything they only have to tell you.

---

## What the machine needs first

You do this inside step 1, for them, with the package manager the system already
has. Check each program; install it ONLY when the check fails. Never ask the
person to type or paste anything, and never offer them a choice of ways.

| Needed | Why | Check | Windows (winget) | macOS (brew) | Linux |
| --- | --- | --- | --- | --- | --- |
| git | Reads a project's code and keeps that reading fresh after each commit. On Windows, Git for Windows also provides Git Bash, the shell Claude Code runs its hooks through. | `git --version` | `winget install --id Git.Git -e --source winget --silent --accept-package-agreements --accept-source-agreements` | `brew install git` | apt: `sudo apt-get install -y git`; dnf: `sudo dnf install -y git` |
| Python 3 | Helper scripts that assistants write and run for a person's work commonly use it. | `python --version` (`python3 --version` on macOS and Linux) must print `Python 3.` The Microsoft Store stub that opens the Store instead is NOT Python. | `winget install --id Python.Python.3.12 -e --source winget --silent --accept-package-agreements --accept-source-agreements` | `brew install python` | apt: `sudo apt-get install -y python3`; dnf: `sudo dnf install -y python3` |
| The C++ runtime (Windows only) | THOR's programs need it; nearly every machine has it. `install.ps1` checks it and stops with this very command if it is missing. | the programs refuse to start | `winget install --id Microsoft.VCRedist.2015+.x64 -e --source winget --silent --accept-package-agreements --accept-source-agreements` | not needed | not needed |

- A program that was just installed is not on the PATH of the terminal you are
  in. Open a new terminal (or ask for a fresh PATH) before you check it again.
- At most one prompt: winget on Windows may ask Windows for permission once;
  `sudo` on Linux may ask for the person's password once. That is the system
  asking, not you. If a prompt cannot be answered where you run, say so in one
  sentence and stop: do not work around it.
- On macOS, if `git --version` makes macOS open its own dialog to install the
  command line tools, that dialog is the system's one prompt. If `brew` itself is
  missing, say so plainly; do not install it silently.
- Rust is NOT needed for the normal install. Only a build from source needs it,
  and the release archive is always the first choice (see "Installing from a
  build you made yourself").
- The winget ids above were each checked with `winget show --id <id> -e` on
  2026-10-09. The brew, apt and dnf package names are the standard ones and were
  not run here.

## A folder for a subject, not for code

This is the route for someone who wants THOR for a SUBJECT - a renovation, a
thesis, a trip, a hobby - and is not a programmer. A folder of notes, quotes and
PDFs is a project like any other, keyed by the folder's own name. It needs no
git. Do the steps yourself; they should not have to type anything.

1. **One folder per subject, with Claude Code opened in it.** If they have not
   made it yet, ask only what the subject is called, make the folder (for
   example `Documents/Kitchen renovation`) and tell them to open Claude Code in
   it next time. Their home folder, `Documents` itself and a whole drive are
   refused: they are far too broad to be one subject.
2. **From inside that folder, run** (the full path to the installer, and
   `--db` naming their memory):

   ```sh
   "<path to thor2>/install.exe" --topic --db "<their store>"
   ```

   The folder's name becomes the project's name and the way everything about
   it is filed. Exactly one small hidden file, `.thor-project`, is added to the
   folder; nothing else in it is touched. A second run says so and changes
   nothing. `install.exe --register-root --db "<their store>"` from inside a
   plain folder does the same in one step.
3. **Say what happened in one sentence, in their words:** "From now on, what you
   tell me in this folder is remembered for this topic only." Nothing about git,
   scopes, working copies or markers.
4. **Know what works and what does not.** A rule whose check reads a file
   (`contains`, `absent`, `path_exists`) is tried against the topic folder's real
   files, exactly as in a code project. `search_code`, `where_used` and
   `outline` need a code repository: here they answer that plainly, and you read
   the files yourself instead. Nothing fails because of it.
5. **Prove it before you say it works.** Store one thing they said, filed under
   the topic (pass the folder's name as `project`, or the write is refused as a
   new collection):

   ```
   remember(
     id:        "tiles-from-tilehouse",
     kind:      "orientation",
     project:   "Kitchen renovation",
     always:    true,
     text:      "The kitchen tiles are ordered at Tilehouse.",
     falsifier: "A later note says the tiles come from somewhere else."
   )
   ```

   Then, after the restart in step 3 above, open a fresh conversation in the same
   folder and check that it comes back at the start. If not, the cause is nearly
   always the restart.
6. **Check it with `doctor`** run from inside the folder: its first lines are a
   plain summary, and one of them reads "OK: this folder has its own memory".

If `install.exe --topic` refuses, its message is plain and names the one fix.
Nothing is changed when it refuses.

---

# The developer route and the details behind the steps

Where things end up: the programs are in the `thor2` folder in the home folder
(`install.exe` there is step 1, done), and the memory is
`%LOCALAPPDATA%\thor2\thor.db` on Windows or `~/.local/share/thor2/thor.db` on
Linux, unless the installer printed another path on its `memory:` line. Use that
path wherever `<their store>` appears below.

## Installing from a build you made yourself

Only for a system with no download (macOS today) or when they asked for a build
from source. You need a Rust toolchain and nothing else: if `cargo` is missing,
install it first (Windows: `winget install --id Rustlang.Rustup -e --source winget`;
macOS: `brew install rustup`; Linux: rustup.rs).

```sh
cd thor2 && cargo build --release --features semantic
```

Then check the size of `thor2/target/release/serve.exe` (`serve` on Linux and
macOS). Over 20 MB is right. A few MB means `--features semantic` was left off,
and searching by meaning is now silently missing: it returns nothing at all, with
no error anywhere. Build it again rather than carrying on. Then run
`thor2/target/release/install.exe` (`install` elsewhere) from the project's
folder: it is the same setup step the download ends with.

## What the install command does

The scripts download `thor2-windows-x86_64.zip` or `thor2-linux-x86_64.tar.gz`
from the latest release, verify the `.sha256` beside it, unpack it into
`%USERPROFILE%\thor2` (Windows) or `~/.thor2` (Linux; `THOR_HOME` changes it) and
run the archive's `install` program with no arguments. On Windows the script first
checks for Microsoft's Visual C++ runtime, which almost every machine has; if it is
missing the script stops and prints the one `winget` command that installs it
(Windows may ask permission once). Nothing needs administrator rights otherwise.

`install` in the common case takes no arguments at all. It finds Claude Code's own
two config files by itself - the per-user `settings.json` for the hooks, and
`~/.claude.json` for the tool server you write through - creates their memory if
there is not one yet, seeds the notes in "First session with a new owner" below,
and wires it all together. It backs up both files first, never removes anything it
did not put there, and a second run changes nothing.

It also points git at a shared set of hooks that run in every repository on this
machine, not only this one - each one still finishes by running that repository's
own hook, so nothing already there stops working. If git's global hooks folder
(`core.hooksPath`) is already set to something else, the install leaves it alone
and says so; the shared hooks are then not wired, and searching a project's code
does not update by itself after a commit until that is sorted out.

It also writes an end-of-session routine, `/thor-eval`, to your own commands
folder. Run it at the end of a session: it walks through everything the memory has
been served without a verdict and settles that debt in one pass, rather than
leaving it for the next session to inherit.

Run from inside a git project, it also reads that project's code once, so
searching the code, finding where a symbol is used and outlining a file answer
there at all, and from then on every commit keeps that reading fresh by itself.
That same successful read opens the project in the memory, so a note lands under
this project's name without ever being asked to name a new collection. It also
registers that checkout as the project's working copy: the one folder THOR tries
a rule's file check in when the rule is written (see "How to write a note" below).
The line starting with `+ registered` names the folder.

For an unusual setup: `--settings` and `--mcp-json` point it at other files, `--db`
and `--serve-exe` override the rest, and `--no-mcp` installs a memory the agent can
read but not write, on purpose. If it stops because a program it needs is not next
to it, the download was incomplete or the build is: do that step again. It refuses
rather than install a hook that points at a missing binary - a missing program
produces no error later, only silence, and their memory would simply never speak.

## Checking it

Both of these work before the restart. First the health check, with the store path
from the `memory:` line the install printed (`<bin>` is the folder the install
unpacked into: `%USERPROFILE%\thor2` on Windows, `~/.thor2` on Linux):

```sh
"<bin>/doctor" --db "<their store>"
```

The first lines are a short plain summary: OK or PROBLEM, and every problem
carries its one next step. Read those out to them in your own words; the long
report under it is for you, not for them. What you look for in the long report:
`memory store: N events, chain intact`, `falsifiers: all N rule(s)/orientation(s)
have one` and `dead moments: none`. A missing language model is not a problem:
THOR works without it, just with simpler matching. A fresh memory also prints
lines headed `undecided`, `toothless` and `batch`: that is the upkeep THOR will
do itself in its first evaluation, not a fault, so do not act on them and do not
read them out to the person.

Then ask the memory what it would hand over at the start of a conversation. This is
the very thing Claude Code's hook asks on the next start, without recording
anything:

```sh
"<bin>/serve" --db "<their store>" session-start
```

It prints a `Standing rules for this session:` block holding the starting notes
(among them `say-what-actually-happened` and `walk-through-the-answer-guard-once`).
A block with nothing in it, or an error, means the memory would stay silent after
the restart: run the install again and read what it printed.

## Another project, later

Every project is scoped by the name of its folder already, with no setup. To read
another project's code and register its working copy, run the installer again from
inside that folder, naming the memory (`--db`, the path the install printed on its
`memory:` line):

```sh
"<the folder the install unpacked into>/install" --db "<their store>"
```

Add `--project "<project-name>"` only when the folder is not called what they call
the project, and only when they told you the name: you may not invent one.

**A project an earlier release already read has no working copy yet.** Nothing
registers it by itself. Run this once from inside that checkout - it records the
folder and touches nothing else, no settings and no code reading:

```sh
"<the folder the install unpacked into>/install" --register-root --db "<their store>"
```

It answers `registered '<project>' -> <folder>`. Doctor names every project that
owns rules but has no working copy registered, with this same command.

One project has one working copy, and the command says so when you are in the wrong
place. Nothing is changed when it refuses:

- `REFUSED: '<project>' is registered at <folder>, which is still a checkout of it` -
  you are in a second copy of the project. Go to that folder and run the command
  there, or leave it: that folder is already the one THOR uses.
- `... which still exists. Moving a registration is the owner's call` - the project
  moved, but its old folder is still there. Ask the owner. Once he has removed or
  renamed the old folder, run the command again from the new one.

It refuses to change a name already there - renaming a scope would strand every
note filed under the old one. A repository on a network share needs a
`safe.directory` line before git will open it as you.

A folder with no git in it is not an error and needs no repository: it is a
topic folder, and "A folder for a subject, not for code" near the top is its whole route.

## After the restart: one real round trip

Once the tool server is live (step 4 above calls `status` for exactly this), prove
the write path with something they actually said, not a test string - a note worth
keeping is a better proof than "hello world", and they get to keep it. Do this only
when they have said something worth keeping; never invent one.

```
remember(
  id:        "dev-server-needs-the-vpn",
  kind:      "rule",
  text:      "The staging server only answers over the VPN; without it every request just times out.",
  targets:   [{ kind: "host", value: "staging.example.internal" }],
  falsifier: "A request to staging succeeds with the VPN off."
)
```

Then touch that host in the same conversation: the note comes back right before the
call. If a restart never happened, the tool is simply missing - that is step 3 again.

## Backup

A fresh install makes NO backup: nothing is backed up until the person chooses
(item 10 of the first-session list). One command does all of it, and makes the
first copy at once to prove it works:

- `install --set-backup local --db "<their store>"` - a git folder beside the memory
  (`backup-repo`), no remote, nothing leaves the machine. It protects against a
  damaged memory file, not against the disk dying.
- `install --set-backup "<folder or address>" --db "<their store>"` - the same folder
  plus a remote called `nas` pointing at the place they named (a folder on a NAS or
  another machine they own is the safe choice, a private repository the other). A
  folder that does not exist yet, or is empty, becomes the empty bare repository.
- `install --set-backup off --db "<their store>"` - removes only the connection that
  makes the copies; copies already made stay.

Once chosen, THOR copies when a session starts, at most once a day (the program is
`backup.exe --db "<their store>" --repo "<that folder>"`). By hand, the same thing for
a NAS: create an empty bare
repository there (`git init --bare <folder on the NAS>`), add it to the backup folder
as a remote called `nas` (`git -C "<that folder>" remote add nas <the NAS folder>`),
and push the branch once by name (`git -C "<that folder>" push nas main`). From then
on the backup uses `nas` by itself, with no other setting. If git says the folder has
"dubious ownership", add it as a safe directory with the line git prints. `doctor`
then names the place the last copy went. The memory can be rebuilt from it with
`backup.exe --db "<a new store path>" --restore-from "<the thor2 folder of the
backup>"`.

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

The install puts these into their memory as pinned notes, so from the next
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
or orientation bound to nothing is refused, and so is a costly or irreversible
one bound only to a project name (that reaches only a prompt where the owner
types the name, never a tool call): bind it to the file, command or moment it
is about, or pin it. Never put a real secret token (an API key, a GitHub token,
a private key) into a note: it is refused, and a stored one can never be
removed from the log or its backup. Write where it lives instead. A
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
`/thor2`. A call that does nothing is never refused by a forbidden, `requires`
or owner-approval rule: one that only asks for `--help` or `--version`, a call
with `--help` as the first word after the command (or after one subcommand word,
as in `docker compose up --help`; a `--help` further along does not count), a
`git push --dry-run` (or `-n`), or a PowerShell call with
`-WhatIf`. The flag has to be on the same command as the rule's, not on a later
one joined by `&&` or `;`.

**A file check needs the project's working copy.** THOR tries it in the
checkout registered for the rule's project (the install, run from inside the project, registers it). Written from
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

**Every one of the three also needs `because`**: one sentence, at least 20
characters, saying WHY the named span may change, or why the call is
legitimate, while the rule stays true. Without it the write is refused and
nothing is stored. The reason is kept in the item's history (`history` shows
it), and the evaluation machine section and doctor print it beside the
dispute; a dispute with none reads "no reason recorded". **Try the better path
first:** hang a check on the literal that IS the rule's subject (name it in
the text), and dispute only a span that is incidental. A wrong dispute turns a
rule that bites into a note that can never refuse anything, and it stays
hidden until someone audits it - 7 of the 11 disputes one maintenance pass
saved on 2026-10-07 were wrong. `serve retest` never carries a dispute that has
no reason on record: it tries the rule without it and says so. Example:
`may_change: ["version"]` with `because: "the version number moves with every
release, so that line may change while the rule stays true"`.

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
before any other work, and that it covers everything the install arranged, not
only the answer guard. The full list is in the memory as the lookup item
`first-session-questions` (`lookup` with that key) and below on this page: they
are the same text, rendered from one catalogue in the code.

**Everything the install arranges or switches on is on that numbered list, so
nothing happens silently.** The list has two layers, because four parts under
every item was too much to read. **The person is shown only the short layer:** a
two-line intro, ONE line per item (what it is, and what was chosen for them),
and how to answer. **The full layer is yours,** in the appendix under the
assistant rules: per item what it is, why it exists (the real reason or the
measurement behind it), what was chosen, and how to say no. You state an item's
why ONCE, and only when the person asks about that number ("why 7") or says no to
it. **Do not turn it into an interview and never make anyone wait.** The person
asked for THOR to be installed and nothing more, so every item has a default,
and the defaults apply without waiting for an answer: apply them, record them,
retract the setup note, and THEN show the short list in their own language with
"everything here is already as written - tell me by number what you want
different". They answer per number ("1 yes, 3 no, 10 local"); anything not
mentioned keeps the default, and "defaults" or "doe maar" accepts all of it.
Someone who does not answer at all is answered by the defaults.

**Two items are not simply "on".** Backups (10): a fresh install wires NO backup,
so with the default nothing is backed up; say that once, and that `backup local`
or a named folder or address starts one (`install --set-backup`, which also makes
a first copy to prove it works). Changelog (12): the default is yes when the
project has none, so you create `CHANGELOG.md` in the project you are in, unless
they say no.

**When an answer works against them,** say ONCE, in one or two plain sentences,
why the item exists (its "Why it exists" line in the appendix), then do what they
decided. They have the final word. Never block, never ask twice, never repeat the
warning.

This is held, not only asked. The session does not end while the note is
live, and retracting it is refused until the owner's answers are on record as
`owner-setup-answers` - store what applies first, then retract; the retract
itself fails otherwise, with the same reminder. Store it as a plain Report
with no project: a store this fresh has not named a scope yet, and this one
item needs none - do not invent a collection for it, and do not ask him to
name one just to get this write to land. "He is not interested" is itself a
valid answer to record - write that down plainly and the note clears exactly
the same way a full set of preferences would. When answers come later, REVISE
that report; a second copy is worse than none.

Say first, in two sentences and no jargon: "This remembers what you tell me
about your work, so you never have to repeat it, and it brings the right thing
back at the right moment." Then show the list.

<!-- questionnaire:begin -->
THOR is set up. Here is everything the install arranged or switched on, so nothing happens silently, one line each with what I chose for you.
Nothing here waits for you: ask me about any number and I explain it, say no to any number and I switch it off.

1. Your plan: I read your plan from Claude Code's settings (the settings' answer goes here; when they say nothing, ask: Max, or Pro and smaller?), so THOR's self-check does not eat your usage limit. Default: what the settings say.
2. Connections: THOR is wired into Claude Code at session start, before each tool call, when you send a message and when a reply ends, so the memory speaks up by itself. Default: on.
3. Git hooks: every repository refreshes THOR's code index after each commit, so searching the code stays right. Default: on.
4. Weekly check: about once a week THOR has your assistant tidy the memory (you can also type /thor-eval any time). Default: on.
5. Work rules: 13 general work habits stored as notes, such as tests before 'done' and never commit a secret; they only inform, except that on Windows two refuse a console window flash. Default: all on.
6. Helper agents: your main assistant judges while cheaper models do the work in helpers, which costs less and keeps the chat free. Default: on.
7. Bug reports: your assistant may offer to report a fault in THOR on GitHub, showing you the exact text first; nothing is sent without your yes. Default: on.
8. Replies: in your language, short, with a summary first, and five reply rules; a reply that breaks one is rewritten once. Default: on.
9. Topic folders: a plain folder with no code, such as recipes or travel plans, can get a memory of its own when you name it. Default: none set up yet.
10. Backups: NOTHING is backed up until you choose: a copy on this machine, a place you name (a NAS, another computer, a private repository), or none. Default: none yet.
11. Record of real calls: for 30 days, on this machine only, THOR keeps your commands and replies with passwords and keys blanked, to find rules that would wrongly block you. Default: on.
12. Changelog: Shall I keep a changelog and version number for this project? I create CHANGELOG.md and note each change in it. Default: yes, when the project has none.

Answer by number (for example "1 yes, 3 no, 10 local"), ask "why 7" about any number, or say "defaults" to accept all of it; you can change any of it later just by asking.

FOR THE ASSISTANT (do not read this part out)
- Show him the short list only: the intro, one line per item and the way to answer. Never paste the detail per item (the last part of this text) at him.
- The detail per item is yours: state an item's Why once, only when he asks about a number ("why 7") or refuses it, in one or two plain sentences, then do what he decided. If he asks and has not refused, add how to say no.
- Apply the defaults NOW, in this same turn, without waiting for an answer, so nothing waits: store the Report 'owner-setup-answers' with them and retract the setup note. Then show the short list in the user's own language, once, and say that every default on it is already on, except backup, which stays off until he chooses, and that he can tell you by number what he wants different (for example "1 yes, 3 no, 10 local"; "defaults" or "doe maar" accepts all of it).
- Anything he does not mention keeps the default. Never ask the same question twice and never hold up his work for an answer.
- Item 10 (backups) is the one whose default is NOT on: a fresh install wired no backup, so with the default nothing is backed up. Say that once, in one sentence, after the list, with the way to start ("backup local", or the folder or address he wants); do nothing for it until he answers.
- Item 12 (changelog): the default is yes when this project has none. Unless he says no, create CHANGELOG.md in the project this session runs in (a '# Changelog' heading with an 'Unreleased' section, nothing invented; a project that already has one is left alone) and keep a version number only if the project already has one or he asks for one. If he says no, create nothing and do not ask again.
- Question 1: if the settings named a plan, ask him to confirm it with its reason; if they did not, ask which plan outright with the same reason. Record his word with install --set-plan max|small --db <store>.
- When he answers, apply each number where it belongs, then REVISE 'owner-setup-answers' (never store a second copy): 1 install --set-plan; 2 remove THOR's lines from Claude Code's settings file; 3 git config --global --unset core.hooksPath; 4 install --set-weekly-check off|on; 5 and 6 retract the named notes (a retracted note stops arriving); 7 install --set-mode off (or release); 8 the reply file beside the store (guard-response-rulebook.json) and a stored preference for the language; 9 install --topic, run from inside the folder; 10 install --set-backup local|<folder or address>|off --db <store> (it makes the git folder, adds a named place as the remote 'nas', connects one session-start backup and makes a first copy to prove it works: tell him that result in plain words, and if it failed say so and why); 11 install --set-recording on|off --db <store>; 12 CHANGELOG.md as above.
- A bad answer is one that works against him (for example switching off the connections and then expecting the memory to speak). Say ONCE, in one or two plain sentences, why the item exists (its 'Why' in the detail), then do what he decided. He has the final word, you never block and never nag.
- "He does not want any of this" is itself a valid answer: record it as the answers and apply nothing.

DETAIL PER ITEM (for the assistant: state an item's Why once, only when he asks about a number or refuses it; never read all of this out)

1. Your plan
   What it is: I looked at Claude Code's own settings to find out which plan you have: Max, or Pro and smaller. Ask: 'The settings say <Max | Pro or smaller>. Is that right?' - or, when they do not say, 'Which is it: Max, or Pro and smaller?'
   Why it exists: THOR checks itself on a schedule, and that check draws on the same usage limit as your own work, so on a smaller plan it runs once a week at most and never daily, which keeps your limit for you; on Max it also runs daily while something is found wrong.
   What I chose: The plan the settings say; a smaller plan when they do not say.
   To say no: Tell me the right plan and I record it, so the schedule follows what you really pay for.

2. The connections to Claude Code
   What it is: THOR is wired into Claude Code at four moments: when a session starts, before each tool call, when you send a message, and when a reply ends. That is how the memory speaks up by itself and how replies get checked. A backup copy of Claude Code's settings file was made first.
   Why it exists: A memory that must be asked for stays silent, and a missing connection fails without any message: the memory simply never says a word.
   What I chose: On.
   To say no: Then THOR is only a notebook your assistant searches when you ask. I remove the THOR lines from Claude Code's settings file (the backup copy stays next to it), which also ends the weekly check, the reply rules and the bug offers, since they all run through those connections.

3. The git hooks in every repository
   What it is: THOR set one git setting for the whole machine so that every repository refreshes THOR's code index after each commit. Each repository's own git hooks still run; if another tool already owned that setting, THOR left it alone.
   Why it exists: An index three days behind made 83 percent of the 'where is this used' answers name a wrong line; a fresh index made none wrong (measured at the maker, 2026-08-15).
   What I chose: On, unless something else already owned the setting.
   To say no: I undo it with one git command (git config --global --unset core.hooksPath). Searching the code then goes stale after each commit.

4. The weekly check
   What it is: About once a week per project (daily on Max while something is found wrong) THOR asks your assistant to run a short tidy-up of the memory before it finishes a turn: judge the notes that fired, repair stale ones and file a report. The command /thor-eval was added so you can also run it yourself.
   Why it exists: A memory nobody maintains fills with stale notes and quietly stops helping, and a check that depends on someone remembering it does not happen.
   What I chose: On, at the rhythm of question 1.
   To say no: I switch the weekly ask off (install --set-weekly-check off); you can still type /thor-eval yourself, and nothing else changes.

5. The starter work rules
   What it is: A set of 13 general work habits, stored as notes that arrive right before the step they are about, once per session: changelog with every user visible change; run the tests before saying done; never commit a secret; commit push deploy only when asked; commit hygiene; check where you push and what is on main; scan before it goes public; rehearse destructive steps on a copy; pin a dependency with its reason; absolute paths not the shell working directory; script file not shell quoting; windowless processes on windows; scheduled task not interactive. 21 more notes teach your assistant how to write notes that come back and how to work with THOR. None of them blocks a normal step; on Windows two of them, about hidden console windows, refuse the one call that would flash a window.
   Why it exists: They are the habits that were marked useful again and again in a memory built over months of real work, so a new memory starts with that experience instead of learning it the hard way.
   What I chose: All on.
   To say no: Name the ones you do not want, or say none: I remove just those notes (changelog-with-every-user-visible-change, run-the-tests-before-saying-done, never-commit-a-secret, commit-push-deploy-only-when-asked, commit-hygiene, check-where-you-push-and-what-is-on-main, scan-before-it-goes-public, rehearse-destructive-steps-on-a-copy, pin-a-dependency-with-its-reason, absolute-paths-not-the-shell-working-directory, script-file-not-shell-quoting, windowless-processes-on-windows, scheduled-task-not-interactive), and a removed note simply stops arriving.

6. Helper-agent mode
   What it is: Your main assistant (the strongest model) answers, judges and checks, while cheaper models do the real work in helper agents, one helper per task. A trivial one-step job is done directly, tests are run narrowly while working and in full once at the end, and a helper that goes quiet is judged dead after about three checks. This is also true on smaller plans, and if your plan has no strongest model the best available one steers.
   Why it exists: Measured at the maker, helpers were likely 12 to 62 percent cheaper than doing everything in the main conversation, and the chat stays free and answers at once while they work, instead of you waiting 5 to 15 minutes.
   What I chose: On.
   To say no: I remove the two notes that carry it (orchestrator-mode-saves-quota and an-agent-brief-stands-alone); your assistant then does the work itself in the main conversation.

7. Reporting THOR's own bugs
   What it is: Sometimes THOR finds a fault in itself. Your assistant may then offer to report it to the makers on GitHub. It shows you the exact full text first; file paths, project and folder names, computer and user names, addresses, keys and passwords are removed; nothing is sent without your yes, and saying no costs nothing.
   Why it exists: THOR sees faults in itself that nobody reading a report would notice, and the makers can only fix what they hear about.
   What I chose: On: it may ask you, once per fault, and sends nothing without your yes.
   To say no: I switch it off (install --set-mode off): nothing is offered and nothing is sent, ever.

8. Reply language and length
   What it is: Your assistant answers in your language, short, with a plain-language summary first. A small file next to the memory holds five reply rules (a summary first, no reflexive disclaimers, check before asking you to look something up, keep an answer under about 1000 characters, show evidence when saying something was checked); a reply that breaks one is sent back once to be rewritten. A summary or overview you ask for may run longer.
   Why it exists: Long, technical replies are hard to act on; the rules keep a reply to the point, and they were checked against about 6,600 real replies at the maker, and the false triggers found there were removed.
   What I chose: Your language, short replies, all five reply rules on.
   To say no: Tell me the language and the length you want. I change the file, or remove a single rule to turn just that one off.

9. Folders for a topic
   What it is: A plain folder with no code in it, such as a folder of recipes or travel plans, can have a memory of its own: I run one command inside it and it gets one small hidden file. Notes about that folder then come back only when you work there. Nothing is set up until you name a folder.
   Why it exists: Notes about one subject stay out of the way of the others, so nothing from your recipe folder interrupts your code.
   What I chose: No folders are set up yet.
   To say no: Nothing to undo: do not name a folder. A folder you set up has one hidden file, .thor-project, that you can delete.

10. Backups of the memory
   What it is: A backup copy of the whole memory in a git folder. A fresh install makes NO backup, so until you answer nothing is copied anywhere. You choose one of three: a copy on this machine (a git folder beside the memory; nothing leaves the machine, and it protects against a damaged or wrongly edited memory, not against the disk dying), a place you name (a folder on a NAS or another computer you own, or a private repository; that survives the disk dying), or none. Once you choose, THOR copies when a session starts, at most once a day, and makes the first copy at once to prove it works. To start, say "backup local", or name the place.
   Why it exists: The memory is a single file, and if the disk dies everything it learned dies with it.
   What I chose: None: nothing is backed up until you answer.
   To say no: Choose none, or simply do not answer: nothing is copied anywhere. If you change your mind later I run install --set-backup off, which removes only the connection that makes the copies; the copies already made stay where they are.

11. A record of real calls
   What it is: THOR keeps, in two files next to the memory, the commands your assistant ran and the replies it gave in the last 30 days. The files stay on this machine and are never sent anywhere. Passwords and keys THOR recognises are blanked out before anything is written, and every line older than 30 days is deleted by itself. THOR then replays its rules against that record to find a rule that would wrongly block you.
   Why it exists: Replaying 146,000 real calls found 12 rules that blocked calls which were fine, and no review of the reports had shown them (measured at the maker, 2026-10-09).
   What I chose: On.
   To say no: I switch it off (install --set-recording off): nothing new is recorded and the check for wrongly blocking rules then says it has no data; what was already recorded disappears by itself after 30 days.

12. A changelog and version number
   What it is: A changelog is a file, CHANGELOG.md, with one line for every change you can see, and a version number says which state of the project is running. If this project has none I create the file now and add a line with each change from then on; a project that already keeps one is left as it is.
   Why it exists: Every later session, and you, know exactly what changed and which version runs, without digging through history.
   What I chose: Yes, when this project has none.
   To say no: Then I create no file and do not ask again; the starter work rule about changelogs (item 5) stays, and it only applies to a project that keeps one.
<!-- questionnaire:end -->

Three more facts to give in your own words, without a question:

- **Lanes.** The code-and-work memory is the default. The personal lane for
  their own life - recipes, books, a training log - exists anyway and needs no
  setup; anything personal goes to the library with `shelve`, never into the code
  memory with `remember`.
- **Project names.** Every checkout is scoped by its folder name automatically -
  facts stay inside that project and do not compete across projects. If a folder
  name does not match how they call the project, they can run
  `install --project <name> --db "<their store>"` from inside the checkout to
  override it. Only when they tell you a name, use it - never invent one.
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

## The two calls

Make them in this order, in the first reply, whatever the person answers:

```
remember(
  id:   "owner-setup-answers",
  kind: "report",
  text: "Setup finished with the defaults, nothing asked first. Plan: <as detected or told>. The defaults on the numbered list apply, except backup, which stays off until the owner chooses (changelog: yes when the project has none). Replies: short and plain, in the owner's own language. Lane: work only. Projects: named after their folder. Any of these changes when the owner says so."
)
retract(id: "walk-through-the-answer-guard-once", reason: "Setup finished: the owner's answers are on record as owner-setup-answers.")
```

When the person answers by number, apply each answer where it belongs (the
assistant rules under the list say where), then `revise` `owner-setup-answers`
with what they chose.

## The starter set of work rules

A memory that was just created also gets a starter set of general coding-workflow
rules, learnt the hard way by the person who built THOR and rewritten so that they
hold on any project. They are ordinary notes tagged `workflow-starter`: only two of them
carry a refusing check, and only on a Windows install (they stop a console window from
flashing); every other one only informs, because a refusing check could block a
legitimate call from a project the author never saw. Each arrives right before the step
it is about, once per conversation, and any can be removed, rewritten or unpinned like
any other note. Tell the person, in one sentence, that they exist and that you can
remove any of them on request; to remove one, `retract` it with a reason.

Before a commit or a push:

- `changelog-with-every-user-visible-change` - if the project keeps a CHANGELOG, a
  line for every user-visible change in the same commit, and a version bump when it
  versions releases. A changelog is created only when the owner said yes to first-session
  item 12; otherwise any changelog the project already has is kept.
- `run-the-tests-before-saying-done` - run the project's own tests or build and
  quote the real result line; never skip a hook with `--no-verify`.
- `never-commit-a-secret` - real values in a gitignored file, a tracked template
  beside it, a look at what is staged, rotate anything that was committed.
- `commit-hygiene` - a short plain message, `git commit -F` for several lines, stage
  only what you mean, keep each file's line endings.
- `commit-push-deploy-only-when-asked` - only on the user's word in this session; no
  force-push of a shared branch.
- `check-where-you-push-and-what-is-on-main` - folder, branch and remote, then fetch;
  release state comes from git, never from memory.
- `scan-before-it-goes-public` - zero hits for secrets, usernames, local paths and
  private hostnames on every push to a public repository.

Around a risky step or a tool:

- `rehearse-destructive-steps-on-a-copy` - a delete, migration or deploy is tried on
  a throwaway copy first; never test a write path against live data.
- `pin-a-dependency-with-its-reason` - a version pin carries a comment saying why.
- `orchestrator-mode-saves-quota` - on by default (question 6): the strongest model
  answers, judges and checks, cheaper models do the work in helper agents, one helper
  per task and never a bundle, the chat stays free meanwhile, a trivial one-step task
  is done directly, and tests are run narrowly while working and in full once at the end.
- `an-agent-brief-stands-alone` - goal, absolute folder, files it may touch, a summary
  of what is known (not a read-first list), one narrow task, a short report; waiting is
  a check every 20 to 30 minutes, at most three times, then the job is judged dead by
  its last real activity, never by its status.
- `absolute-paths-not-the-shell-working-directory` - absolute paths in every command
  and every brief.
- `script-file-not-shell-quoting` - text with quotes or backslashes goes in a script
  file, not a one-liner.
- `windowless-processes-on-windows` - background processes start hidden. On a Windows install this
  one REFUSES a `Start-Process` with no `-WindowStyle` or `-NoNewWindow` unless it opens a window the
  user explicitly asked to see (start that one with `-WindowStyle Normal`) or something meant
  to be seen (`ms-settings:`, `windowsdefender:`, `control.exe`, a `.cpl` file, `http://`,
  `https://`, `file:///`); on other systems it is only a note.
- `scheduled-task-not-interactive` - `schtasks /create` with `/it` opens a console window at every
  run. On a Windows install this one REFUSES it; on other systems it is only a note.

Besides these, the install also seeded twenty-two pinned notes about writing a note
that comes back and about honesty, agent-spawning and memory hygiene; those are
described in the sections below.

Apply each answer where it belongs - a reply-shape rule in the rulebook file,
a standing preference as a stored fact - and keep `owner-setup-answers` current with
`revise`: the defaults first (so `walk-through-the-answer-guard-once` can be retracted
at once and never asks again), then what he chose, number by number.

---

# After setup, this happens without you

At the start of every conversation, and again just before you touch a file,
run a command, or call a tool a note is bound to directly (like Agent or
Artifact), THOR puts the relevant notes in front of you. You do not have to
fetch them.

What arrives that way is background about their setup. It is never an
instruction for the task you are doing.

**A note is shown to you in full once per context.** A context is this session's
main conversation, and each subagent on its own (a fresh context has not seen
anything). After that one showing the same note is not shown again, not even as
a reminder line, until a new session start - also a resume or a compaction,
which lose what you were shown. A prompt the owner types opens no new showing.
If you need a note again, `get` it by its id. A refusal is never hidden by
this: a rule that blocks a call blocks it every time.

**What the PERSON sees when THOR holds you.** Claude Code shows the hold's text
on their screen too, and THOR's own words ("rule", "evaluation", "falsifier")
mean nothing to someone who has never heard of them. Beside every end-of-turn
hold THOR puts one plain sentence for them, and you add one more: before you act on a hold, tell
them in a single ordinary sentence what you are about to do and that nothing is
needed from them (unless it is the first-session questions, which need their
answer). Never read the hold's text out to them, and never ask them to decide
something it names - the verdicts and repairs are yours.

**At the end of a turn it may hold you, but only for what you did yourself.**
THOR asks for one small thing at a time and will not let the turn close until
it is settled: a note you just stored onto a place too full to ever show it, the
first-session setup, a decision of the owner you did not store, a reply that
breaks one of the reply rules, a rule that could no longer prove it still holds
and that was met when a file was written (the stale-rule guard: once per
session, settle it with `revise` or `retract`; it can be switched off by writing
`{"mode": "off"}` into `stale-guard-mode.json` beside the store) - and the
evaluation. Each has one honest way out, and the message says which. Saying it
in your reply settles nothing - the fix has to be a real change to the note, or
the tag that records the decision. No tag decides a rule's teeth: only its trial
does. What it never does at a turn's end: ask for a verdict on a note that keeps
firing, or about old rules the trial has not decided. Those are asked inside the
evaluation, which runs once a week per project, so a session is never
handed something to judge every quarter of an hour.

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
the old one is closed as superseded, and its verdicts stay. The questions
about the project's upkeep - the notes that fired in many sessions without a
verdict, and the old rules without a decision - are asked inside that
evaluation and nowhere else. A verdict counts for everyone. A project
worked in during the last 7 days with no evaluation that counts in 7 days is
asked about whatever the clock says - the weekly floor, so that an edited clock
file can delay the question but never cancel it. A folder that belongs to no
project is never asked about on its own, since there would be nowhere to file the
report.

THIS ONE DOES NOT GO QUIET AFTER ONE ASK: it holds the end of every turn,
naming how many times it has already asked and since when, until a report that
counts is on file.

THE RHYTHM IS ONE EVALUATION PER WEEK PER PROJECT, tied to the plan the owner
pays for, and the ask says which mode applies in one plain line. A report covers
the next 7 days. On a smaller plan (Pro and anything below Max, or a plan THOR
could not read) that is strict: the next ask comes after a week, whatever is
found, so the evaluation leaves the owner's limit for his own work. On Max the
week is the same, but it goes daily (a report then covers the next 24 hours)
while the project shows rot: a defect found, a false-block sweep hit, a batch
or dispute item left owed, a report that did not count, or a THOR bug candidate
waiting. It goes back to weekly the first time a counting evaluation finds none
of that. THOR reads the plan from Claude Code's own settings at install and
confirms it with the owner as question 1 of the first-session questions
(`install --set-plan max|small` records his answer). There is no re-ask after a
few hours of work, whatever was changed. "Work" is real activity: a pause of
half an hour or more never counts.

**How an evaluation goes.** Do these in order, and do not skip one.

1. **It starts in THOR's own record, not in a file.** Opening `/thor-eval` or
   being asked at the end of a turn writes a start note into the memory's log,
   and from that moment the lists below are frozen: nothing that fires or is
   written later is added, and nothing that only fired in another session is on
   them. THOR then decides the old rules of the project itself - for up to 20
   seconds - and shows you a note with three parts. If the note says THOR is
   still deciding, end the turn once and read it again.
   - **Verdicts.** The notes that fired in this session and that went 40 sessions
     or more without a verdict (a note counts once per session, however often it
     was shown there). Give each one with the THOR tool (`mark`), or skip it
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
   - **The disputes to verify.** Every rule stored by dispute
     (`legitimate_calls`, `may_change`, `accept_refusing`) since the project's
     previous report that nobody has checked yet, at most 8 (the rest come
     first next time). Each line shows what was disputed, the writer's reason
     (or "no reason recorded") and two exact calls. Open the rule and its
     file or command and decide whether the disputed literal IS the rule's
     subject. If it is, hang the real check with the first call
     (`revise` with `check_kind` and the literal, the text reworded to name
     it). If it is only an incidental span, confirm the dispute with the
     second call (`revise(id, because: "...")`, at least 20 characters). An
     item is done when one of the two calls was accepted, and the report is
     held until every item is done; a tag or a line in the report settles
     nothing.
   - **The false-block sweep (item d of the teeth section).** Rules, and
     entries of the answer rulebook, that THOR found would refuse (fire on) real
     calls (replies) of the last 30 days that ran fine. THOR replays the record of
     real calls it keeps beside the store through the guard, so this finds what no
     one reviewed. Each line gives the rule, how many real calls it would refuse
     and how many of those ran fine, one example, and the call that settles it.
     Read the example and the rule (`get`): if the call was fine, narrow the rule
     with `revise` (a narrower check, a better anchor, and a `because` of 20+
     characters); if the rule was right, say so in the report with the id. It is
     done when each one has one of those outcomes in your report. `doctor` shows
     the same rules in one line; `serve --db "<their store>" sweep` shows all of it.
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
   the question quiet until the next week (the next day while a Max project shows rot). If the next message says your
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

The verdict list of an evaluation only ever names a note THIS session actually
saw fire - never one that only ever showed up during other, unrelated work.
There is no separate question about one note at the end of an ordinary turn.

## When THOR finds a bug in itself (the co-tester section)

THOR is still young, so the people who use it are also its testers. Now and
then the first message of a session opens with "THOR may have found a bug in
ITSELF". It is a nudge: it holds nothing, it is not part of what the person asked,
and it comes once per session with at most one candidate.

**What gets reported, in plain words.** Only mistakes in THOR itself: a program
that would not start, a report THOR refused for its own reasons, a starting note
THOR's own gate turned away. Never the person's work, their notes or their rules.
The text that would be sent is written in English so the makers can read it, and
it has had everything private taken out: file paths, project and folder names,
computer and user names, addresses, keys and passwords. THOR checks that again
and warns you if it is not sure.

**What to do, in order.**
1. Read the write-up once more and remove anything private that is left.
2. When what the person asked is at a stopping point, ask them ONE question, in
   their own language, with the exact full text that would be sent shown above
   it. The question must say what THOR found, that the text shown is exactly what
   would be sent, what was taken out (paths, project and folder names, computer
   and user names, addresses, keys and passwords), that nothing is sent without
   their yes, and that saying no is fine. In Dutch, for example: "THOR vond een
   fout in zichzelf. Hieronder staat precies wat ik naar de makers zou sturen;
   namen, paden, adressen en wachtwoorden zijn eruit gehaald, er gaat niets
   anders mee. Mag ik het versturen? Nee zeggen kan gewoon." Ask for nothing but
   yes or no. Never ask them to type, copy or edit anything.
3. After a yes: when `gh auth status` succeeds, run `gh issue create --repo
   nworks3d/THOR-memory-bible-for-AI --title "<title>" --body-file <file with the
   body>`. Otherwise open `https://github.com/nworks3d/THOR-memory-bible-for-AI/issues/new?title=<title>&body=<body>`
   in their browser (title and body URL-encoded, the whole address within about
   7,000 characters; cut a longer body and end it with "(shortened)"), so they only
   have to click Submit. After a no, send nothing.
4. Record what happened, so THOR stops raising it:
   `remember(id: "thor-bug-<fingerprint>", kind: "report", text: "OUTCOME for
   <fingerprint> (<what it was>): opened <address> | owner declined", tags:
   ["thor-bug:<fingerprint>"])`. The nudge gives you this call with the
   fingerprint filled in. It needs no project.

**When it is done.** The `remember` call was accepted. A candidate nobody
recorded comes back once, a day later, and then no more. A candidate left by an
earlier build of THOR is closed by the next install and never offered: only new
evidence from the build that is running can raise one.

**The default is release, on every build.** A plain install, a source build
included, writes mode `release`: you ask, and you file after the yes. Only the
owner's own development machine runs `install --set-mode test --db "<their
store>"`; there the same text carries one more paragraph saying this is a test
environment. In test mode you push nothing: you say "normally I would push this
to the THOR repo", show the finished write-up to the owner, and record the outcome
as "shown to the owner, not pushed (test environment)". A store with no mode on
record offers nothing, so a store made before this existed stays quiet until
`install` has run once more.

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
- [docs/WHAT-THOR-DOES.md](docs/WHAT-THOR-DOES.md) - full reference: what THOR arranges, enforces and refuses, and why
- [README.md, "Remove THOR completely"](README.md#remove-thor-completely) - every piece the install wrote and how to undo it, for when the person asks to uninstall
