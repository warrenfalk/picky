use std::collections::HashMap;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::fuzzy;
use crate::module::{ActivationOutcome, DEFAULT_ACTION_ID, MatchKind, Module, SearchResult};

const MODULE_KEY: &str = "niri-windows";

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
                    item_id: window.id.to_string(),
                    title: window.title,
                    subtitle,
                    icon_name: icon_name_for_app_id(&self.icon_index, &window.app_id),
                    kind: MatchKind::Window,
                    actions: Vec::new(),
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
        if action_id != DEFAULT_ACTION_ID {
            anyhow::bail!("unknown window action: {action_id}");
        }

        let status = Command::new("niri")
            .args(["msg", "action", "focus-window", "--id", item_id])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .with_context(|| format!("failed to focus niri window {item_id}"))?;

        if !status.success() {
            bail!("niri refused to focus window {item_id}");
        }

        Ok(ActivationOutcome::ClosePicker)
    }
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
