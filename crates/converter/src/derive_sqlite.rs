//! A per-connection SQLite VFS that admits native writes through the derive space guard.
//!
//! The native VFS still owns the OS handles, locking, shared memory and durability rules.
//! This shim only intercepts allocation-capable operations; it never becomes the default VFS.
//! SQLite can cache a VFS pointer in a named in-memory connection without opening any file.
//! Consequently each registered descriptor/name is retained until process exit. Normal scope
//! exit unregisters that name and releases the owned space guard; the small descriptor shell
//! remains valid and refuses late opens/writes rather than retaining run state or dangling.

use crate::derive_space::SpaceGuard;
use color_eyre::{Result, eyre::ensure};
use rusqlite::{Connection, OpenFlags, ffi};
use std::{
    cell::UnsafeCell,
    ffi::{CString, c_char, c_int, c_void},
    ops::{Deref, DerefMut},
    panic::{AssertUnwindSafe, catch_unwind},
    path::Path,
    ptr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering},
    },
};

/// The connection closes before its private VFS is unregistered.
pub(crate) struct GuardedConnection {
    connection: Option<Connection>,
    registration: Option<Arc<Registration>>,
}

impl Deref for GuardedConnection {
    type Target = Connection;

    fn deref(&self) -> &Self::Target {
        self.connection
            .as_ref()
            .expect("connection is present until drop")
    }
}

impl DerefMut for GuardedConnection {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.connection
            .as_mut()
            .expect("connection is present until drop")
    }
}

impl Drop for GuardedConnection {
    fn drop(&mut self) {
        if let Some(connection) = self.connection.take()
            && let Err((connection, _)) = connection.close()
        {
            // Safe callers can forget a statement or blob, leaving SQLITE_BUSY on close.
            // Keep that inaccessible connection alive rather than freeing a VFS it still uses.
            // Each open file retains its native callback state. Retirement below releases the
            // owned guard and makes late opens/writes fail closed rather than dangling.
            std::mem::forget(connection);
        }
        if let Some(registration) = self.registration.take() {
            registration.retire();
        }
    }
}

pub(crate) fn open(path: &Path, guard: Option<&Arc<SpaceGuard>>) -> Result<GuardedConnection> {
    let Some(guard) = guard else {
        return Ok(GuardedConnection {
            connection: Some(Connection::open(path)?),
            registration: None,
        });
    };
    let registration = Registration::new(guard)?;
    // The generated name contains only ASCII, hence the original string is UTF-8.
    let name = match registration.name.to_str() {
        Ok(name) => name,
        Err(error) => {
            registration.retire();
            return Err(error.into());
        }
    };
    let connection = match Connection::open_with_flags_and_vfs(path, OpenFlags::default(), name) {
        Ok(connection) => connection,
        Err(error) => {
            registration.retire();
            return Err(error.into());
        }
    };
    let connection = GuardedConnection {
        connection: Some(connection),
        registration: Some(registration),
    };
    // SQLite's unnamed sort/temp files otherwise use the OS temporary volume, not necessarily
    // the derived volume whose floor we retain. This is connection-local, not a global setting.
    // Main, rollback journal, WAL and shared-memory files still use the guarded native VFS.
    connection.execute_batch("PRAGMA temp_store=MEMORY")?;
    Ok(connection)
}

struct Registration {
    vfs: UnsafeCell<ffi::sqlite3_vfs>,
    native: *mut ffi::sqlite3_vfs,
    name: CString,
    // Ownership, rather than an erased borrow, keeps callbacks safe when safe Rust code forgets
    // the wrapper, extracts its Connection through DerefMut, or opens this VFS by its name.
    guard: Mutex<Option<Arc<SpaceGuard>>>,
    registered: AtomicBool,
}

// SAFETY: SQLite owns synchronization of its VFS registry; only SQLite accesses/mutates the
// registered VFS's pNext. All other VFS fields are immutable after registration. The native VFS
// is process-owned and thread-safe. The owned SpaceGuard synchronizes its own state and
// requires a static Send free-space reader. Its mutex serializes callback use with retirement.
// A permanent Arc keeps the descriptor/name valid even for a cached no-file VFS reference.
unsafe impl Send for Registration {}
// SAFETY: the same registry, immutable-field and mutex invariants apply to shared references.
unsafe impl Sync for Registration {}

impl Registration {
    fn new(guard: &Arc<SpaceGuard>) -> Result<Arc<Self>> {
        // SAFETY: SQLite initialization and default-VFS lookup are thread-safe SQLite APIs.
        let initialized = unsafe { ffi::sqlite3_initialize() };
        ensure!(
            initialized == ffi::SQLITE_OK,
            "SQLite initialization failed: {initialized}"
        );
        // SAFETY: null asks SQLite for its process-owned default VFS without changing it.
        let native = unsafe { ffi::sqlite3_vfs_find(ptr::null()) };
        ensure!(!native.is_null(), "SQLite has no native VFS");
        // SAFETY: the default VFS is a live process-owned structure.
        let native_version = unsafe { (*native).iVersion };
        ensure!(
            native_version >= 1,
            "unsupported native SQLite VFS version {native_version}"
        );
        // SAFETY: same live native VFS; its file-size field is available in version 1.
        ensure!(
            unsafe { (*native).szOsFile } > 0,
            "native SQLite VFS has no file storage"
        );
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let name = CString::new(format!(
            "mudcrab-derive-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ))?;
        let owner = Arc::new(Self {
            // SAFETY: all fields are integers, pointers or nullable function pointers; zero is
            // valid for them. Required fields are populated before SQLite sees the structure.
            vfs: UnsafeCell::new(unsafe { std::mem::zeroed() }),
            native,
            name,
            guard: Mutex::new(Some(Arc::clone(guard))),
            registered: AtomicBool::new(false),
        });
        // SAFETY: this structure is not yet registered, so no other thread can access it. Every
        // callback forwards the original native VFS pointer, preserving native pAppData.
        unsafe {
            let vfs = &mut *owner.vfs.get();
            vfs.iVersion = native_version.min(3);
            vfs.szOsFile = c_int::try_from(std::mem::size_of::<GuardedFile>())?;
            vfs.mxPathname = (*native).mxPathname;
            vfs.zName = owner.name.as_ptr();
            vfs.pAppData = Arc::as_ptr(&owner).cast_mut().cast();
            vfs.xOpen = Some(vfs_open);
            vfs.xDelete = (*native).xDelete.map(|_| vfs_delete as _);
            vfs.xAccess = (*native).xAccess.map(|_| vfs_access as _);
            vfs.xFullPathname = (*native).xFullPathname.map(|_| vfs_full_pathname as _);
            vfs.xDlOpen = (*native).xDlOpen.map(|_| vfs_dl_open as _);
            vfs.xDlError = (*native).xDlError.map(|_| vfs_dl_error as _);
            vfs.xDlSym = (*native).xDlSym.map(|_| vfs_dl_sym as _);
            vfs.xDlClose = (*native).xDlClose.map(|_| vfs_dl_close as _);
            vfs.xRandomness = (*native).xRandomness.map(|_| vfs_randomness as _);
            vfs.xSleep = (*native).xSleep.map(|_| vfs_sleep as _);
            vfs.xCurrentTime = (*native).xCurrentTime.map(|_| vfs_current_time as _);
            vfs.xGetLastError = (*native).xGetLastError.map(|_| vfs_get_last_error as _);
            if native_version >= 2 {
                vfs.xCurrentTimeInt64 = (*native)
                    .xCurrentTimeInt64
                    .map(|_| vfs_current_time_int64 as _);
            }
            if native_version >= 3 {
                vfs.xSetSystemCall = (*native).xSetSystemCall.map(|_| vfs_set_system_call as _);
                vfs.xGetSystemCall = (*native).xGetSystemCall.map(|_| vfs_get_system_call as _);
                vfs.xNextSystemCall = (*native).xNextSystemCall.map(|_| vfs_next_system_call as _);
            }
            let registered = ffi::sqlite3_vfs_register(owner.vfs.get(), 0);
            ensure!(
                registered == ffi::SQLITE_OK,
                "registering derive SQLite VFS failed: {registered}"
            );
        }
        owner.registered.store(true, Ordering::Release);
        // SQLite has no VFS connection-reference/destructor callback: a safely opened named
        // :memory: connection can retain this raw VFS pointer without ever calling xOpen.
        // Retain one descriptor shell permanently, including its CString and native pointer.
        // retire() unregisters the name and removes the owned guard, so ordinary completed
        // runs do not retain the free-space reader or other run state. Only this small shell
        // accumulates per guarded connection, never a process-global/default space guard.
        let _descriptor_shell = Arc::into_raw(Arc::clone(&owner));
        Ok(owner)
    }

    fn retire(&self) {
        self.unregister();
        let retired = self
            .guard
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        drop(retired);
    }

    fn unregister(&self) {
        if self.registered.swap(false, Ordering::AcqRel) {
            // SAFETY: the registration and its CString remain alive here; unregister removes
            // only this unique VFS, never the native default. Open files retain this Arc.
            unsafe { ffi::sqlite3_vfs_unregister(self.vfs.get()) };
        }
    }

    fn write(&self, bytes: u64, native_write: impl FnOnce() -> c_int) -> c_int {
        // A panic in an injected reader must never unwind through SQLite's C frames.
        catch_unwind(AssertUnwindSafe(|| {
            let active = self
                .guard
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let Some(guard) = active.as_ref() else {
                return ffi::SQLITE_IOERR_WRITE;
            };
            guard
                .with_write(bytes, || Ok(native_write()))
                .unwrap_or(ffi::SQLITE_FULL)
        }))
        .unwrap_or(ffi::SQLITE_IOERR_WRITE)
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        self.unregister();
    }
}

#[repr(C)]
struct GuardedFile {
    header: ffi::sqlite3_file,
    native: *mut ffi::sqlite3_file,
    owner: *const Registration,
    methods: ffi::sqlite3_io_methods,
    // Staging has one cooperative writer. Avoid a native file-size stat on every SQLite page;
    // a failed partial write can only leave this low and make the next reservation larger.
    length: AtomicI64,
}

// SAFETY for callback helpers: SQLite passes the wrapper VFS/file pointers originally supplied
// by this module. Their allocation and native handles stay live until xClose finishes. The
// wrapper header is the first field, and each open file owns one strong Registration reference.
unsafe fn registration<'a>(vfs: *mut ffi::sqlite3_vfs) -> &'a Registration {
    unsafe { &*(*vfs).pAppData.cast::<Registration>() }
}

unsafe fn file<'a>(handle: *mut ffi::sqlite3_file) -> &'a GuardedFile {
    unsafe { &*handle.cast::<GuardedFile>() }
}

unsafe fn native_methods(handle: *mut ffi::sqlite3_file) -> *const ffi::sqlite3_io_methods {
    unsafe { (*file(handle).native).pMethods }
}

unsafe extern "C" fn vfs_open(
    vfs: *mut ffi::sqlite3_vfs,
    name: ffi::sqlite3_filename,
    handle: *mut ffi::sqlite3_file,
    flags: c_int,
    out_flags: *mut c_int,
) -> c_int {
    // SAFETY: all pointers follow SQLite's xOpen contract; native storage is allocated using
    // SQLite's own aligned allocator and initialized before the native VFS receives it.
    unsafe {
        (*handle).pMethods = ptr::null();
        let owner = registration(vfs);
        // A cached no-file SQLite connection may try this VFS after its original scope ends.
        // Hold the mutex through native open, so retirement cannot race the allocation.
        let active = owner
            .guard
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if active.is_none() {
            return ffi::SQLITE_CANTOPEN;
        }
        let Some(open) = (*owner.native).xOpen else {
            return ffi::SQLITE_CANTOPEN;
        };
        let bytes = (*owner.native).szOsFile as usize;
        let native = ffi::sqlite3_malloc64(bytes as u64).cast::<ffi::sqlite3_file>();
        if native.is_null() {
            return ffi::SQLITE_NOMEM;
        }
        ptr::write_bytes(native.cast::<u8>(), 0, bytes);
        let result = open(owner.native, name, native, flags, out_flags);
        if result != ffi::SQLITE_OK {
            // SQLite permits an unsuccessful xOpen to supply xClose for partial cleanup.
            if !(*native).pMethods.is_null()
                && let Some(close) = (*(*native).pMethods).xClose
            {
                close(native);
            }
            ffi::sqlite3_free(native.cast());
            return result;
        }
        if (*native).pMethods.is_null() {
            ffi::sqlite3_free(native.cast());
            return ffi::SQLITE_IOERR;
        }
        let mut length = 0;
        let result = (*(*native).pMethods)
            .xFileSize
            .map_or(ffi::SQLITE_IOERR_FSTAT, |size| size(native, &mut length));
        if result != ffi::SQLITE_OK {
            if let Some(close) = (*(*native).pMethods).xClose {
                close(native);
            }
            ffi::sqlite3_free(native.cast());
            return result;
        }
        let methods = io_methods((*native).pMethods);
        let raw_owner = ptr::from_ref(owner);
        Arc::increment_strong_count(raw_owner);
        let wrapped = handle.cast::<GuardedFile>();
        ptr::write(
            wrapped,
            GuardedFile {
                header: ffi::sqlite3_file {
                    pMethods: ptr::null(),
                },
                native,
                owner: raw_owner,
                methods,
                length: AtomicI64::new(length.max(0)),
            },
        );
        (*wrapped).header.pMethods = ptr::addr_of!((*wrapped).methods);
        ffi::SQLITE_OK
    }
}

unsafe fn io_methods(native: *const ffi::sqlite3_io_methods) -> ffi::sqlite3_io_methods {
    // SAFETY: xOpen returned a valid native method table. Read versioned fields only when the
    // native table advertises them; older VFS implementations may allocate only that prefix.
    unsafe {
        let mut methods = ffi::sqlite3_io_methods {
            iVersion: (*native).iVersion.min(3),
            xClose: Some(io_close),
            xRead: Some(io_read),
            xWrite: Some(io_write),
            xTruncate: Some(io_truncate),
            xSync: Some(io_sync),
            xFileSize: Some(io_file_size),
            xLock: Some(io_lock),
            xUnlock: Some(io_unlock),
            xCheckReservedLock: Some(io_check_reserved_lock),
            xFileControl: Some(io_file_control),
            xSectorSize: Some(io_sector_size),
            xDeviceCharacteristics: Some(io_device_characteristics),
            xShmMap: None,
            xShmLock: None,
            xShmBarrier: None,
            xShmUnmap: None,
            xFetch: None,
            xUnfetch: None,
        };
        if (*native).iVersion >= 2 {
            methods.xShmMap = (*native).xShmMap.map(|_| io_shm_map as _);
            methods.xShmLock = (*native).xShmLock.map(|_| io_shm_lock as _);
            methods.xShmBarrier = (*native).xShmBarrier.map(|_| io_shm_barrier as _);
            methods.xShmUnmap = (*native).xShmUnmap.map(|_| io_shm_unmap as _);
        }
        if (*native).iVersion >= 3 {
            methods.xFetch = (*native).xFetch.map(|_| io_fetch as _);
            methods.xUnfetch = (*native).xUnfetch.map(|_| io_unfetch as _);
        }
        methods
    }
}

unsafe extern "C" fn io_close(handle: *mut ffi::sqlite3_file) -> c_int {
    // SAFETY: xClose is called once per successful xOpen. Preserve native storage and owner
    // through native close, then free precisely the allocations/reference created by xOpen.
    unsafe {
        let wrapped = handle.cast::<GuardedFile>();
        let native = (*wrapped).native;
        let owner = (*wrapped).owner;
        let result = (*(*native).pMethods)
            .xClose
            .map_or(ffi::SQLITE_IOERR_CLOSE, |close| close(native));
        ffi::sqlite3_free(native.cast());
        (*wrapped).header.pMethods = ptr::null();
        drop(Arc::from_raw(owner));
        result
    }
}

unsafe extern "C" fn io_write(
    handle: *mut ffi::sqlite3_file,
    buffer: *const c_void,
    amount: c_int,
    offset: ffi::sqlite3_int64,
) -> c_int {
    // SAFETY: native pointers and buffer are supplied by SQLite for this live file.
    unsafe {
        let wrapped = file(handle);
        let methods = native_methods(handle);
        let Some(write) = (*methods).xWrite else {
            return ffi::SQLITE_IOERR_WRITE;
        };
        let (Ok(amount_bytes), Ok(offset_bytes)) = (u64::try_from(amount), u64::try_from(offset))
        else {
            return ffi::SQLITE_IOERR_WRITE;
        };
        let Some(end) = offset_bytes.checked_add(amount_bytes) else {
            return ffi::SQLITE_IOERR_WRITE;
        };
        let Ok(logical_end) = i64::try_from(end) else {
            return ffi::SQLITE_IOERR_WRITE;
        };
        // Account for a non-sparse gap as well as the supplied bytes. Rewrites are charged too,
        // so reservations remain conservative without guessing native allocation behavior.
        let bytes = amount_bytes
            .max(end.saturating_sub(wrapped.length.load(Ordering::Relaxed).max(0) as u64));
        let result =
            (&*wrapped.owner).write(bytes, || write(wrapped.native, buffer, amount, offset));
        if result == ffi::SQLITE_OK {
            wrapped.length.fetch_max(logical_end, Ordering::Relaxed);
        }
        result
    }
}

unsafe extern "C" fn io_truncate(
    handle: *mut ffi::sqlite3_file,
    length: ffi::sqlite3_int64,
) -> c_int {
    // SAFETY: native method pointers and output storage are valid for this live file.
    unsafe {
        let wrapped = file(handle);
        let methods = native_methods(handle);
        let Some(truncate) = (*methods).xTruncate else {
            return ffi::SQLITE_IOERR_TRUNCATE;
        };
        let old_length = wrapped.length.load(Ordering::Relaxed);
        if length < 0 {
            return ffi::SQLITE_IOERR_TRUNCATE;
        }
        let result = if length <= old_length {
            truncate(wrapped.native, length)
        } else {
            (&*wrapped.owner).write((length - old_length.max(0)) as u64, || {
                truncate(wrapped.native, length)
            })
        };
        if result == ffi::SQLITE_OK {
            wrapped.length.store(length, Ordering::Relaxed);
        }
        result
    }
}

unsafe extern "C" fn io_file_control(
    handle: *mut ffi::sqlite3_file,
    operation: c_int,
    argument: *mut c_void,
) -> c_int {
    // SAFETY: SQLite defines the pointer argument's type for these controls. Other controls
    // are forwarded untouched, with the native file pointer expected by the native VFS.
    unsafe {
        match operation {
            // Native SIZE_HINT/CHUNK_SIZE can preallocate inside the native VFS without calling
            // our xWrite/xTruncate. These are optional optimizations, not transaction semantics.
            ffi::SQLITE_FCNTL_SIZE_HINT | ffi::SQLITE_FCNTL_CHUNK_SIZE => ffi::SQLITE_NOTFOUND,
            ffi::SQLITE_FCNTL_FILE_POINTER => {
                *argument.cast::<*mut ffi::sqlite3_file>() = handle;
                ffi::SQLITE_OK
            }
            ffi::SQLITE_FCNTL_VFS_POINTER => {
                *argument.cast::<*mut ffi::sqlite3_vfs>() = (&*file(handle).owner).vfs.get();
                ffi::SQLITE_OK
            }
            _ => (*native_methods(handle))
                .xFileControl
                .map_or(ffi::SQLITE_NOTFOUND, |control| {
                    control(file(handle).native, operation, argument)
                }),
        }
    }
}

unsafe extern "C" fn io_shm_map(
    handle: *mut ffi::sqlite3_file,
    page: c_int,
    page_size: c_int,
    writable: c_int,
    output: *mut *mut c_void,
) -> c_int {
    // SAFETY: the native version-2 callback and output pointer follow SQLite's xShmMap contract.
    unsafe {
        let wrapped = file(handle);
        let Some(map) = (*native_methods(handle)).xShmMap else {
            return ffi::SQLITE_IOERR_SHMMAP;
        };
        if writable == 0 {
            return map(wrapped.native, page, page_size, writable, output);
        }
        let (Ok(page), Ok(page_size)) = (u64::try_from(page), u64::try_from(page_size)) else {
            return ffi::SQLITE_IOERR_SHMSIZE;
        };
        let Some(bytes) = page
            .checked_add(1)
            .and_then(|pages| pages.checked_mul(page_size))
        else {
            return ffi::SQLITE_IOERR_SHMSIZE;
        };
        // Shared-memory extension happens inside the native VFS, not through this file's
        // xWrite. Charge the entire requested mapping as a conservative allocation bound.
        (&*wrapped.owner).write(bytes, || {
            map(
                wrapped.native,
                page as c_int,
                page_size as c_int,
                writable,
                output,
            )
        })
    }
}

macro_rules! forward_io {
    ($name:ident, $method:ident, ($($arg:ident: $ty:ty),*) -> $result:ty, $fallback:expr) => {
        unsafe extern "C" fn $name(handle: *mut ffi::sqlite3_file, $($arg: $ty),*) -> $result {
            // SAFETY: forward SQLite's arguments unchanged to this file's original native method.
            unsafe { (*native_methods(handle)).$method.map_or($fallback, |method| method(file(handle).native, $($arg),*)) }
        }
    };
}

forward_io!(io_read, xRead, (buffer: *mut c_void, amount: c_int, offset: ffi::sqlite3_int64) -> c_int, ffi::SQLITE_IOERR_READ);
forward_io!(io_sync, xSync, (flags: c_int) -> c_int, ffi::SQLITE_IOERR_FSYNC);
forward_io!(io_file_size, xFileSize, (size: *mut ffi::sqlite3_int64) -> c_int, ffi::SQLITE_IOERR_FSTAT);
forward_io!(io_lock, xLock, (level: c_int) -> c_int, ffi::SQLITE_IOERR_LOCK);
forward_io!(io_unlock, xUnlock, (level: c_int) -> c_int, ffi::SQLITE_IOERR_UNLOCK);
forward_io!(io_check_reserved_lock, xCheckReservedLock, (locked: *mut c_int) -> c_int, ffi::SQLITE_IOERR_CHECKRESERVEDLOCK);
forward_io!(io_sector_size, xSectorSize, () -> c_int, 0);
forward_io!(io_device_characteristics, xDeviceCharacteristics, () -> c_int, 0);
forward_io!(io_shm_lock, xShmLock, (offset: c_int, count: c_int, flags: c_int) -> c_int, ffi::SQLITE_IOERR_SHMLOCK);
forward_io!(io_shm_barrier, xShmBarrier, () -> (), ());
forward_io!(io_shm_unmap, xShmUnmap, (delete: c_int) -> c_int, ffi::SQLITE_IOERR_SHMMAP);
forward_io!(io_fetch, xFetch, (offset: ffi::sqlite3_int64, amount: c_int, output: *mut *mut c_void) -> c_int, ffi::SQLITE_IOERR_MMAP);
forward_io!(io_unfetch, xUnfetch, (offset: ffi::sqlite3_int64, pointer: *mut c_void) -> c_int, ffi::SQLITE_IOERR_MMAP);

macro_rules! forward_vfs {
    ($name:ident, $method:ident, ($($arg:ident: $ty:ty),*) -> $result:ty, $fallback:expr) => {
        unsafe extern "C" fn $name(vfs: *mut ffi::sqlite3_vfs, $($arg: $ty),*) -> $result {
            // SAFETY: preserve the original VFS pointer/pAppData, not the wrapper's, while
            // forwarding every remaining argument unchanged to its original callback.
            unsafe {
                let owner = registration(vfs);
                (*owner.native).$method.map_or($fallback, |method| method(owner.native, $($arg),*))
            }
        }
    };
}

type DlSymbol = Option<unsafe extern "C" fn(*mut ffi::sqlite3_vfs, *mut c_void, *const c_char)>;
forward_vfs!(vfs_delete, xDelete, (name: *const c_char, sync: c_int) -> c_int, ffi::SQLITE_IOERR_DELETE);
forward_vfs!(vfs_access, xAccess, (name: *const c_char, flags: c_int, result: *mut c_int) -> c_int, ffi::SQLITE_IOERR_ACCESS);
forward_vfs!(vfs_full_pathname, xFullPathname, (name: *const c_char, length: c_int, output: *mut c_char) -> c_int, ffi::SQLITE_CANTOPEN);
forward_vfs!(vfs_dl_open, xDlOpen, (name: *const c_char) -> *mut c_void, ptr::null_mut());
forward_vfs!(vfs_dl_error, xDlError, (length: c_int, output: *mut c_char) -> (), ());
forward_vfs!(vfs_dl_sym, xDlSym, (handle: *mut c_void, symbol: *const c_char) -> DlSymbol, None);
forward_vfs!(vfs_dl_close, xDlClose, (handle: *mut c_void) -> (), ());
forward_vfs!(vfs_randomness, xRandomness, (length: c_int, output: *mut c_char) -> c_int, 0);
forward_vfs!(vfs_sleep, xSleep, (micros: c_int) -> c_int, 0);
forward_vfs!(vfs_current_time, xCurrentTime, (output: *mut f64) -> c_int, ffi::SQLITE_ERROR);
forward_vfs!(vfs_get_last_error, xGetLastError, (length: c_int, output: *mut c_char) -> c_int, 0);
forward_vfs!(vfs_current_time_int64, xCurrentTimeInt64, (output: *mut ffi::sqlite3_int64) -> c_int, ffi::SQLITE_ERROR);
forward_vfs!(vfs_set_system_call, xSetSystemCall, (name: *const c_char, function: ffi::sqlite3_syscall_ptr) -> c_int, ffi::SQLITE_NOTFOUND);
forward_vfs!(vfs_get_system_call, xGetSystemCall, (name: *const c_char) -> ffi::sqlite3_syscall_ptr, None);
forward_vfs!(vfs_next_system_call, xNextSystemCall, (name: *const c_char) -> *const c_char, ptr::null());

#[cfg(test)]
mod tests {
    use super::*;
    use crate::derive_space::RETAINED_FREE_BYTES;
    use std::mem::ManuallyDrop;

    #[test]
    fn denied_first_database_write_leaves_main_file_empty() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("clusters.db");
        let guard =
            Arc::new(SpaceGuard::new(directory.path(), |_| Ok(RETAINED_FREE_BYTES)).unwrap());
        let connection = open(&path, Some(&guard)).unwrap();
        let error = connection
            .execute_batch("CREATE TABLE denied(value)")
            .unwrap_err();
        assert!(
            matches!(error, rusqlite::Error::SqliteFailure(ref error, _) if error.code == ffi::ErrorCode::DiskFull),
            "{error}"
        );
        drop(connection);
        assert_eq!(std::fs::metadata(path).unwrap().len(), 0);
    }

    #[test]
    fn denied_journal_write_preserves_committed_database_bytes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("clusters.db");
        {
            let connection = Connection::open(&path).unwrap();
            connection
                .execute_batch("CREATE TABLE data(value); INSERT INTO data VALUES (1)")
                .unwrap();
        }
        let before = std::fs::read(&path).unwrap();
        let guard =
            Arc::new(SpaceGuard::new(directory.path(), |_| Ok(RETAINED_FREE_BYTES)).unwrap());
        let connection = open(&path, Some(&guard)).unwrap();
        let error = connection
            .execute_batch("UPDATE data SET value = 2")
            .unwrap_err();
        assert!(
            matches!(error, rusqlite::Error::SqliteFailure(ref error, _) if error.code == ffi::ErrorCode::DiskFull),
            "{error}"
        );
        let journal = path.with_extension("db-journal");
        assert!(!journal.exists() || std::fs::metadata(journal).unwrap().len() == 0);
        drop(connection);
        assert_eq!(std::fs::read(path).unwrap(), before);
    }

    #[test]
    fn native_sql_errors_remain_sql_errors_and_registration_is_scoped() {
        let directory = tempfile::tempdir().unwrap();
        let guard = Arc::new(SpaceGuard::new(directory.path(), |_| Ok(u64::MAX)).unwrap());
        let connection = open(&directory.path().join("clusters.db"), Some(&guard)).unwrap();
        let name = connection.registration.as_ref().unwrap().name.clone();
        connection
            .execute_batch("CREATE TABLE data(value UNIQUE); INSERT INTO data VALUES (1)")
            .unwrap();
        let error = connection
            .execute("INSERT INTO data VALUES (1)", [])
            .unwrap_err();
        assert!(
            matches!(error, rusqlite::Error::SqliteFailure(ref error, _) if error.code == ffi::ErrorCode::ConstraintViolation),
            "{error}"
        );
        drop(connection);
        // SAFETY: the owned CString is terminated and VFS lookup is a thread-safe SQLite API.
        assert!(unsafe { ffi::sqlite3_vfs_find(name.as_ptr()) }.is_null());
    }

    #[test]
    fn growing_native_truncate_is_admitted_before_file_changes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("clusters.db");
        let guard =
            Arc::new(SpaceGuard::new(directory.path(), |_| Ok(RETAINED_FREE_BYTES)).unwrap());
        let connection = open(&path, Some(&guard)).unwrap();
        // SAFETY: SQLite writes the main sqlite3_file pointer for this file-control opcode;
        // the connection and registration remain alive throughout the direct callback test.
        unsafe {
            let mut handle: *mut ffi::sqlite3_file = ptr::null_mut();
            assert_eq!(
                ffi::sqlite3_file_control(
                    connection.handle(),
                    c"main".as_ptr(),
                    ffi::SQLITE_FCNTL_FILE_POINTER,
                    ptr::from_mut(&mut handle).cast()
                ),
                ffi::SQLITE_OK
            );
            assert!(!handle.is_null());
            assert_eq!(
                (*(*handle).pMethods).xTruncate.unwrap()(handle, 65_536),
                ffi::SQLITE_FULL
            );
        }
        drop(connection);
        assert_eq!(std::fs::metadata(path).unwrap().len(), 0);
    }

    #[test]
    fn extracted_connection_fails_closed_after_wrapper_releases_guard() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("clusters.db");
        let guard =
            Arc::new(SpaceGuard::new(directory.path(), |_| Ok(RETAINED_FREE_BYTES)).unwrap());
        let retained = Arc::downgrade(&guard);
        let mut wrapper = open(&path, Some(&guard)).unwrap();
        let extracted = std::mem::replace(&mut *wrapper, Connection::open_in_memory().unwrap());
        drop(wrapper);
        drop(guard);
        assert!(retained.upgrade().is_none());
        // SAFETY: the extracted connection still owns the live native main file. Its retained
        // descriptor shell must refuse writes once the original wrapper retires the guard.
        unsafe {
            let mut handle: *mut ffi::sqlite3_file = ptr::null_mut();
            assert_eq!(
                ffi::sqlite3_file_control(
                    extracted.handle(),
                    c"main".as_ptr(),
                    ffi::SQLITE_FCNTL_FILE_POINTER,
                    ptr::from_mut(&mut handle).cast()
                ),
                ffi::SQLITE_OK
            );
            assert_eq!(
                (*(*handle).pMethods).xWrite.unwrap()(handle, b"denied".as_ptr().cast(), 6, 0),
                ffi::SQLITE_IOERR_WRITE
            );
        }
        assert!(
            extracted
                .execute_batch("CREATE TABLE denied(value)")
                .is_err()
        );
        drop(extracted);
        assert!(retained.upgrade().is_none());
        assert_eq!(std::fs::metadata(path).unwrap().len(), 0);
    }

    #[test]
    fn cached_named_memory_vfs_fails_closed_after_original_scope_ends() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("clusters.db");
        let guard = Arc::new(SpaceGuard::new(directory.path(), |_| Ok(u64::MAX)).unwrap());
        let retained = Arc::downgrade(&guard);
        let wrapper = open(&path, Some(&guard)).unwrap();
        let name = wrapper.registration.as_ref().unwrap().name.clone();
        let cached_vfs = wrapper.registration.as_ref().unwrap().vfs.get();
        let memory = Connection::open_with_flags_and_vfs(
            ":memory:",
            OpenFlags::default(),
            name.to_str().unwrap(),
        )
        .unwrap();
        drop(wrapper);
        drop(guard);
        assert!(retained.upgrade().is_none());
        let late_path = directory.path().join("late.db");
        let error = memory
            .execute("ATTACH DATABASE ?1 AS late", [late_path.to_str().unwrap()])
            .unwrap_err();
        // ATTACH resolves the VFS name again before xOpen. Retirement removes that name,
        // so SQLite rejects the lookup before the retained descriptor can be called.
        assert!(
            matches!(error, rusqlite::Error::SqliteFailure(_, Some(ref message))
                if message == &format!("no such vfs: {}", name.to_str().unwrap())),
            "{error}"
        );
        let late_name = CString::new(late_path.to_str().unwrap()).unwrap();
        let mut storage = std::mem::MaybeUninit::<GuardedFile>::zeroed();
        let mut out_flags = 0;
        // SAFETY: the permanent descriptor shell keeps the cached VFS pointer valid after
        // retirement. Storage has the wrapper's size/alignment, and the name is terminated.
        // A retired xOpen must refuse before opening a native handle or retaining file state.
        unsafe {
            assert_eq!(
                (*cached_vfs).xOpen.unwrap()(
                    cached_vfs,
                    late_name.as_ptr(),
                    storage.as_mut_ptr().cast(),
                    ffi::SQLITE_OPEN_READWRITE | ffi::SQLITE_OPEN_CREATE | ffi::SQLITE_OPEN_MAIN_DB,
                    &mut out_flags,
                ),
                ffi::SQLITE_CANTOPEN
            );
            assert!((*storage.as_ptr()).header.pMethods.is_null());
        }
        assert!(!late_path.exists());
    }

    #[test]
    fn forgotten_wrapper_keeps_guard_owned_for_late_vfs_connection() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("clusters.db");
        let guard =
            Arc::new(SpaceGuard::new(directory.path(), |_| Ok(RETAINED_FREE_BYTES)).unwrap());
        let retained = Arc::downgrade(&guard);
        // Suppress normal destruction exactly as forgetting the wrapper does, but retain a
        // cleanup handle so this regression does not leak a database handle into other tests.
        let mut forgotten = ManuallyDrop::new(open(&path, Some(&guard)).unwrap());
        let name = forgotten.registration.as_ref().unwrap().name.clone();
        drop(guard);
        assert!(retained.upgrade().is_some());
        let late = Connection::open_with_flags_and_vfs(
            &path,
            OpenFlags::default(),
            name.to_str().unwrap(),
        )
        .unwrap();
        let error = late
            .execute_batch("CREATE TABLE denied(value)")
            .unwrap_err();
        assert!(
            matches!(error, rusqlite::Error::SqliteFailure(ref error, _) if error.code == ffi::ErrorCode::DiskFull),
            "{error}"
        );
        drop(late);
        // SAFETY: this is the one destruction of the ManuallyDrop value. It has not been moved
        // or read after destruction, and no connection using its registered name remains.
        unsafe { ManuallyDrop::drop(&mut forgotten) };
        assert!(retained.upgrade().is_none());
        assert_eq!(std::fs::metadata(path).unwrap().len(), 0);
    }

    #[test]
    fn shared_memory_growth_is_denied_before_native_mapping() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("clusters.db");
        let guard =
            Arc::new(SpaceGuard::new(directory.path(), |_| Ok(RETAINED_FREE_BYTES)).unwrap());
        let connection = open(&path, Some(&guard)).unwrap();
        // SAFETY: SQLite supplies the main file pointer; the wrapper and output pointer remain
        // live while its version-2 shared-memory method is invoked using its documented ABI.
        unsafe {
            let mut handle: *mut ffi::sqlite3_file = ptr::null_mut();
            assert_eq!(
                ffi::sqlite3_file_control(
                    connection.handle(),
                    c"main".as_ptr(),
                    ffi::SQLITE_FCNTL_FILE_POINTER,
                    ptr::from_mut(&mut handle).cast()
                ),
                ffi::SQLITE_OK
            );
            let map = (*(*handle).pMethods)
                .xShmMap
                .expect("native VFS supports WAL shared memory");
            let mut output = ptr::null_mut();
            assert_eq!(map(handle, 0, 32_768, 1, &mut output), ffi::SQLITE_FULL);
            assert!(output.is_null());
        }
        drop(connection);
        assert!(!path.with_extension("db-shm").exists());
        assert_eq!(std::fs::metadata(path).unwrap().len(), 0);
    }
}
