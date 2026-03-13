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
    is_focused: bool,
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

        let mut results = windows
            .into_iter()
            .filter_map(|window| {
                let workspace = window.workspace_id.to_string();
                let base_score = if window.is_focused { 5 } else { 0 };
                let score = fuzzy::score_fields(
                    query,
                    &[(&window.title, 120), (&window.app_id, 70), (&workspace, 20)],
                )? + base_score;

                let subtitle = if window.app_id.is_empty() {
                    format!("workspace {}", window.workspace_id)
                } else {
                    format!("{}  workspace {}", window.app_id, window.workspace_id)
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
