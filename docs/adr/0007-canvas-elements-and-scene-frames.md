# ADR 0007: Canvas elements and scene frames (schema v2)

- Status: accepted
- Date: 2026-10-15
- Amends: ADR 0006, decisions 1 and 2

## Context

The producer builds a show on one canvas: elements (camera, screen share, images, stickers,
text) are placed on the canvas itself, and a scene is a frame drawn over it that streams what
it frames. Frames of different sizes over the same elements give different zoom levels.
Schema v1 cannot say this: each scene owns its layers in a private 1920×1080 space and has
no size on the board.

## Decision

1. Schema v2: `Board.elements` holds every element (`Element { id, name, kind, rect, z,
   crop }`, v1's `Layer` renamed) with `rect` in canvas units and ids unique on the board. A
   `Scene` is `{ id, name, frame }`, a 16:9 rectangle in canvas units. One canvas unit is
   one program pixel when a frame is 1920 units wide.
2. Membership is spatial (the maintainer's choice): a scene streams every element whose
   rectangle overlaps its frame, in `z` order, and nothing else.
3. Frames may overlap (a wide shot and a close-up of the same camera), so a frame never owns
   elements: moving or resizing a frame re-frames the canvas, and removing a scene keeps its
   elements.
4. `render_plan` maps the live frame onto the program, `(p − frame.origin) × width /
   frame.w`, and no longer clamps: the compositor clips at the program edge, so an element
   crossing the frame edge is cut, not re-fitted into its visible part as v1's clamp did.
5. Sources stay implied by the kind (one camera, one screen share, one per guest); several
   elements may show the same source. A sticker is an `Image` element with a bundled `src`.
6. Commands: element add/update/remove replace the layer commands and `set_scene_frame`
   replaces `move_scene`; the other commands keep their meaning.
7. Migration (ADR 0006, decision 5): frozen v1 types live in `prostor_model::v1`; loading
   reads `schema_version` first and upgrades v1 boards and imports. Each scene becomes a
   frame, laid out so no layer lands in another scene's frame, and its layers become
   elements moved with it.

## Consequences

- `RenderPlan` keeps its shape. Encoder, ingest, egress and server do not change; the
  compositor must draw rectangles that overflow the program.
- The canvas shows the truth: what lies inside a frame is what it streams. Hiding an element
  in one scene can come later as an optional per-scene field, without a migration.
- Transitions stay edges between scenes; M4 can add a camera move that interpolates frames.
- The scene editor goes away; elements are edited on the canvas, which shows their media
  live through the DOM, separately from the program compositor.
