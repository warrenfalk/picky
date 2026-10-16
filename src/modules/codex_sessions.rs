use std::collections::HashSet;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use iced::Subscription;
use iced::futures::channel::mpsc;
use iced::futures::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, BufReader};
use iced::futures::{SinkExt, Stream, StreamExt};
use serde::{Deserialize, Deserializer};

use crate::fuzzy;
use crate::module::{
    ActivationOutcome, DEFAULT_ACTION_ID, MatchKind, Module, ResultAction, SearchResult,
    sort_results,
};

const MODULE_KEY: &str = "codex-sessions";
const RESTART_DELAY: Duration = Duration::from_secs(3);
const IDLE_SCORE_PENALTY: i64 = 1_000;
const ACKNOWLEDGE_ACTION_ID: &str = "acknowledge";

pub type FeedUpdate = Result<Vec<Session>, String>;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum SessionIndicator {
    Error,
    Approval,
    Input,
    Attention,
    Unread,
    Working,
    Idle,
}

#[derive(Clone, Default)]
pub struct SessionStore(Arc<Mutex<Vec<Session>>>);

impl SessionStore {
    pub fn replace(&self, sessions: Vec<Session>) {
        *self.0.lock().unwrap() = sessions;
    }

    pub fn indicator(&self, session_id: &str) -> Option<SessionIndicator> {
        self.0
            .lock()
            .unwrap()
            .iter()
            .find(|session| session.id == session_id)
            .map(Session::indicator)
    }

    pub fn merge_results(&self, query: &str, results: &mut Vec<SearchResult>) {
        results.retain(|result| result.module_key != MODULE_KEY);
        results.extend(self.search(query));
        sort_results(results);
    }

    fn search(&self, query: &str) -> Vec<SearchResult> {
        let query = query.trim();
        let sessions = self.0.lock().unwrap().clone();
        let mut results = sessions
            .iter()
            .filter_map(|session| {
                let status = session.status_label();
                let score = if query.is_empty() {
                    if session.is_active() {
                        6_000
                    } else {
                        -IDLE_SCORE_PENALTY
                    }
                } else {
                    fuzzy::score_fields(
                        query,
                        &[
                            (&session.title, 120),
                            (&session.cwd, 50),
                            (&status, 20),
                            ("Codex", 80),
                        ],
                    )? + if session.is_active() {
                        100
                    } else {
                        -IDLE_SCORE_PENALTY
                    }
                };

                Some(SearchResult {
                    module_key: MODULE_KEY,
                    item_id: session.id.clone(),
                    title: session.title.clone(),
                    subtitle: format!("Codex | {status} | {}", session.cwd),
                    icon_name: None,
                    kind: MatchKind::CodexSession,
                    actions: if session.indicator() == SessionIndicator::Unread {
                        vec![ResultAction {
                            id: ACKNOWLEDGE_ACTION_ID,
                            label: "Acknowledge",
                            shortcut: 'a',
                        }]
                    } else {
                        Vec::new()
                    },
                    score,
                })
            })
            .collect::<Vec<_>>();
        sort_results(&mut results);
        results
    }
}

pub struct CodexSessionsModule {
    sessions: SessionStore,
}

impl CodexSessionsModule {
    pub fn new(sessions: SessionStore) -> Self {
        Self { sessions }
    }
}

impl Module for CodexSessionsModule {
    fn key(&self) -> &'static str {
        MODULE_KEY
    }

    fn search(&mut self, query: &str) -> Result<Vec<SearchResult>> {
        Ok(self.sessions.search(query))
    }

    fn activate(&mut self, item_id: &str, action_id: &str) -> Result<ActivationOutcome> {
        if action_id == ACKNOWLEDGE_ACTION_ID {
            ensure!(
                self.sessions.indicator(item_id) == Some(SessionIndicator::Unread),
                "This Codex session is no longer waiting for acknowledgement"
            );
            acknowledge(item_id)?;
            // The watch feed owns status updates; never clear unread optimistically.
            return Ok(ActivationOutcome::RefreshResults);
        }
        ensure!(
            action_id == DEFAULT_ACTION_ID,
            "Unknown Codex action: {action_id}"
        );
        let focus = focus_command(item_id)
            .output()
            .context("failed to run `codex agents --focus`")
            .and_then(|output| {
                ensure!(
                    output.status.success(),
                    "Could not focus Codex session: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                );
                Ok(())
            });
        if let Err(focus_error) = focus {
            let output = Command::new("xdg-open")
                .arg(format!("https://agent.warrenfalk.com/#{item_id}"))
                .stdin(Stdio::null())
                .output()
                .with_context(|| format!("{focus_error}; failed to run `xdg-open`"))?;
            ensure!(
                output.status.success(),
                "{focus_error}; could not open Codex session in browser: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(ActivationOutcome::ClosePicker)
    }
}

fn codex_command() -> Command {
    let mut command = Command::new("codex");
    command.args(["agents", "--local", "unix://"]);
    command.stdin(Stdio::null());
    command
}

fn focus_command(session_id: &str) -> Command {
    let mut command = codex_command();
    // Keep the opaque ID in one argument, even if it starts with a dash.
    command.arg(format!("--focus={session_id}"));
    command.stdout(Stdio::null()).stderr(Stdio::piped());
    command
}

fn acknowledge(session_id: &str) -> Result<()> {
    use iced::futures::future::{Either, select};

    iced::futures::executor::block_on(async {
        let mut command = Command::new("codex");
        command.args(["app-server", "proxy"]);
        let mut command = feed_command(command);
        command.stdin(Stdio::piped());
        let mut child = command
            .spawn()
            .context("failed to start Codex acknowledgement proxy")?;
        let io = ProxyIo {
            input: child.stdin.take().context("missing proxy input")?,
            output: child.stdout.take().context("missing proxy output")?,
        };
        let result = match select(
            Box::pin(acknowledge_exchange(io, session_id)),
            Box::pin(async_io::Timer::after(Duration::from_secs(10))),
        )
        .await
        {
            Either::Left((result, _)) => result,
            Either::Right(_) => Err(anyhow::anyhow!(
                "Timed out acknowledging Codex session; its status will update from the live feed"
            )),
        };
        // This short-lived proxy must not outlive the action, even on protocol errors.
        let _ = child.kill();
        let _ = child.status().await;
        result
    })
}

// The proxy forwards raw socket bytes, including the WebSocket handshake and frames.
struct ProxyIo {
    input: async_process::ChildStdin,
    output: async_process::ChildStdout,
}

impl AsyncRead for ProxyIo {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut [u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        std::pin::Pin::new(&mut self.output).poll_read(cx, buf)
    }
}

impl AsyncWrite for ProxyIo {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        std::pin::Pin::new(&mut self.input).poll_write(cx, buf)
    }
    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.input).poll_flush(cx)
    }
    fn poll_close(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.input).poll_close(cx)
    }
}

async fn acknowledge_exchange<T: AsyncRead + AsyncWrite + Unpin>(
    io: T,
    session_id: &str,
) -> Result<()> {
    use async_tungstenite::{WebSocketStream, tungstenite::Message};
    use serde_json::json;
    let (mut socket, _) = async_tungstenite::client_async("ws://localhost/rpc", io)
        .await
        .context("Codex proxy WebSocket handshake failed")?;
    async fn send<T: AsyncRead + AsyncWrite + Unpin>(
        socket: &mut WebSocketStream<T>,
        value: serde_json::Value,
    ) -> Result<()> {
        socket.send(Message::Text(value.to_string().into())).await?;
        Ok(())
    }
    async fn response<T: AsyncRead + AsyncWrite + Unpin>(
        socket: &mut WebSocketStream<T>,
        id: u64,
    ) -> Result<serde_json::Value> {
        loop {
            let frame = socket
                .next()
                .await
                .context("Codex proxy closed before acknowledging the request")??;
            let line = match frame {
                Message::Text(line) => line,
                Message::Ping(_) => {
                    socket.flush().await?;
                    continue;
                }
                Message::Close(_) => bail!("Codex proxy closed before acknowledging the request"),
                _ => continue,
            };
            let value: serde_json::Value =
                serde_json::from_str(&line).context("Invalid Codex proxy response")?;
            if value.get("id").and_then(serde_json::Value::as_u64) != Some(id) {
                continue;
            }
            ensure!(
                value.get("error").is_none(),
                "Codex rejected the acknowledgement request"
            );
            return value
                .get("result")
                .cloned()
                .context("Missing Codex response result");
        }
    }
    send(
        &mut socket,
        json!({"id": 1, "method": "initialize", "params": {
            "clientInfo": {"name": "picky", "version": env!("CARGO_PKG_VERSION")},
            "capabilities": {"experimentalApi": true}
        }}),
    )
    .await?;
    response(&mut socket, 1)
        .await
        .context("Could not initialize Codex acknowledgement connection")?;
    send(&mut socket, json!({"method": "initialized"})).await?;
    send(
        &mut socket,
        json!({"id": 2, "method": "thread/unread/set", "params": {
            "threadId": session_id, "unread": false
        }}),
    )
    .await?;
    match response(&mut socket, 2)
        .await?
        .get("unread")
        .and_then(serde_json::Value::as_bool)
    {
        Some(false) => Ok(()),
        Some(true) => bail!("Codex did not acknowledge this session because it is active again"),
        None => bail!("Codex did not return an acknowledgement state"),
    }
}

pub fn subscription() -> Subscription<FeedUpdate> {
    Subscription::run(watch)
}

fn watch_command() -> async_process::Command {
    let mut command = codex_command();
    command.args(["--json", "--watch"]);
    feed_command(command)
}

fn feed_command(mut command: Command) -> async_process::Command {
    stop_with_parent(&mut command);
    let mut command = async_process::Command::from(command);
    // Conversion does not preserve stdio configuration when spawning, so set it
    // on the async command itself.
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true);
    command
}

#[cfg(target_os = "linux")]
pub(super) fn stop_with_parent(command: &mut Command) {
    use std::os::unix::process::CommandExt;

    let parent = std::process::id() as libc::pid_t;
    // SAFETY: The child hook only calls async-signal-safe libc functions and
    // constructs an OS error. It does not allocate or acquire Rust locks.
    unsafe {
        command.pre_exec(move || {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            // The parent may have exited between fork and installing the signal.
            if libc::getppid() != parent {
                return Err(std::io::Error::from_raw_os_error(libc::ESRCH));
            }
            Ok(())
        });
    }
}

#[cfg(not(target_os = "linux"))]
pub(super) fn stop_with_parent(_command: &mut Command) {}

fn watch() -> impl Stream<Item = FeedUpdate> {
    watch_with_command(watch_command, RESTART_DELAY)
}

fn watch_with_command(
    command: impl Fn() -> async_process::Command + Send + 'static,
    restart_delay: Duration,
) -> impl Stream<Item = FeedUpdate> {
    iced::stream::channel(1, async move |mut output| {
        loop {
            let error = match forward_snapshots(command(), &mut output).await {
                Ok(()) => return,
                Err(error) => error,
            };
            if output.send(Err(error.to_string())).await.is_err() {
                return;
            }
            async_io::Timer::after(restart_delay).await;
        }
    })
}

async fn forward_snapshots(
    mut command: async_process::Command,
    output: &mut mpsc::Sender<FeedUpdate>,
) -> Result<()> {
    let mut child = command
        .spawn()
        .context("Could not start Codex session feed")?;
    let stdout = child
        .stdout
        .take()
        .context("Codex session feed has no stdout")?;
    let mut lines = BufReader::new(stdout).lines();
    while let Some(line) = lines.next().await {
        let line = line.context("Could not read Codex session feed")?;
        let update = parse_snapshot(&line).map_err(|error| error.to_string());
        output.send(update).await?;
    }
    bail!("Codex session feed stopped; reconnecting")
}

#[derive(Debug, Deserialize)]
struct Snapshot {
    version: u32,
    #[serde(flatten)]
    connection: Connection,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "connection", rename_all = "lowercase")]
enum Connection {
    Connected {
        counts: Counts,
        sessions: Vec<Session>,
    },
    Disconnected {
        #[serde(rename = "counts")]
        _counts: (),
        #[serde(rename = "sessions")]
        _sessions: (),
    },
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Counts {
    total: usize,
    working: usize,
    needs_attention: usize,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    id: String,
    title: String,
    cwd: String,
    working: bool,
    attention: Vec<String>,
    #[serde(default)]
    loaded: bool,
    #[serde(default, deserialize_with = "present")]
    updated_at: Option<u64>,
    // An absent field means unknown; a null name is explicitly unnamed.
    #[serde(default, deserialize_with = "present")]
    name: Option<Option<String>>,
    #[serde(default, deserialize_with = "present")]
    preview: Option<String>,
}

fn present<'de, T: Deserialize<'de>, D: Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Option<T>, D::Error> {
    T::deserialize(deserializer).map(Some)
}

impl Session {
    fn indicator(&self) -> SessionIndicator {
        for (reason, indicator) in [
            ("error", SessionIndicator::Error),
            ("approval", SessionIndicator::Approval),
            ("userInput", SessionIndicator::Input),
        ] {
            if self.attention.iter().any(|value| value == reason) {
                return indicator;
            }
        }
        if self.attention.iter().any(|reason| reason != "unread") {
            SessionIndicator::Attention
        } else if self.attention.iter().any(|reason| reason == "unread") {
            SessionIndicator::Unread
        } else if self.working {
            SessionIndicator::Working
        } else {
            SessionIndicator::Idle
        }
    }

    fn is_active(&self) -> bool {
        self.working || !self.attention.is_empty()
    }

    fn is_visible(&self) -> bool {
        if self.is_active() {
            return true;
        }
        if !self.loaded {
            return false;
        }
        match (&self.name, &self.preview) {
            (Some(name), Some(preview)) => {
                !name.as_deref().unwrap_or_default().trim().is_empty() || !preview.trim().is_empty()
            }
            _ => true,
        }
    }

    fn status_label(&self) -> String {
        let mut labels = Vec::new();
        for (reason, label) in [
            ("error", "Error"),
            ("approval", "Approval"),
            ("userInput", "Input"),
        ] {
            if self.attention.iter().any(|value| value == reason) {
                labels.push(label);
            }
        }
        if self.attention.iter().any(|reason| {
            !matches!(
                reason.as_str(),
                "error" | "approval" | "userInput" | "unread"
            )
        }) {
            labels.push("Attention");
        }
        if self.attention.iter().any(|reason| reason == "unread") {
            labels.push("Unread");
        }
        if self.working {
            labels.push("Working");
        }
        if labels.is_empty() {
            labels.push("Idle");
        }
        labels.join(", ")
    }
}

fn parse_snapshot(line: &str) -> Result<Vec<Session>> {
    // Serde errors can include conversation text. Do not surface raw input.
    let snapshot: Snapshot = serde_json::from_str(line)
        .map_err(|_| anyhow::anyhow!("Invalid Codex session snapshot"))?;
    ensure!(
        snapshot.version == 1,
        "Unsupported Codex session snapshot version"
    );
    let Connection::Connected {
        counts,
        mut sessions,
    } = snapshot.connection
    else {
        bail!("Codex session feed disconnected; reconnecting");
    };
    let mut ids = HashSet::new();
    ensure!(
        sessions
            .iter()
            .all(|session| !session.id.is_empty() && ids.insert(&session.id)),
        "Invalid or duplicate Codex session ID"
    );
    ensure!(
        counts.total == sessions.len()
            && counts.working == sessions.iter().filter(|session| session.working).count()
            && counts.needs_attention
                == sessions
                    .iter()
                    .filter(|session| !session.attention.is_empty())
                    .count(),
        "Codex session counts disagree with the session list"
    );
    sessions.retain(Session::is_visible);
    sessions.sort_by(|left, right| {
        left.indicator()
            .cmp(&right.indicator())
            .then_with(|| {
                if left.is_active() {
                    std::cmp::Ordering::Equal
                } else {
                    right.updated_at.cmp(&left.updated_at)
                }
            })
            .then_with(|| left.id.cmp(&right.id))
    });
    for session in &mut sessions {
        if session.title.is_empty() {
            session.title = "Untitled task".to_string();
        }
        // Preview content only determines empty-draft visibility; do not retain it.
        session.preview = None;
        session.name = None;
    }
    Ok(sessions)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::module::ModuleRegistry;
    use iced::futures::future::{Either, select};
    use serde_json::{Value, json};
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn session(id: &str) -> Value {
        json!({
            "id": id, "title": id, "cwd": "/work/picky", "working": false,
            "attention": [], "loaded": true, "updatedAt": 10,
            "name": null, "preview": "A conversation"
        })
    }

    fn snapshot(sessions: Vec<Value>) -> Value {
        json!({
            "version": 1, "connection": "connected",
            "counts": {
                "total": sessions.len(),
                "working": sessions.iter().filter(|session| session["working"] == true).count(),
                "needsAttention": sessions.iter().filter(|session| !session["attention"].as_array().unwrap().is_empty()).count()
            },
            "sessions": sessions
        })
    }

    fn parse(sessions: Vec<Value>) -> Vec<Session> {
        parse_snapshot(&snapshot(sessions).to_string()).unwrap()
    }

    #[test]
    fn filters_history_and_empty_idle_drafts_but_keeps_active_and_named_sessions() {
        let mut history = session("history");
        history["loaded"] = json!(false);
        let mut blank = session("blank");
        blank["name"] = json!(" \t");
        blank["preview"] = json!(" \n\u{2003}");
        let mut unnamed = blank.clone();
        unnamed["id"] = json!("unnamed");
        unnamed["name"] = Value::Null;
        let mut named = blank.clone();
        named["id"] = json!("named");
        named["name"] = json!("Untitled task");
        let mut working = unnamed.clone();
        working["id"] = json!("working");
        working["working"] = json!(true);
        let mut attention = unnamed.clone();
        attention["id"] = json!("attention");
        attention["attention"] = json!(["approval"]);
        let mut legacy = unnamed.clone();
        legacy["id"] = json!("legacy");
        legacy.as_object_mut().unwrap().remove("preview");
        let mut image = unnamed.clone();
        image["id"] = json!("image");
        image["preview"] = json!("[Image]");

        let sessions = parse(vec![
            history, blank, unnamed, named, working, attention, legacy, image,
        ]);
        let ids = sessions
            .iter()
            .map(|session| session.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(ids, ["attention", "working", "image", "legacy", "named"]);
        assert!(sessions.iter().all(|session| session.preview.is_none()));
    }

    #[test]
    fn acknowledgement_exchanges_only_initialize_and_unread_requests() {
        let id = "opaque\"session";
        let (result, requests) =
            mock_acknowledgement(id, Some(json!({"id": 2, "result": {"unread": false}})));
        result.unwrap();
        assert_eq!(requests.len(), 3);
        assert_eq!(requests[0]["method"], "initialize");
        assert_eq!(requests[1]["method"], "initialized");
        assert_eq!(requests[2]["method"], "thread/unread/set");
        assert_eq!(
            requests[2]["params"],
            json!({"threadId": id, "unread": false})
        );
    }

    #[test]
    fn acknowledgement_rejects_active_errors_missing_state_and_disconnects() {
        for reply in [
            Some(json!({"id": 2, "result": {"unread": true}})),
            Some(json!({"id": 2, "error": {"message": "rejected"}})),
            Some(json!({"id": 2, "result": {}})),
            None,
        ] {
            assert!(mock_acknowledgement("session", reply).0.is_err());
        }
    }

    fn mock_acknowledgement(
        id: &str,
        reply: Option<serde_json::Value>,
    ) -> (Result<()>, Vec<serde_json::Value>) {
        use async_tungstenite::tungstenite::Message;
        iced::futures::executor::block_on(async {
            let (client, server) = std::os::unix::net::UnixStream::pair().unwrap();
            let client = async_io::Async::new(client).unwrap();
            let server = async_io::Async::new(server).unwrap();
            let server = async {
                let mut socket = async_tungstenite::accept_async(server).await.unwrap();
                let mut requests = Vec::new();
                for index in 0..3 {
                    let request = socket.next().await.unwrap().unwrap().into_text().unwrap();
                    requests.push(serde_json::from_str(&request).unwrap());
                    if index == 0 {
                        socket
                            .send(Message::Text(
                                json!({"id": 1, "result": {}}).to_string().into(),
                            ))
                            .await
                            .unwrap();
                    }
                }
                socket.send(Message::Ping(vec![1].into())).await.unwrap();
                assert!(matches!(
                    socket.next().await.unwrap().unwrap(),
                    Message::Pong(_)
                ));
                socket
                    .send(Message::Text(
                        json!({"method": "thread/unread/changed"})
                            .to_string()
                            .into(),
                    ))
                    .await
                    .unwrap();
                if let Some(reply) = reply {
                    socket
                        .send(Message::Text(reply.to_string().into()))
                        .await
                        .unwrap();
                } else {
                    socket.close(None).await.unwrap();
                }
                requests
            };
            match iced::futures::future::select(
                Box::pin(iced::futures::future::join(
                    acknowledge_exchange(client, id),
                    server,
                )),
                Box::pin(async_io::Timer::after(Duration::from_secs(3))),
            )
            .await
            {
                iced::futures::future::Either::Left((result, _)) => result,
                _ => panic!("WebSocket acknowledgement stalled"),
            }
        })
    }

    #[test]
    #[ignore = "requires the installed Codex proxy and running local server; sends an invalid ID without modifying a session"]
    fn real_proxy_rejects_invalid_acknowledgement_without_timing_out() {
        let error = acknowledge("picky-invalid-session-id").unwrap_err();
        assert_eq!(
            error.to_string(),
            "Codex rejected the acknowledgement request"
        );
    }

    #[test]
    fn ranks_unread_between_other_attention_and_working_and_updates_live() {
        let mut working = session("a-working");
        working["working"] = json!(true);
        let mut unread = session("b-unread");
        unread["attention"] = json!(["unread"]);
        let mut other = session("c-other");
        other["attention"] = json!(["unread", "futureReason"]);
        let mut input = session("d-input");
        input["attention"] = json!(["unread", "userInput"]);
        let store = SessionStore::default();
        for query in ["", "codex"] {
            store.replace(parse(vec![
                working.clone(),
                unread.clone(),
                other.clone(),
                input.clone(),
            ]));
            let mut results = store.search(query);
            assert_eq!(
                results
                    .iter()
                    .map(|result| result.item_id.as_str())
                    .collect::<Vec<_>>(),
                ["d-input", "c-other", "b-unread", "a-working"]
            );
            assert_eq!(store.indicator("b-unread"), Some(SessionIndicator::Unread));
            assert_eq!(
                store.indicator("c-other"),
                Some(SessionIndicator::Attention)
            );
            assert_eq!(store.indicator("d-input"), Some(SessionIndicator::Input));
            assert!(results[2].subtitle.contains("| Unread |"));
            assert_eq!(results[2].actions[0].id, ACKNOWLEDGE_ACTION_ID);
            assert!(results[0].actions.is_empty());
            assert!(results[1].actions.is_empty());
            assert!(results[3].actions.is_empty());

            let mut acknowledged = unread.clone();
            acknowledged["attention"] = json!([]);
            store.replace(parse(vec![
                working.clone(),
                acknowledged,
                other.clone(),
                input.clone(),
            ]));
            store.merge_results(query, &mut results);
            assert_eq!(results.last().unwrap().item_id, "b-unread");
            assert_eq!(store.indicator("b-unread"), Some(SessionIndicator::Idle));
            assert!(results.last().unwrap().actions.is_empty());
        }
        store.replace(parse(vec![unread]));
        assert_eq!(store.search("unread").len(), 1);
    }

    #[test]
    fn ranks_active_then_idle_by_updated_time_through_the_registry() {
        let mut working = session("working");
        working["working"] = json!(true);
        let mut recent = session("Z recent");
        recent["updatedAt"] = json!(100);
        recent["recencyAt"] = json!(1);
        let mut old = session("A old");
        old["recencyAt"] = json!(500);
        let mut unknown = session("B undated");
        unknown.as_object_mut().unwrap().remove("updatedAt");
        let store = SessionStore::default();
        store.replace(parse(vec![old, unknown, recent, working]));
        let mut registry = ModuleRegistry::new(vec![Box::new(CodexSessionsModule::new(store))]);

        for query in ["", "codex"] {
            let results = registry.search(query).unwrap();
            assert_eq!(
                results
                    .iter()
                    .map(|result| result.item_id.as_str())
                    .collect::<Vec<_>>(),
                ["working", "Z recent", "A old", "B undated"]
            );
        }
    }

    #[test]
    fn search_matches_title_directory_status_and_codex_with_all_terms_required() {
        let mut working = session("Build session picker");
        working["working"] = json!(true);
        let store = SessionStore::default();
        store.replace(parse(vec![working]));
        assert_eq!(store.search("codex picky working picker").len(), 1);
        assert!(store.search("codex unrelated").is_empty());
        assert!(store.search("idle").is_empty());
    }

    #[test]
    fn becoming_idle_moves_a_session_below_other_results_with_or_without_a_query() {
        for query in ["", "codex"] {
            let store = SessionStore::default();
            let mut task = session("task");
            task["title"] = json!("Codex");
            task["working"] = json!(true);
            store.replace(parse(vec![task.clone()]));
            let mut results = [
                MatchKind::Application,
                MatchKind::Window,
                MatchKind::Workspace,
            ]
            .into_iter()
            .map(|kind| SearchResult {
                module_key: "test",
                item_id: format!("{kind:?}"),
                title: "Other match".to_string(),
                subtitle: String::new(),
                icon_name: None,
                kind,
                actions: Vec::new(),
                score: if query.is_empty() { 0 } else { 100 },
            })
            .collect::<Vec<_>>();

            store.merge_results(query, &mut results);
            assert_eq!(results[0].item_id, "task");

            task["working"] = json!(false);
            store.replace(parse(vec![task]));
            store.merge_results(query, &mut results);
            assert_eq!(results.len(), 4);
            assert_eq!(results.last().unwrap().item_id, "task");
        }
    }

    #[test]
    fn labels_all_family_statuses_and_falls_back_for_empty_titles() {
        let mut mixed = session("mixed");
        mixed["title"] = json!("");
        mixed["working"] = json!(true);
        mixed["attention"] = json!(["userInput", "approval", "error", "futureReason"]);
        let sessions = parse(vec![mixed]);
        assert_eq!(sessions[0].title, "Untitled task");
        assert_eq!(
            sessions[0].status_label(),
            "Error, Approval, Input, Attention, Working"
        );
    }

    #[test]
    fn rejects_inconsistent_snapshots_without_exposing_conversation_text() {
        let valid = snapshot(vec![session("one")]);
        let mut cases = vec![json!({}), snapshot(vec![session("same"), session("same")])];
        for (field, value) in [
            ("version", json!(2)),
            ("connection", json!("private conversation text")),
            (
                "counts",
                json!({"total": 9, "working": 0, "needsAttention": 0}),
            ),
        ] {
            let mut invalid = valid.clone();
            invalid[field] = value;
            cases.push(invalid);
        }
        for (field, value) in [
            ("id", json!("")),
            ("working", json!("private conversation text")),
            ("attention", json!([false])),
            ("loaded", Value::Null),
            ("updatedAt", json!(-1)),
            ("updatedAt", Value::Null),
            ("preview", Value::Null),
            ("name", json!(42)),
        ] {
            let mut invalid = valid.clone();
            invalid["sessions"][0][field] = value;
            cases.push(invalid);
        }
        for invalid in cases {
            let error = parse_snapshot(&invalid.to_string())
                .unwrap_err()
                .to_string();
            assert!(!error.contains("private conversation text"));
        }
    }

    #[test]
    fn disconnected_is_distinct_from_a_connected_empty_snapshot() {
        assert!(
            parse_snapshot(&snapshot(Vec::new()).to_string())
                .unwrap()
                .is_empty()
        );
        let disconnected = json!({
            "version": 1, "connection": "disconnected", "counts": null, "sessions": null
        });
        assert!(
            parse_snapshot(&disconnected.to_string())
                .unwrap_err()
                .to_string()
                .contains("disconnected")
        );
    }

    #[test]
    fn focus_passes_opaque_identity_to_the_same_server_without_a_shell() {
        let command = focus_command("--id;$(literal) with spaces");
        assert_eq!(command.get_program(), "codex");
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            [
                "agents",
                "--local",
                "unix://",
                "--focus=--id;$(literal) with spaces"
            ]
        );
    }

    async fn next_update(stream: &mut (impl Stream<Item = FeedUpdate> + Unpin)) -> FeedUpdate {
        match select(
            stream.next(),
            async_io::Timer::after(Duration::from_secs(5)),
        )
        .await
        {
            Either::Left((Some(update), _)) => update,
            _ => panic!("session feed did not produce an update"),
        }
    }

    #[test]
    fn watcher_restarts_after_exit_and_kills_its_process_on_drop() {
        iced::futures::executor::block_on(async {
            let starts = Arc::new(AtomicUsize::new(0));
            let command_starts = Arc::clone(&starts);
            let frame = snapshot(vec![session("PID")]).to_string();
            let (before_pid, after_pid) = frame.split_once("PID").unwrap();
            let before_pid = before_pid.to_owned();
            let after_pid = after_pid.to_owned();
            let mut stream = Box::pin(watch_with_command(
                move || {
                    let first = command_starts.fetch_add(1, Ordering::SeqCst) == 0;
                    let script = if first {
                        "printf '%s%s%s\\n' \"$1\" \"$$\" \"$2\""
                    } else {
                        "printf '%s%s%s\\n' \"$1\" \"$$\" \"$2\"; exec sleep 30"
                    };
                    let mut command = Command::new("sh");
                    command.args(["-c", script, "feed", &before_pid, &after_pid]);
                    feed_command(command)
                },
                Duration::from_millis(10),
            ));

            assert_eq!(next_update(&mut stream).await.unwrap().len(), 1);
            assert!(
                next_update(&mut stream)
                    .await
                    .unwrap_err()
                    .contains("stopped")
            );
            let sessions = next_update(&mut stream).await.unwrap();
            let pid = &sessions[0].id;
            assert_eq!(starts.load(Ordering::SeqCst), 2);
            drop(stream);

            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            loop {
                let running = Command::new("kill")
                    .args(["-0", pid])
                    .stderr(Stdio::null())
                    .status()
                    .unwrap()
                    .success();
                if !running {
                    break;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "watcher survived stream teardown"
                );
                async_io::Timer::after(Duration::from_millis(10)).await;
            }
        });
    }
}
