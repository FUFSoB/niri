use niri_config::{Action, MruDirection};
use smithay::backend::input::KeyState;
use smithay::input::keyboard::{FilterResult, Keycode};
use smithay::utils::SERIAL_COUNTER;
use wayland_client::protocol::wl_surface::WlSurface;

use super::client::ClientId;
use super::*;

fn set_up_window() -> (Fixture, ClientId, WlSurface) {
    let mut f = Fixture::new();
    f.add_output(1, (1920, 1080));

    let id = f.add_client();
    let window = f.client(id).create_window();
    let surface = window.surface.clone();
    window.commit();
    f.roundtrip(id);

    let window = f.client(id).window(&surface);
    window.attach_new_buffer();
    window.set_size(100, 100);
    window.ack_last_and_commit();
    f.double_roundtrip(id);

    (f, id, surface)
}

fn press_forwarded_key(f: &mut Fixture, keycode: Keycode) {
    let keyboard = f.niri().seat.get_keyboard().unwrap();
    let _ = keyboard.input(
        f.niri_state(),
        keycode,
        KeyState::Pressed,
        SERIAL_COUNTER.next_serial(),
        0,
        |_, _, _| -> FilterResult<()> { FilterResult::Forward },
    );
}

fn press_suppressed_key(f: &mut Fixture, keycode: Keycode) {
    let keyboard = f.niri().seat.get_keyboard().unwrap();
    let _ = keyboard.input(
        f.niri_state(),
        keycode,
        KeyState::Pressed,
        SERIAL_COUNTER.next_serial(),
        0,
        |state, _, _| {
            state.niri.suppressed_keys.insert(keycode);
            FilterResult::Intercept(())
        },
    );
}

#[test]
fn flush_lost_keyboard_state_releases_forwarded_modifiers() {
    let mut f = Fixture::new();
    f.add_output(1, (1920, 1080));

    let keycode = Keycode::from(133u32);
    press_forwarded_key(&mut f, keycode);

    let keyboard = f.niri().seat.get_keyboard().unwrap();
    assert!(keyboard.modifier_state().logo);
    assert!(keyboard.pressed_keys().contains(&keycode));

    f.niri_state().flush_lost_keyboard_state();

    let keyboard = f.niri().seat.get_keyboard().unwrap();
    assert!(!keyboard.modifier_state().logo);
    assert!(!keyboard.pressed_keys().contains(&keycode));
}

#[test]
fn flush_lost_keyboard_state_clears_suppressed_keys() {
    let mut f = Fixture::new();
    f.add_output(1, (1920, 1080));

    let keycode = Keycode::from(133u32);
    press_suppressed_key(&mut f, keycode);

    let keyboard = f.niri().seat.get_keyboard().unwrap();
    assert!(keyboard.modifier_state().logo);
    assert!(keyboard.pressed_keys().contains(&keycode));
    assert!(f.niri().suppressed_keys.contains(&keycode));

    f.niri_state().flush_lost_keyboard_state();

    let keyboard = f.niri().seat.get_keyboard().unwrap();
    assert!(!keyboard.modifier_state().logo);
    assert!(!keyboard.pressed_keys().contains(&keycode));
    assert!(f.niri().suppressed_keys.is_empty());
}

#[test]
fn flush_lost_keyboard_state_cancels_mru() {
    let (mut f, _id, _surface) = set_up_window();

    assert!(!f.niri().window_mru_ui.is_open());

    f.niri_state().do_action(
        Action::MruAdvance {
            direction: MruDirection::Forward,
            scope: None,
            filter: None,
        },
        false,
    );

    assert!(f.niri().window_mru_ui.is_open());

    f.niri_state().flush_lost_keyboard_state();

    assert!(!f.niri().window_mru_ui.is_open());
}
