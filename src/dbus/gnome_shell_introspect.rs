use std::collections::HashMap;

use zbus::fdo::{self, RequestNameFlags};
use zbus::interface;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{SerializeDict, Type, Value};

use super::Start;

pub struct Introspect {
    to_niri: calloop::channel::Sender<IntrospectToNiri>,
    from_niri: async_channel::Receiver<NiriToIntrospect>,
}

pub enum IntrospectToNiri {
    GetWindows,
}

pub enum NiriToIntrospect {
    Windows(HashMap<u64, WindowProperties>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceWindow {
    pub id: u64,
    pub title: Option<String>,
    pub app_id: Option<String>,
}

#[derive(Debug, SerializeDict, Type, Value)]
#[zvariant(signature = "dict")]
pub struct WindowProperties {
    /// Window title.
    pub title: String,
    /// Window app ID.
    ///
    /// This is actually the name of the .desktop file, and Shell does internal tracking to match
    /// Wayland app IDs to desktop files. We don't do that yet, which is the reason why
    /// xdg-desktop-portal-gnome's window list is missing icons.
    #[zvariant(rename = "app-id")]
    pub app_id: String,
}

pub fn workspace_cast_title(
    idx: u8,
    name: Option<&str>,
    active_window_id: Option<u64>,
    windows: &[WorkspaceWindow],
) -> String {
    let base = match name.filter(|name| !name.is_empty()) {
        Some(name) => format!("Workspace {idx} ({name})"),
        None => format!("Workspace {idx}"),
    };

    let mut windows = windows.iter().collect::<Vec<_>>();
    if let Some(active_window_id) = active_window_id {
        if let Some(index) = windows
            .iter()
            .position(|window| window.id == active_window_id)
        {
            let active = windows.remove(index);
            windows.insert(0, active);
        }
    }

    let mut summary = windows
        .iter()
        .take(3)
        .map(|window| workspace_window_label(window))
        .collect::<Vec<_>>()
        .join(", ");

    if summary.is_empty() {
        summary = String::from("empty");
    } else if windows.len() > 3 {
        summary.push_str(", ...");
    }

    format!("{base} - {summary}")
}

fn workspace_window_label(window: &WorkspaceWindow) -> String {
    if let Some(title) = window
        .title
        .as_deref()
        .map(str::trim)
        .filter(|title| !title.is_empty())
    {
        return title.to_string();
    }

    if let Some(app_id) = window
        .app_id
        .as_deref()
        .map(str::trim)
        .filter(|app_id| !app_id.is_empty())
    {
        return workspace_window_app_id_stem(app_id).to_string();
    }

    format!("window {}", window.id)
}

fn workspace_window_app_id_stem(app_id: &str) -> &str {
    let app_id = app_id.strip_suffix(".desktop").unwrap_or(app_id);
    app_id
        .rsplit_once('.')
        .map(|(_, tail)| tail)
        .filter(|tail| !tail.is_empty())
        .unwrap_or(app_id)
}

#[interface(name = "org.gnome.Shell.Introspect")]
impl Introspect {
    async fn get_windows(&self) -> fdo::Result<HashMap<u64, WindowProperties>> {
        if let Err(err) = self.to_niri.send(IntrospectToNiri::GetWindows) {
            warn!("error sending message to niri: {err:?}");
            return Err(fdo::Error::Failed("internal error".to_owned()));
        }

        match self.from_niri.recv().await {
            Ok(NiriToIntrospect::Windows(windows)) => Ok(windows),
            Err(err) => {
                warn!("error receiving message from niri: {err:?}");
                Err(fdo::Error::Failed("internal error".to_owned()))
            }
        }
    }

    // FIXME: call this upon window changes, once more of the infrastructure is there (will be
    // needed for the event stream IPC anyway).
    #[zbus(signal)]
    pub async fn windows_changed(ctxt: &SignalEmitter<'_>) -> zbus::Result<()>;
}

impl Introspect {
    pub fn new(
        to_niri: calloop::channel::Sender<IntrospectToNiri>,
        from_niri: async_channel::Receiver<NiriToIntrospect>,
    ) -> Self {
        Self { to_niri, from_niri }
    }
}

impl Start for Introspect {
    fn start(self) -> anyhow::Result<zbus::blocking::Connection> {
        let conn = zbus::blocking::Connection::session()?;
        let flags = RequestNameFlags::AllowReplacement
            | RequestNameFlags::ReplaceExisting
            | RequestNameFlags::DoNotQueue;

        conn.object_server()
            .at("/org/gnome/Shell/Introspect", self)?;
        conn.request_name_with_flags("org.gnome.Shell.Introspect", flags)?;

        Ok(conn)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_cast_title_includes_name_and_active_window_first() {
        let title = workspace_cast_title(
            2,
            Some("web"),
            Some(9),
            &[
                WorkspaceWindow {
                    id: 1,
                    title: Some(String::from("Terminal")),
                    app_id: Some(String::from("org.wezfurlong.wezterm")),
                },
                WorkspaceWindow {
                    id: 9,
                    title: Some(String::from("Firefox")),
                    app_id: Some(String::from("firefox")),
                },
                WorkspaceWindow {
                    id: 4,
                    title: Some(String::from("Slack")),
                    app_id: Some(String::from("com.slack.Slack")),
                },
            ],
        );

        assert_eq!(title, "Workspace 2 (web) - Firefox, Terminal, Slack");
    }

    #[test]
    fn workspace_cast_title_handles_empty_workspaces() {
        let title = workspace_cast_title(3, None, None, &[]);
        assert_eq!(title, "Workspace 3 - empty");
    }

    #[test]
    fn workspace_cast_title_limits_the_summary() {
        let title = workspace_cast_title(
            5,
            None,
            None,
            &[
                WorkspaceWindow {
                    id: 1,
                    title: Some(String::from("Firefox")),
                    app_id: None,
                },
                WorkspaceWindow {
                    id: 2,
                    title: Some(String::from("Slack")),
                    app_id: None,
                },
                WorkspaceWindow {
                    id: 3,
                    title: Some(String::from("Terminal")),
                    app_id: None,
                },
                WorkspaceWindow {
                    id: 4,
                    title: Some(String::from("Docs")),
                    app_id: None,
                },
            ],
        );

        assert_eq!(title, "Workspace 5 - Firefox, Slack, Terminal, ...");
    }

    #[test]
    fn workspace_cast_title_falls_back_to_app_id_stem() {
        let title = workspace_cast_title(
            1,
            None,
            None,
            &[WorkspaceWindow {
                id: 7,
                title: Some(String::from("  ")),
                app_id: Some(String::from("org.keepassxc.KeePassXC.desktop")),
            }],
        );

        assert_eq!(title, "Workspace 1 - KeePassXC");
    }
}
