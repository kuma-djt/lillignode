//! A plain download: no settings file. Every ability of the page, and every
//! outside program, is off; the page itself is not served. What a person
//! must switch on is never on by itself.

use lillignode::config::{self, Colors, Config, PageLook, WelcomeLook};

#[test]
fn a_plain_download_has_every_page_ability_and_every_outside_program_off() {
    // As a plain download starts: a state folder with no settings file in it.
    let dir = std::env::temp_dir().join(format!("lillignode-defaults-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let (loaded, problem) = config::load(&dir);
    std::fs::remove_dir_all(&dir).ok();
    assert!(problem.is_none(), "no settings file is not a problem: {problem:?}");

    for c in [&loaded, &Config::default()] {
        // The page is not served, so nothing of it can be reached.
        assert!(!c.page, "the page is off");
        assert!(!c.page_can_type, "typing from the page is off");
        assert!(!c.page_can_act, "acting on the Board from the page is off");
        assert!(!c.page_can_start, "starting teams from the page is off");
        assert!(!c.page_notify, "notifications are off");
        // No program of anyone else's is started or sent anything.
        assert!(c.board_adapter.is_none(), "no Board program is started");
        assert!(c.helper.is_none(), "no helper is started or sent a screen");
        assert!(c.withheld_from_agents().is_empty(), "nothing is named, so nothing is withheld");
        // Cost is off, as since 0.1.
        assert!(!c.cost, "cost is off");
        // How things look is chosen, not switched on: the defaults the owner picked.
        assert_eq!(c.page_look, PageLook::Cards);
        assert_eq!(c.welcome_look, WelcomeLook::Wide);
        assert_eq!(c.colors, Colors::default());
    }
}
