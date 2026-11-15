# What CI must cover

Status: 2026-11-13. No CI exists yet. Everything so far has been run on one Windows 11 machine. macOS and Linux are untested and must not be called supported until the checks below run there.

## Why the other platforms are not a formality

On Windows, the agent's output reaches Lillignode already tidied up by the operating system's pseudo-terminal. On macOS and Linux, Lillignode receives exactly what the agent writes. So the terminal emulator inside Lillignode is exercised much harder there, and a pass on Windows says little about it.

## Every platform (Windows, macOS, Linux)

1. `cargo build --release --all-targets`, `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check`.
2. The end-to-end check (`cargo run --release --example e2e`). It drives the real program against the stand-in agent and covers: first run, failed starts, an agent that dies, resize, quit, restore, agents that cannot be restored, a damaged saved workspace, a second window, both menu keys and the settings file, pastes (marked, unmarked, and into an agent that cannot take them whole), a 3,000-line stream read back in full, an agent with a full-screen display, and continuing a conversation.
3. The probe (`examples/probe.rs`) against the stand-in directly and inside Lillignode: a typed character appears, Lillignode's own delay stays under a millisecond, both exit with code 0 and leave no processes.

Tests never run a real agent or touch real settings (see CONTRIBUTING.md). So nothing automated proves that real Claude Code behaves as the stand-in does. A person checks that by hand before each release, and release notes should say so.

The stand-in reads raw input through the Windows console API on Windows and through `stty` elsewhere. The second path has never been run.

## macOS and Linux only

- Finding the agent program on PATH without Windows file extensions.
- Agents that ask the terminal questions Lillignode does not answer yet. On Windows the operating system answers most of them. Known ones to test: terminal type, colour queries, keyboard protocol queries, window size in pixels.
- Output the Windows layer normally hides: scroll regions, alternate screen switching, synchronized updates, wide and combining characters, hyperlinks.
- Signals: closing the terminal window, Ctrl+Z, and whether agents are stopped when Lillignode is killed.
- The state folder location and its permissions.

## Windows only

- Both the Windows Terminal app and the older console window.
- Agents installed through npm (`.cmd` launchers) and as plain `.exe` files.
- Paths with spaces and non-English characters in the folder and user name.
- Arguments containing spaces or quotes passed through a `.cmd` launcher.

## Not automatable

- Whether it looks right: fonts, colours, flicker during a long streamed answer.
- Whether typing feels instant to a person.

A person checks these on each platform before a release.
