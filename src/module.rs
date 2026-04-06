use anyhow::{Result, anyhow};

pub const DEFAULT_ACTION_ID: &str = "default";

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ActivationOutcome {
    ClosePicker,
    RefreshResults,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum MatchKind {
    Application,
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
}

pub trait Module: Send {
    fn key(&self) -> &'static str;
    fn search(&mut self, query: &str) -> Result<Vec<SearchResult>>;
    fn activate(&mut self, item_id: &str, action_id: &str) -> Result<ActivationOutcome>;
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

        results.sort_by(|left, right| {
            right
                .score
                .cmp(&left.score)
                .then_with(|| left.kind.cmp(&right.kind))
                .then_with(|| left.title.cmp(&right.title))
        });

        Ok(results)
    }

    pub fn activate(&mut self, result: &SearchResult) -> Result<ActivationOutcome> {
        self.activate_action(result, DEFAULT_ACTION_ID)
    }

    pub fn activate_action(
        &mut self,
        result: &SearchResult,
        action_id: &str,
    ) -> Result<ActivationOutcome> {
        let module = self
            .modules
            .iter_mut()
            .find(|module| module.key() == result.module_key)
            .ok_or_else(|| anyhow!("Unknown module: {}", result.module_key))?;

        module.activate(&result.item_id, action_id)
    }
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

        fn activate(&mut self, _item_id: &str, _action_id: &str) -> Result<ActivationOutcome> {
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
}
