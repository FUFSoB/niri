use std::time::Duration;

use niri_config::Action;
use smithay::output::Output;
use smithay::utils::{Logical, Point};

use super::*;

fn set_up() -> Fixture {
    let mut f = Fixture::new();
    f.add_output(1, (1000, 1000));
    f.add_output(2, (1000, 1000));
    f.niri_focus_output(1);
    f
}

#[track_caller]
fn assert_close(a: Point<f64, Logical>, b: Point<f64, Logical>) {
    assert!(
        (a.x - b.x).abs() < 1e-6 && (a.y - b.y).abs() < 1e-6,
        "{a:?} != {b:?}"
    );
}

fn output_loc(f: &mut Fixture, output: &Output) -> Point<f64, Logical> {
    f.niri()
        .global_space
        .output_geometry(output)
        .unwrap()
        .loc
        .to_f64()
}

fn cursor_pos(f: &mut Fixture) -> Point<f64, Logical> {
    f.niri().seat.get_pointer().unwrap().current_location()
}

/// Zooms the output under the cursor to the given level and locks the focal point.
fn zoom_and_lock(f: &mut Fixture, level: &str) {
    f.niri_state()
        .do_action(Action::SetZoomLevel(level.into(), None), false);
    f.niri_complete_animations();
    f.niri_state()
        .do_action(Action::ToggleZoomLock(None), false);
    f.niri_complete_animations();
}

#[test]
fn absolute_positions_map_through_zoom() {
    let mut f = set_up();
    let output = f.niri_output(1);
    let loc = output_loc(&mut f, &output);

    f.niri_state().move_cursor(loc + Point::from((300., 400.)));
    zoom_and_lock(&mut f, "2");

    let niri = f.niri();
    assert_eq!(niri.layout.zoom_level_for_output(&output), 2.);
    let focal = niri.layout.zoom_focal_for_output(&output);

    // The focal point is displayed in place.
    assert_eq!(niri.unzoom_screen_pos(loc + focal), loc + focal);

    // Screen corners map to the zoomed viewport corners, not to the output corners.
    let top_left = niri.unzoom_screen_pos(loc);
    assert_close(top_left, loc + focal.downscale(2.));
    let pos = niri.unzoom_screen_pos(loc + Point::from((999., 999.)));
    assert_close(pos, loc + (Point::from((999., 999.)) + focal).downscale(2.));

    // Mapping back to the screen gives the original position.
    let screen = niri.cursor_screen_pos_within_output(&output, pos);
    assert_close(screen, Point::from((999., 999.)));
}

#[test]
fn move_cursor_respects_locked_zoom() {
    let mut f = set_up();
    let output = f.niri_output(1);
    let loc = output_loc(&mut f, &output);

    f.niri_state().move_cursor(loc + Point::from((300., 400.)));
    zoom_and_lock(&mut f, "2");
    let focal = f.niri().layout.zoom_focal_for_output(&output);

    // Warping outside the locked viewport keeps the view and clamps the cursor into it.
    f.niri_state().move_cursor(loc + Point::from((990., 990.)));
    assert_eq!(f.niri().layout.zoom_focal_for_output(&output), focal);
    let pos = cursor_pos(&mut f);
    let effective = f.niri().effective_cursor_pos(pos);
    assert_eq!(pos, effective);
    assert!(pos.x < loc.x + 990. && pos.y < loc.y + 990., "{pos:?}");
}

#[test]
fn move_cursor_pans_unlocked_zoom() {
    let mut f = set_up();
    let output = f.niri_output(1);
    let loc = output_loc(&mut f, &output);

    f.niri_state().move_cursor(loc + Point::from((300., 400.)));
    f.niri_state()
        .do_action(Action::SetZoomLevel("2".into(), None), false);
    f.niri_complete_animations();

    // Warping outside the viewport pans the view to show the new location.
    let target = loc + Point::from((990., 990.));
    f.niri_state().move_cursor(target);
    assert_eq!(cursor_pos(&mut f), target);
    assert_eq!(f.niri().effective_cursor_pos(target), target);
}

#[test]
fn zoom_pinch_ends_on_output_where_it_began() {
    let mut f = set_up();
    let output1 = f.niri_output(1);
    let output2 = f.niri_output(2);
    let loc1 = output_loc(&mut f, &output1);
    let loc2 = output_loc(&mut f, &output2);

    f.niri_state().move_cursor(loc1 + Point::from((500., 500.)));
    assert!(f.niri_state().zoom_pinch_begin());

    // Move the cursor to the other output mid-gesture.
    f.niri_state().move_cursor(loc2 + Point::from((500., 500.)));

    let state = f.niri_state();
    assert!(state.zoom_pinch_update(1., Duration::from_millis(0)));
    assert!(state.zoom_pinch_update(2., Duration::from_millis(10)));
    assert!(state.zoom_pinch_end(false));
    // Subsequent events aren't eaten.
    assert!(!state.zoom_pinch_update(2., Duration::from_millis(20)));
    assert!(!state.zoom_pinch_end(false));

    f.niri_complete_animations();
    let layout = &f.niri().layout;
    assert!(layout.zoom_level_for_output(&output1) > 1.);
    assert!(
        !layout
            .zoom_state_for_output(&output1)
            .unwrap()
            .transitioning
    );
    assert_eq!(layout.zoom_level_for_output(&output2), 1.);
    assert!(
        !layout
            .zoom_state_for_output(&output2)
            .unwrap()
            .transitioning
    );
}

#[test]
fn zoom_pinch_survives_output_removal() {
    let mut f = set_up();
    let output1 = f.niri_output(1);
    let loc1 = output_loc(&mut f, &output1);

    f.niri_state().move_cursor(loc1 + Point::from((500., 500.)));
    assert!(f.niri_state().zoom_pinch_begin());

    f.niri().remove_output(&output1);

    // The gesture events keep being consumed rather than leaking to clients.
    let state = f.niri_state();
    assert!(state.zoom_pinch_update(2., Duration::from_millis(10)));
    assert!(state.zoom_pinch_end(false));
    assert!(state.niri.zoom_pinch_output.is_none());
}
