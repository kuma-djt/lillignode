use std::io::Write;
use std::process::ExitCode;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant, SystemTime};

use crossterm::event::{DisableBracketedPaste, EnableBracketedPaste};
use crossterm::execute;

use lillignode::agent::AgentEvent;
use lillignode::app::App;
use lillignode::config::{self, Config};
use lillignode::input::{self, Input};
use lillignode::local::LocalAgents;
use lillignode::{page, snapshot, state, ui};

/// A copy for the page is published no more often than this, however fast
/// agents write.
const PUBLISH_EVERY: Duration = Duration::from_millis(50);

enum Msg {
    /// A key, paste or resize, and when it was read.
    Input(Input, Instant),
    Agent(AgentEvent),
    /// Text typed on the page for an agent.
    Typed(page::Typed),
    /// Something done to a card of the Board on the page.
    Acted(page::Acted),
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

To see your agents in a browser on this machine, add \"page\": true to that
file. It is off unless you ask. Its address is http://127.0.0.1:7501/ (set
\"page_port\" for another port) and a random word, the same from one start
to the next; Lillignode shows the whole address when it starts and in its
status report. The menu, then w, makes a new word. Anyone with the address
can see every agent's name, status and screen.

The page is read only unless you also add \"page_can_type\": true. Then it
has a box to type to the agent it shows, and anyone with the address can
type to your agents.

The page's Board shows cards from board.json beside your workspace. Add
\"page_can_act\": true and its cards get buttons; what you press is added to
board-outbox.jsonl there. Lillignode never writes to board.json.

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
                    if config.page {
                        println!(
                            "page:      http://127.0.0.1:{}/ and a word; Lillignode shows it. {}",
                            config.page_port,
                            match (config.page_can_type, config.page_can_act) {
                                (false, false) => "Read only.",
                                (true, false) => "It can type to agents.",
                                (false, true) => "It can act on the Board.",
                                (true, true) => "It can type to agents and act on the Board.",
                            }
                        );
                        println!("page word: {}", dir.join(page::WORD_FILE).display());
                        println!("board:     {}", dir.join(lillignode::board::FILE).display());
                        if config.page_can_act {
                            println!("outbox:    {}", dir.join(lillignode::board::OUTBOX).display());
                        }
                    }
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
    // Where the page's word is kept: beside the workspace.
    let page_folder = state_dir.as_ref().ok().cloned();
    let mut app =
        App::launch(backend, ui::agent_size(window, false), state_dir, launch_dir, config, config_problem);
    app.set_window(window);
    // For the first screen: which agents are on this machine (looked for,
    // not started), and whether there is a settings file at all.
    app.found_agents = Some(lillignode::kind::found_on_path(
        &std::env::var_os("PATH").unwrap_or_default(),
        &lillignode::kind::program_extensions(),
        |p| p.is_file(),
    ));
    app.no_settings_file = matches!(&page_folder, Some(dir) if !dir.join(lillignode::config::FILE).exists());
    // Cost is read from Claude Code's own records, and only when the user
    // has asked for it. Where the records are is found here, in the real
    // program only.
    if app.config.cost {
        app.engine.ws.read_cost_from(lillignode::cost::claude_folder_from_env());
        app.engine.ws.set_cost_warning(app.config.cost_warning_tokens);
    }
    app.colors = lillignode::look::depth(app.config.colors, |name| std::env::var(name).ok(), cfg!(windows));
    // The page: only when the user has asked for it. It is handed the
    // reading half; this loop publishes to the other. Only when the user
    // has also asked that it may type is it handed anything else: a way to
    // put what was typed on this loop's own queue, behind the keyboard.
    let publisher = app.config.page.then(snapshot::channel).and_then(|(publisher, reader)| {
        let typing = app.config.page_can_type.then(|| {
            let tx = tx.clone();
            let pass: page::Typing = std::sync::Arc::new(move |typed| tx.send(Msg::Typed(typed)).is_ok());
            pass
        });
        let acting = app.config.page_can_act.then(|| {
            let tx = tx.clone();
            let pass: page::Acting = std::sync::Arc::new(move |acted| tx.send(Msg::Acted(acted)).is_ok());
            pass
        });
        // The Board's adapter, if the settings name one: started here, in
        // the folder Lillignode saves in, and handed to the page and to
        // the app (which sends it what was done to a card).
        let adapter = app.config.board_adapter.clone().map(|named| {
            let folder = page_folder.clone().unwrap_or_else(|| std::path::PathBuf::from("."));
            lillignode::adapter::Link::start(&named.command, &named.args, &named.env, &folder)
                .unwrap_or_else(|why| lillignode::adapter::Link::failed(&why))
        });
        app.board_adapter = adapter.clone();
        if let Some(named) = app.config.board_adapter.clone() {
            app.adapter_named(&named.command, &named.args);
        }
        let ways = page::Ways { typing, acting, adapter };
        match page::serve(reader, app.config.page_port, page_folder.as_deref(), ways) {
            Ok(served) => {
                app.page_serving(served);
                Some(publisher)
            }
            Err(why) => {
                app.page_started(Err(why));
                None
            }
        }
    });
    let mut published_at: Option<Instant> = None;
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
    while !app.engine.closing() {
        app.clock = started.elapsed().as_secs();
        // The window's size is asked for, not only heard about. A resize
        // that arrives in the same moment as keys can go unreported (seen
        // on Linux and macOS); the screen would then be drawn at the new
        // size while text was fitted, and agents sized, for the old one.
        if let Ok(size) = terminal.size() {
            let now = ratatui::layout::Rect::new(0, 0, size.width, size.height);
            if now != app.window() {
                app.set_window(now);
            }
        }
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
        // deadline is shown without waiting for a key or output. Sooner
        // when a copy for the page was held back, so the page is never more
        // than a moment behind.
        let mut wait = Duration::from_secs(1);
        if let Some(publisher) = &publisher {
            // Each turn of this loop follows something that may have
            // changed what the page shows, or a second of quiet.
            let since = published_at.map_or(PUBLISH_EVERY, |at| at.elapsed());
            if since >= PUBLISH_EVERY {
                // Only the screens an open page is showing are copied.
                let screens = publisher.screens_shown();
                publisher.publish(app.engine.published(SystemTime::now(), &screens));
                published_at = Some(Instant::now());
            } else {
                wait = PUBLISH_EVERY - since;
            }
        }
        let first = match rx.recv_timeout(wait) {
            Ok(msg) => Some(msg),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        app.engine.ws.tick(Instant::now());
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
                // Its sender has stopped waiting and been told it is not
                // known whether it was done: now it is known. It was not.
                Msg::Acted(acted) if acted.asked.elapsed() > page::ANSWER_WITHIN => {}
                Msg::Typed(typed) if typed.asked.elapsed() > page::ANSWER_WITHIN => {}
                Msg::Acted(acted) => {
                    let seen = lillignode::board::Seen {
                        title: acted.title.clone(),
                        recommendation: acted.recommendation.clone(),
                    };
                    let (outcome, words) =
                        app.acted_from_page(&acted.card, &acted.action, &acted.note, &seen, acted.undo);
                    let _ = acted.answer.send(page::TypedAnswer { outcome, words });
                }
                Msg::Typed(typed) => {
                    let (outcome, words) = if typed.enter {
                        app.enter_from_page(&typed.run, typed.agent)
                    } else {
                        app.typed_from_page(&typed.run, typed.agent, &typed.text, typed.agreed)
                    };
                    // A page that has stopped waiting is told nothing.
                    let _ = typed.answer.send(page::TypedAnswer { outcome, words });
                }
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
    // The page is told Lillignode has gone, not left to find out.
    drop(publisher);
    // The Board's adapter ends with Lillignode; anything still inside its
    // ten seconds to undo is not sent.
    if let Some(adapter) = &app.board_adapter {
        adapter.stop();
    }

    let stopped = app.engine.ws.running_labels();
    let ended = app.engine.ws.ended_labels();
    let saved = app.save();
    app.engine.ws.stop_all();
    restore_terminal();

    if let Some(e) = draw_error {
        eprintln!("Lillignode could not draw to this terminal ({e}) and closed.");
    }
    println!("{}", lillignode::workspace::closing_line(&stopped, &ended));
    let remembered = app.engine.ws.entries().len();
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
