use std::cell::{Cell, Ref, RefCell};
use std::collections::HashSet;
use std::convert::TryInto;
use std::time::Duration;

use niri_config::{BlockOutFrom, Color, Config, CornerRadius, GradientInterpolation, WindowRule};
use smithay::backend::renderer::element::surface::WaylandSurfaceRenderElement;
use smithay::backend::renderer::element::{Kind, NamespacedElement};
use smithay::backend::renderer::gles::{GlesRenderer, GlesTexture};
use smithay::desktop::space::SpaceElement as _;
use smithay::desktop::{PopupKind, PopupManager, Window};
use smithay::output::{self, Output};
use smithay::reexports::wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1;
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::Resource as _;
use smithay::utils::{Logical, Point, Rectangle, Scale, Serial, Size, Transform};
use smithay::wayland::compositor::{remove_pre_commit_hook, with_states, HookId, SurfaceData};
use smithay::wayland::seat::WaylandFocus;
use smithay::wayland::shell::xdg::{
    SurfaceCachedState, ToplevelCachedState, ToplevelConfigure, ToplevelSurface,
    XdgToplevelSurfaceData,
};
use wayland_backend::server::Credentials;

use super::{ResolvedWindowRules, WindowRef};
use crate::handlers::KdeDecorationsModeState;
use crate::layout::{
    ConfigureIntent, InteractiveResizeData, LayoutElement, LayoutElementRenderElement,
    LayoutElementRenderSnapshot, SizingMode,
};
use crate::niri_render_elements;
use crate::render_helpers::background_effect::BackgroundEffectElement;
use crate::render_helpers::border::BorderRenderElement;
use crate::render_helpers::clipped_surface::ClippedSurfaceRenderElement;
use crate::render_helpers::offscreen::OffscreenData;
use crate::render_helpers::renderer::NiriRenderer;
use crate::render_helpers::scaled_surface::NamespacedScaledWaylandSurfaceRenderElement;
use crate::render_helpers::snapshot::RenderSnapshot;
use crate::render_helpers::solid_color::{SolidColorBuffer, SolidColorRenderElement};
use crate::render_helpers::surface::{
    push_elements_from_surface_tree, render_snapshot_from_surface_tree,
};
use crate::render_helpers::texture::TextureBuffer;
use crate::render_helpers::xray::XrayPos;
use crate::render_helpers::{background_effect, BakedBuffer, RenderCtx, RenderTarget};
use crate::utils::id::IdCounter;
use crate::utils::transaction::Transaction;
use crate::utils::{
    get_credentials_for_surface, send_scale_transform, update_tiled_state,
    with_toplevel_last_uncommitted_configure, with_toplevel_role, with_toplevel_role_and_current,
    ResizeEdge,
};

#[derive(Debug)]
pub struct Mapped {
    pub window: Window,

    /// Unique ID of this `Mapped`.
    id: MappedId,

    /// ID of the real source window for mirrors, or `id` for real windows.
    source_id: MappedId,

    /// Whether this mapped entry is only a mirror of another entry.
    is_mirror: bool,

    /// Current visual size for mirror entries.
    mirror_size: Size<i32, Logical>,

    /// Current sizing mode for mirror entries.
    mirror_sizing_mode: SizingMode,

    /// Pending sizing mode for mirror entries.
    mirror_pending_sizing_mode: SizingMode,

    /// Credentials of the process that created the Wayland connection.
    credentials: Option<Credentials>,

    /// Pre-commit hook that we have on all mapped toplevel surfaces.
    pre_commit_hook: Option<HookId>,

    /// Up-to-date rules.
    rules: ResolvedWindowRules,

    /// Whether the window rules need to be recomputed.
    ///
    /// This is not used in all cases; for example, app ID and title changes recompute the rules
    /// immediately, rather than setting this flag.
    need_to_recompute_rules: bool,

    /// Whether this window needs a configure this loop cycle.
    ///
    /// Certain Wayland requests require a configure in response, like un/fullscreen.
    needs_configure: bool,

    /// Whether this window needs a frame callback.
    ///
    /// We set this after sending a configure to give invisible windows a chance to respond to
    /// resizes immediately, without waiting for a 1 second throttled callback.
    needs_frame_callback: bool,

    /// Data of the offscreen element rendered in place of this window.
    ///
    /// If `None`, then the window is not offscreened.
    offscreen_data: RefCell<Option<OffscreenData>>,

    /// Whether this has an urgent indicator.
    is_urgent: bool,

    /// Whether this window has the keyboard focus.
    is_focused: bool,

    /// Whether this layout entry currently requests xdg_toplevel::Activated.
    is_activated: bool,

    /// Whether this window is the active window in its column.
    is_active_in_column: bool,

    /// Whether this window is floating.
    is_floating: bool,
    /// Whether this window is sticky across all workspaces on its output.
    is_sticky: bool,

    /// Whether this window is a target of a window cast.
    is_window_cast_target: bool,

    /// Whether this window should ignore opacity set through window rules.
    ignore_opacity_window_rule: bool,

    /// Whether this window should invert its configured block-out state.
    invert_block_out_window_rule: bool,

    /// Buffer to draw instead of the window when it should be blocked out.
    block_out_buffer: RefCell<SolidColorBuffer>,

    /// The blur config, passed for background effect rendering.
    blur_config: niri_config::Blur,

    /// Whether the next configure should be animated, if the configured state changed.
    animate_next_configure: bool,

    /// Serials of commits that should be animated.
    animate_serials: Vec<Serial>,

    /// Snapshot right before an animated commit, without popups.
    animation_snapshot: Option<LayoutElementRenderSnapshot>,

    /// State for the logic to request a size once (for floating windows).
    request_size_once: Option<RequestSizeOnce>,

    /// Transaction that the next configure should take part in, if any.
    transaction_for_next_configure: Option<Transaction>,

    /// Pending transactions that have not been added as blockers for this window yet.
    pending_transactions: Vec<(Serial, Transaction)>,

    /// State of an ongoing interactive resize.
    interactive_resize: Option<InteractiveResize>,

    /// Last time interactive resize was started.
    ///
    /// Used for double-resize-click tracking.
    last_interactive_resize_start: Cell<Option<(Duration, ResizeEdge)>>,

    /// Whether this window is in windowed (fake) fullscreen.
    ///
    /// In this mode, the underlying window is told that it's fullscreen, while keeping it as
    /// a regular, non-fullscreen tile.
    is_windowed_fullscreen: bool,

    /// Whether this window is pending to go to windowed (fake) fullscreen.
    ///
    /// Several places in the layout code assume that is_fullscreen() can flip only on a commit.
    /// Which is something that we do want to flip when changing is_windowed_fullscreen. Flipping
    /// it right away would mean remembering to call layout.update_window() after any operation
    /// that may change is_windowed_fullscreen, which is quite tricky and error-prone, especially
    /// for deeply nested operations.
    ///
    /// It's also not clear what's the best way to go about it. Ideally we'd wait for configure ack
    /// and commit before "committing" to is_windowed_fullscreen, however, since it's not real
    /// Wayland state, we may end up with no Wayland state change to configure at all.
    ///
    /// For example: when the window is in real fullscreen, but its non-fullscreen size matches
    /// its fullscreen size. Then turning on is_windowed_fullscreen will both keep the
    /// fullscreen state, and keep the size (since it matches), resulting in no configure.
    ///
    /// So we work around this by emulating a configure-ack/commit cycle through
    /// is_pending_windowed_fullscreen and uncommitted_windowed_fullscreen. We ensure we send
    /// actual configures in all cases through needs_configure. This can result in unnecessary
    /// configures (like in the example above), but in most cases there will be a configure
    /// anyway to change the Fullscreen state and/or the size. What this gives us is being able
    /// to synchronize our windowed fullscreen state to the real window updates to avoid any
    /// flickering.
    is_pending_windowed_fullscreen: bool,

    /// Pending windowed fullscreen updates.
    ///
    /// These have been "sent" to the window in form of configures, but the window hadn't committed
    /// in response yet.
    uncommitted_windowed_fullscreen: Vec<(Serial, bool)>,

    /// Whether this window is maximized.
    ///
    /// We have to track this ourselves in addition to the Maximized toplevel state in order to
    /// support windowed fullscreen, since in windowed fullscreen the toplevel state is always
    /// Fullscreen. So we need this variable to be able to report accurate sizing mode and pending
    /// sizing mode.
    is_maximized: bool,

    /// Whether this window is pending to be maximized.
    ///
    /// We have to track this ourselves due to windowed fullscreen.
    is_pending_maximized: bool,

    /// Pending maximized updates.
    ///
    /// These have been "sent" to the window in form of configures, but the window hadn't committed
    /// in response yet.
    uncommitted_maximized: Vec<(Serial, bool)>,

    /// Most recent monotonic time when the window had the focus.
    focus_timestamp: Option<Duration>,
}

niri_render_elements! {
    WindowCastRenderElements<R> => {
        Layout = LayoutElementRenderElement<R>,
        // Blocked-out window with rounded corners.
        Border = BorderRenderElement,
    }
}

static MAPPED_ID_COUNTER: IdCounter = IdCounter::new();

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MappedId(u64);

impl MappedId {
    pub fn next() -> MappedId {
        MappedId(MAPPED_ID_COUNTER.next())
    }

    pub fn get(self) -> u64 {
        self.0
    }

    /// Converts the ID to a string that can be used as an identifier in
    /// ext_foreign_toplevel_handle_v1::identifier
    ///
    /// > An identifier is a string that contains up to 32 printable ASCII bytes.
    /// > An identifier must not be an empty string.
    ///
    /// Since the ID is exposed to IPC, it's useful for this conversion to be stable and reversible.
    /// That way, clients can associate a foreign toplevel handle with an IPC window ID.
    ///
    /// We use the decimal representation of the ID, which is up to 20 characters long for u64::MAX.
    /// This is within the 32-character limit, and is nice because it matches up with how `niri msg`
    /// prints the IDs to the console.
    ///
    /// This namespace can be extended in the future, with any non-numeric prefix to disambiguate.
    pub fn to_protocol_identifier(self) -> String {
        format!("{}", self.0)
    }
}

#[derive(Default)]
struct SurfaceActivatedEntries {
    ids: RefCell<HashSet<MappedId>>,
}

fn update_surface_activated_entries(surface: &WlSurface, id: MappedId, active: bool) -> bool {
    with_states(surface, |states| {
        let entries = states
            .data_map
            .get_or_insert(SurfaceActivatedEntries::default);
        let mut ids = entries.ids.borrow_mut();

        if active {
            ids.insert(id);
        } else {
            ids.remove(&id);
        }

        !ids.is_empty()
    })
}

fn baked_texture_logical_size(
    baked: &BakedBuffer<TextureBuffer<GlesTexture>>,
) -> Size<f64, Logical> {
    baked
        .dst
        .map(|dst| dst.to_f64())
        .or_else(|| baked.src.map(|src| src.size))
        .unwrap_or_else(|| baked.buffer.logical_size())
}

fn crop_baked_texture_to_rect(
    baked: &mut BakedBuffer<TextureBuffer<GlesTexture>>,
    clip: Rectangle<f64, Logical>,
) -> bool {
    let logical_size = baked_texture_logical_size(baked);
    let logical_rect = Rectangle::new(baked.location, logical_size);
    let Some(intersection) = logical_rect.intersection(clip) else {
        return false;
    };

    if intersection == logical_rect {
        return true;
    }

    let full_src = baked
        .src
        .unwrap_or_else(|| Rectangle::from_size(baked.buffer.logical_size()));
    let src_scale = Scale::from((
        full_src.size.w / logical_rect.size.w.max(1e-9),
        full_src.size.h / logical_rect.size.h.max(1e-9),
    ));
    let src_offset = (intersection.loc - logical_rect.loc).upscale(src_scale);

    baked.location = intersection.loc;
    baked.src = Some(Rectangle::new(
        full_src.loc + src_offset,
        intersection.size.upscale(src_scale),
    ));

    if baked.dst.is_some() {
        baked.dst = Some(intersection.size.to_i32_round());
    }

    true
}

/// Interactive resize state.
#[derive(Debug)]
enum InteractiveResize {
    /// The resize is ongoing.
    Ongoing(InteractiveResizeData),
    /// The resize has stopped and we're waiting to send the last configure.
    WaitingForLastConfigure(InteractiveResizeData),
    /// We had sent the last resize configure and are waiting for the corresponding commit.
    WaitingForLastCommit {
        data: InteractiveResizeData,
        serial: Serial,
    },
}

impl InteractiveResize {
    fn data(&self) -> InteractiveResizeData {
        match self {
            InteractiveResize::Ongoing(data) => *data,
            InteractiveResize::WaitingForLastConfigure(data) => *data,
            InteractiveResize::WaitingForLastCommit { data, .. } => *data,
        }
    }
}

/// Request-size-once logic state.
#[derive(Debug, Clone, Copy)]
enum RequestSizeOnce {
    /// Waiting for configure to be sent with the requested size.
    WaitingForConfigure,
    /// Waiting for the window to commit in response to the configure.
    WaitingForCommit(Serial),
    /// When configuring, use the current window size.
    UseWindowSize,
}

#[derive(Debug, Clone, Copy)]
struct MirrorContentTransform {
    source_geometry: Rectangle<f64, Logical>,
    content_rect: Rectangle<f64, Logical>,
    visible_rect: Rectangle<f64, Logical>,
    scale: f64,
}

#[derive(Debug, Clone, Copy)]
struct MirrorRenderLayout {
    transform: MirrorContentTransform,
    visible_loc: Point<f64, Logical>,
    content_origin: Point<i32, smithay::utils::Physical>,
    surface_origin: Point<i32, smithay::utils::Physical>,
}

const MIRROR_NEAREST_SCALE_SNAP_TOLERANCE: f64 = 1.;

impl Mapped {
    pub fn new(window: Window, rules: ResolvedWindowRules, hook: HookId, config: &Config) -> Self {
        let surface = window.wl_surface().expect("no X11 support");
        let credentials = get_credentials_for_surface(&surface);
        let id = MappedId::next();
        let mut rv = Self {
            window,
            id,
            source_id: id,
            is_mirror: false,
            mirror_size: Size::from((1, 1)),
            mirror_sizing_mode: SizingMode::Normal,
            mirror_pending_sizing_mode: SizingMode::Normal,
            credentials,
            pre_commit_hook: Some(hook),
            rules,
            need_to_recompute_rules: false,
            needs_configure: false,
            needs_frame_callback: false,
            offscreen_data: RefCell::new(None),
            is_urgent: false,
            is_focused: false,
            is_activated: false,
            is_active_in_column: true,
            is_floating: false,
            is_sticky: false,
            is_window_cast_target: false,
            ignore_opacity_window_rule: false,
            invert_block_out_window_rule: false,
            block_out_buffer: RefCell::new(SolidColorBuffer::new((0., 0.), [0., 0., 0., 0.])),
            blur_config: config.blur,
            animate_next_configure: false,
            animate_serials: Vec::new(),
            animation_snapshot: None,
            request_size_once: None,
            transaction_for_next_configure: None,
            pending_transactions: Vec::new(),
            interactive_resize: None,
            last_interactive_resize_start: Cell::new(None),
            is_windowed_fullscreen: false,
            is_pending_windowed_fullscreen: false,
            uncommitted_windowed_fullscreen: Vec::new(),
            is_maximized: false,
            is_pending_maximized: false,
            uncommitted_maximized: Vec::new(),
            focus_timestamp: None,
        };

        rv.is_maximized = rv.sizing_mode().is_maximized();
        rv.is_pending_maximized = rv.pending_sizing_mode().is_maximized();

        rv
    }

    pub fn new_mirror(source: &Mapped) -> Self {
        let id = MappedId::next();
        let mut rules = source.rules.clone();
        rules.clip_to_geometry = Some(true);
        // Mirrors are viewports around another window, so their letterboxing/padding must stay
        // transparent instead of inheriting non-SSD border background fills from the source.
        rules.draw_border_with_background = Some(false);
        Self {
            window: source.window.clone(),
            id,
            source_id: source.source_id,
            is_mirror: true,
            mirror_size: source.size(),
            mirror_sizing_mode: SizingMode::Normal,
            mirror_pending_sizing_mode: SizingMode::Normal,
            credentials: source.credentials.clone(),
            pre_commit_hook: None,
            rules,
            need_to_recompute_rules: false,
            needs_configure: false,
            needs_frame_callback: false,
            offscreen_data: RefCell::new(None),
            is_urgent: false,
            is_focused: false,
            is_activated: false,
            is_active_in_column: true,
            is_floating: source.is_floating,
            is_sticky: false,
            is_window_cast_target: false,
            ignore_opacity_window_rule: source.ignore_opacity_window_rule,
            invert_block_out_window_rule: source.invert_block_out_window_rule,
            block_out_buffer: RefCell::new(SolidColorBuffer::new((0., 0.), [0., 0., 0., 0.])),
            blur_config: source.blur_config,
            animate_next_configure: false,
            animate_serials: Vec::new(),
            animation_snapshot: None,
            request_size_once: None,
            transaction_for_next_configure: None,
            pending_transactions: Vec::new(),
            interactive_resize: None,
            last_interactive_resize_start: Cell::new(None),
            is_windowed_fullscreen: false,
            is_pending_windowed_fullscreen: false,
            uncommitted_windowed_fullscreen: Vec::new(),
            is_maximized: false,
            is_pending_maximized: false,
            uncommitted_maximized: Vec::new(),
            focus_timestamp: None,
        }
    }

    pub fn toplevel(&self) -> &ToplevelSurface {
        self.window.toplevel().expect("no X11 support")
    }

    /// Recomputes the resolved window rules and returns whether they changed.
    pub fn recompute_window_rules(&mut self, rules: &[WindowRule], is_at_startup: bool) -> bool {
        self.need_to_recompute_rules = false;

        let mut new_rules =
            ResolvedWindowRules::compute(rules, WindowRef::Mapped(self), is_at_startup);
        if self.is_mirror {
            new_rules.clip_to_geometry = Some(true);
            new_rules.draw_border_with_background = Some(false);
        }
        if new_rules == self.rules {
            return false;
        }

        // If the opacity window rule no longer makes the window semitransparent, reset the ignore
        // flag to reduce surprises down the line.
        if !new_rules.opacity.is_some_and(|o| o < 1.) {
            self.ignore_opacity_window_rule = false;
        }

        self.rules = new_rules;
        true
    }

    pub fn recompute_window_rules_if_needed(
        &mut self,
        rules: &[WindowRule],
        is_at_startup: bool,
    ) -> bool {
        if !self.need_to_recompute_rules {
            return false;
        }

        self.recompute_window_rules(rules, is_at_startup)
    }

    pub fn set_needs_configure(&mut self) {
        if self.is_mirror {
            return;
        }
        self.needs_configure = true;
    }

    pub fn id(&self) -> MappedId {
        self.id
    }

    pub fn source_id(&self) -> MappedId {
        self.source_id
    }

    pub fn is_mirror(&self) -> bool {
        self.is_mirror
    }

    pub fn element_namespace(&self) -> Option<usize> {
        self.is_mirror
            .then(|| self.id.get().try_into().unwrap_or(usize::MAX))
    }

    fn mirror_transform(&self) -> MirrorContentTransform {
        self.mirror_transform_for_size(self.mirror_size.to_f64())
    }

    fn mirror_content_rect_for_scale(
        &self,
        dst: Size<f64, Logical>,
        source_geometry: Rectangle<f64, Logical>,
        scale: f64,
    ) -> MirrorContentTransform {
        let rendered = source_geometry.size.upscale(scale);
        let offset = Point::from(((dst.w - rendered.w) / 2., (dst.h - rendered.h) / 2.));
        let content_rect = Rectangle::new(offset, rendered);
        let visible_rect = content_rect
            .intersection(Rectangle::from_size(dst))
            .unwrap_or_else(|| Rectangle::from_size(Size::from((0., 0.))));

        MirrorContentTransform {
            source_geometry,
            content_rect,
            visible_rect,
            scale,
        }
    }

    fn mirror_nearest_scale_candidate(
        &self,
        dst: Size<f64, Logical>,
        source_geometry: Rectangle<f64, Logical>,
    ) -> Option<f64> {
        let scale_x = dst.w / source_geometry.size.w;
        let scale_y = dst.h / source_geometry.size.h;
        let contain_scale = f64::min(scale_x, scale_y).max(0.0001);

        let candidate = if contain_scale >= 1. {
            contain_scale.round().max(1.)
        } else {
            let reciprocal = (1. / contain_scale).round().max(1.);
            1. / reciprocal
        };

        if (candidate - contain_scale).abs() <= f64::EPSILON {
            return Some(candidate);
        }

        let rendered = source_geometry.size.upscale(candidate);
        let width_is_constraining = scale_x <= scale_y + f64::EPSILON;
        let height_is_constraining = scale_y <= scale_x + f64::EPSILON;

        let width_matches = !width_is_constraining
            || (rendered.w - dst.w).abs() <= MIRROR_NEAREST_SCALE_SNAP_TOLERANCE;
        let height_matches = !height_is_constraining
            || (rendered.h - dst.h).abs() <= MIRROR_NEAREST_SCALE_SNAP_TOLERANCE;

        let width_overflow_ok = rendered.w - dst.w <= MIRROR_NEAREST_SCALE_SNAP_TOLERANCE;
        let height_overflow_ok = rendered.h - dst.h <= MIRROR_NEAREST_SCALE_SNAP_TOLERANCE;

        (width_matches && height_matches && width_overflow_ok && height_overflow_ok)
            .then_some(candidate)
    }

    fn mirror_transform_for_size(&self, dst: Size<f64, Logical>) -> MirrorContentTransform {
        let mut source_geometry = self.window.geometry().to_f64();
        source_geometry.size.w = source_geometry.size.w.max(1.);
        source_geometry.size.h = source_geometry.size.h.max(1.);

        // Mirrors look crisper when the viewport is effectively asking for an integer upscale or
        // reciprocal downscale. If the contain-fit size differs by at most one logical pixel on
        // the constraining axis, prefer that nearest-neighbour-friendly scale and clip/pad the
        // remainder instead of resampling the whole window.
        if let Some(scale) = self.mirror_nearest_scale_candidate(dst, source_geometry) {
            return self.mirror_content_rect_for_scale(dst, source_geometry, scale);
        }

        let scale = f64::min(
            dst.w / source_geometry.size.w,
            dst.h / source_geometry.size.h,
        )
        .max(0.0001);
        self.mirror_content_rect_for_scale(dst, source_geometry, scale)
    }

    fn mirror_render_layout(
        &self,
        location: Point<f64, Logical>,
        mirror_size: Size<f64, Logical>,
        scale: Scale<f64>,
    ) -> MirrorRenderLayout {
        let transform = self.mirror_transform_for_size(mirror_size);
        let content_loc = location + transform.content_rect.loc;
        let visible_loc = location + transform.visible_rect.loc;
        let content_origin = content_loc.to_physical_precise_round(scale);
        let surface_origin =
            (content_loc - transform.source_geometry.loc).to_physical_precise_round(scale);

        MirrorRenderLayout {
            transform,
            visible_loc,
            content_origin,
            surface_origin,
        }
    }

    fn render_mirror_normal<R: NiriRenderer>(
        &self,
        ctx: RenderCtx<R>,
        location: Point<f64, Logical>,
        mirror_size: Size<f64, Logical>,
        scale: Scale<f64>,
        alpha: f32,
        push: &mut dyn FnMut(LayoutElementRenderElement<R>),
    ) {
        let blocked_out = ctx.should_block_out(self.effective_block_out_from());
        let namespace = self.element_namespace().unwrap();
        let mut buffer = self.block_out_buffer.borrow_mut();
        // Keep a full-size transparent element for damage/tracking and letterboxing. When the
        // mirror is blocked out we simply stop after this transparent fill, matching real windows.
        buffer.update(mirror_size, [0., 0., 0., 0.]);
        let elem =
            SolidColorRenderElement::from_buffer(&buffer, location, alpha, Kind::Unspecified);
        push(elem.into());

        if blocked_out {
            return;
        }

        let layout = self.mirror_render_layout(location, mirror_size, scale);
        let clip_shader = ClippedSurfaceRenderElement::shader(ctx.renderer).cloned();
        let clip_geo = Rectangle::new(layout.visible_loc, layout.transform.visible_rect.size);
        let clip_radius = self
            .geometry_corner_radius()
            .fit_to(clip_geo.size.w as f32, clip_geo.size.h as f32);
        let surface = self.toplevel().wl_surface();
        let mut push = |elem: WaylandSurfaceRenderElement<R>| {
            let elem = NamespacedScaledWaylandSurfaceRenderElement::new(
                NamespacedElement::new(elem, namespace),
                layout.content_origin,
                Scale::from(layout.transform.scale),
            );
            if let Some(shader) = clip_shader.clone() {
                if ClippedSurfaceRenderElement::will_clip(&elem, scale, clip_geo, clip_radius) {
                    push(
                        ClippedSurfaceRenderElement::new(
                            elem,
                            scale,
                            clip_geo,
                            shader,
                            clip_radius,
                        )
                        .into(),
                    );
                    return;
                }
            }

            push(elem.into());
        };
        push_elements_from_surface_tree(
            ctx.renderer,
            surface,
            layout.surface_origin,
            scale,
            alpha,
            Kind::Unspecified,
            &mut push,
        );
    }

    pub fn mirror_content_transform(&self) -> (Point<f64, Logical>, f64) {
        let transform = self.mirror_transform();
        (transform.content_rect.loc, transform.scale)
    }

    pub fn mirror_point_to_source(
        &self,
        point: Point<f64, Logical>,
    ) -> Option<Point<f64, Logical>> {
        let transform = self.mirror_transform();
        let point = (point - transform.content_rect.loc).downscale(transform.scale)
            + transform.source_geometry.loc;
        transform.source_geometry.contains(point).then_some(point)
    }

    pub fn credentials(&self) -> Option<&Credentials> {
        self.credentials.as_ref()
    }

    pub fn offscreen_data(&self) -> Ref<'_, Option<OffscreenData>> {
        self.offscreen_data.borrow()
    }

    pub fn is_focused(&self) -> bool {
        self.is_focused
    }

    pub fn is_active_in_column(&self) -> bool {
        self.is_active_in_column
    }

    pub fn is_floating(&self) -> bool {
        self.is_floating
    }

    pub fn is_sticky(&self) -> bool {
        self.is_sticky
    }

    pub fn is_window_cast_target(&self) -> bool {
        self.is_window_cast_target
    }

    pub fn toggle_ignore_opacity_window_rule(&mut self) {
        self.ignore_opacity_window_rule = !self.ignore_opacity_window_rule;
    }

    pub fn effective_block_out_from(&self) -> Option<BlockOutFrom> {
        let block_out_from = self.rules.block_out_from;
        if !self.invert_block_out_window_rule {
            return block_out_from;
        }

        match block_out_from {
            Some(_) => None,
            None => Some(BlockOutFrom::Screencast),
        }
    }

    pub fn is_block_out(&self) -> bool {
        self.effective_block_out_from().is_some()
    }

    pub fn toggle_block_out_window_rule(&mut self) {
        self.invert_block_out_window_rule = !self.invert_block_out_window_rule;
    }

    pub fn set_is_focused(&mut self, is_focused: bool) {
        if self.is_focused == is_focused {
            return;
        }

        self.is_focused = is_focused;
        self.is_urgent = false;
        self.need_to_recompute_rules = true;
    }

    pub fn set_is_window_cast_target(&mut self, value: bool) {
        if self.is_window_cast_target == value {
            return;
        }

        self.is_window_cast_target = value;
        self.need_to_recompute_rules = true;
    }

    /// Renders a snapshot of the window without popups.
    fn render_snapshot(&self, renderer: &mut GlesRenderer) -> LayoutElementRenderSnapshot {
        let _span = tracy_client::span!("Mapped::render_snapshot");

        let size = self.size().to_f64();

        let mut buffer = self.block_out_buffer.borrow_mut();
        buffer.update(size, [0., 0., 0., 0.]);
        let blocked_out_contents = vec![BakedBuffer {
            buffer: buffer.clone(),
            location: Point::from((0., 0.)),
            src: None,
            dst: None,
        }];

        let buf_pos = self.window.geometry().loc.upscale(-1).to_f64();

        let mut contents = vec![];

        let surface = self.toplevel().wl_surface();
        if self.is_mirror {
            render_snapshot_from_surface_tree(renderer, surface, Point::default(), &mut contents);

            let transform = self.mirror_transform();
            let source_geometry = transform.source_geometry;
            contents.retain_mut(|baked| crop_baked_texture_to_rect(baked, source_geometry));

            for baked in &mut contents {
                let logical_size = baked_texture_logical_size(baked);
                baked.location = transform.content_rect.loc
                    + (baked.location - transform.source_geometry.loc).upscale(transform.scale);
                baked.dst = Some(logical_size.upscale(transform.scale).to_i32_round());
            }
            contents.retain_mut(|baked| crop_baked_texture_to_rect(baked, transform.visible_rect));
        } else {
            render_snapshot_from_surface_tree(renderer, surface, buf_pos, &mut contents);
        }

        RenderSnapshot {
            contents,
            contents_with_blocked_out_bg: None,
            blocked_out_contents,
            block_out_from: self.effective_block_out_from(),
            size,
            texture: Default::default(),
            texture_with_blocked_out_bg: Default::default(),
            blocked_out_texture: Default::default(),
        }
    }

    pub fn should_animate_commit(&mut self, commit_serial: Serial) -> bool {
        let mut should_animate = false;
        self.animate_serials.retain_mut(|serial| {
            if commit_serial.is_no_older_than(serial) {
                should_animate = true;
                false
            } else {
                true
            }
        });
        should_animate
    }

    pub fn store_animation_snapshot(&mut self, renderer: &mut GlesRenderer) {
        self.animation_snapshot = Some(self.render_snapshot(renderer));
    }

    pub fn take_pending_transaction(&mut self, commit_serial: Serial) -> Option<Transaction> {
        let mut rv = None;

        // Pending transactions are appended in order by serial, so we can loop from the start
        // until we hit a serial that is too new.
        while let Some((serial, _)) = self.pending_transactions.first() {
            // In this loop, we will complete the transaction corresponding to the commit, as well
            // as all transactions corresponding to previous serials. This can happen when we
            // request resizes too quickly, and the surface only responds to the last one.
            //
            // Note that in this case, completing the previous transactions can result in an
            // inconsistent visual state, if another window is waiting for this window to assume a
            // specific size (in a previous transaction), which is now different (in this commit).
            //
            // However, there isn't really a good way to deal with that. We cannot cancel any
            // transactions because we need to keep sending frame callbacks, and cancelling a
            // transaction will make the corresponding frame callbacks get lost, and the window
            // will hang.
            //
            // This is why resize throttling (implemented separately) is important: it prevents
            // visually inconsistent states by way of never having more than one transaction in
            // flight.
            if commit_serial.is_no_older_than(serial) {
                let (_, transaction) = self.pending_transactions.remove(0);
                // Previous transaction is dropped here, signaling completion.
                rv = Some(transaction);
            } else {
                break;
            }
        }

        rv
    }

    pub fn last_interactive_resize_start(&self) -> &Cell<Option<(Duration, ResizeEdge)>> {
        &self.last_interactive_resize_start
    }

    pub fn render_for_screen_cast<R: NiriRenderer>(
        &self,
        renderer: &mut R,
        scale: Scale<f64>,
        block_out_enabled: bool,
        push: &mut dyn FnMut(WindowCastRenderElements<R>),
    ) {
        let bbox = self.window_cast_bbox(scale);

        let has_border_shader = BorderRenderElement::has_shader(renderer);
        let radius = self.geometry_corner_radius();
        let window_size = self
            .size()
            .to_f64()
            .to_physical_precise_round(scale)
            .to_logical(scale);
        let radius = radius.fit_to(window_size.w as f32, window_size.h as f32);
        let location = if self.is_mirror {
            Point::default()
        } else {
            self.window.geometry().loc.to_f64() - bbox.loc.to_f64().to_logical(scale)
        };

        let use_border = |elem| {
            if let LayoutElementRenderElement::SolidColor(elem) = &elem {
                // In this branch we're rendering a blocked-out window with a solid color. We need
                // to render it with a rounded corner shader even if clip_to_geometry is false,
                // because in this case we're assuming that the unclipped window CSD already has
                // corners rounded to the user-provided radius, so our blocked-out rendering should
                // match that radius.
                if radius != CornerRadius::default() && has_border_shader {
                    let geo = elem.geo();
                    return BorderRenderElement::new(
                        geo.size,
                        Rectangle::from_size(geo.size),
                        GradientInterpolation::default(),
                        Color::from_color32f(elem.color()),
                        Color::from_color32f(elem.color()),
                        0.,
                        Rectangle::from_size(geo.size),
                        0.,
                        radius,
                        scale.x as f32,
                        1.,
                    )
                    .with_location(geo.loc)
                    .into();
                }
            }

            WindowCastRenderElements::from(elem)
        };

        self.render(
            RenderCtx {
                renderer,
                target: RenderTarget::Screencast,
                block_out_enabled,
                xray: None,
            },
            location,
            scale,
            1.,
            XrayPos::default(),
            &mut |elem| push(use_border(elem)),
        );
    }

    pub fn window_cast_bbox(&self, scale: Scale<f64>) -> Rectangle<i32, smithay::utils::Physical> {
        if self.is_mirror {
            Rectangle::from_size(self.size()).to_physical_precise_up(scale)
        } else {
            self.window.bbox_with_popups().to_physical_precise_up(scale)
        }
    }

    pub fn window_cast_buffer_pos(
        &self,
        win_pos: Point<f64, Logical>,
        scale: Scale<f64>,
    ) -> Point<f64, Logical> {
        if self.is_mirror {
            win_pos - self.buf_loc().to_f64()
        } else {
            let bbox = self.window_cast_bbox(scale);
            win_pos + bbox.loc.to_f64().to_logical(scale)
        }
    }

    pub fn get_focus_timestamp(&self) -> Option<Duration> {
        self.focus_timestamp
    }

    pub fn set_focus_timestamp(&mut self, timestamp: Duration) {
        self.focus_timestamp.replace(timestamp);
    }

    pub fn send_frame<T, F>(
        &mut self,
        output: &Output,
        time: T,
        throttle: Option<Duration>,
        mut primary_scan_out_output: F,
    ) where
        T: Into<Duration>,
        F: FnMut(&WlSurface, &SurfaceData) -> Option<Output> + Copy,
    {
        let needs_frame_callback = self.needs_frame_callback;
        self.needs_frame_callback = false;

        let should_send = move |surface: &WlSurface, states: &SurfaceData| {
            // Let primary_scan_out_output() run its logic and update internal state.
            if let Some(output) = primary_scan_out_output(surface, states) {
                return Some(output);
            }

            // Send unconditionally to all surfaces if the window needs a surface callback.
            needs_frame_callback.then(|| output.clone())
        };
        self.window.send_frame(output, time, throttle, should_send);
    }

    pub fn update_tiled_state(&self, prefer_no_csd: bool) {
        if self.is_mirror {
            let _ = prefer_no_csd;
            return;
        }

        update_tiled_state(self.toplevel(), prefer_no_csd, self.rules.tiled_state);
    }

    pub fn is_windowed_fullscreen(&self) -> bool {
        self.is_windowed_fullscreen
    }

    pub fn set_urgent(&mut self, urgent: bool) {
        if self.is_focused && urgent {
            return;
        }

        let changed = self.is_urgent != urgent;
        self.is_urgent = urgent;
        self.need_to_recompute_rules |= changed;
    }

    pub fn is_urgent(&self) -> bool {
        self.is_urgent
    }
}

impl Drop for Mapped {
    fn drop(&mut self) {
        if self.is_activated {
            let surface = self.toplevel().wl_surface();
            if surface.is_alive() {
                let any_active = update_surface_activated_entries(surface, self.id, false);
                self.toplevel().with_pending_state(|state| {
                    if any_active {
                        state.states.set(xdg_toplevel::State::Activated)
                    } else {
                        state.states.unset(xdg_toplevel::State::Activated)
                    }
                });
            }
        }

        if let Some(hook) = &self.pre_commit_hook {
            remove_pre_commit_hook(self.toplevel().wl_surface(), hook);
        }
    }
}

impl LayoutElement for Mapped {
    type Id = MappedId;

    fn id(&self) -> &Self::Id {
        &self.id
    }

    fn update_config(&mut self, blur_config: niri_config::Blur) {
        self.blur_config = blur_config;
    }

    fn size(&self) -> Size<i32, Logical> {
        if self.is_mirror {
            return self.mirror_size;
        }

        self.window.geometry().size
    }

    fn buf_loc(&self) -> Point<i32, Logical> {
        if self.is_mirror {
            let transform = self.mirror_transform();
            return (transform.content_rect.loc
                - transform.source_geometry.loc.upscale(transform.scale))
            .to_i32_round();
        }

        Point::from((0, 0)) - self.window.geometry().loc
    }

    fn is_in_input_region(&self, point: Point<f64, Logical>) -> bool {
        if self.is_mirror {
            let Some(point) = self.mirror_point_to_source(point) else {
                return false;
            };
            return self.window.is_in_input_region(&point);
        }

        let surface_local = point + self.window.geometry().loc.to_f64();
        self.window.is_in_input_region(&surface_local)
    }

    fn render_normal_with_size<R: NiriRenderer>(
        &self,
        ctx: RenderCtx<R>,
        location: Point<f64, Logical>,
        size: Size<f64, Logical>,
        scale: Scale<f64>,
        alpha: f32,
        push: &mut dyn FnMut(LayoutElementRenderElement<R>),
    ) {
        if self.is_mirror {
            self.render_mirror_normal(ctx, location, size, scale, alpha, push);
            return;
        }

        self.render_normal(ctx, location, scale, alpha, push);
    }

    fn render_normal<R: NiriRenderer>(
        &self,
        ctx: RenderCtx<R>,
        location: Point<f64, Logical>,
        scale: Scale<f64>,
        alpha: f32,
        push: &mut dyn FnMut(LayoutElementRenderElement<R>),
    ) {
        if self.is_mirror {
            self.render_mirror_normal(ctx, location, self.mirror_size.to_f64(), scale, alpha, push);
            return;
        }

        if ctx.should_block_out(self.effective_block_out_from()) {
            let mut buffer = self.block_out_buffer.borrow_mut();
            buffer.resize(self.window.geometry().size.to_f64());
            let elem =
                SolidColorRenderElement::from_buffer(&buffer, location, alpha, Kind::Unspecified);
            push(elem.into());
        } else {
            let buf_pos = location - self.window.geometry().loc.to_f64();
            let surface = self.toplevel().wl_surface();
            let mut push = |elem: WaylandSurfaceRenderElement<R>| push(elem.into());
            push_elements_from_surface_tree(
                ctx.renderer,
                surface,
                buf_pos.to_physical_precise_round(scale),
                scale,
                alpha,
                Kind::ScanoutCandidate,
                &mut push,
            )
        }
    }

    fn render_popups<R: NiriRenderer>(
        &self,
        mut ctx: RenderCtx<R>,
        location: Point<f64, Logical>,
        scale: Scale<f64>,
        alpha: f32,
        xray_pos: XrayPos,
        push: &mut dyn FnMut(LayoutElementRenderElement<R>),
    ) {
        if ctx.should_block_out(self.effective_block_out_from()) {
            return;
        }

        if self.is_mirror {
            let transform = self.mirror_transform();
            let namespace = self.element_namespace().unwrap();
            let content_loc = location + transform.content_rect.loc;
            let content_origin = content_loc.to_physical_precise_round(scale);
            let root = self.toplevel().wl_surface();

            for (popup, offset) in PopupManager::popups_for_surface(root) {
                let popup_rules = match popup {
                    PopupKind::Xdg(_) => self.rules.popups,
                    PopupKind::InputMethod(_) => niri_config::ResolvedPopupsRules::default(),
                };
                let alpha = alpha * popup_rules.opacity.unwrap_or(1.).clamp(0., 1.);

                let surface = popup.wl_surface();
                let popup_geo = popup.geometry();
                let surface_loc = (content_loc - transform.source_geometry.loc
                    + (offset - popup_geo.loc).to_f64())
                .to_physical_precise_round(scale);

                push_elements_from_surface_tree(
                    ctx.renderer,
                    surface,
                    surface_loc,
                    scale,
                    alpha,
                    Kind::Unspecified,
                    &mut |elem| {
                        let elem = NamespacedScaledWaylandSurfaceRenderElement::new(
                            NamespacedElement::new(elem, namespace),
                            content_origin,
                            Scale::from(transform.scale),
                        );
                        push(elem.into())
                    },
                );

                let geometry = Rectangle::new(
                    content_loc
                        + (offset.to_f64() - transform.source_geometry.loc)
                            .upscale(transform.scale),
                    popup_geo.size.to_f64().upscale(transform.scale),
                );
                let surface_off = popup_geo.loc.upscale(-1).to_f64();
                let surface_anim_scale = Scale::from(transform.scale);
                let mut effect = popup_rules.background_effect;
                // Default xray to false for pop-ups since they're always on top of something.
                if effect.xray.is_none() {
                    effect.xray = Some(false);
                }
                let xray_pos = xray_pos.offset(geometry.loc - location);
                background_effect::render_for_tile(
                    ctx.as_gles(),
                    None,
                    geometry,
                    scale.x,
                    false,
                    surface,
                    surface_off,
                    surface_anim_scale,
                    self.blur_config,
                    popup_rules.geometry_corner_radius.unwrap_or_default(),
                    effect,
                    false,
                    xray_pos,
                    &mut |elem| push(elem.into()),
                );
            }

            return;
        }

        let surface = self.toplevel().wl_surface();
        for (popup, offset) in PopupManager::popups_for_surface(surface) {
            let popup_rules = match popup {
                PopupKind::Xdg(_) => self.rules.popups,
                // IME popups aren't affected by rules for regular popups.
                PopupKind::InputMethod(_) => niri_config::ResolvedPopupsRules::default(),
            };
            let alpha = alpha * popup_rules.opacity.unwrap_or(1.).clamp(0., 1.);

            let surface = popup.wl_surface();
            let popup_geo = popup.geometry();
            let surface_loc = location + (offset - popup.geometry().loc).to_f64();

            push_elements_from_surface_tree(
                ctx.renderer,
                surface,
                surface_loc.to_physical_precise_round(scale),
                scale,
                alpha,
                Kind::ScanoutCandidate,
                &mut |elem| push(elem.into()),
            );

            let geometry = Rectangle::new(location + offset.to_f64(), popup_geo.size.to_f64());
            let surface_off = popup_geo.loc.upscale(-1).to_f64();
            let surface_anim_scale = Scale::from(1.);
            let mut effect = popup_rules.background_effect;
            // Default xray to false for pop-ups since they're always on top of something.
            if effect.xray.is_none() {
                effect.xray = Some(false);
            }
            let xray_pos = xray_pos.offset(offset.to_f64());
            background_effect::render_for_tile(
                ctx.as_gles(),
                None,
                geometry,
                scale.x,
                false,
                surface,
                surface_off,
                surface_anim_scale,
                self.blur_config,
                popup_rules.geometry_corner_radius.unwrap_or_default(),
                effect,
                false,
                xray_pos,
                &mut |elem| push(elem.into()),
            );
        }
    }

    fn render_background_effect(
        &self,
        ctx: RenderCtx<GlesRenderer>,
        geometry: Rectangle<f64, Logical>,
        scale: f64,
        clip_to_geometry: bool,
        surface_anim_scale: Scale<f64>,
        radius: CornerRadius,
        xray_pos: XrayPos,
        push: &mut dyn FnMut(BackgroundEffectElement),
    ) {
        let should_block_out = ctx.should_block_out(self.effective_block_out_from());
        if should_block_out {
            return;
        }

        if self.is_mirror {
            let transform = self.mirror_transform();
            let geometry = Rectangle::new(
                geometry.loc + transform.visible_rect.loc,
                transform.visible_rect.size,
            );
            let surface_anim_scale = Scale {
                x: surface_anim_scale.x * transform.scale,
                y: surface_anim_scale.y * transform.scale,
            };
            background_effect::render_for_tile(
                ctx,
                None,
                geometry,
                scale,
                clip_to_geometry,
                self.toplevel().wl_surface(),
                transform.source_geometry.loc.upscale(-1.),
                surface_anim_scale,
                self.blur_config,
                radius,
                self.rules.background_effect,
                false,
                xray_pos.offset(transform.visible_rect.loc),
                push,
            );
            return;
        }

        background_effect::render_for_tile(
            ctx,
            None,
            geometry,
            scale,
            clip_to_geometry,
            self.toplevel().wl_surface(),
            self.buf_loc().to_f64(),
            surface_anim_scale,
            self.blur_config,
            radius,
            self.rules.background_effect,
            should_block_out,
            xray_pos,
            push,
        );
    }

    fn request_size(
        &mut self,
        size: Size<i32, Logical>,
        mode: SizingMode,
        animate: bool,
        transaction: Option<Transaction>,
    ) {
        if self.is_mirror {
            let size = Size::from((size.w.max(1), size.h.max(1)));
            self.mirror_size = size;
            self.mirror_sizing_mode = mode;
            self.mirror_pending_sizing_mode = mode;
            let _ = (animate, transaction);
            return;
        }

        // Going into real fullscreen resets windowed fullscreen.
        if mode == SizingMode::Fullscreen {
            self.is_pending_windowed_fullscreen = false;

            if self.is_windowed_fullscreen {
                // Make sure we receive a commit to update self.is_windowed_fullscreen to false
                // later on.
                self.needs_configure = true;
            }
        }

        self.is_pending_maximized = mode == SizingMode::Maximized;
        if self.is_maximized != self.is_pending_maximized {
            // Make sure we receive a commit to update self.is_maximized later on.
            self.needs_configure = true;
        }

        let changed = self.toplevel().with_pending_state(|state| {
            let changed = state.size != Some(size);
            state.size = Some(size);

            if mode.is_fullscreen() || self.is_pending_windowed_fullscreen {
                state.states.set(xdg_toplevel::State::Fullscreen);
                state.states.unset(xdg_toplevel::State::Maximized);
            } else if mode.is_maximized() {
                state.states.unset(xdg_toplevel::State::Fullscreen);
                state.states.set(xdg_toplevel::State::Maximized);
            } else {
                state.states.unset(xdg_toplevel::State::Fullscreen);
                state.states.unset(xdg_toplevel::State::Maximized);
            }

            changed
        });

        if changed && animate {
            self.animate_next_configure = true;
        }

        self.request_size_once = None;

        // Store the transaction regardless of whether the size changed. This is because with 3+
        // windows in a column, the size may change among windows 1 and 2 and then right away among
        // windows 2 and 3, and we want all windows 1, 2 and 3 to use the last transaction, rather
        // than window 1 getting stuck with the previous transaction that is immediately released
        // by 2.
        if let Some(transaction) = transaction {
            self.transaction_for_next_configure = Some(transaction);
        }
    }

    fn request_size_once(&mut self, size: Size<i32, Logical>, animate: bool) {
        if self.is_mirror {
            self.request_size(size, SizingMode::Normal, animate, None);
            return;
        }

        // Assume that when calling this function, the window is going floating, so it can no
        // longer participate in any transactions with other windows.
        self.transaction_for_next_configure = None;

        self.is_pending_maximized = false;
        if self.is_maximized != self.is_pending_maximized {
            // Make sure we receive a commit to update self.is_maximized later on.
            self.needs_configure = true;
        }

        // If our last requested size already matches the size we want to request-once, clear the
        // size request right away. However, we must also check if we're unfullscreening, because
        // in that case the window itself will restore its previous size upon receiving a (0, 0)
        // configure, whereas what we potentially want is to unfullscreen the window into its
        // fullscreen size.
        let already_sent = with_toplevel_last_uncommitted_configure(self.toplevel(), |configure| {
            let ToplevelConfigure { state, serial } = configure?;

            let same_size = state.size.unwrap_or_default() == size;
            let has_fullscreen = state.states.contains(xdg_toplevel::State::Fullscreen);
            let same_fullscreen = has_fullscreen == self.is_pending_windowed_fullscreen;
            let has_maximized = state.states.contains(xdg_toplevel::State::Maximized);
            let same_maximized = !has_maximized;
            (same_size && same_fullscreen && same_maximized).then_some(*serial)
        });

        if let Some(serial) = already_sent {
            let current_serial = with_states(self.toplevel().wl_surface(), |states| {
                states
                    .cached_state
                    .get::<ToplevelCachedState>()
                    .current()
                    .last_acked
                    .as_ref()
                    .map(|c| c.serial)
            });
            if let Some(current_serial) = current_serial {
                // God this triple negative...
                if !current_serial.is_no_older_than(&serial) {
                    // We have already sent a request for the new size, but the surface has not
                    // committed in response yet, so we will wait for that commit.
                    self.request_size_once = Some(RequestSizeOnce::WaitingForCommit(serial));
                } else {
                    // We have already sent a request for the new size, and the surface has
                    // committed in response, so we will start using the current size right away.
                    self.request_size_once = Some(RequestSizeOnce::UseWindowSize);
                }
            } else {
                warn!("no current serial; did the surface not ack the initial configure?");
                self.request_size_once = Some(RequestSizeOnce::UseWindowSize);
            };
            return;
        }

        let changed = self.toplevel().with_pending_state(|state| {
            let changed = state.size != Some(size);
            state.size = Some(size);
            if !self.is_pending_windowed_fullscreen {
                state.states.unset(xdg_toplevel::State::Fullscreen);
            }
            state.states.unset(xdg_toplevel::State::Maximized);
            changed
        });

        if changed && animate {
            self.animate_next_configure = true;
        }

        self.request_size_once = Some(RequestSizeOnce::WaitingForConfigure);
    }

    fn min_size(&self) -> Size<i32, Logical> {
        if self.is_mirror {
            return Size::from((1, 1));
        }

        let min_size = with_states(self.toplevel().wl_surface(), |state| {
            let mut guard = state.cached_state.get::<SurfaceCachedState>();
            guard.current().min_size
        });

        self.rules.apply_min_size(min_size)
    }

    fn max_size(&self) -> Size<i32, Logical> {
        if self.is_mirror {
            return Size::from((0, 0));
        }

        let max_size = with_states(self.toplevel().wl_surface(), |state| {
            let mut guard = state.cached_state.get::<SurfaceCachedState>();
            guard.current().max_size
        });

        self.rules.apply_max_size(max_size)
    }

    fn is_wl_surface(&self, wl_surface: &WlSurface) -> bool {
        if self.is_mirror {
            return false;
        }

        self.toplevel().wl_surface() == wl_surface
    }

    fn set_preferred_scale_transform(&self, scale: output::Scale, transform: Transform) {
        if self.is_mirror {
            return;
        }

        self.window.with_surfaces(|surface, data| {
            send_scale_transform(surface, data, scale, transform);
        });
    }

    fn has_ssd(&self) -> bool {
        let toplevel = self.toplevel();
        let mode = self
            .toplevel()
            .with_committed_state(|current| current.and_then(|s| s.decoration_mode));

        match mode {
            Some(zxdg_toplevel_decoration_v1::Mode::ServerSide) => true,
            // Check KDE decorations when XDG are not in use.
            None => with_states(toplevel.wl_surface(), |states| {
                states
                    .data_map
                    .get::<KdeDecorationsModeState>()
                    .map(KdeDecorationsModeState::is_server)
                    == Some(true)
            }),
            _ => false,
        }
    }

    fn output_enter(&self, output: &Output) {
        if self.is_mirror {
            let _ = output;
            return;
        }

        let overlap = Rectangle::from_size(Size::from((i32::MAX, i32::MAX)));
        self.window.output_enter(output, overlap)
    }

    fn output_leave(&self, output: &Output) {
        if self.is_mirror {
            let _ = output;
            return;
        }

        self.window.output_leave(output)
    }

    fn set_offscreen_data(&self, data: Option<OffscreenData>) {
        let Some(data) = data else {
            self.offscreen_data.replace(None);
            return;
        };

        let mut offscreen_data = self.offscreen_data.borrow_mut();
        match &mut *offscreen_data {
            None => {
                *offscreen_data = Some(data);
            }
            Some(existing) => {
                // Replace the id, amend existing element states. This is necessary to handle
                // multiple layers of offscreen (e.g. resize animation + alpha animation).
                existing.id = data.id;
                existing.states.states.extend(data.states.states);
            }
        }
    }

    fn is_urgent(&self) -> bool {
        self.is_urgent
    }

    fn set_activated(&mut self, active: bool) {
        if self.is_activated == active {
            return;
        }

        self.is_activated = active;

        let surface = self.toplevel().wl_surface();
        let any_active = update_surface_activated_entries(surface, self.id, active);
        let changed = self.toplevel().with_pending_state(|state| {
            if any_active {
                state.states.set(xdg_toplevel::State::Activated)
            } else {
                state.states.unset(xdg_toplevel::State::Activated)
            }
        });
        self.need_to_recompute_rules = true;
        self.need_to_recompute_rules |= changed;
    }

    fn set_active_in_column(&mut self, active: bool) {
        let changed = self.is_active_in_column != active;
        self.is_active_in_column = active;
        self.need_to_recompute_rules |= changed;
    }

    fn set_floating(&mut self, floating: bool) {
        let changed = self.is_floating != floating;
        self.is_floating = floating;
        self.need_to_recompute_rules |= changed;
    }

    fn set_sticky(&mut self, sticky: bool) {
        let changed = self.is_sticky != sticky;
        self.is_sticky = sticky;
        self.need_to_recompute_rules |= changed;
    }

    fn set_bounds(&self, bounds: Size<i32, Logical>) {
        if self.is_mirror {
            let _ = bounds;
            return;
        }

        self.toplevel().with_pending_state(|state| {
            state.bounds = Some(bounds);
        });
    }

    fn configure_intent(&self) -> ConfigureIntent {
        if self.is_mirror {
            return ConfigureIntent::NotNeeded;
        }

        let _span =
            trace_span!("configure_intent", surface = ?self.toplevel().wl_surface().id()).entered();

        if self.needs_configure {
            trace!("the window needs_configure");
            return ConfigureIntent::ShouldSend;
        }

        with_toplevel_role_and_current(self.toplevel(), |attributes, current_committed| {
            if let Some(server_pending) = &attributes.server_pending {
                let current_server = attributes.current_server_state();
                if *server_pending != current_server {
                    // Something changed. Check if the only difference is the size, and if the
                    // current server size matches the current committed size.
                    let mut current_server_same_size = current_server.clone();
                    current_server_same_size.size = server_pending.size;
                    if current_server_same_size == *server_pending {
                        // Only the size changed. Check if the window committed our previous size
                        // request.
                        let Some(current_committed) = current_committed else {
                            error!("mapped must have had initial commit");
                            return ConfigureIntent::ShouldSend;
                        };

                        if current_committed.size == current_server.size {
                            // The window had committed for our previous size change, so we can
                            // change the size again.
                            trace!(
                                "current size matches server size: {:?}",
                                current_committed.size
                            );
                            ConfigureIntent::CanSend
                        } else {
                            // The window had not committed for our previous size change yet. Since
                            // nothing else changed, do not send the new size request yet. This
                            // throttling is done because some clients do not batch size requests,
                            // leading to bad behavior with very fast input devices (i.e. a 1000 Hz
                            // mouse). This throttling also helps interactive resize transactions
                            // preserve visual consistency.
                            trace!("throttling resize");
                            ConfigureIntent::Throttled
                        }
                    } else {
                        // Something else changed other than the size; send it.
                        trace!("something changed other than the size");
                        ConfigureIntent::ShouldSend
                    }
                } else {
                    // Nothing changed since the last configure.
                    ConfigureIntent::NotNeeded
                }
            } else {
                // Nothing changed since the last configure.
                ConfigureIntent::NotNeeded
            }
        })
    }

    fn send_pending_configure(&mut self) {
        if self.is_mirror {
            return;
        }

        let toplevel = self.toplevel();
        let _span =
            trace_span!("send_pending_configure", surface = ?toplevel.wl_surface().id()).entered();

        // If the window needs a configure, send it regardless.
        let has_pending_changes = self.needs_configure
            || with_toplevel_role(self.toplevel(), |role| {
                // Check for pending changes manually to account for RequestSizeOnce::UseWindowSize.
                if role.server_pending.is_none() {
                    return false;
                }

                let current_server_size = role.current_server_state().size;
                let server_pending = role.server_pending.as_mut().unwrap();

                // With UseWindowSize, we do not consider size-only changes, because we will
                // request the current window size and do not expect it to actually change.
                if let Some(RequestSizeOnce::UseWindowSize) = self.request_size_once {
                    server_pending.size = current_server_size;
                }

                let server_pending = role.server_pending.as_ref().unwrap();
                *server_pending != role.current_server_state()
            });

        if has_pending_changes {
            // If needed, replace the pending size with the current window size.
            if let Some(RequestSizeOnce::UseWindowSize) = self.request_size_once {
                let size = self.window.geometry().size;
                toplevel.with_pending_state(|state| {
                    state.size = Some(size);
                });
            }

            let serial = toplevel.send_configure();
            trace!(?serial, "sending configure");

            self.needs_configure = false;

            // Send the window a frame callback unconditionally to let it respond to size changes
            // and such immediately, even when it's hidden. This especially matters for cases like
            // tabbed columns which compute their width based on all windows in the column, even
            // hidden ones.
            self.needs_frame_callback = true;

            if self.animate_next_configure {
                self.animate_serials.push(serial);
            }

            if let Some(transaction) = self.transaction_for_next_configure.take() {
                self.pending_transactions.push((serial, transaction));
            }

            self.interactive_resize = match self.interactive_resize.take() {
                Some(InteractiveResize::WaitingForLastConfigure(data)) => {
                    Some(InteractiveResize::WaitingForLastCommit { data, serial })
                }
                x => x,
            };

            if let Some(RequestSizeOnce::WaitingForConfigure) = self.request_size_once {
                self.request_size_once = Some(RequestSizeOnce::WaitingForCommit(serial));
            }

            // If is_pending_windowed_fullscreen changed compared to the last value that we "sent"
            // to the window, store the configure serial.
            let last_sent_windowed_fullscreen = self
                .uncommitted_windowed_fullscreen
                .last()
                .map(|(_, value)| *value)
                .unwrap_or(self.is_windowed_fullscreen);
            if last_sent_windowed_fullscreen != self.is_pending_windowed_fullscreen {
                self.uncommitted_windowed_fullscreen
                    .push((serial, self.is_pending_windowed_fullscreen));
            }

            // If is_pending_maximized changed compared to the last value that we "sent" to the
            // window, store the configure serial.
            let last_sent_maximized = self
                .uncommitted_maximized
                .last()
                .map(|(_, value)| *value)
                .unwrap_or(self.is_maximized);
            if last_sent_maximized != self.is_pending_maximized {
                self.uncommitted_maximized
                    .push((serial, self.is_pending_maximized));
            }
        } else {
            self.interactive_resize = match self.interactive_resize.take() {
                // We probably started and stopped resizing in the same loop cycle without anything
                // changing.
                Some(InteractiveResize::WaitingForLastConfigure { .. }) => None,
                x => x,
            };
        }

        self.animate_next_configure = false;
        self.transaction_for_next_configure = None;
    }

    fn sizing_mode(&self) -> SizingMode {
        if self.is_mirror {
            return self.mirror_sizing_mode;
        }

        if self.is_windowed_fullscreen {
            return if self.is_maximized {
                SizingMode::Maximized
            } else {
                SizingMode::Normal
            };
        }

        self.toplevel().with_committed_state(|state| {
            // This must always be Some() for mapped windows. However, this function is called on
            // the code path when removing a just-unmapped window in the commit handler, at which
            // point state is already None.
            let Some(state) = state else {
                return SizingMode::Normal;
            };

            if state.states.contains(xdg_toplevel::State::Fullscreen) {
                SizingMode::Fullscreen
            } else if state.states.contains(xdg_toplevel::State::Maximized) {
                SizingMode::Maximized
            } else {
                SizingMode::Normal
            }
        })
    }

    fn pending_sizing_mode(&self) -> SizingMode {
        if self.is_mirror {
            return self.mirror_pending_sizing_mode;
        }

        if self.is_pending_windowed_fullscreen {
            return if self.is_pending_maximized {
                SizingMode::Maximized
            } else {
                SizingMode::Normal
            };
        }

        self.toplevel().with_pending_state(|state| {
            if state.states.contains(xdg_toplevel::State::Fullscreen) {
                SizingMode::Fullscreen
            } else if state.states.contains(xdg_toplevel::State::Maximized) {
                SizingMode::Maximized
            } else {
                SizingMode::Normal
            }
        })
    }

    fn is_ignoring_opacity_window_rule(&self) -> bool {
        self.ignore_opacity_window_rule
    }

    fn effective_block_out_from(&self) -> Option<BlockOutFrom> {
        Mapped::effective_block_out_from(self)
    }

    fn requested_size(&self) -> Option<Size<i32, Logical>> {
        if self.is_mirror {
            return Some(self.mirror_size);
        }

        self.toplevel().with_pending_state(|state| state.size)
    }

    fn expected_size(&self) -> Option<Size<i32, Logical>> {
        if self.is_mirror {
            return Some(self.mirror_size);
        }

        // We can only use current size if it's not maximized or fullscreen.
        let current_size = (self.sizing_mode().is_normal()).then(|| self.window.geometry().size);

        // Check if we should be using the current window size.
        //
        // This branch can be useful (give different result than the logic below) in this example
        // case:
        //
        // 1. We request_size_once a size change.
        // 2. We send a second configure requesting a state change.
        // 3. The window acks and commits-to the first configure but not the second, with a
        //    different size.
        //
        // In this case self.request_size_once will already flip to UseWindowSize and this branch
        // will return the window's own new size, but the logic below would see an uncommitted size
        // change and return our size.
        if let Some(RequestSizeOnce::UseWindowSize) = self.request_size_once {
            return current_size;
        }

        let pending = with_states(self.toplevel().wl_surface(), |states| {
            let role = states
                .data_map
                .get::<XdgToplevelSurfaceData>()
                .unwrap()
                .lock()
                .unwrap();

            // If we have a server-pending size change that we haven't sent yet, use that size.
            let server_pending = role.server_pending.as_ref()?;

            let current_server = role.current_server_state();
            if server_pending.size != current_server.size {
                return Some((
                    server_pending.size.unwrap_or_default(),
                    server_pending
                        .states
                        .contains(xdg_toplevel::State::Fullscreen),
                    server_pending
                        .states
                        .contains(xdg_toplevel::State::Maximized),
                ));
            }

            None
        })
        .or_else(|| {
            with_toplevel_last_uncommitted_configure(self.toplevel(), |configure| {
                // If we have a sent-but-not-committed-to size, use that.
                let ToplevelConfigure { state, .. } = configure?;

                Some((
                    state.size.unwrap_or_default(),
                    state.states.contains(xdg_toplevel::State::Fullscreen),
                    state.states.contains(xdg_toplevel::State::Maximized),
                ))
            })
        });

        if let Some((mut size, fullscreen, maximized)) = pending {
            // If the pending change is maximized or fullscreen, we can't use that size.
            //
            // Pending windowed fullscreen is good (means not real fullscreen), unless it's also
            // pending maximized (means maximized windowed fullscreen, so maximized size, bad).
            if maximized
                || (fullscreen
                    && (!self.is_pending_windowed_fullscreen || self.is_pending_maximized))
            {
                return None;
            }

            // If some component of the pending size is zero, substitute it with the current window
            // size. But only if the current size is not fullscreen.
            if size.w == 0 {
                size.w = current_size?.w;
            }
            if size.h == 0 {
                size.h = current_size?.h;
            }

            Some(size)
        } else {
            // No pending size, return the current size if it's non-fullscreen.
            current_size
        }
    }

    fn is_windowed_fullscreen(&self) -> bool {
        if self.is_mirror {
            return self.mirror_sizing_mode.is_fullscreen();
        }

        self.is_windowed_fullscreen
    }

    fn is_pending_windowed_fullscreen(&self) -> bool {
        if self.is_mirror {
            return self.mirror_pending_sizing_mode.is_fullscreen();
        }

        self.is_pending_windowed_fullscreen
    }

    fn request_windowed_fullscreen(&mut self, value: bool) {
        if self.is_mirror {
            let mode = if value {
                SizingMode::Fullscreen
            } else {
                SizingMode::Normal
            };
            self.mirror_sizing_mode = mode;
            self.mirror_pending_sizing_mode = mode;
            return;
        }

        if self.is_pending_windowed_fullscreen == value {
            return;
        }

        self.is_pending_windowed_fullscreen = value;

        // Set the fullscreen state to match.
        //
        // When going from windowed to real fullscreen, we'll use request_size() which will set the
        // fullscreen state back.
        self.toplevel().with_pending_state(|state| {
            if value {
                state.states.set(xdg_toplevel::State::Fullscreen);
                state.states.unset(xdg_toplevel::State::Maximized);
            } else {
                state.states.unset(xdg_toplevel::State::Fullscreen);

                if self.is_pending_maximized {
                    state.states.set(xdg_toplevel::State::Maximized);
                }
            }
        });

        // Make sure we receive a commit later to update self.is_windowed_fullscreen.
        self.needs_configure = true;
    }

    fn is_child_of(&self, parent: &Self) -> bool {
        if self.is_mirror || parent.is_mirror {
            return false;
        }

        self.toplevel().parent().as_ref() == Some(parent.toplevel().wl_surface())
    }

    fn refresh(&self) {
        if self.is_mirror {
            return;
        }

        self.window.refresh();
    }

    fn rules(&self) -> &ResolvedWindowRules {
        &self.rules
    }

    fn take_animation_snapshot(&mut self) -> Option<LayoutElementRenderSnapshot> {
        self.animation_snapshot.take()
    }

    fn set_interactive_resize(&mut self, data: Option<InteractiveResizeData>) {
        if self.is_mirror {
            self.interactive_resize = data.map(InteractiveResize::Ongoing);
            return;
        }

        self.toplevel().with_pending_state(|state| {
            if data.is_some() {
                state.states.set(xdg_toplevel::State::Resizing);
            } else {
                state.states.unset(xdg_toplevel::State::Resizing);
            }
        });

        if let Some(data) = data {
            self.interactive_resize = Some(InteractiveResize::Ongoing(data));
        } else {
            self.interactive_resize = match self.interactive_resize.take() {
                Some(InteractiveResize::Ongoing(data)) => {
                    Some(InteractiveResize::WaitingForLastConfigure(data))
                }
                x => x,
            }
        }
    }

    fn cancel_interactive_resize(&mut self) {
        self.set_interactive_resize(None);
        self.interactive_resize = None;
    }

    fn interactive_resize_data(&self) -> Option<InteractiveResizeData> {
        Some(self.interactive_resize.as_ref()?.data())
    }

    fn on_commit(&mut self, commit_serial: Serial) {
        if self.is_mirror {
            let _ = commit_serial;
            return;
        }

        if let Some(InteractiveResize::WaitingForLastCommit { serial, .. }) =
            &self.interactive_resize
        {
            if commit_serial.is_no_older_than(serial) {
                self.interactive_resize = None;
            }
        }

        if let Some(RequestSizeOnce::WaitingForCommit(serial)) = &self.request_size_once {
            if commit_serial.is_no_older_than(serial) {
                self.request_size_once = Some(RequestSizeOnce::UseWindowSize);
            }
        }

        // "Commit" our "acked" pending windowed fullscreen state.
        self.uncommitted_windowed_fullscreen
            .retain_mut(|(serial, value)| {
                if commit_serial.is_no_older_than(serial) {
                    self.is_windowed_fullscreen = *value;
                    false
                } else {
                    true
                }
            });

        // "Commit" our "acked" pending maximized state.
        self.uncommitted_maximized.retain_mut(|(serial, value)| {
            if commit_serial.is_no_older_than(serial) {
                self.is_maximized = *value;
                false
            } else {
                true
            }
        });
    }
}
