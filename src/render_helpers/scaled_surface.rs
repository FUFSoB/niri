use std::marker::PhantomData;

use smithay::backend::renderer::element::surface::WaylandSurfaceRenderElement;
use smithay::backend::renderer::element::{
    Element, Id, Kind, NamespacedElement, RenderElement, UnderlyingStorage,
};
use smithay::backend::renderer::utils::{CommitCounter, DamageSet, OpaqueRegions};
use smithay::utils::user_data::UserDataMap;
use smithay::utils::{Buffer, Physical, Point, Rectangle, Scale, Size, Transform};

use super::clipped_surface::ClippedSurfaceInner;
use super::renderer::NiriRenderer;

#[derive(Debug)]
pub struct ScaledWaylandSurfaceRenderElement<
    R: NiriRenderer,
    E: ClippedSurfaceInner<R> = NamespacedElement<WaylandSurfaceRenderElement<R>>,
> {
    inner: E,
    _renderer: PhantomData<R>,
    origin: Point<i32, Physical>,
    element_scale: Scale<f64>,
}

pub type NamespacedScaledWaylandSurfaceRenderElement<R> =
    ScaledWaylandSurfaceRenderElement<R, NamespacedElement<WaylandSurfaceRenderElement<R>>>;
pub type NamespacedScaledTransformedWaylandSurfaceRenderElement<R> =
    ScaledWaylandSurfaceRenderElement<R, NamespacedTransformedWaylandSurfaceRenderElement<R>>;

#[derive(Debug)]
pub struct TransformedWaylandSurfaceRenderElement<
    R: NiriRenderer,
    E: ClippedSurfaceInner<R> = NamespacedElement<WaylandSurfaceRenderElement<R>>,
> {
    inner: E,
    _renderer: PhantomData<R>,
    origin: Point<i32, Physical>,
    area: Size<i32, Physical>,
    element_transform: Transform,
}

pub type NamespacedTransformedWaylandSurfaceRenderElement<R> =
    TransformedWaylandSurfaceRenderElement<R, NamespacedElement<WaylandSurfaceRenderElement<R>>>;

fn transform_vector(transform: Transform, vector: Point<i32, Physical>) -> Point<i32, Physical> {
    transform.transform_point_in(vector, &Size::from((0, 0)))
}

fn compose_transforms(first: Transform, second: Transform) -> Transform {
    let x = transform_vector(second, transform_vector(first, Point::from((1, 0))));
    let y = transform_vector(second, transform_vector(first, Point::from((0, 1))));

    match (x, y) {
        (Point { x: 1, y: 0, .. }, Point { x: 0, y: 1, .. }) => Transform::Normal,
        (Point { x: 0, y: 1, .. }, Point { x: -1, y: 0, .. }) => Transform::_90,
        (Point { x: -1, y: 0, .. }, Point { x: 0, y: -1, .. }) => Transform::_180,
        (Point { x: 0, y: -1, .. }, Point { x: 1, y: 0, .. }) => Transform::_270,
        (Point { x: -1, y: 0, .. }, Point { x: 0, y: 1, .. }) => Transform::Flipped,
        (Point { x: 0, y: 1, .. }, Point { x: 1, y: 0, .. }) => Transform::Flipped90,
        (Point { x: 1, y: 0, .. }, Point { x: 0, y: -1, .. }) => Transform::Flipped180,
        (Point { x: 0, y: -1, .. }, Point { x: -1, y: 0, .. }) => Transform::Flipped270,
        _ => unreachable!("unexpected transform composition"),
    }
}

impl<R: NiriRenderer, E: ClippedSurfaceInner<R>> ScaledWaylandSurfaceRenderElement<R, E> {
    pub fn new(
        inner: E,
        origin: Point<i32, Physical>,
        element_scale: impl Into<Scale<f64>>,
    ) -> Self {
        Self {
            inner,
            _renderer: PhantomData,
            origin,
            element_scale: element_scale.into(),
        }
    }
}

impl<R: NiriRenderer, E: ClippedSurfaceInner<R>> TransformedWaylandSurfaceRenderElement<R, E> {
    pub fn new(
        inner: E,
        origin: Point<i32, Physical>,
        area: Size<i32, Physical>,
        element_transform: Transform,
    ) -> Self {
        Self {
            inner,
            _renderer: PhantomData,
            origin,
            area,
            element_transform,
        }
    }
}

impl<R: NiriRenderer, E: ClippedSurfaceInner<R>> ClippedSurfaceInner<R>
    for ScaledWaylandSurfaceRenderElement<R, E>
{
    fn surface_render_element(&self) -> &WaylandSurfaceRenderElement<R> {
        self.inner.surface_render_element()
    }

    fn sample_transform(&self) -> Transform {
        self.inner.sample_transform()
    }
}

impl<R: NiriRenderer, E: ClippedSurfaceInner<R>> ClippedSurfaceInner<R>
    for TransformedWaylandSurfaceRenderElement<R, E>
{
    fn surface_render_element(&self) -> &WaylandSurfaceRenderElement<R> {
        self.inner.surface_render_element()
    }

    fn sample_transform(&self) -> Transform {
        compose_transforms(
            self.element_transform.invert(),
            self.inner.sample_transform(),
        )
    }
}

impl<R: NiriRenderer, E: ClippedSurfaceInner<R>> Element
    for ScaledWaylandSurfaceRenderElement<R, E>
{
    fn id(&self) -> &Id {
        self.inner.id()
    }

    fn current_commit(&self) -> CommitCounter {
        self.inner.current_commit()
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        self.inner.src()
    }

    fn geometry(&self, scale: Scale<f64>) -> Rectangle<i32, Physical> {
        let mut geometry = self.inner.geometry(scale);
        geometry.loc -= self.origin;
        geometry = geometry.to_f64().upscale(self.element_scale).to_i32_round();
        geometry.loc += self.origin;
        geometry
    }

    fn transform(&self) -> Transform {
        self.inner.transform()
    }

    fn damage_since(
        &self,
        scale: Scale<f64>,
        commit: Option<CommitCounter>,
    ) -> DamageSet<i32, Physical> {
        self.inner
            .damage_since(scale, commit)
            .into_iter()
            .map(|rect| rect.to_f64().upscale(self.element_scale).to_i32_up())
            .collect()
    }

    fn opaque_regions(&self, scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        self.inner
            .opaque_regions(scale)
            .into_iter()
            .map(|rect| rect.to_f64().upscale(self.element_scale).to_i32_round())
            .collect()
    }

    fn alpha(&self) -> f32 {
        self.inner.alpha()
    }

    fn kind(&self) -> Kind {
        self.inner.kind()
    }

    fn is_framebuffer_effect(&self) -> bool {
        self.inner.is_framebuffer_effect()
    }
}

impl<R: NiriRenderer, E: ClippedSurfaceInner<R>> Element
    for TransformedWaylandSurfaceRenderElement<R, E>
{
    fn id(&self) -> &Id {
        self.inner.id()
    }

    fn current_commit(&self) -> CommitCounter {
        self.inner.current_commit()
    }

    fn src(&self) -> Rectangle<f64, Buffer> {
        self.inner.src()
    }

    fn geometry(&self, scale: Scale<f64>) -> Rectangle<i32, Physical> {
        let mut geometry = self.inner.geometry(scale);
        geometry.loc -= self.origin;
        geometry = self
            .element_transform
            .transform_rect_in(geometry, &self.area);
        geometry.loc += self.origin;
        geometry
    }

    fn transform(&self) -> Transform {
        self.inner.transform()
    }

    fn damage_since(
        &self,
        scale: Scale<f64>,
        commit: Option<CommitCounter>,
    ) -> DamageSet<i32, Physical> {
        let inner_size = self.inner.geometry(scale).size;
        self.inner
            .damage_since(scale, commit)
            .into_iter()
            .map(|rect| self.element_transform.transform_rect_in(rect, &inner_size))
            .collect()
    }

    fn opaque_regions(&self, scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        let inner_size = self.inner.geometry(scale).size;
        self.inner
            .opaque_regions(scale)
            .into_iter()
            .map(|rect| self.element_transform.transform_rect_in(rect, &inner_size))
            .collect()
    }

    fn alpha(&self) -> f32 {
        self.inner.alpha()
    }

    fn kind(&self) -> Kind {
        self.inner.kind()
    }

    fn is_framebuffer_effect(&self) -> bool {
        self.inner.is_framebuffer_effect()
    }
}

impl<R: NiriRenderer, E: ClippedSurfaceInner<R> + RenderElement<R>> RenderElement<R>
    for ScaledWaylandSurfaceRenderElement<R, E>
{
    fn draw(
        &self,
        frame: &mut R::Frame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
        cache: Option<&UserDataMap>,
    ) -> Result<(), R::Error> {
        self.inner
            .draw(frame, src, dst, damage, opaque_regions, cache)
    }

    fn underlying_storage(&self, renderer: &mut R) -> Option<UnderlyingStorage<'_>> {
        self.inner.underlying_storage(renderer)
    }

    fn capture_framebuffer(
        &self,
        frame: &mut R::Frame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        cache: &UserDataMap,
    ) -> Result<(), R::Error> {
        self.inner.capture_framebuffer(frame, src, dst, cache)
    }
}

impl<R: NiriRenderer, E: ClippedSurfaceInner<R> + RenderElement<R>> RenderElement<R>
    for TransformedWaylandSurfaceRenderElement<R, E>
{
    fn draw(
        &self,
        frame: &mut R::Frame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
        cache: Option<&UserDataMap>,
    ) -> Result<(), R::Error> {
        self.inner
            .draw(frame, src, dst, damage, opaque_regions, cache)
    }

    fn underlying_storage(&self, renderer: &mut R) -> Option<UnderlyingStorage<'_>> {
        self.inner.underlying_storage(renderer)
    }

    fn capture_framebuffer(
        &self,
        frame: &mut R::Frame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        cache: &UserDataMap,
    ) -> Result<(), R::Error> {
        self.inner.capture_framebuffer(frame, src, dst, cache)
    }
}
