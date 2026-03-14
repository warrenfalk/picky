use std::collections::HashMap;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serde::Deserialize;

use crate::fuzzy;
use crate::module::{
    ActivationOutcome, MatchKind, Module, ResultAction, SearchResult, DEFAULT_ACTION_ID,
};

const MODULE_KEY: &str = "niri-windows";
const CLOSE_ACTION_ID: &str = "close";
const TERMINATE_ACTION_ID: &str = "terminate";
const KILL_ACTION_ID: &str = "kill";
const CLOSE_POLL_TIMEOUT: Duration = Duration::from_millis(750);
const CLOSE_POLL_INTERVAL: Duration = Duration::from_millis(50);

pub struct NiriWindowsModule {
    icon_index: HashMap<String, String>,
}

#[derive(Debug, Deserialize)]
struct NiriWindow {
    id: u64,
    #[serde(default)]
    title: String,
    #[serde(default)]
    app_id: String,
    pid: Option<u32>,
    workspace_id: u64,
}

#[derive(Debug, Deserialize)]
struct NiriWorkspace {
    id: u64,
    idx: u64,
    name: Option<String>,
    output: String,
}

impl NiriWindowsModule {
    pub fn new() -> Self {
        Self {
            icon_index: crate::modules::applications::load_icon_index().unwrap_or_else(|err| {
                eprintln!("failed to load application icon index: {err:#}");
                HashMap::new()
            }),
        }
    }
}

impl Module for NiriWindowsModule {
    fn key(&self) -> &'static str {
        MODULE_KEY
    }

    fn search(&mut self, query: &str) -> Result<Vec<SearchResult>> {
        let query = query.trim();
        let windows = load_windows()?;
        let workspaces = load_workspaces()?
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
                let score = fuzzy::score_fields(
                    query,
                    &[
                        (&window.title, 120),
                        (&window.app_id, 70),
                        (&workspace_label, 30),
                        (output_name, 20),
                    ],
                )?;

                let subtitle = if window.app_id.is_empty() {
                    format!("{workspace_label} on {output_name}")
                } else {
                    format!("{}  {} on {}", window.app_id, workspace_label, output_name)
                };

                Some(SearchResult {
                    module_key: MODULE_KEY,
                    item_id: encode_window_target(window.id, window.pid),
                    title: window.title,
                    subtitle,
                    icon_name: icon_name_for_app_id(&self.icon_index, &window.app_id),
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
                focus_window(target.window_id)?;
                Ok(ActivationOutcome::ClosePicker)
            }
            CLOSE_ACTION_ID => {
                close_window(target.window_id)?;
                wait_for_close_or_focus(target.window_id)
            }
            TERMINATE_ACTION_ID => {
                signal_window_process(target.pid, "TERM")?;
                Ok(ActivationOutcome::ClosePicker)
            }
            KILL_ACTION_ID => {
                signal_window_process(target.pid, "KILL")?;
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

fn wait_for_close_or_focus(window_id: &str) -> Result<ActivationOutcome> {
    let deadline = Instant::now() + CLOSE_POLL_TIMEOUT;

    while Instant::now() < deadline {
        if !window_exists(window_id)? {
            return Ok(ActivationOutcome::RefreshResults);
        }

        thread::sleep(CLOSE_POLL_INTERVAL);
    }

    if window_exists(window_id)? {
        focus_window(window_id)?;
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

fn signal_window_process(pid: Option<u32>, signal: &str) -> Result<()> {
    let Some(pid) = pid else {
        bail!("window has no process id available");
    };

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

fn icon_name_for_app_id(icon_index: &HashMap<String, String>, app_id: &str) -> Option<String> {
    let app_id = app_id.trim();
    if app_id.is_empty() {
        return None;
    }

    icon_index
        .get(app_id)
        .or_else(|| {
            let desktop_id = format!("{app_id}.desktop");
            icon_index.get(&desktop_id)
        })
        .cloned()
}
