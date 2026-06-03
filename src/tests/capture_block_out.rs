use std::str::FromStr;

use niri_config::input::FocusFollowsMouse;
use niri_config::layer_rule::{LayerRule, Match as LayerMatch};
use niri_config::utils::RegexEq;
use niri_config::window_rule::{Match as WindowMatch, WindowRule};
use niri_config::{Action, BlockOutFrom, Config, CornerRadius};
use niri_ipc::state::EventStreamStatePart as _;
use niri_ipc::{
    BlockOutFrom as IpcBlockOutFrom, Layer as IpcLayer, PositionChange, SizeChange,
    Transform as IpcTransform,
};
use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::utils::{
    CropRenderElement, Relocate, RelocateRenderElement,
};
use smithay::backend::renderer::element::{
    Element as _, Id, Kind, RenderElementPresentationState, RenderElementState, RenderElementStates,
};
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::desktop::utils::surface_primary_scanout_output;
use smithay::output::Output;
use smithay::reexports::wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_shell_v1::Layer;
use smithay::reexports::wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_surface_v1::Anchor;
use smithay::utils::{Logical, Physical, Point, Scale, Size, Transform};
use smithay::wayland::compositor::with_states;
use wayland_client::protocol::wl_output::WlOutput;
use wayland_client::protocol::wl_surface::WlSurface;

use super::client::{ClientId, LayerConfigureProps};
use super::*;
use crate::layout::{
    ActivateWindow, AddWindowTarget, HitType, LayoutElement as _, LayoutElementRenderElement,
    LayoutElementRenderSnapshot,
};
use crate::render_helpers::texture::{TextureBuffer, TextureRenderElement};
use crate::render_helpers::xray::XrayPos;
use crate::render_helpers::{
    encompassing_geo, render_to_vec as render_pixels, RenderCtx, RenderTarget,
};
use crate::utils::center_preferring_top_left_in_area;
use crate::window::mapped::MappedId;
use crate::window::Mapped;

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

fn create_window_with_geometry(
    f: &mut Fixture,
    id: ClientId,
    title: &str,
    size: (u16, u16),
    geometry: (i32, i32, i32, i32),
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
    window.set_window_geometry(geometry.0, geometry.1, geometry.2, geometry.3);
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

fn render_window_cast_pixels_for(
    f: &mut Fixture,
    output: &Output,
    id: MappedId,
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

fn render_cropped_window_cast_pixels_for(
    f: &mut Fixture,
    output: &Output,
    id: MappedId,
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

            let mut elements = Vec::new();
            mapped.render_for_screen_cast(renderer, scale, niri.block_out_enabled, &mut |elem| {
                elements.push(elem)
            });

            let geo = encompassing_geo(scale, elements.iter());
            let cropped = elements
                .into_iter()
                .filter_map(|elem| CropRenderElement::from_element(elem, scale, geo))
                .collect::<Vec<_>>();
            let elements = cropped.iter().rev().map(|elem| {
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

fn render_window_output_pixels_for(
    f: &mut Fixture,
    output: &Output,
    id: MappedId,
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

            let mut elements = Vec::new();
            mapped.render(
                RenderCtx {
                    renderer,
                    target: RenderTarget::Output,
                    block_out_enabled: niri.block_out_enabled,
                    xray: None,
                },
                Point::default(),
                scale,
                1.,
                XrayPos::default(),
                &mut |elem| elements.push(elem),
            );

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

fn render_snapshot_texture_pixels(
    renderer: &mut GlesRenderer,
    snapshot: &LayoutElementRenderSnapshot,
    scale: Scale<f64>,
) -> (Size<i32, Physical>, Vec<u8>) {
    let (texture, geo) = {
        let mut ctx = RenderCtx {
            renderer,
            target: RenderTarget::Screencast,
            block_out_enabled: false,
            xray: None,
        };
        snapshot.texture(ctx.r(), scale).cloned().unwrap()
    };

    let buffer = TextureBuffer::from_texture(renderer, texture, scale, Transform::Normal, vec![]);
    let element = TextureRenderElement::from_texture_buffer(
        buffer,
        Point::from((0., 0.)),
        1.,
        None,
        None,
        Kind::Unspecified,
    );
    let pixels = render_pixels(
        renderer,
        geo.size,
        scale,
        Transform::Normal,
        Fourcc::Abgr8888,
        std::iter::once(&element),
    )
    .unwrap();

    (geo.size, pixels)
}

fn create_window_mirror(f: &mut Fixture) -> MappedId {
    let niri = f.niri();
    let source_id = niri.layout.windows().next().unwrap().1.id();
    create_window_mirror_for(f, source_id)
}

fn create_window_mirror_for(f: &mut Fixture, source_id: MappedId) -> MappedId {
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
        ActivateWindow::Smart,
    );
    mirror_id
}

fn mirror_mapped_by_id(
    f: &mut Fixture,
    id: MappedId,
) -> (crate::layout::SizingMode, Size<i32, Logical>) {
    let mapped = f
        .niri()
        .layout
        .windows()
        .find(|(_, mapped)| mapped.id() == id)
        .map(|(_, mapped)| mapped)
        .unwrap();
    (mapped.pending_sizing_mode(), mapped.size())
}

fn is_mirror_linked_by_id(f: &mut Fixture, id: MappedId) -> bool {
    f.niri()
        .layout
        .windows()
        .find(|(_, mapped)| mapped.id() == id)
        .map(|(_, mapped)| mapped.is_mirror_linked())
        .unwrap()
}

fn window_output_name_by_id(f: &mut Fixture, id: MappedId) -> Option<String> {
    f.niri()
        .layout
        .windows()
        .find(|(_, mapped)| mapped.id() == id)
        .and_then(|(mon, _)| mon.map(|mon| mon.output_name().clone()))
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

fn focused_window_ids(f: &mut Fixture) -> Vec<u64> {
    let state = f.niri_state();
    state.ipc_refresh_layout();

    let server = state.niri.ipc_server.as_ref().unwrap();
    let state = server.test_event_stream_state();
    state
        .windows
        .windows
        .values()
        .filter(|window| window.is_focused)
        .map(|window| window.id)
        .collect()
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

fn rendered_element_states(ids: impl IntoIterator<Item = Id>) -> RenderElementStates {
    let mut states = RenderElementStates::default();
    for id in ids {
        states.states.insert(
            id,
            RenderElementState {
                visible_area: 1,
                presentation_state: RenderElementPresentationState::Rendering { reason: None },
                needs_capture: false,
            },
        );
    }
    states
}

fn first_surface_geometry(
    elements: &[LayoutElementRenderElement<GlesRenderer>],
    scale: Scale<f64>,
) -> smithay::utils::Rectangle<i32, Physical> {
    elements
        .iter()
        .find_map(|elem| match elem {
            LayoutElementRenderElement::Wayland(elem) => Some(elem.geometry(scale)),
            LayoutElementRenderElement::NamespacedWayland(elem) => Some(elem.geometry(scale)),
            LayoutElementRenderElement::MirrorScaledWayland(elem) => Some(elem.geometry(scale)),
            LayoutElementRenderElement::MirrorScaledClippedWayland(elem) => {
                Some(elem.geometry(scale))
            }
            LayoutElementRenderElement::SolidColor(_) => None,
            LayoutElementRenderElement::BackgroundEffect(_) => None,
        })
        .unwrap()
}

fn mirror_output_content_rect(
    f: &mut Fixture,
    output: &Output,
    id: MappedId,
) -> smithay::utils::Rectangle<i32, Physical> {
    let scale = Scale::from(output.current_scale().fractional_scale());
    let mapped = f
        .niri()
        .layout
        .windows()
        .find(|(_, mapped)| mapped.id() == id)
        .map(|(_, mapped)| mapped)
        .unwrap();
    let source_geometry = mapped.window.geometry().to_f64();
    let (loc, content_scale) = mapped.mirror_content_transform();
    smithay::utils::Rectangle::new(loc, source_geometry.size.upscale(content_scale))
        .to_physical_precise_round(scale)
}

fn assert_mirror_output_contents(
    f: &mut Fixture,
    output: &Output,
    id: MappedId,
    expected_size: Size<i32, Physical>,
    rgba: [u8; 4],
) {
    let (size, pixels) = render_window_output_pixels_for(f, output, id);
    assert_eq!(size, expected_size);

    let content = mirror_output_content_rect(f, output, id);
    let center_x = content.loc.x + content.size.w / 2;
    let center_y = content.loc.y + content.size.h / 2;
    assert_eq!(sample_pixel(size, &pixels, center_x, center_y), rgba);

    let edge_x = content.loc.x + (content.size.w - 2).max(0);
    let edge_y = content.loc.y + (content.size.h - 2).max(0);
    assert_eq!(sample_pixel(size, &pixels, edge_x, edge_y), rgba);

    if content.loc.x > 0 {
        assert_eq!(
            sample_pixel(size, &pixels, content.loc.x / 2, center_y),
            [0, 0, 0, 0]
        );
    }
    if content.loc.x + content.size.w < size.w {
        let x = content.loc.x + content.size.w + (size.w - content.loc.x - content.size.w) / 2;
        assert_eq!(sample_pixel(size, &pixels, x, center_y), [0, 0, 0, 0]);
    }
    if content.loc.y > 0 {
        assert_eq!(
            sample_pixel(size, &pixels, center_x, content.loc.y / 2),
            [0, 0, 0, 0]
        );
    }
    if content.loc.y + content.size.h < size.h {
        let y = content.loc.y + content.size.h + (size.h - content.loc.y - content.size.h) / 2;
        assert_eq!(sample_pixel(size, &pixels, center_x, y), [0, 0, 0, 0]);
    }
}

#[test]
fn mirror_window_renders_source_content() {
    let Some(mut f) = set_up(Config::default()) else {
        return;
    };
    let id = f.add_client();
    create_window(&mut f, id, "source", (40, 20), GREEN);

    let mirror_id = create_window_mirror(&mut f);
    let output = f.niri_output(1);
    let (size, pixels) = render_window_cast_pixels_for(&mut f, &output, mirror_id);

    assert_eq!(size, Size::from((40, 20)));
    assert_eq!(
        sample_pixel(size, &pixels, size.w / 2, size.h / 2),
        [0, 255, 0, 255]
    );
}

#[test]
fn mirror_window_resize_preserves_aspect_and_padding() {
    let Some(mut f) = set_up(Config::default()) else {
        return;
    };
    let id = f.add_client();
    create_window(&mut f, id, "source", (40, 20), RED);

    let mirror_id = create_window_mirror(&mut f);
    f.niri().layout.toggle_window_floating(Some(&mirror_id));
    f.niri()
        .layout
        .set_window_width(Some(&mirror_id), SizeChange::SetFixed(80));
    f.niri()
        .layout
        .set_window_height(Some(&mirror_id), SizeChange::SetFixed(80));

    let output = f.niri_output(1);
    let (size, pixels) = render_window_cast_pixels_for(&mut f, &output, mirror_id);

    assert_eq!(size, Size::from((80, 80)));
    assert_eq!(
        sample_pixel(size, &pixels, size.w / 2, size.h / 2),
        [255, 0, 0, 255]
    );
    assert_eq!(sample_pixel(size, &pixels, 5, size.h / 2), [255, 0, 0, 255]);
    assert_eq!(
        sample_pixel(size, &pixels, size.w - 6, size.h / 2),
        [255, 0, 0, 255]
    );
    assert_eq!(sample_pixel(size, &pixels, size.w / 2, 10), [0, 0, 0, 0]);
    assert_eq!(
        sample_pixel(size, &pixels, size.w / 2, size.h - 11),
        [0, 0, 0, 0]
    );

    let (padding_hit, content_hit) = {
        let mapped = f
            .niri()
            .layout
            .windows()
            .find(|(_, mapped)| mapped.id() == mirror_id)
            .map(|(_, mapped)| mapped)
            .unwrap();
        (
            mapped.is_in_input_region(Point::<f64, Logical>::from((40., 10.))),
            mapped.is_in_input_region(Point::<f64, Logical>::from((40., 40.))),
        )
    };
    assert!(!padding_hit);
    assert!(content_hit);
}

#[test]
fn mirror_output_render_scales_and_centers_contents_across_sizes() {
    for size in [(80, 80), (20, 60), (80, 10), (80, 40), (20, 10)] {
        let Some(mut f) = set_up(Config::default()) else {
            return;
        };
        let id = f.add_client();
        create_window(&mut f, id, "source", (40, 20), GREEN);

        let mirror_id = create_window_mirror(&mut f);
        f.niri().layout.toggle_window_floating(Some(&mirror_id));
        f.niri()
            .layout
            .set_window_width(Some(&mirror_id), SizeChange::SetFixed(size.0));
        f.niri()
            .layout
            .set_window_height(Some(&mirror_id), SizeChange::SetFixed(size.1));

        let output = f.niri_output(1);
        assert_mirror_output_contents(
            &mut f,
            &output,
            mirror_id,
            Size::from(size),
            [0, 255, 0, 255],
        );
    }
}

#[test]
fn mirror_equal_to_window_geometry_renders_1_to_1_on_output() {
    let Some(mut f) = set_up(Config::default()) else {
        return;
    };
    let id = f.add_client();
    create_window_with_geometry(&mut f, id, "source", (100, 60), (2, 2, 96, 56), GREEN);

    let mirror_id = create_window_mirror(&mut f);
    let output = f.niri_output(1);

    let mapped = f
        .niri()
        .layout
        .windows()
        .find(|(_, mapped)| mapped.id() == mirror_id)
        .map(|(_, mapped)| mapped)
        .unwrap();
    assert_eq!(mapped.size(), Size::from((96, 56)));
    assert_eq!(
        mapped.mirror_content_transform(),
        (Point::from((0., 0.)), 1.)
    );

    let (size, pixels) = render_window_output_pixels_for(&mut f, &output, mirror_id);
    assert_eq!(size, Size::from((96, 56)));
    assert_eq!(
        sample_pixel(size, &pixels, size.w / 2, size.h / 2),
        [0, 255, 0, 255]
    );
    assert_eq!(sample_pixel(size, &pixels, 0, size.h / 2), [0, 255, 0, 255]);
    assert_eq!(
        sample_pixel(size, &pixels, size.w - 1, size.h / 2),
        [0, 255, 0, 255]
    );
    assert_eq!(sample_pixel(size, &pixels, size.w / 2, 0), [0, 255, 0, 255]);
    assert_eq!(
        sample_pixel(size, &pixels, size.w / 2, size.h - 1),
        [0, 255, 0, 255]
    );
}

#[test]
fn mirror_equal_size_matches_source_surface_geometry_at_fractional_scale() {
    let Some(mut f) = set_up(Config::default()) else {
        return;
    };
    let id = f.add_client();
    create_window_with_geometry(&mut f, id, "source", (40, 20), (1, 1, 39, 19), GREEN);

    let source_id = f.niri().layout.windows().next().unwrap().1.id();
    let mirror_id = create_window_mirror_for(&mut f, source_id);
    let state = f.niri_state();
    let (backend, niri) = (&mut state.backend, &mut state.niri);
    backend
        .with_primary_renderer(|renderer| {
            let location = Point::from((0.4, 0.));
            let scale = Scale::from(1.25);
            let mut ctx = RenderCtx {
                renderer,
                target: RenderTarget::Output,
                block_out_enabled: niri.block_out_enabled,
                xray: None,
            };

            let source = niri
                .layout
                .windows()
                .find(|(_, mapped)| mapped.id() == source_id)
                .map(|(_, mapped)| mapped)
                .unwrap();
            let mut source_elements = Vec::new();
            source.render_normal(ctx.r(), location, scale, 1., &mut |elem| {
                source_elements.push(elem)
            });

            let mirror = niri
                .layout
                .windows()
                .find(|(_, mapped)| mapped.id() == mirror_id)
                .map(|(_, mapped)| mapped)
                .unwrap();
            let mut mirror_elements = Vec::new();
            mirror.render_normal(ctx.r(), location, scale, 1., &mut |elem| {
                mirror_elements.push(elem)
            });

            assert_eq!(
                first_surface_geometry(&mirror_elements, scale),
                first_surface_geometry(&source_elements, scale),
            );
        })
        .unwrap();
}

#[test]
fn tiled_mirror_one_pixel_short_keeps_source_scale() {
    let Some(mut f) = set_up(Config::default()) else {
        return;
    };
    let id = f.add_client();
    create_window(&mut f, id, "source", (40, 20), GREEN);

    let source_id = f.niri().layout.windows().next().unwrap().1.id();
    let mirror_id = create_window_mirror_for(&mut f, source_id);
    let state = f.niri_state();
    let (backend, niri) = (&mut state.backend, &mut state.niri);
    backend
        .with_primary_renderer(|renderer| {
            let scale = Scale::from(1.);
            let mut ctx = RenderCtx {
                renderer,
                target: RenderTarget::Output,
                block_out_enabled: niri.block_out_enabled,
                xray: None,
            };

            let mirror = niri
                .layout
                .windows()
                .find(|(_, mapped)| mapped.id() == mirror_id)
                .map(|(_, mapped)| mapped)
                .unwrap();
            let mut mirror_elements = Vec::new();
            mirror.render_normal_with_size(
                ctx.r(),
                Point::default(),
                Size::from((40., 19.)),
                scale,
                1.,
                &mut |elem| mirror_elements.push(elem),
            );

            assert_eq!(
                first_surface_geometry(&mirror_elements, scale).size,
                Size::from((40, 20)),
            );
        })
        .unwrap();
}

#[test]
fn tiled_mirror_near_double_size_keeps_integer_scale() {
    let Some(mut f) = set_up(Config::default()) else {
        return;
    };
    let id = f.add_client();
    create_window(&mut f, id, "source", (40, 20), GREEN);

    let source_id = f.niri().layout.windows().next().unwrap().1.id();
    let mirror_id = create_window_mirror_for(&mut f, source_id);
    let state = f.niri_state();
    let (backend, niri) = (&mut state.backend, &mut state.niri);
    backend
        .with_primary_renderer(|renderer| {
            let scale = Scale::from(1.);
            let mut ctx = RenderCtx {
                renderer,
                target: RenderTarget::Output,
                block_out_enabled: niri.block_out_enabled,
                xray: None,
            };

            let mirror = niri
                .layout
                .windows()
                .find(|(_, mapped)| mapped.id() == mirror_id)
                .map(|(_, mapped)| mapped)
                .unwrap();
            let mut mirror_elements = Vec::new();
            mirror.render_normal_with_size(
                ctx.r(),
                Point::default(),
                Size::from((79., 39.)),
                scale,
                1.,
                &mut |elem| mirror_elements.push(elem),
            );

            assert_eq!(
                first_surface_geometry(&mirror_elements, scale).size,
                Size::from((80, 40)),
            );
        })
        .unwrap();
}

#[test]
fn mirror_non_integer_scale_keeps_contain_scaling() {
    let Some(mut f) = set_up(Config::default()) else {
        return;
    };
    let id = f.add_client();
    create_window(&mut f, id, "source", (40, 20), GREEN);

    let source_id = f.niri().layout.windows().next().unwrap().1.id();
    let mirror_id = create_window_mirror_for(&mut f, source_id);
    let state = f.niri_state();
    let (backend, niri) = (&mut state.backend, &mut state.niri);
    backend
        .with_primary_renderer(|renderer| {
            let scale = Scale::from(1.);
            let mut ctx = RenderCtx {
                renderer,
                target: RenderTarget::Output,
                block_out_enabled: niri.block_out_enabled,
                xray: None,
            };

            let mirror = niri
                .layout
                .windows()
                .find(|(_, mapped)| mapped.id() == mirror_id)
                .map(|(_, mapped)| mapped)
                .unwrap();
            let mut mirror_elements = Vec::new();
            mirror.render_normal_with_size(
                ctx.r(),
                Point::default(),
                Size::from((70., 39.)),
                scale,
                1.,
                &mut |elem| mirror_elements.push(elem),
            );

            assert_eq!(
                first_surface_geometry(&mirror_elements, scale).size,
                Size::from((70, 35)),
            );
        })
        .unwrap();
}

#[test]
fn mirror_zoom_actions_crop_to_requested_region() {
    let Some(mut f) = set_up(Config::default()) else {
        return;
    };
    let id = f.add_client();
    create_window(&mut f, id, "source", (40, 20), GREEN);

    let mirror_id = create_window_mirror(&mut f);
    f.niri().layout.toggle_window_floating(Some(&mirror_id));
    f.niri()
        .layout
        .set_window_width(Some(&mirror_id), SizeChange::SetFixed(20));
    f.niri()
        .layout
        .set_window_height(Some(&mirror_id), SizeChange::SetFixed(20));
    f.niri().layout.activate_window(&mirror_id);

    f.niri_state()
        .do_action(Action::SetWindowMirrorZoom("2.0".into()), false);
    f.niri_state().do_action(
        Action::SetWindowMirrorCenterX(PositionChange::SetProportion(75.)),
        false,
    );

    let mapped = f
        .niri()
        .layout
        .windows()
        .find(|(_, mapped)| mapped.id() == mirror_id)
        .map(|(_, mapped)| mapped)
        .unwrap();
    assert_eq!(
        mapped.mirror_content_transform(),
        (Point::from((-20., 0.)), 1.)
    );
    assert_eq!(
        mapped.mirror_point_to_source(Point::from((10., 10.))),
        Some(Point::from((30., 10.))),
    );
}

#[test]
fn mirror_directional_pan_actions_move_by_visible_fraction() {
    let Some(mut f) = set_up(Config::default()) else {
        return;
    };
    let id = f.add_client();
    create_window(&mut f, id, "source", (40, 20), GREEN);

    let mirror_id = create_window_mirror(&mut f);
    f.niri().layout.toggle_window_floating(Some(&mirror_id));
    f.niri()
        .layout
        .set_window_width(Some(&mirror_id), SizeChange::SetFixed(20));
    f.niri()
        .layout
        .set_window_height(Some(&mirror_id), SizeChange::SetFixed(20));
    f.niri().layout.activate_window(&mirror_id);

    f.niri_state()
        .do_action(Action::SetWindowMirrorZoom("2.0".into()), false);

    let center = Point::from((10., 10.));
    let before = f
        .niri()
        .layout
        .windows()
        .find(|(_, mapped)| mapped.id() == mirror_id)
        .map(|(_, mapped)| mapped.mirror_point_to_source(center).unwrap())
        .unwrap();

    f.niri_state()
        .do_action(Action::MoveWindowMirrorViewRight, false);

    let after = f
        .niri()
        .layout
        .windows()
        .find(|(_, mapped)| mapped.id() == mirror_id)
        .map(|(_, mapped)| mapped.mirror_point_to_source(center).unwrap())
        .unwrap();

    assert_eq!(after, Point::from((before.x + 2., before.y)));
}

#[test]
fn flipped_mirror_point_maps_to_source_opposite_edge() {
    let Some(mut f) = set_up(Config::default()) else {
        return;
    };
    let id = f.add_client();
    create_window(&mut f, id, "source", (40, 20), GREEN);

    let mirror_id = create_window_mirror(&mut f);
    f.niri().layout.toggle_window_floating(Some(&mirror_id));
    f.niri()
        .layout
        .set_window_width(Some(&mirror_id), SizeChange::SetFixed(40));
    f.niri()
        .layout
        .set_window_height(Some(&mirror_id), SizeChange::SetFixed(20));
    f.niri().layout.activate_window(&mirror_id);
    f.niri_state().do_action(
        Action::SetWindowMirrorTransform(IpcTransform::Flipped),
        false,
    );

    let mapped = f
        .niri()
        .layout
        .windows()
        .find(|(_, mapped)| mapped.id() == mirror_id)
        .map(|(_, mapped)| mapped)
        .unwrap();
    assert_eq!(
        mapped.mirror_point_to_source(Point::from((1., 10.))),
        Some(Point::from((39., 10.))),
    );
    assert_eq!(
        mapped.mirror_point_to_source(Point::from((39., 10.))),
        Some(Point::from((1., 10.))),
    );
}

#[test]
fn rotated_mirror_directional_pan_is_screen_relative() {
    let Some(mut f) = set_up(Config::default()) else {
        return;
    };
    let id = f.add_client();
    create_window(&mut f, id, "source", (40, 20), GREEN);

    let mirror_id = create_window_mirror(&mut f);
    f.niri().layout.toggle_window_floating(Some(&mirror_id));
    f.niri()
        .layout
        .set_window_width(Some(&mirror_id), SizeChange::SetFixed(10));
    f.niri()
        .layout
        .set_window_height(Some(&mirror_id), SizeChange::SetFixed(20));
    f.niri().layout.activate_window(&mirror_id);
    f.niri_state()
        .do_action(Action::SetWindowMirrorTransform(IpcTransform::_90), false);
    f.niri_state()
        .do_action(Action::SetWindowMirrorZoom("2.0".into()), false);

    let center = Point::from((5., 10.));
    let before = f
        .niri()
        .layout
        .windows()
        .find(|(_, mapped)| mapped.id() == mirror_id)
        .map(|(_, mapped)| mapped.mirror_point_to_source(center).unwrap())
        .unwrap();

    f.niri_state()
        .do_action(Action::MoveWindowMirrorViewRight, false);

    let after = f
        .niri()
        .layout
        .windows()
        .find(|(_, mapped)| mapped.id() == mirror_id)
        .map(|(_, mapped)| mapped.mirror_point_to_source(center).unwrap())
        .unwrap();

    assert_eq!(after, Point::from((before.x, before.y - 1.)));
}

#[test]
fn transformed_mirror_anchor_preserves_source_point_when_zoomed() {
    let Some(mut f) = set_up(Config::default()) else {
        return;
    };
    let id = f.add_client();
    create_window(&mut f, id, "source", (40, 20), GREEN);

    let mirror_id = create_window_mirror(&mut f);
    f.niri().layout.toggle_window_floating(Some(&mirror_id));
    f.niri()
        .layout
        .set_window_width(Some(&mirror_id), SizeChange::SetFixed(20));
    f.niri()
        .layout
        .set_window_height(Some(&mirror_id), SizeChange::SetFixed(20));

    let anchor = Point::from((15., 10.));
    let source_point = Point::from((30., 10.));
    f.niri().layout.with_windows_mut(|mapped, _| {
        if mapped.id() == mirror_id {
            mapped.set_mirror_transform(Transform::_90);
            mapped.set_mirror_view_from_anchor(2., anchor, source_point);
        }
    });

    let mapped = f
        .niri()
        .layout
        .windows()
        .find(|(_, mapped)| mapped.id() == mirror_id)
        .map(|(_, mapped)| mapped)
        .unwrap();
    assert_eq!(mapped.mirror_zoom(), 2.);
    assert_eq!(mapped.mirror_point_to_source(anchor), Some(source_point));
}

#[test]
fn zoomed_mirror_hover_uses_source_surface_coords() {
    let Some(mut f) = set_up(Config::default()) else {
        return;
    };
    let id = f.add_client();
    create_window(&mut f, id, "source", (40, 20), GREEN);

    let mirror_id = create_window_mirror(&mut f);
    f.niri().layout.toggle_window_floating(Some(&mirror_id));
    f.niri()
        .layout
        .set_window_width(Some(&mirror_id), SizeChange::SetFixed(20));
    f.niri()
        .layout
        .set_window_height(Some(&mirror_id), SizeChange::SetFixed(20));
    f.niri().layout.activate_window(&mirror_id);
    f.niri_state()
        .do_action(Action::SetWindowMirrorZoom("2.0".into()), false);
    f.niri_state().do_action(
        Action::SetWindowMirrorCenterX(PositionChange::SetProportion(75.)),
        false,
    );

    let source_surface = f
        .niri()
        .layout
        .windows()
        .find(|(_, mapped)| !mapped.is_mirror())
        .map(|(_, mapped)| mapped.toplevel().wl_surface().clone())
        .unwrap();
    let (pos, _) = tile_geometry_for(&mut f, mirror_id);
    let point = Point::from((pos.x as f64 + 10., pos.y as f64 + 10.));
    let under = f.niri().contents_under(point);

    assert_eq!(under.window.map(|(id, _)| id), Some(mirror_id));
    let (surface, surface_pos) = under.surface.unwrap();
    assert_eq!(surface, source_surface);
    assert_eq!(surface_pos, Point::from((pos.x as f64 - 20., pos.y as f64)));
}

#[test]
fn zoomed_mirror_does_not_take_input_outside_window_bounds() {
    let Some(mut f) = set_up(Config::default()) else {
        return;
    };
    let id = f.add_client();
    create_window(&mut f, id, "source", (40, 20), GREEN);

    let mirror_id = create_window_mirror(&mut f);
    f.niri().layout.activate_window(&mirror_id);
    f.niri_state()
        .do_action(Action::SetWindowMirrorZoom("2.0".into()), false);
    f.niri_state().do_action(
        Action::SetWindowMirrorCenterX(PositionChange::SetProportion(75.)),
        false,
    );

    let (pos, size) = tile_geometry_for(&mut f, mirror_id);
    let outside = Point::from((pos.x as f64 - 1., pos.y as f64 + size.h as f64 / 2.));
    let under = f.niri().contents_under(outside);

    assert_ne!(under.window.map(|(id, _)| id), Some(mirror_id));
    let mapped = f
        .niri()
        .layout
        .windows()
        .find(|(_, mapped)| mapped.id() == mirror_id)
        .map(|(_, mapped)| mapped)
        .unwrap();
    assert_eq!(mapped.mirror_point_to_source(Point::from((-1., 10.))), None);
}

#[test]
fn mirror_padding_is_activate_only() {
    let Some(mut f) = set_up(Config::default()) else {
        return;
    };
    let id = f.add_client();
    create_window(&mut f, id, "source", (40, 20), GREEN);

    let mirror_id = create_window_mirror(&mut f);
    f.niri().layout.toggle_window_floating(Some(&mirror_id));
    f.niri()
        .layout
        .set_window_width(Some(&mirror_id), SizeChange::SetFixed(20));
    f.niri()
        .layout
        .set_window_height(Some(&mirror_id), SizeChange::SetFixed(20));

    let (pos, _) = tile_geometry_for(&mut f, mirror_id);
    let padding_point = Point::from((pos.x as f64 + 10., pos.y as f64 + 1.));
    let under = f.niri().contents_under(padding_point);

    assert!(matches!(
        under.window,
        Some((
            id,
            HitType::Activate {
                is_tab_indicator: false
            }
        )) if id == mirror_id
    ));
    assert!(under.surface.is_none());
}

#[test]
fn zoomed_mirror_hover_keeps_source_surface_origin_at_left_edge() {
    let Some(mut f) = set_up(Config::default()) else {
        return;
    };
    let id = f.add_client();
    create_window(&mut f, id, "source", (40, 20), GREEN);

    let mirror_id = create_window_mirror(&mut f);
    f.niri().layout.toggle_window_floating(Some(&mirror_id));
    f.niri()
        .layout
        .set_window_width(Some(&mirror_id), SizeChange::SetFixed(20));
    f.niri()
        .layout
        .set_window_height(Some(&mirror_id), SizeChange::SetFixed(20));
    f.niri().layout.activate_window(&mirror_id);
    f.niri_state()
        .do_action(Action::SetWindowMirrorZoom("2.0".into()), false);
    f.niri_state().do_action(
        Action::SetWindowMirrorCenterX(PositionChange::SetProportion(75.)),
        false,
    );

    let source_surface = f
        .niri()
        .layout
        .windows()
        .find(|(_, mapped)| !mapped.is_mirror())
        .map(|(_, mapped)| mapped.toplevel().wl_surface().clone())
        .unwrap();
    let (pos, _) = tile_geometry_for(&mut f, mirror_id);
    let center = Point::from((pos.x as f64 + 10., pos.y as f64 + 10.));
    let left_edge = Point::from((pos.x as f64 + 1., pos.y as f64 + 10.));

    let center_under = f.niri().contents_under(center);
    let left_under = f.niri().contents_under(left_edge);

    assert_eq!(center_under.window.map(|(id, _)| id), Some(mirror_id));
    assert_eq!(left_under.window.map(|(id, _)| id), Some(mirror_id));

    let (center_surface, center_pos) = center_under.surface.unwrap();
    let (left_surface, left_pos) = left_under.surface.unwrap();
    assert_eq!(center_surface, source_surface);
    assert_eq!(left_surface, source_surface);
    assert_eq!(center_pos, Point::from((pos.x as f64 - 20., pos.y as f64)));
    assert_eq!(left_pos, center_pos);
}

#[test]
fn focus_follows_mouse_switches_from_zoomed_mirror_to_source_window() {
    let mut config = Config::default();
    config.input.focus_follows_mouse = Some(FocusFollowsMouse {
        max_scroll_amount: None,
    });

    let Some(mut f) = set_up(config) else {
        return;
    };
    let id = f.add_client();
    create_window(&mut f, id, "source", (40, 20), GREEN);

    let source_id = f
        .niri()
        .layout
        .windows()
        .find(|(_, mapped)| !mapped.is_mirror())
        .map(|(_, mapped)| mapped.id())
        .unwrap();
    let mirror_id = create_window_mirror(&mut f);
    f.niri().layout.toggle_window_floating(Some(&mirror_id));
    f.niri()
        .layout
        .set_window_width(Some(&mirror_id), SizeChange::SetFixed(20));
    f.niri()
        .layout
        .set_window_height(Some(&mirror_id), SizeChange::SetFixed(20));
    f.niri().layout.activate_window(&mirror_id);
    f.niri_state()
        .do_action(Action::SetWindowMirrorZoom("2.0".into()), false);
    f.niri_state().do_action(
        Action::SetWindowMirrorCenterX(PositionChange::SetProportion(75.)),
        false,
    );

    let (source_pos, source_size) = tile_geometry_for(&mut f, source_id);
    let (mirror_pos, mirror_size) = tile_geometry_for(&mut f, mirror_id);
    let (old_point, new_point) = if mirror_pos.x > source_pos.x {
        (
            Point::from((mirror_pos.x as f64 + 1., mirror_pos.y as f64 + 10.)),
            Point::from((
                source_pos.x as f64 + source_size.w as f64 - 1.,
                source_pos.y as f64 + source_size.h as f64 / 2.,
            )),
        )
    } else {
        (
            Point::from((
                mirror_pos.x as f64 + mirror_size.w as f64 - 1.,
                mirror_pos.y as f64 + 10.,
            )),
            Point::from((
                source_pos.x as f64 + 1.,
                source_pos.y as f64 + source_size.h as f64 / 2.,
            )),
        )
    };

    let old_focus = f.niri().contents_under(old_point);
    let new_focus = f.niri().contents_under(new_point);
    assert_eq!(old_focus.window.map(|(id, _)| id), Some(mirror_id));
    assert_eq!(new_focus.window.map(|(id, _)| id), Some(source_id));

    let pointer = f.niri().seat.get_pointer().unwrap();
    pointer.set_location(new_point);
    f.niri().pointer_contents = old_focus.clone();
    f.niri().handle_focus_follows_mouse(&old_focus, &new_focus);

    assert_eq!(
        f.niri().layout.focus().map(|mapped| mapped.id()),
        Some(source_id)
    );
}

#[test]
fn mirror_created_from_mirror_inherits_view_state() {
    let Some(mut f) = set_up(Config::default()) else {
        return;
    };
    let id = f.add_client();
    create_window(&mut f, id, "source", (40, 20), GREEN);

    let mirror1_id = create_window_mirror(&mut f);
    f.niri().layout.toggle_window_floating(Some(&mirror1_id));
    f.niri()
        .layout
        .set_window_width(Some(&mirror1_id), SizeChange::SetFixed(20));
    f.niri()
        .layout
        .set_window_height(Some(&mirror1_id), SizeChange::SetFixed(20));
    f.niri().layout.activate_window(&mirror1_id);
    f.niri_state()
        .do_action(Action::SetWindowMirrorZoom("2.0".into()), false);
    f.niri_state().do_action(
        Action::SetWindowMirrorCenterX(PositionChange::SetProportion(75.)),
        false,
    );
    f.niri_state()
        .do_action(Action::SetWindowMirrorTransform(IpcTransform::_90), false);

    let mirror2_id = create_window_mirror_for(&mut f, mirror1_id);

    let (transform1, transform2) = {
        let mut transforms = f
            .niri()
            .layout
            .windows()
            .filter(|(_, mapped)| mapped.id() == mirror1_id || mapped.id() == mirror2_id)
            .map(|(_, mapped)| (mapped.id(), mapped.mirror_content_transform()))
            .collect::<Vec<_>>();
        transforms.sort_by_key(|(id, _)| id.get());
        (transforms[0].1, transforms[1].1)
    };
    assert_eq!(transform1, transform2);
    let point = Point::from((10., 10.));
    let mirror1_point = f
        .niri()
        .layout
        .windows()
        .find(|(_, mapped)| mapped.id() == mirror1_id)
        .map(|(_, mapped)| mapped.mirror_point_to_source(point))
        .unwrap();
    let mirror2_point = f
        .niri()
        .layout
        .windows()
        .find(|(_, mapped)| mapped.id() == mirror2_id)
        .map(|(_, mapped)| mapped.mirror_point_to_source(point))
        .unwrap();
    assert_eq!(mirror1_point, mirror2_point);
    let ws = f.niri().layout.active_workspace().unwrap();
    assert!(ws.is_floating(&mirror1_id));
    assert!(ws.is_floating(&mirror2_id));
}

#[test]
fn mirror_from_floating_source_starts_floating_and_is_independent() {
    let Some(mut f) = set_up(Config::default()) else {
        return;
    };
    let id = f.add_client();
    create_window(&mut f, id, "source", (40, 20), GREEN);

    let source_id = f.niri().layout.windows().next().unwrap().1.id();
    f.niri().layout.toggle_window_floating(Some(&source_id));

    let mirror_id = create_window_mirror(&mut f);

    let ws = f.niri().layout.active_workspace().unwrap();
    assert!(ws.is_floating(&source_id));
    assert!(ws.is_floating(&mirror_id));

    f.niri().layout.toggle_window_floating(Some(&source_id));

    let ws = f.niri().layout.active_workspace().unwrap();
    assert!(!ws.is_floating(&source_id));
    assert!(ws.is_floating(&mirror_id));
}

#[test]
fn mirror_from_sticky_source_starts_sticky_and_is_independent() {
    let Some(mut f) = set_up(Config::default()) else {
        return;
    };
    let id = f.add_client();
    create_window(&mut f, id, "source", (40, 20), GREEN);

    let source_id = f.niri().layout.windows().next().unwrap().1.id();
    f.niri().layout.toggle_window_sticky(Some(&source_id));

    let mirror_id = create_window_mirror(&mut f);

    assert!(f.niri().layout.is_sticky_window(&source_id));
    assert!(f.niri().layout.is_sticky_window(&mirror_id));

    f.niri().layout.toggle_window_sticky(Some(&source_id));

    assert!(!f.niri().layout.is_sticky_window(&source_id));
    assert!(f.niri().layout.is_sticky_window(&mirror_id));
}

#[test]
fn mirror_link_enable_snaps_to_real_and_routes_floating_changes() {
    let Some(mut f) = set_up(Config::default()) else {
        return;
    };
    let id = f.add_client();
    create_window(&mut f, id, "source", (40, 20), GREEN);

    let source_id = f.niri().layout.windows().next().unwrap().1.id();
    let mirror_id = create_window_mirror(&mut f);

    f.niri().layout.toggle_window_floating(Some(&mirror_id));
    f.niri()
        .layout
        .set_window_width(Some(&mirror_id), SizeChange::SetFixed(80));
    f.niri()
        .layout
        .set_window_height(Some(&mirror_id), SizeChange::SetFixed(60));

    let ws = f.niri().layout.active_workspace().unwrap();
    assert!(!ws.is_floating(&source_id));
    assert!(ws.is_floating(&mirror_id));
    assert!(!is_mirror_linked_by_id(&mut f, mirror_id));

    assert!(f.niri().layout.toggle_window_mirror_link(&mirror_id));
    assert!(is_mirror_linked_by_id(&mut f, mirror_id));

    let ws = f.niri().layout.active_workspace().unwrap();
    assert!(!ws.is_floating(&source_id));
    assert!(!ws.is_floating(&mirror_id));

    f.niri().layout.toggle_window_floating(Some(&mirror_id));
    let ws = f.niri().layout.active_workspace().unwrap();
    assert!(ws.is_floating(&source_id));
    assert!(ws.is_floating(&mirror_id));

    f.niri().layout.toggle_window_floating(Some(&mirror_id));
    let ws = f.niri().layout.active_workspace().unwrap();
    assert!(!ws.is_floating(&source_id));
    assert!(!ws.is_floating(&mirror_id));
}

#[test]
fn mirror_link_resizes_with_real_and_linked_siblings() {
    let Some(mut f) = set_up(Config::default()) else {
        return;
    };
    let id = f.add_client();
    create_window(&mut f, id, "source", (40, 20), GREEN);

    let source_id = f.niri().layout.windows().next().unwrap().1.id();
    f.niri().layout.toggle_window_floating(Some(&source_id));

    let mirror1_id = create_window_mirror_for(&mut f, source_id);
    let mirror2_id = create_window_mirror_for(&mut f, source_id);
    assert!(f.niri().layout.toggle_window_mirror_link(&mirror1_id));
    assert!(f.niri().layout.toggle_window_mirror_link(&mirror2_id));

    f.niri()
        .layout
        .set_window_width(Some(&source_id), SizeChange::SetFixed(80));
    f.niri()
        .layout
        .set_window_height(Some(&source_id), SizeChange::SetFixed(60));

    assert_eq!(
        mirror_mapped_by_id(&mut f, source_id).1,
        Size::from((80, 60))
    );
    assert_eq!(
        mirror_mapped_by_id(&mut f, mirror1_id).1,
        Size::from((80, 60))
    );
    assert_eq!(
        mirror_mapped_by_id(&mut f, mirror2_id).1,
        Size::from((80, 60))
    );

    f.niri()
        .layout
        .set_window_width(Some(&mirror1_id), SizeChange::SetFixed(20));
    f.niri()
        .layout
        .set_window_height(Some(&mirror1_id), SizeChange::SetFixed(30));

    assert_eq!(
        mirror_mapped_by_id(&mut f, source_id).1,
        Size::from((20, 30))
    );
    assert_eq!(
        mirror_mapped_by_id(&mut f, mirror1_id).1,
        Size::from((20, 30))
    );
    assert_eq!(
        mirror_mapped_by_id(&mut f, mirror2_id).1,
        Size::from((20, 30))
    );
}

#[test]
fn unlinked_mirror_inherits_tiled_height_from_source() {
    let Some(mut f) = set_up(Config::default()) else {
        return;
    };
    let id = f.add_client();
    create_window(&mut f, id, "source", (40, 20), GREEN);

    let source_id = f.niri().layout.windows().next().unwrap().1.id();
    f.niri()
        .layout
        .set_window_height(Some(&source_id), SizeChange::SetFixed(48));

    let source_height = mirror_mapped_by_id(&mut f, source_id).1.h;
    assert_eq!(source_height, 48);

    let mirror_id = create_window_mirror_for(&mut f, source_id);
    assert!(!is_mirror_linked_by_id(&mut f, mirror_id));

    assert_eq!(mirror_mapped_by_id(&mut f, mirror_id).1.h, source_height);
}

#[test]
fn mirror_linked_group_does_not_mutate_unlinked_siblings() {
    let Some(mut f) = set_up(Config::default()) else {
        return;
    };
    let id = f.add_client();
    create_window(&mut f, id, "source", (40, 20), GREEN);

    let source_id = f.niri().layout.windows().next().unwrap().1.id();
    f.niri().layout.toggle_window_floating(Some(&source_id));

    let linked_mirror_id = create_window_mirror_for(&mut f, source_id);
    let unlinked_mirror_id = create_window_mirror_for(&mut f, source_id);
    assert!(f.niri().layout.toggle_window_mirror_link(&linked_mirror_id));
    assert!(!is_mirror_linked_by_id(&mut f, unlinked_mirror_id));

    f.niri()
        .layout
        .set_window_width(Some(&source_id), SizeChange::SetFixed(72));
    f.niri()
        .layout
        .set_window_height(Some(&source_id), SizeChange::SetFixed(48));

    assert_eq!(
        mirror_mapped_by_id(&mut f, source_id).1,
        Size::from((72, 48))
    );
    assert_eq!(
        mirror_mapped_by_id(&mut f, linked_mirror_id).1,
        Size::from((72, 48))
    );
    assert_eq!(
        mirror_mapped_by_id(&mut f, unlinked_mirror_id).1,
        Size::from((40, 20))
    );
}

#[test]
fn mirror_linked_group_tracks_commit_driven_source_resize() {
    let Some(mut f) = set_up(Config::default()) else {
        return;
    };
    let id = f.add_client();
    let surface = create_window(&mut f, id, "source", (40, 20), GREEN);

    let source_id = f.niri().layout.windows().next().unwrap().1.id();
    f.niri().layout.toggle_window_floating(Some(&source_id));

    let mirror_id = create_window_mirror_for(&mut f, source_id);
    assert!(f.niri().layout.toggle_window_mirror_link(&mirror_id));

    let window = f.client(id).window(&surface);
    window.attach_rgba_buffer(GREEN);
    window.set_size(72, 48);
    window.ack_last_and_commit();
    f.double_roundtrip(id);

    assert_eq!(
        mirror_mapped_by_id(&mut f, source_id).1,
        Size::from((72, 48))
    );
    assert_eq!(
        mirror_mapped_by_id(&mut f, mirror_id).1,
        Size::from((72, 48))
    );
}

#[test]
fn mirror_linked_group_does_not_sync_transform() {
    let Some(mut f) = set_up(Config::default()) else {
        return;
    };
    let id = f.add_client();
    create_window(&mut f, id, "source", (40, 20), GREEN);

    let source_id = f.niri().layout.windows().next().unwrap().1.id();
    let mirror1_id = create_window_mirror_for(&mut f, source_id);
    let mirror2_id = create_window_mirror_for(&mut f, source_id);
    assert!(f.niri().layout.toggle_window_mirror_link(&mirror1_id));
    assert!(f.niri().layout.toggle_window_mirror_link(&mirror2_id));

    f.niri().layout.activate_window(&mirror1_id);
    f.niri_state()
        .do_action(Action::SetWindowMirrorTransform(IpcTransform::_90), false);

    let mirror1_transform = f
        .niri()
        .layout
        .windows()
        .find(|(_, mapped)| mapped.id() == mirror1_id)
        .map(|(_, mapped)| mapped.mirror_view_transform())
        .unwrap();
    let mirror2_transform = f
        .niri()
        .layout
        .windows()
        .find(|(_, mapped)| mapped.id() == mirror2_id)
        .map(|(_, mapped)| mapped.mirror_view_transform())
        .unwrap();
    assert_eq!(mirror1_transform, Transform::_90);
    assert_eq!(mirror2_transform, Transform::Normal);
}

#[test]
fn mirror_view_tracks_source_resize_proportionally() {
    let Some(mut f) = set_up(Config::default()) else {
        return;
    };
    let id = f.add_client();
    let source = create_window(&mut f, id, "source", (40, 20), GREEN);

    let mirror_id = create_window_mirror(&mut f);
    f.niri().layout.toggle_window_floating(Some(&mirror_id));
    f.niri()
        .layout
        .set_window_width(Some(&mirror_id), SizeChange::SetFixed(20));
    f.niri()
        .layout
        .set_window_height(Some(&mirror_id), SizeChange::SetFixed(20));
    f.niri().layout.activate_window(&mirror_id);
    f.niri_state()
        .do_action(Action::SetWindowMirrorZoom("2.0".into()), false);
    f.niri_state().do_action(
        Action::SetWindowMirrorCenterX(PositionChange::SetProportion(75.)),
        false,
    );

    let window = f.client(id).window(&source);
    window.attach_rgba_buffer(GREEN);
    window.set_size(80, 40);
    window.ack_last_and_commit();
    f.double_roundtrip(id);

    let mapped = f
        .niri()
        .layout
        .windows()
        .find(|(_, mapped)| mapped.id() == mirror_id)
        .map(|(_, mapped)| mapped)
        .unwrap();
    assert_eq!(
        mapped.mirror_point_to_source(Point::from((10., 10.))),
        Some(Point::from((60., 20.))),
    );
}

#[test]
fn mirror_view_anchor_preserves_source_point_when_panned() {
    let Some(mut f) = set_up(Config::default()) else {
        return;
    };
    let id = f.add_client();
    create_window(&mut f, id, "source", (40, 20), GREEN);

    let mirror_id = create_window_mirror(&mut f);
    f.niri().layout.toggle_window_floating(Some(&mirror_id));
    f.niri()
        .layout
        .set_window_width(Some(&mirror_id), SizeChange::SetFixed(20));
    f.niri()
        .layout
        .set_window_height(Some(&mirror_id), SizeChange::SetFixed(20));

    let source_point = Point::from((30., 10.));
    f.niri().layout.with_windows_mut(|mapped, _| {
        if mapped.id() == mirror_id {
            mapped.set_mirror_view_from_anchor(2., Point::from((10., 10.)), source_point);
            mapped.set_mirror_view_from_anchor(2., Point::from((6., 10.)), source_point);
        }
    });

    let mapped = f
        .niri()
        .layout
        .windows()
        .find(|(_, mapped)| mapped.id() == mirror_id)
        .map(|(_, mapped)| mapped)
        .unwrap();
    assert_eq!(
        mapped.mirror_point_to_source(Point::from((6., 10.))),
        Some(source_point),
    );
}

#[test]
fn mirror_view_anchor_preserves_source_point_when_zoomed() {
    let Some(mut f) = set_up(Config::default()) else {
        return;
    };
    let id = f.add_client();
    create_window(&mut f, id, "source", (40, 20), GREEN);

    let mirror_id = create_window_mirror(&mut f);
    f.niri().layout.toggle_window_floating(Some(&mirror_id));
    f.niri()
        .layout
        .set_window_width(Some(&mirror_id), SizeChange::SetFixed(20));
    f.niri()
        .layout
        .set_window_height(Some(&mirror_id), SizeChange::SetFixed(20));

    let anchor = Point::from((15., 10.));
    let source_point = Point::from((30., 10.));
    f.niri().layout.with_windows_mut(|mapped, _| {
        if mapped.id() == mirror_id {
            mapped.set_mirror_view_from_anchor(2., anchor, source_point);
        }
    });

    let mapped = f
        .niri()
        .layout
        .windows()
        .find(|(_, mapped)| mapped.id() == mirror_id)
        .map(|(_, mapped)| mapped)
        .unwrap();
    assert_eq!(mapped.mirror_zoom(), 2.);
    assert_eq!(mapped.mirror_point_to_source(anchor), Some(source_point));
}

#[test]
fn mirror_window_cast_bbox_matches_mirror_viewport() {
    let Some(mut f) = set_up(Config::default()) else {
        return;
    };
    let id = f.add_client();
    create_window(&mut f, id, "source", (40, 20), GREEN);

    let mirror_id = create_window_mirror(&mut f);
    f.niri().layout.toggle_window_floating(Some(&mirror_id));
    f.niri()
        .layout
        .set_window_width(Some(&mirror_id), SizeChange::SetFixed(20));
    f.niri()
        .layout
        .set_window_height(Some(&mirror_id), SizeChange::SetFixed(10));

    let output = f.niri_output(1);
    let scale = Scale::from(output.current_scale().fractional_scale());
    let mapped = f
        .niri()
        .layout
        .windows()
        .find(|(_, mapped)| mapped.id() == mirror_id)
        .map(|(_, mapped)| mapped)
        .unwrap();

    assert_eq!(mapped.window_cast_bbox(scale).size, Size::from((20, 10)));
}

#[test]
fn rotated_mirror_window_cast_letterboxes_sideways() {
    let Some(mut f) = set_up(Config::default()) else {
        return;
    };
    let id = f.add_client();
    create_window(&mut f, id, "source", (40, 20), GREEN);

    let mirror_id = create_window_mirror(&mut f);
    f.niri().layout.toggle_window_floating(Some(&mirror_id));
    f.niri()
        .layout
        .set_window_width(Some(&mirror_id), SizeChange::SetFixed(40));
    f.niri()
        .layout
        .set_window_height(Some(&mirror_id), SizeChange::SetFixed(20));
    f.niri().layout.activate_window(&mirror_id);
    f.niri_state()
        .do_action(Action::SetWindowMirrorTransform(IpcTransform::_90), false);

    let output = f.niri_output(1);
    let (size, pixels) = render_window_cast_pixels_for(&mut f, &output, mirror_id);

    assert_eq!(size, Size::from((40, 20)));
    assert_eq!(
        sample_pixel(size, &pixels, size.w / 2, size.h / 2),
        [0, 255, 0, 255]
    );
    assert_eq!(sample_pixel(size, &pixels, 5, size.h / 2), [0, 0, 0, 0]);
    assert_eq!(
        sample_pixel(size, &pixels, size.w - 6, size.h / 2),
        [0, 0, 0, 0]
    );
}

#[test]
fn rotated_zoomed_panned_mirror_window_cast_stays_clipped_to_viewport() {
    let Some(mut f) = set_up(Config::default()) else {
        return;
    };
    let id = f.add_client();
    create_window(&mut f, id, "source", (40, 20), GREEN);

    let mirror_id = create_window_mirror(&mut f);
    f.niri().layout.toggle_window_floating(Some(&mirror_id));
    f.niri()
        .layout
        .set_window_width(Some(&mirror_id), SizeChange::SetFixed(20));
    f.niri()
        .layout
        .set_window_height(Some(&mirror_id), SizeChange::SetFixed(20));
    f.niri().layout.activate_window(&mirror_id);
    f.niri_state()
        .do_action(Action::SetWindowMirrorTransform(IpcTransform::_90), false);
    f.niri_state()
        .do_action(Action::SetWindowMirrorZoom("2.0".into()), false);
    f.niri_state()
        .do_action(Action::MoveWindowMirrorViewDown, false);

    let output = f.niri_output(1);
    let (size, pixels) = render_window_cast_pixels_for(&mut f, &output, mirror_id);

    assert_eq!(size, Size::from((20, 20)));
    assert_eq!(sample_pixel(size, &pixels, 0, 0), [0, 255, 0, 255]);
    assert_eq!(sample_pixel(size, &pixels, 19, 0), [0, 255, 0, 255]);
    assert_eq!(sample_pixel(size, &pixels, 0, 19), [0, 255, 0, 255]);
    assert_eq!(sample_pixel(size, &pixels, 19, 19), [0, 255, 0, 255]);
    assert_eq!(sample_pixel(size, &pixels, 10, 10), [0, 255, 0, 255]);
}

#[test]
fn cropped_rotated_zoomed_panned_mirror_keeps_full_viewport() {
    let Some(mut f) = set_up(Config::default()) else {
        return;
    };
    let id = f.add_client();
    create_window(&mut f, id, "source", (40, 20), GREEN);

    let mirror_id = create_window_mirror(&mut f);
    f.niri().layout.toggle_window_floating(Some(&mirror_id));
    f.niri()
        .layout
        .set_window_width(Some(&mirror_id), SizeChange::SetFixed(20));
    f.niri()
        .layout
        .set_window_height(Some(&mirror_id), SizeChange::SetFixed(20));
    f.niri().layout.activate_window(&mirror_id);
    f.niri_state()
        .do_action(Action::SetWindowMirrorTransform(IpcTransform::_180), false);
    f.niri_state()
        .do_action(Action::SetWindowMirrorZoom("2.0".into()), false);
    f.niri_state()
        .do_action(Action::MoveWindowMirrorViewDown, false);

    let output = f.niri_output(1);
    let (size, pixels) = render_cropped_window_cast_pixels_for(&mut f, &output, mirror_id);

    assert_eq!(size, Size::from((20, 20)));
    assert_eq!(sample_pixel(size, &pixels, 0, 0), [0, 255, 0, 255]);
    assert_eq!(sample_pixel(size, &pixels, 19, 0), [0, 255, 0, 255]);
    assert_eq!(sample_pixel(size, &pixels, 0, 19), [0, 255, 0, 255]);
    assert_eq!(sample_pixel(size, &pixels, 19, 19), [0, 255, 0, 255]);
    assert_eq!(sample_pixel(size, &pixels, 10, 10), [0, 255, 0, 255]);
}

#[test]
fn floating_mirror_keeps_viewport_corner_radius_when_downscaled() {
    let mut config = Config::default();
    config.window_rules.push(WindowRule {
        matches: vec![WindowMatch {
            title: Some(RegexEq::from_str("^source$").unwrap()),
            is_floating: Some(true),
            ..Default::default()
        }],
        geometry_corner_radius: Some(CornerRadius::from(20.)),
        ..Default::default()
    });
    config.window_rules.push(WindowRule {
        matches: vec![WindowMatch {
            title: Some(RegexEq::from_str("^source$").unwrap()),
            is_floating: Some(false),
            ..Default::default()
        }],
        geometry_corner_radius: Some(CornerRadius::default()),
        ..Default::default()
    });

    let Some(mut f) = set_up(config) else {
        return;
    };
    let id = f.add_client();
    create_window(&mut f, id, "source", (80, 80), GREEN);

    let mirror_id = create_window_mirror(&mut f);
    f.niri().layout.toggle_window_floating(Some(&mirror_id));
    f.niri()
        .layout
        .set_window_width(Some(&mirror_id), SizeChange::SetFixed(40));
    f.niri()
        .layout
        .set_window_height(Some(&mirror_id), SizeChange::SetFixed(40));

    let output = f.niri_output(1);
    let (size, pixels) = render_window_output_pixels_for(&mut f, &output, mirror_id);

    assert_eq!(size, Size::from((40, 40)));
    assert_eq!(sample_pixel(size, &pixels, 8, 2), [0, 0, 0, 0]);
    assert_eq!(sample_pixel(size, &pixels, 2, 8), [0, 0, 0, 0]);
    assert_eq!(sample_pixel(size, &pixels, 20, 20), [0, 255, 0, 255]);
}

#[test]
fn oversized_mirror_with_window_geometry_keeps_padding_transparent() {
    let Some(mut f) = set_up(Config::default()) else {
        return;
    };
    let id = f.add_client();
    create_window_with_geometry(&mut f, id, "source", (100, 60), (2, 2, 96, 56), GREEN);

    let mirror_id = create_window_mirror(&mut f);
    f.niri().layout.toggle_window_floating(Some(&mirror_id));
    f.niri()
        .layout
        .set_window_width(Some(&mirror_id), SizeChange::SetFixed(120));
    f.niri()
        .layout
        .set_window_height(Some(&mirror_id), SizeChange::SetFixed(80));

    let output = f.niri_output(1);
    let (size, pixels) = render_window_output_pixels_for(&mut f, &output, mirror_id);

    assert_eq!(size, Size::from((120, 80)));
    assert_eq!(sample_pixel(size, &pixels, size.w / 2, 4), [0, 0, 0, 0]);
    assert_eq!(sample_pixel(size, &pixels, size.w / 2, 5), [0, 255, 0, 255]);
    assert_eq!(
        sample_pixel(size, &pixels, size.w / 2, size.h / 2),
        [0, 255, 0, 255]
    );
    assert_eq!(sample_pixel(size, &pixels, 2, size.h / 2), [0, 255, 0, 255]);
    assert_eq!(
        sample_pixel(size, &pixels, size.w - 3, size.h / 2),
        [0, 255, 0, 255]
    );
    assert_eq!(sample_pixel(size, &pixels, size.w / 2, 75), [0, 0, 0, 0]);
}

#[test]
fn oversized_focused_mirror_keeps_padding_transparent_with_border_background() {
    let mut config = Config::default();
    config.layout.border.off = false;
    config.window_rules.push(WindowRule {
        matches: vec![WindowMatch {
            title: Some(RegexEq::from_str("^source$").unwrap()),
            ..Default::default()
        }],
        draw_border_with_background: Some(true),
        ..Default::default()
    });

    let mut f = Fixture::with_config(config);
    if f.niri_state().backend.headless().add_renderer().is_err() {
        eprintln!("skipping capture block-out test: headless EGL renderer unavailable");
        return;
    }
    f.add_output(1, (200, 150));
    let id = f.add_client();
    create_window_with_geometry(&mut f, id, "source", (100, 60), (2, 2, 96, 56), GREEN);

    let mirror_id = create_window_mirror(&mut f);
    f.niri().layout.activate_window(&mirror_id);
    f.niri().layout.toggle_window_floating(Some(&mirror_id));
    f.niri()
        .layout
        .set_window_width(Some(&mirror_id), SizeChange::SetFixed(120));
    f.niri()
        .layout
        .set_window_height(Some(&mirror_id), SizeChange::SetFixed(80));

    let output = f.niri_output(1);
    let (size, pixels) = render_output_pixels(&mut f, &output, RenderTarget::Output);
    let (window_render_loc, content_rect) = {
        let ws = f.niri().layout.active_workspace().unwrap();
        let (tile, tile_pos, visible) = ws
            .tiles_with_render_positions()
            .find(|(tile, _, _)| tile.window().id() == mirror_id)
            .unwrap();
        assert!(visible);

        let window_render_loc = (tile_pos + tile.window_loc()).to_i32_round();
        let mapped = f
            .niri()
            .layout
            .windows()
            .find(|(_, mapped)| mapped.id() == mirror_id)
            .map(|(_, mapped)| mapped)
            .unwrap();
        let (content_loc, content_scale) = mapped.mirror_content_transform();
        let source_geometry = mapped.window.geometry().to_f64();
        let content_rect = smithay::utils::Rectangle::new(
            window_render_loc + content_loc.to_i32_round(),
            source_geometry.size.upscale(content_scale).to_i32_round(),
        );

        (window_render_loc, content_rect)
    };

    let center_x = window_render_loc.x + 120 / 2;
    assert_eq!(
        sample_pixel(size, &pixels, center_x, content_rect.loc.y - 1),
        [0, 0, 0, 0]
    );
    assert_eq!(
        sample_pixel(size, &pixels, center_x, content_rect.loc.y),
        [0, 255, 0, 255]
    );
    assert_eq!(
        sample_pixel(
            size,
            &pixels,
            center_x,
            content_rect.loc.y + content_rect.size.h - 1
        ),
        [0, 255, 0, 255]
    );
    assert_eq!(
        sample_pixel(
            size,
            &pixels,
            center_x,
            content_rect.loc.y + content_rect.size.h
        ),
        [0, 0, 0, 0]
    );
}

#[test]
fn mirror_windows_resize_independently_from_each_other() {
    let Some(mut f) = set_up(Config::default()) else {
        return;
    };
    let id = f.add_client();
    create_window(&mut f, id, "source", (40, 20), GREEN);

    let source_id = f.niri().layout.windows().next().unwrap().1.id();
    let mirror1_id = create_window_mirror_for(&mut f, source_id);
    let mirror2_id = create_window_mirror_for(&mut f, source_id);

    f.niri().layout.toggle_window_floating(Some(&mirror1_id));
    f.niri()
        .layout
        .set_window_width(Some(&mirror1_id), SizeChange::SetFixed(80));
    f.niri()
        .layout
        .set_window_height(Some(&mirror1_id), SizeChange::SetFixed(80));

    f.niri().layout.toggle_window_floating(Some(&mirror2_id));
    f.niri()
        .layout
        .set_window_width(Some(&mirror2_id), SizeChange::SetFixed(20));
    f.niri()
        .layout
        .set_window_height(Some(&mirror2_id), SizeChange::SetFixed(60));

    let output = f.niri_output(1);
    let (size1, pixels1) = render_window_cast_pixels_for(&mut f, &output, mirror1_id);
    let (size2, pixels2) = render_window_cast_pixels_for(&mut f, &output, mirror2_id);

    assert_eq!(size1, Size::from((80, 80)));
    assert_eq!(
        sample_pixel(size1, &pixels1, size1.w / 2, size1.h / 2),
        [0, 255, 0, 255]
    );
    assert_eq!(sample_pixel(size1, &pixels1, size1.w / 2, 10), [0, 0, 0, 0]);
    assert_eq!(
        sample_pixel(size1, &pixels1, size1.w / 2, size1.h - 11),
        [0, 0, 0, 0]
    );

    assert_eq!(size2, Size::from((20, 60)));
    assert_eq!(
        sample_pixel(size2, &pixels2, size2.w / 2, size2.h / 2),
        [0, 255, 0, 255]
    );
    assert_eq!(sample_pixel(size2, &pixels2, size2.w / 2, 15), [0, 0, 0, 0]);
    assert_eq!(
        sample_pixel(size2, &pixels2, size2.w / 2, size2.h - 16),
        [0, 0, 0, 0]
    );
}

#[test]
fn mirror_link_move_to_output_stays_local() {
    let Some(mut f) = set_up(Config::default()) else {
        return;
    };
    f.add_output(2, (120, 90));

    let id = f.add_client();
    create_window(&mut f, id, "source", (40, 20), GREEN);

    let source_id = f.niri().layout.windows().next().unwrap().1.id();
    let mirror_id = create_window_mirror_for(&mut f, source_id);
    assert!(f.niri().layout.toggle_window_mirror_link(&mirror_id));

    let source_output_before = window_output_name_by_id(&mut f, source_id).unwrap();
    assert_eq!(
        window_output_name_by_id(&mut f, mirror_id).as_deref(),
        Some(source_output_before.as_str())
    );

    let output2 = f.niri_output(2);
    f.niri()
        .layout
        .move_to_output(Some(&mirror_id), &output2, None, ActivateWindow::No);

    assert_eq!(
        window_output_name_by_id(&mut f, source_id).as_deref(),
        Some(source_output_before.as_str())
    );
    assert_ne!(
        window_output_name_by_id(&mut f, mirror_id).as_deref(),
        Some(source_output_before.as_str())
    );
}

#[test]
fn mirror_animation_snapshot_scales_contents_into_mirror() {
    let Some(mut f) = set_up(Config::default()) else {
        return;
    };
    let id = f.add_client();
    create_window(&mut f, id, "source", (40, 20), GREEN);

    let mirror_id = create_window_mirror(&mut f);
    f.niri().layout.toggle_window_floating(Some(&mirror_id));
    f.niri()
        .layout
        .set_window_width(Some(&mirror_id), SizeChange::SetFixed(80));
    f.niri()
        .layout
        .set_window_height(Some(&mirror_id), SizeChange::SetFixed(80));

    let output = f.niri_output(1);
    let state = f.niri_state();
    let (backend, niri) = (&mut state.backend, &mut state.niri);
    backend
        .with_primary_renderer(|renderer| {
            let mapped = niri
                .layout
                .windows_for_output_mut(&output)
                .find(|mapped| mapped.id() == mirror_id)
                .unwrap();

            mapped.store_animation_snapshot(renderer);
            let snapshot = mapped.take_animation_snapshot().unwrap();

            assert_eq!(snapshot.size, Size::from((80., 80.)));
            assert_eq!(snapshot.contents.len(), 1);
            assert_eq!(snapshot.contents[0].location, Point::from((0., 20.)));
            assert_eq!(snapshot.contents[0].dst, Some(Size::from((80, 40))));
        })
        .unwrap();
}

#[test]
fn mirror_animation_snapshot_uses_window_geometry_not_buffer_extents() {
    let Some(mut f) = set_up(Config::default()) else {
        return;
    };
    let id = f.add_client();
    create_window_with_geometry(&mut f, id, "source", (100, 60), (2, 2, 96, 56), GREEN);

    let mirror_id = create_window_mirror(&mut f);
    let output = f.niri_output(1);
    let state = f.niri_state();
    let (backend, niri) = (&mut state.backend, &mut state.niri);
    backend
        .with_primary_renderer(|renderer| {
            let mapped = niri
                .layout
                .windows_for_output_mut(&output)
                .find(|mapped| mapped.id() == mirror_id)
                .unwrap();

            mapped.store_animation_snapshot(renderer);
            let snapshot = mapped.take_animation_snapshot().unwrap();

            assert_eq!(snapshot.size, Size::from((96., 56.)));
            assert_eq!(snapshot.contents.len(), 1);
            assert_eq!(snapshot.contents[0].location, Point::from((0., 0.)));
            assert_eq!(snapshot.contents[0].dst, Some(Size::from((96, 56))));
        })
        .unwrap();
}

#[test]
fn mirror_animation_snapshot_preserves_rotation() {
    let Some(mut f) = set_up(Config::default()) else {
        return;
    };
    let id = f.add_client();
    create_window(&mut f, id, "source", (40, 20), GREEN);

    let mirror_id = create_window_mirror(&mut f);
    f.niri().layout.toggle_window_floating(Some(&mirror_id));
    f.niri()
        .layout
        .set_window_width(Some(&mirror_id), SizeChange::SetFixed(40));
    f.niri()
        .layout
        .set_window_height(Some(&mirror_id), SizeChange::SetFixed(20));
    f.niri().layout.activate_window(&mirror_id);
    f.niri_state()
        .do_action(Action::SetWindowMirrorTransform(IpcTransform::_90), false);

    let output = f.niri_output(1);
    let (live_size, live_pixels) = render_window_cast_pixels_for(&mut f, &output, mirror_id);

    let state = f.niri_state();
    let (backend, niri) = (&mut state.backend, &mut state.niri);
    let (snapshot_size, snapshot_pixels) = backend
        .with_primary_renderer(|renderer| {
            let mapped = niri
                .layout
                .windows_for_output_mut(&output)
                .find(|mapped| mapped.id() == mirror_id)
                .unwrap();

            mapped.store_animation_snapshot(renderer);
            let snapshot = mapped.take_animation_snapshot().unwrap();
            let scale = Scale::from(output.current_scale().fractional_scale());
            render_snapshot_texture_pixels(renderer, &snapshot, scale)
        })
        .unwrap();

    assert_eq!(snapshot_size, live_size);
    assert_eq!(snapshot_pixels, live_pixels);
}

#[test]
fn mirror_source_unmap_removes_all_instances() {
    let Some(mut f) = set_up(Config::default()) else {
        return;
    };
    f.add_output(2, (120, 90));

    let id = f.add_client();
    let surface = create_window(&mut f, id, "source", (40, 20), RED);

    let source_id = f.niri().layout.windows().next().unwrap().1.id();
    let mirror1_id = create_window_mirror_for(&mut f, source_id);
    let mirror2_id = create_window_mirror_for(&mut f, source_id);
    assert_ne!(mirror1_id, mirror2_id);

    let output2 = f.niri_output(2);
    f.niri()
        .layout
        .move_to_output(Some(&mirror2_id), &output2, None, ActivateWindow::No);

    assert_eq!(f.niri().layout.windows().count(), 3);
    assert_eq!(
        f.niri()
            .layout
            .windows()
            .filter(|(_, mapped)| mapped.source_id() == source_id)
            .count(),
        3
    );

    let window = f.client(id).window(&surface);
    window.attach_null();
    window.commit();
    f.double_roundtrip(id);

    assert_eq!(f.niri().layout.windows().count(), 0);
    assert!(f.niri().layout.focus().is_none());
    assert!(f.niri().mapped_ids_for_source(source_id).is_empty());
}

#[test]
fn mirror_focus_tracks_layout_entry_not_shared_surface() {
    let Some(mut f) = set_up(Config::default()) else {
        return;
    };
    let id = f.add_client();
    create_window(&mut f, id, "source", (40, 20), GREEN);

    let source_id = f.niri().layout.windows().next().unwrap().1.id();
    let mirror_id = create_window_mirror(&mut f);

    f.niri().layout.activate_window(&mirror_id);
    f.niri_state().update_keyboard_focus();
    assert_eq!(
        f.niri_state().niri.keyboard_focus.layout_id(),
        Some(mirror_id)
    );
    assert_eq!(focused_window_ids(&mut f), vec![mirror_id.get()]);

    f.niri().layout.activate_window(&source_id);
    f.niri_state().update_keyboard_focus();
    assert_eq!(
        f.niri_state().niri.keyboard_focus.layout_id(),
        Some(source_id)
    );
    assert_eq!(focused_window_ids(&mut f), vec![source_id.get()]);
}

#[test]
fn hidden_mirror_does_not_clear_visible_source_primary_scanout() {
    let Some(mut f) = set_up(Config::default()) else {
        return;
    };
    let id = f.add_client();
    let _surface = create_window(&mut f, id, "source", (40, 20), GREEN);

    let source_id = f.niri().layout.windows().next().unwrap().1.id();
    let mirror_id = create_window_mirror(&mut f);

    f.niri().layout.activate_window(&mirror_id);
    f.niri().layout.move_to_workspace_down(true);
    f.niri().layout.activate_window(&source_id);

    let server_surface = f
        .niri()
        .layout
        .windows()
        .find(|(_, mapped)| mapped.id() == source_id)
        .map(|(_, mapped)| mapped.toplevel().wl_surface().clone())
        .unwrap();
    let output = f.niri_output(1);
    let states = rendered_element_states([Id::from_wayland_resource(&server_surface)]);
    f.niri().update_primary_scanout_output(&output, &states);

    let primary = with_states(&server_surface, |states| {
        surface_primary_scanout_output(&server_surface, states)
    });
    assert_eq!(primary.as_ref(), Some(&output));
}

#[test]
fn hidden_source_does_not_clear_visible_mirror_primary_scanout() {
    let Some(mut f) = set_up(Config::default()) else {
        return;
    };
    let id = f.add_client();
    let _surface = create_window(&mut f, id, "source", (40, 20), GREEN);

    let mirror_id = create_window_mirror(&mut f);
    f.niri().layout.activate_window(&mirror_id);
    f.niri().layout.move_to_workspace_down(true);

    let server_surface = f
        .niri()
        .layout
        .windows()
        .find(|(_, mapped)| mapped.id() == mirror_id)
        .map(|(_, mapped)| mapped.toplevel().wl_surface().clone())
        .unwrap();
    let mirror_namespace = f
        .niri()
        .layout
        .windows()
        .find(|(_, mapped)| mapped.id() == mirror_id)
        .map(|(_, mapped)| mapped.element_namespace().unwrap())
        .unwrap();

    let output = f.niri_output(1);
    let states = rendered_element_states([
        Id::from_wayland_resource(&server_surface).namespaced(mirror_namespace)
    ]);
    f.niri().update_primary_scanout_output(&output, &states);

    let primary = with_states(&server_surface, |states| {
        surface_primary_scanout_output(&server_surface, states)
    });
    assert_eq!(primary.as_ref(), Some(&output));
}

#[test]
fn mirror_from_fullscreen_source_inherits_fullscreen_state() {
    let mut config = Config::default();
    config.layout.gaps = 0.;
    let Some(mut f) = set_up(config) else {
        return;
    };
    let id = f.add_client();
    create_window(&mut f, id, "source", (40, 20), RED);

    let source_id = f.niri().layout.windows().next().unwrap().1.id();
    f.niri().layout.set_fullscreen(&source_id, true);

    let mirror_id = create_window_mirror(&mut f);

    let (source_mode, _) = mirror_mapped_by_id(&mut f, source_id);
    let (mirror_mode, mirror_size) = mirror_mapped_by_id(&mut f, mirror_id);
    assert!(source_mode.is_fullscreen());
    assert!(mirror_mode.is_fullscreen());
    assert_eq!(mirror_size, Size::from((100, 100)));

    f.niri().layout.toggle_fullscreen(&mirror_id);
    let (mirror_mode, _) = mirror_mapped_by_id(&mut f, mirror_id);
    assert!(mirror_mode.is_normal());
}

#[test]
fn hidden_fullscreen_born_mirror_uses_default_floating_placement() {
    let mut config = Config::default();
    config.layout.gaps = 0.;
    let Some(mut f) = set_up(config) else {
        return;
    };
    let id = f.add_client();
    create_window(&mut f, id, "source", (40, 20), RED);

    let source_id = f.niri().layout.windows().next().unwrap().1.id();
    f.niri().layout.set_fullscreen(&source_id, true);

    let mirror_id = create_window_mirror(&mut f);

    let ws = f.niri().layout.active_workspace().unwrap();
    let (_, _, visible) = ws
        .tiles_with_render_positions()
        .find(|(tile, _, _)| tile.window().id() == mirror_id)
        .unwrap();
    assert!(!visible);

    f.niri().layout.toggle_window_floating(Some(&mirror_id));

    let working_area = f.niri().layout.active_workspace().unwrap().working_area();
    let (pos, size) = tile_geometry_for(&mut f, mirror_id);
    let expected = center_preferring_top_left_in_area(working_area, size.to_f64()).to_i32_round();

    assert_eq!(pos, expected);
}

#[test]
fn mirror_from_fullscreen_source_inherits_floating_restore_size() {
    let mut config = Config::default();
    config.layout.gaps = 0.;
    let Some(mut f) = set_up(config) else {
        return;
    };
    let id = f.add_client();
    create_window(&mut f, id, "source", (40, 20), RED);

    let source_id = f.niri().layout.windows().next().unwrap().1.id();
    f.niri().layout.toggle_window_floating(Some(&source_id));
    f.niri()
        .layout
        .set_window_width(Some(&source_id), SizeChange::SetFixed(60));
    f.niri()
        .layout
        .set_window_height(Some(&source_id), SizeChange::SetFixed(50));
    f.niri().layout.set_fullscreen(&source_id, true);

    let mirror_id = create_window_mirror(&mut f);
    f.niri().layout.toggle_window_floating(Some(&mirror_id));

    let (mirror_mode, mirror_size) = mirror_mapped_by_id(&mut f, mirror_id);
    assert!(mirror_mode.is_normal());
    assert_eq!(mirror_size, Size::from((60, 50)));
}

#[test]
fn mirror_from_maximized_source_inherits_maximized_state() {
    let Some(mut f) = set_up(Config::default()) else {
        return;
    };
    let id = f.add_client();
    create_window(&mut f, id, "source", (40, 20), RED);

    let source_id = f.niri().layout.windows().next().unwrap().1.id();
    f.niri().layout.set_maximized(&source_id, true);

    let mirror_id = create_window_mirror(&mut f);

    let (source_mode, _) = mirror_mapped_by_id(&mut f, source_id);
    let (mirror_mode, _) = mirror_mapped_by_id(&mut f, mirror_id);
    assert!(source_mode.is_maximized());
    assert!(mirror_mode.is_maximized());

    f.niri().layout.toggle_maximized(&mirror_id);
    let (mirror_mode, _) = mirror_mapped_by_id(&mut f, mirror_id);
    assert!(mirror_mode.is_normal());

    f.niri().layout.toggle_window_floating(Some(&mirror_id));
    f.niri()
        .layout
        .set_window_width(Some(&mirror_id), SizeChange::SetFixed(80));
    f.niri()
        .layout
        .set_window_height(Some(&mirror_id), SizeChange::SetFixed(80));

    let (mirror_mode, mirror_size) = mirror_mapped_by_id(&mut f, mirror_id);
    assert!(mirror_mode.is_normal());
    assert_eq!(mirror_size, Size::from((80, 80)));
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

fn window_is_block_out(f: &mut Fixture, id: MappedId) -> bool {
    f.niri()
        .layout
        .windows()
        .find(|(_, mapped)| mapped.id() == id)
        .map(|(_, mapped)| mapped.effective_block_out_from().is_some())
        .unwrap()
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
fn blocked_mirror_window_cast_is_fully_transparent() {
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

    create_window(&mut f, id, "blocked", (40, 20), GREEN);

    let mirror_id = create_window_mirror(&mut f);
    f.niri().layout.toggle_window_floating(Some(&mirror_id));
    f.niri()
        .layout
        .set_window_width(Some(&mirror_id), SizeChange::SetFixed(80));
    f.niri()
        .layout
        .set_window_height(Some(&mirror_id), SizeChange::SetFixed(80));

    let output = f.niri_output(1);
    let (size, pixels) = render_window_cast_pixels_for(&mut f, &output, mirror_id);

    assert_eq!(size, Size::from((80, 80)));
    assert_eq!(
        sample_pixel(size, &pixels, size.w / 2, size.h / 2),
        [0, 0, 0, 0]
    );
    assert_eq!(sample_pixel(size, &pixels, size.w / 2, 10), [0, 0, 0, 0]);
    assert_eq!(sample_pixel(size, &pixels, 10, size.h / 2), [0, 0, 0, 0]);
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
fn toggle_block_out_window_targets_focused_sticky_window() {
    let Some(mut f) = set_up(Config::default()) else {
        return;
    };
    let id = f.add_client();

    create_window(&mut f, id, "sticky", (40, 30), GREEN);
    let sticky = f.niri().layout.focus().unwrap().id();

    create_window(&mut f, id, "tiling", (40, 30), RED);
    let tiling = f.niri().layout.focus().unwrap().id();

    f.niri().layout.toggle_window_sticky(Some(&sticky));
    f.niri().layout.focus_floating();

    assert_eq!(f.niri().layout.focus().unwrap().id(), sticky);
    assert!(!window_is_block_out(&mut f, sticky));
    assert!(!window_is_block_out(&mut f, tiling));

    f.niri_state()
        .do_action(Action::ToggleBlockOutWindow, false);

    assert!(window_is_block_out(&mut f, sticky));
    assert!(!window_is_block_out(&mut f, tiling));
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
