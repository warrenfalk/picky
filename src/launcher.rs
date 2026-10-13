use std::env;
use std::fs::{File, OpenOptions, TryLockError};
use std::hash::{Hash, Hasher};
use std::io::{Read, Seek, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

const STARTUP_TIMEOUT: Duration = Duration::from_secs(5);
const STARTUP_POLL_INTERVAL: Duration = Duration::from_millis(25);

pub fn run() -> Result<()> {
    // Scope the lock to the compositor session, including nested Niri sessions.
    let lock_path = env::var_os("NIRI_SOCKET")
        .map(|socket| PathBuf::from(socket).with_extension("picky.lock"))
        .or_else(|| env::var_os("XDG_RUNTIME_DIR").map(|dir| PathBuf::from(dir).join("picky.lock")))
        .context("NIRI_SOCKET or XDG_RUNTIME_DIR must be set")?;

    let socket_path = lock_path.with_extension("socket");
    if env::args().any(|argument| argument == "--quit") {
        send_request(&socket_path, b'Q')?;
        return Ok(());
    }
    if let Some(_instance) =
        acquire_or_focus(&lock_path, STARTUP_TIMEOUT, |_| {
            match send_request(&socket_path, b'A') {
                Ok(()) => Ok(true),
                Err(_) => Ok(false),
            }
        })?
    {
        // Only the lock owner can replace a socket left behind by a crash.
        match std::fs::remove_file(&socket_path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let listener = UnixListener::bind(&socket_path)?;
        std::fs::set_permissions(&socket_path, std::fs::Permissions::from_mode(0o600))?;
        let server = Arc::new(ActivationServer {
            path: socket_path,
            listener: async_io::Async::new(listener)?,
        });
        crate::app::run(server).context("failed to launch picky")?;
    }

    Ok(())
}

#[derive(Debug)]
pub struct ActivationServer {
    path: PathBuf,
    listener: async_io::Async<UnixListener>,
}

impl Hash for ActivationServer {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.path.hash(state);
    }
}

impl Drop for ActivationServer {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[derive(Clone, Debug)]
pub enum Request {
    Activate,
    Quit,
    Failed(String),
}

pub fn subscription(server: Arc<ActivationServer>) -> iced::Subscription<Request> {
    iced::Subscription::run_with(server, requests)
}

fn requests(server: &Arc<ActivationServer>) -> impl iced::futures::Stream<Item = Request> + use<> {
    use iced::futures::SinkExt;
    use iced::futures::io::{AsyncReadExt, AsyncWriteExt};
    let server = server.clone();
    iced::stream::channel(16, async move |mut output| {
        loop {
            let (stream, _) = match server.listener.accept().await {
                Ok(connection) => connection,
                Err(error) => {
                    let _ = output.send(Request::Failed(error.to_string())).await;
                    return;
                }
            };
            // A malformed local client cannot indefinitely block activation.
            let mut stream = &stream;
            let mut command = [0];
            let read = stream.read_exact(&mut command);
            let timer = async_io::Timer::after(Duration::from_secs(1));
            if !matches!(
                iced::futures::future::select(Box::pin(read), Box::pin(timer)).await,
                iced::futures::future::Either::Left((Ok(_), _))
            ) {
                continue;
            }
            let request = match command[0] {
                b'A' => Request::Activate,
                b'Q' => Request::Quit,
                _ => continue,
            };
            if output.send(request).await.is_err() {
                return;
            }
            let _ = stream.write_all(b"OK").await;
        }
    })
}

fn send_request(path: &Path, command: u8) -> Result<()> {
    let mut stream = UnixStream::connect(path)?;
    stream.set_read_timeout(Some(Duration::from_secs(1)))?;
    stream.set_write_timeout(Some(Duration::from_secs(1)))?;
    stream.write_all(&[command])?;
    let mut reply = [0; 2];
    stream.read_exact(&mut reply)?;
    if &reply != b"OK" {
        bail!("invalid activation reply");
    }
    Ok(())
}

fn acquire_or_focus(
    lock_path: &Path,
    timeout: Duration,
    mut focus_existing: impl FnMut(u32) -> Result<bool>,
) -> Result<Option<File>> {
    let mut lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(lock_path)
        .with_context(|| format!("failed to open instance lock {}", lock_path.display()))?;
    let deadline = Instant::now() + timeout;

    loop {
        match lock.try_lock() {
            Ok(()) => {
                lock.set_len(0)?;
                lock.rewind()?;
                writeln!(lock, "{}", std::process::id())?;
                // Keep this descriptor alive until the resident process exits. Leave the file in
                // place so every launch locks the same inode, even after a crash.
                return Ok(Some(lock));
            }
            Err(TryLockError::WouldBlock) => {}
            Err(TryLockError::Error(error)) => {
                return Err(error).context("failed to lock picky instance");
            }
        }

        let mut contents = String::new();
        lock.rewind()?;
        lock.read_to_string(&mut contents)?;
        let pid = contents
            .strip_suffix('\n')
            .and_then(|pid| pid.parse::<u32>().ok());

        if let Some(pid) = pid
            && focus_existing(pid)?
        {
            return Ok(None);
        }

        if Instant::now() >= deadline {
            bail!("timed out waiting for the existing picky instance to respond");
        }

        // The owner may still be starting its listener, or may be exiting. Retry
        // both the lock and the activation request so a relaunch can take over.
        thread::sleep(STARTUP_POLL_INTERVAL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TestLock(PathBuf);

    impl TestLock {
        fn new() -> Self {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            Self(env::temp_dir().join(format!(
                "picky-instance-test-{}-{unique}.lock",
                std::process::id()
            )))
        }
    }

    impl Drop for TestLock {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }

    fn start_instance(path: &Path) -> File {
        acquire_or_focus(path, STARTUP_TIMEOUT, |_| {
            panic!("an unlocked instance must start without a focus request")
        })
        .unwrap()
        .unwrap()
    }

    #[test]
    fn resident_socket_delivers_activation_and_quit_and_is_removed_on_drop() {
        use iced::futures::{StreamExt, executor::block_on};
        let path = TestLock::new();
        let server = Arc::new(ActivationServer {
            path: path.0.clone(),
            listener: async_io::Async::new(UnixListener::bind(&path.0).unwrap()).unwrap(),
        });
        let client_path = path.0.clone();
        let client = thread::spawn(move || {
            send_request(&client_path, b'A').unwrap();
            send_request(&client_path, b'Q').unwrap();
        });
        let received = block_on(requests(&server).take(2).collect::<Vec<_>>());
        assert!(matches!(
            received.as_slice(),
            [Request::Activate, Request::Quit]
        ));
        client.join().unwrap();
        drop(server);
        assert!(!path.0.exists());
    }

    #[test]
    fn second_launch_focuses_owner_without_starting_another_instance() {
        let path = TestLock::new();
        let _owner = start_instance(&path.0);
        let mut focused = Vec::new();

        let second = acquire_or_focus(&path.0, STARTUP_TIMEOUT, |pid| {
            focused.push(pid);
            Ok(true)
        })
        .unwrap();

        assert!(second.is_none());
        assert_eq!(focused, vec![std::process::id()]);
        assert_eq!(
            fs::read_to_string(&path.0).unwrap(),
            format!("{}\n", std::process::id())
        );
    }

    #[test]
    fn second_launch_waits_for_the_owners_window() {
        let path = TestLock::new();
        let _owner = start_instance(&path.0);
        let mut attempts = 0;

        let second = acquire_or_focus(&path.0, STARTUP_TIMEOUT, |_| {
            attempts += 1;
            Ok(attempts == 3)
        })
        .unwrap();

        assert!(second.is_none());
        assert_eq!(attempts, 3);
    }

    #[test]
    fn relaunch_starts_when_the_owner_exits_while_waiting() {
        let path = TestLock::new();
        let mut owner = Some(start_instance(&path.0));

        let replacement = acquire_or_focus(&path.0, STARTUP_TIMEOUT, |_| {
            drop(owner.take());
            Ok(false)
        })
        .unwrap();

        assert!(replacement.is_some());
    }

    #[test]
    fn stale_pid_does_not_prevent_a_new_instance() {
        let path = TestLock::new();
        fs::write(&path.0, "4294967295\n").unwrap();

        let _owner = start_instance(&path.0);

        assert_eq!(
            fs::read_to_string(&path.0).unwrap(),
            format!("{}\n", std::process::id())
        );
    }

    #[test]
    fn unresponsive_owner_never_allows_a_duplicate_instance() {
        let path = TestLock::new();
        let _owner = start_instance(&path.0);

        let error = acquire_or_focus(&path.0, Duration::ZERO, |_| Ok(false)).unwrap_err();

        assert!(error.to_string().contains("timed out"));
        let contender = File::open(&path.0).unwrap();
        assert!(matches!(
            contender.try_lock(),
            Err(TryLockError::WouldBlock)
        ));
    }
}
