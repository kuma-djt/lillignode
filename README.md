# Lillignode

Lillignode runs several AI coding agents in one terminal window. Each agent works in its own folder and has its own screen; a sidebar lists them all and says what each one is doing, and you hop between them with a key.

**Status: v0.1, rough.** It is built and tested on Windows 11 only. macOS and Linux are untested. Expect sharp edges.

## What v0.1 does today

- **Two or more Claude Code agents in one window.** Each has its own folder and its own screen. One agent is in front at a time and takes your typing; the sidebar shows the rest. This is not yet split panes: you see one agent's screen at a time.
- **Hop between them.** Press the menu key (Ctrl+Q or F12), then Tab.
- **See what each one is doing.** For Claude Code the sidebar shows Starting, Working, Needs you, Done, Idle or Error, as Claude Code itself reports it. An agent waiting for you gets a yellow block, a count in the heading, a notice naming it, and one bell. When Lillignode cannot tell, it says Unknown and why. It never guesses.
- **Come back to where you were.** Quit and start again: the same agents reopen in the same folders, and Claude Code continues each conversation.
- **Read back what scrolled away.** The last 5,000 lines per agent, with Shift+PgUp.
- **Paste safely.** A multi-line paste reaches the agent as one paste, never as a string of Enter presses.
- **Run any other command-line agent too.** It runs, and its status shows "Unknown: not Claude Code".

### Experimental: the cost readout

Beside each agent Lillignode shows how many tokens its conversation has used, with a total for all agents at the foot of the sidebar and a breakdown one key away (menu key, then `s`).

**This has not been checked against real Claude Code yet.** It reads the token counts from the record Claude Code keeps of each conversation. That record's format is internal to Claude Code and not documented, and Lillignode's reading of it has only been tested against a stand-in. With real Claude Code it may show a correct number, "cost unknown", or a wrong number. Until that check is done, trust `/usage` inside the agent, not this.

What it does promise: before anything is read it says "cost pending", and when it cannot read or understand the record it says "cost unknown". It never shows a zero it has not read. Dollar figures are an estimate from a price list built into the program, shown only if you put `"billing": "key"` in the settings file. Plan limits are not shown.

## Build and run

You need the [Rust toolchain](https://rustup.rs) and, to do anything useful, [Claude Code](https://claude.com/claude-code) installed and signed in.

```
git clone https://github.com/kuma-djt/lillignode
cd lillignode
cargo build --release
target\release\lillignode.exe
```

On macOS or Linux the last line is `target/release/lillignode`, and you are in untested territory.

Inside Lillignode, press `n` to start an agent: Enter for Claude Code, then the folder it should work in. Your typing goes to the agent in front. Ctrl+Q or F12 opens Lillignode's menu: switch, history, new agent, close, start again, status and cost, quit.

```
lillignode --where     where your workspace and settings are kept
lillignode --help
lillignode --version
```

### On Windows, one extra step for status

Claude Code's status only reaches Lillignode through Microsoft's newer pseudo-terminal, two files (`conpty.dll` and `OpenConsole.exe`) that must sit beside `lillignode.exe`. Without them agents run normally and every status shows Unknown. This script downloads them from Microsoft's Windows Terminal release, checks their hashes and Microsoft's signature, and puts them in place:

```
powershell -ExecutionPolicy Bypass -File scripts\package-windows.ps1 -BesideBuild
```

It also builds a release folder in `dist\`. See [docs/packaging-windows.md](docs/packaging-windows.md).

### Settings

One JSON file; `lillignode --where` says where it is.

```json
{ "menu_keys": ["ctrl+q", "f12"], "bell": true, "billing": "key" }
```

All three are optional. A file Lillignode cannot use is left alone, the defaults are used, and you are told why.

## Known limits

- **Windows 11 only so far.** Use Windows Terminal. In the classic console window, pasting into Claude Code does not work (with or without Lillignode) and some characters draw as boxes. In VS Code's terminal Ctrl+Q belongs to VS Code, so use F12.
- **Claude Code is the one agent with real status and cost.** Its status report format is not published and was read from Claude Code 2.1.325; a Claude Code update can change it, and then status shows Unknown.
- **If `CLAUDE_CODE_DISABLE_TERMINAL_TITLE` is set where you start Lillignode,** Claude Code does not report status and every agent shows Unknown. Lillignode leaves your setting alone.
- **Agents stop when you quit.** They reopen next time. Lillignode asks before quitting only while an agent may be in the middle of something.
- **The sidebar does not scroll,** the mouse is not passed to agents, history cannot be searched, and an agent cannot be renamed.
- **No installer.** You build it yourself.

## What it touches on your machine

- It does not ask for, read, or store API keys. Each agent uses the account it already has.
- It sends nothing anywhere: no telemetry, no crash reports, no update checks.
- It saves which agents are open, their folders, and their conversation ids. It never saves what an agent printed.
- For the cost readout it opens Claude Code's record of each conversation it started, takes the token counts and the model name, and keeps nothing else.
- It does not change Claude Code's settings.

## Checking your changes

```
cargo build --release --all-targets
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
cargo run --release --example e2e
```

The last one drives the real program against a stand-in agent and takes about two minutes. See [CONTRIBUTING.md](CONTRIBUTING.md) before sending a change, including the rule that tests never touch a real Claude Code setup.

## Licence

Copyright (C) 2026 Turnbow Analytics LLC.

Lillignode is free software under the [GNU Affero General Public License, version 3](LICENSE). It comes with no warranty.

If the AGPL does not suit your use, Turnbow Analytics LLC can offer a commercial licence. Contributions are accepted under a contributor licence agreement so that this stays possible; see [CONTRIBUTING.md](CONTRIBUTING.md).

The two Microsoft files the Windows packaging script downloads are Microsoft's, under the MIT licence: [packaging/windows/LICENSE-Microsoft-ConPTY.txt](packaging/windows/LICENSE-Microsoft-ConPTY.txt). They are not in this repository.
