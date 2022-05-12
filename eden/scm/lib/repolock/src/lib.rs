/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use std::error;
use std::fmt;
use std::fs;
use std::fs::File;
use std::fs::OpenOptions;
use std::fs::Permissions;
use std::io;
use std::io::Write;
use std::ops::Add;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::path::PathBuf;
use std::thread::sleep;
use std::time::Duration;
use std::time::SystemTime;

use configmodel::Config;
use configmodel::ConfigExt;
use fs2::FileExt;
use util::lock::PathLock;

const WORKING_COPY_NAME: &str = "wlock";

pub fn lock_working_copy(config: &dyn Config, dot_hg: &Path) -> anyhow::Result<File, LockError> {
    lock(
        config,
        dot_hg,
        WORKING_COPY_NAME,
        format!(
            "{}:{}",
            hostname::get()?.to_string_lossy(),
            std::process::id()
        )
        .as_bytes(),
    )
}

pub fn lock_store(_config: &dyn Config, _dot_hg: &Path) -> anyhow::Result<File, LockError> {
    todo!("be sure to enforce wlock -> lock acquisition order to avoid deadlocks")
}

/// lock loops until it can acquire the specified lock, subject to
/// ui.timeout timeout. Errors other than lock contention are
/// propagated immediately with no retries.
pub fn lock(
    config: &dyn Config,
    dir: &Path,
    name: &str,
    contents: &[u8],
) -> anyhow::Result<File, LockError> {
    let now = SystemTime::now();

    let deadline = now.add(Duration::from_secs_f64(
        config.get_or_default("ui", "timeout")?,
    ));

    let warn_deadline = now.add(Duration::from_secs_f64(
        config.get_or_default("ui", "timeout.warn")?,
    ));

    let backoff = Duration::from_secs_f64(config.get_or("devel", "lock_backoff", || 1.0)?);

    loop {
        match try_lock(dir, name, contents) {
            Ok(f) => return Ok(f),
            Err(err) => match err {
                LockError::Contended(_) => {
                    // TODO: add user friendly debugging similar to Python locks.

                    let now = SystemTime::now();
                    if now >= warn_deadline {
                        tracing::warn!(name, "lock contended");
                    } else {
                        tracing::info!(name, "lock contended");
                    };

                    if now >= deadline {
                        return Err(err);
                    }

                    sleep(backoff)
                }
                _ => return Err(err),
            },
        }
    }
}

/// try_lock attempts to acquire an advisory file lock and write
/// specified contents. Lock acquisition and content writing are
/// atomic as long as the content reader also uses this method. If
/// the lock is not available, LockContendederror is returned
/// immediately containing the lock's current contents.
pub fn try_lock(dir: &Path, name: &str, contents: &[u8]) -> anyhow::Result<File, LockError> {
    // Our locking strategy uses three files:
    //   1. An empty advisory lock file at the directory level.
    //   2. An empty advisory lock file named <name>.lock. This file is returned.
    //   3. A plain file named <name>.data which contains the contents.
    //
    //  Readers and writers acquire the directory lock first. This
    //  ensures atomicity across lock acquisition and content
    //  writing.
    let _dir_lock = PathLock::exclusive(dir.join(".dir_lock"))?;

    #[cfg(unix)]
    let _ = _dir_lock
        .as_file()
        .set_permissions(Permissions::from_mode(0o666));

    let name = sanitize_lock_name(name);

    let path = dir.join(name).with_extension("data");
    let lock_file = OpenOptions::new()
        .write(true)
        .create(true)
        .open(path.with_extension("lock"))?;

    #[cfg(unix)]
    let _ = lock_file.set_permissions(Permissions::from_mode(0o666));

    match lock_file.try_lock_exclusive() {
        Ok(_) => {}
        Err(err) if err.kind() == fs2::lock_contended_error().kind() => {
            let contents = fs::read(&path)?;
            return Err(LockContendedError { path, contents }.into());
        }
        Err(err) => return Err(err.into()),
    };

    let mut contents_file = File::create(path)?;
    #[cfg(unix)]
    let _ = contents_file.set_permissions(Permissions::from_mode(0o666));
    contents_file.write_all(contents.as_ref())?;

    Ok(lock_file)
}

fn sanitize_lock_name(name: &str) -> String {
    // Avoid letting a caller specify "foo.lock" and accidentally
    // interfering with the underlying locking details. This is
    // mainly for compatibility during python lock transition to
    // avoid a python lock "foo.lock" accidentally colliding with
    // the rust lock file.
    name.replace('.', "_")
}

#[derive(thiserror::Error, Debug)]
pub enum LockError {
    #[error(transparent)]
    ConfigError(#[from] configmodel::Error),
    #[error(transparent)]
    Contended(#[from] LockContendedError),
    #[error(transparent)]
    Io(#[from] io::Error),
}

#[derive(Debug)]
pub struct LockContendedError {
    pub path: PathBuf,
    pub contents: Vec<u8>,
}

impl error::Error for LockContendedError {}

impl fmt::Display for LockContendedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "lock {:?} contended", self.path)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::thread;
    use std::thread::spawn;

    use anyhow::Result;

    use super::*;

    #[test]
    fn test_try_lock() -> Result<()> {
        let tmp = tempfile::tempdir()?;

        {
            let _foo_lock = try_lock(tmp.path(), "foo", "some contents".as_bytes())?;

            // Can get current lock data via lock contended error.
            if let Err(LockError::Contended(LockContendedError { contents, .. })) =
                try_lock(tmp.path(), "foo", "bar".as_bytes())
            {
                assert_eq!("some contents".as_bytes(), contents);
            } else {
                panic!("expected LockContendedError")
            }
        }

        // Now we can acquire "foo" lock since above lock has been dropped.
        let _foo_lock = try_lock(tmp.path(), "foo", "some contents".as_bytes())?;

        Ok(())
    }

    #[test]
    #[cfg(unix)]
    fn test_try_lock_permissions() -> Result<()> {
        let tmp = tempfile::tempdir()?;

        try_lock(tmp.path(), "foo", "some contents".as_bytes())?;

        let assert_666 = |name: &str| {
            assert_eq!(
                tmp.path()
                    .join(name)
                    .metadata()
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o666
            );
        };

        assert_666(".dir_lock");
        assert_666("foo.lock");
        assert_666("foo.data");

        Ok(())
    }

    // Test readers never see incomplete or inconsistent lock data
    // contents.
    #[test]
    fn test_lock_atomicity() -> Result<()> {
        let tmp = tempfile::tempdir()?;

        // Two threads taking turns with the lock. If lock is
        // unavailable, the thread knows the lock contents should be
        // that of the other thread (because there are only two
        // threads).
        let threads: Vec<_> = vec!["a", "b"]
            .into_iter()
            .map(|c| {
                // Make contents big so we include the case where
                // writing the contents takes multiple writes.
                let my_contents = c.repeat(1_000_000);
                let other = if c == "a" { "b" } else { "a" };
                let other_contents = other.repeat(1_000_000);
                let path = tmp.path().to_path_buf();
                thread::spawn(move || {
                    for _ in 0..10 {
                        match try_lock(&path, "foo", my_contents.as_bytes()) {
                            Ok(_) => {}
                            Err(LockError::Contended(LockContendedError { contents, .. })) => {
                                assert_eq!(other_contents.as_bytes(), contents);
                            }
                            _ => panic!("unexpected result"),
                        }
                    }
                })
            })
            .collect();

        for t in threads {
            t.join().unwrap();
        }

        Ok(())
    }

    #[test]
    fn test_lock_loop() -> Result<()> {
        let tmp = tempfile::tempdir()?;

        let mut cfg = BTreeMap::from([("ui.timeout", "0.001"), ("devel.lock_backoff", "0.001")]);

        let first = lock(&cfg, tmp.path(), "foo", "contents".as_bytes())?;

        assert!(matches!(
            lock(&cfg, tmp.path(), "foo", "contents".as_bytes()),
            Err(LockError::Contended(_))
        ));

        cfg.insert("ui.timeout", "60");

        let dropper = spawn(move || {
            sleep(Duration::from_millis(5));
            drop(first);
        });

        assert!(lock(&cfg, tmp.path(), "foo", "contents".as_bytes()).is_ok());

        dropper.join().unwrap();

        Ok(())
    }

    #[test]
    fn test_working_copy() -> Result<()> {
        let tmp = tempfile::tempdir()?;

        let cfg = BTreeMap::<&str, &str>::new();

        let _wlock = lock_working_copy(&cfg, tmp.path())?;

        // Make sure locked the right file, and check the contents.
        match try_lock(tmp.path(), WORKING_COPY_NAME, "foo".as_bytes()) {
            Err(LockError::Contended(LockContendedError { contents, .. })) => {
                assert_eq!(
                    String::from_utf8(contents)?,
                    format!(
                        "{}:{}",
                        hostname::get()?.to_string_lossy(),
                        std::process::id()
                    )
                );
            }
            _ => panic!("lock should be contended"),
        };

        Ok(())
    }
}
