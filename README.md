# Lillignode

Lillignode runs several AI coding agents at once in one terminal window. Each agent works in its own folder and has its own screen; a sidebar lists them all and says what each one is doing, and you hop between them with a key. Two can share the screen when you want to watch both.

The name is said "Lilli-node": the g is silent, as in gnome.

![Four Claude Code agents: one working, one waiting for you, two idle](assets/screenshot.png)

**Status: v0.2, rough.** Built and tested on Windows 11. It builds and passes its automated checks on Linux and macOS; nobody has run an agent in it on either. Expect sharp edges.

## What v0.2 does

- **Several Claude Code agents in one window.** Each has its own folder and its own screen. One agent is in front and takes your typing; the sidebar lists the rest.
- **Hop between them.** Press the menu key (Ctrl+Q or F12), then Tab.
- **See what each one is doing.** The sidebar shows Starting, Working, Needs you, Done, Idle or Error, as the agent itself reports it. Claude Code and Codex CLI report; so can any other program (see [Status](#status)). When Lillignode cannot tell, it says Unknown. It never guesses.
- **Two side by side, when you want.** Menu key, then `v`: the agent in front on the left, the next one beside it, each half the width. `v` again goes back to one. See [Two side by side](#two-side-by-side).
- **Come back to where you were.** Quit and start again: the same agents reopen in the same folders, and Claude Code continues each conversation. Any other agent, Codex included, is started again and begins a new conversation; Lillignode says which.
- **Read back what scrolled away.** The last 5,000 lines per agent, with Shift+PgUp.
- **Paste safely.** A multi-line paste reaches the agent as one paste, never as a string of Enter presses. Control characters are left out, and you are told when they were (in Windows Terminal the terminal removes them first, so you are not told: see Known limits). Where a paste has to be sent as typed keys, its tabs are sent as spaces.
- **Run any other command-line agent too.** If it reports its status in the open format, the sidebar shows it. If it does not, it runs all the same and its status shows Unknown.
- **Cost, if you switch it on.** Experimental and off by default. See [Cost](#cost-experimental-off-by-default).

## The look

Each agent in the sidebar has two lines: a gnome mark and its name, then its folder and its status. The mark is a copper hat `/\` over one sign, so the sidebar can be read at a glance and without colour:

| Sign | Status |
|---|---|
| `+` and `*`, turning about once a second | Working |
| `+` held still | Working, but the agent has drawn nothing at all for a minute or more ("Working, quiet 2 min"). Claude Code 2.1.325 redraws about once a second while it works, even during a long command, so with it this appears only if it stops drawing altogether |
| `!` | Needs you |
| `=` | Done |
| `z` | Idle |
| `x` | Error, or stopped with an error |
| `?` | Unknown |
| `.` | Starting |
| `-` | Exited |

The sidebar says the status in a word or two: "NEEDS YOU", "Unknown", "Error". Why (permission or a question, why status is unknown, the error's own words) is in the notice under the agent and in the status report (menu key, then `s`). When there are more agents than the sidebar has room for, it keeps the one in front in view and says how many are above and below; if one of those needs you, it names it on a yellow row.

**Yellow means Needs you and nothing else.** An agent waiting for you gets a yellow block in the sidebar, a count beside the name at the top, a notice naming it, and one bell. Copper marks the hats, the name at the top and the agent in front, which also has a `>` before its hat.

Lillignode works out how many colours your terminal shows and uses fewer where it has to. With 16 colours or none, the hats and the agent in front are bold instead of copper; the characters are the same at every depth. Lillignode never colours an agent's own screen.

The bottom line always starts with the menu keys. After them it says where your typing goes.

## Get it (Windows)

1. Download `lillignode-0.2.0-windows-x64.zip` from the [latest release](https://github.com/kuma-djt/lillignode/releases/latest). The release page lists the zip's SHA-256, so you can check the download (`Get-FileHash lillignode-0.2.0-windows-x64.zip` in PowerShell).
2. Unzip it anywhere. It unpacks into one folder.
3. Open Windows Terminal in that folder and run `.\lillignode.exe`.
4. Press `n` to start an agent: Enter for Claude Code, Enter again for no options, then type the folder it should work in and press Enter. (Enter alone uses the folder Lillignode was started in.)

You need [Claude Code](https://claude.com/claude-code) installed and signed in. The program is not signed, so Windows may warn you the first time you run it.

## Using it

**On Windows, use Windows Terminal.** In the classic console window, pasting into Claude Code does not work (with or without Lillignode). In VS Code's terminal Ctrl+Q belongs to VS Code, so use F12.

Inside Lillignode, press `n` to start an agent: Enter for Claude Code, then its options (Enter alone for none), then the folder it should work in. Your typing goes to the agent in front. Ctrl+Q or F12 opens Lillignode's menu:

**Options and kind.** Options are split on spaces; double quotes keep a space inside one (`--name "two words"`). Lillignode takes a program named `claude` for Claude Code, `codex` for Codex, and anything else for a plain program. To start a wrapper script as Claude Code, or `claude` as a plain program, press Tab at the options to change the kind. When options were typed or the kind was changed, one more line shows the kind, the options as split, the program and the folder before anything starts: `y` starts it, `n` goes back to change it, `k` changes the kind. As Claude Code, an agent is given its conversation by Lillignode, so options that name one (`--resume`, `--session-id`, `--continue`) are refused there.

| Key | What it does |
|---|---|
| Esc | back to the agent |
| q | quit |
| Tab, Shift+Tab | next agent, previous agent |
| n | new agent |
| x | close the agent in front |
| r | start it again after it has stopped |
| s | its status report (and its cost, if switched on). When it is longer than the window, PgUp, PgDn, Up, Down and Home move through it. While a part of the agent is waiting, `d` there stops showing it |
| PgUp | read back its history |
| v | two agents side by side, or back to one |

```
lillignode --where     where your workspace and settings are kept
lillignode --help
lillignode --version
```

## Build from source

You need the [Rust toolchain](https://rustup.rs) and, to do anything useful, [Claude Code](https://claude.com/claude-code) installed and signed in.

```
git clone https://github.com/kuma-djt/lillignode
cd lillignode
cargo build --release
target\release\lillignode.exe
```

On macOS or Linux the last line is `target/release/lillignode`, and you are in untested territory.

### On Windows, one extra step for status

Claude Code reports its status only after its terminal answers a question, and on Windows that answer only reaches it through Microsoft's newer pseudo-terminal, two files (`conpty.dll` and `OpenConsole.exe`) that must sit beside `lillignode.exe`. The release zip has them; a build from source does not. Without them agents run normally, and an agent that waits for the answer, as Claude Code does, shows Unknown. An agent that reports without asking, as Codex does, is not affected. This script downloads them from Microsoft's Windows Terminal release, checks their hashes and Microsoft's signature, and puts them in place:

```
powershell -ExecutionPolicy Bypass -File scripts\package-windows.ps1 -BesideBuild
```

Without `-BesideBuild` it builds a release folder and a zip in `dist\`. See [docs/packaging-windows.md](docs/packaging-windows.md).

## Two side by side

Menu key, then `v`. The agent in front goes on the left with the next agent beside it, and each gets a heading. The one typing goes to says TYPING GOES HERE on a copper block; the other says "not typing here". `v` again goes back to one. The view is not remembered when you quit.

- **Typing goes to one agent only,** the one whose heading says TYPING GOES HERE. The bottom line says which side. While the menu or a question is open, or the agent in front has stopped, the heading says so instead: keys are Lillignode's then.
- **Tab (after the menu key) goes to the next agent in the sidebar's order,** as it does with one on screen. If that agent is already on screen, the keyboard moves to it. If not, it takes the side you are on and the other side stays put. With more than two agents open, Tab is not always "the other side".
- **An agent that needs you while you are typing into the other one** gets a yellow heading: "NEEDS YOU - you are NOT typing here". Read it before you answer: what you type goes to the other agent until you switch.
- **It needs a window at least 109 columns wide.** In a narrower one `v` is refused and says so. If you narrow the window while two are showing, Lillignode shows one, says why, and puts both back when there is room.
- **Changing the view resizes every agent,** shown or not, and each redraws at its new size. While Lillignode knows an agent is waiting for you, it asks first. It cannot know that for an agent whose status is Unknown (any agent that is not reporting): those are resized without a question.
- **What a narrower size costs.** Anything on an agent's screen beyond the new width is gone until the agent draws it again. Lines already in its history are kept whole.

Checked by hand with Claude Code 2.1.325: both agents redraw cleanly at half width, and on November 16, 2026, in Windows Terminal, a resize while Claude Code was showing a permission prompt left the prompt whole and answerable. If an agent looks wrong after a change, `v` goes back to one; most agents also redraw on Ctrl+L.

## Cost (experimental, off by default)

Put `"cost": true` in the settings file and Lillignode shows, under each Claude Code agent, how many tokens its conversation has used, with a total for all agents at the foot of the sidebar and a breakdown one key away (menu key, then `s`). Everywhere it is shown, it says it is experimental.

**What it reads.** Claude Code keeps a record of each conversation in its own folder on this machine. Lillignode opens the record of each conversation it started, read-only, takes the token counts and the model name from each reply, and keeps nothing else. It changes nothing, saves nothing, and sends nothing anywhere. While cost is off, it reads none of these records.

**Why it is experimental.** The record's format is internal to Claude Code and not documented; an update to Claude Code can change it. Lillignode reads it exactly or not at all: before anything is read it says "cost pending", and when it cannot read or understand a record it says "cost unknown". It never shows a zero it has not read. It has been seen reading real records and showing counts, but its figures have not yet been compared with Claude Code's own `/usage`. Trust `/usage`.

**Why a new conversation starts high.** A conversation's first request counts tens of thousands of tokens (about 59,000 in one check), most of them Claude Code writing its own instructions to its cache. That is real use, so it is counted, not hidden. It also means a cost warning set below that is passed by every agent at once: set it well above it.

- **The count in the sidebar leaves out tokens read from cache.** They are most of a long conversation's count and the cheapest part of it. The breakdown shows them apart.
- **The breakdown also says what was used since Lillignode was started,** for the agent in front and for all agents open now.
- **A warning, if you want one.** Add `"cost_warning_tokens": 500000` (any count above zero) and a conversation that reaches it gets PAST WARNING in bold beside its figure, and one notice. That is all it does: nothing is stopped, held, or asked. The warning is for each conversation by itself, not for all of them added up, and it is given again each time Lillignode is started while the conversation is past it.
- **Dollars** are an estimate from a price list built into the program, shown in the sidebar only if you put `"billing": "key"` in the settings file. Unlike the token count, the estimate includes cache reads, because they are billed. Plan limits are not shown.

## The page (off by default, and read only unless you say otherwise)

Put `"page": true` in the settings file and Lillignode also serves a page for a browser on this machine. Its address is `http://127.0.0.1:7501/` (`"page_port"` sets another port) followed by a random word, such as `http://127.0.0.1:7501/b9iq5c8f72cy7ev3/`. Lillignode shows the whole address when it starts, and again at the foot of the status report (menu key, then `s`).

- **The address stays the same.** The word is made once and kept in a file only you can read (`page-word`, beside your workspace; `lillignode --where` says where), so you can bookmark the page, and a page left open comes back by itself after Lillignode is started again. The menu key, then `w`, makes a new word: the old address stops working at once, a page open on it says so, and Lillignode shows the new one.

- **What it shows.** One row for each agent with its status and how long ago it reported it ("Working, 2 s ago", "Needs you, waiting 4 min", "Unknown: no report"); the terminal of the agent you choose, live; and, above everything, a strip titled "Blocking now" naming each agent that needs you and how long it has waited. A third column, "Board", says it is not connected yet: nothing is behind it.
- **It is read only.** The page can ask Lillignode for nothing and type nothing. Choosing an agent on the page changes only what the page shows. (Unless you switch typing on: see below.)
- **It says how fresh it is.** The header says when the page last heard from Lillignode, and says so plainly when it has lost contact; what is on the page then is marked as old, and it keeps trying.
- **Who can read it.** It listens on this machine only, never on the network, and answers only at its address with the word: anything else gets nothing of the page. Anyone with the address can read every agent's name, folder, status and screen; a page on another web site cannot. It is the one way an agent's screen leaves Lillignode's own window, which is why it is off unless you ask. The word is in your browser's history and in a file of yours, so a program running as you can find it. A page left open while Lillignode is closed keeps asking for its address: a program on this machine that took the port meanwhile would be told the word. If you think it has got out, the menu key, then `w`.

### Typing to an agent from the page (off unless you ask, apart from the page)

Add `"page_can_type": true` beside `"page": true` and the page gets a box under the agent's terminal, labelled "To" and the agent's name, and a Send button. Send passes the text to that agent as if you had typed it in Lillignode, then Enter.

- **Anyone with the address can then type to your agents,** which for a coding agent can mean running commands as you. So with typing on, the address is not kept: the word is new each time Lillignode starts and is saved nowhere, the page cannot be bookmarked, and a page left open cannot follow a new start. The page answers only at `127.0.0.1`, not at `localhost`.
- **It says so.** The page's header says it can type instead of "Read only"; Lillignode's start notice and status report say so; and each time the page sends, Lillignode's own window says "The page typed to" and the agent's name.
- **Only to the agent named beside the box.** Once there is text in the box it is for that agent until you choose another; the page does not move it to whichever agent comes to the front.
- **The same rules as a paste.** Control characters are taken out and said. Text of more than one line for an agent that does not take a paste as one block asks first, on the page, in the words Lillignode's own window uses. A stopped or closed agent refuses, in those words too. An empty box sends nothing, so Enter alone cannot be sent.
- **Who can send.** Only the page itself: a request must come from the page's own address, carry a token the page is given with its data, and be made the way only the page's script makes it. A link, a form, or a page on another web site cannot. A program running as you that has learned the address can.
- **Not yet checked by a person, and never with a real agent.** Whether a real agent takes Enter straight after a paste the way the stand-in does is not known.
- **Not yet checked by a person.** Checked by script, with stand-in agents, in a browser with no window. The agent's terminal on the page is its text and colours as the agent sent them; it is not an exact copy of the terminal window, and history cannot be read there.

## Settings

One JSON file; `lillignode --where` says where it is.

```json
{ "menu_keys": ["ctrl+q", "f12"], "bell": true }
```

Those two are the defaults. The settings for cost, which is off unless you ask:

```json
{ "cost": true, "cost_warning_tokens": 500000, "billing": "key" }
```

The page, which is off unless you ask:

```json
{ "page": true, "page_port": 7501 }
```

And to let that page type to agents, which is off unless you ask even when the page is on:

```json
{ "page": true, "page_can_type": true }
```

If the colours come out wrong, add `"colors"` with `"full"`, `"256"`, `"16"` or `"none"`. Setting the `NO_COLOR` environment variable also switches colour off.

All are optional, and a change takes effect the next time Lillignode is started. `billing` does nothing without `cost`, and `cost_warning_tokens` without it is reported as a mistake; so is `page_port` or `page_can_type` without `page`. A Lillignode older than the page does not know `"page"` and treats it as a mistake in the file.

**The file is strict.** One mistake in it and none of it is used: every setting is at its default until it is fixed. Lillignode never changes the file. It tells you when it starts, beginning with how to fix it ("To fix your settings file: ...") and naming the line and column where it can.

## Status

An agent's status is what the agent says it is. Lillignode reads the status reports a program sends to its terminal in an open format, the [Program Status proposal (OSC 7501)](https://mitchellh.com/writing/program-status-osc7501) by Mitchell Hashimoto ([specification](https://www.superlogical.com/rex/docs/build/program-status), read here as revision 0.4). Any program may send them, and the report's own name for its sender says who it is. The status report (menu key, then `s`) shows the last report as it arrived, how it was read, and who sent it.

- **Any program that reports gets a status,** with the yellow block and the bell when it needs you. Lillignode's terminal answers "yes" to a program that asks whether reports are taken.
- **A program that reports nothing shows Unknown,** and the status report says no report has come. Claude Code and Codex are expected to report, so for them a notice says so once, with the likeliest cause.
- **Done means the agent's turn ended.** It does not mean the work succeeded, and the turn may have ended on a question written as ordinary text, which no report marks as waiting for you. Seen with Grok Build.

### Which programs, and how we know

Three levels of evidence, strongest first. Everything checked by hand was on Windows 11 in Windows Terminal, in November 2026.

| Program | Status in Lillignode | How we know |
|---|---|---|
| Claude Code 2.1.325 | Starting, Idle, Working, Needs you with one bell, Done | **Checked by hand** |
| Claude Code 2.1.329 | At its "do you trust this folder" question: Needs you with one bell, then Idle once answered. Nothing else was checked with this version | **Checked by hand** |
| Codex CLI 0.169.0 | The same. It asks permission, and so shows Needs you, only when started with `-a on-request` (see [Codex](#codex)) | **Checked by hand** |
| Grok Build 1.0.77 | The same, but only with `program_status = "on"` in Grok Build's own settings file. Without it Grok Build sent nothing here and showed Unknown | **Checked by hand** |
| A program that prints a report itself | Needs you with one bell, Done | **Checked by hand,** with reports printed from PowerShell |
| OpenCode 2.2.0 and later, Pi 1.1.0 and later, omp 18.8.7 and later | Not run in Lillignode | **Checked by source:** the code that sends the reports was read in each project's public repository |
| Gemini CLI 0.68.0, Cursor's CLI (build of 2026-11-13) | Unknown: each was started once, by script, and sent nothing | **Checked by a scripted run** |
| Devin CLI, Amp | Not run, source not read | **Claimed** by the vendor's changelog (Devin CLI) or by a third-party list (Amp) |

Lillignode changes no other program's settings. Reports from a program not in the first five rows are read the same way and have not been seen by anyone here.

What Lillignode does with a report that is not as expected:

- **A state it does not know** shows "Unknown", never the status before it.
- **A part it does not know is passed over,** as the format asks. A reason for waiting that it does not know still shows Needs you.
- **A message is shown as plain words.** Characters that would draw, move, hide or reorder text become spaces, and a long message is cut.
- **A program that changes status very fast** rings at most three bells in a row, then one every 20 seconds. Its row still shows its latest status.
- **A part of a program that is waiting makes the agent Needs you.** The format lets a program report for each of its parts (a session, a helper agent, a tab), each with its own id. Lillignode keeps the latest report of each part, up to 64 for one agent. If the program itself or any part of it says it is waiting, the row says Needs you, with the bell, and the notice and the status report say that it is a part, give the part's own name in quotes ("a part with no name" if it gave none) and say how long it has waited. A waiting part is shown until it says something else, is withdrawn, or the program ends; it is never ended by a clock, so an old one says "waiting since 52 minutes ago". In the status report, `d` stops showing the parts waiting now, after asking. A part that repeats exactly the same report stays hidden; once it reports something different and waits again, it is shown again. Parts that are working or done do not change the row. A part's id the format does not allow is still read as a part, never as the program. Here Lillignode is stricter than the proposal, which says such a report is ignored: a part that is waiting for you is the one thing not to miss, so a malformed id does not make Lillignode drop it. Built against a stand-in program and not yet seen with a real one.
- **Inside a shell,** Working or Needs you ends when the shell marks its next prompt (shells with prompt marking switched on). Without that, a program that died while Working stays "Working, quiet N min".

What to watch for:

- **Anything that prints a status report into an agent's terminal is read as that agent's status,** for example a file holding those bytes shown raw in a shell. A false Idle could hide a real Needs you.
- **The format is a draft** and can change. A report this Lillignode cannot read shows Unknown.

## Codex

Start Codex CLI like any agent: press `n`, type `codex`, then its options if any (`-a on-request` asks before running commands), then the folder. A program whose file is named `codex` is taken for Codex. Lillignode adds nothing to how Codex is started and changes none of its settings.

**Codex asks permission only when it is started with `-a on-request`.** Started plainly it may never ask: a command it is not allowed to run ends its turn, and the row says Done. Lillignode adds no option for you: type it at the options step when you start Codex.

**Checked by hand** with Codex 0.169.0 on Windows 11 in Windows Terminal, started with `-a on-request`: Starting, Idle, Working, Needs you with one bell at an approval, Done, two side by side with Claude Code, and the quit rules. Unknown when signed out was seen by script only. Not seen from real Codex: an error, a question.

What Codex gets, and what it does not:

- **Status, the yellow block and the bell:** yes, as for any program that reports.
- **On Windows, the two Microsoft files are not needed for Codex's status.** Codex reports without waiting for an answer.
- **Its conversation is not continued.** When Lillignode reopens a Codex agent, or you start one again, it begins a new conversation. Codex's own `codex resume` still finds the earlier one.
- **No cost.** It shows "cost unknown", never zero.

What to watch for:

- **Codex says Idle on at least one of its own set-up screens that is waiting for you.** Seen with Codex 0.169.0 at its first-run sandbox question; there may be others. Lillignode shows Idle there, with no yellow block and no bell, because that is what Codex reported. A request for permission while it was working was seen to be reported as it should be.
- **Signed out, Codex reports nothing** (seen with 0.169.0). Its row shows Unknown after 15 seconds, and a notice says so. The same notice appears if Codex is silent for any other reason.
- **History.** Codex draws on a full-screen display, which leaves Lillignode no history to keep. Scroll with Codex's own keys.

## Known limits in 0.2

These are known and not yet fixed. Please do not report them as security problems.

- **The saved workspace is trusted at launch, within limits.** When Lillignode starts, it runs the agents listed in its workspace file as they are written there: the command, its arguments and its folder. It checks the file first and starts nothing it would never have written itself: a control character in a name, command, argument or folder, a conversation id that is not one it writes, an argument a `.cmd` or `.bat` launcher would act on (`& | < > ^ % "` or a line break), a launcher whose own path holds one of those characters (npm's launchers read their own folder back without quotes, so a folder such as `C:\Users\R&D` would split there), more than 64 agents, or a file over 1 MB. Each such agent stays in the sidebar, not started, with the reason. But a well-formed entry naming another program is started: anyone who can change that file can make Lillignode start a program the next time you open it. The file is in your own user folder (`lillignode --where` says where); keep it there and do not point `LILLIGNODE_STATE_DIR` at a shared folder.
- **A paste can still say anything in plain words.** Where Lillignode sees control characters (Ctrl+C, Ctrl+D, Escape and the like) in a paste, it takes them out and says how many it left out, so a paste cannot interrupt an agent or speak to its terminal. Words that tell an agent to do something are passed on as they are. Paste only what you meant to type.
- **Done means the agent's turn ended.** It may have ended on a question written as ordinary text. Lillignode shows what the agent reports.
- **Codex** asks permission only when started with `-a on-request`. On at least one of its own set-up screens it reports Idle while it waits for an answer. It reports Done for a turn that failed.
- **Grok Build** is silent unless its own status setting is on.
- **Claude Code 2.1.325** sends no report at its "do you trust this folder" question, so the row does not say it is waiting. 2.1.329 does report there.
- **A helper agent waiting inside a program** is not shown yet. The format lets a program report each helper separately; Lillignode counts those reports and does not show them yet.
- **Gemini CLI and Cursor's CLI** sent no status reports in the versions tried (Gemini CLI 0.68.0, Cursor's CLI build of 2026-11-13, each started once by script). They show Unknown.
- **Cost** has not been compared with Claude Code's own `/usage`. Trust `/usage`.
- **Pasting on Windows.** Windows Terminal hands a paste to Lillignode as ordinary key presses, so Lillignode cannot always tell a paste from typing. In our checks Windows Terminal itself removed Ctrl+D and Esc from pasted text, so Lillignode never saw them and says nothing about them. Text with a tab or a line break in it that arrives in one go is taken as a paste, and its tabs are sent as spaces. A paste that is only a tab is still taken as the Tab key, and typing that arrives all at once with a Tab or Enter in it (over a slow remote link, say) is taken as a paste.
- **A launcher file in a folder whose path holds `&`** (for example `C:\Users\R&D`), or another character the command interpreter acts on, is refused. Start the program itself, not its launcher.
- **Windows is the only system checked by hand.** Linux and macOS (Apple Silicon) build and pass the unit tests, the lint and the end-to-end check against a stand-in agent in CI on every change. Nobody has started an agent in Lillignode on either, and no download is offered for them yet. If you test it on one and it works, thanks. Please let me know.

## Other limits

- **Windows 11 only so far.** Use Windows Terminal (see above).
- **Claude Code is the one agent with cost and a conversation that continues.** Status is for any program that reports it; see [Status](#status).
- **Codex has status only.** See [Codex](#codex) for what that leaves out and what to watch for.
- **If `CLAUDE_CODE_DISABLE_TERMINAL_TITLE` is set where you start Lillignode,** Claude Code does not report status and every Claude Code agent shows Unknown. Lillignode leaves your setting alone.
- **Agents stop when you quit.** They reopen next time. Lillignode asks before quitting only while an agent may be in the middle of something.
- **The mouse is not passed to agents,** history cannot be searched, and an agent cannot be renamed.
- **No installer, and the program is not signed.** On Windows, build it yourself or use the release zip.

## What it touches on your machine

- It does not ask for, read, or store API keys. Each agent uses the account it already has.
- It sends nothing anywhere: no telemetry, no crash reports, no update checks.
- It saves which agents are open, their folders, and their conversation ids. It never saves what an agent printed.
- Only if you switch cost on, it reads Claude Code's own record of each conversation it started, read-only, and keeps only the token counts and the model name.
- It does not change Claude Code's settings.
- Only if you switch the page on, it listens on this machine (127.0.0.1) and shows agents' status and screens to a browser here that has its address. It opens no port otherwise. It then keeps one small file of its own, the word in that address. Only if you also switch typing on can anything be sent to an agent from the page.

## Checking your changes

```
cargo build --release --all-targets
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
cargo run --release --example e2e
cargo run --release --example page_check
```

The last two drive the real program against a stand-in agent; the first of them takes several minutes, the second checks the page and takes about one. On Windows, run the packaging script with `-BesideBuild` first so it covers status. See [CONTRIBUTING.md](CONTRIBUTING.md) before sending a change, including the rule that tests never touch a real Claude Code setup.

## How it is built

Most of Lillignode's code is written by an AI coding agent, Claude Code, directed by a person. Every change passes the checks above and independent reviews, and each release is checked by hand by a person against real Claude Code. What has not been checked yet is listed under [Known limits in 0.2](#known-limits-in-02).

## Licence

Copyright (C) 2026 Turnbow Analytics LLC.

Lillignode is free software under the [GNU Affero General Public License, version 3](LICENSE). It comes with no warranty.

If the AGPL does not suit your use, Turnbow Analytics LLC may offer a commercial licence. Contributions are accepted under a contributor licence agreement so that this stays possible; see [CONTRIBUTING.md](CONTRIBUTING.md).

The two Microsoft files the Windows packaging script downloads are Microsoft's, under the MIT licence: [packaging/windows/LICENSE-Microsoft-ConPTY.txt](packaging/windows/LICENSE-Microsoft-ConPTY.txt). They are not in this repository. The icon in `assets/` is Lillignode's own, under the same licence as the rest.
