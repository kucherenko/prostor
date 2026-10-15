//! The show state machine: a [`Board`] plus the live-scene selection, driven by
//! [`Command`]s and answering with [`Event`]s.
//!
//! Sans-IO: the engine never reads the clock, the network or the DOM. Every mutation is
//! validated here (ids must exist or be new, rectangles must be finite with a positive
//! size and touch the program, transitions must join two different existing scenes), so
//! a document the engine accepted is one the compositor can render. Layout math lives in
//! [`layout`]; transition timing and mixer decisions arrive in M4.

#![forbid(unsafe_code)]

pub mod layout;

pub use layout::render_plan;

use prostor_model::{
    Board, BoardDocument, Command, Event, Layer, LayerId, Point, Rect, RenderPlan, SCHEMA_VERSION,
    Scene, SceneId, Transition, TransitionId,
};

/// Longest name accepted for a board, a scene or a layer.
pub const MAX_NAME_LEN: usize = 120;

/// Why a command or a document was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineError {
    /// The document was written by another schema version.
    UnsupportedSchema(u32),
    UnknownScene(SceneId),
    DuplicateScene(SceneId),
    UnknownLayer(SceneId, LayerId),
    DuplicateLayer(SceneId, LayerId),
    UnknownTransition(TransitionId),
    DuplicateTransition(TransitionId),
    /// A transition's `from` and `to` are the same scene.
    SelfTransition(TransitionId),
    /// A coordinate or size is NaN or infinite.
    NotFinite,
    /// A rectangle has a non-positive width or height.
    EmptyRect,
    /// A rectangle lies entirely outside the program.
    OutsideProgram,
    /// A name is empty or longer than [`MAX_NAME_LEN`].
    BadName,
}

impl core::fmt::Display for EngineError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            EngineError::UnsupportedSchema(v) => write!(f, "unsupported schema version {v}"),
            EngineError::UnknownScene(id) => write!(f, "unknown scene `{id}`"),
            EngineError::DuplicateScene(id) => write!(f, "scene `{id}` exists already"),
            EngineError::UnknownLayer(s, l) => write!(f, "unknown layer `{l}` in scene `{s}`"),
            EngineError::DuplicateLayer(s, l) => {
                write!(f, "layer `{l}` exists already in scene `{s}`")
            }
            EngineError::UnknownTransition(id) => write!(f, "unknown transition `{id}`"),
            EngineError::DuplicateTransition(id) => {
                write!(f, "transition `{id}` exists already")
            }
            EngineError::SelfTransition(id) => {
                write!(f, "transition `{id}` joins a scene to itself")
            }
            EngineError::NotFinite => write!(f, "a coordinate is not a finite number"),
            EngineError::EmptyRect => write!(f, "a rectangle has no area"),
            EngineError::OutsideProgram => write!(f, "a rectangle lies outside the program"),
            EngineError::BadName => write!(f, "a name is empty or too long"),
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
    /// Starts with a board and no live scene. The board is not validated; use
    /// [`Engine::load`] for documents from outside.
    pub fn new(board: Board) -> Self {
        Self { board, live: None }
    }

    /// Loads a persisted document, refusing other schema versions and inconsistent
    /// boards (duplicate ids, dangling transitions, invalid rectangles).
    pub fn load(document: BoardDocument) -> Result<Self, EngineError> {
        if document.schema_version != SCHEMA_VERSION {
            return Err(EngineError::UnsupportedSchema(document.schema_version));
        }
        validate_board(&document.board)?;
        Ok(Self::new(document.board))
    }

    /// The board this engine runs.
    pub fn board(&self) -> &Board {
        &self.board
    }

    /// The board as a persistable document.
    pub fn snapshot(&self) -> BoardDocument {
        BoardDocument::new(self.board.clone())
    }

    /// The id of the scene on program, if any.
    pub fn live(&self) -> Option<&SceneId> {
        self.live.as_ref()
    }

    /// The scene currently on program, if any.
    pub fn live_scene(&self) -> Option<&Scene> {
        self.live.as_ref().and_then(|id| self.board.scene(id))
    }

    /// Puts a scene on program immediately (a cut).
    pub fn select_scene(&mut self, id: &SceneId) -> Result<(), EngineError> {
        if self.board.scene(id).is_none() {
            return Err(EngineError::UnknownScene(id.clone()));
        }
        self.live = Some(id.clone());
        Ok(())
    }

    /// What the compositor draws now for a `program.0 × program.1` output: the live
    /// scene's layers, or nothing. `now_ms` is unused until M4 animates transitions.
    pub fn render_plan(&self, now_ms: f64, program: (u32, u32)) -> RenderPlan {
        let _ = now_ms;
        match self.live_scene() {
            Some(scene) => render_plan(scene, program),
            None => RenderPlan {
                width: program.0,
                height: program.1,
                layers: Vec::new(),
            },
        }
    }

    /// Applies a command. On error nothing changed.
    pub fn apply(&mut self, command: Command) -> Result<Vec<Event>, EngineError> {
        match command {
            Command::AddScene { id, name, position } => {
                check_name(&name)?;
                check_point(position)?;
                if self.board.scene(&id).is_some() {
                    return Err(EngineError::DuplicateScene(id));
                }
                let scene = Scene {
                    id,
                    name,
                    position,
                    layers: Vec::new(),
                };
                self.board.scenes.push(scene.clone());
                Ok(vec![Event::SceneAdded { scene }])
            }
            Command::MoveScene { id, position } => {
                check_point(position)?;
                self.scene_mut(&id)?.position = position;
                Ok(vec![Event::SceneMoved { id, position }])
            }
            Command::RenameScene { id, name } => {
                check_name(&name)?;
                self.scene_mut(&id)?.name = name.clone();
                Ok(vec![Event::SceneRenamed { id, name }])
            }
            Command::RemoveScene { id } => {
                let index = self
                    .board
                    .scenes
                    .iter()
                    .position(|s| s.id == id)
                    .ok_or_else(|| EngineError::UnknownScene(id.clone()))?;
                self.board.scenes.remove(index);
                let (gone, kept): (Vec<Transition>, Vec<Transition>) = self
                    .board
                    .transitions
                    .drain(..)
                    .partition(|t| t.from == id || t.to == id);
                self.board.transitions = kept;
                let mut events = vec![Event::SceneRemoved {
                    id: id.clone(),
                    transitions: gone.into_iter().map(|t| t.id).collect(),
                }];
                if self.live.as_ref() == Some(&id) {
                    self.live = None;
                    events.push(Event::LiveChanged { scene: None });
                }
                Ok(events)
            }
            Command::AddLayer { scene, layer } => {
                check_layer(&layer)?;
                let target = self.scene_mut(&scene)?;
                if target.layers.iter().any(|l| l.id == layer.id) {
                    return Err(EngineError::DuplicateLayer(scene, layer.id));
                }
                target.layers.push(layer.clone());
                Ok(vec![Event::LayerAdded { scene, layer }])
            }
            Command::UpdateLayer { scene, layer } => {
                check_layer(&layer)?;
                let target = self.scene_mut(&scene)?;
                let slot = target
                    .layers
                    .iter_mut()
                    .find(|l| l.id == layer.id)
                    .ok_or_else(|| EngineError::UnknownLayer(scene.clone(), layer.id.clone()))?;
                *slot = layer.clone();
                Ok(vec![Event::LayerUpdated { scene, layer }])
            }
            Command::RemoveLayer { scene, id } => {
                let target = self.scene_mut(&scene)?;
                let index = target
                    .layers
                    .iter()
                    .position(|l| l.id == id)
                    .ok_or_else(|| EngineError::UnknownLayer(scene.clone(), id.clone()))?;
                target.layers.remove(index);
                Ok(vec![Event::LayerRemoved { scene, id }])
            }
            Command::AddTransition { transition } => {
                check_transition(&self.board, &transition)?;
                if self.board.transitions.iter().any(|t| t.id == transition.id) {
                    return Err(EngineError::DuplicateTransition(transition.id));
                }
                self.board.transitions.push(transition.clone());
                Ok(vec![Event::TransitionAdded { transition }])
            }
            Command::RemoveTransition { id } => {
                let index = self
                    .board
                    .transitions
                    .iter()
                    .position(|t| t.id == id)
                    .ok_or_else(|| EngineError::UnknownTransition(id.clone()))?;
                self.board.transitions.remove(index);
                Ok(vec![Event::TransitionRemoved { id }])
            }
            Command::SetLive { scene } => {
                self.select_scene(&scene)?;
                Ok(vec![Event::LiveChanged { scene: Some(scene) }])
            }
            Command::RenameBoard { name } => {
                check_name(&name)?;
                self.board.name = name.clone();
                Ok(vec![Event::BoardRenamed { name }])
            }
        }
    }

    fn scene_mut(&mut self, id: &SceneId) -> Result<&mut Scene, EngineError> {
        self.board
            .scenes
            .iter_mut()
            .find(|s| &s.id == id)
            .ok_or_else(|| EngineError::UnknownScene(id.clone()))
    }
}

fn check_name(name: &str) -> Result<(), EngineError> {
    let trimmed = name.trim();
    if trimmed.is_empty() || name.chars().count() > MAX_NAME_LEN {
        return Err(EngineError::BadName);
    }
    Ok(())
}

fn check_point(point: Point) -> Result<(), EngineError> {
    if !(point.x.is_finite() && point.y.is_finite()) {
        return Err(EngineError::NotFinite);
    }
    Ok(())
}

/// A layer rectangle: finite, with area, and at least partly on the program.
fn check_rect(rect: Rect) -> Result<(), EngineError> {
    if ![rect.x, rect.y, rect.w, rect.h]
        .iter()
        .all(|v| v.is_finite())
    {
        return Err(EngineError::NotFinite);
    }
    if rect.w <= 0.0 || rect.h <= 0.0 {
        return Err(EngineError::EmptyRect);
    }
    let (program_w, program_h) = (Rect::FULL.w, Rect::FULL.h);
    if rect.x >= program_w
        || rect.y >= program_h
        || rect.x + rect.w <= 0.0
        || rect.y + rect.h <= 0.0
    {
        return Err(EngineError::OutsideProgram);
    }
    Ok(())
}

/// A crop: finite with area (it lives in source pixels, so no program bound applies).
fn check_crop(crop: Rect) -> Result<(), EngineError> {
    if ![crop.x, crop.y, crop.w, crop.h]
        .iter()
        .all(|v| v.is_finite())
    {
        return Err(EngineError::NotFinite);
    }
    if crop.w <= 0.0 || crop.h <= 0.0 || crop.x < 0.0 || crop.y < 0.0 {
        return Err(EngineError::EmptyRect);
    }
    Ok(())
}

fn check_layer(layer: &Layer) -> Result<(), EngineError> {
    check_name(&layer.name)?;
    check_rect(layer.rect)?;
    if let Some(crop) = layer.crop {
        check_crop(crop)?;
    }
    Ok(())
}

fn check_transition(board: &Board, transition: &Transition) -> Result<(), EngineError> {
    if transition.from == transition.to {
        return Err(EngineError::SelfTransition(transition.id.clone()));
    }
    for scene in [&transition.from, &transition.to] {
        if board.scene(scene).is_none() {
            return Err(EngineError::UnknownScene(scene.clone()));
        }
    }
    Ok(())
}

/// Every invariant a loaded board must hold.
fn validate_board(board: &Board) -> Result<(), EngineError> {
    check_name(&board.name)?;
    let mut scene_ids = std::collections::HashSet::new();
    for scene in &board.scenes {
        if !scene_ids.insert(&scene.id) {
            return Err(EngineError::DuplicateScene(scene.id.clone()));
        }
        check_name(&scene.name)?;
        check_point(scene.position)?;
        let mut layer_ids = std::collections::HashSet::new();
        for layer in &scene.layers {
            if !layer_ids.insert(&layer.id) {
                return Err(EngineError::DuplicateLayer(
                    scene.id.clone(),
                    layer.id.clone(),
                ));
            }
            check_layer(layer)?;
        }
    }
    let mut transition_ids = std::collections::HashSet::new();
    for transition in &board.transitions {
        if !transition_ids.insert(&transition.id) {
            return Err(EngineError::DuplicateTransition(transition.id.clone()));
        }
        check_transition(board, transition)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use prostor_model::{LayerKind, TransitionKind};

    fn scene_cmd(id: &str) -> Command {
        Command::AddScene {
            id: SceneId::new(id),
            name: format!("Scene {id}"),
            position: Point { x: 0.0, y: 0.0 },
        }
    }

    fn layer(id: &str, rect: Rect) -> Layer {
        Layer {
            id: LayerId::new(id),
            name: id.to_owned(),
            kind: LayerKind::Camera,
            rect,
            z: 0,
            crop: None,
        }
    }

    fn transition(id: &str, from: &str, to: &str) -> Transition {
        Transition {
            id: TransitionId::new(id),
            from: SceneId::new(from),
            to: SceneId::new(to),
            kind: TransitionKind::Cut,
            duration_ms: 0,
        }
    }

    fn engine_with_two_scenes() -> Engine {
        let mut engine = Engine::new(Board::new("b", "Board"));
        engine.apply(scene_cmd("s1")).unwrap();
        engine.apply(scene_cmd("s2")).unwrap();
        engine
    }

    #[test]
    fn scenes_are_added_moved_renamed_and_removed_with_their_transitions() {
        let mut engine = engine_with_two_scenes();
        let events = engine
            .apply(Command::AddTransition {
                transition: transition("t1", "s1", "s2"),
            })
            .unwrap();
        assert!(matches!(events[0], Event::TransitionAdded { .. }));
        engine
            .apply(Command::MoveScene {
                id: SceneId::new("s2"),
                position: Point { x: 10.0, y: 20.0 },
            })
            .unwrap();
        engine
            .apply(Command::RenameScene {
                id: SceneId::new("s2"),
                name: "Two".into(),
            })
            .unwrap();
        let s2 = engine.board().scene(&SceneId::new("s2")).unwrap();
        assert_eq!((s2.position.x, s2.name.as_str()), (10.0, "Two"));

        engine
            .apply(Command::SetLive {
                scene: SceneId::new("s2"),
            })
            .unwrap();
        let events = engine
            .apply(Command::RemoveScene {
                id: SceneId::new("s2"),
            })
            .unwrap();
        assert_eq!(
            events,
            vec![
                Event::SceneRemoved {
                    id: SceneId::new("s2"),
                    transitions: vec![TransitionId::new("t1")],
                },
                Event::LiveChanged { scene: None },
            ]
        );
        assert!(engine.board().transitions.is_empty());
        assert_eq!(engine.live(), None);
        assert_eq!(engine.board().scenes.len(), 1);
    }

    #[test]
    fn refuses_duplicates_unknown_ids_and_bad_names_without_changing_anything() {
        let mut engine = engine_with_two_scenes();
        let before = engine.snapshot();
        assert_eq!(
            engine.apply(scene_cmd("s1")).unwrap_err(),
            EngineError::DuplicateScene(SceneId::new("s1"))
        );
        assert_eq!(
            engine
                .apply(Command::MoveScene {
                    id: SceneId::new("nope"),
                    position: Point { x: 0.0, y: 0.0 }
                })
                .unwrap_err(),
            EngineError::UnknownScene(SceneId::new("nope"))
        );
        assert_eq!(
            engine
                .apply(Command::MoveScene {
                    id: SceneId::new("s1"),
                    position: Point {
                        x: f64::NAN,
                        y: 0.0
                    }
                })
                .unwrap_err(),
            EngineError::NotFinite
        );
        assert_eq!(
            engine
                .apply(Command::RenameScene {
                    id: SceneId::new("s1"),
                    name: "   ".into()
                })
                .unwrap_err(),
            EngineError::BadName
        );
        assert_eq!(
            engine
                .apply(Command::RenameBoard {
                    name: "x".repeat(MAX_NAME_LEN + 1)
                })
                .unwrap_err(),
            EngineError::BadName
        );
        assert_eq!(
            engine
                .apply(Command::SetLive {
                    scene: SceneId::new("nope")
                })
                .unwrap_err(),
            EngineError::UnknownScene(SceneId::new("nope"))
        );
        assert_eq!(
            engine
                .apply(Command::RemoveTransition {
                    id: TransitionId::new("t9")
                })
                .unwrap_err(),
            EngineError::UnknownTransition(TransitionId::new("t9"))
        );
        assert_eq!(
            engine.snapshot(),
            before,
            "a refused command changes nothing"
        );
    }

    #[test]
    fn layers_are_validated_against_the_program() {
        let mut engine = engine_with_two_scenes();
        let s1 = SceneId::new("s1");
        engine
            .apply(Command::AddLayer {
                scene: s1.clone(),
                layer: layer("l1", Rect::FULL),
            })
            .unwrap();
        assert_eq!(
            engine
                .apply(Command::AddLayer {
                    scene: s1.clone(),
                    layer: layer("l1", Rect::FULL),
                })
                .unwrap_err(),
            EngineError::DuplicateLayer(s1.clone(), LayerId::new("l1"))
        );
        let bad_rects = [
            (
                Rect {
                    x: 0.0,
                    y: 0.0,
                    w: 0.0,
                    h: 10.0,
                },
                EngineError::EmptyRect,
            ),
            (
                Rect {
                    x: 2000.0,
                    y: 0.0,
                    w: 10.0,
                    h: 10.0,
                },
                EngineError::OutsideProgram,
            ),
            (
                Rect {
                    x: -100.0,
                    y: 0.0,
                    w: 100.0,
                    h: 10.0,
                },
                EngineError::OutsideProgram,
            ),
            (
                Rect {
                    x: 0.0,
                    y: f64::INFINITY,
                    w: 10.0,
                    h: 10.0,
                },
                EngineError::NotFinite,
            ),
        ];
        for (rect, expected) in bad_rects {
            assert_eq!(
                engine
                    .apply(Command::AddLayer {
                        scene: s1.clone(),
                        layer: layer("l2", rect),
                    })
                    .unwrap_err(),
                expected,
                "{rect:?}"
            );
        }
        // Partly off-program is fine (a layer sliding in).
        engine
            .apply(Command::AddLayer {
                scene: s1.clone(),
                layer: layer(
                    "l2",
                    Rect {
                        x: 1800.0,
                        y: -50.0,
                        w: 400.0,
                        h: 200.0,
                    },
                ),
            })
            .unwrap();

        let mut updated = layer("l2", Rect::FULL);
        updated.z = 5;
        updated.crop = Some(Rect {
            x: 0.0,
            y: 0.0,
            w: 640.0,
            h: 360.0,
        });
        let events = engine
            .apply(Command::UpdateLayer {
                scene: s1.clone(),
                layer: updated.clone(),
            })
            .unwrap();
        assert_eq!(
            events,
            vec![Event::LayerUpdated {
                scene: s1.clone(),
                layer: updated
            }]
        );
        let mut bad_crop = layer("l2", Rect::FULL);
        bad_crop.crop = Some(Rect {
            x: 0.0,
            y: 0.0,
            w: 0.0,
            h: 1.0,
        });
        assert_eq!(
            engine
                .apply(Command::UpdateLayer {
                    scene: s1.clone(),
                    layer: bad_crop
                })
                .unwrap_err(),
            EngineError::EmptyRect
        );
        assert_eq!(
            engine
                .apply(Command::UpdateLayer {
                    scene: s1.clone(),
                    layer: layer("l9", Rect::FULL)
                })
                .unwrap_err(),
            EngineError::UnknownLayer(s1.clone(), LayerId::new("l9"))
        );
        engine
            .apply(Command::RemoveLayer {
                scene: s1.clone(),
                id: LayerId::new("l1"),
            })
            .unwrap();
        assert_eq!(engine.board().scene(&s1).unwrap().layers.len(), 1);
        assert_eq!(
            engine
                .apply(Command::RemoveLayer {
                    scene: SceneId::new("s9"),
                    id: LayerId::new("l1"),
                })
                .unwrap_err(),
            EngineError::UnknownScene(SceneId::new("s9"))
        );
    }

    #[test]
    fn transitions_need_two_different_existing_scenes() {
        let mut engine = engine_with_two_scenes();
        assert_eq!(
            engine
                .apply(Command::AddTransition {
                    transition: transition("t", "s1", "s1")
                })
                .unwrap_err(),
            EngineError::SelfTransition(TransitionId::new("t"))
        );
        assert_eq!(
            engine
                .apply(Command::AddTransition {
                    transition: transition("t", "s1", "s9")
                })
                .unwrap_err(),
            EngineError::UnknownScene(SceneId::new("s9"))
        );
        engine
            .apply(Command::AddTransition {
                transition: transition("t", "s1", "s2"),
            })
            .unwrap();
        assert_eq!(
            engine
                .apply(Command::AddTransition {
                    transition: transition("t", "s2", "s1")
                })
                .unwrap_err(),
            EngineError::DuplicateTransition(TransitionId::new("t"))
        );
        engine
            .apply(Command::RemoveTransition {
                id: TransitionId::new("t"),
            })
            .unwrap();
        assert!(engine.board().transitions.is_empty());
    }

    #[test]
    fn render_plan_follows_the_live_scene() {
        let mut engine = engine_with_two_scenes();
        assert!(engine.render_plan(0.0, (1920, 1080)).layers.is_empty());
        engine
            .apply(Command::AddLayer {
                scene: SceneId::new("s1"),
                layer: layer("l1", Rect::FULL),
            })
            .unwrap();
        engine
            .apply(Command::SetLive {
                scene: SceneId::new("s1"),
            })
            .unwrap();
        let plan = engine.render_plan(0.0, (1280, 720));
        assert_eq!(plan.layers.len(), 1);
        assert_eq!((plan.width, plan.height), (1280, 720));
        engine
            .apply(Command::SetLive {
                scene: SceneId::new("s2"),
            })
            .unwrap();
        assert!(engine.render_plan(0.0, (1920, 1080)).layers.is_empty());
    }

    #[test]
    fn load_checks_the_schema_and_the_invariants_and_snapshot_round_trips() {
        let mut engine = engine_with_two_scenes();
        engine
            .apply(Command::AddTransition {
                transition: transition("t", "s1", "s2"),
            })
            .unwrap();
        let doc = engine.snapshot();
        assert_eq!(doc.schema_version, SCHEMA_VERSION);
        let loaded = Engine::load(doc.clone()).unwrap();
        assert_eq!(loaded.snapshot(), doc);
        assert_eq!(loaded.live(), None, "the live selection is not persisted");

        let mut other_version = doc.clone();
        other_version.schema_version = 2;
        assert_eq!(
            Engine::load(other_version).unwrap_err(),
            EngineError::UnsupportedSchema(2)
        );
        let mut dangling = doc.clone();
        dangling.board.scenes.pop();
        assert_eq!(
            Engine::load(dangling).unwrap_err(),
            EngineError::UnknownScene(SceneId::new("s2"))
        );
        let mut duplicate = doc.clone();
        let copy = duplicate.board.scenes[0].clone();
        duplicate.board.scenes.push(copy);
        assert_eq!(
            Engine::load(duplicate).unwrap_err(),
            EngineError::DuplicateScene(SceneId::new("s1"))
        );
        let mut bad_layer = doc;
        bad_layer.board.scenes[0].layers.push(layer(
            "l",
            Rect {
                x: 0.0,
                y: 0.0,
                w: -1.0,
                h: 1.0,
            },
        ));
        assert_eq!(Engine::load(bad_layer).unwrap_err(), EngineError::EmptyRect);
    }
}
