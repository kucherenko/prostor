//! Scene state machine for prostor (Phase 0 placeholder).
//!
//! The engine owns a [`Board`] and the live-scene selection. It is sans-IO: it is
//! driven by commands and timestamps supplied by the caller and never reads the
//! clock, the network or the DOM. Layout math, transition timing and mixer
//! decisions will be added here as the Phase 0 plan progresses.

#![forbid(unsafe_code)]

pub mod layout;

pub use layout::render_plan;

use prostor_model::{Board, Scene, SceneId};

/// Errors returned by [`Engine`] commands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineError {
    /// The scene id does not exist on the board.
    UnknownScene(SceneId),
}

impl core::fmt::Display for EngineError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            EngineError::UnknownScene(id) => write!(f, "unknown scene `{id}`"),
        }
    }
}

impl std::error::Error for EngineError {}

/// The show state machine.
#[derive(Debug, Clone)]
pub struct Engine {
    board: Board,
    live: Option<SceneId>,
}

impl Engine {
    /// Starts with no live scene.
    pub fn new(board: Board) -> Self {
        Self { board, live: None }
    }

    /// The board this engine runs.
    pub fn board(&self) -> &Board {
        &self.board
    }

    /// The scene currently on program, if any.
    pub fn live_scene(&self) -> Option<&Scene> {
        self.live.as_ref().and_then(|id| self.board.scene(id))
    }

    /// Puts a scene on program immediately (a cut). Transitions come later.
    pub fn select_scene(&mut self, id: &SceneId) -> Result<(), EngineError> {
        if self.board.scene(id).is_none() {
            return Err(EngineError::UnknownScene(id.clone()));
        }
        self.live = Some(id.clone());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use prostor_model::{Point, Scene};

    fn board() -> Board {
        let mut board = Board::new("b", "Test");
        board.scenes.push(Scene {
            id: SceneId::new("s1"),
            name: "One".into(),
            position: Point { x: 0.0, y: 0.0 },
            layers: Vec::new(),
        });
        board
    }

    #[test]
    fn selecting_a_known_scene_puts_it_on_program() {
        let mut engine = Engine::new(board());
        assert!(engine.live_scene().is_none());
        engine.select_scene(&SceneId::new("s1")).unwrap();
        assert_eq!(engine.live_scene().map(|s| s.name.as_str()), Some("One"));
    }

    #[test]
    fn selecting_an_unknown_scene_fails_and_keeps_program() {
        let mut engine = Engine::new(board());
        engine.select_scene(&SceneId::new("s1")).unwrap();
        let err = engine.select_scene(&SceneId::new("missing")).unwrap_err();
        assert_eq!(err, EngineError::UnknownScene(SceneId::new("missing")));
        assert_eq!(engine.live_scene().map(|s| s.id.as_str()), Some("s1"));
    }
}
