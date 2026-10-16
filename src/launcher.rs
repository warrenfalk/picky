use std::env;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::hash::{Hash, Hasher};
use std::io::{self, Read, Seek, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

const STARTUP_TIMEOUT: Duration = Duration::from_secs(5);
const STARTUP_POLL_INTERVAL: Duration = Duration::from_millis(25);

pub fn run() -> Result<()> {
    let InstancePaths {
        lock: lock_path,
        socket: socket_path,
    } = instance_paths(
        env::var_os("PICKY_SOCKET").map(PathBuf::from),
        env::var_os("NIRI_SOCKET").map(PathBuf::from),
        env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from),
    )?;

    if env::args().any(|argument| argument == "--quit") {
        send_request(&socket_path, b'Q')?;
        return Ok(());
    }

    if let Some(parent) = socket_path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create socket directory {}", parent.display()))?;
    }
    let executable = ExecutableIdentity::for_process(std::process::id())
        .context("failed to identify the running picky executable")?;
    let mut quitting_pid = None;
    if let Some(_instance) = acquire_or_focus(&lock_path, STARTUP_TIMEOUT, |pid| {
        activate_or_replace(&socket_path, pid, executable, &mut quitting_pid)
    })? {
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

struct InstancePaths {
    lock: PathBuf,
    socket: PathBuf,
}

fn instance_paths(
    socket_override: Option<PathBuf>,
    niri_socket: Option<PathBuf>,
    runtime_dir: Option<PathBuf>,
) -> Result<InstancePaths> {
    let default_lock = niri_socket
        .map(|socket| socket.with_extension("picky.lock"))
        .or_else(|| runtime_dir.map(|dir| dir.join("picky.lock")));
    if let Some(socket) = socket_override {
        if socket.as_os_str().is_empty() {
            bail!("PICKY_SOCKET must not be empty");
        }
        // Naming the default socket explicitly must still use its existing lock.
        if let Some(lock) = default_lock.filter(|lock| lock.with_extension("socket") == socket) {
            return Ok(InstancePaths { lock, socket });
        }
        // Append instead of replacing the extension: distinct socket names must
        // have distinct locks, even when only their extensions differ.
        let mut lock = socket.as_os_str().to_owned();
        lock.push(".lock");
        return Ok(InstancePaths {
            lock: PathBuf::from(lock),
            socket,
        });
    }

    // Preserve the installed resident's paths, scoped to its compositor session.
    let lock = default_lock.context("PICKY_SOCKET, NIRI_SOCKET or XDG_RUNTIME_DIR must be set")?;
    Ok(InstancePaths {
        socket: lock.with_extension("socket"),
        lock,
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ExecutableIdentity {
    device: u64,
    inode: u64,
}

impl ExecutableIdentity {
    fn for_process(pid: u32) -> io::Result<Self> {
        // /proc follows the executable actually running, even after Cargo or an
        // installation replaces its pathname. A live executable cannot be
        // overwritten in place on Linux, so a rebuild gets a different inode.
        let metadata = fs::metadata(format!("/proc/{pid}/exe"))?;
        Ok(Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }
}

fn activate_or_replace(
    socket_path: &Path,
    pid: u32,
    executable: ExecutableIdentity,
    quitting_pid: &mut Option<u32>,
) -> Result<bool> {
    if *quitting_pid == Some(pid) {
        return Ok(false);
    }
    let mut stream = match UnixStream::connect(socket_path) {
        Ok(stream) => stream,
        Err(_) => return Ok(false),
    };
    // The owner may have exited since its PID was read. Never send a quit
    // intended for that owner to a replacement which already owns the socket.
    if peer_pid(&stream).context("failed to identify the activation socket owner")? != pid {
        return Ok(false);
    }
    let resident = match ExecutableIdentity::for_process(pid) {
        Ok(identity) => identity,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to identify picky process {pid}"));
        }
    };
    let same_build = resident == executable;
    let command = if same_build { b'A' } else { b'Q' };
    if send_command(&mut stream, command).is_err() {
        return Ok(false);
    }
    if same_build {
        return Ok(true);
    }

    // The existing quit command also works with residents predating this build
    // check. Wait for the owner to release its lock before replacing its socket.
    *quitting_pid = Some(pid);
    Ok(false)
}

fn peer_pid(stream: &UnixStream) -> io::Result<u32> {
    let mut credentials = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut length = std::mem::size_of_val(&credentials) as libc::socklen_t;
    // SAFETY: The live socket descriptor and writable ucred buffer are valid;
    // length describes that buffer for the Linux SO_PEERCRED socket option.
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            std::ptr::from_mut(&mut credentials).cast(),
            &mut length,
        )
    };
    if result == -1 {
        return Err(io::Error::last_os_error());
    }
    u32::try_from(credentials.pid).map_err(|_| io::Error::other("invalid socket owner PID"))
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
    send_command(&mut stream, command)
}

fn send_command(stream: &mut UnixStream, command: u8) -> Result<()> {
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
            bail!("timed out waiting for the existing picky instance to respond or exit");
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
    fn default_paths_still_reach_the_installed_resident() {
        let paths = instance_paths(
            None,
            Some(PathBuf::from("/run/user/1000/niri.wayland-1.42.sock")),
            Some(PathBuf::from("/run/user/1000")),
        )
        .unwrap();
        assert_eq!(
            paths.lock,
            Path::new("/run/user/1000/niri.wayland-1.42.picky.lock")
        );
        assert_eq!(
            paths.socket,
            Path::new("/run/user/1000/niri.wayland-1.42.picky.socket")
        );
        let explicit = instance_paths(
            Some(paths.socket.clone()),
            Some(PathBuf::from("/run/user/1000/niri.wayland-1.42.sock")),
            Some(PathBuf::from("/run/user/1000")),
        )
        .unwrap();
        assert_eq!(explicit.lock, paths.lock);

        let fallback = instance_paths(None, None, Some(PathBuf::from("/run/user/1000"))).unwrap();
        assert_eq!(fallback.lock, Path::new("/run/user/1000/picky.lock"));
        assert_eq!(fallback.socket, Path::new("/run/user/1000/picky.socket"));
    }

    #[test]
    fn socket_override_separates_both_the_socket_and_lock() {
        let paths = instance_paths(
            Some(PathBuf::from("/checkout/.direnv/picky.socket")),
            Some(PathBuf::from("/run/user/1000/niri.sock")),
            Some(PathBuf::from("/run/user/1000")),
        )
        .unwrap();
        assert_eq!(paths.socket, Path::new("/checkout/.direnv/picky.socket"));
        assert_eq!(paths.lock, Path::new("/checkout/.direnv/picky.socket.lock"));

        let other = instance_paths(
            Some(PathBuf::from("/checkout/.direnv/picky.other")),
            None,
            None,
        )
        .unwrap();
        assert_ne!(paths.lock, other.lock);
        assert!(instance_paths(Some(PathBuf::new()), None, None).is_err());
    }

    fn reply_to_one_request(listener: UnixListener) -> thread::JoinHandle<u8> {
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut command = [0];
            stream.read_exact(&mut command).unwrap();
            stream.write_all(b"OK").unwrap();
            command[0]
        })
    }

    #[test]
    fn unchanged_executable_activates_the_resident() {
        let path = TestLock::new();
        let server = reply_to_one_request(UnixListener::bind(&path.0).unwrap());
        let executable = ExecutableIdentity::for_process(std::process::id()).unwrap();
        let mut quitting_pid = None;

        assert!(
            activate_or_replace(&path.0, std::process::id(), executable, &mut quitting_pid)
                .unwrap()
        );
        assert_eq!(server.join().unwrap(), b'A');
        assert_eq!(quitting_pid, None);
    }

    #[test]
    fn rebuilt_executable_requests_quit_and_waits_for_the_lock() {
        use std::sync::mpsc;

        let lock_path = TestLock::new();
        let socket_path = TestLock::new();
        let owner = start_instance(&lock_path.0);
        let listener = UnixListener::bind(&socket_path.0).unwrap();
        let resident = ExecutableIdentity::for_process(std::process::id()).unwrap();
        let rebuilt = ExecutableIdentity {
            inode: resident.inode.wrapping_add(1),
            ..resident
        };
        let (quit_sent, quit_received) = mpsc::channel();
        let (allow_exit, wait_for_exit) = mpsc::channel();

        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut command = [0];
            stream.read_exact(&mut command).unwrap();
            assert_eq!(command[0], b'Q');
            stream.write_all(b"OK").unwrap();
            quit_sent.send(()).unwrap();
            wait_for_exit.recv_timeout(Duration::from_secs(2)).unwrap();
            drop(owner);
        });
        thread::scope(|scope| {
            let replacement = scope.spawn(|| {
                let mut quitting_pid = None;
                acquire_or_focus(&lock_path.0, STARTUP_TIMEOUT, |pid| {
                    activate_or_replace(&socket_path.0, pid, rebuilt, &mut quitting_pid)
                })
                .unwrap()
            });

            quit_received.recv_timeout(Duration::from_secs(2)).unwrap();
            let contender = File::open(&lock_path.0).unwrap();
            assert!(matches!(
                contender.try_lock(),
                Err(TryLockError::WouldBlock)
            ));

            // An acknowledged quit is not enough: the original must release
            // its lock before the new executable can become the resident.
            allow_exit.send(()).unwrap();
            assert!(replacement.join().unwrap().is_some());
        });
        server.join().unwrap();
    }

    #[test]
    fn acknowledged_quit_is_not_sent_again_while_waiting_for_exit() {
        let path = TestLock::new();
        let listener = UnixListener::bind(&path.0).unwrap();
        let server = reply_to_one_request(listener.try_clone().unwrap());
        let resident = ExecutableIdentity::for_process(std::process::id()).unwrap();
        let rebuilt = ExecutableIdentity {
            inode: resident.inode.wrapping_add(1),
            ..resident
        };
        let mut quitting_pid = None;

        assert!(
            !activate_or_replace(&path.0, std::process::id(), rebuilt, &mut quitting_pid).unwrap()
        );
        assert_eq!(server.join().unwrap(), b'Q');
        assert!(
            !activate_or_replace(&path.0, std::process::id(), rebuilt, &mut quitting_pid).unwrap()
        );
        listener.set_nonblocking(true).unwrap();
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
    }

    #[test]
    fn does_not_quit_a_socket_owner_that_changed_since_reading_the_lock() {
        let path = TestLock::new();
        let listener = UnixListener::bind(&path.0).unwrap();
        let resident = ExecutableIdentity::for_process(std::process::id()).unwrap();
        let rebuilt = ExecutableIdentity {
            inode: resident.inode.wrapping_add(1),
            ..resident
        };

        assert!(
            !activate_or_replace(
                &path.0,
                std::process::id().wrapping_add(1),
                rebuilt,
                &mut None,
            )
            .unwrap()
        );
        let (mut connection, _) = listener.accept().unwrap();
        let mut command = [0];
        assert_eq!(connection.read(&mut command).unwrap(), 0);
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
