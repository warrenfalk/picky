//! One persistent Niri connection shared by the window and workspace indexes.
use std::collections::BTreeMap;
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use iced::Subscription;
use iced::futures::channel::mpsc;
use iced::futures::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use iced::futures::{SinkExt, Stream, StreamExt};
use serde::de::DeserializeOwned;
use serde_json::Value;

pub type Update = Result<Value, String>;

#[derive(Clone, Default, Debug)]
pub struct Store(Arc<Mutex<State>>);

#[derive(Default, Debug)]
struct State {
    windows: BTreeMap<u64, Value>,
    workspaces: BTreeMap<u64, Value>,
    has_windows: bool,
    has_workspaces: bool,
}

impl Store {
    pub fn windows<T: DeserializeOwned>(&self) -> Option<Result<Vec<T>>> {
        let state = self.0.lock().unwrap();
        state.has_windows.then(|| decode(&state.windows))
    }

    pub fn workspaces<T: DeserializeOwned>(&self) -> Option<Result<Vec<T>>> {
        let state = self.0.lock().unwrap();
        state.has_workspaces.then(|| decode(&state.workspaces))
    }

    /// Returns whether searchable data changed. Layout-only window events are ignored.
    pub fn apply(&self, update: Update) -> bool {
        let mut state = self.0.lock().unwrap();
        let Ok(event) = update else {
            let changed = state.has_windows || state.has_workspaces;
            *state = State::default();
            return changed;
        };
        if let Some(windows) = event
            .pointer("/WindowsChanged/windows")
            .and_then(Value::as_array)
        {
            let windows = windows.iter().filter_map(project_window).collect();
            let changed = !state.has_windows || state.windows != windows;
            state.windows = windows;
            state.has_windows = true;
            return changed;
        } else if let Some(window) = event.pointer("/WindowOpenedOrChanged/window") {
            if is_picker(window) {
                return window
                    .get("id")
                    .and_then(Value::as_u64)
                    .is_some_and(|id| state.windows.remove(&id).is_some());
            }
            if let Some((id, window)) = project_window(window)
                && state.windows.get(&id) != Some(&window)
            {
                state.windows.insert(id, window);
                return true;
            }
        } else if let Some(id) = event.pointer("/WindowClosed/id").and_then(Value::as_u64) {
            return state.windows.remove(&id).is_some();
        } else if let Some(workspaces) = event
            .pointer("/WorkspacesChanged/workspaces")
            .and_then(Value::as_array)
        {
            let workspaces = workspaces.iter().filter_map(project_workspace).collect();
            let changed = !state.has_workspaces || state.workspaces != workspaces;
            state.workspaces = workspaces;
            state.has_workspaces = true;
            return changed;
        } else if let Some(activation) = event.get("WorkspaceActivated")
            && let Some(id) = activation.get("id").and_then(Value::as_u64)
        {
            let output = state
                .workspaces
                .get(&id)
                .and_then(|workspace| workspace.get("output"))
                .cloned();
            let focused = activation
                .get("focused")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let mut changed = false;
            for (workspace_id, workspace) in &mut state.workspaces {
                if output.is_some() && workspace.get("output") == output.as_ref() {
                    let active = Value::Bool(*workspace_id == id);
                    changed |= workspace["is_active"] != active;
                    workspace["is_active"] = active;
                }
                if focused {
                    let focused = Value::Bool(*workspace_id == id);
                    changed |= workspace["is_focused"] != focused;
                    workspace["is_focused"] = focused;
                }
            }
            return changed;
        }
        false
    }
}

fn project_workspace(workspace: &Value) -> Option<(u64, Value)> {
    let id = workspace.get("id")?.as_u64()?;
    Some((
        id,
        serde_json::json!({
            "id": id,
            "idx": workspace.get("idx")?,
            "name": workspace.get("name").unwrap_or(&Value::Null),
            "output": workspace.get("output").and_then(Value::as_str).unwrap_or("unknown output"),
            "is_active": workspace.get("is_active").and_then(Value::as_bool).unwrap_or(false),
            "is_focused": workspace.get("is_focused").and_then(Value::as_bool).unwrap_or(false),
        }),
    ))
}

fn project_window(window: &Value) -> Option<(u64, Value)> {
    if is_picker(window) {
        return None;
    }
    let id = window.get("id")?.as_u64()?;
    let mut projected = serde_json::Map::new();
    for key in ["id", "title", "app_id", "pid", "workspace_id"] {
        projected.insert(
            key.to_owned(),
            window.get(key).cloned().unwrap_or(Value::Null),
        );
    }
    // Untitled windows and windows between workspaces are valid Niri state.
    for key in ["title", "app_id"] {
        if projected[key].is_null() {
            projected.insert(key.to_owned(), Value::String(String::new()));
        }
    }
    if projected["workspace_id"].is_null() {
        projected.insert("workspace_id".to_owned(), Value::from(0));
    }
    Some((id, Value::Object(projected)))
}

fn is_picker(window: &Value) -> bool {
    window.get("app_id").and_then(Value::as_str) == Some("picky")
        && window.get("pid").and_then(Value::as_u64) == Some(u64::from(std::process::id()))
}

fn decode<T: DeserializeOwned>(items: &BTreeMap<u64, Value>) -> Result<Vec<T>> {
    items
        .values()
        .cloned()
        .map(|value| serde_json::from_value(value).context("invalid Niri index entry"))
        .collect()
}

pub fn subscription() -> Subscription<Update> {
    Subscription::run(watch)
}

fn watch() -> impl Stream<Item = Update> {
    iced::stream::channel(64, async |mut output| {
        loop {
            let error = forward(&mut output).await.unwrap_err();
            if output.send(Err(error.to_string())).await.is_err() {
                return;
            }
            async_io::Timer::after(Duration::from_secs(1)).await;
        }
    })
}

async fn forward(output: &mut mpsc::Sender<Update>) -> Result<()> {
    let path = std::env::var_os("NIRI_SOCKET").context("NIRI_SOCKET is not set")?;
    let socket = async_io::Async::<UnixStream>::connect(path)
        .await
        .context("could not connect to Niri event stream")?;
    forward_events(socket, output).await
}

async fn forward_events(
    mut socket: impl AsyncRead + AsyncWrite + Unpin,
    output: &mut mpsc::Sender<Update>,
) -> Result<()> {
    socket.write_all(b"\"EventStream\"\n").await?;
    let mut lines = BufReader::new(socket).lines();
    let reply = lines
        .next()
        .await
        .context("Niri event stream disconnected before reply")??;
    let reply: Value = serde_json::from_str(&reply).context("invalid Niri event stream reply")?;
    if reply != serde_json::json!({"Ok": "Handled"}) {
        bail!("Niri rejected event stream request: {reply}");
    }

    // The CLI decodes a fixed Event enum and exits on events added by a newer
    // compositor. Read raw JSON instead; Store::apply ignores unrelated events.
    while let Some(line) = lines.next().await {
        output.send(Ok(serde_json::from_str(&line?)?)).await?;
    }
    bail!("Niri event stream disconnected")
}

pub fn ticks() -> Subscription<()> {
    Subscription::run(|| {
        iced::stream::channel(1, async |mut output| {
            loop {
                async_io::Timer::after(Duration::from_millis(100)).await;
                if output.send(()).await.is_err() {
                    return;
                }
            }
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn event_stream_survives_unknown_events_and_keeps_updating_windows() {
        async_io::block_on(async {
            let (client, server) = UnixStream::pair().unwrap();
            let client = async_io::Async::new(client).unwrap();
            let server = async_io::Async::new(server).unwrap();
            let (mut output, mut updates) = mpsc::channel(8);
            let events = [
                json!({"WindowsChanged": {"windows": [{"id": 1, "title": "Old"}]}}),
                json!({"TouchpadToggled": {"on": true}}),
                json!({"FutureEvent": {"value": 42}}),
                json!({"WindowOpenedOrChanged": {"window": {"id": 1, "title": "New"}}}),
                json!({"WindowClosed": {"id": 1}}),
            ];
            let serve = async {
                let mut server = BufReader::new(server);
                let mut request = String::new();
                server.read_line(&mut request).await.unwrap();
                assert_eq!(request, "\"EventStream\"\n");
                server
                    .get_mut()
                    .write_all(b"{\"Ok\":\"Handled\"}\n")
                    .await
                    .unwrap();
                for event in &events {
                    let line = format!("{event}\n");
                    server.get_mut().write_all(line.as_bytes()).await.unwrap();
                }
            };
            let (result, ()) = iced::futures::join!(forward_events(client, &mut output), serve);
            assert_eq!(
                result.unwrap_err().to_string(),
                "Niri event stream disconnected"
            );
            drop(output);
            let store = Store::default();
            for (index, event) in events.iter().enumerate() {
                let update = updates.next().await.unwrap().unwrap();
                assert_eq!(&update, event);
                assert_eq!(store.apply(Ok(update)), !matches!(index, 1 | 2));
                if index == 3 {
                    assert_eq!(
                        store.windows::<Value>().unwrap().unwrap()[0]["title"],
                        "New"
                    );
                }
            }
            assert!(store.windows::<Value>().unwrap().unwrap().is_empty());
            assert!(updates.next().await.is_none());
        });
    }

    #[test]
    fn event_stream_rejects_failed_handshake() {
        async_io::block_on(async {
            let (client, server) = UnixStream::pair().unwrap();
            let client = async_io::Async::new(client).unwrap();
            let mut server = async_io::Async::new(server).unwrap();
            let (mut output, mut updates) = mpsc::channel(1);
            let serve = async {
                server.write_all(b"{\"Err\":\"denied\"}\n").await.unwrap();
            };
            let (result, ()) = iced::futures::join!(forward_events(client, &mut output), serve);
            assert!(result.unwrap_err().to_string().contains("denied"));
            drop(output);
            assert!(updates.next().await.is_none());
        });
    }

    #[test]
    fn snapshots_replace_state_and_reconnection_cannot_retain_closed_windows() {
        let store = Store::default();
        assert!(store.windows::<Value>().is_none());
        store.apply(Ok(
            json!({"WindowsChanged": {"windows": [{"id": 1, "title": "Old"}]}}),
        ));
        assert!(store.apply(Err("disconnected".to_owned())));
        assert!(store.windows::<Value>().is_none());
        store.apply(Ok(
            json!({"WindowsChanged": {"windows": [{"id": 2, "title": "New"}]}}),
        ));
        let windows = store.windows::<Value>().unwrap().unwrap();
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0]["id"], 2);
        assert!(store.apply(Ok(json!({"WindowClosed": {"id": 2}}))));
        assert!(store.windows::<Value>().unwrap().unwrap().is_empty());
    }

    #[test]
    fn layout_changes_do_not_refresh_results_but_titles_and_workspaces_do() {
        let store = Store::default();
        let mut window = json!({"id": 1, "title": "Editor", "workspace_id": 1});
        assert!(store.apply(Ok(json!({"WindowOpenedOrChanged": {"window": window}}))));
        window["layout"] = json!({"tile_size": [820, 1368]});
        assert!(!store.apply(Ok(json!({"WindowOpenedOrChanged": {"window": window}}))));
        window["title"] = json!("New title");
        assert!(store.apply(Ok(json!({"WindowOpenedOrChanged": {"window": window}}))));
        window["workspace_id"] = json!(2);
        assert!(store.apply(Ok(json!({"WindowOpenedOrChanged": {"window": window}}))));
    }

    #[test]
    fn workspace_activation_updates_only_its_output_and_moves_focus() {
        let store = Store::default();
        store.apply(Ok(json!({"WorkspacesChanged": {"workspaces": [
            {"id": 1, "idx": 1, "output": "DP-1", "is_active": true, "is_focused": true},
            {"id": 2, "idx": 2, "output": "DP-1"},
            {"id": 3, "idx": 1, "output": "DP-2", "is_active": true}
        ]}})));
        assert!(store.apply(Ok(
            json!({"WorkspaceActivated": {"id": 2, "focused": true}})
        )));
        let workspaces = store.workspaces::<Value>().unwrap().unwrap();
        assert_eq!(workspaces[0]["is_active"], false);
        assert_eq!(workspaces[0]["is_focused"], false);
        assert_eq!(workspaces[1]["is_active"], true);
        assert_eq!(workspaces[1]["is_focused"], true);
        assert_eq!(workspaces[2]["is_active"], true);
        assert!(!store.apply(Ok(
            json!({"WorkspaceActivated": {"id": 2, "focused": true}})
        )));
    }

    #[test]
    fn picker_is_excluded_and_nullable_window_metadata_is_normalized() {
        let store = Store::default();
        store.apply(Ok(json!({"WindowsChanged": {"windows": [
            {"id": 1, "title": null, "app_id": null, "workspace_id": null},
            {"id": 2, "app_id": "picky", "pid": std::process::id()}
        ]}})));
        let windows = store.windows::<Value>().unwrap().unwrap();
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0]["title"], "");
        assert_eq!(windows[0]["workspace_id"], 0);
        store.apply(Ok(json!({"WindowOpenedOrChanged": {"window": {
            "id": 3, "app_id": null, "pid": std::process::id()
        }}})));
        assert!(store.apply(Ok(json!({"WindowOpenedOrChanged": {"window": {
            "id": 3, "app_id": "picky", "pid": std::process::id()
        }}}))));
        assert_eq!(store.windows::<Value>().unwrap().unwrap().len(), 1);
    }
}
