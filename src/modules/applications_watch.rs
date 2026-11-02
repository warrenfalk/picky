use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::ffi::OsString;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use iced::Subscription;
use iced::futures::{SinkExt, Stream, StreamExt, channel};
use notify::event::{AccessKind, AccessMode};
use notify::{Event, EventKind, RecursiveMode, Watcher};

const DEBOUNCE: Duration = Duration::from_millis(200);
const MAX_DEBOUNCE: Duration = Duration::from_secs(1);
const RETRY_DELAY: Duration = Duration::from_secs(5);

pub type Update = Result<(), String>;

#[derive(Clone, Debug, Default)]
struct DirectoryWatch {
    all_children: bool,
    children: BTreeSet<OsString>,
}

#[derive(Clone, Debug, Default)]
struct WatchPlan(BTreeMap<PathBuf, DirectoryWatch>);

impl WatchPlan {
    fn build(roots: &[PathBuf], immutable: &Path) -> Result<Self> {
        let mut plan = Self::default();
        let mut seen = BTreeSet::new();
        let mut pending = roots.to_vec();
        while let Some(path) = pending.pop() {
            let Some(directory) = plan.resolve(&path, immutable)? else {
                continue;
            };
            if !directory.is_dir() || !seen.insert(directory.clone()) {
                continue;
            }
            plan.watch_child(&directory);
            plan.0.entry(directory.clone()).or_default().all_children = true;
            for entry in fs::read_dir(&directory)
                .with_context(|| format!("could not scan {}", directory.display()))?
            {
                let path = entry?.path();
                if path.is_dir() {
                    pending.push(path);
                }
            }
        }
        Ok(plan)
    }

    /// Trace each link before entering the store, keeping watches on the mutable
    /// parents where links can be replaced. Never watch resolved store contents.
    fn resolve(&mut self, path: &Path, immutable: &Path) -> Result<Option<PathBuf>> {
        let path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()?.join(path)
        };
        let mut remaining = path
            .components()
            .map(|part| part.as_os_str().to_owned())
            .collect::<VecDeque<_>>();
        let mut resolved = PathBuf::new();
        let mut links = 0;
        while let Some(component) = remaining.pop_front() {
            match Path::new(&component).components().next() {
                Some(Component::RootDir) => resolved = PathBuf::from("/"),
                Some(Component::ParentDir) => {
                    resolved.pop();
                }
                Some(Component::CurDir) | None => continue,
                _ => resolved.push(component),
            }
            if resolved.starts_with(immutable) {
                return Ok(None);
            }
            match fs::symlink_metadata(&resolved) {
                Ok(metadata) if metadata.is_symlink() => {
                    self.watch_child(&resolved);
                    links += 1;
                    if links > 40 {
                        bail!("too many symlinks while tracing {}", path.display());
                    }
                    let target = fs::read_link(&resolved)?;
                    resolved.pop();
                    for part in target.components().rev() {
                        remaining.push_front(part.as_os_str().to_owned());
                    }
                }
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    // Watch the first missing component so newly created roots
                    // and intermediate directories become discoverable too.
                    self.watch_child(&resolved);
                    return Ok(None);
                }
                Err(error) => {
                    return Err(error)
                        .with_context(|| format!("could not trace {}", resolved.display()));
                }
            }
        }
        Ok(Some(resolved))
    }

    fn watch_child(&mut self, path: &Path) {
        if let (Some(parent), Some(name)) = (path.parent(), path.file_name()) {
            self.0
                .entry(parent.to_path_buf())
                .or_default()
                .children
                .insert(name.to_owned());
        }
    }

    fn relevant(&self, event: &Event) -> bool {
        if event.need_rescan() {
            return true;
        }
        // Reading desktop files or enumerating directories must not trigger a
        // new refresh. Only filesystem mutations invalidate the catalog.
        if matches!(event.kind, EventKind::Access(_))
            && event.kind != EventKind::Access(AccessKind::Close(AccessMode::Write))
        {
            return false;
        }
        event.paths.is_empty()
            || event.paths.iter().any(|path| {
                self.0.contains_key(path)
                    || path
                        .parent()
                        .and_then(|parent| self.0.get(parent))
                        .is_some_and(|scope| {
                            scope.all_children
                                || path
                                    .file_name()
                                    .is_some_and(|name| scope.children.contains(name))
                        })
            })
    }
}

enum Request {
    Changed,
    Failed(String),
    Stop,
}

struct StopOnDrop(mpsc::Sender<Request>);

impl Drop for StopOnDrop {
    fn drop(&mut self) {
        let _ = self.0.send(Request::Stop);
    }
}

pub fn subscription() -> Subscription<Update> {
    Subscription::run(watch)
}

fn watch() -> impl Stream<Item = Update> {
    watch_roots(
        super::applications::application_dirs(),
        PathBuf::from("/nix/store"),
    )
}

fn watch_roots(roots: Vec<PathBuf>, immutable: PathBuf) -> impl Stream<Item = Update> {
    iced::stream::channel(1, async move |mut output| {
        let (updates, mut incoming) = channel::mpsc::unbounded();
        let (requests, receiver) = mpsc::channel();
        let _stop = StopOnDrop(requests.clone());
        // Planning, directory traversal, and watch registration stay off Iced's
        // executor. This worker sleeps on events when the filesystem is quiet.
        std::thread::spawn(move || {
            loop {
                match forward(&roots, &immutable, &requests, &receiver, &updates) {
                    Ok(()) => return,
                    Err(error) => {
                        if updates.unbounded_send(Err(format!("{error:#}"))).is_err() {
                            return;
                        }
                        match receiver.recv_timeout(RETRY_DELAY) {
                            Ok(Request::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => return,
                            _ => {}
                        }
                    }
                }
            }
        });
        while let Some(update) = incoming.next().await {
            if output.send(update).await.is_err() {
                return;
            }
        }
    })
}

fn forward(
    roots: &[PathBuf],
    immutable: &Path,
    requests: &mpsc::Sender<Request>,
    receiver: &mpsc::Receiver<Request>,
    updates: &channel::mpsc::UnboundedSender<Update>,
) -> Result<()> {
    let plan = Arc::new(Mutex::new(WatchPlan::default()));
    let callback_plan = plan.clone();
    let callback_requests = requests.clone();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<Event>| {
        let request = match event {
            Ok(event) if callback_plan.lock().unwrap().relevant(&event) => Request::Changed,
            Ok(_) => return,
            Err(error) => Request::Failed(error.to_string()),
        };
        let _ = callback_requests.send(request);
    })?;
    let mut watched = BTreeSet::new();
    loop {
        let desired = WatchPlan::build(roots, immutable)?;
        let directories = desired.0.keys().cloned().collect::<BTreeSet<_>>();
        *plan.lock().unwrap() = desired;
        for directory in &directories {
            // Re-arm even existing paths: a directory may have been replaced
            // since the previous event, leaving a watch on its old inode.
            watcher
                .watch(directory, RecursiveMode::NonRecursive)
                .with_context(|| format!("could not watch {}", directory.display()))?;
        }
        for directory in watched.difference(&directories) {
            let _ = watcher.unwatch(directory);
        }
        watched = directories;
        // Register before requesting a scan, including on the initial pass.
        if updates.unbounded_send(Ok(())).is_err() {
            return Ok(());
        }
        match receiver.recv() {
            Ok(Request::Stop) | Err(_) => return Ok(()),
            Ok(Request::Failed(error)) => bail!(error),
            Ok(Request::Changed) => {}
        }
        let deadline = Instant::now() + MAX_DEBOUNCE;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            match receiver.recv_timeout(DEBOUNCE.min(remaining)) {
                Ok(Request::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
                Ok(Request::Failed(error)) => bail!(error),
                Ok(Request::Changed) => {}
                Err(mpsc::RecvTimeoutError::Timeout) => break,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::futures::FutureExt;
    use notify::event::CreateKind;
    use std::os::unix::fs::symlink;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "picky-app-watch-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed),
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn path(&self, relative: &str) -> PathBuf {
            self.0.join(relative)
        }

        fn directory(&self, relative: &str) -> PathBuf {
            let path = self.path(relative);
            fs::create_dir_all(&path).unwrap();
            path
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn immutable_roots_use_no_watches() {
        let fixture = Fixture::new();
        let store = fixture.directory("store");
        let apps = fixture.directory("store/generation/share/applications");
        let plan = WatchPlan::build(&[apps], &store).unwrap();
        assert!(plan.0.is_empty());
    }

    #[test]
    fn traces_relative_and_indirect_profile_links_to_mutable_parents() {
        let fixture = Fixture::new();
        let store = fixture.directory("store");
        fixture.directory("home");
        fixture.directory("profiles");
        fixture.directory("store/generation/share/applications");
        symlink("../profiles/current", fixture.path("home/profile")).unwrap();
        symlink("../store/generation", fixture.path("profiles/current")).unwrap();
        let plan =
            WatchPlan::build(&[fixture.path("home/profile/share/applications")], &store).unwrap();
        assert_eq!(
            plan.0.keys().cloned().collect::<BTreeSet<_>>(),
            BTreeSet::from([fixture.path("home"), fixture.path("profiles"),])
        );
        assert!(
            !plan.relevant(
                &Event::new(EventKind::Create(CreateKind::File))
                    .add_path(fixture.path("profiles/unrelated"))
            )
        );
        assert!(
            plan.relevant(
                &Event::new(EventKind::Create(CreateKind::File))
                    .add_path(fixture.path("profiles/current"))
            )
        );
    }

    #[test]
    fn local_trees_watch_directories_and_skip_store_link_targets() {
        let fixture = Fixture::new();
        let apps = fixture.directory("applications");
        fixture.directory("applications/wine/Programs");
        let store_apps = fixture.directory("store/generation/applications");
        symlink(&store_apps, apps.join("immutable")).unwrap();
        for index in 0..100 {
            fs::write(apps.join(format!("{index}.desktop")), "test").unwrap();
        }
        let plan = WatchPlan::build(std::slice::from_ref(&apps), &fixture.path("store")).unwrap();
        assert_eq!(plan.0.len(), 4); // application directories plus their parent
        assert!(
            plan.0
                .keys()
                .all(|path| !path.starts_with(fixture.path("store")))
        );
        assert!(plan.0[&apps].all_children);
        assert!(
            !plan.relevant(
                &Event::new(EventKind::Access(AccessKind::Open(AccessMode::Read)))
                    .add_path(apps.join("0.desktop"))
            )
        );
        assert!(
            plan.relevant(
                &Event::new(EventKind::Access(AccessKind::Close(AccessMode::Write)))
                    .add_path(apps.join("0.desktop"))
            )
        );
    }

    #[test]
    fn missing_application_directories_watch_the_first_missing_component() {
        let fixture = Fixture::new();
        let apps = fixture.path("new/share/applications");
        let plan = WatchPlan::build(std::slice::from_ref(&apps), &fixture.path("store")).unwrap();
        assert_eq!(plan.0.len(), 1);
        assert_eq!(
            plan.0[&fixture.0].children,
            BTreeSet::from([OsString::from("new")])
        );
        fs::create_dir_all(&apps).unwrap();
        let plan = WatchPlan::build(std::slice::from_ref(&apps), &fixture.path("store")).unwrap();
        assert!(plan.0[&apps].all_children);
    }

    async fn next_update(stream: &mut (impl Stream<Item = Update> + Unpin)) {
        iced::futures::select! {
            update = stream.next().fuse() => update.expect("watcher stopped").unwrap(),
            _ = FutureExt::fuse(async_io::Timer::after(Duration::from_secs(4))) => panic!("application change timed out"),
        }
    }

    async fn remains_quiet(stream: &mut (impl Stream<Item = Update> + Unpin)) {
        iced::futures::select! {
            update = stream.next().fuse() => panic!("unexpected application change: {update:?}"),
            _ = FutureExt::fuse(async_io::Timer::after(DEBOUNCE * 2)) => {},
        }
    }

    #[test]
    fn native_watcher_detects_create_edit_delete_and_ignores_reads() {
        async_io::block_on(async {
            let fixture = Fixture::new();
            let apps = fixture.directory("applications");
            let mut feed = Box::pin(watch_roots(vec![apps.clone()], fixture.path("store")));
            next_update(&mut feed).await;
            let desktop = apps.join("test.desktop");
            fs::write(&desktop, "original").unwrap();
            next_update(&mut feed).await;
            fs::write(&desktop, "edited").unwrap();
            next_update(&mut feed).await;
            fs::read_to_string(&desktop).unwrap();
            remains_quiet(&mut feed).await;
            fs::remove_file(&desktop).unwrap();
            next_update(&mut feed).await;
        });
    }

    #[test]
    fn native_watcher_follows_profile_switches_and_rebuilds_its_watch_set() {
        async_io::block_on(async {
            let fixture = Fixture::new();
            let store = fixture.directory("store");
            let generation = fixture.directory("store/generation/share/applications");
            let mutable = fixture.directory("mutable/share/applications");
            let profile = fixture.path("profile");
            symlink(fixture.path("store/generation"), &profile).unwrap();
            let mut feed = Box::pin(watch_roots(vec![profile.join("share/applications")], store));
            next_update(&mut feed).await;
            fs::write(generation.join("ignored.desktop"), "immutable fixture").unwrap();
            remains_quiet(&mut feed).await;
            symlink(fixture.path("mutable"), fixture.path("next-profile")).unwrap();
            fs::rename(fixture.path("next-profile"), &profile).unwrap();
            next_update(&mut feed).await;
            fs::write(mutable.join("new.desktop"), "new").unwrap();
            next_update(&mut feed).await;
            symlink(
                fixture.path("store/generation"),
                fixture.path("next-profile"),
            )
            .unwrap();
            fs::rename(fixture.path("next-profile"), &profile).unwrap();
            next_update(&mut feed).await;
            fs::write(mutable.join("new.desktop"), "no longer watched").unwrap();
            remains_quiet(&mut feed).await;
        });
    }

    #[test]
    fn native_watcher_recovers_replaced_roots_and_tracks_new_subdirectories() {
        async_io::block_on(async {
            let fixture = Fixture::new();
            let apps = fixture.directory("applications");
            let mut feed = Box::pin(watch_roots(vec![apps.clone()], fixture.path("store")));
            next_update(&mut feed).await;
            fs::rename(&apps, fixture.path("old-applications")).unwrap();
            fs::create_dir_all(&apps).unwrap();
            next_update(&mut feed).await;
            let nested = apps.join("wine/Programs");
            fs::create_dir_all(&nested).unwrap();
            next_update(&mut feed).await;
            fs::write(nested.join("new.desktop"), "new").unwrap();
            next_update(&mut feed).await;
        });
    }

    #[test]
    fn overflow_events_force_a_rescan() {
        let event = Event::new(EventKind::Other).set_flag(notify::event::Flag::Rescan);
        assert!(WatchPlan::default().relevant(&event));
    }
}
