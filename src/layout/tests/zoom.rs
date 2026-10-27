//! Tests for zoom animation state machine.

use std::time::Duration;

use niri_config::animations::{Kind, SpringParams};
use niri_config::ZoomMovementMode;
use smithay::utils::Point;

use super::*;
use crate::animation::Clock;
use crate::layout::{ZoomFocalAnimation, ZoomLevelAnimation, ZoomLevelProgress};
use crate::zoom::OutputZoomState;

fn test_animation_config() -> niri_config::Animation {
    niri_config::Animation {
        off: false,
        kind: Kind::Spring(SpringParams {
            damping_ratio: 1.0,
            stiffness: 1000,
            epsilon: 0.0001,
        }),
    }
}

#[test]
fn zoom_level_animation_value() {
    let clock = Clock::with_time(Duration::ZERO);
    let anim = ZoomLevelAnimation::new(clock.clone(), 1.0, 3.0, test_animation_config());

    // Animation just started, should be near 1.0
    let level = anim.value();
    assert!(
        (1.0..1.5).contains(&level),
        "level should be near start: {}",
        level
    );
}

#[test]
fn zoom_focal_animation_value() {
    let clock = Clock::with_time(Duration::ZERO);
    let anim = ZoomFocalAnimation::new(
        clock.clone(),
        Point::from((0.0, 0.0)),
        Point::from((50.0, 100.0)),
        test_animation_config(),
    );

    // Animation just started, should be near start
    let focal = anim.value();
    assert!(focal.x < 10.0, "focal x should be near start: {}", focal.x);
    assert!(focal.y < 20.0, "focal y should be near start: {}", focal.y);
}

#[test]
fn zoom_level_progress_animation_variant() {
    let clock = Clock::with_time(Duration::ZERO);
    let level_anim = ZoomLevelAnimation::new(clock.clone(), 1.0, 2.0, test_animation_config());
    let progress = ZoomLevelProgress::Animation(level_anim);

    assert!(progress.is_animation());
    assert!(!progress.is_gesture());
    assert!(!progress.is_done());

    let level = progress.level();
    assert!(
        (1.0..1.5).contains(&level),
        "level should be near start: {}",
        level
    );
}

#[test]
fn zoom_level_progress_is_done() {
    let clock = Clock::with_time(Duration::ZERO);
    let level_anim = ZoomLevelAnimation::new(clock.clone(), 1.0, 2.0, test_animation_config());
    let progress = ZoomLevelProgress::Animation(level_anim);

    // Should not be done at start
    assert!(!progress.is_done());
}

#[test]
fn output_zoom_state_default() {
    let state = OutputZoomState::default();
    assert_eq!(state.level, 1.0);
    assert_eq!(state.focal.x, 0.0);
    assert_eq!(state.focal.y, 0.0);
    assert!(!state.locked);
    assert!(!state.transitioning);
}

#[test]
fn zoom_focal_animation_is_done() {
    let clock = Clock::with_time(Duration::ZERO);
    let anim = ZoomFocalAnimation::new(
        clock.clone(),
        Point::from((0.0, 0.0)),
        Point::from((100.0, 100.0)),
        test_animation_config(),
    );

    // Should not be done at start
    assert!(!anim.is_done());
}

fn zoomed_layout(mode: ZoomMovementMode) -> (Layout<TestWindow>, Output) {
    let mut layout = check_ops([Op::AddOutput(1)]);
    let output = layout.outputs().next().unwrap().clone();

    layout.set_zoom_level(&output, 2.0, Point::from((200., 150.)), &mode, false);
    check_ops_on_layout(&mut layout, [Op::CompleteAnimations]);
    assert_eq!(layout.zoom_level_for_output(&output), 2.0);

    (layout, output)
}

#[test]
fn locked_focal_survives_set_zoom_level() {
    for mode in [
        ZoomMovementMode::CursorFollow,
        ZoomMovementMode::Centered,
        ZoomMovementMode::OnEdge,
    ] {
        let (mut layout, output) = zoomed_layout(mode);
        assert!(!layout.toggle_zoom_lock(&output));
        let focal = layout.zoom_focal_for_output(&output);

        // Change the zoom level with the cursor far away from the focal point.
        layout.set_zoom_level(&output, 3.0, Point::from((1200., 700.)), &mode, true);
        check_ops_on_layout(&mut layout, [Op::CompleteAnimations]);

        assert_eq!(layout.zoom_level_for_output(&output), 3.0, "{mode:?}");
        assert_eq!(layout.zoom_focal_for_output(&output), focal, "{mode:?}");
    }
}

#[test]
fn locked_focal_survives_zoom_gesture() {
    for mode in [
        ZoomMovementMode::CursorFollow,
        ZoomMovementMode::Centered,
        ZoomMovementMode::OnEdge,
    ] {
        let (mut layout, output) = zoomed_layout(mode);
        assert!(!layout.toggle_zoom_lock(&output));
        let focal = layout.zoom_focal_for_output(&output);

        layout.zoom_gesture_begin(
            &output,
            Some(Point::from((1200., 700.))),
            Some(Size::from((1280., 720.))),
            Some(mode),
        );
        layout.zoom_gesture_update(&output, 1., 1., Duration::from_millis(0));
        layout.zoom_gesture_update(&output, 1.5, 1., Duration::from_millis(10));
        layout.zoom_gesture_update(&output, 2., 1., Duration::from_millis(20));
        assert_eq!(layout.zoom_gesture_end(&output, false), Some(true));
        check_ops_on_layout(&mut layout, [Op::CompleteAnimations]);

        assert!(layout.zoom_level_for_output(&output) > 2.0, "{mode:?}");
        assert_eq!(layout.zoom_focal_for_output(&output), focal, "{mode:?}");
        assert!(!layout.zoom_state_for_output(&output).unwrap().transitioning);
    }
}

#[test]
fn zoom_focal_clamped_on_output_resize() {
    let (mut layout, output) = zoomed_layout(ZoomMovementMode::CursorFollow);
    layout.set_zoom_level(
        &output,
        2.0,
        Point::from((1200., 700.)),
        &ZoomMovementMode::CursorFollow,
        false,
    );
    check_ops_on_layout(&mut layout, [Op::CompleteAnimations]);
    assert_eq!(
        layout.zoom_focal_for_output(&output),
        Point::from((1200., 700.))
    );

    output.change_current_state(
        Some(Mode {
            size: Size::from((800, 600)),
            refresh: 60000,
        }),
        None,
        None,
        None,
    );
    layout.update_output_size(&output);

    let focal = layout.zoom_focal_for_output(&output);
    assert!(focal.x <= 800. && focal.y <= 600., "{focal:?}");
}
