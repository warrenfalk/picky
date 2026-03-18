use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::Value;

use crate::fuzzy;
use crate::module::{
    ActivationOutcome, DEFAULT_ACTION_ID, MatchKind, Module, ResultAction, SearchResult,
};

const MODULE_KEY: &str = "mako-notifications";
const EMPTY_QUERY_BASE_SCORE: i64 = 10_000;
const DISMISS_ACTION_ID: &str = "dismiss";
const GO_TO_ACTION_ID: &str = "go-to";

pub struct MakoNotificationsModule {
    client: Box<dyn MakoClient>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Notification {
    id: u64,
    app_name: String,
    summary: String,
    body: String,
    urgency: u8,
}

#[derive(Debug, Deserialize)]
struct BusctlResponse {
    data: Vec<Vec<Value>>,
}

trait MakoClient: Send {
    fn list_notifications(&self) -> Result<Vec<Notification>>;
    fn invoke(&self, item_id: &str) -> Result<bool>;
    fn dismiss(&self, item_id: &str) -> Result<()>;
}

struct ProcessMakoClient;

impl MakoClient for ProcessMakoClient {
    fn list_notifications(&self) -> Result<Vec<Notification>> {
        load_notifications()
    }

    fn invoke(&self, item_id: &str) -> Result<bool> {
        let invoke_status = Command::new("makoctl")
            .args(["invoke", "-n", item_id])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .with_context(|| format!("failed to invoke notification {item_id}"))?;

        Ok(invoke_status.success())
    }

    fn dismiss(&self, item_id: &str) -> Result<()> {
        dismiss_notification(item_id)
    }
}

impl MakoNotificationsModule {
    pub fn new() -> Self {
        Self::with_client(Box::new(ProcessMakoClient))
    }

    fn with_client(client: Box<dyn MakoClient>) -> Self {
        Self { client }
    }
}

impl Module for MakoNotificationsModule {
    fn key(&self) -> &'static str {
        MODULE_KEY
    }

    fn search(&mut self, query: &str) -> Result<Vec<SearchResult>> {
        let query = query.trim();
        let notifications = self.client.list_notifications()?;

        let mut results = notifications
            .into_iter()
            .rev()
            .enumerate()
            .filter_map(|(index, notification)| {
                let score = if query.is_empty() {
                    EMPTY_QUERY_BASE_SCORE - index as i64
                } else {
                    fuzzy::score_fields(
                        query,
                        &[
                            (&notification.summary, 130),
                            (&notification.body, 70),
                            (&notification.app_name, 40),
                        ],
                    )? + i64::from(notification.urgency)
                };

                let subtitle = match (
                    notification.app_name.trim().is_empty(),
                    notification.body.trim().is_empty(),
                ) {
                    (true, true) => String::new(),
                    (false, true) => notification.app_name.clone(),
                    (true, false) => notification.body.clone(),
                    (false, false) => format!("{}  {}", notification.app_name, notification.body),
                };

                Some(SearchResult {
                    module_key: MODULE_KEY,
                    item_id: notification.id.to_string(),
                    title: notification.summary,
                    subtitle,
                    icon_name: None,
                    kind: MatchKind::Notification,
                    actions: vec![
                        ResultAction {
                            id: GO_TO_ACTION_ID,
                            label: "Go to",
                            shortcut: 'g',
                        },
                        ResultAction {
                            id: DISMISS_ACTION_ID,
                            label: "Dismiss",
                            shortcut: 'd',
                        },
                    ],
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

    fn activate(&mut self, item_id: &str, action_id: &str) -> Result<ActivationOutcome> {
        match action_id {
            DEFAULT_ACTION_ID => {
                if self.client.invoke(item_id)? {
                    self.client.dismiss(item_id)?;
                    return Ok(ActivationOutcome::ClosePicker);
                }

                self.client.dismiss(item_id).map_err(|_| {
                    anyhow::anyhow!("mako refused to invoke or dismiss notification {item_id}")
                })?;

                Ok(ActivationOutcome::ClosePicker)
            }
            GO_TO_ACTION_ID => {
                if self.client.invoke(item_id)? {
                    Ok(ActivationOutcome::ClosePicker)
                } else {
                    bail!("mako refused to invoke notification {item_id}");
                }
            }
            DISMISS_ACTION_ID => {
                self.client.dismiss(item_id)?;
                Ok(ActivationOutcome::RefreshResults)
            }
            _ => bail!("unknown notification action: {action_id}"),
        }
    }
}

fn load_notifications() -> Result<Vec<Notification>> {
    let output = Command::new("busctl")
        .args([
            "--json=short",
            "--user",
            "call",
            "org.freedesktop.Notifications",
            "/fr/emersion/Mako",
            "fr.emersion.Mako",
            "ListNotifications",
        ])
        .output()
        .context("failed to run `busctl ... ListNotifications`")?;

    if !output.status.success() {
        bail!(
            "`busctl ... ListNotifications` failed with status {}",
            output.status
        );
    }

    let response: BusctlResponse =
        serde_json::from_slice(&output.stdout).context("failed to parse busctl JSON")?;

    Ok(response
        .data
        .into_iter()
        .next()
        .unwrap_or_default()
        .into_iter()
        .filter_map(parse_notification)
        .collect())
}

fn parse_notification(value: Value) -> Option<Notification> {
    let map = value.as_object()?;
    let id = variant_u64(map.get("id")?)?;
    let summary = variant_string(map.get("summary")).unwrap_or_default();

    if summary.trim().is_empty() {
        return None;
    }

    Some(Notification {
        id,
        app_name: variant_string(map.get("app-name")).unwrap_or_default(),
        summary,
        body: variant_string(map.get("body")).unwrap_or_default(),
        urgency: variant_u8(map.get("urgency")).unwrap_or(0),
    })
}

fn variant_string(value: Option<&Value>) -> Option<String> {
    value?
        .as_object()?
        .get("data")?
        .as_str()
        .map(ToOwned::to_owned)
}

fn variant_u64(value: &Value) -> Option<u64> {
    value.as_object()?.get("data")?.as_u64()
}

fn variant_u8(value: Option<&Value>) -> Option<u8> {
    value?
        .as_object()?
        .get("data")?
        .as_u64()
        .and_then(|value| u8::try_from(value).ok())
}

fn dismiss_notification(item_id: &str) -> Result<()> {
    let dismiss_status = Command::new("makoctl")
        .args(["dismiss", "-n", item_id])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .with_context(|| format!("failed to dismiss notification {item_id}"))?;

    if !dismiss_status.success() {
        bail!("mako refused to dismiss notification {item_id}");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct FakeState {
        notifications: Vec<Notification>,
        invokes: Vec<String>,
        dismisses: Vec<String>,
        invoke_success: bool,
    }

    struct FakeMakoClient {
        state: Arc<Mutex<FakeState>>,
    }

    impl MakoClient for FakeMakoClient {
        fn list_notifications(&self) -> Result<Vec<Notification>> {
            Ok(self.state.lock().unwrap().notifications.clone())
        }

        fn invoke(&self, item_id: &str) -> Result<bool> {
            let mut state = self.state.lock().unwrap();
            state.invokes.push(item_id.to_string());
            Ok(state.invoke_success)
        }

        fn dismiss(&self, item_id: &str) -> Result<()> {
            self.state
                .lock()
                .unwrap()
                .dismisses
                .push(item_id.to_string());
            Ok(())
        }
    }

    fn notification(
        id: u64,
        app_name: &str,
        summary: &str,
        body: &str,
        urgency: u8,
    ) -> Notification {
        Notification {
            id,
            app_name: app_name.to_string(),
            summary: summary.to_string(),
            body: body.to_string(),
            urgency,
        }
    }

    #[test]
    fn search_empty_query_keeps_newest_first() {
        let state = Arc::new(Mutex::new(FakeState {
            notifications: vec![
                notification(1, "Mail", "Old", "", 0),
                notification(2, "Mail", "New", "", 0),
            ],
            ..FakeState::default()
        }));
        let mut module = MakoNotificationsModule::with_client(Box::new(FakeMakoClient { state }));

        let results = module.search("").unwrap();

        assert_eq!(results[0].title, "New");
        assert_eq!(results[0].actions[0].id, GO_TO_ACTION_ID);
        assert_eq!(results[0].actions[1].id, DISMISS_ACTION_ID);
    }

    #[test]
    fn default_activate_invokes_then_dismisses() {
        let state = Arc::new(Mutex::new(FakeState {
            invoke_success: true,
            ..FakeState::default()
        }));
        let mut module = MakoNotificationsModule::with_client(Box::new(FakeMakoClient {
            state: Arc::clone(&state),
        }));

        let outcome = module.activate("42", DEFAULT_ACTION_ID).unwrap();

        assert_eq!(outcome, ActivationOutcome::ClosePicker);
        let state = state.lock().unwrap();
        assert_eq!(state.invokes.as_slice(), ["42"]);
        assert_eq!(state.dismisses.as_slice(), ["42"]);
    }

    #[test]
    fn failed_invoke_still_dismisses() {
        let state = Arc::new(Mutex::new(FakeState {
            invoke_success: false,
            ..FakeState::default()
        }));
        let mut module = MakoNotificationsModule::with_client(Box::new(FakeMakoClient {
            state: Arc::clone(&state),
        }));

        let outcome = module.activate("42", DEFAULT_ACTION_ID).unwrap();

        assert_eq!(outcome, ActivationOutcome::ClosePicker);
        assert_eq!(state.lock().unwrap().dismisses.as_slice(), ["42"]);
    }

    #[test]
    fn go_to_action_invokes_without_dismissing() {
        let state = Arc::new(Mutex::new(FakeState {
            invoke_success: true,
            ..FakeState::default()
        }));
        let mut module = MakoNotificationsModule::with_client(Box::new(FakeMakoClient {
            state: Arc::clone(&state),
        }));

        let outcome = module.activate("42", GO_TO_ACTION_ID).unwrap();

        assert_eq!(outcome, ActivationOutcome::ClosePicker);
        let state = state.lock().unwrap();
        assert_eq!(state.invokes.as_slice(), ["42"]);
        assert!(state.dismisses.is_empty());
    }

    #[test]
    fn dismiss_action_refreshes_results() {
        let state = Arc::new(Mutex::new(FakeState::default()));
        let mut module = MakoNotificationsModule::with_client(Box::new(FakeMakoClient {
            state: Arc::clone(&state),
        }));

        let outcome = module.activate("42", DISMISS_ACTION_ID).unwrap();

        assert_eq!(outcome, ActivationOutcome::RefreshResults);
        assert_eq!(state.lock().unwrap().dismisses.as_slice(), ["42"]);
    }
}
