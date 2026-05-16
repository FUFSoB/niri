use std::f64::consts::LN_2;
use std::time::Duration;

use smithay::input::pointer::{
    AxisFrame, ButtonEvent, GestureHoldBeginEvent, GestureHoldEndEvent, GesturePinchBeginEvent,
    GesturePinchEndEvent, GesturePinchUpdateEvent, GestureSwipeBeginEvent, GestureSwipeEndEvent,
    GestureSwipeUpdateEvent, GrabStartData as PointerGrabStartData, MotionEvent, PointerGrab,
    PointerInnerHandle, RelativeMotionEvent,
};
use smithay::input::SeatHandler;
use smithay::utils::{Logical, Point, SERIAL_COUNTER};

use crate::niri::State;
use crate::utils::get_monotonic_time;
use crate::window::mapped::MappedId;

const MIRROR_ZOOM_PIXELS_PER_OCTAVE: f64 = 120.;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MirrorViewGrabMode {
    Pan,
    Zoom,
}

pub struct MirrorViewGrab {
    start_data: PointerGrabStartData<State>,
    window: MappedId,
    mode: MirrorViewGrabMode,
    start_mirror_local: Point<f64, Logical>,
    start_source_point: Point<f64, Logical>,
    start_zoom: f64,

    // Updated from absolute or relative motion, then applied in frame().
    new_location: Point<f64, Logical>,
    event_timestamp: Option<Duration>,
}

impl MirrorViewGrab {
    pub fn new(
        start_data: PointerGrabStartData<State>,
        window: MappedId,
        mode: MirrorViewGrabMode,
        start_mirror_local: Point<f64, Logical>,
        start_source_point: Point<f64, Logical>,
        start_zoom: f64,
    ) -> Self {
        Self {
            new_location: start_data.location,
            event_timestamp: None,
            start_data,
            window,
            mode,
            start_mirror_local,
            start_source_point,
            start_zoom,
        }
    }

    fn zoom_for_delta_y(start_zoom: f64, delta_y: f64, max_zoom: f64) -> f64 {
        let zoom = start_zoom * (-delta_y * LN_2 / MIRROR_ZOOM_PIXELS_PER_OCTAVE).exp();
        zoom.clamp(1., max_zoom)
    }

    fn on_frame(&mut self, data: &mut State) -> bool {
        let Some(_timestamp) = self.event_timestamp.take() else {
            return true;
        };

        let delta = self.new_location - self.start_data.location;
        let mirror_local = self.start_mirror_local + delta;
        let max_zoom = data.niri.config.borrow().zoom.max_zoom;

        let mut updated = false;
        data.niri.layout.with_windows_mut(|mapped, _| {
            if mapped.id() != self.window {
                return;
            }

            let zoom = match self.mode {
                MirrorViewGrabMode::Pan => self.start_zoom,
                MirrorViewGrabMode::Zoom => {
                    Self::zoom_for_delta_y(self.start_zoom, delta.y, max_zoom)
                }
            };
            mapped.set_mirror_view_from_anchor(zoom, mirror_local, self.start_source_point);
            updated = true;
        });

        if updated {
            data.niri.queue_redraw_all();
        }

        updated
    }
}

impl PointerGrab<State> for MirrorViewGrab {
    fn motion(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        _focus: Option<(<State as SeatHandler>::PointerFocus, Point<f64, Logical>)>,
        event: &MotionEvent,
    ) {
        // While the grab is active, no client has pointer focus.
        handle.motion(data, None, event);

        self.new_location = event.location;
        self.event_timestamp = Some(Duration::from_millis(u64::from(event.time)));
    }

    fn relative_motion(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        _focus: Option<(<State as SeatHandler>::PointerFocus, Point<f64, Logical>)>,
        event: &RelativeMotionEvent,
    ) {
        // While the grab is active, no client has pointer focus.
        handle.relative_motion(data, None, event);

        self.new_location += event.delta;
        self.event_timestamp = Some(Duration::from_micros(event.utime));
    }

    fn button(
        &mut self,
        data: &mut State,
        handle: &mut PointerInnerHandle<'_, State>,
        event: &ButtonEvent,
    ) {
        handle.button(data, event);

        if !handle.current_pressed().contains(&self.start_data.button) {
            handle.unset_grab(self, data, event.serial, event.time, true);
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

        if !self.on_frame(data) {
            handle.unset_grab(
                self,
                data,
                SERIAL_COUNTER.next_serial(),
                get_monotonic_time().as_millis() as u32,
                true,
            );
        }
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
    use super::MirrorViewGrab;

    #[test]
    fn zoom_for_delta_y_is_exponential() {
        assert_eq!(MirrorViewGrab::zoom_for_delta_y(2., -120., 8.), 4.);
        assert_eq!(MirrorViewGrab::zoom_for_delta_y(2., 120., 8.), 1.);
    }

    #[test]
    fn zoom_for_delta_y_clamps_to_max() {
        assert_eq!(MirrorViewGrab::zoom_for_delta_y(4., -120., 6.), 6.);
    }
}
