use std::sync::Arc;

use niri_config::Config;
use smithay::output::Output;
use smithay::wayland::compositor::{with_states, SurfaceAttributes};
use wayland_client::protocol::wl_surface::WlSurface as ClientSurface;
use wayland_server::protocol::wl_surface::WlSurface as ServerSurface;

use super::client::ClientId;
use super::*;
use crate::layout::{ActivateWindow, AddWindowTarget};
use crate::niri::RedrawState;
use crate::window::mapped::MappedId;
use crate::window::Mapped;

fn create_window(
    f: &mut Fixture,
    title: &str,
    size: (u16, u16),
) -> (ClientId, ClientSurface, MappedId) {
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

    (id, surface, f.niri().layout.focus().unwrap().id())
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
fn virtual_output_custom_mode_does_not_accumulate_modes() {
    let mut f = Fixture::new();

    // Create a managed virtual output so it goes through the same config application path as in a
    // real session (`niri msg create-virtual-output`, `niri msg output ... custom-mode`).
    let name = {
        let state = f.niri_state();
        state
            .backend
            .create_virtual_output(&mut state.niri, 1920, 1080, 60, Some("sunshine".to_owned()))
            .unwrap()
    };

    let output = f
        .niri()
        .global_space
        .outputs()
        .find(|o| o.name() == name)
        .unwrap()
        .clone();

    // Sanity: single initial mode.
    {
        let modes = output.modes();
        assert_eq!(modes.len(), 1);
        assert_eq!(modes[0].size.w, 1920);
        assert_eq!(modes[0].size.h, 1080);
    }

    // 1080p -> 3200x1800
    {
        let state = f.niri_state();
        state.apply_transient_output_config(
            &name,
            niri_ipc::OutputAction::CustomMode {
                mode: niri_ipc::ConfiguredMode {
                    width: 3200,
                    height: 1800,
                    refresh: Some(60.0),
                },
            },
        );
    }

    {
        let modes = output.modes();
        assert_eq!(modes.len(), 1);
        assert_eq!(modes[0].size.w, 3200);
        assert_eq!(modes[0].size.h, 1800);
    }

    // 3200x1800 -> 1080p
    {
        let state = f.niri_state();
        state.apply_transient_output_config(
            &name,
            niri_ipc::OutputAction::CustomMode {
                mode: niri_ipc::ConfiguredMode {
                    width: 1920,
                    height: 1080,
                    refresh: Some(60.0),
                },
            },
        );
    }

    {
        let modes = output.modes();
        assert_eq!(modes.len(), 1);
        assert_eq!(modes[0].size.w, 1920);
        assert_eq!(modes[0].size.h, 1080);
    }
}

#[test]
fn touch_input_targets_virtual_output_when_focused() {
    let mut f = Fixture::new();
    f.add_output(1, (1920, 1080));

    // Create a virtual output and focus it.
    let name = {
        let state = f.niri_state();
        state
            .backend
            .create_virtual_output(&mut state.niri, 1920, 1080, 60, Some("virt".to_owned()))
            .unwrap()
    };

    let virt = f
        .niri()
        .global_space
        .outputs()
        .find(|o| o.name() == name)
        .unwrap()
        .clone();

    f.niri().layout.focus_output(&virt);

    // With no explicit `input.touch.map-to-output` configured, touch should follow the active
    // output (which may be virtual).
    let touch_output = f.niri().output_for_touch().unwrap().clone();
    assert_eq!(touch_output, virt);
}

#[test]
fn removing_off_virtual_output_does_not_panic() {
    let mut f = Fixture::new();

    let name = {
        let state = f.niri_state();
        state
            .backend
            .create_virtual_output(&mut state.niri, 1920, 1080, 60, Some("virt".to_owned()))
            .unwrap()
    };

    let output = f
        .niri()
        .global_space
        .outputs()
        .find(|o| o.name() == name)
        .unwrap()
        .clone();

    let output_id = {
        let state = f.niri_state();
        *state
            .backend
            .ipc_outputs()
            .lock()
            .unwrap()
            .iter()
            .find(|(_, ipc_output)| ipc_output.name == name)
            .map(|(id, _)| id)
            .unwrap()
    };

    {
        let state = f.niri_state();
        state.apply_transient_output_config(&name, niri_ipc::OutputAction::Off);
        assert!(!state.niri.output_exists(&output));

        state
            .backend
            .remove_virtual_output(&mut state.niri, &name)
            .unwrap();
    }

    assert!(!f
        .niri_state()
        .backend
        .ipc_outputs()
        .lock()
        .unwrap()
        .contains_key(&output_id));
}

#[test]
fn mirrored_surface_frame_callbacks_stay_single_virtual_output() {
    let mut f = Fixture::new();

    let source_output_name = {
        let state = f.niri_state();
        state
            .backend
            .create_virtual_output(&mut state.niri, 1920, 1080, 60, Some("virt-a".to_owned()))
            .unwrap()
    };
    let mirror_output_name = {
        let state = f.niri_state();
        state
            .backend
            .create_virtual_output(&mut state.niri, 1920, 1080, 60, Some("virt-b".to_owned()))
            .unwrap()
    };

    let source_output = f
        .niri()
        .global_space
        .outputs()
        .find(|o| o.name() == source_output_name)
        .unwrap()
        .clone();
    let mirror_output = f
        .niri()
        .global_space
        .outputs()
        .find(|o| o.name() == mirror_output_name)
        .unwrap()
        .clone();

    f.niri().layout.focus_output(&source_output);

    let (client_id, client_surface, source_id) = create_window(&mut f, "mirror-source", (40, 30));
    f.niri()
        .layout
        .move_to_output(Some(&source_id), &source_output, None, ActivateWindow::No);

    let mirror_id = create_window_mirror_for(&mut f, source_id);
    f.niri()
        .layout
        .move_to_output(Some(&mirror_id), &mirror_output, None, ActivateWindow::No);
    let owner_output = f
        .niri()
        .outputs_for_source(source_id)
        .into_iter()
        .next()
        .unwrap();
    let non_owner_output = if owner_output == source_output {
        mirror_output.clone()
    } else {
        source_output.clone()
    };
    f.niri_state().refresh_and_flush_clients();
    f.double_roundtrip(client_id);
    let server_surface = {
        let niri = f.niri();
        niri.layout
            .windows()
            .find(|(_, mapped)| mapped.id() == source_id)
            .map(|(_, mapped)| mapped.toplevel().wl_surface().clone())
            .unwrap()
    };
    let queued_frame_callbacks = |surface: &ServerSurface| {
        with_states(surface, |states| {
            states
                .cached_state
                .get::<SurfaceAttributes>()
                .current()
                .frame_callbacks
                .len()
        })
    };

    // Frame callbacks on virtual outputs are paced by the estimated vblank timer. Stop the pending
    // timers so they can't fire (and send frame callbacks) during the dispatch below if the test
    // runs slowly, and advance the frame callback sequence by hand like the timer would.
    stop_estimated_vblank_timer(&mut f, &source_output);
    stop_estimated_vblank_timer(&mut f, &mirror_output);

    {
        let client = f.client(client_id);
        client_surface.frame(&client.qh, Arc::new(SyncData::default()));
        client_surface.commit();
        client.connection.flush().unwrap();
    }
    f.state.server.dispatch_without_refresh();
    assert_eq!(queued_frame_callbacks(&server_surface), 1);

    advance_frame_callback_sequence(&mut f, &non_owner_output);
    f.niri()
        .send_frame_callbacks_for_virtual_output(&non_owner_output);
    assert_eq!(queued_frame_callbacks(&server_surface), 1);

    advance_frame_callback_sequence(&mut f, &owner_output);
    f.niri()
        .send_frame_callbacks_for_virtual_output(&owner_output);
    assert_eq!(queued_frame_callbacks(&server_surface), 0);
}

fn stop_estimated_vblank_timer(f: &mut Fixture, output: &Output) {
    let niri = f.niri();
    let state = niri.output_state.get_mut(output).unwrap();
    if let RedrawState::WaitingForEstimatedVBlank(token)
    | RedrawState::WaitingForEstimatedVBlankAndQueued(token) = state.redraw_state
    {
        niri.event_loop.remove(token);
        state.redraw_state = RedrawState::Idle;
    }
}

fn advance_frame_callback_sequence(f: &mut Fixture, output: &Output) {
    let state = f.niri().output_state.get_mut(output).unwrap();
    state.frame_callback_sequence = state.frame_callback_sequence.wrapping_add(1);
}

fn create_virtual(f: &mut Fixture, name: &str) -> Result<String, String> {
    let state = f.niri_state();
    state
        .backend
        .create_virtual_output(&mut state.niri, 1920, 1080, 60, Some(name.to_owned()))
}

fn remove_virtual(f: &mut Fixture, name: &str) -> Result<(), String> {
    let state = f.niri_state();
    state.backend.remove_virtual_output(&mut state.niri, name)
}

fn output_named(f: &mut Fixture, name: &str) -> Option<Output> {
    f.niri()
        .global_space
        .outputs()
        .find(|o| o.name() == name)
        .cloned()
}

fn has_ipc_output(f: &mut Fixture, name: &str) -> bool {
    f.niri_state()
        .backend
        .ipc_outputs()
        .lock()
        .unwrap()
        .values()
        .any(|o| o.name == name)
}

fn config_with_virtual_outputs(names: &[&str], extra: &str) -> Config {
    let mut kdl = String::from(extra);
    for name in names {
        kdl += &format!("output \"{name}\" {{\n    create-virtual\n}}\n");
    }
    Config::parse_mem(&kdl).unwrap()
}

#[test]
fn headless_redraw_keeps_frame_callback_pacing() {
    let mut f = Fixture::new();
    create_virtual(&mut f, "virt").unwrap();
    let output = output_named(&mut f, "virt").unwrap();

    f.niri().queue_redraw(&output);
    f.niri_state().refresh_and_flush_clients();

    let state = &f.niri().output_state[&output];
    let sequence = state.frame_callback_sequence;
    let RedrawState::WaitingForEstimatedVBlank(token) = state.redraw_state else {
        panic!("unexpected redraw state: {:?}", state.redraw_state);
    };

    // Redraws before the estimated vblank must neither restart the timer nor allow new frame
    // callbacks, otherwise clients committing in a loop render unthrottled.
    for _ in 0..3 {
        f.niri().queue_redraw(&output);
        f.niri_state().refresh_and_flush_clients();

        let state = &f.niri().output_state[&output];
        assert_eq!(state.frame_callback_sequence, sequence);
        assert!(matches!(
            state.redraw_state,
            RedrawState::WaitingForEstimatedVBlank(t) if t == token
        ));
    }
}

#[test]
fn virtual_outputs_keep_redrawing_with_monitors_off() {
    let mut f = Fixture::new();
    create_virtual(&mut f, "virt").unwrap();
    let output = output_named(&mut f, "virt").unwrap();

    {
        let state = f.niri_state();
        state.niri.deactivate_monitors(&mut state.backend);
    }

    f.niri().queue_redraw(&output);
    f.niri_state().refresh_and_flush_clients();

    assert!(matches!(
        f.niri().output_state[&output].redraw_state,
        RedrawState::WaitingForEstimatedVBlank(_)
    ));
}

#[test]
fn removing_virtual_output_notifies_ipc() {
    let mut f = Fixture::new();
    create_virtual(&mut f, "virt").unwrap();
    f.niri().ipc_outputs_changed = false;

    remove_virtual(&mut f, "virt").unwrap();
    assert!(f.niri().ipc_outputs_changed);
}

#[test]
fn virtual_output_names_are_case_insensitive() {
    let mut f = Fixture::new();
    f.add_output(1, (1920, 1080));

    assert!(create_virtual(&mut f, "HEADLESS-1").is_err());

    create_virtual(&mut f, "virt").unwrap();
    assert!(create_virtual(&mut f, "VIRT").is_err());

    remove_virtual(&mut f, "VIRT").unwrap();
    assert!(output_named(&mut f, "virt").is_none());
    assert!(!has_ipc_output(&mut f, "virt"));
}

#[test]
fn off_virtual_output_is_not_confused_with_same_named_output() {
    let config = Config::parse_mem("output \"headless-1\" { off; }").unwrap();
    let mut f = Fixture::with_config(config);

    create_virtual(&mut f, "headless-1").unwrap();
    assert!(output_named(&mut f, "headless-1").is_none());

    // An output with the same name appears through some other path (like a monitor hotplug).
    f.add_output(1, (1920, 1080));
    let output = output_named(&mut f, "headless-1").unwrap();

    // This must not try to remove the other output in place of the disconnected virtual one.
    f.niri_state().reload_output_config();
    assert!(f.niri().output_exists(&output));
}

#[test]
fn declared_virtual_outputs_follow_config() {
    let mut f = Fixture::with_config(config_with_virtual_outputs(&["virt-a"], ""));
    f.add_output(1, (1920, 1080));
    assert!(output_named(&mut f, "virt-a").is_some());

    // Renaming the declaration replaces the output.
    f.niri_state()
        .reload_config(Ok(config_with_virtual_outputs(&["virt-b"], "")));
    assert!(output_named(&mut f, "virt-a").is_none());
    assert!(!has_ipc_output(&mut f, "virt-a"));
    assert!(output_named(&mut f, "virt-b").is_some());

    // An output removed through IPC is not recreated by unrelated output config changes.
    remove_virtual(&mut f, "virt-b").unwrap();
    let extra = "output \"headless-1\" { scale 2; }\n";
    f.niri_state()
        .reload_config(Ok(config_with_virtual_outputs(&["virt-b"], extra)));
    assert!(output_named(&mut f, "virt-b").is_none());
    assert!(!has_ipc_output(&mut f, "virt-b"));

    // Removing the declaration doesn't touch an output created through IPC with the same name.
    create_virtual(&mut f, "virt-b").unwrap();
    f.niri_state()
        .reload_config(Ok(config_with_virtual_outputs(&[], extra)));
    assert!(output_named(&mut f, "virt-b").is_some());
    remove_virtual(&mut f, "virt-b").unwrap();

    // Adding the declaration back creates the output again.
    f.niri_state()
        .reload_config(Ok(config_with_virtual_outputs(&["virt-b"], extra)));
    assert!(output_named(&mut f, "virt-b").is_some());

    // Removing the declaration removes the output.
    f.niri_state()
        .reload_config(Ok(config_with_virtual_outputs(&[], extra)));
    assert!(output_named(&mut f, "virt-b").is_none());
    assert!(!has_ipc_output(&mut f, "virt-b"));
}
