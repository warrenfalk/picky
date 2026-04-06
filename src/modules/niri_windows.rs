use std::collections::HashMap;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::fuzzy;
use crate::fuzzy::WeightedField;
use crate::module::{
    ActivationOutcome, DEFAULT_ACTION_ID, MatchKind, Module, ResultAction, SearchResult,
};
use crate::modules::applications::ApplicationMetadata;

const MODULE_KEY: &str = "niri-windows";
const CLOSE_ACTION_ID: &str = "close";
const TERMINATE_ACTION_ID: &str = "terminate";
const KILL_ACTION_ID: &str = "kill";
const CLOSE_POLL_TIMEOUT: Duration = Duration::from_millis(750);
const CLOSE_POLL_INTERVAL: Duration = Duration::from_millis(50);

pub struct NiriWindowsModule {
    application_index: HashMap<String, ApplicationMetadata>,
    backend: Box<dyn WindowsBackend>,
}

#[derive(Clone, Debug, Deserialize)]
struct NiriWindow {
    id: u64,
    #[serde(default)]
    title: String,
    #[serde(default)]
    app_id: String,
    pid: Option<u32>,
    workspace_id: u64,
}

#[derive(Clone, Debug, Deserialize)]
struct NiriWorkspace {
    id: u64,
    idx: u64,
    name: Option<String>,
    output: String,
}

trait WindowsBackend: Send {
    fn load_windows(&self) -> Result<Vec<NiriWindow>>;
    fn load_workspaces(&self) -> Result<Vec<NiriWorkspace>>;
    fn focus_window(&self, window_id: &str) -> Result<()>;
    fn close_window(&self, window_id: &str) -> Result<()>;
    fn signal_window_process(&self, pid: u32, signal: &str) -> Result<()>;
    fn window_exists(&self, window_id: &str) -> Result<bool>;
    fn sleep(&self, duration: Duration);
}

struct ProcessWindowsBackend;

impl WindowsBackend for ProcessWindowsBackend {
    fn load_windows(&self) -> Result<Vec<NiriWindow>> {
        load_windows()
    }

    fn load_workspaces(&self) -> Result<Vec<NiriWorkspace>> {
        load_workspaces()
    }

    fn focus_window(&self, window_id: &str) -> Result<()> {
        focus_window(window_id)
    }

    fn close_window(&self, window_id: &str) -> Result<()> {
        close_window(window_id)
    }

    fn signal_window_process(&self, pid: u32, signal: &str) -> Result<()> {
        signal_window_process(pid, signal)
    }

    fn window_exists(&self, window_id: &str) -> Result<bool> {
        window_exists(window_id)
    }

    fn sleep(&self, duration: Duration) {
        thread::sleep(duration);
    }
}

impl NiriWindowsModule {
    pub fn new() -> Self {
        let application_index = crate::modules::applications::load_application_index()
            .unwrap_or_else(|err| {
                eprintln!("failed to load application index: {err:#}");
                HashMap::new()
            });

        Self::with_backend(application_index, Box::new(ProcessWindowsBackend))
    }

    fn with_backend(
        application_index: HashMap<String, ApplicationMetadata>,
        backend: Box<dyn WindowsBackend>,
    ) -> Self {
        Self {
            application_index,
            backend,
        }
    }
}

fn score_window(
    query: &str,
    application_name: &str,
    app_id: &str,
    title: &str,
    workspace_label: &str,
    output_name: &str,
) -> Option<i64> {
    fuzzy::score_weighted_fields(
        query,
        &[
            WeightedField::new(application_name, 110, 3, 2),
            WeightedField::new(app_id, 70, 1, 1),
            WeightedField::new(title, 120, 1, 2),
            WeightedField::new(workspace_label, 30, 1, 4),
            WeightedField::new(output_name, 20, 1, 4),
        ],
    )
}

fn application_for_app_id<'a>(
    application_index: &'a HashMap<String, ApplicationMetadata>,
    app_id: &str,
) -> Option<&'a ApplicationMetadata> {
    let app_id = app_id.trim();
    if app_id.is_empty() {
        return None;
    }

    if let Some(application) = application_index.get(app_id) {
        return Some(application);
    }

    let desktop_id = format!("{app_id}.desktop");
    application_index.get(&desktop_id)
}

fn application_label_for_window<'a>(
    application_index: &'a HashMap<String, ApplicationMetadata>,
    app_id: &'a str,
) -> &'a str {
    application_for_app_id(application_index, app_id)
        .map(|application| application.name.trim())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| app_id.trim())
}

fn icon_name_for_app_id(
    application_index: &HashMap<String, ApplicationMetadata>,
    app_id: &str,
) -> Option<String> {
    application_for_app_id(application_index, app_id)
        .and_then(|application| application.icon_name.clone())
}

impl Module for NiriWindowsModule {
    fn key(&self) -> &'static str {
        MODULE_KEY
    }

    fn search(&mut self, query: &str) -> Result<Vec<SearchResult>> {
        let query = query.trim();
        let windows = self.backend.load_windows()?;
        let workspaces = self
            .backend
            .load_workspaces()?
            .into_iter()
            .map(|workspace| (workspace.id, workspace))
            .collect::<HashMap<_, _>>();

        let mut results = windows
            .into_iter()
            .filter_map(|window| {
                let workspace = workspaces.get(&window.workspace_id);
                let workspace_label = workspace
                    .and_then(|workspace| workspace.name.clone())
                    .unwrap_or_else(|| {
                        workspace
                            .map(|workspace| workspace.idx.to_string())
                            .unwrap_or_else(|| window.workspace_id.to_string())
                    });
                let output_name = workspace
                    .map(|workspace| workspace.output.as_str())
                    .unwrap_or("unknown output");
                let application_label =
                    application_label_for_window(&self.application_index, &window.app_id);
                let score = score_window(
                    query,
                    application_label,
                    &window.app_id,
                    &window.title,
                    &workspace_label,
                    output_name,
                )?;

                let subtitle = if application_label.is_empty() {
                    format!("{workspace_label} on {output_name}")
                } else {
                    format!("{application_label}  {workspace_label} on {output_name}")
                };

                Some(SearchResult {
                    module_key: MODULE_KEY,
                    item_id: encode_window_target(window.id, window.pid),
                    title: window.title,
                    subtitle,
                    icon_name: icon_name_for_app_id(&self.application_index, &window.app_id),
                    kind: MatchKind::Window,
                    actions: vec![
                        ResultAction {
                            id: CLOSE_ACTION_ID,
                            label: "close",
                            shortcut: 'q',
                        },
                        ResultAction {
                            id: TERMINATE_ACTION_ID,
                            label: "terminate",
                            shortcut: 't',
                        },
                        ResultAction {
                            id: KILL_ACTION_ID,
                            label: "kill",
                            shortcut: 'k',
                        },
                    ],
                    score,
                })
            })
            .collect::<Vec<_>>();

        results.sort_by(|left, right| {
            right
                .score
                .cmp(&left.score)
                .then_with(|| left.title.cmp(&right.title))
        });

        Ok(results)
    }

    fn activate(&mut self, item_id: &str, action_id: &str) -> Result<ActivationOutcome> {
        let target = parse_window_target(item_id)?;

        match action_id {
            DEFAULT_ACTION_ID => {
                self.backend.focus_window(target.window_id)?;
                Ok(ActivationOutcome::ClosePicker)
            }
            CLOSE_ACTION_ID => {
                self.backend.close_window(target.window_id)?;
                wait_for_close_or_focus(self.backend.as_ref(), target.window_id)
            }
            TERMINATE_ACTION_ID => {
                signal_window_process_with_backend(self.backend.as_ref(), target.pid, "TERM")?;
                Ok(ActivationOutcome::ClosePicker)
            }
            KILL_ACTION_ID => {
                signal_window_process_with_backend(self.backend.as_ref(), target.pid, "KILL")?;
                Ok(ActivationOutcome::ClosePicker)
            }
            _ => anyhow::bail!("unknown window action: {action_id}"),
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct WindowTarget<'a> {
    window_id: &'a str,
    pid: Option<u32>,
}

fn encode_window_target(window_id: u64, pid: Option<u32>) -> String {
    match pid {
        Some(pid) => format!("{window_id}:{pid}"),
        None => window_id.to_string(),
    }
}

fn parse_window_target(item_id: &str) -> Result<WindowTarget<'_>> {
    if let Some((window_id, pid)) = item_id.split_once(':') {
        let pid = pid
            .parse::<u32>()
            .with_context(|| format!("invalid window pid in item id: {item_id}"))?;

        Ok(WindowTarget {
            window_id,
            pid: Some(pid),
        })
    } else {
        Ok(WindowTarget {
            window_id: item_id,
            pid: None,
        })
    }
}

fn focus_window(window_id: &str) -> Result<()> {
    let status = Command::new("niri")
        .args(["msg", "action", "focus-window", "--id", window_id])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .with_context(|| format!("failed to focus niri window {window_id}"))?;

    if !status.success() {
        bail!("niri refused to focus window {window_id}");
    }

    Ok(())
}

fn close_window(window_id: &str) -> Result<()> {
    let status = Command::new("niri")
        .args(["msg", "action", "close-window", "--id", window_id])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .with_context(|| format!("failed to close niri window {window_id}"))?;

    if !status.success() {
        bail!("niri refused to close window {window_id}");
    }

    Ok(())
}

fn wait_for_close_or_focus(
    backend: &dyn WindowsBackend,
    window_id: &str,
) -> Result<ActivationOutcome> {
    let mut elapsed = Duration::ZERO;

    while elapsed < CLOSE_POLL_TIMEOUT {
        if !backend.window_exists(window_id)? {
            return Ok(ActivationOutcome::RefreshResults);
        }

        backend.sleep(CLOSE_POLL_INTERVAL);
        elapsed += CLOSE_POLL_INTERVAL;
    }

    if backend.window_exists(window_id)? {
        backend.focus_window(window_id)?;
        Ok(ActivationOutcome::ClosePicker)
    } else {
        Ok(ActivationOutcome::RefreshResults)
    }
}

fn window_exists(window_id: &str) -> Result<bool> {
    Ok(load_windows()?
        .into_iter()
        .any(|window| window.id.to_string() == window_id))
}

fn signal_window_process_with_backend(
    backend: &dyn WindowsBackend,
    pid: Option<u32>,
    signal: &str,
) -> Result<()> {
    let Some(pid) = pid else {
        bail!("window has no process id available");
    };

    backend.signal_window_process(pid, signal)
}

fn signal_window_process(pid: u32, signal: &str) -> Result<()> {
    let status = Command::new("kill")
        .args([format!("-{signal}"), pid.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .with_context(|| format!("failed to send SIG{signal} to process {pid}"))?;

    if !status.success() {
        bail!("kill refused to send SIG{signal} to process {pid}");
    }

    Ok(())
}

fn load_windows() -> Result<Vec<NiriWindow>> {
    let output = Command::new("niri")
        .args(["msg", "--json", "windows"])
        .output()
        .context("failed to run `niri msg --json windows`")?;

    if !output.status.success() {
        bail!(
            "`niri msg --json windows` failed with status {}",
            output.status
        );
    }

    serde_json::from_slice(&output.stdout).context("failed to parse niri windows JSON")
}

fn load_workspaces() -> Result<Vec<NiriWorkspace>> {
    let output = Command::new("niri")
        .args(["msg", "--json", "workspaces"])
        .output()
        .context("failed to run `niri msg --json workspaces`")?;

    if !output.status.success() {
        bail!(
            "`niri msg --json workspaces` failed with status {}",
            output.status
        );
    }

    serde_json::from_slice(&output.stdout).context("failed to parse niri workspaces JSON")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct FakeState {
        windows: Vec<NiriWindow>,
        workspaces: Vec<NiriWorkspace>,
        focused: Vec<String>,
        closed: Vec<String>,
        signaled: Vec<(u32, String)>,
        existence_checks: VecDeque<bool>,
        sleeps: usize,
    }

    struct FakeWindowsBackend {
        state: Arc<Mutex<FakeState>>,
    }

    impl WindowsBackend for FakeWindowsBackend {
        fn load_windows(&self) -> Result<Vec<NiriWindow>> {
            Ok(self.state.lock().unwrap().windows.clone())
        }

        fn load_workspaces(&self) -> Result<Vec<NiriWorkspace>> {
            Ok(self.state.lock().unwrap().workspaces.clone())
        }

        fn focus_window(&self, window_id: &str) -> Result<()> {
            self.state
                .lock()
                .unwrap()
                .focused
                .push(window_id.to_string());
            Ok(())
        }

        fn close_window(&self, window_id: &str) -> Result<()> {
            self.state
                .lock()
                .unwrap()
                .closed
                .push(window_id.to_string());
            Ok(())
        }

        fn signal_window_process(&self, pid: u32, signal: &str) -> Result<()> {
            self.state
                .lock()
                .unwrap()
                .signaled
                .push((pid, signal.to_string()));
            Ok(())
        }

        fn window_exists(&self, _window_id: &str) -> Result<bool> {
            let mut state = self.state.lock().unwrap();
            Ok(state.existence_checks.pop_front().unwrap_or(false))
        }

        fn sleep(&self, _duration: Duration) {
            self.state.lock().unwrap().sleeps += 1;
        }
    }

    fn window(
        id: u64,
        title: &str,
        app_id: &str,
        pid: Option<u32>,
        workspace_id: u64,
    ) -> NiriWindow {
        NiriWindow {
            id,
            title: title.to_string(),
            app_id: app_id.to_string(),
            pid,
            workspace_id,
        }
    }

    fn workspace(id: u64, idx: u64, name: Option<&str>, output: &str) -> NiriWorkspace {
        NiriWorkspace {
            id,
            idx,
            name: name.map(ToOwned::to_owned),
            output: output.to_string(),
        }
    }

    fn application(name: &str) -> ApplicationMetadata {
        ApplicationMetadata {
            name: name.to_string(),
            icon_name: None,
        }
    }

    #[test]
    fn search_exposes_close_terminate_and_kill_actions() {
        let state = Arc::new(Mutex::new(FakeState {
            windows: vec![window(1, "Firefox", "firefox", Some(111), 10)],
            workspaces: vec![workspace(10, 2, Some("code"), "DP-1")],
            ..FakeState::default()
        }));
        let mut module =
            NiriWindowsModule::with_backend(HashMap::new(), Box::new(FakeWindowsBackend { state }));

        let results = module.search("fire").unwrap();

        assert_eq!(results[0].actions.len(), 3);
        assert_eq!(results[0].actions[0].id, CLOSE_ACTION_ID);
        assert_eq!(results[0].actions[1].id, TERMINATE_ACTION_ID);
        assert_eq!(results[0].actions[2].id, KILL_ACTION_ID);
    }

    #[test]
    fn search_prefers_application_name_matches_over_title_matches() {
        let state = Arc::new(Mutex::new(FakeState {
            windows: vec![
                window(1, "Project notes", "firefox", Some(111), 10),
                window(2, "Firefox docs", "wezterm", Some(222), 10),
            ],
            workspaces: vec![workspace(10, 2, Some("code"), "DP-1")],
            ..FakeState::default()
        }));
        let mut module = NiriWindowsModule::with_backend(
            HashMap::from([
                ("firefox".to_string(), application("Firefox")),
                ("wezterm".to_string(), application("WezTerm")),
            ]),
            Box::new(FakeWindowsBackend { state }),
        );

        let results = module.search("firefox").unwrap();

        assert_eq!(results[0].title, "Project notes");
        assert_eq!(results[0].subtitle, "Firefox  code on DP-1");
        assert_eq!(results[1].title, "Firefox docs");
    }

    #[test]
    fn search_uses_resolved_application_name_for_matching() {
        let state = Arc::new(Mutex::new(FakeState {
            windows: vec![window(1, "Project notes", "Navigator", Some(111), 10)],
            workspaces: vec![workspace(10, 2, Some("code"), "DP-1")],
            ..FakeState::default()
        }));
        let mut module = NiriWindowsModule::with_backend(
            HashMap::from([("Navigator.desktop".to_string(), application("Firefox"))]),
            Box::new(FakeWindowsBackend { state }),
        );

        let results = module.search("firefox").unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "Project notes");
        assert_eq!(results[0].subtitle, "Firefox  code on DP-1");
    }

    #[test]
    fn default_activate_focuses_window() {
        let state = Arc::new(Mutex::new(FakeState::default()));
        let mut module = NiriWindowsModule::with_backend(
            HashMap::new(),
            Box::new(FakeWindowsBackend {
                state: Arc::clone(&state),
            }),
        );

        let outcome = module.activate("42", DEFAULT_ACTION_ID).unwrap();

        assert_eq!(outcome, ActivationOutcome::ClosePicker);
        assert_eq!(state.lock().unwrap().focused.as_slice(), ["42"]);
    }

    #[test]
    fn close_refreshes_when_window_disappears() {
        let state = Arc::new(Mutex::new(FakeState {
            existence_checks: VecDeque::from([false]),
            ..FakeState::default()
        }));
        let mut module = NiriWindowsModule::with_backend(
            HashMap::new(),
            Box::new(FakeWindowsBackend {
                state: Arc::clone(&state),
            }),
        );

        let outcome = module.activate("42", CLOSE_ACTION_ID).unwrap();

        assert_eq!(outcome, ActivationOutcome::RefreshResults);
        let state = state.lock().unwrap();
        assert_eq!(state.closed.as_slice(), ["42"]);
        assert!(state.focused.is_empty());
    }

    #[test]
    fn close_focuses_window_if_it_persists() {
        let checks = std::iter::repeat(true)
            .take((CLOSE_POLL_TIMEOUT.as_millis() / CLOSE_POLL_INTERVAL.as_millis()) as usize + 1)
            .collect();
        let state = Arc::new(Mutex::new(FakeState {
            existence_checks: checks,
            ..FakeState::default()
        }));
        let mut module = NiriWindowsModule::with_backend(
            HashMap::new(),
            Box::new(FakeWindowsBackend {
                state: Arc::clone(&state),
            }),
        );

        let outcome = module.activate("42", CLOSE_ACTION_ID).unwrap();

        assert_eq!(outcome, ActivationOutcome::ClosePicker);
        assert_eq!(state.lock().unwrap().focused.as_slice(), ["42"]);
    }

    #[test]
    fn terminate_and_kill_signal_process() {
        let state = Arc::new(Mutex::new(FakeState::default()));
        let mut module = NiriWindowsModule::with_backend(
            HashMap::new(),
            Box::new(FakeWindowsBackend {
                state: Arc::clone(&state),
            }),
        );

        module.activate("42:99", TERMINATE_ACTION_ID).unwrap();
        module.activate("42:99", KILL_ACTION_ID).unwrap();

        assert_eq!(
            state.lock().unwrap().signaled.as_slice(),
            [(99, "TERM".to_string()), (99, "KILL".to_string())]
        );
    }
}
