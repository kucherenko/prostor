//! Commands the producer issues against a board and the events the engine reports back.
//!
//! Every mutation of a [`BoardDocument`](crate::BoardDocument) goes through a [`Command`]
//! applied by `prostor_engine::Engine::apply`, which validates it and answers with the
//! [`Event`]s that describe what changed. Commands carry the ids they create (the editor
//! mints them), so replaying a command log yields the same document, and events are
//! past-tense mirrors of commands; both are what a CRDT or an undo stack will carry later
//! (ADR 0006).

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::{Layer, LayerId, Point, Scene, SceneId, Transition, TransitionId};

/// A mutation of the board or of the live selection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(export)]
pub enum Command {
    /// Adds an empty scene at `position`.
    AddScene {
        id: SceneId,
        name: String,
        position: Point,
    },
    MoveScene {
        id: SceneId,
        position: Point,
    },
    RenameScene {
        id: SceneId,
        name: String,
    },
    /// Removes the scene and every transition touching it; a live scene goes off program.
    RemoveScene {
        id: SceneId,
    },
    /// Adds a layer to a scene (its id must be new within the scene).
    AddLayer {
        scene: SceneId,
        layer: Layer,
    },
    /// Replaces a layer (matched by `layer.id`) wholesale.
    UpdateLayer {
        scene: SceneId,
        layer: Layer,
    },
    RemoveLayer {
        scene: SceneId,
        id: LayerId,
    },
    AddTransition {
        transition: Transition,
    },
    RemoveTransition {
        id: TransitionId,
    },
    /// Puts a scene on program (a cut; M4 animates it along the matching transition).
    SetLive {
        scene: SceneId,
    },
    RenameBoard {
        name: String,
    },
}

/// What changed. Past-tense mirror of [`Command`], plus what cascaded.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(export)]
pub enum Event {
    SceneAdded {
        scene: Scene,
    },
    SceneMoved {
        id: SceneId,
        position: Point,
    },
    SceneRenamed {
        id: SceneId,
        name: String,
    },
    SceneRemoved {
        id: SceneId,
        /// Transitions that went with the scene.
        transitions: Vec<TransitionId>,
    },
    LayerAdded {
        scene: SceneId,
        layer: Layer,
    },
    LayerUpdated {
        scene: SceneId,
        layer: Layer,
    },
    LayerRemoved {
        scene: SceneId,
        id: LayerId,
    },
    TransitionAdded {
        transition: Transition,
    },
    TransitionRemoved {
        id: TransitionId,
    },
    /// The program changed; `None` when the live scene was removed.
    LiveChanged {
        scene: Option<SceneId>,
    },
    BoardRenamed {
        name: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{LayerKind, Rect};
    use serde_json::json;

    #[test]
    fn commands_and_events_use_the_documented_wire_shapes() {
        assert_eq!(
            serde_json::to_value(Command::AddScene {
                id: SceneId::new("s1"),
                name: "Intro".into(),
                position: Point { x: 1.0, y: 2.0 },
            })
            .unwrap(),
            json!({ "type": "add_scene", "id": "s1", "name": "Intro", "position": { "x": 1.0, "y": 2.0 } })
        );
        assert_eq!(
            serde_json::to_value(Command::SetLive {
                scene: SceneId::new("s1")
            })
            .unwrap(),
            json!({ "type": "set_live", "scene": "s1" })
        );
        assert_eq!(
            serde_json::to_value(Event::LiveChanged { scene: None }).unwrap(),
            json!({ "type": "live_changed", "scene": null })
        );
        let layer = Layer {
            id: LayerId::new("l1"),
            name: "Color".into(),
            kind: LayerKind::Color {
                rgba: [1, 2, 3, 255],
            },
            rect: Rect::FULL,
            z: 0,
            crop: None,
        };
        let value = serde_json::to_value(Command::UpdateLayer {
            scene: SceneId::new("s1"),
            layer: layer.clone(),
        })
        .unwrap();
        assert_eq!(value["type"], "update_layer");
        assert_eq!(
            value["layer"]["kind"],
            json!({ "type": "color", "rgba": [1, 2, 3, 255] })
        );
        let back: Command = serde_json::from_value(value).unwrap();
        assert_eq!(
            back,
            Command::UpdateLayer {
                scene: SceneId::new("s1"),
                layer
            }
        );
    }
}
