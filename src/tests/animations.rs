use std::fmt::Write as _;
use std::time::Duration;

use insta::assert_snapshot;
use niri_config::animations::{Curve, EasingParams, Kind};
use niri_config::Config;
use niri_ipc::SizeChange;
use smithay::utils::{Logical, Point, Size};
use wayland_client::protocol::wl_surface::WlSurface;

use super::client::ClientId;
use super::*;
use crate::layout::{ActivateWindow, AddWindowTarget};
use crate::niri::Niri;
use crate::utils::ResizeEdge;
use crate::window::mapped::MappedId;
use crate::window::Mapped;

fn format_tiles(niri: &Niri) -> String {
    let mut buf = String::new();
    let ws = niri.layout.active_workspace().unwrap();
    let mut tiles: Vec<_> = ws.tiles_with_render_positions().collect();

    // We sort by id since that gives us a consistent order (from first opened to last), but we
    // don't print the id since it's nondeterministic (the id is a global counter across all
    // running tests in the same binary).
    tiles.sort_by_key(|(tile, _, _)| tile.window().id().get());
    for (tile, pos, _visible) in tiles {
        let Size { w, h, .. } = tile.animated_tile_size();
        let Point { x, y, .. } = pos;
        writeln!(&mut buf, "{w:>3.0} × {h:>3.0} at x:{x:>3.0} y:{y:>3.0}").unwrap();
    }
    buf
}

fn tile_geometry_for(niri: &Niri, id: MappedId) -> (Point<i32, Logical>, Size<i32, Logical>) {
    let ws = niri.layout.active_workspace().unwrap();
    let (tile, pos, visible) = ws
        .tiles_with_render_positions()
        .find(|(tile, _, _)| tile.window().id() == id)
        .unwrap();
    assert!(visible);
    (pos.to_i32_round(), tile.animated_tile_size().to_i32_round())
}

fn create_window(f: &mut Fixture, id: ClientId, w: u16, h: u16) -> WlSurface {
    let window = f.client(id).create_window();
    let surface = window.surface.clone();
    window.commit();
    f.roundtrip(id);

    let window = f.client(id).window(&surface);
    window.attach_new_buffer();
    window.set_size(w, h);
    window.ack_last_and_commit();
    f.roundtrip(id);

    surface
}

fn create_window_mirror_next_to(
    f: &mut Fixture,
    source_id: MappedId,
    next_to: MappedId,
) -> MappedId {
    let niri = f.niri();
    let mapped = niri
        .layout
        .windows()
        .find(|(_, mapped)| mapped.id() == source_id)
        .map(|(_, mapped)| mapped)
        .unwrap();
    let mirror = Mapped::new_mirror(mapped);
    let mirror_id = mirror.id();
    niri.layout.add_window(
        mirror,
        AddWindowTarget::NextTo(&next_to),
        None,
        None,
        false,
        false,
        false,
        ActivateWindow::No,
    );
    mirror_id
}

fn set_time(niri: &mut Niri, time: Duration) {
    // This is a bit involved because we're dealing with an AdjustableClock that maintains its own
    // internal current_time.

    // First, reset current_time to zero by matching unadjusted time to it (at rate 0.0), then
    // setting unadjusted time to zero at rate 1.0 (causing current_time to also go to zero).
    let now = niri.clock.now();
    niri.clock.set_unadjusted(now);
    let _ = niri.clock.now();
    niri.clock.set_unadjusted(Duration::ZERO);
    niri.clock.set_rate(1.0);
    let _ = niri.clock.now();

    // Now, set the desired time at rate 1.0.
    niri.clock.set_unadjusted(time);
    let _ = niri.clock.now();

    // Freeze the clock so that clear() inside the niri loop callback followed by some get()
    // doesn't replace it with the monotonic time.
    niri.clock.set_rate(0.0);
}

fn set_up_base(with_renderer: bool) -> Fixture {
    const LINEAR: Kind = Kind::Easing(EasingParams {
        duration_ms: 1000,
        curve: Curve::Linear,
    });

    let mut config = Config::default();
    config.layout.gaps = 0.0;
    config.animations.window_resize.anim.kind = LINEAR;
    config.animations.window_movement.0.kind = LINEAR;

    let mut f = Fixture::with_config(config);
    if with_renderer {
        f.niri_state().backend.headless().add_renderer().unwrap();
    }
    f.add_output(1, (1920, 1080));

    f
}

// Sets up a fixture with linear animations, a renderer, and an output.
fn set_up() -> Fixture {
    set_up_base(true)
}

fn set_up_without_renderer() -> Fixture {
    set_up_base(false)
}

fn set_up_two_in_column() -> (Fixture, ClientId, WlSurface, WlSurface) {
    let mut f = set_up();

    let id = f.add_client();

    let surface1 = create_window(&mut f, id, 100, 100);
    let surface2 = create_window(&mut f, id, 200, 200);
    f.double_roundtrip(id);

    let _ = f.client(id).window(&surface1).recent_configures();
    let _ = f.client(id).window(&surface2).recent_configures();

    // Consume into one column.
    f.niri().layout.focus_left();
    f.niri().layout.consume_into_column();
    f.double_roundtrip(id);

    // Commit for the column consume.
    let window = f.client(id).window(&surface1);
    window.ack_last_and_commit();

    let window = f.client(id).window(&surface2);
    window.ack_last_and_commit();

    f.double_roundtrip(id);

    set_time(f.niri(), Duration::ZERO);
    f.niri_complete_animations();

    (f, id, surface1, surface2)
}

#[test]
fn egl_height_resize_animates_next_y() {
    let (mut f, id, surface1, surface2) = set_up_two_in_column();

    // Issue a resize.
    f.niri()
        .layout
        .set_window_height(None, SizeChange::AdjustFixed(-50));
    f.double_roundtrip(id);

    // The top window shrinks in response, the bottom remains as is.
    let window = f.client(id).window(&surface1);
    window.set_size(100, 50);
    window.ack_last_and_commit();
    let window = f.client(id).window(&surface2);
    window.ack_last_and_commit();

    // This starts the resize animation for the top window and the Y move for the bottom.
    f.roundtrip(id);

    // No time had passed yet, so we're at the initial state.
    assert_snapshot!(format_tiles(f.niri()), @r"
    100 × 100 at x:  0 y:  0
    200 × 200 at x:  0 y:100
    ");

    // Advance the time halfway.
    set_time(f.niri(), Duration::from_millis(500));
    f.niri().advance_animations();

    // Top window is half-resized at 75 px tall, bottom window is at y=75 matching it.
    assert_snapshot!(format_tiles(f.niri()), @r"
    100 ×  75 at x:  0 y:  0
    200 × 200 at x:  0 y: 75
    ");

    // Advance the time to completion.
    set_time(f.niri(), Duration::from_millis(1000));
    f.niri().advance_animations();

    // Final state at 50 px.
    assert_snapshot!(format_tiles(f.niri()), @r"
    100 ×  50 at x:  0 y:  0
    200 × 200 at x:  0 y: 50
    ");
}

#[test]
fn egl_clientside_height_change_doesnt_animate() {
    let (mut f, id, surface1, _surface2) = set_up_two_in_column();

    // The initial state.
    assert_snapshot!(format_tiles(f.niri()), @r"
    100 × 100 at x:  0 y:  0
    200 × 200 at x:  0 y:100
    ");

    // The top window shrinks by itself, without a niri-issued resize.
    let window = f.client(id).window(&surface1);
    window.set_size(100, 50);
    window.commit();

    // This does not start any animations.
    f.roundtrip(id);

    // No time had passed yet, but we are at the final state right away.
    assert_snapshot!(format_tiles(f.niri()), @r"
    100 ×  50 at x:  0 y:  0
    200 × 200 at x:  0 y: 50
    ");
}

#[test]
fn mirror_consume_into_column_snaps_to_final_positions() {
    let mut f = set_up_without_renderer();
    let id = f.add_client();
    create_window(&mut f, id, 100, 100);

    let source_id = f.niri().layout.windows().next().unwrap().1.id();
    let mirror1_id = create_window_mirror_next_to(&mut f, source_id, source_id);
    let mirror2_id = create_window_mirror_next_to(&mut f, source_id, mirror1_id);

    f.niri_complete_animations();
    f.niri().layout.focus_right();

    f.niri().layout.consume_into_column();

    let working_area = f.niri().layout.active_workspace().unwrap().working_area();
    let area_x = working_area.loc.x.round() as i32;
    let area_y = working_area.loc.y.round() as i32;
    let area_w = working_area.size.w.round() as i32;
    let area_h = working_area.size.h.round() as i32;

    let (mirror1_pos, mirror1_size) = tile_geometry_for(f.niri(), mirror1_id);
    let (mirror2_pos, mirror2_size) = tile_geometry_for(f.niri(), mirror2_id);

    assert_eq!(mirror1_pos.x, mirror2_pos.x);
    assert_eq!(mirror1_size.w, mirror2_size.w);
    assert_eq!(mirror1_size.h, mirror2_size.h);
    assert_eq!(mirror2_pos.y, mirror1_pos.y + mirror1_size.h);

    assert!(mirror1_pos.x >= area_x);
    assert!(mirror1_pos.y >= area_y);
    assert!(mirror2_pos.x + mirror2_size.w <= area_x + area_w);
    assert!(mirror2_pos.y + mirror2_size.h <= area_y + area_h);
}

#[test]
fn mirror_interactive_resize_left_keeps_right_edge() {
    let mut f = set_up_without_renderer();
    let id = f.add_client();
    create_window(&mut f, id, 100, 100);

    let source_id = f.niri().layout.windows().next().unwrap().1.id();
    let mirror_id = create_window_mirror_next_to(&mut f, source_id, source_id);
    f.niri_complete_animations();
    f.niri().layout.activate_window(&mirror_id);
    f.niri_complete_animations();

    let (before_pos, before_size) = tile_geometry_for(f.niri(), mirror_id);
    let before_right = before_pos.x + before_size.w;

    assert!(f
        .niri()
        .layout
        .interactive_resize_begin(mirror_id, ResizeEdge::LEFT));
    assert!(f
        .niri()
        .layout
        .interactive_resize_update(&mirror_id, Point::from((-40., 0.))));

    let (after_pos, after_size) = tile_geometry_for(f.niri(), mirror_id);
    let after_right = after_pos.x + after_size.w;

    assert!(after_size.w > before_size.w);
    assert_eq!(after_right, before_right);
}

#[test]
fn mirror_maximize_to_edges_stays_within_viewport() {
    let mut f = set_up_without_renderer();
    let id = f.add_client();
    create_window(&mut f, id, 40, 20);

    let source_id = f.niri().layout.windows().next().unwrap().1.id();
    let mirror_id = create_window_mirror_next_to(&mut f, source_id, source_id);
    f.niri_complete_animations();
    f.niri().layout.activate_window(&mirror_id);
    f.niri().layout.set_maximized(&mirror_id, true);
    f.niri_complete_animations();

    let ws = f.niri().layout.active_workspace().unwrap();
    let area = ws.scrolling().parent_area();
    let area_x = area.loc.x.round() as i32;
    let area_y = area.loc.y.round() as i32;
    let area_w = area.size.w.round() as i32;
    let area_h = area.size.h.round() as i32;

    let (mirror_pos, mirror_size) = tile_geometry_for(f.niri(), mirror_id);
    assert_eq!(mirror_pos.x, area_x);
    assert_eq!(mirror_pos.y, area_y);
    assert!(mirror_pos.x + mirror_size.w <= area_x + area_w);
    assert!(mirror_pos.y + mirror_size.h <= area_y + area_h);
}
