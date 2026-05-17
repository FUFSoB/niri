use std::str::FromStr;
use std::time::Duration;

use niri_config::utils::RegexEq;
use niri_config::window_rule::{DrawCursor, Match as WindowMatch, WindowRule};
use niri_config::{Action, Config};
use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::utils::{Relocate, RelocateRenderElement};
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::input::pointer::CursorImageStatus;
use smithay::output::Output;
use smithay::utils::{Logical, Physical, Point, Scale, Size, Transform};

use super::*;
use crate::layout::LayoutElement;
use crate::niri::{PointerVisibility, WindowScreenshotRenderElement};
use crate::render_helpers::xray::XrayPos;
use crate::render_helpers::{
    encompassing_geo, render_to_vec as render_pixels, RenderCtx, RenderTarget,
};
use crate::screencasting::CastRenderElement;
use crate::window::mapped::MappedId;

const WINDOW_COLOR: [u32; 4] = [u32::MAX, 0, 0, u32::MAX];

fn set_up(config: Config) -> Option<Fixture> {
    let mut f = Fixture::with_config(config);
    if f.niri_state().backend.headless().add_renderer().is_err() {
        eprintln!("skipping draw-cursor test: headless EGL renderer unavailable");
        return None;
    }
    f.add_output(1, (100, 100));
    Some(f)
}

fn create_window(f: &mut Fixture, title: &str, size: (u16, u16)) -> MappedId {
    let id = f.add_client();
    let window = f.client(id).create_window();
    let surface = window.surface.clone();
    window.set_title(title);
    window.commit();
    f.roundtrip(id);

    let window = f.client(id).window(&surface);
    window.attach_rgba_buffer(WINDOW_COLOR);
    window.set_size(size.0, size.1);
    window.ack_last_and_commit();
    f.double_roundtrip(id);

    f.niri().layout.focus().unwrap().id()
}

fn tile_geometry_for(f: &mut Fixture, id: MappedId) -> (Point<i32, Logical>, Size<i32, Logical>) {
    let ws = f.niri().layout.active_workspace().unwrap();
    let (tile, pos, visible) = ws
        .tiles_with_render_positions()
        .find(|(tile, _, _)| tile.window().id() == id)
        .unwrap();
    assert!(visible);
    (pos.to_i32_round(), tile.animated_tile_size().to_i32_round())
}

fn move_cursor_to_window(f: &mut Fixture, id: MappedId) {
    let (pos, size) = tile_geometry_for(f, id);
    let point = Point::from((
        pos.x as f64 + size.w as f64 / 2.,
        pos.y as f64 + size.h as f64 / 2.,
    ));

    let state = f.niri_state();
    state.niri.pointer_visibility = PointerVisibility::Visible;
    state.niri.tablet_cursor_location = None;
    state.move_cursor(point);
}

fn move_cursor_to_output_center(f: &mut Fixture) {
    let state = f.niri_state();
    state.niri.pointer_visibility = PointerVisibility::Visible;
    state.niri.tablet_cursor_location = None;
    state.move_cursor(Point::from((50.0, 50.0)));
}

fn zoom_output(f: &mut Fixture, level: &str) {
    f.niri_state()
        .do_action(Action::SetZoomLevel(level.into(), None), false);
    f.niri_complete_animations();
}

fn cursor_pos(f: &mut Fixture) -> Point<f64, Logical> {
    f.niri().seat.get_pointer().unwrap().current_location()
}

fn render_output_pixels(
    f: &mut Fixture,
    output: &Output,
    target: RenderTarget,
    include_pointer: bool,
) -> (Size<i32, Physical>, Vec<u8>) {
    let output = output.clone();
    let state = f.niri_state();
    let (backend, niri) = (&mut state.backend, &mut state.niri);

    backend
        .with_primary_renderer(|renderer| {
            niri.update_render_elements(Some(&output));

            let size = output.current_mode().unwrap().size;
            let transform = output.current_transform();
            let size = transform.transform_size(size);

            let scale = Scale::from(output.current_scale().fractional_scale());
            let ctx = RenderCtx {
                renderer,
                target,
                block_out_enabled: niri.block_out_enabled,
                xray: None,
            };
            let elements = niri.render_to_vec(ctx, &output, include_pointer);
            let pixels = render_pixels(
                renderer,
                size,
                scale,
                Transform::Normal,
                Fourcc::Abgr8888,
                elements.iter().rev(),
            )
            .unwrap();

            (size, pixels)
        })
        .unwrap()
}

fn render_window_screencast_pixels(
    f: &mut Fixture,
    output: &Output,
    id: MappedId,
    include_pointer: bool,
) -> (Size<i32, Physical>, Vec<u8>) {
    let output = output.clone();
    let state = f.niri_state();
    let (backend, niri) = (&mut state.backend, &mut state.niri);

    backend
        .with_primary_renderer(|renderer| {
            niri.update_render_elements(Some(&output));

            let scale = Scale::from(output.current_scale().fractional_scale());
            let mapped = niri
                .layout
                .windows()
                .find(|(_, mapped)| mapped.id() == id)
                .map(|(_, mapped)| mapped)
                .unwrap();

            let mut elements: Vec<CastRenderElement<GlesRenderer>> = Vec::new();

            if include_pointer {
                if let Some((_, win_pos)) = niri.pointer_pos_for_window_cast(mapped) {
                    let pos = mapped
                        .window_cast_buffer_pos(win_pos, scale)
                        .to_physical_precise_round(scale)
                        .upscale(-1);
                    niri.render_pointer(
                        renderer,
                        &output,
                        RenderTarget::Screencast,
                        Some(id),
                        &mut |elem| {
                            let elem =
                                RelocateRenderElement::from_element(elem, pos, Relocate::Relative);
                            elements.push(CastRenderElement::from(elem));
                        },
                    );
                }
            }

            mapped.render_for_screen_cast(renderer, scale, niri.block_out_enabled, &mut |elem| {
                elements.push(CastRenderElement::from(elem))
            });

            let geo = encompassing_geo(scale, elements.iter());
            let pixels = render_pixels(
                renderer,
                geo.size,
                scale,
                Transform::Normal,
                Fourcc::Abgr8888,
                elements.iter().rev().map(|elem| {
                    RelocateRenderElement::from_element(
                        elem,
                        geo.loc.upscale(-1),
                        Relocate::Relative,
                    )
                }),
            )
            .unwrap();

            (geo.size, pixels)
        })
        .unwrap()
}

fn render_window_screen_capture_pixels(
    f: &mut Fixture,
    output: &Output,
    id: MappedId,
    include_pointer: bool,
) -> (Size<i32, Physical>, Vec<u8>) {
    let output = output.clone();
    let state = f.niri_state();
    let (backend, niri) = (&mut state.backend, &mut state.niri);

    backend
        .with_primary_renderer(|renderer| {
            niri.update_render_elements(Some(&output));

            let scale = Scale::from(output.current_scale().fractional_scale());
            let mapped = niri
                .layout
                .windows()
                .find(|(_, mapped)| mapped.id() == id)
                .map(|(_, mapped)| mapped)
                .unwrap();
            let alpha = if mapped.sizing_mode().is_fullscreen()
                || mapped.is_ignoring_opacity_window_rule()
            {
                1.
            } else {
                mapped.rules().opacity.unwrap_or(1.).clamp(0., 1.)
            };

            let mut elements: Vec<WindowScreenshotRenderElement<GlesRenderer>> = Vec::new();

            if include_pointer {
                if let Some((_, win_pos)) = niri.pointer_pos_for_window_cast(mapped) {
                    let pos = win_pos.to_physical_precise_round(scale).upscale(-1);
                    niri.render_pointer(
                        renderer,
                        &output,
                        RenderTarget::ScreenCapture,
                        Some(id),
                        &mut |elem| {
                            let elem =
                                RelocateRenderElement::from_element(elem, pos, Relocate::Relative);
                            elements.push(elem.into());
                        },
                    );
                }
            }

            mapped.render(
                RenderCtx {
                    renderer,
                    target: RenderTarget::ScreenCapture,
                    block_out_enabled: niri.block_out_enabled,
                    xray: None,
                },
                mapped.window.geometry().loc.to_f64(),
                scale,
                alpha,
                XrayPos::default(),
                &mut |elem| elements.push(elem.into()),
            );

            let geo = encompassing_geo(scale, elements.iter());
            let pixels = render_pixels(
                renderer,
                geo.size,
                scale,
                Transform::Normal,
                Fourcc::Abgr8888,
                elements.iter().rev().map(|elem| {
                    RelocateRenderElement::from_element(
                        elem,
                        geo.loc.upscale(-1),
                        Relocate::Relative,
                    )
                }),
            )
            .unwrap();

            (geo.size, pixels)
        })
        .unwrap()
}

fn set_cursor_image(f: &mut Fixture, image: CursorImageStatus) {
    f.niri().cursor_manager.set_cursor_image(image);
}

fn start_workspace_switch(f: &mut Fixture, output: &Output) {
    f.niri().layout.workspace_switch_gesture_begin(output, true);

    for (step, delta) in [150., 300., 450., 600.].into_iter().enumerate() {
        let _ = f.niri().layout.workspace_switch_gesture_update(
            delta,
            Duration::from_millis(step as u64 + 1),
            true,
        );

        let progress = f
            .niri()
            .layout
            .monitor_for_output(output)
            .unwrap()
            .workspace_render_idx();
        if progress.abs() > 0.5 {
            return;
        }
    }

    panic!("workspace switch did not progress far enough for test");
}

fn config_with_draw_cursor(draw_cursor: DrawCursor) -> Config {
    let mut config = Config::default();
    config.window_rules.push(WindowRule {
        matches: vec![WindowMatch {
            title: Some(RegexEq::from_str("^test$").unwrap()),
            ..Default::default()
        }],
        draw_cursor: Some(draw_cursor),
        ..Default::default()
    });
    config
}

#[test]
fn draw_cursor_always_hidden_hides_visible_cursor_on_output() {
    let Some(mut f) = set_up(config_with_draw_cursor(DrawCursor::AlwaysHidden)) else {
        return;
    };
    let window = create_window(&mut f, "test", (40, 30));
    move_cursor_to_window(&mut f, window);
    set_cursor_image(&mut f, CursorImageStatus::default_named());

    let output = f.niri_output(1);
    let without_pointer = render_output_pixels(&mut f, &output, RenderTarget::Output, false);
    let with_pointer = render_output_pixels(&mut f, &output, RenderTarget::Output, true);

    assert_eq!(with_pointer, without_pointer);
}

#[test]
fn draw_cursor_always_shown_draws_hidden_cursor_on_output() {
    let Some(mut f) = set_up(config_with_draw_cursor(DrawCursor::AlwaysShown)) else {
        return;
    };
    let window = create_window(&mut f, "test", (40, 30));
    move_cursor_to_window(&mut f, window);
    set_cursor_image(&mut f, CursorImageStatus::Hidden);

    let output = f.niri_output(1);
    let without_pointer = render_output_pixels(&mut f, &output, RenderTarget::Output, false);
    let with_pointer = render_output_pixels(&mut f, &output, RenderTarget::Output, true);

    assert_ne!(with_pointer, without_pointer);
}

#[test]
fn draw_cursor_hidden_on_capture_hides_output_screen_capture_only() {
    let Some(mut f) = set_up(config_with_draw_cursor(DrawCursor::HiddenOnCapture)) else {
        return;
    };
    let window = create_window(&mut f, "test", (40, 30));
    move_cursor_to_window(&mut f, window);
    set_cursor_image(&mut f, CursorImageStatus::default_named());

    let output = f.niri_output(1);
    let output_without_pointer = render_output_pixels(&mut f, &output, RenderTarget::Output, false);
    let output_with_pointer = render_output_pixels(&mut f, &output, RenderTarget::Output, true);
    assert_ne!(output_with_pointer, output_without_pointer);

    let capture_without_pointer =
        render_output_pixels(&mut f, &output, RenderTarget::ScreenCapture, false);
    let capture_with_pointer =
        render_output_pixels(&mut f, &output, RenderTarget::ScreenCapture, true);
    assert_eq!(capture_with_pointer, capture_without_pointer);
}

#[test]
fn draw_cursor_hidden_on_capture_hides_window_screencast_pointer() {
    let Some(mut f) = set_up(config_with_draw_cursor(DrawCursor::HiddenOnCapture)) else {
        return;
    };
    let window = create_window(&mut f, "test", (40, 30));
    move_cursor_to_window(&mut f, window);
    set_cursor_image(&mut f, CursorImageStatus::default_named());

    let output = f.niri_output(1);
    let without_pointer = render_window_screencast_pixels(&mut f, &output, window, false);
    let with_pointer = render_window_screencast_pixels(&mut f, &output, window, true);

    assert_eq!(with_pointer, without_pointer);
}

#[test]
fn later_draw_cursor_default_restores_default_behavior() {
    let mut config = Config::default();
    config.window_rules.push(WindowRule {
        matches: vec![WindowMatch {
            title: Some(RegexEq::from_str("^test$").unwrap()),
            ..Default::default()
        }],
        draw_cursor: Some(DrawCursor::AlwaysHidden),
        ..Default::default()
    });
    config.window_rules.push(WindowRule {
        matches: vec![WindowMatch {
            title: Some(RegexEq::from_str("^test$").unwrap()),
            ..Default::default()
        }],
        draw_cursor: Some(DrawCursor::Default),
        ..Default::default()
    });

    let Some(mut f) = set_up(config) else {
        return;
    };
    let window = create_window(&mut f, "test", (40, 30));
    move_cursor_to_window(&mut f, window);
    set_cursor_image(&mut f, CursorImageStatus::default_named());

    let output = f.niri_output(1);
    let without_pointer = render_output_pixels(&mut f, &output, RenderTarget::Output, false);
    let with_pointer = render_output_pixels(&mut f, &output, RenderTarget::Output, true);

    assert_ne!(with_pointer, without_pointer);
}

#[test]
fn draw_cursor_always_hidden_stays_hidden_during_workspace_switch_gap() {
    let Some(mut f) = set_up(config_with_draw_cursor(DrawCursor::AlwaysHidden)) else {
        return;
    };
    let window = create_window(&mut f, "test", (40, 30));
    move_cursor_to_window(&mut f, window);
    set_cursor_image(&mut f, CursorImageStatus::default_named());

    let output = f.niri_output(1);
    start_workspace_switch(&mut f, &output);

    let without_pointer = render_output_pixels(&mut f, &output, RenderTarget::Output, false);
    let with_pointer = render_output_pixels(&mut f, &output, RenderTarget::Output, true);

    assert_eq!(with_pointer, without_pointer);
}

#[test]
fn draw_cursor_always_hidden_stays_hidden_on_screencast_during_workspace_switch() {
    let mut config = Config::default();
    config.window_rules.push(WindowRule {
        matches: vec![WindowMatch {
            title: Some(RegexEq::from_str("^hidden$").unwrap()),
            ..Default::default()
        }],
        draw_cursor: Some(DrawCursor::AlwaysHidden),
        ..Default::default()
    });

    let Some(mut f) = set_up(config) else {
        return;
    };
    let hidden = create_window(&mut f, "hidden", (40, 30));
    let visible = create_window(&mut f, "visible", (40, 30));
    f.niri().layout.move_to_workspace_down(true);
    f.niri().layout.activate_window(&hidden);

    move_cursor_to_window(&mut f, hidden);
    set_cursor_image(&mut f, CursorImageStatus::default_named());

    let output = f.niri_output(1);
    start_workspace_switch(&mut f, &output);

    let pointer_owner = f.niri().pointer_contents.window.map(|(id, _)| id);
    assert_eq!(pointer_owner, Some(hidden));

    let cursor = cursor_pos(&mut f);
    let live_window_under_cursor = f.niri().contents_under(cursor).window.map(|(id, _)| id);
    assert_eq!(live_window_under_cursor, Some(visible));
    assert_ne!(live_window_under_cursor, pointer_owner);

    let without_pointer = render_output_pixels(&mut f, &output, RenderTarget::Screencast, false);
    let with_pointer = render_output_pixels(&mut f, &output, RenderTarget::Screencast, true);

    assert_eq!(with_pointer, without_pointer);
}

#[test]
fn draw_cursor_always_hidden_stays_hidden_on_window_captures_after_workspace_switch() {
    let mut config = Config::default();
    config.window_rules.push(WindowRule {
        matches: vec![WindowMatch {
            title: Some(RegexEq::from_str("^hidden$").unwrap()),
            ..Default::default()
        }],
        draw_cursor: Some(DrawCursor::AlwaysHidden),
        ..Default::default()
    });

    let Some(mut f) = set_up(config) else {
        return;
    };
    let hidden = create_window(&mut f, "hidden", (40, 30));
    let visible = create_window(&mut f, "visible", (40, 30));
    f.niri().layout.move_to_workspace_down(true);
    f.niri().layout.activate_window(&hidden);

    move_cursor_to_window(&mut f, hidden);
    set_cursor_image(&mut f, CursorImageStatus::default_named());

    let output = f.niri_output(1);
    start_workspace_switch(&mut f, &output);
    let _ = f.niri().layout.workspace_switch_gesture_end(Some(true));

    assert!(f
        .niri()
        .layout
        .monitor_for_output(&output)
        .unwrap()
        .are_transitions_ongoing());

    let active_window_ids = f
        .niri()
        .layout
        .active_workspace()
        .unwrap()
        .tiles_with_render_positions()
        .map(|(tile, _, _)| tile.window().id())
        .collect::<Vec<_>>();
    assert!(active_window_ids.contains(&visible));
    assert!(!active_window_ids.contains(&hidden));

    let pointer_still_belongs_to_hidden = {
        let niri = f.niri();
        let hidden_mapped = niri
            .layout
            .windows()
            .find(|(_, mapped)| mapped.id() == hidden)
            .map(|(_, mapped)| mapped)
            .unwrap();
        niri.pointer_pos_for_window_cast(hidden_mapped).is_some()
    };
    assert!(pointer_still_belongs_to_hidden);

    let cursor = cursor_pos(&mut f);
    let live_window_under_cursor = f.niri().contents_under(cursor).window.map(|(id, _)| id);
    assert_ne!(live_window_under_cursor, Some(hidden));

    let screencast_without_pointer =
        render_window_screencast_pixels(&mut f, &output, hidden, false);
    let screencast_with_pointer = render_window_screencast_pixels(&mut f, &output, hidden, true);
    assert_eq!(screencast_with_pointer, screencast_without_pointer);

    let capture_without_pointer =
        render_window_screen_capture_pixels(&mut f, &output, hidden, false);
    let capture_with_pointer = render_window_screen_capture_pixels(&mut f, &output, hidden, true);
    assert_eq!(capture_with_pointer, capture_without_pointer);
}

#[test]
fn output_zoom_renders_pointer_on_empty_output() {
    let Some(mut f) = set_up(Config::default()) else {
        return;
    };
    move_cursor_to_output_center(&mut f);
    set_cursor_image(&mut f, CursorImageStatus::default_named());
    zoom_output(&mut f, "+0.25");

    let output = f.niri_output(1);
    assert!(f.niri().layout.zoom_level_for_output(&output) > 1.0);

    let without_pointer = render_output_pixels(&mut f, &output, RenderTarget::Output, false);
    let with_pointer = render_output_pixels(&mut f, &output, RenderTarget::Output, true);

    assert_ne!(with_pointer, without_pointer);
}

#[test]
fn output_zoom_renders_pointer_over_window() {
    let Some(mut f) = set_up(Config::default()) else {
        return;
    };
    let window = create_window(&mut f, "test", (40, 30));
    move_cursor_to_window(&mut f, window);
    set_cursor_image(&mut f, CursorImageStatus::default_named());
    zoom_output(&mut f, "+0.25");

    let output = f.niri_output(1);
    assert!(f.niri().layout.zoom_level_for_output(&output) > 1.0);

    let without_pointer = render_output_pixels(&mut f, &output, RenderTarget::Output, false);
    let with_pointer = render_output_pixels(&mut f, &output, RenderTarget::Output, true);

    assert_ne!(with_pointer, without_pointer);
}
