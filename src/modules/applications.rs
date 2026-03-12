use std::collections::HashSet;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result};

use crate::fuzzy;
use crate::module::{DEFAULT_ACTION_ID, MatchKind, Module, SearchResult};

const MODULE_KEY: &str = "applications";

pub struct ApplicationsModule {
    entries: Vec<ApplicationEntry>,
}

#[derive(Clone, Debug)]
struct ApplicationEntry {
    id: String,
    name: String,
    comment: String,
    keywords: String,
    icon_name: Option<String>,
}

impl ApplicationsModule {
    pub fn new() -> Self {
        Self {
            entries: load_entries().unwrap_or_else(|err| {
                eprintln!("failed to load desktop entries: {err:#}");
                Vec::new()
            }),
        }
    }
}

impl Module for ApplicationsModule {
    fn key(&self) -> &'static str {
        MODULE_KEY
    }

    fn search(&mut self, query: &str) -> Result<Vec<SearchResult>> {
        let query = query.trim();

        let mut results = self
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
                    title: entry.name.clone(),
                    subtitle: if entry.comment.is_empty() {
                        entry.id.clone()
                    } else {
                        entry.comment.clone()
                    },
                    icon_name: entry.icon_name.clone(),
                    kind: MatchKind::Application,
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

    fn activate(&mut self, item_id: &str, action_id: &str) -> Result<()> {
        if action_id != DEFAULT_ACTION_ID {
            anyhow::bail!("unknown application action: {action_id}");
        }

        Command::new("gtk-launch")
            .arg(item_id)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| format!("failed to launch desktop entry {item_id}"))?;

        Ok(())
    }
}

fn load_entries() -> Result<Vec<ApplicationEntry>> {
    let mut entries = Vec::new();
    let mut seen = HashSet::new();

    for applications_dir in application_dirs() {
        if !applications_dir.exists() {
            continue;
        }

        let mut desktop_files = Vec::new();
        collect_desktop_files(&applications_dir, &mut desktop_files)?;

        for desktop_file in desktop_files {
            let Some(entry) = parse_entry(&applications_dir, &desktop_file)? else {
                continue;
            };

            if seen.insert(entry.id.clone()) {
                entries.push(entry);
            }
        }
    }

    Ok(entries)
}

fn application_dirs() -> Vec<PathBuf> {
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
    let mut in_desktop_entry = false;
    let mut name = None;
    let mut comment = None;
    let mut keywords = None;
    let mut icon_name = None;
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
        return Ok(None);
    }

    let Some(id) = desktop_entry_id(applications_dir, desktop_file) else {
        return Ok(None);
    };

    let default_name = desktop_file
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("unknown")
        .to_string();

    Ok(Some(ApplicationEntry {
        id,
        name: name.unwrap_or(default_name),
        comment: comment.unwrap_or_default(),
        keywords: keywords.unwrap_or_default(),
        icon_name,
    }))
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
