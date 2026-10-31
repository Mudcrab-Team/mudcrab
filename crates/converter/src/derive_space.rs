//! Per-derivation write admission shared by payload workers and SQLite.
use color_eyre::{
    Result,
    eyre::{WrapErr, bail, ensure, eyre},
};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Mutex, MutexGuard},
};

pub(crate) const RETAINED_FREE_BYTES: u64 = 10 * 1024 * 1024 * 1024;
// A filesystem sample covers at most 8 MiB of SQLite writes, rather than every page.
const SQLITE_CHECK_BYTES: u64 = 8 * 1024 * 1024;

type FreeSpaceReader = dyn FnMut(&Path) -> Result<u64> + Send;

struct State {
    reader: Box<FreeSpaceReader>,
    credit: u64,
    lowest: Option<u64>,
    failure: Option<String>,
    samples: u64,
}

pub(crate) struct SpaceGuard {
    path: PathBuf,
    allocation_unit: u64,
    state: Mutex<State>,
}

impl SpaceGuard {
    pub(crate) fn new(
        path: &Path,
        reader: impl FnMut(&Path) -> Result<u64> + Send + 'static,
    ) -> Result<Self> {
        Ok(Self {
            path: path.to_owned(),
            allocation_unit: allocation_unit(path)?,
            state: Mutex::new(State {
                reader: Box::new(reader),
                credit: 0,
                lowest: None,
                failure: None,
                samples: 0,
            }),
        })
    }

    pub(crate) fn with_write<T>(&self, bytes: u64, write: impl FnOnce() -> Result<T>) -> Result<T> {
        self.perform(bytes, false, write)
    }

    fn lock(&self) -> Result<MutexGuard<'_, State>> {
        self.state
            .lock()
            .map_err(|_| eyre!("write-space guard poisoned on {}", self.path.display()))
    }

    fn sample(&self, state: &mut State) -> Result<u64> {
        if let Some(failure) = &state.failure {
            bail!("{failure}");
        }
        let free = match (state.reader)(&self.path)
            .wrap_err_with(|| format!("cannot query write-space on {}", self.path.display()))
        {
            Ok(free) => free,
            Err(error) => {
                state.failure = Some(format!("{error:#}"));
                return Err(error);
            }
        };
        state.lowest = Some(state.lowest.map_or(free, |lowest| lowest.min(free)));
        state.samples += 1;
        Ok(free)
    }

    /// Samples the volume for upfront admission or the final pre-publication check.
    pub(crate) fn available(&self) -> Result<u64> {
        let mut state = self.lock()?;
        state.credit = 0;
        self.sample(&mut state)
    }

    pub(crate) fn checkpoint(&self) -> Result<()> {
        self.perform(0, true, || Ok(()))
    }

    /// A swallowed cache-reuse or SQLite error must still stop publication.
    pub(crate) fn check_failure(&self) -> Result<()> {
        if let Some(failure) = &self.lock()?.failure {
            bail!("{failure}");
        }
        Ok(())
    }

    fn perform<T>(
        &self,
        bytes: u64,
        refresh: bool,
        write: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        let charge = bytes
            .div_ceil(self.allocation_unit)
            .checked_mul(self.allocation_unit)
            .ok_or_else(|| eyre!("write-space allocation overflow"))?;
        let mut state = self.lock()?;
        if let Some(failure) = &state.failure {
            bail!("{failure}");
        }
        if refresh || charge > state.credit {
            let free = self.sample(&mut state)?;
            let needed = RETAINED_FREE_BYTES
                .checked_add(charge)
                .ok_or_else(|| eyre!("write-space requirement overflow"))?;
            if free < needed {
                let message = format!(
                    "insufficient write-space on {}: needed {needed} bytes (next allocation {charge} bytes plus retained floor {RETAINED_FREE_BYTES} bytes), free {free} bytes",
                    self.path.display()
                );
                state.failure = Some(message.clone());
                bail!("{message}");
            }
            state.credit = if refresh {
                charge
            } else {
                (free - RETAINED_FREE_BYTES).min(SQLITE_CHECK_BYTES.max(charge))
            };
        }
        state.credit -= charge;
        // Keep this lock through the write: another worker must not sample free bytes before
        // an already admitted write has reached the volume and spend the same bytes again.
        let result = write();
        if refresh {
            state.credit = 0;
        }
        result
    }

    /// Reserve a complete file and missing parent directories before any of them is created.
    /// The extra allocation unit covers the new directory entry; predictions never authorize IO.
    pub(crate) fn with_file<T>(
        &self,
        path: &Path,
        bytes: u64,
        write: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        let mut directories = 1_u64;
        let mut parent = path.parent();
        while let Some(directory) = parent {
            if directory.try_exists()? {
                break;
            }
            directories = directories
                .checked_add(1)
                .ok_or_else(|| eyre!("directory allocation overflow"))?;
            parent = directory.parent();
        }
        let metadata = directories
            .checked_mul(self.allocation_unit)
            .ok_or_else(|| eyre!("directory allocation overflow"))?;
        let bytes = bytes
            .checked_add(metadata)
            .ok_or_else(|| eyre!("file allocation overflow"))?;
        self.perform(bytes, true, write)
    }

    pub(crate) fn write_file(&self, path: &Path, bytes: &[u8]) -> Result<()> {
        self.with_file(path, bytes.len() as u64, || {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(path, bytes)?;
            Ok(())
        })
    }
}

impl Drop for SpaceGuard {
    fn drop(&mut self) {
        let state = self
            .state
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(lowest) = state.lowest {
            eprintln!(
                "derive-clusters write-space: volume path {}, lowest free {lowest} bytes, retained floor {RETAINED_FREE_BYTES} bytes, {} samples",
                self.path.display(),
                state.samples
            );
        }
    }
}

#[cfg(windows)]
fn allocation_unit(path: &Path) -> Result<u64> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{GetDiskFreeSpaceW, GetVolumePathNameW};
    let existing = path
        .ancestors()
        .find(|ancestor| ancestor.is_dir())
        .ok_or_else(|| eyre!("no existing ancestor for write volume {}", path.display()))?;
    let wide: Vec<u16> = existing.as_os_str().encode_wide().chain(Some(0)).collect();
    ensure!(
        !wide[..wide.len() - 1].contains(&0),
        "NUL in write volume path"
    );
    let mut volume = vec![0_u16; 32_768];
    // SAFETY: terminated UTF-16 input and live UTF-16 output with its exact capacity.
    let found =
        unsafe { GetVolumePathNameW(wide.as_ptr(), volume.as_mut_ptr(), volume.len() as u32) };
    if found == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let (mut sectors, mut bytes, mut free_clusters, mut clusters) = (0, 0, 0, 0);
    // SAFETY: volume is terminated by GetVolumePathNameW; all output pointers are writable.
    let found = unsafe {
        GetDiskFreeSpaceW(
            volume.as_ptr(),
            &mut sectors,
            &mut bytes,
            &mut free_clusters,
            &mut clusters,
        )
    };
    if found == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let unit = u64::from(sectors) * u64::from(bytes);
    ensure!(unit > 0, "zero allocation unit for {}", path.display());
    Ok(unit)
}

#[cfg(unix)]
fn allocation_unit(path: &Path) -> Result<u64> {
    let existing = path
        .ancestors()
        .find(|ancestor| ancestor.is_dir())
        .ok_or_else(|| eyre!("no existing ancestor for write volume {}", path.display()))?;
    let unit = rustix::fs::statvfs(existing)?.f_frsize;
    ensure!(unit > 0, "zero allocation unit for {}", path.display());
    Ok(unit)
}

#[cfg(not(any(windows, unix)))]
fn allocation_unit(_: &Path) -> Result<u64> {
    bail!("derive write-space guard unsupported on this platform")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    };

    #[test]
    fn write_floor_refuses_before_creating_file() {
        let directory = tempfile::tempdir().unwrap();
        let guard = SpaceGuard::new(directory.path(), |_| Ok(RETAINED_FREE_BYTES)).unwrap();
        let output = directory.path().join("payload.bin");
        let error = guard.write_file(&output, b"payload").unwrap_err();
        let error = format!("{error:#}");
        assert!(
            error.contains("needed") && error.contains("free"),
            "{error}"
        );
        assert!(
            error.contains(&directory.path().display().to_string()),
            "{error}"
        );
        assert!(!output.exists());
    }

    #[test]
    fn sqlite_writes_share_chunk_budget_and_resample_before_crossing_floor() {
        let directory = tempfile::tempdir().unwrap();
        let reads = Arc::new(AtomicUsize::new(0));
        let sampled = reads.clone();
        let guard = SpaceGuard::new(directory.path(), move |_| {
            let sample = sampled.fetch_add(1, Ordering::SeqCst);
            Ok(RETAINED_FREE_BYTES + if sample == 0 { SQLITE_CHECK_BYTES } else { 0 })
        })
        .unwrap();
        let written = AtomicU64::new(0);
        for _ in 0..(SQLITE_CHECK_BYTES / 65_536) {
            guard
                .with_write(65_536, || {
                    written.fetch_add(65_536, Ordering::SeqCst);
                    Ok(())
                })
                .unwrap();
        }
        assert!(
            guard
                .with_write(65_536, || {
                    written.fetch_add(65_536, Ordering::SeqCst);
                    Ok(())
                })
                .is_err()
        );
        assert_eq!(written.load(Ordering::SeqCst), SQLITE_CHECK_BYTES);
        assert_eq!(reads.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn concurrent_workers_cannot_spend_the_same_free_bytes() {
        let directory = tempfile::tempdir().unwrap();
        let free = Arc::new(AtomicU64::new(RETAINED_FREE_BYTES + 65_536));
        let sampled = free.clone();
        let guard = SpaceGuard::new(
            directory.path(),
            move |_| Ok(sampled.load(Ordering::SeqCst)),
        )
        .unwrap();
        std::thread::scope(|scope| {
            let handles: Vec<_> = (0..2)
                .map(|_| {
                    scope.spawn(|| {
                        guard
                            .with_write(65_536, || {
                                free.fetch_sub(65_536, Ordering::SeqCst);
                                Ok(())
                            })
                            .is_ok()
                    })
                })
                .collect();
            let admitted = handles
                .into_iter()
                .map(|handle| usize::from(handle.join().unwrap()))
                .sum::<usize>();
            assert_eq!(admitted, 1);
        });
        assert_eq!(free.load(Ordering::SeqCst), RETAINED_FREE_BYTES);
    }

    #[test]
    fn allocation_rounding_protects_a_one_byte_file() {
        let directory = tempfile::tempdir().unwrap();
        let guard = SpaceGuard::new(directory.path(), |_| Ok(RETAINED_FREE_BYTES + 1)).unwrap();
        assert!(guard.allocation_unit > 1);
        assert!(
            guard
                .write_file(&directory.path().join("one-byte.bin"), b"x")
                .is_err()
        );
    }

    #[test]
    fn reader_error_and_floor_refusal_are_sticky() {
        let directory = tempfile::tempdir().unwrap();
        let guard = SpaceGuard::new(directory.path(), |_| {
            color_eyre::eyre::bail!("injected write-space reader failure")
        })
        .unwrap();
        let error = guard.with_write(4096, || Ok(())).unwrap_err();
        assert!(format!("{error:#}").contains("injected write-space reader failure"));
        assert!(guard.with_write(0, || Ok(())).is_err());
    }
}
