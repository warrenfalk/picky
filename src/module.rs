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
