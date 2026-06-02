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

        let mut redraw_output = None;
        let mut updated = false;
        data.niri.layout.with_windows_mut(|mapped, output| {
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
            redraw_output = output.cloned();
            updated = true;
        });

        if let Some(output) = redraw_output {
            data.niri.queue_redraw(&output);
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
    use std::time::Duration;

    use smithay::utils::Point;

    use super::*;
    use crate::layout::{ActivateWindow, AddWindowTarget};
    use crate::niri::RedrawState;
    use crate::tests::Fixture;
    use crate::window::Mapped;

    fn create_window(f: &mut Fixture, title: &str, size: (u16, u16)) -> MappedId {
        let id = f.add_client();
        let window = f.client(id).create_window();
        let surface = window.surface.clone();
        window.set_title(title);
        window.commit();
        f.roundtrip(id);

        let window = f.client(id).window(&surface);
        window.attach_rgba_buffer([0, u32::MAX, 0, u32::MAX]);
        window.set_size(size.0, size.1);
        window.ack_last_and_commit();
        f.double_roundtrip(id);

        f.niri().layout.focus().unwrap().id()
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

    #[test]
    fn zoom_for_delta_y_is_exponential() {
        assert_eq!(MirrorViewGrab::zoom_for_delta_y(2., -120., 8.), 4.);
        assert_eq!(MirrorViewGrab::zoom_for_delta_y(2., 120., 8.), 1.);
    }

    #[test]
    fn zoom_for_delta_y_clamps_to_max() {
        assert_eq!(MirrorViewGrab::zoom_for_delta_y(4., -120., 6.), 6.);
    }

    #[test]
    fn on_frame_redraws_only_the_mirror_output() {
        let mut f = Fixture::new();
        f.add_output(1, (100, 100));
        f.add_output(2, (100, 100));

        let source_id = create_window(&mut f, "source", (40, 30));
        let mirror_id = create_window_mirror_for(&mut f, source_id);
        let output1 = f.niri_output(1);
        let output2 = f.niri_output(2);

        f.niri()
            .layout
            .move_to_output(Some(&mirror_id), &output2, None, ActivateWindow::No);
        for state in f.niri().output_state.values_mut() {
            state.redraw_state = RedrawState::Idle;
        }

        let start_data = PointerGrabStartData {
            focus: None,
            button: 0,
            location: Point::from((10., 10.)),
        };
        let mut grab = MirrorViewGrab::new(
            start_data,
            mirror_id,
            MirrorViewGrabMode::Pan,
            Point::from((10., 10.)),
            Point::from((10., 10.)),
            1.,
        );
        grab.new_location = Point::from((18., 14.));
        grab.event_timestamp = Some(Duration::from_millis(1));

        assert!(grab.on_frame(f.niri_state()));
        assert!(matches!(
            f.niri().output_state.get(&output1).unwrap().redraw_state,
            RedrawState::Idle
        ));
        assert!(matches!(
            f.niri().output_state.get(&output2).unwrap().redraw_state,
            RedrawState::Queued
        ));
    }
}
