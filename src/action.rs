//! What a person can ask Lillignode to do, by name and never by key.
//!
//! A face turns what the person does into one of these; the terminal face
//! turns the menu's keys into them (`App::command`). Each acts on the agent
//! in front. Opening the menu, the history, the status report and the form
//! for a new agent are not actions: they are the face's own.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Bring the next agent in the list to the front.
    Next,
    /// Bring the previous agent to the front.
    Previous,
    /// Start the agent in front again. Refused if it is running.
    StartAgain,
    /// Close the agent in front. Always asked first.
    Close,
    /// Put the agent in front and the next one side by side. Refused with
    /// the reason when they cannot be shown, and asked first while an agent
    /// is waiting for the user.
    ShowTwo,
    /// Back to one agent on screen. Asked first while an agent is waiting.
    ShowOne,
    /// Stop every agent and close. Asked first while one may be busy.
    Quit,
}
