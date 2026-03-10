use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::fuzzy;
use crate::module::{MatchKind, Module, SearchResult};

const MODULE_KEY: &str = "firefox-tabs";

pub struct FirefoxTabsModule {
    profiles: Vec<FirefoxProfile>,
}

#[derive(Clone, Debug)]
struct FirefoxProfile {
    path: PathBuf,
    label: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct FirefoxActivation {
    url: String,
}

impl FirefoxTabsModule {
    pub fn new() -> Self {
        Self {
            profiles: find_firefox_profiles().unwrap_or_else(|err| {
                eprintln!("failed to find firefox profiles: {err:#}");
                Vec::new()
            }),
        }
    }
}

impl Module for FirefoxTabsModule {
    fn key(&self) -> &'static str {
        MODULE_KEY
    }

    fn search(&mut self, query: &str) -> Result<Vec<SearchResult>> {
        let query = query.trim();
        let mut results = Vec::new();

        for profile in &self.profiles {
            let Some(session_file) = latest_firefox_session_file(&profile.path) else {
                continue;
            };

            for tab in load_firefox_tabs(&session_file)? {
                let score = fuzzy::score_fields(
                    query,
                    &[(&tab.title, 120), (&tab.url, 60), (&profile.label, 20)],
                );

                let Some(score) = score else {
                    continue;
                };

                results.push(SearchResult {
                    module_key: MODULE_KEY,
                    item_id: serde_json::to_string(&FirefoxActivation {
                        url: tab.url.clone(),
                    })?,
                    title: tab.title,
                    subtitle: format!("Firefox tab  {}", profile.label),
                    icon_name: Some("firefox".to_string()),
                    kind: MatchKind::BrowserTab,
                    score,
                });
            }
        }

        results.sort_by(|left, right| {
            right
                .score
                .cmp(&left.score)
                .then_with(|| left.title.cmp(&right.title))
        });

        Ok(results)
    }

    fn activate(&mut self, item_id: &str) -> Result<()> {
        let activation: FirefoxActivation =
            serde_json::from_str(item_id).context("invalid firefox tab activation payload")?;

        Command::new("firefox")
            .args(["--new-tab", &activation.url])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| format!("failed to open firefox tab {}", activation.url))?;

        Ok(())
    }
}

#[derive(Debug)]
struct FirefoxTab {
    title: String,
    url: String,
}

fn find_firefox_profiles() -> Result<Vec<FirefoxProfile>> {
    let firefox_root = home_dir()
        .context("HOME is not set")?
        .join(".mozilla/firefox");
    let mut profiles = Vec::new();

    for entry in fs::read_dir(&firefox_root)
        .with_context(|| format!("failed to read {}", firefox_root.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        if path.join("sessionstore-backups").is_dir() {
            let label = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("default")
                .to_string();
            profiles.push(FirefoxProfile { path, label });
        }
    }

    profiles.sort_by(|left, right| left.label.cmp(&right.label));
    Ok(profiles)
}

fn latest_firefox_session_file(profile_path: &Path) -> Option<PathBuf> {
    let backups = profile_path.join("sessionstore-backups");
    [
        backups.join("recovery.jsonlz4"),
        backups.join("recovery.baklz4"),
        backups.join("previous.jsonlz4"),
    ]
    .into_iter()
    .find(|path| path.exists())
}

fn load_firefox_tabs(path: &Path) -> Result<Vec<FirefoxTab>> {
    let payload = read_mozlz4(path)?;
    let json: Value = serde_json::from_slice(&payload)
        .with_context(|| format!("invalid JSON in {}", path.display()))?;
    let mut tabs = Vec::new();

    for window in json
        .get("windows")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        for tab in window
            .get("tabs")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(entries) = tab.get("entries").and_then(Value::as_array) else {
                continue;
            };

            let current_index = tab
                .get("index")
                .and_then(Value::as_u64)
                .and_then(|index| usize::try_from(index).ok())
                .and_then(|index| index.checked_sub(1))
                .filter(|index| *index < entries.len())
                .unwrap_or(entries.len().saturating_sub(1));

            let Some(entry) = entries.get(current_index) else {
                continue;
            };
            let Some(url) = entry.get("url").and_then(Value::as_str) else {
                continue;
            };

            let title = entry
                .get("title")
                .and_then(Value::as_str)
                .filter(|title| !title.is_empty())
                .unwrap_or(url)
                .to_string();

            tabs.push(FirefoxTab {
                title,
                url: url.to_string(),
            });
        }
    }

    Ok(tabs)
}

fn read_mozlz4(path: &Path) -> Result<Vec<u8>> {
    let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    if bytes.len() < 12 || &bytes[..8] != b"mozLz40\0" {
        bail!("{} is not a mozLz4 file", path.display());
    }

    let decompressed_size = u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize;
    lz4_flex::block::decompress(&bytes[12..], decompressed_size)
        .with_context(|| format!("failed to decompress {}", path.display()))
}

fn home_dir() -> Option<PathBuf> {
    env::var_os("HOME").map(PathBuf::from)
}
