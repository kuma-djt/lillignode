# Checks in a browser and in a real window

Stand-ins only: no real agent, no real Board service, no real settings.

- `page/*.mjs` drive the real Lillignode's page in Edge or Chrome with no window (Node 22 or later). Each takes a folder for its pictures: `node checks/page/teams-check.mjs teams-out`. Build first: `cargo build --release --all-targets`.
- `window/*.ps1` start Lillignode in a real Windows Terminal window, press keys and read the screen. **They take the keyboard, the mouse and the clipboard of the desktop they run on**, so they refuse to start unless `-Seat ci` says the desktop belongs to the run (GitHub's Windows machine). Never run them on your own desktop.

`.github/workflows/browser-checks.yml` runs both.
