use std::str::FromStr;

use niri_config::layer_rule::{LayerRule, Match as LayerMatch};
use niri_config::utils::RegexEq;
use niri_config::window_rule::{Match as WindowMatch, WindowRule};
use niri_config::{Action, BlockOutFrom, Config};
use niri_ipc::state::EventStreamStatePart as _;
use niri_ipc::{BlockOutFrom as IpcBlockOutFrom, Layer as IpcLayer};
use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::utils::{Relocate, RelocateRenderElement};
use smithay::output::Output;
use smithay::reexports::wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_shell_v1::Layer;
use smithay::reexports::wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_surface_v1::Anchor;
use smithay::utils::{Physical, Scale, Size, Transform};
use wayland_client::protocol::wl_output::WlOutput;
use wayland_client::protocol::wl_surface::WlSurface;

use super::client::{ClientId, LayerConfigureProps};
use super::*;
use crate::render_helpers::{
    encompassing_geo, render_to_vec as render_pixels, RenderCtx, RenderTarget,
};

const RED: [u32; 4] = [u32::MAX, 0, 0, u32::MAX];
const GREEN: [u32; 4] = [0, u32::MAX, 0, u32::MAX];

fn set_up(config: Config) -> Option<Fixture> {
    let mut f = Fixture::with_config(config);
    if f.niri_state().backend.headless().add_renderer().is_err() {
        eprintln!("skipping capture block-out test: headless EGL renderer unavailable");
        return None;
    }
    f.add_output(1, (100, 100));
    Some(f)
}

fn create_window(
    f: &mut Fixture,
    id: ClientId,
    title: &str,
    size: (u16, u16),
    rgba: [u32; 4],
) -> WlSurface {
    let window = f.client(id).create_window();
    let surface = window.surface.clone();
    window.set_title(title);
    window.commit();
    f.roundtrip(id);

    let window = f.client(id).window(&surface);
    window.attach_rgba_buffer(rgba);
    window.set_size(size.0, size.1);
    window.ack_last_and_commit();
    f.double_roundtrip(id);

    surface
}

fn create_layer(
    f: &mut Fixture,
    id: ClientId,
    output: &WlOutput,
    layer_kind: Layer,
    namespace: &str,
    props: LayerConfigureProps,
    size: (u16, u16),
    rgba: [u32; 4],
) -> WlSurface {
    let layer = f
        .client(id)
        .create_layer(Some(output), layer_kind, namespace);
    let surface = layer.surface.clone();
    layer.set_configure_props(props);
    layer.commit();
    f.roundtrip(id);

    let layer = f.client(id).layer(&surface);
    layer.attach_rgba_buffer(rgba);
    layer.set_size(size.0, size.1);
    layer.ack_last_and_commit();
    f.double_roundtrip(id);

    surface
}

fn render_output_pixels(
    f: &mut Fixture,
    output: &Output,
    target: RenderTarget,
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
            let elements = niri.render_to_vec(ctx, &output, false);
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

fn render_window_cast_pixels(f: &mut Fixture, output: &Output) -> (Size<i32, Physical>, Vec<u8>) {
    let output = output.clone();
    let state = f.niri_state();
    let (backend, niri) = (&mut state.backend, &mut state.niri);

    backend
        .with_primary_renderer(|renderer| {
            niri.update_render_elements(Some(&output));

            let scale = Scale::from(output.current_scale().fractional_scale());
            let mapped = niri.layout.windows_for_output(&output).next().unwrap();

            let mut elements = Vec::new();
            mapped.render_for_screen_cast(renderer, scale, niri.block_out_enabled, &mut |elem| {
                elements.push(elem)
            });

            let geo = encompassing_geo(scale, elements.iter());
            let elements = elements.iter().rev().map(|elem| {
                RelocateRenderElement::from_element(elem, geo.loc.upscale(-1), Relocate::Relative)
            });
            let pixels = render_pixels(
                renderer,
                geo.size,
                scale,
                Transform::Normal,
                Fourcc::Abgr8888,
                elements,
            )
            .unwrap();

            (geo.size, pixels)
        })
        .unwrap()
}

fn sample_pixel(size: Size<i32, Physical>, pixels: &[u8], x: i32, y: i32) -> [u8; 4] {
    let idx = ((y * size.w + x) * 4) as usize;
    [
        pixels[idx],
        pixels[idx + 1],
        pixels[idx + 2],
        pixels[idx + 3],
    ]
}

fn focused_window_is_block_out(f: &mut Fixture) -> bool {
    let state = f.niri_state();
    state.ipc_refresh_layout();

    let server = state.niri.ipc_server.as_ref().unwrap();
    let state = server.test_event_stream_state();
    state
        .windows
        .windows
        .values()
        .find(|window| window.is_focused)
        .unwrap()
        .is_block_out
}

fn block_out_state(f: &mut Fixture) -> niri_ipc::BlockOutState {
    f.niri().block_out_state()
}

fn event_stream_block_out_state(f: &mut Fixture) -> niri_ipc::BlockOutState {
    let state = f.niri_state();
    state.ipc_refresh_block_out();

    let server = state.niri.ipc_server.as_ref().unwrap();
    let state = server.test_event_stream_state();
    state.block_out.block_out_state.clone().unwrap()
}

fn replicated_event_stream_block_out_state(f: &mut Fixture) -> niri_ipc::BlockOutState {
    let state = f.niri_state();
    state.ipc_refresh_block_out();

    let server = state.niri.ipc_server.as_ref().unwrap();
    let state = server.test_event_stream_state();
    let events = state.block_out.replicate();
    assert_eq!(events.len(), 1);

    match events.into_iter().next().unwrap() {
        niri_ipc::Event::BlockOutStateChanged { block_out_state } => block_out_state,
        event => panic!("unexpected event: {event:?}"),
    }
}

#[test]
fn blocked_layer_reveals_background_in_screen_capture() {
    let mut config = Config::default();
    config.layout.gaps = 0.;
    config.layer_rules.push(LayerRule {
        matches: vec![LayerMatch {
            namespace: Some(RegexEq::from_str("^blocked$").unwrap()),
            ..Default::default()
        }],
        block_out_from: Some(BlockOutFrom::ScreenCapture),
        ..Default::default()
    });

    let Some(mut f) = set_up(config) else {
        return;
    };
    let id = f.add_client();
    let output = f.client(id).output("headless-1");

    create_layer(
        &mut f,
        id,
        &output,
        Layer::Background,
        "background",
        LayerConfigureProps {
            anchor: Some(Anchor::Top | Anchor::Bottom | Anchor::Left | Anchor::Right),
            size: Some((0, 0)),
            ..Default::default()
        },
        (100, 100),
        RED,
    );
    create_layer(
        &mut f,
        id,
        &output,
        Layer::Top,
        "blocked",
        LayerConfigureProps {
            anchor: Some(Anchor::Top | Anchor::Left),
            size: Some((40, 40)),
            ..Default::default()
        },
        (40, 40),
        GREEN,
    );

    let output = f.niri_output(1);
    let (size, pixels) = render_output_pixels(&mut f, &output, RenderTarget::ScreenCapture);

    assert_eq!(sample_pixel(size, &pixels, 10, 10), [255, 0, 0, 255]);
    assert_eq!(sample_pixel(size, &pixels, 80, 80), [255, 0, 0, 255]);
}

#[test]
fn blocked_window_cast_is_fully_transparent() {
    let mut config = Config::default();
    config.layout.gaps = 0.;
    config.window_rules.push(WindowRule {
        matches: vec![WindowMatch {
            title: Some(RegexEq::from_str("^blocked$").unwrap()),
            ..Default::default()
        }],
        block_out_from: Some(BlockOutFrom::Screencast),
        ..Default::default()
    });

    let Some(mut f) = set_up(config) else {
        return;
    };
    let id = f.add_client();

    create_window(&mut f, id, "blocked", (40, 30), GREEN);

    let output = f.niri_output(1);
    let (size, pixels) = render_window_cast_pixels(&mut f, &output);

    assert_eq!(
        sample_pixel(size, &pixels, size.w / 2, size.h / 2),
        [0, 0, 0, 0]
    );
}

#[test]
fn toggle_block_out_window_enables_and_disables_ruleless_window() {
    let Some(mut f) = set_up(Config::default()) else {
        return;
    };
    let id = f.add_client();

    create_window(&mut f, id, "blocked", (40, 30), GREEN);

    assert!(!focused_window_is_block_out(&mut f));

    f.niri_state()
        .do_action(Action::ToggleBlockOutWindow, false);
    assert!(focused_window_is_block_out(&mut f));

    let output = f.niri_output(1);
    let (size, pixels) = render_window_cast_pixels(&mut f, &output);
    assert_eq!(
        sample_pixel(size, &pixels, size.w / 2, size.h / 2),
        [0, 0, 0, 0]
    );

    f.niri_state()
        .do_action(Action::ToggleBlockOutWindow, false);
    assert!(!focused_window_is_block_out(&mut f));

    let (size, pixels) = render_window_cast_pixels(&mut f, &output);
    assert_eq!(
        sample_pixel(size, &pixels, size.w / 2, size.h / 2),
        [0, 255, 0, 255]
    );
}

#[test]
fn toggle_block_out_window_disables_and_restores_configured_rule() {
    let mut config = Config::default();
    config.window_rules.push(WindowRule {
        matches: vec![WindowMatch {
            title: Some(RegexEq::from_str("^blocked$").unwrap()),
            ..Default::default()
        }],
        block_out_from: Some(BlockOutFrom::ScreenCapture),
        ..Default::default()
    });

    let Some(mut f) = set_up(config) else {
        return;
    };
    let id = f.add_client();

    create_window(&mut f, id, "blocked", (40, 30), GREEN);

    assert!(focused_window_is_block_out(&mut f));

    f.niri_state()
        .do_action(Action::ToggleBlockOutWindow, false);
    assert!(!focused_window_is_block_out(&mut f));

    let output = f.niri_output(1);
    let (size, pixels) = render_window_cast_pixels(&mut f, &output);
    assert_eq!(
        sample_pixel(size, &pixels, size.w / 2, size.h / 2),
        [0, 255, 0, 255]
    );

    f.niri_state()
        .do_action(Action::ToggleBlockOutWindow, false);
    assert!(focused_window_is_block_out(&mut f));

    let (size, pixels) = render_window_cast_pixels(&mut f, &output);
    assert_eq!(
        sample_pixel(size, &pixels, size.w / 2, size.h / 2),
        [0, 0, 0, 0]
    );
}

#[test]
fn toggle_block_out_globally_disables_layer_block_out() {
    let mut config = Config::default();
    config.layout.gaps = 0.;
    config.layer_rules.push(LayerRule {
        matches: vec![LayerMatch {
            namespace: Some(RegexEq::from_str("^blocked$").unwrap()),
            ..Default::default()
        }],
        block_out_from: Some(BlockOutFrom::ScreenCapture),
        ..Default::default()
    });

    let Some(mut f) = set_up(config) else {
        return;
    };
    let id = f.add_client();
    let output = f.client(id).output("headless-1");

    create_layer(
        &mut f,
        id,
        &output,
        Layer::Background,
        "background",
        LayerConfigureProps {
            anchor: Some(Anchor::Top | Anchor::Bottom | Anchor::Left | Anchor::Right),
            size: Some((0, 0)),
            ..Default::default()
        },
        (100, 100),
        RED,
    );
    create_layer(
        &mut f,
        id,
        &output,
        Layer::Top,
        "blocked",
        LayerConfigureProps {
            anchor: Some(Anchor::Top | Anchor::Left),
            size: Some((40, 40)),
            ..Default::default()
        },
        (40, 40),
        GREEN,
    );

    let output = f.niri_output(1);

    let (size, pixels) = render_output_pixels(&mut f, &output, RenderTarget::ScreenCapture);
    assert_eq!(sample_pixel(size, &pixels, 10, 10), [255, 0, 0, 255]);

    f.niri_state().do_action(Action::ToggleBlockOut, false);

    let (size, pixels) = render_output_pixels(&mut f, &output, RenderTarget::ScreenCapture);
    assert_eq!(sample_pixel(size, &pixels, 10, 10), [0, 255, 0, 255]);

    f.niri_state().do_action(Action::ToggleBlockOut, false);

    let (size, pixels) = render_output_pixels(&mut f, &output, RenderTarget::ScreenCapture);
    assert_eq!(sample_pixel(size, &pixels, 10, 10), [255, 0, 0, 255]);
}

#[test]
fn toggle_block_out_globally_disables_window_rendering_but_not_window_state() {
    let mut config = Config::default();
    config.window_rules.push(WindowRule {
        matches: vec![WindowMatch {
            title: Some(RegexEq::from_str("^blocked$").unwrap()),
            ..Default::default()
        }],
        block_out_from: Some(BlockOutFrom::Screencast),
        ..Default::default()
    });

    let Some(mut f) = set_up(config) else {
        return;
    };
    let id = f.add_client();

    create_window(&mut f, id, "blocked", (40, 30), GREEN);

    let output = f.niri_output(1);

    assert!(focused_window_is_block_out(&mut f));

    let (size, pixels) = render_window_cast_pixels(&mut f, &output);
    assert_eq!(
        sample_pixel(size, &pixels, size.w / 2, size.h / 2),
        [0, 0, 0, 0]
    );

    f.niri_state().do_action(Action::ToggleBlockOut, false);
    assert!(focused_window_is_block_out(&mut f));

    let (size, pixels) = render_window_cast_pixels(&mut f, &output);
    assert_eq!(
        sample_pixel(size, &pixels, size.w / 2, size.h / 2),
        [0, 255, 0, 255]
    );

    f.niri_state().do_action(Action::ToggleBlockOut, false);
    assert!(focused_window_is_block_out(&mut f));

    let (size, pixels) = render_window_cast_pixels(&mut f, &output);
    assert_eq!(
        sample_pixel(size, &pixels, size.w / 2, size.h / 2),
        [0, 0, 0, 0]
    );
}

#[test]
fn block_out_state_reports_window_scope_and_global_toggle() {
    let mut config = Config::default();
    config.window_rules.push(WindowRule {
        matches: vec![WindowMatch {
            title: Some(RegexEq::from_str("^blocked$").unwrap()),
            ..Default::default()
        }],
        block_out_from: Some(BlockOutFrom::ScreenCapture),
        ..Default::default()
    });

    let Some(mut f) = set_up(config) else {
        return;
    };
    let id = f.add_client();

    create_window(&mut f, id, "blocked", (40, 30), GREEN);

    let state = block_out_state(&mut f);
    assert!(state.is_enabled);
    assert_eq!(state.windows.len(), 1);
    assert_eq!(state.layers.len(), 0);
    assert!(state.windows[0].id > 0);
    assert_eq!(state.windows[0].title.as_deref(), Some("blocked"));
    assert_eq!(
        state.windows[0].block_out_from,
        IpcBlockOutFrom::ScreenCapture
    );

    f.niri_state().do_action(Action::ToggleBlockOut, false);

    let state = block_out_state(&mut f);
    assert!(!state.is_enabled);
    assert_eq!(state.windows.len(), 1);
    assert_eq!(
        state.windows[0].block_out_from,
        IpcBlockOutFrom::ScreenCapture
    );
}

#[test]
fn block_out_state_reports_runtime_window_toggle_scope() {
    let Some(mut f) = set_up(Config::default()) else {
        return;
    };
    let id = f.add_client();

    create_window(&mut f, id, "blocked", (40, 30), GREEN);

    let state = block_out_state(&mut f);
    assert!(state.windows.is_empty());

    f.niri_state()
        .do_action(Action::ToggleBlockOutWindow, false);

    let state = block_out_state(&mut f);
    assert_eq!(state.windows.len(), 1);
    assert_eq!(state.windows[0].block_out_from, IpcBlockOutFrom::Screencast);

    f.niri_state()
        .do_action(Action::ToggleBlockOutWindow, false);

    let state = block_out_state(&mut f);
    assert!(state.windows.is_empty());
}

#[test]
fn block_out_state_reports_layer_scope_and_global_toggle() {
    let mut config = Config::default();
    config.layer_rules.push(LayerRule {
        matches: vec![LayerMatch {
            namespace: Some(RegexEq::from_str("^blocked$").unwrap()),
            ..Default::default()
        }],
        block_out_from: Some(BlockOutFrom::Screencast),
        ..Default::default()
    });

    let Some(mut f) = set_up(config) else {
        return;
    };
    let id = f.add_client();
    let output = f.client(id).output("headless-1");

    create_layer(
        &mut f,
        id,
        &output,
        Layer::Top,
        "blocked",
        LayerConfigureProps {
            anchor: Some(Anchor::Top | Anchor::Left),
            size: Some((40, 40)),
            ..Default::default()
        },
        (40, 40),
        GREEN,
    );

    let state = block_out_state(&mut f);
    assert!(state.is_enabled);
    assert!(state.windows.is_empty());
    assert_eq!(state.layers.len(), 1);
    assert_eq!(state.layers[0].namespace, "blocked");
    assert_eq!(state.layers[0].output, "headless-1");
    assert_eq!(state.layers[0].layer, IpcLayer::Top);
    assert_eq!(state.layers[0].block_out_from, IpcBlockOutFrom::Screencast);

    f.niri_state().do_action(Action::ToggleBlockOut, false);

    let state = block_out_state(&mut f);
    assert!(!state.is_enabled);
    assert_eq!(state.layers.len(), 1);
    assert_eq!(state.layers[0].block_out_from, IpcBlockOutFrom::Screencast);
}

#[test]
fn event_stream_block_out_replicates_initial_snapshot() {
    let mut config = Config::default();
    config.window_rules.push(WindowRule {
        matches: vec![WindowMatch {
            title: Some(RegexEq::from_str("^blocked$").unwrap()),
            ..Default::default()
        }],
        block_out_from: Some(BlockOutFrom::ScreenCapture),
        ..Default::default()
    });

    let Some(mut f) = set_up(config) else {
        return;
    };
    let id = f.add_client();

    create_window(&mut f, id, "blocked", (40, 30), GREEN);

    let state = replicated_event_stream_block_out_state(&mut f);
    assert!(state.is_enabled);
    assert_eq!(state.windows.len(), 1);
    assert_eq!(state.windows[0].title.as_deref(), Some("blocked"));
    assert_eq!(
        state.windows[0].block_out_from,
        IpcBlockOutFrom::ScreenCapture
    );
}

#[test]
fn event_stream_block_out_tracks_runtime_window_toggle() {
    let Some(mut f) = set_up(Config::default()) else {
        return;
    };
    let id = f.add_client();

    create_window(&mut f, id, "blocked", (40, 30), GREEN);

    let state = event_stream_block_out_state(&mut f);
    assert!(state.windows.is_empty());

    f.niri_state()
        .do_action(Action::ToggleBlockOutWindow, false);

    let state = event_stream_block_out_state(&mut f);
    assert_eq!(state.windows.len(), 1);
    assert_eq!(state.windows[0].block_out_from, IpcBlockOutFrom::Screencast);

    f.niri_state()
        .do_action(Action::ToggleBlockOutWindow, false);

    let state = event_stream_block_out_state(&mut f);
    assert!(state.windows.is_empty());
}

#[test]
fn event_stream_block_out_tracks_global_enable_toggle() {
    let mut config = Config::default();
    config.window_rules.push(WindowRule {
        matches: vec![WindowMatch {
            title: Some(RegexEq::from_str("^blocked$").unwrap()),
            ..Default::default()
        }],
        block_out_from: Some(BlockOutFrom::Screencast),
        ..Default::default()
    });

    let Some(mut f) = set_up(config) else {
        return;
    };
    let id = f.add_client();

    create_window(&mut f, id, "blocked", (40, 30), GREEN);

    let state = event_stream_block_out_state(&mut f);
    assert!(state.is_enabled);

    f.niri_state().do_action(Action::ToggleBlockOut, false);

    let state = event_stream_block_out_state(&mut f);
    assert!(!state.is_enabled);

    f.niri_state().do_action(Action::ToggleBlockOut, false);

    let state = event_stream_block_out_state(&mut f);
    assert!(state.is_enabled);
}

#[test]
fn event_stream_block_out_tracks_layer_only_snapshot_updates() {
    let mut config = Config::default();
    config.layer_rules.push(LayerRule {
        matches: vec![LayerMatch {
            namespace: Some(RegexEq::from_str("^blocked$").unwrap()),
            ..Default::default()
        }],
        block_out_from: Some(BlockOutFrom::Screencast),
        ..Default::default()
    });

    let Some(mut f) = set_up(config) else {
        return;
    };
    let id = f.add_client();
    let output = f.client(id).output("headless-1");

    create_layer(
        &mut f,
        id,
        &output,
        Layer::Top,
        "blocked",
        LayerConfigureProps {
            anchor: Some(Anchor::Top | Anchor::Left),
            size: Some((40, 40)),
            ..Default::default()
        },
        (40, 40),
        GREEN,
    );

    let state = event_stream_block_out_state(&mut f);
    assert!(state.is_enabled);
    assert!(state.windows.is_empty());
    assert_eq!(state.layers.len(), 1);
    assert_eq!(state.layers[0].namespace, "blocked");

    f.niri_state().do_action(Action::ToggleBlockOut, false);

    let state = event_stream_block_out_state(&mut f);
    assert!(!state.is_enabled);
    assert_eq!(state.layers.len(), 1);
    assert_eq!(state.layers[0].block_out_from, IpcBlockOutFrom::Screencast);
}
