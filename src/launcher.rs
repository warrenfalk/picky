use std::env;
use std::fs::{File, OpenOptions, TryLockError};
use std::io::{Read, Seek, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
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

    if let Some(_instance) = acquire_or_focus(
        &lock_path,
        STARTUP_TIMEOUT,
        crate::modules::niri_windows::focus_picker_window,
    )? {
        crate::app::run().context("failed to launch picky")?;
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
                // Keep this descriptor alive until the UI exits. Leave the file in
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
            bail!("timed out waiting for the existing picky window");
        }

        // The owner may still be creating its window, or may be exiting. Retry
        // both the lock and the focus request so a relaunch can take over.
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
