use std::collections::{HashMap, HashSet};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};

use crate::fuzzy;
use crate::module::{
    ActivationOutcome, ApplicationMatch, DEFAULT_ACTION_ID, LaunchWorkspace, MatchKind, Module,
    SearchResult,
};

const MODULE_KEY: &str = "applications";

pub struct ApplicationsModule {
    store: ApplicationStore,
    launcher: Box<dyn ApplicationLauncher>,
}

#[derive(Clone, Debug, Default)]
pub struct ApplicationStore(Arc<Mutex<Arc<ApplicationCatalog>>>);

#[derive(Debug, Default, Eq, PartialEq)]
pub struct ApplicationCatalog {
    entries: Vec<ApplicationEntry>,
    pub metadata: HashMap<String, ApplicationMetadata>,
    // Detect profile switches even when the new desktop metadata is identical.
    sources: Vec<PathBuf>,
}

impl ApplicationStore {
    pub fn snapshot(&self) -> Arc<ApplicationCatalog> {
        self.0.lock().unwrap().clone()
    }

    pub fn replace(&self, catalog: Arc<ApplicationCatalog>) -> bool {
        let mut current = self.0.lock().unwrap();
        let changed = *current != catalog;
        *current = catalog;
        changed
    }

    #[cfg(test)]
    pub(super) fn with_metadata(metadata: HashMap<String, ApplicationMetadata>) -> Self {
        Self(Arc::new(Mutex::new(Arc::new(ApplicationCatalog {
            metadata,
            ..ApplicationCatalog::default()
        }))))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ApplicationMetadata {
    pub desktop_id: String,
    pub name: String,
    pub icon_name: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ApplicationEntry {
    id: String,
    name: String,
    comment: String,
    keywords: String,
    icon_name: Option<String>,
    startup_wm_class: Option<String>,
}

trait ApplicationLauncher: Send {
    fn launch(&self, item_id: &str, workspace: Option<&LaunchWorkspace>) -> Result<()>;
}

struct ProcessApplicationLauncher;

impl ApplicationLauncher for ProcessApplicationLauncher {
    fn launch(&self, item_id: &str, workspace: Option<&LaunchWorkspace>) -> Result<()> {
        launch_command(item_id, workspace)
            .spawn()
            .with_context(|| format!("failed to launch desktop entry {item_id}"))?;

        Ok(())
    }
}

fn launch_command(item_id: &str, workspace: Option<&LaunchWorkspace>) -> Command {
    let mut command = Command::new("gtk-launch");
    command
        .arg(item_id)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .env_remove("NIRI_LAUNCHED_FROM_WORKSPACE")
        .env_remove("NIRI_LAUNCHED_FROM_WORKSPACE_ID");
    if let Some(workspace) = workspace {
        command.env("NIRI_LAUNCHED_FROM_WORKSPACE_ID", workspace.id.to_string());
        if let Some(name) = &workspace.name {
            command.env("NIRI_LAUNCHED_FROM_WORKSPACE", name);
        }
    }
    command
}

pub(super) fn application_match(query: &str, name: &str, desktop_id: &str) -> ApplicationMatch {
    ApplicationMatch {
        desktop_id: desktop_id.to_owned(),
        score: fuzzy::score_fields(query, &[(name, 110)])
            .max(fuzzy::score_fields(query, &[(desktop_id, 30)])),
    }
}

impl ApplicationsModule {
    pub fn new(store: ApplicationStore) -> Self {
        Self {
            store,
            launcher: Box::new(ProcessApplicationLauncher),
        }
    }

    #[cfg(test)]
    fn with_launcher(
        entries: Vec<ApplicationEntry>,
        launcher: Box<dyn ApplicationLauncher>,
    ) -> Self {
        let store = ApplicationStore::default();
        store.replace(Arc::new(ApplicationCatalog {
            metadata: build_application_index(entries.clone()),
            entries,
            sources: Vec::new(),
        }));
        Self { store, launcher }
    }
}

pub fn load_catalog() -> Result<Arc<ApplicationCatalog>> {
    load_catalog_from(&application_dirs())
}

fn load_catalog_from(roots: &[PathBuf]) -> Result<Arc<ApplicationCatalog>> {
    // Resolve each root once so a profile switch cannot mix generations within
    // a scan. A later watcher event (or reopening) picks up the next generation.
    let mut sources = Vec::new();
    for root in roots {
        match root.canonicalize() {
            Ok(source) => sources.push(source),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| format!("failed to resolve {}", root.display()));
            }
        }
    }
    let entries = load_entries(&sources)?;
    Ok(Arc::new(ApplicationCatalog {
        metadata: build_application_index(entries.clone()),
        entries,
        sources,
    }))
}

fn build_application_index(entries: Vec<ApplicationEntry>) -> HashMap<String, ApplicationMetadata> {
    let mut applications = HashMap::new();

    for entry in &entries {
        insert_application_aliases(
            &mut applications,
            &entry.id,
            ApplicationMetadata {
                desktop_id: entry.id.clone(),
                name: entry.name.clone(),
                icon_name: entry.icon_name.clone().filter(|icon| !icon.is_empty()),
            },
        );
    }

    // Explicit desktop IDs win over aliases shared by multiple entries.
    for entry in &entries {
        if let Some(alias) = &entry.startup_wm_class {
            let metadata = applications[&entry.id].clone();
            applications.entry(alias.clone()).or_insert(metadata);
        }
    }

    applications
}

fn insert_application_aliases(
    applications: &mut HashMap<String, ApplicationMetadata>,
    desktop_id: &str,
    metadata: ApplicationMetadata,
) {
    applications
        .entry(desktop_id.to_string())
        .or_insert_with(|| metadata.clone());

    if let Some(stripped) = desktop_id.strip_suffix(".desktop") {
        applications.entry(stripped.to_string()).or_insert(metadata);
    }
}

impl Module for ApplicationsModule {
    fn key(&self) -> &'static str {
        MODULE_KEY
    }

    fn search(&mut self, query: &str) -> Result<Vec<SearchResult>> {
        let query = query.trim();

        let catalog = self.store.snapshot();
        let mut results = catalog
            .entries
            .iter()
            .filter_map(|entry| {
                let score = fuzzy::score_fields(
                    query,
                    &[
                        (&entry.name, 110),
                        (&entry.keywords, 85),
                        (&entry.comment, 45),
                        (&entry.id, 30),
                    ],
                )?;

                Some(SearchResult {
                    module_key: MODULE_KEY,
                    item_id: entry.id.clone(),
                    title: format!("Launch {}", entry.name),
                    subtitle: if entry.comment.is_empty() {
                        entry.id.clone()
                    } else {
                        entry.comment.clone()
                    },
                    icon_name: entry.icon_name.clone(),
                    kind: MatchKind::Application,
                    actions: Vec::new(),
                    score,
                    application: Some(application_match(query, &entry.name, &entry.id)),
                    window: None,
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

    fn activate(
        &mut self,
        item_id: &str,
        action_id: &str,
        workspace: Option<&LaunchWorkspace>,
    ) -> Result<ActivationOutcome> {
        if action_id != DEFAULT_ACTION_ID {
            anyhow::bail!("unknown application action: {action_id}");
        }

        self.launcher.launch(item_id, workspace)?;
        Ok(ActivationOutcome::ClosePicker)
    }
}

fn load_entries(roots: &[PathBuf]) -> Result<Vec<ApplicationEntry>> {
    let mut entries = Vec::new();
    let mut seen = HashSet::new();

    for applications_dir in roots {
        if !applications_dir.exists() {
            continue;
        }

        let mut desktop_files = Vec::new();
        collect_desktop_files(applications_dir, &mut desktop_files)?;

        for desktop_file in desktop_files {
            let Some(entry) = parse_entry(applications_dir, &desktop_file)? else {
                continue;
            };

            if seen.insert(entry.id.clone()) {
                entries.push(entry);
            }
        }
    }

    Ok(entries)
}

pub(super) fn application_dirs() -> Vec<PathBuf> {
    let mut roots = Vec::new();

    let data_home = env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")));

    if let Some(data_home) = data_home {
        roots.push(data_home.join("applications"));
    }

    let data_dirs = env::var("XDG_DATA_DIRS")
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".to_string());

    roots.extend(
        data_dirs
            .split(':')
            .filter(|segment| !segment.is_empty())
            .map(|segment| PathBuf::from(segment).join("applications")),
    );

    roots
}

fn collect_desktop_files(dir: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(dir).with_context(|| format!("failed to read {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();

        if path.is_dir() {
            collect_desktop_files(&path, files)?;
            continue;
        }

        if path.extension().and_then(|extension| extension.to_str()) == Some("desktop") {
            files.push(path);
        }
    }

    files.sort();
    Ok(())
}

fn parse_entry(applications_dir: &Path, desktop_file: &Path) -> Result<Option<ApplicationEntry>> {
    let contents = fs::read_to_string(desktop_file)
        .with_context(|| format!("failed to read {}", desktop_file.display()))?;
    Ok(parse_entry_contents(
        &contents,
        applications_dir,
        desktop_file,
    ))
}

fn parse_entry_contents(
    contents: &str,
    applications_dir: &Path,
    desktop_file: &Path,
) -> Option<ApplicationEntry> {
    let mut in_desktop_entry = false;
    let mut name = None;
    let mut comment = None;
    let mut keywords = None;
    let mut icon_name = None;
    let mut startup_wm_class = None;
    let mut exec = None;
    let mut no_display = false;
    let mut hidden = false;
    let mut entry_type = None;

    for raw_line in contents.lines() {
        let line = raw_line.trim();

        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        if line.starts_with('[') && line.ends_with(']') {
            in_desktop_entry = line == "[Desktop Entry]";
            continue;
        }

        if !in_desktop_entry {
            continue;
        }

        let Some((key, value)) = line.split_once('=') else {
            continue;
        };

        match key {
            "Name" if name.is_none() => name = Some(value.trim().to_string()),
            "Comment" if comment.is_none() => comment = Some(value.trim().to_string()),
            "Keywords" if keywords.is_none() => {
                keywords = Some(value.split(';').collect::<Vec<_>>().join(" "))
            }
            "Icon" if icon_name.is_none() => icon_name = Some(value.trim().to_string()),
            "StartupWMClass" if startup_wm_class.is_none() && !value.trim().is_empty() => {
                startup_wm_class = Some(value.trim().to_string())
            }
            "Exec" if exec.is_none() => exec = Some(value.trim().to_string()),
            "NoDisplay" => no_display = parse_bool(value),
            "Hidden" => hidden = parse_bool(value),
            "Type" if entry_type.is_none() => entry_type = Some(value.trim().to_string()),
            _ => {}
        }
    }

    if hidden
        || no_display
        || exec.is_none()
        || matches!(entry_type.as_deref(), Some(entry_type) if entry_type != "Application")
    {
        return None;
    }

    let Some(id) = desktop_entry_id(applications_dir, desktop_file) else {
        return None;
    };

    let default_name = desktop_file
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("unknown")
        .to_string();

    Some(ApplicationEntry {
        id,
        name: name.unwrap_or(default_name),
        comment: comment.unwrap_or_default(),
        keywords: keywords.unwrap_or_default(),
        icon_name,
        startup_wm_class,
    })
}

fn desktop_entry_id(applications_dir: &Path, desktop_file: &Path) -> Option<String> {
    let relative = desktop_file.strip_prefix(applications_dir).ok()?;
    let components = relative
        .components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>();

    Some(components.join("-"))
}

fn parse_bool(value: &str) -> bool {
    matches!(value.trim(), "true" | "True" | "1")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::module::{ModuleRegistry, sort_results};
    use crate::modules::{niri_feed, niri_windows::NiriWindowsModule};
    use serde_json::{Value, json};
    use std::ffi::OsStr;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    struct FakeLauncher {
        launches: Arc<Mutex<Vec<String>>>,
    }

    impl ApplicationLauncher for FakeLauncher {
        fn launch(&self, item_id: &str, _workspace: Option<&LaunchWorkspace>) -> Result<()> {
            self.launches.lock().unwrap().push(item_id.to_string());
            Ok(())
        }
    }

    fn entry(id: &str, name: &str, comment: &str, keywords: &str) -> ApplicationEntry {
        ApplicationEntry {
            id: id.to_string(),
            name: name.to_string(),
            comment: comment.to_string(),
            keywords: keywords.to_string(),
            icon_name: None,
            startup_wm_class: None,
        }
    }

    fn window_registry(mut windows: Vec<Value>) -> (ModuleRegistry, niri_feed::Store) {
        let mut firefox = entry("firefox.desktop", "Firefox", "Web Browser", "browser web");
        firefox.startup_wm_class = Some("Navigator".to_owned());
        let applications = ApplicationsModule::with_launcher(
            vec![
                firefox,
                entry("budget-manager.desktop", "Budget Manager", "", "finance"),
            ],
            Box::new(ProcessApplicationLauncher),
        );
        let feed = niri_feed::Store::default();
        windows.push(
            json!({"id": 99, "app_id": "picky", "pid": std::process::id(), "workspace_id": 10}),
        );
        feed.apply(Ok(json!({"WindowsChanged": {"windows": windows}})));
        // Focus is deliberately elsewhere: ranking must use Picky's window.
        feed.apply(Ok(json!({"WorkspacesChanged": {"workspaces": [
            {"id": 10, "idx": 1, "name": "code", "output": "DP-1"},
            {"id": 20, "idx": 1, "name": "other", "output": "DP-2", "is_focused": true}
        ]}})));
        let windows = NiriWindowsModule::with_store(feed.clone(), applications.store.clone());
        (
            ModuleRegistry::new(vec![Box::new(applications), Box::new(windows)]),
            feed,
        )
    }

    #[test]
    fn application_search_orders_local_windows_by_recency_then_launch_then_remote_windows() {
        let (mut registry, feed) = window_registry(vec![
            json!({"id": 1, "title": "A Firefox", "app_id": "firefox", "pid": 111,
                "workspace_id": 10, "focus_timestamp": {"secs": 10, "nanos": 0}}),
            json!({"id": 2, "title": "Z notes", "app_id": "Navigator", "pid": 222,
                "workspace_id": 10, "focus_timestamp": {"secs": 20, "nanos": 0}}),
            json!({"id": 3, "title": "Firefox", "app_id": "firefox", "pid": 333,
                "workspace_id": 20, "focus_timestamp": {"secs": 30, "nanos": 0}}),
        ]);
        let mut results = registry.search("firefox").unwrap();
        sort_results(&mut results, feed.picker_workspace_id());
        assert_eq!(
            results
                .iter()
                .map(|result| result.item_id.as_str())
                .collect::<Vec<_>>(),
            ["2:222", "1:111", "firefox.desktop", "3:333"]
        );
        assert_eq!(results[2].title, "Launch Firefox");

        feed.apply(Ok(json!({"WindowFocusTimestampChanged": {
            "id": 1, "focus_timestamp": {"secs": 40, "nanos": 0}
        }})));
        let mut results = registry.search("firefox").unwrap();
        sort_results(&mut results, feed.picker_workspace_id());
        assert_eq!(results[0].item_id, "1:111");

        feed.apply(Ok(json!({"WindowOpenedOrChanged": {"window": {
            "id": 99, "app_id": "picky", "pid": std::process::id(), "workspace_id": 20
        }}})));
        sort_results(&mut results, feed.picker_workspace_id());
        assert_eq!(results[0].item_id, "3:333");
        assert_eq!(results[1].item_id, "firefox.desktop");
    }

    #[test]
    fn application_launch_precedes_windows_when_none_are_on_picker_workspace() {
        let (mut registry, feed) = window_registry(vec![json!({
            "id": 1, "title": "Firefox", "app_id": "firefox", "pid": 111, "workspace_id": 20
        })]);
        let mut results = registry.search("firefox").unwrap();
        sort_results(&mut results, feed.picker_workspace_id());
        assert_eq!(results[0].item_id, "firefox.desktop");
        assert_eq!(results[1].item_id, "1:111");
    }

    #[test]
    fn specific_remote_title_precedes_local_application_name_matches() {
        let (mut registry, feed) = window_registry(vec![
            json!({"id": 1, "title": "Schedule", "app_id": "budget-manager", "pid": 111, "workspace_id": 10}),
            json!({"id": 2, "title": "Budget", "app_id": "firefox", "pid": 222, "workspace_id": 20}),
        ]);
        let mut results = registry.search("budget").unwrap();
        sort_results(&mut results, feed.picker_workspace_id());
        assert_eq!(
            results
                .iter()
                .map(|result| result.item_id.as_str())
                .collect::<Vec<_>>(),
            ["2:222", "1:111", "budget-manager.desktop"]
        );
    }

    #[test]
    fn mixed_application_and_title_query_finds_remote_window() {
        let (mut registry, feed) = window_registry(vec![json!({
            "id": 1, "title": "Quarterly budget", "app_id": "firefox", "pid": 111, "workspace_id": 20
        })]);
        let mut results = registry.search("firefox budget").unwrap();
        sort_results(&mut results, feed.picker_workspace_id());
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].item_id, "1:111");
    }

    #[test]
    fn launch_environment_replaces_stale_workspace_values_and_preserves_unnamed_workspace_id() {
        for (workspace, expected_name, expected_id) in [
            (
                Some(LaunchWorkspace {
                    id: 7,
                    name: Some("Project A 'quoted'".to_owned()),
                }),
                Some(OsStr::new("Project A 'quoted'")),
                Some(OsStr::new("7")),
            ),
            (
                Some(LaunchWorkspace { id: 8, name: None }),
                None,
                Some(OsStr::new("8")),
            ),
            (None, None, None),
        ] {
            let command = launch_command("firefox.desktop", workspace.as_ref());
            let environment = command.get_envs().collect::<HashMap<_, _>>();
            assert_eq!(
                environment.get(OsStr::new("NIRI_LAUNCHED_FROM_WORKSPACE")),
                Some(&expected_name)
            );
            assert_eq!(
                environment.get(OsStr::new("NIRI_LAUNCHED_FROM_WORKSPACE_ID")),
                Some(&expected_id)
            );
        }
    }

    #[test]
    fn startup_wm_class_alias_resolves_to_canonical_desktop_entry() {
        let parsed = parse_entry_contents(
            "[Desktop Entry]\nType=Application\nName=Firefox\nExec=firefox\nStartupWMClass=Navigator\n",
            Path::new("/tmp/apps"), Path::new("/tmp/apps/firefox.desktop"),
        ).unwrap();
        let applications = build_application_index(vec![parsed]);
        assert_eq!(applications["Navigator"].desktop_id, "firefox.desktop");
    }

    #[test]
    fn parse_entry_contents_extracts_desktop_entry_fields() {
        let contents = r#"
            [Desktop Entry]
            Type=Application
            Name=Firefox
            Comment=Web Browser
            Keywords=browser;web;
            Icon=firefox
            Exec=firefox
        "#;

        let entry = parse_entry_contents(
            contents,
            &PathBuf::from("/tmp/apps"),
            &PathBuf::from("/tmp/apps/firefox.desktop"),
        )
        .unwrap();

        assert_eq!(entry.id, "firefox.desktop");
        assert_eq!(entry.name, "Firefox");
        assert_eq!(entry.comment, "Web Browser");
        assert_eq!(entry.keywords, "browser web ");
        assert_eq!(entry.icon_name.as_deref(), Some("firefox"));
    }

    #[test]
    fn parse_entry_contents_skips_hidden_entries() {
        let contents = r#"
            [Desktop Entry]
            Type=Application
            Name=Hidden
            Exec=hidden
            Hidden=true
        "#;

        let entry = parse_entry_contents(
            contents,
            &PathBuf::from("/tmp/apps"),
            &PathBuf::from("/tmp/apps/hidden.desktop"),
        );

        assert!(entry.is_none());
    }

    #[test]
    fn search_orders_better_match_first() {
        let mut module = ApplicationsModule::with_launcher(
            vec![
                entry("org.gnome.Calculator.desktop", "Calculator", "", "math"),
                entry(
                    "org.mozilla.Firefox.desktop",
                    "Firefox",
                    "Web Browser",
                    "browser web",
                ),
            ],
            Box::new(ProcessApplicationLauncher),
        );

        let results = module.search("fire").unwrap();

        assert_eq!(results[0].title, "Launch Firefox");
        assert_eq!(results[0].module_key, MODULE_KEY);
    }

    #[test]
    fn build_application_index_adds_stripped_desktop_aliases() {
        let applications = build_application_index(vec![entry(
            "org.mozilla.Firefox.desktop",
            "Firefox",
            "Web Browser",
            "browser web",
        )]);

        assert_eq!(
            applications.get("org.mozilla.Firefox.desktop"),
            Some(&ApplicationMetadata {
                desktop_id: "org.mozilla.Firefox.desktop".to_owned(),
                name: "Firefox".to_string(),
                icon_name: None,
            })
        );
        assert_eq!(
            applications.get("org.mozilla.Firefox"),
            Some(&ApplicationMetadata {
                desktop_id: "org.mozilla.Firefox.desktop".to_owned(),
                name: "Firefox".to_string(),
                icon_name: None,
            })
        );
    }

    #[test]
    fn activate_uses_launcher() {
        let launches = Arc::new(Mutex::new(Vec::new()));
        let launcher = FakeLauncher {
            launches: Arc::clone(&launches),
        };
        let mut module = ApplicationsModule::with_launcher(
            vec![entry("org.mozilla.Firefox.desktop", "Firefox", "", "")],
            Box::new(launcher),
        );

        let outcome = module
            .activate("org.mozilla.Firefox.desktop", DEFAULT_ACTION_ID, None)
            .unwrap();

        assert_eq!(outcome, ActivationOutcome::ClosePicker);
        assert_eq!(
            launches.lock().unwrap().as_slice(),
            ["org.mozilla.Firefox.desktop"]
        );
    }

    #[test]
    fn catalog_replacement_updates_search_and_metadata_together() {
        let mut module = ApplicationsModule::with_launcher(
            vec![entry("browser.desktop", "Old browser", "", "")],
            Box::new(ProcessApplicationLauncher),
        );
        let replacement = ApplicationsModule::with_launcher(
            vec![entry("browser.desktop", "New browser", "", "")],
            Box::new(ProcessApplicationLauncher),
        );
        module.store.replace(replacement.store.snapshot());
        assert!(module.search("old").unwrap().is_empty());
        assert_eq!(module.search("new").unwrap()[0].title, "Launch New browser");
        assert_eq!(
            module.store.snapshot().metadata["browser"].name,
            "New browser"
        );
        module
            .store
            .replace(Arc::new(ApplicationCatalog::default()));
        assert!(module.search("").unwrap().is_empty());
        assert!(module.store.snapshot().metadata.is_empty());
    }

    #[test]
    fn profile_switches_invalidate_the_catalog_even_when_metadata_is_unchanged() {
        use std::os::unix::fs::symlink;
        use std::time::{SystemTime, UNIX_EPOCH};

        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("picky-catalog-{}-{unique}", std::process::id()));
        for generation in ["first", "second"] {
            let directory = root.join(generation).join("applications");
            fs::create_dir_all(&directory).unwrap();
            fs::write(
                directory.join("test.desktop"),
                "[Desktop Entry]\nType=Application\nName=Test\nExec=test\n",
            )
            .unwrap();
        }
        let profile = root.join("profile");
        symlink("first", &profile).unwrap();
        let roots = [profile.join("applications")];
        let first = load_catalog_from(&roots).unwrap();
        symlink("second", root.join("next-profile")).unwrap();
        fs::rename(root.join("next-profile"), &profile).unwrap();
        let second = load_catalog_from(&roots).unwrap();
        assert_eq!(first.entries, second.entries);
        assert_eq!(first.metadata, second.metadata);
        let store = ApplicationStore::default();
        assert!(store.replace(first));
        assert!(store.replace(second));
        assert!(!store.replace(load_catalog_from(&roots).unwrap()));
        fs::remove_dir_all(root).unwrap();
    }
}
