use std::sync::Arc;

use smithay::wayland::compositor::{with_states, SurfaceAttributes};
use wayland_client::protocol::wl_surface::WlSurface as ClientSurface;
use wayland_server::protocol::wl_surface::WlSurface as ServerSurface;

use super::client::ClientId;
use super::*;
use crate::layout::{ActivateWindow, AddWindowTarget};
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

    {
        let client = f.client(client_id);
        client_surface.frame(&client.qh, Arc::new(SyncData::default()));
        client_surface.commit();
        client.connection.flush().unwrap();
    }
    f.state.server.dispatch_without_refresh();
    assert_eq!(queued_frame_callbacks(&server_surface), 1);

    f.niri()
        .send_frame_callbacks_for_virtual_output(&non_owner_output);
    assert_eq!(queued_frame_callbacks(&server_surface), 1);

    f.niri()
        .send_frame_callbacks_for_virtual_output(&owner_output);
    assert_eq!(queued_frame_callbacks(&server_surface), 0);
}
