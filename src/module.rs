use anyhow::{Result, anyhow};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum MatchKind {
    Application,
    Notification,
    Window,
}

#[derive(Clone, Debug)]
pub struct SearchResult {
    pub module_key: &'static str,
    pub item_id: String,
    pub title: String,
    pub subtitle: String,
    pub icon_name: Option<String>,
    pub kind: MatchKind,
    pub score: i64,
}

pub trait Module {
    fn key(&self) -> &'static str;
    fn search(&mut self, query: &str) -> Result<Vec<SearchResult>>;
    fn activate(&mut self, item_id: &str) -> Result<()>;
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

    pub fn activate(&mut self, result: &SearchResult) -> Result<()> {
        let module = self
            .modules
            .iter_mut()
            .find(|module| module.key() == result.module_key)
            .ok_or_else(|| anyhow!("Unknown module: {}", result.module_key))?;

        module.activate(&result.item_id)
    }
}
