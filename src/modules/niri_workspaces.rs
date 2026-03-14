use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};
use serde::Deserialize;

use crate::fuzzy;
use crate::module::{ActivationOutcome, MatchKind, Module, SearchResult, DEFAULT_ACTION_ID};

const MODULE_KEY: &str = "niri-workspaces";

pub struct NiriWorkspacesModule;

#[derive(Clone, Debug, Deserialize)]
struct NiriWorkspace {
    id: u64,
    idx: u64,
    name: Option<String>,
    output: String,
    #[serde(default)]
    is_active: bool,
    #[serde(default)]
    is_focused: bool,
}

impl NiriWorkspacesModule {
    pub fn new() -> Self {
        Self
    }
}

impl Module for NiriWorkspacesModule {
    fn key(&self) -> &'static str {
        MODULE_KEY
    }

    fn search(&mut self, query: &str) -> Result<Vec<SearchResult>> {
        let query = query.trim();
        let mut workspaces = load_workspaces()?;

        workspaces.sort_by(|left, right| {
            left.output
                .cmp(&right.output)
                .then_with(|| left.idx.cmp(&right.idx))
                .then_with(|| left.name.cmp(&right.name))
                .then_with(|| left.id.cmp(&right.id))
        });

        let mut results = workspaces
            .into_iter()
            .filter_map(|workspace| {
                let title = workspace_title(&workspace);
                let output_label = format!("on {}", workspace.output);
                let score = fuzzy::score_fields(
                    query,
                    &[
                        (&title, 100),
                        (&workspace.output, 50),
                        (&workspace.idx.to_string(), 25),
                    ],
                )?;

                Some(SearchResult {
                    module_key: MODULE_KEY,
                    item_id: workspace.id.to_string(),
                    title,
                    subtitle: output_label,
                    icon_name: None,
                    kind: MatchKind::Workspace,
                    actions: Vec::new(),
                    score: if query.is_empty() {
                        score + workspace_empty_query_bonus(&workspace)
                    } else {
                        score
                    },
                })
            })
            .collect::<Vec<_>>();

        results.sort_by(|left, right| {
            right
                .score
                .cmp(&left.score)
                .then_with(|| left.title.cmp(&right.title))
                .then_with(|| left.subtitle.cmp(&right.subtitle))
        });

        Ok(results)
    }

    fn activate(&mut self, item_id: &str, action_id: &str) -> Result<ActivationOutcome> {
        if action_id != DEFAULT_ACTION_ID {
            anyhow::bail!("unknown workspace action: {action_id}");
        }

        let workspace_id = item_id
            .parse::<u64>()
            .with_context(|| format!("invalid workspace id: {item_id}"))?;
        let workspace = load_workspaces()?
            .into_iter()
            .find(|workspace| workspace.id == workspace_id)
            .ok_or_else(|| anyhow::anyhow!("workspace {workspace_id} no longer exists"))?;

        run_niri_action(["focus-monitor", workspace.output.as_str()])?;

        let reference = workspace
            .name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| workspace.idx.to_string());
        run_niri_action(["focus-workspace", reference.as_str()])?;

        Ok(ActivationOutcome::ClosePicker)
    }
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

fn workspace_title(workspace: &NiriWorkspace) -> String {
    workspace
        .name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(|name| format!("Workspace: \"{name}\""))
        .unwrap_or_else(|| format!("Workspace: [{}]", workspace.idx))
}

fn workspace_empty_query_bonus(workspace: &NiriWorkspace) -> i64 {
    if workspace.is_focused {
        15
    } else if workspace.is_active {
        10
    } else {
        0
    }
}

fn run_niri_action(args: [&str; 2]) -> Result<()> {
    let status = Command::new("niri")
        .args(["msg", "action"])
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .with_context(|| format!("failed to run `niri msg action {}`", args.join(" ")))?;

    if !status.success() {
        bail!("`niri msg action {}` failed", args.join(" "));
    }

    Ok(())
}
