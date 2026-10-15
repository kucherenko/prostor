//! What the compositor draws: a flat, ordered list of layers in program pixels.
//!
//! A [`RenderPlan`] is computed by the engine from the live scene (and, later, from the
//! transition in progress) and handed to the browser's compositor worker, which only
//! executes it. Policy (ordering, clamping, crops, opacity) lives in Rust.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::{LayerId, LayerKind, Rect};

/// One frame's worth of drawing instructions, back to front.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct RenderPlan {
    /// Program width in pixels.
    pub width: u32,
    /// Program height in pixels.
    pub height: u32,
    /// Layers in drawing order: the first is drawn first (bottom), the last on top.
    pub layers: Vec<RenderLayer>,
}

/// One layer to draw.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct RenderLayer {
    /// The source layer; the compositor uses it to find textures and text caches.
    pub id: LayerId,
    pub kind: LayerKind,
    /// Destination rectangle in program pixels, already clamped to the program.
    pub rect: Rect,
    /// Source crop in source pixels, if any.
    pub crop: Option<Rect>,
    /// The source layer's stacking value (informational; `layers` is already sorted).
    pub z: i32,
    /// 0.0 (invisible) to 1.0 (opaque); transitions animate this.
    pub opacity: f32,
}
