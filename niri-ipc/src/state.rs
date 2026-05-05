//! Helpers for keeping track of the event stream state.
//!
//! 1. Create an [`EventStreamState`] using `Default::default()`, or any individual state part if
//!    you only care about part of the state.
//! 2. Connect to the niri socket and request an event stream.
//! 3. Pass every [`Event`] to [`EventStreamStatePart::apply`] on your state.
//! 4. Read the fields of the state as needed.

use std::collections::hash_map::Entry;
use std::collections::HashMap;

use crate::{BlockOutState, BlockedWindow, Cast, Event, KeyboardLayouts, Window, Workspace};

/// Part of the state communicated via the event stream.
pub trait EventStreamStatePart {
    /// Returns a sequence of events that replicates this state from default initialization.
    fn replicate(&self) -> Vec<Event>;

    /// Applies the event to this state.
    ///
    /// Returns `None` after applying the event, and `Some(event)` if the event is ignored by this
    /// part of the state.
    fn apply(&mut self, event: Event) -> Option<Event>;
}

/// The full state communicated over the event stream.
///
/// Different parts of the state are not guaranteed to be consistent across every single event
/// sent by niri. For example, you may receive the first [`Event::WindowOpenedOrChanged`] for a
/// just-opened window *after* an [`Event::WorkspaceActiveWindowChanged`] for that window. Between
/// these two events, the workspace active window id refers to a window that does not yet exist in
/// the windows state part.
#[derive(Debug, Default)]
pub struct EventStreamState {
    /// State of workspaces.
    pub workspaces: WorkspacesState,

    /// State of workspaces.
    pub windows: WindowsState,

    /// State of the keyboard layouts.
    pub keyboard_layouts: KeyboardLayoutsState,

    /// State of the overview.
    pub overview: OverviewState,

    /// State of the config.
    pub config: ConfigState,

    /// State of block-out.
    pub block_out: BlockOutStateState,

    /// State of screencasts.
    pub casts: CastsState,
}

/// The workspaces state communicated over the event stream.
#[derive(Debug, Default)]
pub struct WorkspacesState {
    /// Map from a workspace id to the workspace.
    pub workspaces: HashMap<u64, Workspace>,
}

/// The windows state communicated over the event stream.
#[derive(Debug, Default)]
pub struct WindowsState {
    /// Map from a window id to the window.
    pub windows: HashMap<u64, Window>,
}

/// The keyboard layout state communicated over the event stream.
#[derive(Debug, Default)]
pub struct KeyboardLayoutsState {
    /// Configured keyboard layouts.
    pub keyboard_layouts: Option<KeyboardLayouts>,
}

/// The overview state communicated over the event stream.
#[derive(Debug, Default)]
pub struct OverviewState {
    /// Whether the overview is currently open.
    pub is_open: bool,
}

/// The config state communicated over the event stream.
#[derive(Debug, Default)]
pub struct ConfigState {
    /// Whether the last config load attempt had failed.
    pub failed: bool,
}

/// The block-out state communicated over the event stream.
#[derive(Debug, Default)]
pub struct BlockOutStateState {
    /// Current block-out state snapshot.
    pub block_out_state: Option<BlockOutState>,
}

/// The casts state communicated over the event stream.
#[derive(Debug, Default)]
pub struct CastsState {
    /// Map from a stream id to the screencast.
    pub casts: HashMap<u64, Cast>,
}

impl EventStreamStatePart for EventStreamState {
    fn replicate(&self) -> Vec<Event> {
        let mut events = Vec::new();
        events.extend(self.workspaces.replicate());
        events.extend(self.windows.replicate());
        events.extend(self.keyboard_layouts.replicate());
        events.extend(self.overview.replicate());
        events.extend(self.config.replicate());
        events.extend(self.block_out.replicate());
        events.extend(self.casts.replicate());
        events
    }

    fn apply(&mut self, event: Event) -> Option<Event> {
        let event = self.workspaces.apply(event)?;
        let event = self.windows.apply(event)?;
        let event = self.keyboard_layouts.apply(event)?;
        let event = self.overview.apply(event)?;
        let event = self.config.apply(event)?;
        let event = self.block_out.apply(event)?;
        let event = self.casts.apply(event)?;
        Some(event)
    }
}

impl EventStreamStatePart for WorkspacesState {
    fn replicate(&self) -> Vec<Event> {
        let workspaces = self.workspaces.values().cloned().collect();
        vec![Event::WorkspacesChanged { workspaces }]
    }

    fn apply(&mut self, event: Event) -> Option<Event> {
        match event {
            Event::WorkspacesChanged { workspaces } => {
                self.workspaces = workspaces.into_iter().map(|ws| (ws.id, ws)).collect();
            }
            Event::WorkspaceUrgencyChanged { id, urgent } => {
                for ws in self.workspaces.values_mut() {
                    if ws.id == id {
                        ws.is_urgent = urgent;
                    }
                }
            }
            Event::WorkspaceActivated { id, focused } => {
                let ws = self.workspaces.get(&id);
                let ws = ws.expect("activated workspace was missing from the map");
                let output = ws.output.clone();

                for ws in self.workspaces.values_mut() {
                    let got_activated = ws.id == id;
                    if ws.output == output {
                        ws.is_active = got_activated;
                    }

                    if focused {
                        ws.is_focused = got_activated;
                    }
                }
            }
            Event::WorkspaceActiveWindowChanged {
                workspace_id,
                active_window_id,
            } => {
                let ws = self.workspaces.get_mut(&workspace_id);
                let ws = ws.expect("changed workspace was missing from the map");
                ws.active_window_id = active_window_id;
            }
            event => return Some(event),
        }
        None
    }
}

impl EventStreamStatePart for WindowsState {
    fn replicate(&self) -> Vec<Event> {
        let windows = self.windows.values().cloned().collect();
        vec![Event::WindowsChanged { windows }]
    }

    fn apply(&mut self, event: Event) -> Option<Event> {
        match event {
            Event::WindowsChanged { windows } => {
                self.windows = windows.into_iter().map(|win| (win.id, win)).collect();
            }
            Event::WindowOpenedOrChanged { window } => {
                let (id, is_focused) = match self.windows.entry(window.id) {
                    Entry::Occupied(mut entry) => {
                        let entry = entry.get_mut();
                        *entry = window;
                        (entry.id, entry.is_focused)
                    }
                    Entry::Vacant(entry) => {
                        let entry = entry.insert(window);
                        (entry.id, entry.is_focused)
                    }
                };

                if is_focused {
                    for win in self.windows.values_mut() {
                        if win.id != id {
                            win.is_focused = false;
                        }
                    }
                }
            }
            Event::WindowClosed { id } => {
                let win = self.windows.remove(&id);
                win.expect("closed window was missing from the map");
            }
            Event::WindowFocusChanged { id } => {
                for win in self.windows.values_mut() {
                    win.is_focused = Some(win.id) == id;
                }
            }
            Event::WindowFocusTimestampChanged {
                id,
                focus_timestamp,
            } => {
                for win in self.windows.values_mut() {
                    if win.id == id {
                        win.focus_timestamp = focus_timestamp;
                        break;
                    }
                }
            }
            Event::WindowUrgencyChanged { id, urgent } => {
                for win in self.windows.values_mut() {
                    if win.id == id {
                        win.is_urgent = urgent;
                        break;
                    }
                }
            }
            Event::WindowLayoutsChanged { changes } => {
                for (id, update) in changes {
                    let win = self.windows.get_mut(&id);
                    let win = win.expect("changed window was missing from the map");
                    win.layout = update;
                }
            }
            event => return Some(event),
        }
        None
    }
}

impl EventStreamStatePart for KeyboardLayoutsState {
    fn replicate(&self) -> Vec<Event> {
        if let Some(keyboard_layouts) = self.keyboard_layouts.clone() {
            vec![Event::KeyboardLayoutsChanged { keyboard_layouts }]
        } else {
            vec![]
        }
    }

    fn apply(&mut self, event: Event) -> Option<Event> {
        match event {
            Event::KeyboardLayoutsChanged { keyboard_layouts } => {
                self.keyboard_layouts = Some(keyboard_layouts);
            }
            Event::KeyboardLayoutSwitched { idx } => {
                let kb = self.keyboard_layouts.as_mut();
                let kb = kb.expect("keyboard layouts must be set before a layout can be switched");
                kb.current_idx = idx;
            }
            event => return Some(event),
        }
        None
    }
}

impl EventStreamStatePart for OverviewState {
    fn replicate(&self) -> Vec<Event> {
        vec![Event::OverviewOpenedOrClosed {
            is_open: self.is_open,
        }]
    }

    fn apply(&mut self, event: Event) -> Option<Event> {
        match event {
            Event::OverviewOpenedOrClosed { is_open } => {
                self.is_open = is_open;
            }
            event => return Some(event),
        }
        None
    }
}

impl EventStreamStatePart for ConfigState {
    fn replicate(&self) -> Vec<Event> {
        vec![Event::ConfigLoaded {
            failed: self.failed,
        }]
    }

    fn apply(&mut self, event: Event) -> Option<Event> {
        match event {
            Event::ConfigLoaded { failed } => {
                self.failed = failed;
            }
            event => return Some(event),
        }
        None
    }
}

impl EventStreamStatePart for BlockOutStateState {
    fn replicate(&self) -> Vec<Event> {
        let Some(block_out_state) = self.block_out_state.clone() else {
            return vec![];
        };

        vec![Event::BlockOutStateChanged { block_out_state }]
    }

    fn apply(&mut self, event: Event) -> Option<Event> {
        match event {
            Event::BlockOutStateChanged { block_out_state } => {
                self.block_out_state = Some(block_out_state);
            }
            Event::BlockOutWindowAddedOrChanged { window } => {
                let block_out_state = self
                    .block_out_state
                    .get_or_insert_with(|| BlockOutState {
                        is_enabled: false,
                        windows: vec![],
                        layers: vec![],
                    });

                upsert_blocked_window(&mut block_out_state.windows, window);
            }
            Event::BlockOutWindowRemoved { id } => {
                let Some(block_out_state) = self.block_out_state.as_mut() else {
                    return None;
                };

                block_out_state.windows.retain(|window| window.id != id);
            }
            Event::BlockOutEnabledChanged { is_enabled } => {
                let block_out_state = self
                    .block_out_state
                    .get_or_insert_with(|| BlockOutState {
                        is_enabled,
                        windows: vec![],
                        layers: vec![],
                    });
                block_out_state.is_enabled = is_enabled;
            }
            event => return Some(event),
        }

        None
    }
}

impl EventStreamStatePart for CastsState {
    fn replicate(&self) -> Vec<Event> {
        let casts = self.casts.values().cloned().collect();
        vec![Event::CastsChanged { casts }]
    }

    fn apply(&mut self, event: Event) -> Option<Event> {
        match event {
            Event::CastsChanged { casts } => {
                self.casts = casts.into_iter().map(|c| (c.stream_id, c)).collect();
            }
            Event::CastStartedOrChanged { cast } => {
                self.casts.insert(cast.stream_id, cast);
            }
            Event::CastStopped { stream_id } => {
                let cast = self.casts.remove(&stream_id);
                cast.expect("stopped cast was missing from the map");
            }
            event => return Some(event),
        }
        None
    }
}

fn upsert_blocked_window(windows: &mut Vec<BlockedWindow>, window: BlockedWindow) {
    if let Some(existing) = windows.iter_mut().find(|existing| existing.id == window.id) {
        *existing = window;
    } else {
        windows.push(window);
        windows.sort_unstable_by_key(|window| window.id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BlockOutFrom, Layer};

    #[test]
    fn block_out_state_replicates_and_applies_full_snapshot() {
        let block_out_state = BlockOutState {
            is_enabled: true,
            windows: vec![BlockedWindow {
                id: 1,
                title: Some(String::from("blocked")),
                app_id: Some(String::from("app")),
                workspace_id: Some(2),
                block_out_from: BlockOutFrom::Screencast,
            }],
            layers: vec![crate::BlockedLayerSurface {
                namespace: String::from("layer"),
                output: String::from("headless-1"),
                layer: Layer::Top,
                block_out_from: BlockOutFrom::ScreenCapture,
            }],
        };

        let mut state = BlockOutStateState::default();
        assert!(
            state
                .apply(Event::BlockOutStateChanged {
                    block_out_state: block_out_state.clone(),
                })
                .is_none()
        );
        assert_eq!(state.block_out_state, Some(block_out_state.clone()));

        let events = state.replicate();
        assert_eq!(events.len(), 1);
        match &events[0] {
            Event::BlockOutStateChanged {
                block_out_state: got,
            } => assert_eq!(got, &block_out_state),
            event => panic!("unexpected event: {event:?}"),
        }
    }

    #[test]
    fn block_out_state_patches_windows_and_enabled_flag() {
        let mut state = BlockOutStateState {
            block_out_state: Some(BlockOutState {
                is_enabled: true,
                windows: vec![BlockedWindow {
                    id: 2,
                    title: Some(String::from("second")),
                    app_id: None,
                    workspace_id: Some(9),
                    block_out_from: BlockOutFrom::ScreenCapture,
                }],
                layers: vec![],
            }),
        };

        state.apply(Event::BlockOutWindowAddedOrChanged {
            window: BlockedWindow {
                id: 1,
                title: Some(String::from("first")),
                app_id: Some(String::from("app")),
                workspace_id: Some(3),
                block_out_from: BlockOutFrom::Screencast,
            },
        });
        state.apply(Event::BlockOutWindowAddedOrChanged {
            window: BlockedWindow {
                id: 2,
                title: Some(String::from("updated")),
                app_id: None,
                workspace_id: Some(10),
                block_out_from: BlockOutFrom::Screencast,
            },
        });
        state.apply(Event::BlockOutEnabledChanged { is_enabled: false });
        state.apply(Event::BlockOutWindowRemoved { id: 1 });

        let block_out_state = state.block_out_state.unwrap();
        assert!(!block_out_state.is_enabled);
        assert_eq!(block_out_state.windows.len(), 1);
        assert_eq!(block_out_state.windows[0].id, 2);
        assert_eq!(block_out_state.windows[0].title.as_deref(), Some("updated"));
        assert_eq!(block_out_state.windows[0].workspace_id, Some(10));
        assert_eq!(
            block_out_state.windows[0].block_out_from,
            BlockOutFrom::Screencast
        );
    }
}
