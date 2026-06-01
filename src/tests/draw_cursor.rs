use std::str::FromStr;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use niri_config::utils::RegexEq;
use niri_config::window_rule::{DrawCursor, ForceCursorShape, Match as WindowMatch, WindowRule};
use niri_config::{Action, Config};
use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::utils::{Relocate, RelocateRenderElement};
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::input::pointer::{CursorIcon, CursorImageStatus, CursorImageSurfaceData};
use smithay::output::Output;
use smithay::reexports::wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_shell_v1::Layer;
use smithay::reexports::wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_surface_v1::Anchor;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Logical, Physical, Point, Scale, Size, Transform};
use smithay::wayland::compositor::with_states;

use super::client::{LayerConfigureProps, SyncData};
use super::*;
use crate::layout::workspace::WorkspaceId;
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

fn move_cursor_to_point(f: &mut Fixture, point: Point<f64, Logical>) {
    let state = f.niri_state();
    state.niri.pointer_visibility = PointerVisibility::Visible;
    state.niri.tablet_cursor_location = None;
    state.move_cursor(point);
}

fn zoom_output(f: &mut Fixture, level: &str) {
    f.niri_state()
        .do_action(Action::SetZoomLevel(level.into(), None), false);
    f.niri_complete_animations();
}

fn cursor_pos(f: &mut Fixture) -> Point<f64, Logical> {
    f.niri().seat.get_pointer().unwrap().current_location()
}

fn window_surface(f: &mut Fixture, id: MappedId) -> WlSurface {
    f.niri()
        .layout
        .windows()
        .find(|(_, mapped)| mapped.id() == id)
        .map(|(_, mapped)| mapped.toplevel().wl_surface().clone())
        .unwrap()
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

fn render_workspace_screencast_pixels(
    f: &mut Fixture,
    output: &Output,
    workspace_id: WorkspaceId,
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
            let workspace = niri.layout.find_workspace_by_id(workspace_id).unwrap().1;

            let mut elements = Vec::new();
            if include_pointer
                && niri.should_render_pointer_for_target(RenderTarget::Screencast, None)
                && niri
                    .workspace_cast_pointer_pos(&output, workspace_id)
                    .is_some()
            {
                niri.render_pointer(
                    renderer,
                    &output,
                    RenderTarget::Screencast,
                    None,
                    &mut |elem| elements.push(elem.into()),
                );
            }

            niri.render_workspace_for_screen_cast(
                RenderCtx {
                    renderer,
                    target: RenderTarget::Screencast,
                    block_out_enabled: niri.block_out_enabled,
                    xray: None,
                },
                &output,
                workspace,
                &mut |elem| elements.push(elem),
            );

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
    f.niri().cursor_manager.set_client_cursor_image(image);
}

fn create_top_layer(f: &mut Fixture, size: (u16, u16)) {
    let id = f.add_client();
    let output = f.client(id).output("headless-1");
    let layer = f
        .client(id)
        .create_layer(Some(&output), Layer::Top, "test-layer");
    let surface = layer.surface.clone();
    layer.set_configure_props(LayerConfigureProps {
        anchor: Some(Anchor::Left | Anchor::Right | Anchor::Top),
        size: Some((0, u32::from(size.1))),
        ..Default::default()
    });
    layer.commit();
    f.roundtrip(id);

    let layer = f.client(id).layer(&surface);
    layer.attach_new_buffer();
    layer.set_size(size.0, size.1);
    layer.ack_last_and_commit();
    f.double_roundtrip(id);
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

fn config_with_force_cursor_shape(force_cursor_shape: ForceCursorShape) -> Config {
    let mut config = Config::default();
    config.window_rules.push(WindowRule {
        matches: vec![WindowMatch {
            title: Some(RegexEq::from_str("^test$").unwrap()),
            ..Default::default()
        }],
        force_cursor_shape: Some(force_cursor_shape),
        ..Default::default()
    });
    config
}

fn output_cursor_image(f: &mut Fixture) -> CursorImageStatus {
    f.niri().cursor_image_for_target(RenderTarget::Output, None)
}

fn assert_named_cursor(image: CursorImageStatus, expected: CursorIcon) {
    match image {
        CursorImageStatus::Named(icon) => assert_eq!(icon, expected),
        _ => panic!("expected named cursor"),
    }
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
fn screenshot_ui_shows_live_pointer_above_always_hidden_window() {
    let Some(mut f) = set_up(config_with_draw_cursor(DrawCursor::AlwaysHidden)) else {
        return;
    };
    let window = create_window(&mut f, "test", (40, 30));
    move_cursor_to_window(&mut f, window);
    set_cursor_image(&mut f, CursorImageStatus::default_named());

    f.niri_state().open_screenshot_ui(false, None);
    f.niri_complete_animations();
    assert!(f.niri().screenshot_ui.is_open());

    let output = f.niri_output(1);
    let without_pointer = render_output_pixels(&mut f, &output, RenderTarget::Output, false);
    let with_pointer = render_output_pixels(&mut f, &output, RenderTarget::Output, true);

    assert_ne!(with_pointer, without_pointer);
}

#[test]
fn screenshot_ui_does_not_show_pointer_in_window_screencast_for_always_hidden_window() {
    let Some(mut f) = set_up(config_with_draw_cursor(DrawCursor::AlwaysHidden)) else {
        return;
    };
    let window = create_window(&mut f, "test", (40, 30));
    move_cursor_to_window(&mut f, window);
    set_cursor_image(&mut f, CursorImageStatus::default_named());

    f.niri_state().open_screenshot_ui(false, None);
    f.niri_complete_animations();
    assert!(f.niri().screenshot_ui.is_open());

    let output = f.niri_output(1);
    let without_pointer = render_window_screencast_pixels(&mut f, &output, window, false);
    let with_pointer = render_window_screencast_pixels(&mut f, &output, window, true);

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
fn draw_cursor_always_shown_overrides_hidden_pointer_visibility() {
    let Some(mut f) = set_up(config_with_draw_cursor(DrawCursor::AlwaysShown)) else {
        return;
    };
    let window = create_window(&mut f, "test", (40, 30));
    move_cursor_to_window(&mut f, window);
    set_cursor_image(&mut f, CursorImageStatus::default_named());
    f.niri().pointer_visibility = PointerVisibility::Hidden;

    let output = f.niri_output(1);
    let without_pointer = render_output_pixels(&mut f, &output, RenderTarget::Output, false);
    let with_pointer = render_output_pixels(&mut f, &output, RenderTarget::Output, true);

    assert_ne!(with_pointer, without_pointer);
}

#[test]
fn force_cursor_shape_overrides_hidden_client_cursor_on_output() {
    let Some(mut f) = set_up(config_with_force_cursor_shape(ForceCursorShape::Shape(
        CursorIcon::Crosshair,
    ))) else {
        return;
    };
    let window = create_window(&mut f, "test", (40, 30));
    move_cursor_to_window(&mut f, window);
    set_cursor_image(&mut f, CursorImageStatus::Hidden);

    assert_named_cursor(output_cursor_image(&mut f), CursorIcon::Crosshair);

    let output = f.niri_output(1);
    let without_pointer = render_output_pixels(&mut f, &output, RenderTarget::Output, false);
    let with_pointer = render_output_pixels(&mut f, &output, RenderTarget::Output, true);
    assert_ne!(with_pointer, without_pointer);
}

#[test]
fn force_cursor_shape_freezes_named_client_cursor_changes() {
    let Some(mut f) = set_up(config_with_force_cursor_shape(ForceCursorShape::Shape(
        CursorIcon::Crosshair,
    ))) else {
        return;
    };
    let window = create_window(&mut f, "test", (40, 30));
    move_cursor_to_window(&mut f, window);

    set_cursor_image(&mut f, CursorImageStatus::Named(CursorIcon::Pointer));
    assert_named_cursor(output_cursor_image(&mut f), CursorIcon::Crosshair);

    set_cursor_image(&mut f, CursorImageStatus::Named(CursorIcon::Text));
    assert_named_cursor(output_cursor_image(&mut f), CursorIcon::Crosshair);
}

#[test]
fn later_force_cursor_shape_none_restores_app_control() {
    let mut config = Config::default();
    config.window_rules.push(WindowRule {
        matches: vec![WindowMatch {
            title: Some(RegexEq::from_str("^test$").unwrap()),
            ..Default::default()
        }],
        force_cursor_shape: Some(ForceCursorShape::Shape(CursorIcon::Crosshair)),
        ..Default::default()
    });
    config.window_rules.push(WindowRule {
        matches: vec![WindowMatch {
            title: Some(RegexEq::from_str("^test$").unwrap()),
            ..Default::default()
        }],
        force_cursor_shape: Some(ForceCursorShape::None),
        ..Default::default()
    });

    let Some(mut f) = set_up(config) else {
        return;
    };
    let window = create_window(&mut f, "test", (40, 30));
    move_cursor_to_window(&mut f, window);

    set_cursor_image(&mut f, CursorImageStatus::Named(CursorIcon::Pointer));
    assert_named_cursor(output_cursor_image(&mut f), CursorIcon::Pointer);

    set_cursor_image(&mut f, CursorImageStatus::Named(CursorIcon::Text));
    assert_named_cursor(output_cursor_image(&mut f), CursorIcon::Text);
}

#[test]
fn force_cursor_shape_does_not_override_compositor_cursor() {
    let Some(mut f) = set_up(config_with_force_cursor_shape(ForceCursorShape::Shape(
        CursorIcon::Text,
    ))) else {
        return;
    };
    let window = create_window(&mut f, "test", (40, 30));
    move_cursor_to_window(&mut f, window);
    set_cursor_image(&mut f, CursorImageStatus::Named(CursorIcon::Pointer));
    f.niri()
        .cursor_manager
        .set_compositor_cursor_image(CursorImageStatus::Named(CursorIcon::Crosshair));

    assert_named_cursor(output_cursor_image(&mut f), CursorIcon::Crosshair);
}

#[test]
fn force_cursor_shape_overrides_client_surface() {
    let Some(mut f) = set_up(config_with_force_cursor_shape(ForceCursorShape::Shape(
        CursorIcon::Crosshair,
    ))) else {
        return;
    };
    let window = create_window(&mut f, "test", (40, 30));
    move_cursor_to_window(&mut f, window);

    let surface = window_surface(&mut f, window);
    set_cursor_image(&mut f, CursorImageStatus::Surface(surface));

    assert_named_cursor(output_cursor_image(&mut f), CursorIcon::Crosshair);
}

#[test]
fn virtual_output_frame_callbacks_tick_cursor_surfaces() {
    let mut f = Fixture::new();

    let name = {
        let state = f.niri_state();
        state
            .backend
            .create_virtual_output(&mut state.niri, 100, 100, 60, Some("virt".to_owned()))
            .unwrap()
    };

    let output = f
        .niri()
        .global_space
        .outputs()
        .find(|o| o.name() == name)
        .unwrap()
        .clone();

    let id = f.add_client();
    let window = f.client(id).create_window();
    let client_surface = window.surface.clone();
    window.set_title("cursor-source");
    window.commit();
    f.roundtrip(id);

    let window = f.client(id).window(&client_surface);
    window.attach_rgba_buffer(WINDOW_COLOR);
    window.set_size(20, 20);
    window.ack_last_and_commit();
    f.double_roundtrip(id);

    let mapped = f.niri().layout.focus().unwrap().id();
    let server_surface = window_surface(&mut f, mapped);

    let window = f.client(id).window(&client_surface);
    window.attach_null();
    window.commit();
    f.double_roundtrip(id);

    let frame_done = Arc::new(SyncData::default());
    {
        let client = f.client(id);
        client_surface.frame(&client.qh, frame_done.clone());
        client_surface.commit();
        client.connection.flush().unwrap();
    }
    f.dispatch();

    set_cursor_image(&mut f, CursorImageStatus::Surface(server_surface.clone()));
    with_states(&server_surface, |states| {
        states
            .data_map
            .insert_if_missing_threadsafe(CursorImageSurfaceData::default);
    });
    f.niri().send_frame_callbacks_for_virtual_output(&output);
    f.niri_state().refresh_and_flush_clients();

    for _ in 0..10 {
        if frame_done.done.load(Ordering::Relaxed) {
            break;
        }
        f.dispatch();
    }

    assert!(frame_done.done.load(Ordering::Relaxed));
}

#[test]
fn draw_cursor_always_hidden_overrides_force_cursor_shape() {
    let mut config = Config::default();
    config.window_rules.push(WindowRule {
        matches: vec![WindowMatch {
            title: Some(RegexEq::from_str("^test$").unwrap()),
            ..Default::default()
        }],
        draw_cursor: Some(DrawCursor::AlwaysHidden),
        force_cursor_shape: Some(ForceCursorShape::Shape(CursorIcon::Crosshair)),
        ..Default::default()
    });

    let Some(mut f) = set_up(config) else {
        return;
    };
    let window = create_window(&mut f, "test", (40, 30));
    move_cursor_to_window(&mut f, window);
    set_cursor_image(&mut f, CursorImageStatus::Named(CursorIcon::Pointer));

    assert!(matches!(
        output_cursor_image(&mut f),
        CursorImageStatus::Hidden
    ));
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
fn workspace_screencast_keeps_cursor_over_sticky_window_on_inactive_workspace() {
    let Some(mut f) = set_up(Config::default()) else {
        return;
    };
    let window = create_window(&mut f, "test", (40, 30));
    move_cursor_to_window(&mut f, window);

    let output = f.niri_output(1);
    let workspace_id = f.niri().layout.active_workspace().unwrap().id();
    f.niri().layout.toggle_window_sticky(Some(&window));
    f.niri()
        .layout
        .monitor_for_output_mut(&output)
        .unwrap()
        .add_workspace_bottom();
    f.niri().layout.switch_workspace_down();

    let without_pointer = render_workspace_screencast_pixels(&mut f, &output, workspace_id, false);
    let with_pointer = render_workspace_screencast_pixels(&mut f, &output, workspace_id, true);

    assert_ne!(with_pointer, without_pointer);
}

#[test]
fn workspace_screencast_keeps_cursor_over_layer_surface_on_inactive_workspace() {
    let Some(mut f) = set_up(Config::default()) else {
        return;
    };
    create_top_layer(&mut f, (100, 20));

    let output = f.niri_output(1);
    let workspace_id = f.niri().layout.active_workspace().unwrap().id();
    f.niri()
        .layout
        .monitor_for_output_mut(&output)
        .unwrap()
        .add_workspace_bottom();
    f.niri().layout.switch_workspace_down();
    move_cursor_to_point(&mut f, Point::from((10.0, 10.0)));

    let without_pointer = render_workspace_screencast_pixels(&mut f, &output, workspace_id, false);
    let with_pointer = render_workspace_screencast_pixels(&mut f, &output, workspace_id, true);

    assert_ne!(with_pointer, without_pointer);
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
