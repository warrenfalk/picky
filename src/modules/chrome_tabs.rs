use std::collections::HashSet;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::fuzzy;
use crate::module::{MatchKind, Module, SearchResult};

const MODULE_KEY: &str = "chrome-tabs";

pub struct ChromeTabsModule {
    profiles: Vec<ChromeProfile>,
}

#[derive(Clone, Debug)]
struct ChromeProfile {
    path: PathBuf,
    profile_dir: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct ChromeActivation {
    profile_dir: String,
    url: String,
}

#[derive(Debug)]
struct ChromeTab {
    title: String,
    url: String,
}

impl ChromeTabsModule {
    pub fn new() -> Self {
        Self {
            profiles: find_chrome_profiles().unwrap_or_else(|err| {
                eprintln!("failed to find chrome profiles: {err:#}");
                Vec::new()
            }),
        }
    }
}

impl Module for ChromeTabsModule {
    fn key(&self) -> &'static str {
        MODULE_KEY
    }

    fn search(&mut self, query: &str) -> Result<Vec<SearchResult>> {
        let query = query.trim();
        let mut results = Vec::new();

        for profile in &self.profiles {
            let Some(session_file) = latest_chrome_tabs_file(&profile.path) else {
                continue;
            };

            for tab in load_chrome_tabs(&session_file)? {
                let Some(score) = fuzzy::score_fields(
                    query,
                    &[
                        (&tab.title, 120),
                        (&tab.url, 60),
                        (&profile.profile_dir, 20),
                    ],
                ) else {
                    continue;
                };

                results.push(SearchResult {
                    module_key: MODULE_KEY,
                    item_id: serde_json::to_string(&ChromeActivation {
                        profile_dir: profile.profile_dir.clone(),
                        url: tab.url.clone(),
                    })?,
                    title: tab.title,
                    subtitle: format!("Chrome tab  {}", profile.profile_dir),
                    icon_name: Some("chrome".to_string()),
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
        let activation: ChromeActivation =
            serde_json::from_str(item_id).context("invalid chrome tab activation payload")?;

        Command::new(chrome_binary())
            .arg(format!("--profile-directory={}", activation.profile_dir))
            .arg(&activation.url)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| format!("failed to open chrome tab {}", activation.url))?;

        Ok(())
    }
}

fn find_chrome_profiles() -> Result<Vec<ChromeProfile>> {
    let chrome_root = home_dir()
        .context("HOME is not set")?
        .join(".config/google-chrome");
    let mut profiles = Vec::new();

    for entry in fs::read_dir(&chrome_root)
        .with_context(|| format!("failed to read {}", chrome_root.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if !path.is_dir() || !path.join("Sessions").is_dir() {
            continue;
        }

        let profile_dir = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("Default")
            .to_string();

        profiles.push(ChromeProfile { path, profile_dir });
    }

    profiles.sort_by(|left, right| left.profile_dir.cmp(&right.profile_dir));
    Ok(profiles)
}

fn latest_chrome_tabs_file(profile_path: &Path) -> Option<PathBuf> {
    let sessions_dir = profile_path.join("Sessions");
    let mut tab_files = fs::read_dir(sessions_dir)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("Tabs_"))
        })
        .collect::<Vec<_>>();

    tab_files.sort();
    tab_files.pop()
}

fn load_chrome_tabs(path: &Path) -> Result<Vec<ChromeTab>> {
    let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    let strings = extract_utf16le_strings(&bytes, 8);
    let mut seen = HashSet::new();
    let mut tabs = Vec::new();

    for (index, (_, value)) in strings.iter().enumerate() {
        if !looks_like_url(value) || !keep_url(value) {
            continue;
        }

        let title = (0..index.min(6))
            .find_map(|offset| {
                strings
                    .get(index - offset - 1)
                    .map(|(_, candidate)| candidate)
            })
            .filter(|candidate| is_reasonable_title(candidate))
            .cloned()
            .unwrap_or_else(|| fallback_title(value));

        if seen.insert((title.clone(), value.clone())) {
            tabs.push(ChromeTab {
                title,
                url: value.clone(),
            });
        }
    }

    Ok(tabs)
}

fn extract_utf16le_strings(bytes: &[u8], min_len: usize) -> Vec<(usize, String)> {
    let mut strings = Vec::new();

    for parity in [0usize, 1usize] {
        let mut index = parity;
        while index + 1 < bytes.len() {
            let start = index;
            let mut chars = Vec::new();

            while index + 1 < bytes.len() {
                let code_unit = u16::from_le_bytes([bytes[index], bytes[index + 1]]);
                if !valid_utf16_code_unit(code_unit) {
                    break;
                }
                chars.push(char::from_u32(code_unit as u32).unwrap_or('\u{FFFD}'));
                index += 2;
            }

            if chars.len() >= min_len {
                let value = chars.iter().collect::<String>().trim().to_string();
                if !value.is_empty() {
                    strings.push((start, value));
                }
            }

            if index == start {
                index += 2;
            }
        }
    }

    strings.sort_by_key(|(offset, _)| *offset);
    strings
}

fn valid_utf16_code_unit(code_unit: u16) -> bool {
    matches!(code_unit, 0x20..=0x7E) || matches!(code_unit, 0x09 | 0x0A | 0x0D)
}

fn looks_like_url(value: &str) -> bool {
    value.starts_with("http://")
        || value.starts_with("https://")
        || value.starts_with("chrome://")
        || value.starts_with("chrome-untrusted://")
        || value.starts_with("file://")
}

fn keep_url(value: &str) -> bool {
    !value.contains("<!--dynamicFrame") && !value.contains("one-google-bar?paramsencoded=")
}

fn is_reasonable_title(value: &&String) -> bool {
    !looks_like_url(value)
        && !value.starts_with("<!--")
        && !looks_like_uuid(value)
        && !value.starts_with('#')
        && !value.eq_ignore_ascii_case("html")
        && !value.eq_ignore_ascii_case("radio")
}

fn looks_like_uuid(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 36
        && bytes.iter().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => *byte == b'-',
            _ => byte.is_ascii_hexdigit(),
        })
}

fn fallback_title(url: &str) -> String {
    if url.starts_with("chrome://new-tab-page") || url.starts_with("chrome://newtab") {
        "New Tab".to_string()
    } else {
        url.to_string()
    }
}

fn home_dir() -> Option<PathBuf> {
    env::var_os("HOME").map(PathBuf::from)
}

fn chrome_binary() -> &'static str {
    if Path::new("/run/current-system/sw/bin/google-chrome").exists() {
        "/run/current-system/sw/bin/google-chrome"
    } else {
        "google-chrome"
    }
}
