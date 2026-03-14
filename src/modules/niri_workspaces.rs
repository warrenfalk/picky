use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};
use serde::Deserialize;

use crate::fuzzy;
use crate::module::{ActivationOutcome, MatchKind, Module, SearchResult, DEFAULT_ACTION_ID};

const MODULE_KEY: &str = "niri-workspaces";

pub struct NiriWorkspacesModule {
    backend: Box<dyn WorkspaceBackend>,
}

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

trait WorkspaceBackend: Send {
    fn load_workspaces(&self) -> Result<Vec<NiriWorkspace>>;
    fn run_action(&self, action: &str, target: &str) -> Result<()>;
}

struct ProcessWorkspaceBackend;

impl WorkspaceBackend for ProcessWorkspaceBackend {
    fn load_workspaces(&self) -> Result<Vec<NiriWorkspace>> {
        load_workspaces()
    }

    fn run_action(&self, action: &str, target: &str) -> Result<()> {
        run_niri_action([action, target])
    }
}

impl NiriWorkspacesModule {
    pub fn new() -> Self {
        Self::with_backend(Box::new(ProcessWorkspaceBackend))
    }

    fn with_backend(backend: Box<dyn WorkspaceBackend>) -> Self {
        Self { backend }
    }
}

impl Module for NiriWorkspacesModule {
    fn key(&self) -> &'static str {
        MODULE_KEY
    }

    fn search(&mut self, query: &str) -> Result<Vec<SearchResult>> {
        let query = query.trim();
        let mut workspaces = self.backend.load_workspaces()?;

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
        let workspace = self
            .backend
            .load_workspaces()?
            .into_iter()
            .find(|workspace| workspace.id == workspace_id)
            .ok_or_else(|| anyhow::anyhow!("workspace {workspace_id} no longer exists"))?;

        self.backend
            .run_action("focus-monitor", workspace.output.as_str())?;

        let reference = workspace
            .name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| workspace.idx.to_string());
        self.backend
            .run_action("focus-workspace", reference.as_str())?;

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct FakeState {
        workspaces: Vec<NiriWorkspace>,
        actions: Vec<(String, String)>,
    }

    struct FakeWorkspaceBackend {
        state: Arc<Mutex<FakeState>>,
    }

    impl WorkspaceBackend for FakeWorkspaceBackend {
        fn load_workspaces(&self) -> Result<Vec<NiriWorkspace>> {
            Ok(self.state.lock().unwrap().workspaces.clone())
        }

        fn run_action(&self, action: &str, target: &str) -> Result<()> {
            self.state
                .lock()
                .unwrap()
                .actions
                .push((action.to_string(), target.to_string()));
            Ok(())
        }
    }

    fn workspace(
        id: u64,
        idx: u64,
        name: Option<&str>,
        output: &str,
        is_active: bool,
        is_focused: bool,
    ) -> NiriWorkspace {
        NiriWorkspace {
            id,
            idx,
            name: name.map(ToOwned::to_owned),
            output: output.to_string(),
            is_active,
            is_focused,
        }
    }

    #[test]
    fn search_empty_query_boosts_focused_workspace() {
        let state = Arc::new(Mutex::new(FakeState {
            workspaces: vec![
                workspace(1, 1, Some("chat"), "DP-1", false, false),
                workspace(2, 2, Some("code"), "DP-1", true, true),
            ],
            ..FakeState::default()
        }));
        let mut module = NiriWorkspacesModule::with_backend(Box::new(FakeWorkspaceBackend {
            state,
        }));

        let results = module.search("").unwrap();

        assert_eq!(results[0].item_id, "2");
    }

    #[test]
    fn activate_focuses_monitor_then_named_workspace() {
        let state = Arc::new(Mutex::new(FakeState {
            workspaces: vec![workspace(2, 2, Some("code"), "DP-1", true, true)],
            ..FakeState::default()
        }));
        let mut module = NiriWorkspacesModule::with_backend(Box::new(FakeWorkspaceBackend {
            state: Arc::clone(&state),
        }));

        let outcome = module.activate("2", DEFAULT_ACTION_ID).unwrap();

        assert_eq!(outcome, ActivationOutcome::ClosePicker);
        assert_eq!(
            state.lock().unwrap().actions.as_slice(),
            [
                ("focus-monitor".to_string(), "DP-1".to_string()),
                ("focus-workspace".to_string(), "code".to_string())
            ]
        );
    }

    #[test]
    fn activate_falls_back_to_workspace_index() {
        let state = Arc::new(Mutex::new(FakeState {
            workspaces: vec![workspace(3, 7, None, "DP-2", false, false)],
            ..FakeState::default()
        }));
        let mut module = NiriWorkspacesModule::with_backend(Box::new(FakeWorkspaceBackend {
            state: Arc::clone(&state),
        }));

        module.activate("3", DEFAULT_ACTION_ID).unwrap();

        assert_eq!(
            state.lock().unwrap().actions[1],
            ("focus-workspace".to_string(), "7".to_string())
        );
    }
}
