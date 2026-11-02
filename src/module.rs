use std::time::Duration;

use anyhow::{Result, anyhow};

pub const DEFAULT_ACTION_ID: &str = "default";
pub const LAUNCH_ACTION_ID: &str = "launch";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LaunchWorkspace {
    pub id: u64,
    pub name: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ActivationOutcome {
    ClosePicker,
    RefreshResults,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum MatchKind {
    Application,
    CodexSession,
    Notification,
    Window,
    Workspace,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ResultAction {
    pub id: &'static str,
    #[allow(dead_code)]
    pub label: &'static str,
    pub shortcut: char,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct SearchResult {
    pub module_key: &'static str,
    pub item_id: String,
    pub title: String,
    pub subtitle: String,
    pub icon_name: Option<String>,
    pub kind: MatchKind,
    pub actions: Vec<ResultAction>,
    pub score: i64,
    pub application: Option<ApplicationMatch>,
    pub window: Option<WindowMatch>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ApplicationMatch {
    pub desktop_id: String,
    pub score: Option<i64>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct WindowMatch {
    pub workspace_id: u64,
    pub focus_timestamp: Option<Duration>,
    pub title_score: Option<i64>,
}

impl SearchResult {
    pub fn action_target(
        &self,
        action_id: &'static str,
    ) -> Option<(&'static str, &str, &'static str)> {
        if action_id == LAUNCH_ACTION_ID {
            let application = self.application.as_ref()?;
            Some(("applications", &application.desktop_id, DEFAULT_ACTION_ID))
        } else {
            Some((self.module_key, &self.item_id, action_id))
        }
    }
}

pub trait Module: Send {
    fn key(&self) -> &'static str;
    fn search(&mut self, query: &str) -> Result<Vec<SearchResult>>;
    fn activate(
        &mut self,
        item_id: &str,
        action_id: &str,
        workspace: Option<&LaunchWorkspace>,
    ) -> Result<ActivationOutcome>;
}

pub struct ModuleRegistry {
    modules: Vec<Box<dyn Module>>,
}

impl ModuleRegistry {
    pub fn new(modules: Vec<Box<dyn Module>>) -> Self {
        Self { modules }
    }

    pub fn search(&mut self, query: &str) -> Result<Vec<SearchResult>> {
        let mut results = Vec::new();

        for module in &mut self.modules {
            let mut module_results = module.search(query)?;
            results.append(&mut module_results);
        }

        sort_results(&mut results, None);

        Ok(results)
    }

    pub fn search_niri(&mut self, query: &str) -> Result<Vec<SearchResult>> {
        let mut results = Vec::new();
        for module in &mut self.modules {
            if matches!(module.key(), "niri-windows" | "niri-workspaces") {
                results.extend(module.search(query)?);
            }
        }
        sort_results(&mut results, None);
        Ok(results)
    }

    pub fn activate_action(
        &mut self,
        result: &SearchResult,
        action_id: &'static str,
        workspace: Option<&LaunchWorkspace>,
    ) -> Result<ActivationOutcome> {
        let (module_key, item_id, action_id) = result
            .action_target(action_id)
            .ok_or_else(|| anyhow!("no application associated with this result"))?;
        let module = self
            .modules
            .iter_mut()
            .find(|module| module.key() == module_key)
            .ok_or_else(|| anyhow!("Unknown module: {module_key}"))?;

        module.activate(item_id, action_id, workspace)
    }
}

pub fn sort_results(results: &mut [SearchResult], workspace_id: Option<u64>) {
    // Use a common application-name score before applying workspace preference.
    // Comparing only pairs from the same application would make the ordering
    // non-transitive when another application's results are interleaved.
    fn rank(result: &SearchResult, workspace_id: Option<u64>) -> (i64, u8) {
        let application_score = result.application.as_ref().and_then(|app| app.score);
        match (&result.window, application_score) {
            (Some(window), Some(score)) if window.title_score <= Some(score) => {
                let priority = if Some(window.workspace_id) == workspace_id {
                    0
                } else {
                    2
                };
                (score, priority)
            }
            (None, Some(score)) => (score, 1),
            (Some(window), _) => (result.score.max(window.title_score.unwrap_or(i64::MIN)), 1),
            _ => (result.score, 1),
        }
    }

    results.sort_by(|left, right| {
        let (left_score, left_priority) = rank(left, workspace_id);
        let (right_score, right_priority) = rank(right, workspace_id);
        right_score
            .cmp(&left_score)
            .then_with(|| left_priority.cmp(&right_priority))
            .then_with(|| left.kind.cmp(&right.kind))
            .then_with(|| {
                let recency = right
                    .window
                    .as_ref()
                    .and_then(|window| window.focus_timestamp)
                    .cmp(
                        &left
                            .window
                            .as_ref()
                            .and_then(|window| window.focus_timestamp),
                    );
                if left_priority == 0 {
                    recency.then_with(|| right.score.cmp(&left.score))
                } else {
                    right.score.cmp(&left.score).then(recency)
                }
            })
            .then_with(|| {
                if left.kind == MatchKind::CodexSession {
                    // Preserve the session module's recency order when match scores tie.
                    std::cmp::Ordering::Equal
                } else {
                    left.title.cmp(&right.title)
                }
            })
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeModule {
        key: &'static str,
        results: Vec<SearchResult>,
    }

    impl Module for FakeModule {
        fn key(&self) -> &'static str {
            self.key
        }

        fn search(&mut self, _query: &str) -> Result<Vec<SearchResult>> {
            Ok(self.results.clone())
        }

        fn activate(
            &mut self,
            _item_id: &str,
            _action_id: &str,
            _workspace: Option<&LaunchWorkspace>,
        ) -> Result<ActivationOutcome> {
            Ok(ActivationOutcome::ClosePicker)
        }
    }

    fn result(module_key: &'static str, kind: MatchKind, title: &str, score: i64) -> SearchResult {
        SearchResult {
            module_key,
            item_id: title.to_string(),
            title: title.to_string(),
            subtitle: String::new(),
            icon_name: None,
            kind,
            actions: Vec::new(),
            score,
            application: None,
            window: None,
        }
    }

    #[test]
    fn search_prefers_applications_when_scores_tie() {
        let mut registry = ModuleRegistry::new(vec![
            Box::new(FakeModule {
                key: "applications",
                results: vec![result(
                    "applications",
                    MatchKind::Application,
                    "Firefox",
                    300,
                )],
            }),
            Box::new(FakeModule {
                key: "niri-windows",
                results: vec![result("niri-windows", MatchKind::Window, "Firefox", 300)],
            }),
        ]);

        let results = registry.search("firefox").unwrap();

        assert_eq!(results[0].kind, MatchKind::Application);
        assert_eq!(results[1].kind, MatchKind::Window);
    }

    #[test]
    fn launch_action_routes_title_matched_windows_to_the_desktop_entry_with_workspace_context() {
        struct Launcher;

        impl Module for Launcher {
            fn key(&self) -> &'static str {
                "applications"
            }

            fn search(&mut self, _query: &str) -> Result<Vec<SearchResult>> {
                Ok(Vec::new())
            }

            fn activate(
                &mut self,
                item_id: &str,
                action_id: &str,
                workspace: Option<&LaunchWorkspace>,
            ) -> Result<ActivationOutcome> {
                assert_eq!(item_id, "firefox.desktop");
                assert_eq!(action_id, DEFAULT_ACTION_ID);
                assert_eq!(
                    workspace,
                    Some(&LaunchWorkspace {
                        id: 10,
                        name: Some("project".to_owned())
                    })
                );
                Ok(ActivationOutcome::ClosePicker)
            }
        }

        let mut window = result("niri-windows", MatchKind::Window, "Quarterly budget", 290);
        window.application = Some(ApplicationMatch {
            desktop_id: "firefox.desktop".to_owned(),
            score: None,
        });
        let mut registry = ModuleRegistry::new(vec![Box::new(Launcher)]);
        let workspace = LaunchWorkspace {
            id: 10,
            name: Some("project".to_owned()),
        };
        assert_eq!(
            registry
                .activate_action(&window, LAUNCH_ACTION_ID, Some(&workspace))
                .unwrap(),
            ActivationOutcome::ClosePicker
        );
        assert!(
            registry
                .activate_action(&window, DEFAULT_ACTION_ID, Some(&workspace))
                .is_err()
        );
    }
}
