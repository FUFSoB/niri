//! Layout integration tests for window mirrors.

use std::time::Duration;

use niri_config::animations::{Curve, EasingParams, Kind};
use niri_config::{Action, Config, MruDirection};
use niri_ipc::SizeChange;
use smithay::utils::Point;
use wayland_client::protocol::wl_surface::WlSurface;

use super::client::ClientId;
use super::*;
use crate::layout::{ActivateWindow, AddWindowTarget, LayoutElement as _, SizingMode};
use crate::niri::Niri;
use crate::window::mapped::MappedId;
use crate::window::Mapped;

fn set_up_with_config(config: Config) -> Fixture {
    let mut f = Fixture::with_config(config);
    f.add_output(1, (1000, 800));
    f
}

fn set_up() -> Fixture {
    set_up_with_config(Config::default())
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
    f.double_roundtrip(id);

    surface
}

/// Acks the last configure and commits with the size it requested.
fn ack_and_commit(f: &mut Fixture, id: ClientId, surface: &WlSurface) {
    f.double_roundtrip(id);
    let window = f.client(id).window(surface);
    let (w, h) = window.configures_received.last().unwrap().1.size;
    window.attach_new_buffer();
    window.set_size(w as u16, h as u16);
    window.ack_last_and_commit();
    f.double_roundtrip(id);
}

fn create_window_mirror(f: &mut Fixture, source_id: MappedId) -> MappedId {
    let niri = f.niri();
    let mapped = niri
        .layout
        .windows()
        .find(|(_, mapped)| mapped.id() == source_id)
        .map(|(_, mapped)| mapped)
        .unwrap();
    let mirror = Mapped::new_mirror(mapped);
    let mirror_id = mirror.id();
    niri.layout.add_window_mirror(
        &source_id,
        mirror,
        AddWindowTarget::NextTo(&source_id),
        ActivateWindow::No,
    );
    mirror_id
}

fn mapped(f: &mut Fixture, id: MappedId) -> &Mapped {
    f.niri()
        .layout
        .windows()
        .find(|(_, mapped)| mapped.id() == id)
        .map(|(_, mapped)| mapped)
        .unwrap()
}

fn tile_width(f: &mut Fixture, id: MappedId) -> f64 {
    let ws = f.niri().layout.active_workspace().unwrap();
    ws.tiles()
        .find(|tile| tile.window().id() == id)
        .unwrap()
        .tile_size()
        .w
}

fn has_resize_animation(f: &mut Fixture, id: MappedId) -> bool {
    let ws = f.niri().layout.active_workspace().unwrap();
    ws.tiles()
        .find(|tile| tile.window().id() == id)
        .unwrap()
        .resize_animation()
        .is_some()
}

fn is_floating(f: &mut Fixture, id: MappedId) -> bool {
    f.niri().layout.active_workspace().unwrap().is_floating(&id)
}

fn verify_invariants(f: &mut Fixture) {
    let ws = f.niri().layout.active_workspace().unwrap();
    ws.verify_invariants(None);
}

fn set_time(niri: &mut Niri, time: Duration) {
    // Same as in the animations tests: reset the adjustable clock, then freeze it at `time`.
    let now = niri.clock.now();
    niri.clock.set_unadjusted(now);
    let _ = niri.clock.now();
    niri.clock.set_unadjusted(Duration::ZERO);
    niri.clock.set_rate(1.0);
    let _ = niri.clock.now();

    niri.clock.set_unadjusted(time);
    let _ = niri.clock.now();
    niri.clock.set_rate(0.0);
}

fn source_window_id(f: &mut Fixture) -> MappedId {
    f.niri().layout.windows().next().unwrap().1.id()
}

#[test]
fn mru_close_on_mirror_removes_only_the_mirror() {
    let mut f = set_up();
    let id = f.add_client();
    let surface = create_window(&mut f, id, 100, 100);
    let source_id = source_window_id(&mut f);
    let mirror_id = create_window_mirror(&mut f, source_id);
    f.niri().layout.activate_window(&mirror_id);

    f.niri_state().do_action(
        Action::MruAdvance {
            direction: MruDirection::Forward,
            scope: None,
            filter: None,
        },
        false,
    );
    assert!(f.niri().window_mru_ui.is_open());
    for _ in 0..4 {
        if f.niri().window_mru_ui.current_window_id() == Some(mirror_id) {
            break;
        }
        f.niri_state().do_action(
            Action::MruAdvance {
                direction: MruDirection::Forward,
                scope: None,
                filter: None,
            },
            false,
        );
    }
    assert_eq!(f.niri().window_mru_ui.current_window_id(), Some(mirror_id));

    f.niri_state()
        .do_action(Action::MruCloseCurrentWindow, false);
    f.double_roundtrip(id);

    let ids: Vec<_> = f.niri().layout.windows().map(|(_, m)| m.id()).collect();
    assert_eq!(ids, [source_id]);
    assert!(!f.client(id).window(&surface).close_requested);
}

#[test]
fn ipc_reports_mirror_link_toggle() {
    let mut f = set_up();
    let id = f.add_client();
    create_window(&mut f, id, 100, 100);
    let source_id = source_window_id(&mut f);
    let mirror_id = create_window_mirror(&mut f, source_id);

    let is_linked_in_ipc = |f: &mut Fixture| {
        let state = f.niri_state();
        state.ipc_refresh_layout();
        let server = state.niri.ipc_server.as_ref().unwrap();
        let state = server.test_event_stream_state();
        state.windows.windows[&mirror_id.get()].is_mirror_linked
    };

    assert!(!is_linked_in_ipc(&mut f));
    assert!(f.niri().layout.toggle_window_mirror_link(&mirror_id));
    assert!(is_linked_in_ipc(&mut f));
}

#[test]
fn mirror_keeps_fixed_column_width_with_borders() {
    let mut config = Config::default();
    config.layout.border.off = false;
    config.layout.border.width = 4.;
    let mut f = set_up_with_config(config);
    let id = f.add_client();
    let surface = create_window(&mut f, id, 100, 100);
    let source_id = source_window_id(&mut f);

    f.niri()
        .layout
        .set_window_width(Some(&source_id), SizeChange::SetFixed(300));
    ack_and_commit(&mut f, id, &surface);
    assert_eq!(tile_width(&mut f, source_id), 308.);

    let mirror_id = create_window_mirror(&mut f, source_id);
    assert_eq!(tile_width(&mut f, mirror_id), 308.);

    // Linked sync must not grow the mirror either.
    let mirror2_id = create_window_mirror(&mut f, source_id);
    f.niri()
        .layout
        .set_window_width(Some(&mirror2_id), SizeChange::SetFixed(50));
    assert!(f.niri().layout.toggle_window_mirror_link(&mirror2_id));
    assert_eq!(tile_width(&mut f, mirror2_id), 308.);
    verify_invariants(&mut f);
}

#[test]
fn linked_mirror_sync_is_idempotent_on_source_commits() {
    let mut config = Config::default();
    config.animations.window_resize.anim.kind = Kind::Easing(EasingParams {
        duration_ms: 1000,
        curve: Curve::Linear,
    });
    let mut f = set_up_with_config(config);
    let id = f.add_client();
    let surface = create_window(&mut f, id, 100, 100);
    let source_id = source_window_id(&mut f);
    let mirror_id = create_window_mirror(&mut f, source_id);
    assert!(f.niri().layout.toggle_window_mirror_link(&mirror_id));
    f.niri_complete_animations();

    set_time(f.niri(), Duration::ZERO);
    f.niri().layout.set_maximized(&source_id, true);
    ack_and_commit(&mut f, id, &surface);
    assert_eq!(
        mapped(&mut f, mirror_id).pending_sizing_mode(),
        SizingMode::Maximized
    );
    assert!(has_resize_animation(&mut f, mirror_id));

    // A plain source commit halfway through must not restart the linked mirror animation.
    set_time(f.niri(), Duration::from_millis(500));
    f.niri().advance_animations();
    let window = f.client(id).window(&surface);
    window.commit();
    f.double_roundtrip(id);

    set_time(f.niri(), Duration::from_millis(1000));
    f.niri().advance_animations();
    assert!(!has_resize_animation(&mut f, mirror_id));
    verify_invariants(&mut f);
}

#[test]
fn fullscreen_of_inactive_mirror_column_keeps_view() {
    let mut f = set_up();
    let id = f.add_client();
    create_window(&mut f, id, 100, 100);
    let source_id = source_window_id(&mut f);
    let mirror_id = create_window_mirror(&mut f, source_id);
    f.niri().layout.activate_window(&source_id);
    f.niri_complete_animations();

    let view_pos = |f: &mut Fixture| {
        f.niri()
            .layout
            .active_workspace()
            .unwrap()
            .scrolling()
            .target_view_pos()
    };
    let before = view_pos(&mut f);

    f.niri().layout.set_fullscreen(&mirror_id, true);
    assert_eq!(
        mapped(&mut f, mirror_id).pending_sizing_mode(),
        SizingMode::Fullscreen
    );
    assert_eq!(view_pos(&mut f), before);
    verify_invariants(&mut f);
}

#[test]
fn mirror_windowed_fullscreen_is_not_real_fullscreen() {
    let mut f = set_up();
    let id = f.add_client();
    create_window(&mut f, id, 100, 100);
    let source_id = source_window_id(&mut f);
    let mirror_id = create_window_mirror(&mut f, source_id);

    f.niri().layout.toggle_windowed_fullscreen(&mirror_id);
    let mirror = mapped(&mut f, mirror_id);
    assert!(Mapped::is_windowed_fullscreen(mirror));
    assert_eq!(mirror.pending_sizing_mode(), SizingMode::Normal);
    assert_eq!(mirror.sizing_mode(), SizingMode::Normal);

    f.niri().layout.toggle_windowed_fullscreen(&mirror_id);
    assert!(!Mapped::is_windowed_fullscreen(mapped(&mut f, mirror_id)));

    // Real fullscreen is not windowed fullscreen, so unfullscreening works.
    f.niri().layout.set_fullscreen(&mirror_id, true);
    let mirror = mapped(&mut f, mirror_id);
    assert!(!Mapped::is_windowed_fullscreen(mirror));
    assert_eq!(mirror.pending_sizing_mode(), SizingMode::Fullscreen);

    f.niri().layout.set_fullscreen(&mirror_id, false);
    assert_eq!(
        mapped(&mut f, mirror_id).pending_sizing_mode(),
        SizingMode::Normal
    );
    let ws = f.niri().layout.active_workspace().unwrap();
    let col = ws
        .scrolling()
        .columns()
        .find(|col| col.contains(&mirror_id))
        .unwrap();
    assert!(!col.is_pending_fullscreen());
    verify_invariants(&mut f);
}

#[test]
fn linked_mirror_follows_real_to_windowed_fullscreen() {
    let mut f = set_up();
    let id = f.add_client();
    let surface = create_window(&mut f, id, 100, 100);
    let source_id = source_window_id(&mut f);
    let mirror_id = create_window_mirror(&mut f, source_id);
    assert!(f.niri().layout.toggle_window_mirror_link(&mirror_id));

    f.niri().layout.set_fullscreen(&source_id, true);
    ack_and_commit(&mut f, id, &surface);
    assert_eq!(
        mapped(&mut f, mirror_id).pending_sizing_mode(),
        SizingMode::Fullscreen
    );

    f.niri().layout.toggle_windowed_fullscreen(&source_id);
    ack_and_commit(&mut f, id, &surface);

    let mirror = mapped(&mut f, mirror_id);
    assert!(Mapped::is_windowed_fullscreen(mirror));
    assert_eq!(mirror.pending_sizing_mode(), SizingMode::Normal);
    verify_invariants(&mut f);
}

#[test]
fn mirror_column_mutations_keep_column_data_up_to_date() {
    let mut f = set_up();
    let id = f.add_client();
    create_window(&mut f, id, 100, 100);
    let source_id = source_window_id(&mut f);
    let mirror_id = create_window_mirror(&mut f, source_id);
    f.niri().layout.activate_window(&mirror_id);
    verify_invariants(&mut f);

    let layout = &mut f.niri().layout;
    layout.set_window_width(Some(&mirror_id), SizeChange::SetProportion(25.));
    f.niri()
        .layout
        .active_workspace()
        .unwrap()
        .verify_invariants(None);

    let layout = &mut f.niri().layout;
    layout.toggle_window_width(Some(&mirror_id), true);
    f.niri()
        .layout
        .active_workspace()
        .unwrap()
        .verify_invariants(None);

    let layout = &mut f.niri().layout;
    layout.toggle_width(true);
    f.niri()
        .layout
        .active_workspace()
        .unwrap()
        .verify_invariants(None);

    let layout = &mut f.niri().layout;
    layout.toggle_full_width();
    f.niri()
        .layout
        .active_workspace()
        .unwrap()
        .verify_invariants(None);

    let layout = &mut f.niri().layout;
    layout.toggle_full_width();
    f.niri()
        .layout
        .active_workspace()
        .unwrap()
        .verify_invariants(None);

    let layout = &mut f.niri().layout;
    layout.expand_column_to_available_width();
    f.niri()
        .layout
        .active_workspace()
        .unwrap()
        .verify_invariants(None);

    let layout = &mut f.niri().layout;
    layout.set_maximized(&mirror_id, true);
    f.niri()
        .layout
        .active_workspace()
        .unwrap()
        .verify_invariants(None);

    let layout = &mut f.niri().layout;
    layout.set_maximized(&mirror_id, false);
    f.niri()
        .layout
        .active_workspace()
        .unwrap()
        .verify_invariants(None);
}

#[test]
fn linked_mirror_in_source_column_does_not_reset_source_height() {
    let mut f = set_up();
    let id = f.add_client();
    let surface = create_window(&mut f, id, 100, 100);
    let source_id = source_window_id(&mut f);
    let mirror_id = create_window_mirror(&mut f, source_id);
    f.niri().layout.activate_window(&mirror_id);
    f.niri()
        .layout
        .consume_or_expel_window_left(Some(&mirror_id));
    assert!(f.niri().layout.toggle_window_mirror_link(&mirror_id));

    f.niri()
        .layout
        .set_window_height(Some(&source_id), SizeChange::SetFixed(200));
    ack_and_commit(&mut f, id, &surface);

    let ws = f.niri().layout.active_workspace().unwrap();
    let col = ws
        .scrolling()
        .columns()
        .find(|col| col.contains(&source_id))
        .unwrap();
    assert!(col.contains(&mirror_id));
    let (source_idx, _) = col
        .tiles()
        .enumerate()
        .find(|(_, (tile, _))| tile.window().id() == source_id)
        .unwrap();
    assert!(matches!(
        col.window_height(source_idx),
        crate::layout::scrolling::WindowHeight::Fixed(_)
    ));
    ws.verify_invariants(None);
}

#[test]
fn linked_mirror_floating_toggle_during_interactive_move_applies_to_source() {
    let mut f = set_up();
    let id = f.add_client();
    let surface = create_window(&mut f, id, 100, 100);
    let source_id = source_window_id(&mut f);
    let mirror_id = create_window_mirror(&mut f, source_id);
    assert!(f.niri().layout.toggle_window_mirror_link(&mirror_id));

    let output = f.niri_output(1);
    let layout = &mut f.niri().layout;
    assert!(layout.interactive_move_begin(mirror_id, &output, Point::from((100., 100.))));
    assert!(layout.interactive_move_update(
        &mirror_id,
        Point::from((300., 300.)),
        output.clone(),
        Point::from((400., 400.)),
        false,
    ));
    layout.toggle_window_floating(Some(&mirror_id));
    layout.interactive_move_end(&mirror_id);

    assert!(is_floating(&mut f, mirror_id));
    assert!(is_floating(&mut f, source_id));

    // And it stays that way after the source commits.
    ack_and_commit(&mut f, id, &surface);
    assert!(is_floating(&mut f, mirror_id));
    assert!(is_floating(&mut f, source_id));
}
