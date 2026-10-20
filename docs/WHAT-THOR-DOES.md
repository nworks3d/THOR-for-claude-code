# THOR: everything it does to, asks of, or refuses a user and their agent

Version described: 2.5.0 (read from the repo at commit 008df2a, 2026-10-09). Source of truth for each
item: the code first, the docs second. The code wins wherever a page and the code ever disagree.
This page is the agent-facing reference; the setup steps are in AGENTS.md.

How to read an item. Every item has four lines:
- What: one plain sentence.
- Why: the real reason, with the incident or measurement where the docs or the code give one.
- You notice: what the user actually sees or feels.
- Refuse or change: whether the user can say no, and how.

Words used once, in plain language:
- "Hook" = a small automatic step Claude Code runs at fixed moments (session start, before a tool runs,
  when you send a prompt, when the assistant finishes a reply). THOR is wired in through four of them.
- "Note" = one remembered fact. A "rule" is a note that comes back at the right moment. A rule can carry
  a "check" = a small test THOR can run right now (for example "this file still contains that line").
- "Place" = the file, folder or command a note is tied to. "Binding" = when or where a note comes back.
- "Falsifier" = one sentence saying what would prove the note wrong.
- "Scope" or "project" = the folder-sized area a note belongs to.
- "Working copy" = the one folder THOR tries a rule's file checks in.

---

## A. What the install arranges

### A1. It downloads, checks and unpacks THOR
- What: install.ps1 (Windows) or install.sh (Linux) downloads the latest release, checks it against its
  published checksum, unpacks it into a "thor2" folder in the home folder, then runs the setup program.
- Why: one command, so nothing is skipped. It refuses to install an archive whose checksum does not
  match ("Nothing was installed").
- You notice: a few lines of plain output. On Windows the program files are un-blocked so Windows does
  not warn on each start; if Microsoft's C++ runtime is missing, the script stops and prints the one
  winget command to fix it.
- Refuse or change: set THOR_HOME to unpack elsewhere; set THOR_NO_SETUP=1 to unpack only and run the
  setup yourself later. Reading the script first is encouraged (README.md:391-394).

### A2. The agent checks the machine for git, Python 3 and the C++ runtime
- What: when the AI assistant does the install, it first checks git, Python 3 and (Windows) the C++
  runtime, and installs only what is missing through winget, brew, apt or dnf.
- Why: git reads a project's code and keeps that reading fresh after each commit; Python is used by the
  helper scripts assistants write for a person's work. The aim is zero typing by the person (AGENTS.md:62-88).
- You notice: at most one system permission prompt (winget or sudo). That click is yours.
- Refuse or change: you can decline the system prompt; then the assistant stops and says so. Rust is
  not needed for the normal install.

### A3. It creates the memory file, once
- What: a single file (thor.db) in the per-user data folder, plus small helper files beside it. An
  existing memory is never touched or re-seeded.
- Why: a memory inside a build folder is one "cargo clean" from gone (bin/install.rs:68). Seeding an
  existing store would push notes into someone's real work uninvited.
- You notice: a line "created a memory at <path>".
- Refuse or change: --db puts it elsewhere. Nothing leaves your machine.

### A4. It wires four hooks into Claude Code's settings.json
- What: SessionStart, PreToolUse (every tool), UserPromptSubmit and Stop all run the same "serve hook"
  command from the THOR folder.
- Why: without them the memory never speaks and nothing announces it. This was caught on 2026-08-03,
  when the hooks had --db in the wrong place and every hook would have exited with an error silently.
- You notice: "connected THOR to Claude Code". settings.json is first copied to settings.json.bak (only once: an existing .bak is never replaced, so it keeps your file as it was before THOR), nothing
  that is not THOR's is ever removed, a second run changes nothing, and a hook that points at a program
  that no longer exists is repointed (reported) instead of duplicated.
- Refuse or change: --settings writes to another file. Removing the four entries from settings.json
  switches THOR off completely (my reading of what the install writes; no doc says so in one place).

### A5. It refuses to install hooks that cannot work
- What: before writing hooks it checks that the "serve" program exists and actually starts
  (serve --version). If not, nothing is set up.
- Why: a hook pointing at a missing or blocked program fails open: the assistant carries on and THOR is
  silent forever. This used to be a warning; since 2026-08-14 it is a refusal because the warning hid in
  the middle of success lines (bin/install.rs:408-418). Windows Smart App Control and antivirus can block
  unknown programs.
- You notice: "NOTHING WAS SET UP" with the one reason. THOR never changes your Windows security settings.
- Refuse or change: no override on purpose; allow the program in Windows Security and rerun.

### A6. It registers the "note-saving" connection (tool server)
- What: it adds an entry called "thor" to ~/.claude.json so the assistant gets sixteen tools: fourteen for
  the work memory (remember, revise, retract, resolve, pin, unpin, get, lookup, history, mark, status,
  search_code, where_used, outline) and two for the personal library (library, shelve).
- Why: without it the assistant could read the memory but never write to it.
- You notice: "gave your assistant a way to save notes". Both config files are backed up first.
- Refuse or change: --no-mcp installs a read-only memory on purpose; --mcp-json writes to a project's own
  .mcp.json. Takes effect only after a restart.

### A7. It points git at a shared set of hooks for every repository
- What: it writes small dispatcher scripts for nine git events into a folder beside the memory and sets
  git's global core.hooksPath to it. After each commit the one for post-commit refreshes the project's
  code reading. Every dispatcher then runs that repository's own hook, so nothing already there stops.
- Why: an index three days behind made 83 percent of "who uses this" answers point at a wrong line,
  against 0 percent when fresh (githooks.rs:3-14). A naive hooksPath would silently switch off a
  repository's own pre-commit gate, so every dispatcher hands control back.
- You notice: two lines ("told git to use them", "your own git hooks keep working"). If git is not
  installed this step is skipped with a friendly line.
- Refuse or change: if you already had a global core.hooksPath, THOR leaves it alone and says code
  search will not auto-refresh until you sort it out.

### A7b. The pre-commit one of those hooks stops video files and very large files (the media gate)
- What: before it hands control to the repository's own pre-commit hook, the dispatcher looks at what is
  staged right now and refuses the commit when a staged file is a video (ending mp4, mov, mkv, webm, avi
  or m4v, capitals included) or is larger than 25 MB (26214400 bytes). It reads the index, so it also
  catches `git add -A`, `git add .` and `git commit -a`, which stage files the command never names. It
  prints every refused file with the reason (video type, or its size against the limit) and the steps to
  take: unstage it with `git restore --staged "<file>"` (`git rm --cached` in a repository with no commit
  yet), keep it outside the repository, commit again. Nothing is committed and no file on disk is touched.
  A path with the git attribute `thor-media-ok` set (a line `<path> thor-media-ok` in .gitattributes, the
  path in double quotes when it has a space) is let through. The settings are plain git config, global or
  per repository: `thor.mediagate.enabled` (`false` switches the gate off), `thor.mediagate.extensions`
  (the video endings, space separated, default `mp4 mov mkv webm avi m4v`), `thor.mediagate.maxbytes`
  (a plain number of bytes, default 26214400) and `thor.mediagate.parkdir` (optional: a folder the refusal
  suggests parking the file in, as `<parkdir>/<YYYY-MM-DD>_<repo>_<subject>/` with a STATUS.txt).
- Why: a 26 MB 4K video went into a repository with `git add -A` and was pushed. A rule about command
  text cannot see what `git add -A` stages, and git keeps every version for good, so a file added once
  makes the repository heavy forever. The check costs about seven short processes per commit (one
  `git cat-file --batch-check` reads every size at once).
- You notice: nothing on a normal commit; on a refusal a message in plain words that names the files. It
  never mentions how to skip the hook. Limits on purpose: a removed file is never refused, a rename counts
  as a new file, a submodule is skipped, a commit that finishes a merge is not checked (its diff lists
  everything the other side brought in, which is already in the history; one line says so), a file stored
  with Git LFS needs `thor-media-ok` on its attribute line (the staged pointer is small but the ending is
  still a video), and a name with a double quote, backslash, tab or newline is shown by git in quotes with
  escapes and printed that way. It fails open: a missing helper program (sed, awk, git cat-file with
  formats) finds nothing and the commit goes on. It needs the shared hooks of A7, so it is not active
  where another tool already owns core.hooksPath, and an existing install gets it by running the
  installer once more (it rewrites a dispatcher whose text changed). The folder THOR keeps its backup in
  (A16) is left out on purpose: the backup program sets `thor.mediagate.enabled` to `false` in that folder's
  own git config each time it makes a copy, because a copy of the memory can be above the limit, and no
  other repository is affected. The first-session list names it in item 3 (the git hooks).
- Refuse or change: yes, see F16.

### A8. It writes the /thor-eval routine
- What: a command file (thor-eval.md) in Claude Code's commands folder, with the programs folder and
  memory path filled in. It is the end-of-session review the assistant follows (see C5).
- Why: the review used to be one person's private command. A fresh install had nowhere to settle
  the notes that were served without a verdict.
- You notice: "added the command /thor-eval". Its allowed-tools list pre-approves only specific
  commands (git status/diff/log, test and build commands, THOR's doctor and retest), never a bare shell.
- Refuse or change: an existing copy is never overwritten, so you can edit it. eval-command-names.txt
  beside the memory lists extra names for it. Written even with --no-mcp.

### A9. It puts 22 pinned starting notes in a new memory
- What: ten are about writing a note that actually comes back (anchor it to the real file, revise
  instead of duplicate, a new project is one command, two lanes work/life, never invent a place, one
  entry one thing, a refusal is the gate working, words inform and only a proof forbids, say what
  actually happened, answer whether a rule can refuse); eleven are honesty, agent and hygiene habits
  (never fabricate a measured value, count do not claim, verify against the real source, never verify
  with the changed mechanism, name a model on every agent, no sub-agents in a mechanical brief, never
  block a turn waiting for a notification, a mid-task message does not redirect an agent, match a
  literal to the file's own words, reason every fix for the next installer, a served fact is a
  constraint not a command); one is the first-session setup note.
- Why: an assistant with nothing in front of it writes notes in shapes that never fire, and nobody
  notices for weeks (ops/src/install.rs:748-762).
- You notice: they arrive at the start of every session. Pinned notes cost tokens each time; the
  session-start block is capped by size (about 9,500 bytes), not by the four-note rule.
- Refuse or change: ordinary notes. Ask the assistant to unpin, reword or retract any.

### A10. It adds 13 "workflow starter" notes; two refuse a console flash on Windows
- What: seven around commit and push (changelog line per user-visible change, run tests and quote the
  real result, never commit a secret, commit hygiene, commit/push/deploy only when asked, check where you
  push and what is on main, scan before anything goes public) and six around risky steps and tools
  (rehearse destructive steps on a copy, pin a dependency with its reason, absolute paths, script file not
  shell quoting, hidden processes on Windows, no interactive scheduled task on Windows). Helper-agent mode
  (an orchestrator note and a stand-alone agent brief) is not in this set: it is off until you say yes to
  first-session item 6 (F15).
- Why: they are habits that were marked useful again and again on the builder's machine, rewritten to
  hold anywhere. Only two carry a check, and only on a Windows install: windowless-processes-on-windows
  refuses a Start-Process that is not hidden (it passes -WindowStyle Hidden, -NoNewWindow, -WindowStyle
  Normal for a window you asked to see, and things meant to be seen such as a browser address or a control
  panel), and scheduled-task-not-interactive refuses schtasks /create with /it. Both exist so an agent
  never flashes a console window. All the others stay plain notes: seven other candidate checks were
  replayed over 145,976 recorded calls and every one refused legitimate work (for example a --no-verify
  ban refused 11 legitimate commits), so a new user never gets a false block from a note they did not
  write (ops/src/install.rs, the TEETH paragraph of workflow_starter_pack).
- You notice: each arrives right before its step (commit, push, delete...), once per
  conversation. Apart from the two Windows teeth they can never refuse anything. A changelog is only
  created when you said yes to first-session item 12. Commit/push/deploy/install/delete moments are covered.
- Refuse or change: tell the assistant to remove any; it retracts them with a reason.

### A11. It writes a reply rulebook with six rules
- What: guard-response-rulebook.json beside the memory, only if none exists. The six: open result-like
  replies (md5, commit, test result...) with a plain summary (from 600 characters), do not ask the user to check what you can check
  yourself, no reflexive disclaimers, answers over 1000 characters are held back and shortened, and a
  claim of "checked" needs a real look this turn, and a claim that EVERYTHING was checked ("alles nagekeken",
  "everything checks out") is held unless it says what it covered. All six block when they fire (no tier means block).
- Why: so the end-of-reply check has something to check from the first session instead of silently
  checking nothing. The content is a neutral example, not the user's choice.
- You notice: see C4. Some trigger words are Dutch ("gepusht", "kun je checken").
- Refuse or change: edit a rule's reminder line, delete an entry or the file to switch it off. Never
  overwritten once present.

### A12. It settles the bug-reporting mode
- What: writes thor-mode.json = "release" (on every build) unless a mode is already set, and closes bug
  candidates left by an earlier build.
- Why: THOR finds some bugs in itself and may offer one per session to be written up for the public
  THOR repository (see E9). With no mode on file it offers nothing at all.
- You notice: "if THOR finds a bug in itself, your assistant may offer to report it... nothing is sent
  without your yes".
- Refuse or change: install --set-mode test only shows the write-up and pushes nothing (the
  maintainer's machine); no mode file = no offers.

### A13. From inside a git project it reads the code and opens the project
- What: reads the project's source once into a code index (so search_code, where_used and outline
  work), records the project in the memory, and registers this checkout as the project's working copy.
- Why: so the first note of a project is never refused as an unknown collection (measured 2026-08-18
  and 2026-09-09: a newcomer who followed the page got a silent half-install), and so a rule's file check
  has one folder to be tried in.
- You notice: "read this project's code, N file(s)" and "+ registered its working copy: <folder>".
  Idempotent: a second run reads nothing again.
- Refuse or change: no code reading outside a git folder (a plain folder becomes a topic, A15). Without
  git on the machine this step is skipped.

### A14. It names the project after the folder, and will not rename it
- What: the project is named after the folder (or the first line of a .thor-project file). --project
  writes that file to give another name.
- Why: renaming a project's scope would strand every note filed under the old name, invisibly.
- You notice: nothing, unless you pass --project; then a refusal if the folder already has another name.
- Refuse or change: only the owner names a project; the assistant may not invent one (see B2).

### A15. A topic folder for a subject, not for code
- What: "install --topic" run inside a plain folder (a kitchen renovation, a thesis) gives it its own
  memory. It adds one hidden file, .thor-project, and touches nothing else. No git needed.
- Why: not everyone is a programmer. Everything said in that folder is remembered for that topic only.
- You notice: "now a topic". Code search is not available there; the assistant reads the files itself.
  A file check on a rule still works against the folder's real files.
- Refuse or change: your home folder, Documents itself and a whole drive are refused as too broad.
  "install --register-root" does only the registration.

### A16. Backup is a first-session choice (item 10); the install arranges none
- What: THOR has a backup program (backup.exe) that exports the memory to a git repository, with a
  remote called "nas" preferred, and doctor reports where the last copy went. A fresh install makes no
  backup and wires no hook for it: nothing is backed up until you choose, at the first session (item 10)
  or later. The choices are install --set-backup local|<place>|off. "local" is a git folder beside the
  memory (backup-repo) with no remote, so nothing leaves the machine; a place (a NAS folder, another
  computer, a private repository) becomes a remote called "nas"; "off" removes only the one
  SessionStart hook that runs the backup, and copies already made stay. Choosing wires that hook
  (it runs at session start, at most one copy a day) and makes a first copy at once to prove it works.
  The backup folder is exempt from the media gate (A7b): a copy of the memory is cut into files of up to
  50 MB, and the gate would refuse them, so the folder's own git config gets `thor.mediagate.enabled=false`
  when you choose and again before every backup commit.
- Why: the log is append-only and a lost file is gone. On 2026-10-01 a backup silently failed for days
  because the log passed GitHub's file limit.
- You notice: nothing until you choose, because no hook or schedule for it is written by the install.
  After you choose, one line says the first copy worked, or says plainly that it failed and why.
- Refuse or change: do not choose, or choose "off" (install --set-backup off). See "Backup" in AGENTS.md;
  restore with backup --restore-from.

### A17. It asks for one restart and a first conversation
- What: Claude Code reads hooks and tools at startup only, so the person must close it fully and reopen.
  The first session then finishes setup on defaults (see C1).
- Why: until the restart nothing installed is running.
- You notice: one sentence from the assistant. It is the one thing the install cannot do for them.
- Refuse or change: no. Until you restart, THOR is inert.

### A18. Optional: a read-only copy on a second machine, of everything or of a selection you choose
- What: only when you ask for it, with install --copy-from <your backup> (or the same values in the
  environment variables THOR_COPY_FROM, THOR_COPY_SCOPES, THOR_COPY_GLOBAL, THOR_COPY_WHOLE that the one-line download
  passes on). Instead of making a memory of its own, the install builds a read-only copy from your
  backup, writes a small marker file (thor-copy.json) beside it that makes every hook read-only, connects
  the four hooks and the reply-tidying file, and registers nothing that could write: no note-saving tool,
  no machine-wide git hooks, no /thor-eval. You can name the projects the copy holds (--scope, repeatable)
  and whether it holds the general rules (--include-global); with neither you get the project of the folder
  you ran it in, and --whole-memory (or THOR_COPY_WHOLE=1 in the one-line download) copies everything and
  wins over a project choice, so use it only on a computer you trust, never a work or company laptop. The items of every other project, the personal
  library, refusal records and the "where it fired" text of served records are not in the copy file at all.
  A selective copy has a hash chain of its own, so doctor can verify it. The choice is remembered in the
  marker and repeated by every later refresh (sync pull). The refresh runs by itself at session start, at
  most once an hour, builds a new copy beside the old one and swaps it in only when it is complete.
- Why: a laptop, possibly a company's, should get the part of the memory it needs and nothing else, with
  no way for its hooks to fork the one log.
- You notice: a long summary at the end of the install, "This machine runs a copy of the memory ... It holds
  only: ..." at each session start, and a "copy:" line in doctor with the selection, the age of the last pull
  and the number of items. Doctor warns when the last successful pull is older than three days, when the
  last refresh failed, when a changed selection is not applied yet, and when the backup source is a git
  clone on the same machine (it holds the complete memory).
- Refuse or change: do not use the flag. To go back to the whole memory: sync pull --whole. To stop being
  a copy, delete the folder of the copy and follow "Remove THOR completely" in the README. An existing
  memory that is not a copy is never replaced.

---

## B. What THOR requires or enforces when notes are written

### B1. Projects are scoped by folder name, automatically
- What: every project has its own memory; one project's notes do not appear in another.
- Why: without scoping every search competes with projects you were not asking about, so THOR gets worse
  the more you use it (README.md:489-492).
- You notice: nothing; notes just stay in their project.
- Refuse or change: --project / .thor-project overrides the name; it refuses to change an existing name.

### B2. Nobody but the owner opens a new project, shelf or place
- What: a write that would open a new collection is refused unless the owner named it; the assistant
  must repeat the owner's exact word (new_collection_named_by_owner). Library shelves work the same.
- Why: twice in two days (store of the owner) a personal register was filed under a name nobody chose,
  and a reminder in a tool description did not change that; a gate did (mcp/src/lib.rs:538-550).
- You notice: the assistant asks "what should the new one be called?" instead of filing it.
- Refuse or change: you can name it; you cannot make the assistant skip the question.

### B3. One working copy per project
- What: a project is registered at exactly one checkout. A second copy (a clone) is refused, naming the
  registered folder. A registration moves only when the old folder no longer exists.
- Why: file checks must be tried in one known folder, not in whatever folder the session stands in.
- You notice: a refusal that says "go to that folder" or "moving a registration is the owner's call".
- Refuse or change: remove or rename the old folder yourself, then rerun. No assistant can move it.

### B4. A rule must be able to come back (binding required)
- What: a rule or orientation needs a moment (an action like push), a target (a real file, command,
  host...) or "always". Refused: no binding; only a symbol (a function name) as the anchor; wildcards,
  "." or bare names like main.rs; a costly or irreversible rule bound only to a project name; a NEW
  binding to the moments "answer" or "claim_done", which nothing fires.
- Why: a rule that cannot fire is invisible from every surface. Measured: 1794 of 2369 fireable items had
  never fired, 1067 anchored to a symbol only; 13 weighted project-only items, 6 never served.
- You notice: a refusal that says exactly what to change.
- Refuse or change: no override; bind it elsewhere or pin it (pinned notes cost tokens each session).

### B5. A rule needs a falsifier
- What: one sentence naming what would prove the rule wrong. Without it the write is refused.
- Why: rules never expire, so this is the only thing that ever says one went stale.
- You notice: a refusal asking for it.
- Refuse or change: no. (Archive material such as reports does not need one.)

### B6. At most 300 characters, no line numbers, no expiry, no source file in a global rule
- What: a rule is at most 300 characters; its text may not name a line number; a rule never carries an
  expiry date; a rule with no project may not name or anchor to a source file.
- Why: four notes of 300 characters is the 1200-character block budget. Line numbers were wrong within a
  week in 3 of 3 cases. A global note naming guard.rs was handed to unrelated projects (the owner's store held 421 global
  rules; 27 were anchored at a source file).
- You notice: the refusal says how far over you are and offers the cheapest cut (reworded after about
  17 rewrites in one day, gate.rs:1145-1154).
- Refuse or change: no; long reasoning goes into a separate Report.

### B7. A check's literal must be named in the rule's own text
- What: if a rule carries a check on a text fragment (a "literal"), the rule's words must contain that
  fragment. A non-ASCII glyph is named by its code point (U+2014 for an em dash). THOR also builds checks
  from the rule's own text (a flag in backticks after a command); a rule whose own words make a check that
  bites is refused until that check is hung.
- Why: a check has to say in its own words what it refuses, or nobody reading the rule knows what it does.
- You notice: a refusal such as "Hang it: check_kind forbidden, check_literals ['--force']".
- Refuse or change: only via the three dispute arguments (B10); never widen a rule to catch more.

### B8. A place shows at most four notes (the four-per-place limit) and three exits
- What: a file, folder or command shows at most four notes in one block, 1200 characters in all. Writing
  a note onto a place that already holds as many as can ever show is refused (heavier rivals, and lighter
  ones too). The refusal lists every occupant, its weight and whether it can refuse, and three exits: fold
  two that say the same, move one to the narrower place it is really about, or retract a stale one with a reason.
- Why: measured on the owner's store: the worst place held 38 notes for 4 seats, and 239 of 698 eligible
  notes were never shown anywhere (CONTRACT.md R1/R2). A memory fills with advice nobody sees.
- You notice: the assistant has to tidy before it can store. It is never turned into a question for you.
- Refuse or change: a true check is never thrown away to get past it. A "crowded-on-purpose" tag keeps a
  deliberately crowded place (eval-command.example.md:590-593).

### B9. A repeat of an existing note is refused
- What: a note whose text is a near-copy (word overlap 0.8) of a live one, or whose key equals an existing
  key, is refused: revise the existing one.
- Why: two copies take two of the few seats and the older one keeps being served.
- You notice: "this is a near-duplicate of <id>; revise it".
- Refuse or change: no.

### B10. Disputes and weakening need a written reason
- What: to answer a refusal that is right about the check but wrong about your rule you may use
  legitimate_calls, may_change or accept_refusing, each only together with "because" (one sentence,
  at least 20 characters). Weakening a rule that has teeth (clearing or changing its check, lowering
  severity, dropping a binding, narrowing its reach, removing a flag from its text, turning it into a
  report) also needs a because, as does retracting.
- Why: a wrong dispute turns a rule that bites into a note that can never refuse, hidden until someone
  audits: 7 of 11 disputes saved on 2026-10-07 were wrong.
- You notice: the write is refused without a reason; the reason is kept in the item's history and shown
  in doctor and in the evaluation.
- Refuse or change: no override; the reason is the price.

### B11. A check that would refuse too much is refused
- What: a check that would refuse more than half of at least five real earlier calls it reaches is
  refused as too broad, listing those calls. accept_refusing names one of them when refusing them is the
  point (a "never push until I say" ban).
- Why: a rule that blocks legitimate work is the most expensive thing this system can do.
- You notice: a refusal with the real calls listed.
- Refuse or change: narrow it, or accept_refusing with a reason.

### B12. No secret tokens in a note
- What: the text and falsifier of every note are scanned for token shapes (Stripe-style keys, GitHub
  tokens, Slack, AWS keys, tskey-, sk-ant-, AIza, JWTs, PEM private-key headers) with minimum lengths.
  A hit is refused, showing the first six and last two characters only.
- Why: the log is append-only and the backup copies every write, so a stored secret can never be removed,
  only rotated. Write where the secret lives instead.
- You notice: a refusal that says to rotate it if it was real. Doctor warns if an old note already holds one.
- Refuse or change: no override at all. Passwords, "key" as a word and commit hashes are not scanned.

### B13. Notes that contradict a live note, or sit next to a note whose proof has gone false
- What: a new check that demands the exact opposite of a live check on the same file is refused; adding
  a note to a place that holds a note whose own proof now comes out false is refused until that is settled.
- Why: contradicting checks make a memory untrustworthy; stale proof next to new notes is how a memory
  stops being worth trusting.
- You notice: a refusal naming the other note. Only the place you touch, never a backlog elsewhere; never
  on a correction or a removal.
- Refuse or change: settle the other note first (revise or retract with a reason).

### B14. Every rule is tried the moment it is written: RULE, NOTE or REFUSED
- What: remember, revise, pin and unpin run each rule's check (and any check its words imply) through
  the same guard the hooks use. RULE = the check refused the forbidden call and let ordinary ones
  through, so it is stored with its check. NOTE = no check holds, so it is stored without one and can only
  inform (most rules end here). REFUSED = nothing written, one fix named.
- Why: the earlier "answer this question or add a reason tag" gate was removed on 2026-09-28; the trial
  decides. Words alone can inform, never forbid. Only a check that runs and holds right now may block.
- You notice: the reply says which of three happened and "teeth: wired, not proven" or "proven guard".
  "Proven" needs evidence from reality (a verified real refusal, or the literal in the committed file).
- Refuse or change: no.

### B15. A file check needs the project's working copy
- What: a check on a file is tried in the registered working copy. Written inside the project before
  it is registered, the rule is refused with the one command to run. Written from elsewhere, it is stored
  as a note with the check pending (doctor lists it; "serve retest --id <id> --apply" tries again).
- Why: THOR will not trust a check against a random folder.
- You notice: a refusal naming install --register-root.
- Refuse or change: register the project (A13, B3).

### B16. The owner's go is the owner's to type
- What: a rule tagged owner-approval is answered only by the owner's own message in this or the
  previous turn. The assistant writing the word in its own call, a helper's report or quoted text does
  not count; if the transcript cannot be read, it refuses (fail closed).
- Why: so a command like "send the e-mail" or "touch the live server" cannot be self-approved.
- You notice: the assistant stops and asks you to say the go word.
- Refuse or change: you say it; nothing else works.

### B17. Reports, open points and the first-session note are protected
- What: an evaluation report can only be filed through THOR's own tool in the session that ran the
  evaluation; its machine-written part cannot be edited. An agent cannot file, tag or remove an "open
  point". The first-session note cannot be retracted until the owner's answers are on record. Reports and
  other archive material must have a project (except the setup answers and bug outcomes).
- Why: a report that decides whether a turn may end must be read from the log, not taken on the agent's
  word. A report with no scope falls on a pile nobody opens (603 of 2134 on the owner's store).
- You notice: refusals naming what to do instead.
- Refuse or change: no.

### B18. A rule about what the assistant says cannot carry a proof
- What: the gate watches files and commands, and an answer is neither. Such a rule belongs in the reply
  rulebook, tagged answer-guard:<entry-id>.
- Why: only a check can refuse; words in a reply cannot be checked by the file gate.
- You notice: the rule stays a note.
- Refuse or change: add an entry to guard-response-rulebook.json.

### B19. Writes on a read-only copy are queued, not applied
- What: on an optional replica (a copy on a NAS or server), every write tool answers "queued for the main
  machine"; the main machine applies it later and the write gate may still refuse it.
- Why: if the copy wrote to its own log, the two logs would fork, with no way back except rebuilding.
- You notice: "queued" means success; do not retry.
- Refuse or change: not installed by default.

---

## C. When THOR holds the assistant at the end of a turn

Common rules for all holds: a hold applies once per turn (a retried stop in the same turn is let
through; the evaluation ask is the one exception, see C5); it is never applied to a subagent's stop; and Claude Code shows the person one plain
sentence beside it ("THOR asks your assistant to do a short tidy-up... Nothing is needed from you").
The assistant should say in one sentence what it is doing (AGENTS.md:633-640).

### C1. First-session setup, on defaults
- What: while the seeded setup note is live, every turn end is held until the assistant has told the
  owner in one sentence the defaults (short plain replies in his own language; work lane only; every
  starting note and example rule kept; projects named after their folder; every default on except
  helper agents and backup, which stay off until he chooses), stored them as
  "owner-setup-answers" (a Report, no project needed), and retracted the setup note.
- Why: a seeded sentence in a note was the entire enforcement before and was skipped. It is not an
  interview: "just do it", or no answer, is answered by the defaults.
- You notice: one sentence from your assistant in the first reply; nothing to answer.
- Refuse or change: say what you want different; "I am not interested" is itself a recorded answer.

### C2. A decision of the owner that was not stored (capture guard)
- What: when your own typed prompt contains a firm-decision phrase ("from now on", "never again",
  "voortaan", ...), the end of the turn is held once, asking the assistant to record it with remember or
  revise unless it is not new.
- Why: a durable decision stated by the owner must not end the turn unrecorded.
- You notice: the assistant saves what you decided (or says in one line it was not new).
- Refuse or change: guard-capture-mode.json: "off", "self" (default) or "judge". At most one hold per
  flag. In the default mode the wrong-sink check (a write to CLAUDE.md, BRAND.md, AGENTS.md or a
  decisions/ folder while a decision is owed) only warns; it blocks only when the judge mode ruled.

### C3. A note stored onto a place too full to ever show it
- What: if THIS session wrote a note onto a place that cannot show it, the turn is held until the
  assistant folds, moves or retracts (B8), or marks the place crowded-on-purpose.
- Why: the person who wrote it still knows what it was for; tomorrow nobody does.
- You notice: a tidy-up before the turn ends.
- Refuse or change: only for the session's own writes; no override.

### C4. A reply that breaks a reply rule (answer guard)
- What: after each reply, a text check against guard-response-rulebook.json; if a rule fires, the
  assistant is nudged to fix the reply before it goes out. A rule may be block or warn.
- Why: so untidy replies are fixed before you read them. Rules about "how a reply reads" step aside for a
  subagent's reply, which you do not read.
- You notice: the assistant shortens or adds evidence; the length hold quotes your own request back so every item you asked for is kept.
- Refuse or change: edit or delete entries; a malformed rulebook blocks nothing (fails open).

### C5. The evaluation: when it is asked and how often
- What: per project, at a turn end, THOR asks for the full /thor-eval review when: the project has had 60
  minutes of work since the last valid review (all sessions added, parallel minutes counted once); this
  session has done 10 minutes of its own work there; it is not this session's first turn end there; no
  other session is running that project's review (active in the last 30 minutes). The rhythm is one
  evaluation per week per project, tied to your plan (first-session item 1): on a smaller plan (Pro and
  anything below Max, or a plan THOR could not read) a valid report covers the next 7 days, whatever is
  found. On Max it is the same week, but it goes daily (a report covers the next 24 hours) while the
  project shows rot (a defect, a false-block sweep hit, an owed batch or dispute item, a report that
  did not count, a THOR bug candidate), and back to weekly once a counting evaluation finds none. There
  is no re-ask after a few hours of work. Any project worked in during the last 7 days with no valid
  review in 7 days is due regardless.
- Why: owner's decision of 2026-10-02: one block, inside the evaluation, instead of something to judge
  every quarter of an hour; and the schedule follows the plan you pay for, so the review leaves your
  usage limit for your own work. A fact counts once per session toward the 40 that make a verdict owed.
- You notice: the turn will not end until a valid report is on file; the message counts how many times
  it has already asked and says which mode applies. A short task is never held over old rules it did not
  touch.
- Refuse or change: install --set-plan max|small records your plan; install --set-weekly-check off stops
  the ask (you can still type /thor-eval yourself). The only way out of a held turn is a valid report.
  Folder with no project: never asked.

### C6. What happens inside an evaluation
- What: THOR writes a start record in its log and freezes three lists: verdicts (notes that fired this
  session and went 40 sessions without a verdict), a batch (at most 8 rules only an agent can fix) and
  an A4 list (at most 16 that only need to name their literal), plus disputes to verify (at most 8) and
  the false-block sweep. THOR first tries the old rules itself for up to 20 seconds. The assistant gives
  verdicts with the mark tool, makes the named fixes, ends the turn once, then files the report.
- Why: verdicts given by script or another session settle nothing (205 verdicts in four minutes were
  given blind on 2026-10-01 and three were wrong). Only a report filed through THOR's own tool, validated
  at the next stop, counts.
- You notice: a few minutes of tidy-up. Nothing is needed from you.
- Refuse or change: see C5. If Claude Code changes the format of its session record, THOR cannot check
  filings, asks for none, and says so on every turn until THOR is updated.

### C7. Open points and the owner's queue
- What: for things only the owner can lift (a project without a working copy, a mid-change checkout, a
  full place), THOR writes and closes its own "open points". Ones still open after a valid review go to
  the owner's queue; over 8 per project every new session there starts with a banner.
- Why: so the assistant is not asked to do what only you can do.
- You notice: one line at session start with a count.
- Refuse or change: you resolve the cause (register the folder, make room).

### C8. The stale-rule guard
- What: once per session, if a rule that could not prove it still holds was met when a file was
  written, the turn is held once to settle it (revise or retract that rule).
- Why: a rule that can no longer prove itself should not quietly go on being wrong.
- You notice: a single tidy-up line.
- Refuse or change: stale-guard-mode.json {"mode": "off"}.

### C9. Script files left outside any git repository (only when a tools folder is named)
- What: only when the owner has named a tools folder (F17). Before a Write, Edit, MultiEdit or
  NotebookEdit tool call runs, THOR notes the file when it is a script (.py .ps1 .psm1 .mjs .cjs .js .ts
  .sh .bat .cmd .vbs) that lies outside every git work tree, outside the tools folder and outside the
  temp-until-live folder. The note is kept per session in leftover-scripts.json beside the memory (a
  session's notes are forgotten after 14 days). At a turn end THOR lists the noted files that still exist
  and were last changed more than 30 minutes ago, and holds the turn once: move each reusable one into a
  new sub-folder of the tools folder with a README.txt whose first line says in one sentence what the tool
  does, and delete the one-offs. Each file is asked about once per session, so a later turn end never
  holds for it again. Never for a subagent's stop (but a subagent's writes are noted for the main session),
  and never on a copy of the memory, which takes no state write and so could not remember having asked.
- Why: a helper script left in a scratch folder is found again by accident or never, and the next session
  writes the same script a second time.
- You notice: a single plain sentence from Claude Code ("Your assistant left a few helper scripts outside
  your projects... Nothing is needed from you"). Not covered: a script made by a shell redirect
  (echo ... > x.py) never passes through the file tools and is not noticed.
- Refuse or change: install --set-tools-folder off --db <memory>.

---

## D. What THOR refuses while the assistant works

### D1. A forbidden fragment in a command
- What: a rule with a "forbidden" check refuses a command containing its fragment as whole words
  (--force does not match inside --force-with-lease), in any part of the call: after cd, on a later
  line, inside ssh "...", bash -c, $(...), behind a pipe, past sudo or timeout. Text that only mentions
  the command (echo, grep, a heredoc, a comment) is not a call.
- Why: a wrong change that does not land is the whole promise.
- You notice: the tool call is refused with "[THOR]", the rule id and the fragment.
- Refuse or change: remove or revise the rule (with a reason). A part of a call that runs in another project's folder
  (`cd ../other && git push`) is judged by that project's rules.

### D2. A forgotten required part (requires)
- What: a rule can demand that a call also contains one of a set of answers (for example every Agent
  call names a cheap model). A trigger may be a literal inside the command; end it with * for a family.
- Why: forgetting leaves no text to look for, so "forbidden" cannot see it.
- You notice: the call is refused naming what is missing.
- Refuse or change: same as D1.

### D3. A file that must keep or avoid some text (contains, absent, absent_all, path_exists)
- What: a rule can block an edit that removes a required line or adds a banned one, and also a shell
  command that deletes, empties or overwrites the file (rm, truncate, sed -i, tee, redirects, mv or cp
  onto it). A folder can be named instead of a file (only the files directly in it).
- Why: since 2.2.0 (2026-09-02) edits through the shell count as the same wrong change.
- You notice: the tool call is refused.
- Refuse or change: the rule must be revised or retracted first; "keeping the text in a comment to get
  past the guard" is called out as wrong (thor2/README.md:349-352).

### D4. Calls that do nothing are never refused
- What: a forbidden, requires or owner-approval rule never refuses: a call that only asks for --help,
  -h, --version, -V, help; a call with --help as the first word after the command (or second, after one
  subcommand); a git push --dry-run or -n; a PowerShell -WhatIf. The flag has to be on the same command
  as the rule's, not on a later one joined by && or ;.
- Why: found by replaying 30 days of real calls; nothing the rule forbids happens.
- You notice: help and dry runs just work.
- Refuse or change: built in. A forbidden check is exempted too. --help counts only as the first word
  after the command (or after one subcommand word), not anywhere in the call.

### D5. Searching is never refused
- What: Grep or Glob that names a file or folder brings up the notes anchored there, only to inform.
- Why: a search is not a write.
- You notice: notes appear, nothing blocks.
- Refuse or change: built in.

### D6. A write to CLAUDE.md, BRAND.md, AGENTS.md or a decisions/ folder after an owed decision
- What: if a capture is owed and the assistant is about to write one of those mirror files, it gets a
  warning (a block only when a judge model is configured and ruled so).
- Why: those files are mirrors, never the source; the decision belongs in THOR first.
- You notice: a warning line with the matched phrase.
- Refuse or change: guard-capture-mode.json off.

### D7. If a check cannot run, nothing is blocked
- What: a check on a missing file, an unreadable rulebook, a store that will not open, or a hook with a
  missing program lets the call through; the first two are reported as needing a look.
- Why: an enforcement layer must never itself be the reason work cannot proceed. Doctrine: only a rule
  whose check runs and holds right now may block.
- You notice: silence; doctor names what rotted.
- Refuse or change: built in.

### D8. Starter notes never refuse
- What: the 11 workflow notes that are not about Windows console windows, the two helper-mode notes and the 22 pinned notes carry no check.
- Why: see A10.
- You notice: a note appears at its moment; no refusal.
- Refuse or change: you may hang a check on your own rules.

---

## E. What THOR does on its own

### E1. Session start: the pinned notes
- What: at session start the assistant is handed the pinned notes (this project's first, then heavy
  global ones, then house style), within about 9,500 bytes because Claude Code shows only the first 2 KB
  of a larger hook output.
- Why: the notes come back before any work.
- You notice: a "Standing rules for this session" block; a headless run (claude -p) leaves out notes tagged
  interactive-only and caps global notes at 8.
- Refuse or change: unpin, retract, or tag interactive-only.

### E2. After a compaction: "the summary is not proof"
- What: a session that starts right after a compaction gets one extra line telling the assistant to
  re-check anything it calls done, tested, flashed, deployed or live.
- Why: a summary replaces what was verified with what was claimed.
- You notice: the assistant re-checks.
- Refuse or change: built in.

### E3. Notes on your prompt and right before a tool runs
- What: when you type a prompt that names a place or an action, and right before a file or command
  that a note is about, the relevant notes (at most four, 1200 characters) are put in front of the
  assistant, once per context (the main conversation, and each subagent on its own). A refusal is never
  hidden by this.
- Why: not a search box to remember; a memory that shows up on time.
- You notice: nothing, unless something relevant appears.
- Refuse or change: edit or retract notes. A bare generic word (dev, src, test, docs) never fires a path.

### E4. It stamps each write to prove who wrote it
- What: the hook adds "[THOR] write attested: <id>" to writes it lets through; at the stop each write is
  checked against Claude Code's own record.
- Why: so verdicts and evaluation reports can only count when a real THOR call made them.
- You notice: that line in a tool result.
- Refuse or change: built in.

### E5. It records the turn's calls and replies (30 days, beside the memory)
- What: real-calls.jsonl and real-replies.jsonl (redacted like the log, size-capped: 144 MB and 32 MB).
- Why: so THOR can later test every rule against what really happened.
- You notice: nothing; the files are beside the memory file. Nothing leaves the machine.
- Refuse or change: yes, install --set-recording off|on --db <memory> (see F12); you can also delete the files.

### E6. The standing sweep for false blocks
- What: THOR replays every rule that can refuse, and every reply-rulebook entry, against that record and
  reports rules that would refuse real calls that ran fine. Seen in doctor, in the evaluation start note
  and when a rule is written ("this rule would refuse K of N real calls").
- Why: two one-off replays on 2026-10-09 found twelve rules with false blocks and 21 false reply-guard
  triggers that no review had shown.
- You notice: one doctor line when there is something; silent at zero.
- Refuse or change: it only informs; narrowing the rule is the fix.

### E7. Judging notes
- What: the mark tool says whether a note belonged where it fired. Two "noise" marks since the last useful
  mark stop it from being shown at session start and before a tool call (it stays findable). Two limits:
  a verdict judged the version it was given on, so a revision that changes the note's text, check or
  bindings clears the earlier marks; and a rule whose check can refuse is never hidden this way, because
  the guard refuses with it anyway and a refusal must not come from a rule you are never shown. A note
  you pinned is never asked about.
- Why: stale and misplaced notes should leave, not rot.
- You notice: only inside the evaluation.
- Refuse or change: you can mark or retract any time.

### E8. Machine pass: THOR tries old rules itself
- What: at an evaluation start (and via "serve retest") THOR tries the project's old rules, writes a RULE or
  NOTE verdict, and writes or closes its own open points. A new matcher build can demote a rule; those go
  to the batch with a reason, never silently.
- Why: older rules have no current verdict; the owner should not hunt for them.
- You notice: only the start note.
- Refuse or change: THOR_MACHINE_PASS_SECS (1 to 200) sets the time box.

### E9. THOR finds bugs in itself and may offer to report one
- What: from its own signals (a program that would not start, a report refused for THOR's own reasons, a
  starting note THOR's own gate rejected, a hook panic...) it keeps candidates and offers at most one per
  session as a nudge in the first message; it never holds a turn. In release mode the assistant asks you
  one yes/no question showing the exact text (paths, project names, computer names, addresses, keys
  stripped) and only after yes opens a GitHub issue; it is offered again once after a day, then dropped.
- Why: THOR is young and its users are its testers; only mistakes in THOR, never in your work.
- You notice: one question, in your language; "no" sends nothing.
- Refuse or change: say no; or remove thor-mode.json so nothing is offered.

### E10. Code index refreshed after each commit; the optional backup and sync
- What: the git hook (A7) refreshes the project's code reading after each commit. The backup runs only
  once you chose it (see A16); the sync program is started by you or your schedule. "sync ship" sends events to a copy;
  "doctor" names a copy or backup that has gone stale.
- Why: a stale code reading gave wrong line numbers for 83 percent of answers.
- You notice: nothing.
- Refuse or change: switch the git hook off by changing core.hooksPath.

### E11. Doctor: a read-only health check
- What: "doctor --db <memory>" prints a plain summary (OK or PROBLEM, each with one next step), then a long
  report: memory healthy, falsifiers, dead anchors, secrets in notes, how many rules can truly refuse
  (2 of 2999 when first measured on 2026-08-06), working copies, evaluation status, backup, the volume
  of what was shown. A missing language model is not a problem.
- Why: a safety net nothing is attached to looks exactly like a safety net that works.
- You notice: nothing until you run it, or the assistant does.
- Refuse or change: it changes nothing.

### E12. Notices at session start
- What: for seven days after a working copy is registered or moved, one line names it; the open-points line
  and an owner-queue banner appear once a day per project; stop notices that did not hold the turn are
  queued (at most 3) and handed over at the next prompt.
- Why: so the owner sees what a first-come registration did.
- You notice: a line or two.
- Refuse or change: not for headless or subagent sessions.

### E13. The personal library
- What: recipes, books, a training log: filed with shelve into a library file kept apart, never injected or
  ranked. A shelf is created only by the owner; the same entry twice is refused; nothing is deleted; a
  search that misses hands back the shelf to read.
- Why: it must never compete with a deploy rule for room.
- You notice: only when you ask.
- Refuse or change: you choose what goes there.

### E14. It notices a THOR program that has gone missing, and may offer one exclusion
- What: at every session start, and in doctor (which also reads the scheduled tasks), THOR checks that the
  program each THOR hook, the tool-server entry, a scheduled task and the memory's own sidecars (the ship
  and the backup) call is on disk. A missing one is named in one line with the likely cause, an antivirus
  quarantine, and the steps: open Windows Security > Virus and threat protection > Protection history,
  open the entry naming the file and choose Actions > Restore; or run THOR's install again. On Windows
  doctor also shows the THOR file names Windows Defender's own detection history gives, when Windows lets
  your user read it without administrator rights (read-only, no window).
- Why: on 2026-10-11 Windows Defender quarantined THOR's sync program as a false positive; the hourly ship
  failed for two days and nothing said why. The ship line is now three cases: never recorded, ran and
  failed (with the error; the example script thor2/deploy/ship-hourly.example.ps1 records every failure,
  a missing program included), and program missing.
- You notice: a line starting "PROBLEM: THOR program missing" (session start and doctor). Only then may
  your assistant OFFER, never unasked, to add a Defender exclusion for the THOR program folder. It must say
  what that does and its risk: Defender stops scanning every file in that folder, so a bad file placed
  there would not be caught. Windows shows one administrator prompt, and saying no is fine. Only after your
  clear yes it runs, in an elevated PowerShell: Start-Process powershell -Verb RunAs -WindowStyle Hidden
  -ArgumentList '-NoProfile -Command Add-MpPreference -ExclusionPath "<folder>"', and checks the result
  with (Get-MpPreference).ExclusionPath.
- Refuse or change: say no and nothing changes. THOR itself never changes a Windows security setting; the
  exclusion is yours to allow and yours to remove (Windows Security > Virus and threat protection >
  Manage settings > Exclusions).

### E15. It reads two folders the owner may name: a temp-until-live folder and a tools folder
- What: both are off until the owner names a folder (F17). THOR only reads them; it never deletes, moves
  or writes inside either.
  Temp-until-live: one sub-folder per production, named <YYYY-MM-DD>_<repo>_<subject>, each with a
  STATUS.txt whose first line starts with "STATUS:". A first line starting with "STATUS: live" (any letter
  case) means the production is live and may be deleted; a production whose date prefix is older than
  temp_max_age_days (30 unless the file says other) and that is not live is named as old. A session start
  says so at most once a day (one line naming the folders, capped; a day with nothing to say is not
  used up), and doctor has the same in its short version (a NOTE line) and in the detailed report.
  Tools: one sub-folder per helper tool, each with a README.txt or README.md whose first non-empty line says
  in one sentence what the tool does. EVERY session start hands the assistant "Helper tools available in
  <folder>:" and one line per tool ("<folder name> - <that first line>", cut to about 100 characters),
  about 2,000 bytes at most (then "... and N more in <folder>"), and one sentence: look here before writing
  a new helper script. The list is built from the folders, so nobody maintains it. It is kept in
  tools-overview-cache.json beside the memory with a fingerprint (folder names, README sizes and times)
  and built again only when the fingerprint changes. At most once a day it also names a tool folder with
  no README, a README with no first line, and script files lying loose in the tools folder.
  Network folders: every read runs on a worker thread with about 1.5 seconds in total, so a share that is
  switched off or unreachable can never hold a session start. On timeout or error the temp notice is
  silent and the tools list falls back to the cached copy (its first line then carries the date it was
  built and says the folder could not be reached just now), or is silent. A headless run gets the tools
  list but not the notices, so a headless run never uses up the day's showing. Not for a subagent.
- Why: finished jobs that nobody deletes pile up, and a helper script that nobody can find gets written
  again. A list built from the folders cannot go out of date the way a list somebody keeps does.
- You notice: up to three paragraphs at the start of a session; "NOTE: in your temp-until-live folder..." in
  doctor.
- Refuse or change: install --set-temp-folder off, install --set-tools-folder off (F17).

---

## F. What the user can switch off or change, and how

### F1. Starting notes (22 pinned, 13 workflow)
- What: any starting note.
- Why: they are ordinary notes.
- You notice: they stop arriving.
- Refuse or change: ask the assistant to unpin, revise or retract (retract needs a reason).

### F2. The reply rulebook
- What: guard-response-rulebook.json (six rules, A11).
- Why: the content is a neutral example.
- You notice: replies stop being held, or are held differently.
- Refuse or change: edit the reminder, delete an entry, delete the file. Optional fields (min_chars, none_of,
  tier warn or block, owner_reading_only...) are listed in thor2/README.md:475-549.

### F3. The capture guard
- What: guard-capture-mode.json beside the memory: "off", "self" (default) or "judge". guard-capture-
  rulebook.json replaces the built-in narrow phrase list.
- Why: it is the one place a false hold costs trust.
- You notice: fewer or no "record this decision" holds.
- Refuse or change: write the file; no setting by command.

### F4. The stale-rule guard
- What: stale-guard-mode.json {"mode": "off"} turns the once-per-session hold off; on by default.
- Why: documented only in the code (serve/src/stale_guard.rs:48-57).
- You notice: that hold disappears.
- Refuse or change: write the file.

### F5. Bug-report mode
- What: install --set-mode release|test, or delete thor-mode.json.
- Why: see A12 and E9.
- You notice: whether an offer appears at all.
- Refuse or change: yes.

### F6. Read-only memory
- What: install --no-mcp.
- Why: a memory the assistant can read but not write, on purpose.
- You notice: no tool server entry; status shows it.
- Refuse or change: yes, at install.

### F7. The evaluation
- What: the routine file (editable), extra command names in eval-command-names.txt, THOR_MACHINE_PASS_SECS.
- Why: the method is meant to be tuned.
- You notice: a different routine.
- Refuse or change: you cannot turn the ask off; the cadence is a code constant so an agent cannot edit it away.
  The hold ends only with a valid report.

### F8. Project name and folders
- What: --project, .thor-project, --topic, --register-root.
- Why: see A14, A15, B3.
- You notice: where notes land.
- Refuse or change: renaming an existing scope is refused.

### F9. Where the backup goes
- What: backup.exe --remote, a remote called "nas" in the backup folder, or the branch's remote.
- Why: see A16.
- You notice: doctor names the target.
- Refuse or change: you set it up and choose.

### F10. What goes to headless runs
- What: tag a note interactive-only so a headless session (claude -p) does not receive it.
- Why: some notes are for the owner's chat.
- You notice: a smaller start block there.
- Refuse or change: revise tags.

### F11. How to remove THOR
- What: README.md has a "Remove THOR completely" section with ten steps. In short: remove its four entries
  in settings.json (plus the backup SessionStart entry if you chose a backup), the "thor" entry in
  ~/.claude.json, the /thor-eval command, unset git's core.hooksPath if it points to THOR's folder
  (the media gate, A7b, goes with it), then delete the memory folder, the programs folder and any
  .thor-project file.
- Why: nothing THOR put on the computer should be left behind unknowingly.
- You notice: THOR goes silent.
- Refuse or change: your call; a backup of both files exists as .bak from the install.

### F12. The record of real calls can be switched off
- What: install --set-recording off|on --db <memory>. Off writes "recording": "off" into thor-mode.json:
  nothing new is recorded in real-calls.jsonl or real-replies.jsonl, and the false-block sweep (E6) then
  says it has no data. What was already recorded disappears by itself after 30 days. On by default.
- Why: the record is what found twelve wrongly blocking rules (E6); it stays on this machine, and the
  first-session list (item 11) tells you it exists.
- You notice: with it off, the sweep reports no data instead of rules.
- Refuse or change: yes, with the command above.

### F13. A changelog and version number (first-session item 12)
- What: the default is yes when the project has none: the assistant creates CHANGELOG.md in the project
  it runs in (a "Changelog" heading with an "Unreleased" section, nothing invented) and keeps a version
  number only if the project has one or you ask for one. A project that already keeps one is left alone.
  The starter note changelog-with-every-user-visible-change then adds a line with each change.
- Why: every later session knows what changed and which version runs.
- You notice: one new file, once.
- Refuse or change: say no to item 12: nothing is created and it is not asked again.

### F14. Fixed on purpose (cannot be changed by setting)
- What: the 300-character limit, the four-per-place cap, the secret scan, the evaluation cadence, the
  need for a reason on a dispute or a weakening, who may name a new place.
- Why: each exists because a softer version was measured to be bypassed (CONTRACT.md R2, R9; the habit
  "every designed way out gets used within a day").
- You notice: refusals with a way on.
- Refuse or change: no.

### F15. Helper-agent mode (first-session item 6) is off until you switch it on
- What: install --set-helpers on|off --db <memory>. A new memory does not hold the two notes that carry the
  mode (orchestrator-mode-saves-quota and an-agent-brief-stands-alone, bound to the Agent tool). On stores
  exactly those two; off retracts them with a reason; running either twice changes nothing. A retracted id
  cannot simply be created again, so on after off stores the note again and then resolves the id onto the
  new version: the log keeps created, retracted, created, resolved, and the id is live with one head.
- Why: measured at the maker, helpers were likely 12 to 62 percent cheaper and keep the chat free, but you do
  not see the work happen and cannot steer a helper halfway, each helper starts with a fixed context cost
  (small tasks cost more), it knows nothing of the conversation beyond its brief, parallel helpers can
  collide in the same files, and checking the result costs an extra turn. So it is yours to choose.
- You notice: with it off your assistant does the work itself in the main conversation; with it on, a
  reminder arrives right before a helper agent is started.
- Refuse or change: say "helpers on" or "helpers off" to your assistant.

### F16. The media gate (A7b) can be tuned, allowed per file, or switched off
- What: all of it is plain git config, so it works per repository (no flag) or for the whole computer
  (`--global` right after `git config`). Raise the limit: `git config thor.mediagate.maxbytes 52428800`
  (50 MB; 104857600 is 100 MB; a plain number, a value like 50m is not understood and the default is used
  with a notice). Change the video endings: `git config thor.mediagate.extensions "mp4 mov mkv"`; an empty
  value (`""`) stops no file for its type, and the size limit still applies. Switch the whole gate off:
  `git config thor.mediagate.enabled false` (no, off and 0 count as false too). Allow one file on purpose:
  a line `<path> thor-media-ok` in .gitattributes, then `git add .gitattributes`. Suggest a parking
  folder in the refusal: `git config thor.mediagate.parkdir "D:/parked"`. Take a setting back with
  `git config --unset thor.mediagate.maxbytes` (add `--global` for a global one).
- Why: the defaults suit most projects; a project that really keeps a demo video or a large data file
  needs a way to say so that travels with the repository (the attribute) and a person needs a way to say so
  for their own computer (the config).
- You notice: the refusal message prints the same steps, and the exact lines for the files it refused.
- Refuse or change: yes, with the commands above. Removing THOR (F11) removes the gate with the hooks.

### F17. The temp-until-live folder and the tools folder: name them, change the age limit, switch them off
- What: work-folders.json beside the memory: {"temp_until_live": "<full path>", "temp_max_age_days": 30,
  "tools": "<full path>"}; every key is optional and a missing or malformed file means nothing is named.
  install --set-temp-folder <full path|off> --db <memory> and install --set-tools-folder <full path|off>
  --db <memory> write the keys (both may be given in one call). Each path must be a full path to a folder
  that exists and answers (a network path such as \\server\share\tools works); everything is checked before
  anything is written, a file somebody edited into invalid JSON is not overwritten, and the command prints in
  plain words what it set. Naming the temp folder also writes temp_max_age_days (30), so the knob is
  visible; change the number by hand to change the age limit. off removes only that folder's key.
- Why: both are the owner's own folders, so nothing about them can be guessed or defaulted.
- You notice: nothing at all until a folder is named.
- Refuse or change: yes, with the commands above; or delete work-folders.json.

---

