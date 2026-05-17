#![cfg(feature = "xdp-gnome-screencast")]

use niri_config::Config;

use super::client::ClientId;
use super::*;
use crate::dbus::gnome_shell_introspect::{
    IntrospectToNiri, NiriToIntrospect, OrderedWindowProperties,
};

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

fn introspect_windows(f: &mut Fixture) -> OrderedWindowProperties {
    let (tx, rx) = async_channel::bounded(1);
    f.niri_state()
        .on_introspect_msg(&tx, IntrospectToNiri::GetWindows);

    match rx.try_recv().unwrap() {
        NiriToIntrospect::Windows(windows) => windows,
    }
}

#[test]
fn introspect_windows_follow_picker_entry_order() {
    let mut f = Fixture::with_config(Config::default());
    f.add_output(1, (100, 100));

    let id = f.add_client();
    create_window(&mut f, id, "ws1", (40, 30));

    let output = f.niri_output(1);
    f.niri()
        .layout
        .monitor_for_output_mut(&output)
        .unwrap()
        .add_workspace_bottom();
    f.niri().layout.switch_workspace_down();

    create_window(&mut f, id, "ws2", (40, 30));

    let windows = introspect_windows(&mut f);
    let titles = windows
        .0
        .into_iter()
        .map(|(_, props)| props.title)
        .collect::<Vec<_>>();

    assert_eq!(
        &titles[..5],
        [
            String::from("niri Dynamic Cast Target"),
            String::from("ws1"),
            String::from("ws2"),
            String::from("Workspace 1 on headless-1 - ws1"),
            String::from("Workspace 2 on headless-1 - ws2"),
        ]
    );
    assert_eq!(titles.last().unwrap(), "Workspace 3 on headless-1 - empty");
}

#[test]
fn workspace_portal_titles_include_sticky_windows_and_output_names() {
    let mut f = Fixture::with_config(Config::default());
    f.add_output(1, (100, 100));
    f.add_output(2, (100, 100));
    f.niri_focus_output(1);

    let id = f.add_client();
    create_window(&mut f, id, "sticky", (40, 30));
    let sticky = f.niri().layout.focus().unwrap().id();
    f.niri().layout.toggle_window_sticky(Some(&sticky));

    create_window(&mut f, id, "tiling", (40, 30));
    f.niri().layout.focus_floating();

    let windows = introspect_windows(&mut f);
    let niri = f.niri();
    let workspace_titles = niri
        .layout
        .workspaces()
        .map(|(monitor, _, workspace)| {
            let monitor = monitor.unwrap();
            let portal_id = niri.portal_workspace_cast_id(workspace.id()).unwrap().get();
            let title = windows.get(portal_id).unwrap().title.clone();
            (monitor.output_name().clone(), title)
        })
        .collect::<Vec<_>>();

    assert!(workspace_titles.contains(&(
        String::from("headless-1"),
        String::from("Workspace 1 on headless-1 - sticky, tiling"),
    )));
    assert!(workspace_titles.contains(&(
        String::from("headless-2"),
        String::from("Workspace 1 on headless-2 - empty"),
    )));
}
