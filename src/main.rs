use std::io::Write;
use std::process::ExitCode;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

use crossterm::event::{DisableBracketedPaste, EnableBracketedPaste};
use crossterm::execute;

use lillignode::agent::AgentEvent;
use lillignode::app::App;
use lillignode::config::{self, Config};
use lillignode::input::{self, Input};
use lillignode::local::LocalAgents;
use lillignode::{state, ui};

enum Msg {
    /// A key, paste or resize, and when it was read.
    Input(Input, Instant),
    Agent(AgentEvent),
}

impl From<AgentEvent> for Msg {
    fn from(e: AgentEvent) -> Self {
        Msg::Agent(e)
    }
}

fn help(config: &Config) -> String {
    format!(
        "\
Lillignode runs several AI coding agents at once in one terminal window.

Usage: lillignode            open your workspace
       lillignode --where    print where your workspace and settings are kept
       lillignode --version
       lillignode --help

Inside Lillignode, typing goes to the agent in front. Press {keys} for
Lillignode's own menu: switch agents, read back history, new agent, close, quit,
and v for two agents side by side (in a window at least 109 columns wide).

To change the menu keys, put this in the settings file (see --where):
  {{ \"menu_keys\": [\"ctrl+q\", \"f12\"] }}
Allowed: ctrl+letter, alt+letter, f1 to f12.

To see what each Claude Code conversation has used, add \"cost\": true to that
file. It is experimental and off unless you ask. Lillignode then reads Claude
Code's own record of each conversation on this machine, and changes nothing.

Each agent uses its own account. Lillignode does not ask for your keys and
sends nothing anywhere.",
        keys = config.menu_keys_label()
    )
}

fn main() -> ExitCode {
    let mut state_dir = state::default_dir();
    let (mut config, config_problem) = match &state_dir {
        Ok(dir) => config::load(dir),
        Err(_) => (Config::default(), None),
    };
    if config::terminal_takes_ctrl_keys(std::env::var("TERM_PROGRAM").ok().as_deref()) {
        config.function_keys_first();
    }
    match std::env::args().nth(1).as_deref() {
        None => {}
        Some("--help" | "-h") => {
            println!("{}", help(&config));
            return ExitCode::SUCCESS;
        }
        Some("--version" | "-V") => {
            println!("Lillignode {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Some("--where") => {
            return match &state_dir {
                Ok(dir) => {
                    println!("workspace: {}", state::file_in(dir).display());
                    println!("settings:  {}", dir.join(config::FILE).display());
                    if let Some(problem) = &config_problem {
                        println!("\n{problem}");
                    }
                    ExitCode::SUCCESS
                }
                Err(why) => {
                    eprintln!("Lillignode has nowhere to save your workspace: {why}.");
                    ExitCode::FAILURE
                }
            };
        }
        Some(other) => {
            eprintln!(
                "Lillignode does not understand \"{other}\". Nothing was started.\n\n{}",
                help(&config)
            );
            return ExitCode::FAILURE;
        }
    }

    // Held until Lillignode closes. See `state::claim`.
    let _claim = match &state_dir {
        Ok(dir) => match state::claim(dir) {
            Ok(file) => Some(file),
            Err(state::ClaimError::InUse) => {
                eprintln!(
                    "Lillignode is already open in another window and is using your workspace. \
                     Nothing was started here. Switch to that window, or close it and run lillignode again."
                );
                return ExitCode::FAILURE;
            }
            Err(state::ClaimError::Other(why)) => {
                state_dir = Err(why);
                None
            }
        },
        Err(_) => None,
    };

    let launch_dir = match std::env::current_dir() {
        Ok(dir) => dir,
        Err(e) => {
            eprintln!("Lillignode cannot read the current folder ({e}). Nothing was started. Change to a folder that exists and run it again.");
            return ExitCode::FAILURE;
        }
    };

    let mut terminal = match ratatui::try_init() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("Lillignode could not take over this terminal ({e}). Nothing was started. Run it in a terminal window, not through a pipe.");
            return ExitCode::FAILURE;
        }
    };
    let restore_terminal = || {
        let _ = execute!(std::io::stdout(), DisableBracketedPaste);
        ratatui::restore();
    };
    // Ask the terminal to mark pastes, so a paste is known to be one.
    let _ = execute!(std::io::stdout(), EnableBracketedPaste);
    let window = match terminal.size() {
        Ok(size) => ratatui::layout::Rect::new(0, 0, size.width, size.height),
        Err(e) => {
            restore_terminal();
            eprintln!("Lillignode could not read the size of this terminal ({e}). Nothing was started.");
            return ExitCode::FAILURE;
        }
    };

    let (tx, rx) = mpsc::channel::<Msg>();
    let backend = LocalAgents::new(tx.clone());
    let mut app =
        App::launch(backend, ui::agent_size(window, false), state_dir, launch_dir, config, config_problem);
    app.set_window(window);
    // Cost is read from Claude Code's own records, and only when the user
    // has asked for it. Where the records are is found here, in the real
    // program only.
    if app.config.cost {
        app.ws.read_cost_from(lillignode::cost::claude_folder_from_env());
        app.ws.set_cost_warning(app.config.cost_warning_tokens);
    }
    app.colors = lillignode::look::depth(app.config.colors, |name| std::env::var(name).ok(), cfg!(windows));
    let started = Instant::now();
    input::spawn_reader(tx, Msg::Input);

    // Measurement only: with LILLIGNODE_TIMING=<file>, record for each key how
    // long Lillignode held it before handing it to the agent, and how long until
    // the agent's response was drawn. Nothing is recorded otherwise.
    let mut timing = std::env::var_os("LILLIGNODE_TIMING").and_then(|f| std::fs::File::create(f).ok());
    let mut key_read: Option<Instant> = None;
    let mut handed_over_us = 0u128;
    let mut output_since_key = false;

    let mut draw_error = None;
    let mut painter = ui::Painter::default();
    while !app.quit {
        app.clock = started.elapsed().as_secs();
        // Nothing is written when the screen would be the same: see `Painter`.
        if let Err(e) = painter.paint(&mut terminal, |frame| {
            app.too_small = ui::areas(frame.area()).is_none();
            ui::render(frame, &app)
        }) {
            draw_error = Some(e);
            break;
        }
        if let (Some(t0), true, Some(file)) = (key_read, output_since_key, timing.as_mut()) {
            let _ = writeln!(file, "{handed_over_us} {}", t0.elapsed().as_micros());
            key_read = None;
        }
        output_since_key = false;

        // Block for one message, then take everything else already waiting so
        // a burst of output costs one redraw.
        // Wake at least once a second, so a status that has gone past its
        // deadline is shown without waiting for a key or output.
        let first = match rx.recv_timeout(Duration::from_secs(1)) {
            Ok(msg) => Some(msg),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        app.ws.tick(Instant::now());
        for msg in first.into_iter().chain(rx.try_iter()) {
            match msg {
                Msg::Agent(event) => {
                    output_since_key = key_read.is_some();
                    app.on_agent_event(event);
                }
                Msg::Input(Input::Key(key), read_at) => {
                    app.on_key(key);
                    key_read = Some(read_at);
                    handed_over_us = read_at.elapsed().as_micros();
                }
                Msg::Input(Input::Paste(text), _) => app.on_paste(text),
                Msg::Input(Input::Resize { cols, rows }, _) => {
                    app.set_window(ratatui::layout::Rect::new(0, 0, cols, rows));
                }
            }
        }
        if app.after_events() {
            // Once per agent per change into Needs you, whichever agent is in
            // front: Lillignode cannot tell whether its window is in view. A
            // failed write only loses the bell. Off with "bell": false.
            let mut out = std::io::stdout();
            let _ = out.write_all(b"\x07");
            let _ = out.flush();
        }
    }

    let stopped = app.ws.running_labels();
    let ended = app.ws.ended_labels();
    let saved = app.save();
    app.ws.stop_all();
    restore_terminal();

    if let Some(e) = draw_error {
        eprintln!("Lillignode could not draw to this terminal ({e}) and closed.");
    }
    println!("{}", lillignode::workspace::closing_line(&stopped, &ended));
    let remembered = app.ws.entries().len();
    match saved {
        Ok(()) if remembered > 0 => {
            println!("Your workspace was saved. Its agents reopen next time you run lillignode.")
        }
        Ok(()) => {}
        Err(problem) => eprintln!("{problem}"),
    }
    for problem in &app.start_problems {
        eprintln!("Earlier, on starting: {problem}");
    }
    ExitCode::SUCCESS
}
