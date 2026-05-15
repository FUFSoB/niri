use std::str::FromStr;

use niri_config::utils::RegexEq;
use niri_config::window_rule::{Match as WindowMatch, WindowRule};
use niri_config::{Action, Config};

use super::client::ClientId;
use super::*;
use crate::window::mapped::MappedId;

fn set_up(config: Config) -> Fixture {
    let mut f = Fixture::with_config(config);
    f.add_output(1, (100, 100));
    f
}

fn create_window(f: &mut Fixture, id: ClientId, title: &str, size: (u16, u16)) {
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
}

fn focused_window_captures_cursor(f: &mut Fixture) -> bool {
    f.niri().layout.focus().unwrap().effective_cursor_capture()
}

fn window_captures_cursor(f: &mut Fixture, id: MappedId) -> bool {
    f.niri()
        .layout
        .windows()
        .find(|(_, window)| window.id() == id)
        .map(|(_, window)| window.effective_cursor_capture())
        .unwrap()
}

#[test]
fn toggle_window_cursor_capture_enables_and_disables_ruleless_window() {
    let mut f = set_up(Config::default());
    let id = f.add_client();

    create_window(&mut f, id, "plain", (40, 30));

    assert!(!focused_window_captures_cursor(&mut f));

    f.niri_state()
        .do_action(Action::ToggleWindowCursorCapture, false);
    assert!(focused_window_captures_cursor(&mut f));

    f.niri_state()
        .do_action(Action::ToggleWindowCursorCapture, false);
    assert!(!focused_window_captures_cursor(&mut f));
}

#[test]
fn toggle_window_cursor_capture_disables_and_restores_configured_rule() {
    let mut config = Config::default();
    config.window_rules.push(WindowRule {
        matches: vec![WindowMatch {
            title: Some(RegexEq::from_str("^captured$").unwrap()),
            ..Default::default()
        }],
        cursor_capture: Some(true),
        ..Default::default()
    });

    let mut f = set_up(config);
    let id = f.add_client();

    create_window(&mut f, id, "captured", (40, 30));

    assert!(focused_window_captures_cursor(&mut f));

    f.niri_state()
        .do_action(Action::ToggleWindowCursorCapture, false);
    assert!(!focused_window_captures_cursor(&mut f));

    f.niri_state()
        .do_action(Action::ToggleWindowCursorCapture, false);
    assert!(focused_window_captures_cursor(&mut f));
}

#[test]
fn toggle_window_cursor_capture_targets_focused_sticky_window() {
    let mut f = set_up(Config::default());
    let id = f.add_client();

    create_window(&mut f, id, "sticky", (40, 30));
    let sticky = f.niri().layout.focus().unwrap().id();

    create_window(&mut f, id, "tiling", (40, 30));
    let tiling = f.niri().layout.focus().unwrap().id();

    f.niri().layout.toggle_window_sticky(Some(&sticky));
    f.niri().layout.focus_floating();

    assert_eq!(f.niri().layout.focus().unwrap().id(), sticky);
    assert!(!window_captures_cursor(&mut f, sticky));
    assert!(!window_captures_cursor(&mut f, tiling));

    f.niri_state()
        .do_action(Action::ToggleWindowCursorCapture, false);

    assert!(window_captures_cursor(&mut f, sticky));
    assert!(!window_captures_cursor(&mut f, tiling));
}
