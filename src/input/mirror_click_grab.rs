use smithay::input::pointer::{
    AxisFrame, ButtonEvent, GestureHoldBeginEvent, GestureHoldEndEvent, GesturePinchBeginEvent,
    GesturePinchEndEvent, GesturePinchUpdateEvent, GestureSwipeBeginEvent, GestureSwipeEndEvent,
    GestureSwipeUpdateEvent, GrabStartData as PointerGrabStartData, MotionEvent, PointerGrab,
    PointerInnerHandle, RelativeMotionEvent,
};
use smithay::input::SeatHandler;
use smithay::utils::{Logical, Point};

use crate::niri::State;

pub struct MirrorClickGrab {
    start_data: PointerGrabStartData<State>,
    start_surface_local: Point<f64, Logical>,
    mirror_scale: f64,
}

impl MirrorClickGrab {
    pub fn new(
        start_data: PointerGrabStartData<State>,
        start_surface_local: Point<f64, Logical>,
        mirror_scale: f64,
    ) -> Self {
        Self {
            start_data,
            start_surface_local,
            mirror_scale,
        }
    }

    fn focus_for_location(
        &self,
        location: Point<f64, Logical>,
    ) -> Option<(<State as SeatHandler>::PointerFocus, Point<f64, Logical>)> {
        let (focus, _) = self.start_data.focus.as_ref()?;
        let surface_local = self.start_surface_local
            + (location - self.start_data.location).downscale(self.mirror_scale);
        Some((focus.clone(), location - surface_local))
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
        handle.relative_motion(data, self.start_data.focus.clone(), event);
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

#[cfg(test)]
mod tests {
    use smithay::utils::{Logical, Point};

    fn focus_origin_for_location(
        start_location: Point<f64, Logical>,
        start_surface_local: Point<f64, Logical>,
        mirror_scale: f64,
        location: Point<f64, Logical>,
    ) -> Point<f64, Logical> {
        let surface_local =
            start_surface_local + (location - start_location).downscale(mirror_scale);
        location - surface_local
    }

    #[test]
    fn mirror_focus_origin_tracks_scaled_motion() {
        let start_location = Point::from((100., 100.));
        let start_surface_local = Point::from((20., 10.));

        let origin = focus_origin_for_location(
            start_location,
            start_surface_local,
            2.,
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
            Point::from((106., 104.)),
        );

        assert_eq!(origin, Point::from((80., 90.)));
    }
}
