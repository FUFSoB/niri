use smithay::input::pointer::{
    AxisFrame, ButtonEvent, GestureHoldBeginEvent, GestureHoldEndEvent, GesturePinchBeginEvent,
    GesturePinchEndEvent, GesturePinchUpdateEvent, GestureSwipeBeginEvent, GestureSwipeEndEvent,
    GestureSwipeUpdateEvent, GrabStartData as PointerGrabStartData, MotionEvent, PointerGrab,
    PointerInnerHandle, RelativeMotionEvent,
};
use smithay::input::touch::{
    DownEvent, GrabStartData as TouchGrabStartData, MotionEvent as TouchMotionEvent,
    OrientationEvent, ShapeEvent, TouchGrab, TouchInnerHandle, UpEvent,
};
use smithay::input::SeatHandler;
use smithay::utils::{Logical, Point, Size, Transform};

use crate::niri::State;

fn transform_relative_delta(
    delta: Point<f64, Logical>,
    mirror_scale: f64,
    inverse_transform: Transform,
) -> Point<f64, Logical> {
    inverse_transform.transform_point_in(delta.downscale(mirror_scale), &Size::from((0., 0.)))
}

/// Returns the focus origin that maps `location` to the right surface-local point inside a
/// mirror, given the surface-local point at `start_location`.
fn focus_origin_for_location(
    start_location: Point<f64, Logical>,
    start_surface_local: Point<f64, Logical>,
    mirror_scale: f64,
    inverse_transform: Transform,
    location: Point<f64, Logical>,
) -> Point<f64, Logical> {
    let surface_local = start_surface_local
        + transform_relative_delta(location - start_location, mirror_scale, inverse_transform);
    location - surface_local
}

pub struct MirrorClickGrab {
    start_data: PointerGrabStartData<State>,
    start_surface_local: Point<f64, Logical>,
    mirror_scale: f64,
    inverse_transform: Transform,
}

impl MirrorClickGrab {
    pub fn new(
        start_data: PointerGrabStartData<State>,
        start_surface_local: Point<f64, Logical>,
        mirror_scale: f64,
        inverse_transform: Transform,
    ) -> Self {
        Self {
            start_data,
            start_surface_local,
            mirror_scale,
            inverse_transform,
        }
    }

    fn focus_for_location(
        &self,
        location: Point<f64, Logical>,
    ) -> Option<(<State as SeatHandler>::PointerFocus, Point<f64, Logical>)> {
        let (focus, _) = self.start_data.focus.as_ref()?;
        let origin = focus_origin_for_location(
            self.start_data.location,
            self.start_surface_local,
            self.mirror_scale,
            self.inverse_transform,
            location,
        );
        Some((focus.clone(), origin))
    }
}

impl PointerGrab<State> for MirrorClickGrab {
    fn motion(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        _focus: Option<(<State as SeatHandler>::PointerFocus, Point<f64, Logical>)>,
        event: &MotionEvent,
    ) {
        handle.motion(data, self.focus_for_location(event.location), event);
    }

    fn relative_motion(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        _focus: Option<(<State as SeatHandler>::PointerFocus, Point<f64, Logical>)>,
        event: &RelativeMotionEvent,
    ) {
        let mut event = event.clone();
        event.delta =
            transform_relative_delta(event.delta, self.mirror_scale, self.inverse_transform);
        event.delta_unaccel = transform_relative_delta(
            event.delta_unaccel,
            self.mirror_scale,
            self.inverse_transform,
        );

        handle.relative_motion(data, self.start_data.focus.clone(), &event);
    }

    fn button(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &ButtonEvent,
    ) {
        handle.button(data, event);

        if handle.current_pressed().is_empty() {
            handle.unset_grab(self, data, event.serial, event.time, false);
        }
    }

    fn axis(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        details: AxisFrame,
    ) {
        handle.axis(data, details);
    }

    fn frame(&mut self, data: &mut State, handle: &mut PointerInnerHandle<'_, State>) {
        handle.frame(data);
    }

    fn gesture_swipe_begin(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &GestureSwipeBeginEvent,
    ) {
        handle.gesture_swipe_begin(data, event);
    }

    fn gesture_swipe_update(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &GestureSwipeUpdateEvent,
    ) {
        handle.gesture_swipe_update(data, event);
    }

    fn gesture_swipe_end(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &GestureSwipeEndEvent,
    ) {
        handle.gesture_swipe_end(data, event);
    }

    fn gesture_pinch_begin(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &GesturePinchBeginEvent,
    ) {
        handle.gesture_pinch_begin(data, event);
    }

    fn gesture_pinch_update(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &GesturePinchUpdateEvent,
    ) {
        handle.gesture_pinch_update(data, event);
    }

    fn gesture_pinch_end(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &GesturePinchEndEvent,
    ) {
        handle.gesture_pinch_end(data, event);
    }

    fn gesture_hold_begin(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &GestureHoldBeginEvent,
    ) {
        handle.gesture_hold_begin(data, event);
    }

    fn gesture_hold_end(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &GestureHoldEndEvent,
    ) {
        handle.gesture_hold_end(data, event);
    }

    fn start_data(&self) -> &PointerGrabStartData<State> {
        &self.start_data
    }

    fn unset(&mut self, _data: &mut State) {}
}

/// Touch counterpart of [`MirrorClickGrab`]: Smithay's `TouchDownGrab`, but with the focus origin
/// mapped through the mirror's scale and transform, so drags inside the mirror land on the right
/// surface-local points.
pub struct MirrorTouchDownGrab {
    start_data: TouchGrabStartData<State>,
    start_surface_local: Point<f64, Logical>,
    mirror_scale: f64,
    inverse_transform: Transform,
    touch_points: usize,
}

impl MirrorTouchDownGrab {
    pub fn new(
        start_data: TouchGrabStartData<State>,
        start_surface_local: Point<f64, Logical>,
        mirror_scale: f64,
        inverse_transform: Transform,
    ) -> Self {
        Self {
            start_data,
            start_surface_local,
            mirror_scale,
            inverse_transform,
            touch_points: 1,
        }
    }

    fn focus_for_location(
        &self,
        location: Point<f64, Logical>,
    ) -> Option<(<State as SeatHandler>::TouchFocus, Point<f64, Logical>)> {
        let (focus, _) = self.start_data.focus.as_ref()?;
        let origin = focus_origin_for_location(
            self.start_data.location,
            self.start_surface_local,
            self.mirror_scale,
            self.inverse_transform,
            location,
        );
        Some((focus.clone(), origin))
    }
}

impl TouchGrab<State> for MirrorTouchDownGrab {
    fn down(
        &mut self,
        data: &mut State,
        handle: &mut TouchInnerHandle<'_, State>,
        _focus: Option<(<State as SeatHandler>::TouchFocus, Point<f64, Logical>)>,
        event: &DownEvent,
    ) {
        handle.down(data, self.focus_for_location(event.location), event);
        self.touch_points += 1;
    }

    fn up(&mut self, data: &mut State, handle: &mut TouchInnerHandle<'_, State>, event: &UpEvent) {
        handle.up(data, event);
        self.touch_points = self.touch_points.saturating_sub(1);
        if self.touch_points == 0 {
            handle.unset_grab(self, data);
        }
    }

    fn motion(
        &mut self,
        data: &mut State,
        handle: &mut TouchInnerHandle<'_, State>,
        _focus: Option<(<State as SeatHandler>::TouchFocus, Point<f64, Logical>)>,
        event: &TouchMotionEvent,
    ) {
        handle.motion(data, self.focus_for_location(event.location), event);
    }

    fn frame(&mut self, data: &mut State, handle: &mut TouchInnerHandle<'_, State>) {
        handle.frame(data);
    }

    fn cancel(&mut self, data: &mut State, handle: &mut TouchInnerHandle<'_, State>) {
        handle.cancel(data);
        handle.unset_grab(self, data);
    }

    fn shape(
        &mut self,
        data: &mut State,
        handle: &mut TouchInnerHandle<'_, State>,
        event: &ShapeEvent,
    ) {
        handle.shape(data, event);
    }

    fn orientation(
        &mut self,
        data: &mut State,
        handle: &mut TouchInnerHandle<'_, State>,
        event: &OrientationEvent,
    ) {
        handle.orientation(data, event);
    }

    fn start_data(&self) -> &TouchGrabStartData<State> {
        &self.start_data
    }

    fn unset(&mut self, _data: &mut State) {}
}

#[cfg(test)]
mod tests {
    use smithay::utils::{Point, Transform};

    use super::{focus_origin_for_location, transform_relative_delta};

    #[test]
    fn mirror_relative_delta_rotates_with_transform() {
        let delta = transform_relative_delta(Point::from((4., 8.)), 2., Transform::_270);

        assert_eq!(delta, Point::from((4., -2.)));
    }

    #[test]
    fn mirror_relative_delta_flips_and_scales_with_transform() {
        let delta = transform_relative_delta(Point::from((6., 10.)), 2., Transform::Flipped);

        assert_eq!(delta, Point::from((-3., 5.)));
    }

    #[test]
    fn mirror_focus_origin_tracks_scaled_motion() {
        let start_location = Point::from((100., 100.));
        let start_surface_local = Point::from((20., 10.));

        let origin = focus_origin_for_location(
            start_location,
            start_surface_local,
            2.,
            Transform::Normal,
            Point::from((106., 100.)),
        );

        assert_eq!(origin, Point::from((83., 90.)));
    }

    #[test]
    fn mirror_focus_origin_matches_unscaled_motion_at_scale_one() {
        let start_location = Point::from((100., 100.));
        let start_surface_local = Point::from((20., 10.));

        let origin = focus_origin_for_location(
            start_location,
            start_surface_local,
            1.,
            Transform::Normal,
            Point::from((106., 104.)),
        );

        assert_eq!(origin, Point::from((80., 90.)));
    }
}
