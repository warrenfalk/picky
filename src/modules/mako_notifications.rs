use std::collections::HashMap;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use iced::Subscription;
use iced::futures::channel::mpsc;
use iced::futures::{FutureExt, SinkExt, Stream, StreamExt};
use zbus::zvariant::OwnedValue;

use crate::fuzzy;
use crate::module::{
    ActivationOutcome, DEFAULT_ACTION_ID, MatchKind, Module, ResultAction, SearchResult,
    sort_results,
};

const MODULE_KEY: &str = "mako-notifications";
const EMPTY_QUERY_BASE_SCORE: i64 = 10_000;
const DISMISS_ACTION_ID: &str = "dismiss";
const GO_TO_ACTION_ID: &str = "go-to";
const SERVICE: &str = "org.freedesktop.Notifications";
const PATH: &str = "/fr/emersion/Mako";
const INTERFACE: &str = "fr.emersion.Mako";
const RESTART_DELAY: Duration = Duration::from_secs(1);
const RECONCILE_INTERVAL: Duration = Duration::from_secs(1);

pub type FeedUpdate = Result<Vec<Notification>, String>;

pub struct MakoNotificationsModule {
    notifications: NotificationStore,
    client: Box<dyn MakoClient>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notification {
    pub id: u64,
    pub app_name: String,
    pub summary: String,
    pub body: String,
    pub icon_name: Option<String>,
    pub urgency: u8,
}

trait MakoClient: Send {
    fn invoke(&self, item_id: &str) -> Result<bool>;
    fn dismiss(&self, item_id: &str) -> Result<()>;
}

struct ProcessMakoClient;

impl MakoClient for ProcessMakoClient {
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
    pub fn new(notifications: NotificationStore) -> Self {
        Self::with_client(notifications, Box::new(ProcessMakoClient))
    }

    fn with_client(notifications: NotificationStore, client: Box<dyn MakoClient>) -> Self {
        Self {
            notifications,
            client,
        }
    }
}

#[derive(Clone, Default)]
pub struct NotificationStore(Arc<Mutex<Vec<Notification>>>);

impl NotificationStore {
    pub fn replace(&self, notifications: Vec<Notification>) {
        *self.0.lock().unwrap() = notifications;
    }

    pub fn merge_results(&self, query: &str, results: &mut Vec<SearchResult>) {
        results.retain(|result| result.module_key != MODULE_KEY);
        results.extend(self.search(query));
        sort_results(results);
    }

    fn search(&self, query: &str) -> Vec<SearchResult> {
        let query = query.trim();
        let notifications = self.0.lock().unwrap().clone();

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
                    icon_name: notification.icon_name.clone(),
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

        results
    }
}

impl Module for MakoNotificationsModule {
    fn key(&self) -> &'static str {
        MODULE_KEY
    }

    fn search(&mut self, query: &str) -> Result<Vec<SearchResult>> {
        Ok(self.notifications.search(query))
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

pub fn subscription() -> Subscription<FeedUpdate> {
    Subscription::run(watch)
}

fn watch() -> impl Stream<Item = FeedUpdate> {
    watch_with_connection(zbus::Connection::session)
}

fn watch_with_connection<F: std::future::Future<Output = zbus::Result<zbus::Connection>> + Send>(
    connect: impl Fn() -> F + Send + 'static,
) -> impl Stream<Item = FeedUpdate> {
    iced::stream::channel(1, async move |mut output| {
        loop {
            let result = async {
                let connection = connect().await?;
                forward_notifications(&connection, &mut output).await
            }
            .await;
            let Err(error) = result else { return };
            if output.send(Err(error.to_string())).await.is_err() {
                return;
            }
            async_io::Timer::after(RESTART_DELAY).await;
        }
    })
}

async fn forward_notifications(
    connection: &zbus::Connection,
    output: &mut mpsc::Sender<FeedUpdate>,
) -> Result<()> {
    let mako = zbus::Proxy::new(connection, SERVICE, PATH, INTERFACE).await?;
    let properties = zbus::fdo::PropertiesProxy::builder(connection)
        .destination(SERVICE)?
        .path(PATH)?
        .build()
        .await?;
    // Subscribe before the first snapshot so changes during startup cannot be lost.
    let mut owners = mako.receive_owner_changed().await?;
    let mut changes = properties.receive_properties_changed().await?;
    let mut previous = None;
    loop {
        let notifications: Vec<HashMap<String, OwnedValue>> =
            mako.call("ListNotifications", &()).await?;
        let notifications: Vec<_> = notifications
            .into_iter()
            .filter_map(parse_notification)
            .collect();
        if previous.as_ref() != Some(&notifications) {
            if output.send(Ok(notifications.clone())).await.is_err() {
                return Ok(());
            }
            previous = Some(notifications);
        }
        // Mako does not invalidate Notifications when Notify replaces an existing
        // ID. Reconcile those changes too, without emitting unchanged snapshots.
        let reconcile = FutureExt::fuse(async_io::Timer::after(RECONCILE_INTERVAL));
        iced::futures::pin_mut!(reconcile);
        loop {
            iced::futures::select! {
                _ = reconcile => break,
                _ = owners.next().fuse() => bail!("Mako notification service disconnected; reconnecting"),
                change = changes.next().fuse() => {
                    let change = change.context("Mako notification feed disconnected; reconnecting")?;
                    let args = change.args()?;
                    if args.interface_name().as_str() == INTERFACE
                        && (args.changed_properties().contains_key("Notifications")
                            || args.invalidated_properties().contains(&"Notifications"))
                    {
                        break;
                    }
                }
            }
        }
    }
}

fn parse_notification(map: HashMap<String, OwnedValue>) -> Option<Notification> {
    let id = u64::from(u32::try_from(map.get("id")?).ok()?);
    let summary = variant_string(map.get("summary")).unwrap_or_default();

    if summary.trim().is_empty() {
        return None;
    }

    Some(Notification {
        id,
        app_name: variant_string(map.get("app-name")).unwrap_or_default(),
        summary,
        body: variant_string(map.get("body")).unwrap_or_default(),
        icon_name: variant_string(map.get("app-icon")).filter(|icon| !icon.trim().is_empty()),
        urgency: map
            .get("urgency")
            .and_then(|value| u8::try_from(value).ok())
            .unwrap_or(0),
    })
}

fn variant_string(value: Option<&OwnedValue>) -> Option<String> {
    <&str>::try_from(value?).ok().map(ToOwned::to_owned)
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
        invokes: Vec<String>,
        dismisses: Vec<String>,
        invoke_success: bool,
    }

    struct FakeMakoClient {
        state: Arc<Mutex<FakeState>>,
    }

    impl MakoClient for FakeMakoClient {
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
            icon_name: None,
            urgency,
        }
    }

    #[test]
    fn parse_notification_reads_app_icon() {
        let value = HashMap::from([
            ("id".to_owned(), OwnedValue::from(9_u32)),
            (
                "summary".to_owned(),
                zbus::zvariant::Str::from("Summary").into(),
            ),
            ("body".to_owned(), zbus::zvariant::Str::from("Body").into()),
            (
                "app-name".to_owned(),
                zbus::zvariant::Str::from("Mail").into(),
            ),
            (
                "app-icon".to_owned(),
                zbus::zvariant::Str::from("/tmp/icon.png").into(),
            ),
            ("urgency".to_owned(), OwnedValue::from(1_u8)),
        ]);

        let notification = parse_notification(value).unwrap();

        assert_eq!(notification.icon_name.as_deref(), Some("/tmp/icon.png"));
    }

    #[test]
    fn search_empty_query_keeps_newest_first() {
        let notifications = NotificationStore::default();
        notifications.replace(vec![
            notification(1, "Mail", "Old", "", 0),
            notification(2, "Mail", "New", "", 0),
        ]);
        let mut module = MakoNotificationsModule::new(notifications);

        let results = module.search("").unwrap();

        assert_eq!(results[0].title, "New");
        assert_eq!(results[0].actions[0].id, GO_TO_ACTION_ID);
        assert_eq!(results[0].actions[1].id, DISMISS_ACTION_ID);
    }

    #[derive(Clone, Default)]
    struct TestMako {
        notifications: Arc<Mutex<Vec<HashMap<String, OwnedValue>>>>,
    }

    #[zbus::interface(name = "fr.emersion.Mako")]
    impl TestMako {
        fn list_notifications(&self) -> Vec<HashMap<String, OwnedValue>> {
            self.notifications.lock().unwrap().clone()
        }
    }

    impl TestMako {
        async fn replace(&self, connection: &zbus::Connection, title: Option<&str>) {
            *self.notifications.lock().unwrap() = title
                .into_iter()
                .map(|title| {
                    HashMap::from([
                        ("id".to_owned(), OwnedValue::from(42_u32)),
                        (
                            "summary".to_owned(),
                            zbus::zvariant::Str::from(title).into(),
                        ),
                    ])
                })
                .collect();
            let emitter = zbus::object_server::SignalEmitter::new(connection, PATH).unwrap();
            zbus::fdo::Properties::properties_changed(
                &emitter,
                INTERFACE.try_into().unwrap(),
                HashMap::new(),
                vec!["Notifications"].into(),
            )
            .await
            .unwrap();
        }
    }

    async fn next_update(feed: &mut (impl Stream<Item = FeedUpdate> + Unpin)) -> FeedUpdate {
        iced::futures::select! {
            update = feed.next().fuse() => update.expect("notification feed ended"),
            _ = FutureExt::fuse(async_io::Timer::after(Duration::from_secs(5))) => panic!("notification feed timed out"),
        }
    }

    #[test]
    #[ignore = "requires dbus-daemon on PATH; starts an isolated session bus"]
    fn dbus_feed_tracks_notifications_and_recovers_after_service_restart() {
        use std::io::{BufRead, BufReader};

        struct TestBus(std::process::Child);
        impl Drop for TestBus {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }

        let mut bus = TestBus(
            Command::new("dbus-daemon")
                .args(["--session", "--nofork", "--print-address=1"])
                .stdout(Stdio::piped())
                .spawn()
                .expect("could not start test D-Bus"),
        );
        let mut address = String::new();
        BufReader::new(bus.0.stdout.take().unwrap())
            .read_line(&mut address)
            .unwrap();
        let address = address.trim().to_owned();

        iced::futures::executor::block_on(async {
            let service = TestMako::default();
            let server = zbus::connection::Builder::address(address.as_str())
                .unwrap()
                .serve_at(PATH, service.clone())
                .unwrap()
                .name(SERVICE)
                .unwrap()
                .build()
                .await
                .unwrap();
            service.replace(&server, Some("Existing")).await;

            let feed_address = address.clone();
            let mut feed = Box::pin(watch_with_connection(move || {
                let address = feed_address.clone();
                async move {
                    zbus::connection::Builder::address(address.as_str())?
                        .build()
                        .await
                }
            }));
            assert_eq!(next_update(&mut feed).await.unwrap()[0].summary, "Existing");
            service.replace(&server, Some("Updated")).await;
            assert_eq!(next_update(&mut feed).await.unwrap()[0].summary, "Updated");
            // Repeated signals must not publish duplicate snapshots. Once the
            // feed has handled that signal, silently replace the same ID.
            service.replace(&server, Some("Updated")).await;
            let (update, ()) = iced::futures::join!(next_update(&mut feed), async {
                async_io::Timer::after(Duration::from_millis(100)).await;
                service.notifications.lock().unwrap()[0].insert(
                    "summary".to_owned(),
                    zbus::zvariant::Str::from("Silently replaced").into(),
                );
            });
            assert_eq!(update.unwrap()[0].summary, "Silently replaced");
            service.replace(&server, None).await;
            assert!(next_update(&mut feed).await.unwrap().is_empty());
            service.replace(&server, Some("New arrival")).await;
            assert_eq!(
                next_update(&mut feed).await.unwrap()[0].summary,
                "New arrival"
            );

            server.close().await.unwrap();
            assert!(next_update(&mut feed).await.is_err());
            let replacement = TestMako::default();
            let server = zbus::connection::Builder::address(address.as_str())
                .unwrap()
                .serve_at(PATH, replacement.clone())
                .unwrap()
                .name(SERVICE)
                .unwrap()
                .build()
                .await
                .unwrap();
            replacement.replace(&server, Some("After restart")).await;
            assert_eq!(
                next_update(&mut feed).await.unwrap()[0].summary,
                "After restart"
            );
            drop(feed);
            server.close().await.unwrap();
        });
    }

    #[test]
    fn default_activate_invokes_then_dismisses() {
        let state = Arc::new(Mutex::new(FakeState {
            invoke_success: true,
            ..FakeState::default()
        }));
        let mut module = MakoNotificationsModule::with_client(
            NotificationStore::default(),
            Box::new(FakeMakoClient {
                state: Arc::clone(&state),
            }),
        );

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
        let mut module = MakoNotificationsModule::with_client(
            NotificationStore::default(),
            Box::new(FakeMakoClient {
                state: Arc::clone(&state),
            }),
        );

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
        let mut module = MakoNotificationsModule::with_client(
            NotificationStore::default(),
            Box::new(FakeMakoClient {
                state: Arc::clone(&state),
            }),
        );

        let outcome = module.activate("42", GO_TO_ACTION_ID).unwrap();

        assert_eq!(outcome, ActivationOutcome::ClosePicker);
        let state = state.lock().unwrap();
        assert_eq!(state.invokes.as_slice(), ["42"]);
        assert!(state.dismisses.is_empty());
    }

    #[test]
    fn dismiss_action_refreshes_results() {
        let state = Arc::new(Mutex::new(FakeState::default()));
        let mut module = MakoNotificationsModule::with_client(
            NotificationStore::default(),
            Box::new(FakeMakoClient {
                state: Arc::clone(&state),
            }),
        );

        let outcome = module.activate("42", DISMISS_ACTION_ID).unwrap();

        assert_eq!(outcome, ActivationOutcome::RefreshResults);
        assert_eq!(state.lock().unwrap().dismisses.as_slice(), ["42"]);
    }
}
