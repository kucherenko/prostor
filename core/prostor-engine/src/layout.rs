//! Layout: turns a scene into the flat [`RenderPlan`] the compositor executes.

use prostor_model::{Rect, RenderLayer, RenderPlan, Scene};

/// Computes the drawing order and the destination rectangles of a scene's layers for a
/// program of `program.0 × program.1` pixels.
///
/// Layers are authored in the 1920×1080 space of [`Rect`] (see
/// [`prostor_model::PROGRAM_WIDTH`]); they are scaled to the program and then clamped to
/// it, so a 720p output shows the same picture as 1080p. Layers are sorted by
/// [`prostor_model::Layer::z`] (stable, so equal `z` keeps the scene's order), crops pass
/// through unchanged (they are in source pixels) and every layer is fully opaque.
/// Transitions animate opacity and rectangles later.
pub fn render_plan(scene: &Scene, program: (u32, u32)) -> RenderPlan {
    let (width, height) = program;
    let sx = f64::from(width) / Rect::FULL.w;
    let sy = f64::from(height) / Rect::FULL.h;
    let mut layers: Vec<&prostor_model::Layer> = scene.layers.iter().collect();
    layers.sort_by_key(|layer| layer.z);
    RenderPlan {
        width,
        height,
        layers: layers
            .into_iter()
            .map(|layer| RenderLayer {
                id: layer.id.clone(),
                kind: layer.kind.clone(),
                rect: clamp(
                    scale(layer.rect, sx, sy),
                    f64::from(width),
                    f64::from(height),
                ),
                crop: layer.crop,
                z: layer.z,
                opacity: 1.0,
            })
            .collect(),
    }
}

fn scale(rect: Rect, sx: f64, sy: f64) -> Rect {
    Rect {
        x: rect.x * sx,
        y: rect.y * sy,
        w: rect.w * sx,
        h: rect.h * sy,
    }
}

/// Clamps `rect` to `[0, width] × [0, height]`; a rectangle entirely outside, or with a
/// non-positive size, becomes empty (zero width or height) at its clamped position.
fn clamp(rect: Rect, width: f64, height: f64) -> Rect {
    let x0 = rect.x.max(0.0).min(width);
    let y0 = rect.y.max(0.0).min(height);
    let x1 = (rect.x + rect.w.max(0.0)).max(0.0).min(width);
    let y1 = (rect.y + rect.h.max(0.0)).max(0.0).min(height);
    Rect {
        x: x0,
        y: y0,
        w: (x1 - x0).max(0.0),
        h: (y1 - y0).max(0.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use prostor_model::{Layer, LayerId, LayerKind, Point, SceneId};

    fn layer(id: &str, z: i32, rect: Rect) -> Layer {
        Layer {
            id: LayerId::new(id),
            name: id.to_owned(),
            kind: LayerKind::Color {
                rgba: [0, 0, 0, 255],
            },
            rect,
            z,
            crop: None,
        }
    }

    fn scene(layers: Vec<Layer>) -> Scene {
        Scene {
            id: SceneId::new("s"),
            name: "Scene".into(),
            position: Point { x: 0.0, y: 0.0 },
            layers,
        }
    }

    #[test]
    fn orders_layers_by_z_keeping_scene_order_for_ties() {
        let plan = render_plan(
            &scene(vec![
                layer("top", 5, Rect::FULL),
                layer("first-zero", 0, Rect::FULL),
                layer("second-zero", 0, Rect::FULL),
                layer("below", -1, Rect::FULL),
            ]),
            (1920, 1080),
        );
        let order: Vec<&str> = plan.layers.iter().map(|l| l.id.as_str()).collect();
        assert_eq!(order, ["below", "first-zero", "second-zero", "top"]);
        assert_eq!(plan.width, 1920);
        assert_eq!(plan.height, 1080);
        assert!(plan.layers.iter().all(|l| l.opacity == 1.0));
        assert_eq!(plan.layers[3].z, 5);
    }

    #[test]
    fn clamps_rectangles_to_the_program() {
        let plan = render_plan(
            &scene(vec![
                layer(
                    "overflow",
                    0,
                    Rect {
                        x: 1800.0,
                        y: -50.0,
                        w: 400.0,
                        h: 200.0,
                    },
                ),
                layer(
                    "outside",
                    1,
                    Rect {
                        x: 5000.0,
                        y: 10.0,
                        w: 10.0,
                        h: 10.0,
                    },
                ),
                layer(
                    "negative",
                    2,
                    Rect {
                        x: 10.0,
                        y: 10.0,
                        w: -5.0,
                        h: 20.0,
                    },
                ),
            ]),
            (1920, 1080),
        );
        assert_eq!(
            plan.layers[0].rect,
            Rect {
                x: 1800.0,
                y: 0.0,
                w: 120.0,
                h: 150.0
            }
        );
        assert_eq!(
            plan.layers[1].rect,
            Rect {
                x: 1920.0,
                y: 10.0,
                w: 0.0,
                h: 10.0
            }
        );
        assert_eq!(
            plan.layers[2].rect,
            Rect {
                x: 10.0,
                y: 10.0,
                w: 0.0,
                h: 20.0
            }
        );
    }

    #[test]
    fn scales_from_the_authoring_space_to_the_program() {
        let title = layer(
            "title",
            1,
            Rect {
                x: 96.0,
                y: 880.0,
                w: 1200.0,
                h: 120.0,
            },
        );
        let plan = render_plan(&scene(vec![title]), (1280, 720));
        let rect = plan.layers[0].rect;
        assert!((rect.x - 64.0).abs() < 1e-9);
        assert!((rect.y - 586.6666).abs() < 1e-3, "{rect:?}");
        assert!((rect.w - 800.0).abs() < 1e-9);
        assert!((rect.h - 80.0).abs() < 1e-9);
        assert!(rect.y + rect.h <= 720.0, "the title stays visible on 720p");

        let plan = render_plan(&scene(vec![layer("full", 0, Rect::FULL)]), (3840, 2160));
        assert_eq!(
            plan.layers[0].rect,
            Rect {
                x: 0.0,
                y: 0.0,
                w: 3840.0,
                h: 2160.0
            }
        );
    }

    #[test]
    fn passes_crops_through_and_handles_an_empty_scene() {
        let mut cropped = layer("c", 0, Rect::FULL);
        let crop = Rect {
            x: 10.0,
            y: 20.0,
            w: 640.0,
            h: 360.0,
        };
        cropped.crop = Some(crop);
        let plan = render_plan(&scene(vec![cropped]), (1280, 720));
        assert_eq!(plan.layers[0].crop, Some(crop));
        assert_eq!(
            plan.layers[0].rect,
            Rect {
                x: 0.0,
                y: 0.0,
                w: 1280.0,
                h: 720.0
            },
            "the 1920x1080 full rect scales to a 720p program"
        );

        let empty = render_plan(&scene(vec![]), (1920, 1080));
        assert!(empty.layers.is_empty());
    }
}
