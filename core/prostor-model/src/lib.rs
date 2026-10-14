//! Domain model of a prostor board document.
//!
//! This crate is pure data. Every type is serializable with serde and mirrored to
//! TypeScript with `ts-rs` (`just gen-types` writes `web/src/lib/generated/`). It
//! performs no I/O and compiles for native targets and `wasm32-unknown-unknown`.
//!
//! Identity is always an explicit id newtype, never an array index, so that
//! mutations can later be expressed as commands and replicated through a CRDT.
//!
//! Stream keys are deliberately not part of the document. A [`Destination`] holds
//! only the public RTMP URL; keys are supplied at stream start, held in memory on
//! the server and never persisted or logged.

#![forbid(unsafe_code)]

pub mod ingest;
pub mod render;

pub use ingest::{
    EgressKind, EgressPhase, EgressState, IngestClientMessage, IngestErrorCode,
    IngestServerMessage, StreamInfo,
};
pub use render::{RenderLayer, RenderPlan};

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Version of the board document schema. Bump on every incompatible change and
/// add a migration for the previous version.
pub const SCHEMA_VERSION: u32 = 1;

/// Width of the program output in pixels. Layer rectangles use this space.
pub const PROGRAM_WIDTH: u32 = 1920;

/// Height of the program output in pixels. Layer rectangles use this space.
pub const PROGRAM_HEIGHT: u32 = 1080;

macro_rules! id_newtype {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        ///
        /// Serializes as a plain string. Values are opaque; stable ids are produced by
        /// the editor and never reused.
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, TS)]
        #[ts(export)]
        pub struct $name(pub String);

        impl $name {
            /// Wraps an existing id string.
            pub fn new(id: impl Into<String>) -> Self {
                Self(id.into())
            }

            /// Borrows the id as a string slice.
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl From<&str> for $name {
            fn from(id: &str) -> Self {
                Self(id.to_owned())
            }
        }

        impl From<String> for $name {
            fn from(id: String) -> Self {
                Self(id)
            }
        }

        impl core::fmt::Display for $name {
            fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

id_newtype!(
    /// Identifies a [`Board`].
    BoardId
);
id_newtype!(
    /// Identifies a [`Scene`] within a board.
    SceneId
);
id_newtype!(
    /// Identifies a [`Layer`] within a scene.
    LayerId
);
id_newtype!(
    /// Identifies a [`Transition`] within a board.
    TransitionId
);
id_newtype!(
    /// Identifies a [`Participant`] (producer or guest) in a show.
    ParticipantId
);
id_newtype!(
    /// Identifies a streaming [`Destination`].
    DestinationId
);

/// A point on the infinite board, in board units.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

/// An axis-aligned rectangle in program pixels ([`PROGRAM_WIDTH`] × [`PROGRAM_HEIGHT`]).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Rect {
    /// The full program frame.
    pub const FULL: Rect = Rect {
        x: 0.0,
        y: 0.0,
        w: PROGRAM_WIDTH as f64,
        h: PROGRAM_HEIGHT as f64,
    };
}

/// The persisted unit: a schema version plus the board it describes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct BoardDocument {
    /// Always [`SCHEMA_VERSION`] for documents written by this version of the model.
    pub schema_version: u32,
    pub board: Board,
}

impl BoardDocument {
    /// Wraps a board with the current [`SCHEMA_VERSION`].
    pub fn new(board: Board) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            board,
        }
    }
}

/// The infinite board: a graph of scenes connected by transitions, plus the
/// participants and destinations of the show.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Board {
    pub id: BoardId,
    pub name: String,
    pub scenes: Vec<Scene>,
    pub transitions: Vec<Transition>,
    pub participants: Vec<Participant>,
    pub destinations: Vec<Destination>,
}

impl Board {
    /// Creates an empty board.
    pub fn new(id: impl Into<BoardId>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            scenes: Vec::new(),
            transitions: Vec::new(),
            participants: Vec::new(),
            destinations: Vec::new(),
        }
    }

    /// Looks up a scene by id.
    pub fn scene(&self, id: &SceneId) -> Option<&Scene> {
        self.scenes.iter().find(|scene| &scene.id == id)
    }
}

/// A 16:9 composition placed on the board.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Scene {
    pub id: SceneId,
    pub name: String,
    /// Where the scene node sits on the board.
    pub position: Point,
    /// Layers composited back to front; ties are broken by [`Layer::z`].
    pub layers: Vec<Layer>,
}

/// One visual element of a scene.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Layer {
    pub id: LayerId,
    pub name: String,
    pub kind: LayerKind,
    /// Placement in program pixels.
    pub rect: Rect,
    /// Stacking order; higher values are drawn on top.
    pub z: i32,
    /// Optional crop applied to the source before it is fitted into `rect`,
    /// in source pixels.
    pub crop: Option<Rect>,
}

/// What a layer shows. Serialized with an internal `type` tag.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(export)]
pub enum LayerKind {
    /// The producer's camera.
    Camera,
    /// The producer's screen share.
    ScreenShare,
    /// A still image.
    Image { src: String },
    /// A text label.
    Text { text: String },
    /// A solid color; components are 0–255.
    Color { rgba: [u8; 4] },
    /// A remote guest's video.
    Guest { participant_id: ParticipantId },
}

/// A directed edge between two scenes on the board.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Transition {
    pub id: TransitionId,
    pub from: SceneId,
    pub to: SceneId,
    pub kind: TransitionKind,
    /// Ignored for [`TransitionKind::Cut`].
    pub duration_ms: u32,
}

/// How the program moves from one scene to the next.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(export)]
pub enum TransitionKind {
    Cut,
    Crossfade,
    Slide { direction: SlideDirection },
}

/// Direction the incoming scene slides in from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
pub enum SlideDirection {
    Left,
    Right,
    Up,
    Down,
}

/// Someone taking part in the show.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Participant {
    pub id: ParticipantId,
    pub display_name: String,
    pub role: ParticipantRole,
}

/// Role of a participant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
pub enum ParticipantRole {
    Producer,
    Guest,
}

/// Where the program output is published. Holds no secret: the stream key is
/// supplied separately at stream start and never persisted.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Destination {
    pub id: DestinationId,
    pub name: String,
    pub kind: DestinationKind,
    /// RTMP(S) ingest URL without the stream key, e.g. `rtmp://live.twitch.tv/app`.
    pub rtmp_url: String,
}

/// Platform behind a destination; drives UI hints only, egress is identical.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
pub enum DestinationKind {
    YouTube,
    Twitch,
    TikTok,
    Telegram,
    Custom,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample() -> BoardDocument {
        let mut board = Board::new("b1", "Launch show");
        board.scenes.push(Scene {
            id: SceneId::new("s1"),
            name: "Intro".into(),
            position: Point { x: 100.0, y: 200.0 },
            layers: vec![
                Layer {
                    id: LayerId::new("l1"),
                    name: "Camera".into(),
                    kind: LayerKind::Camera,
                    rect: Rect::FULL,
                    z: 0,
                    crop: None,
                },
                Layer {
                    id: LayerId::new("l2"),
                    name: "Title".into(),
                    kind: LayerKind::Text {
                        text: "Welcome".into(),
                    },
                    rect: Rect {
                        x: 96.0,
                        y: 880.0,
                        w: 1200.0,
                        h: 120.0,
                    },
                    z: 1,
                    crop: Some(Rect {
                        x: 0.0,
                        y: 0.0,
                        w: 640.0,
                        h: 360.0,
                    }),
                },
            ],
        });
        board.scenes.push(Scene {
            id: SceneId::new("s2"),
            name: "Guest".into(),
            position: Point { x: 600.0, y: 200.0 },
            layers: vec![Layer {
                id: LayerId::new("l3"),
                name: "Guest".into(),
                kind: LayerKind::Guest {
                    participant_id: ParticipantId::new("p2"),
                },
                rect: Rect::FULL,
                z: 0,
                crop: None,
            }],
        });
        board.transitions.push(Transition {
            id: TransitionId::new("t1"),
            from: SceneId::new("s1"),
            to: SceneId::new("s2"),
            kind: TransitionKind::Slide {
                direction: SlideDirection::Left,
            },
            duration_ms: 500,
        });
        board.participants.push(Participant {
            id: ParticipantId::new("p2"),
            display_name: "Ada".into(),
            role: ParticipantRole::Guest,
        });
        board.destinations.push(Destination {
            id: DestinationId::new("d1"),
            name: "Twitch".into(),
            kind: DestinationKind::Twitch,
            rtmp_url: "rtmp://live.twitch.tv/app".into(),
        });
        BoardDocument::new(board)
    }

    #[test]
    fn round_trips_through_json() {
        let doc = sample();
        let json = serde_json::to_string_pretty(&doc).unwrap();
        let back: BoardDocument = serde_json::from_str(&json).unwrap();
        assert_eq!(doc, back);
    }

    #[test]
    fn document_carries_the_schema_version() {
        let value = serde_json::to_value(sample()).unwrap();
        assert_eq!(value["schema_version"], json!(SCHEMA_VERSION));
        assert_eq!(SCHEMA_VERSION, 1);
    }

    #[test]
    fn ids_serialize_as_plain_strings() {
        assert_eq!(
            serde_json::to_string(&SceneId::new("s1")).unwrap(),
            "\"s1\""
        );
        let id: LayerId = serde_json::from_str("\"l9\"").unwrap();
        assert_eq!(id, LayerId::new("l9"));
    }

    #[test]
    fn enums_use_the_documented_wire_shapes() {
        assert_eq!(
            serde_json::to_value(LayerKind::Text { text: "hi".into() }).unwrap(),
            json!({ "type": "text", "text": "hi" })
        );
        assert_eq!(
            serde_json::to_value(LayerKind::ScreenShare).unwrap(),
            json!({ "type": "screen_share" })
        );
        assert_eq!(
            serde_json::to_value(TransitionKind::Slide {
                direction: SlideDirection::Up
            })
            .unwrap(),
            json!({ "type": "slide", "direction": "up" })
        );
        assert_eq!(
            serde_json::to_value(DestinationKind::YouTube).unwrap(),
            json!("youtube")
        );
    }

    #[test]
    fn board_looks_up_scenes_by_id() {
        let doc = sample();
        assert_eq!(
            doc.board
                .scene(&SceneId::new("s2"))
                .map(|s| s.name.as_str()),
            Some("Guest")
        );
        assert!(doc.board.scene(&SceneId::new("nope")).is_none());
    }
}
