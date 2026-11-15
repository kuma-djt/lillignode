# Contributing to Lillignode

Thank you for looking. Lillignode is at v0.1 and rough, so bug reports are as welcome as code.

## Before you write code

- **Open an issue first** for anything bigger than a small fix, so nobody builds something that will not be merged.
- **Security problems do not go in issues.** See [SECURITY.md](SECURITY.md).

## The contributor licence agreement

Lillignode is published under the AGPL-3.0, and Turnbow Analytics LLC also sells commercial licences to people who cannot use the AGPL. That only works if Turnbow Analytics has the right to license every line of the code both ways.

So **every contribution needs a signed contributor licence agreement**, [CLA.md](CLA.md), before it can be merged. In short: you keep the copyright in your contribution, and you give Turnbow Analytics LLC the right to use it and to license it to others under any terms, including commercial ones. Read the whole thing; the summary is not the agreement.

How to sign, until an automated check is set up: add this line, with your own details, as a comment on your first pull request.

> I have read the Lillignode Contributor Licence Agreement (CLA.md, version 0.1) and I agree to it. Full name: ___. GitHub user name: ___.

If you are contributing as part of your job, your employer may own what you write. Check that you are allowed to sign, or ask the person who is.

A pull request without a signed agreement will not be merged, however good it is.

## Rules for tests and experiments

These are hard rules. An early experiment pressed Enter at a Claude Code screen it did not recognise and switched on a setting in someone's real configuration.

- **Tests never run against a real Claude Code setup.** Use the stand-in agent (`examples/standin.rs`), reached through a launcher in a throwaway folder.
- **Point `LILLIGNODE_STATE_DIR` at a throwaway folder** for every experiment, so your own workspace and settings are not touched.
- **Point `CLAUDE_CONFIG_DIR` at a throwaway folder too** whenever a test starts the real `lillignode` program. Lillignode reads Claude Code's conversation records to show cost, and a test must never read real ones. The end-to-end run does both.
- **A tool stops at any screen it does not recognise.** It types only after it has seen the exact text it expects. It never presses Enter or Esc at an unknown prompt.
- **Never send a real message to a real agent from a test.** It spends somebody's money.

## Checks that must pass

```
cargo build --release --all-targets
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
cargo run --release --example e2e
```

If you change `scripts\package-windows.ps1`, also run `scripts\test-package-windows.ps1`.

What these do not prove is listed in [docs/ci-coverage.md](docs/ci-coverage.md). There is no CI yet.

## What a change should look like

- **Every feature ships with its failure path.** A failure message says what happened, what was and was not changed, and what to do next.
- **Unknown is never shown as zero or as the last known value.** No status is "Unknown", missing cost is "cost pending" or "cost unknown".
- **No guessing at an agent's state.** Status comes from what the agent reports, read exactly or not at all.
- **A paste never reaches an agent as typed Enter presses** unless the user was asked and agreed.
- **Use the user's words on screen:** workspace, agent, folder, status, cost, history, conversation. Not pane, PTY, hook, socket.
- **No telemetry, no keys.** Lillignode sends nothing anywhere and does not ask for, read, or store API keys.
- **Agent process handling stays behind `src/agent.rs`.** Nothing outside `src/local.rs` touches a process.
- **Windows first.** A change that has only been run on another system says so.

## Where things are

- `src/agent.rs` is the one interface to running agents; `src/local.rs` is the only implementation.
- `src/workspace.rs` holds the open agents and what became of each; `src/state.rs` saves and loads them.
- `src/app.rs` holds key handling and every sentence shown to the user; `src/ui.rs` and `src/view.rs` draw.
- `src/input.rs` reads the keyboard and tells typing from pasting; `src/config.rs` reads the settings file.
- `src/terminal.rs` is the terminal an agent believes it is in. `src/status.rs` reads Claude Code's status reports. `src/cost.rs` reads what a conversation has used.
- `examples/standin.rs` is the stand-in agent, `examples/e2e.rs` drives the real program, `examples/probe.rs` measures keystroke delay.
