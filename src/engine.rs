//! The engine: the agents and the rules of the product, apart from any
//! screen or keyboard. A face reads what it holds and asks it, by name, to
//! do things.
//!
//! The rules are moving here from the terminal face (`app.rs`) one at a
//! time; until each has moved, the face still holds it.

use crate::agent::AgentBackend;
use crate::workspace::Workspace;

pub struct Engine<B: AgentBackend> {
    /// The open agents and what became of each.
    pub ws: Workspace<B>,
}

impl<B: AgentBackend> Engine<B> {
    pub fn new(ws: Workspace<B>) -> Self {
        Self { ws }
    }
}
