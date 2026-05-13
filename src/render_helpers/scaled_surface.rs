use std::marker::PhantomData;

use smithay::backend::renderer::element::surface::WaylandSurfaceRenderElement;
use smithay::backend::renderer::element::{
    Element, Id, Kind, NamespacedElement, RenderElement, UnderlyingStorage,
};
use smithay::backend::renderer::utils::{CommitCounter, DamageSet, OpaqueRegions};
use smithay::utils::user_data::UserDataMap;
use smithay::utils::{Buffer, Physical, Point, Rectangle, Scale, Transform};

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

impl<R: NiriRenderer, E: ClippedSurfaceInner<R>> ClippedSurfaceInner<R>
    for ScaledWaylandSurfaceRenderElement<R, E>
{
    fn surface_render_element(&self) -> &WaylandSurfaceRenderElement<R> {
        self.inner.surface_render_element()
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
