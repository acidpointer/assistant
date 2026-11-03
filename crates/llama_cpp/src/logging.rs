//! Process-wide management of llama.cpp's native log callback.
//!
//! llama.cpp writes its own logs through a process-global C callback installed
//! with `llama_log_set`. This module provides the smallest safe Rust wrapper
//! around that hook: a caller supplies a Rust closure, and that closure becomes
//! the active native log callback.
//!
//! This is deliberately **not** a logging framework. It does not filter,
//! format, buffer, or forward records anywhere. In particular, it has no
//! dependency on `tracing`; bridging native records into `tracing` is the
//! application's job. The binding only owns the FFI mechanics that cannot be
//! expressed safely by a caller:
//!
//! - retaining the callback while C++ code may invoke it;
//! - converting the native `const char *` into a borrowed `&str`;
//! - containing panics so they never unwind across the C ABI; and
//! - suppressing recursive emission from inside the callback.
//!
//! # Persistence
//!
//! Registration is process-wide and persistent. Calling
//! [`set_log_callback`] again replaces the active callback. This module does
//! not keep a stack of registrations and does not restore the callback that was
//! active before the first Rust registration. That behaviour is deliberate:
//! the application owns the process-wide logging choice, and replacing or
//! silencing a callback is an explicit call.
//!
//! The callback is stored in global state for the lifetime of the process, or
//! until another call replaces it.
//!
//! # Coverage
//!
//! The callback receives llama and ggml core logs. Two other native log paths
//! are separate and are **not** captured here:
//!
//! - the `common/` logger used by chat, sampling, and speculative helpers
//!   (configured through [`crate::runtime::log`]); and
//! - the clip/mtmd logger (configured through `MtmdContext::void_logs` under
//!   the `mtmd` feature).
//!
//! `llama_log_set` routes both the llama and ggml logger states, so a single
//! registration covers those core logs.
//!
//! # Threading
//!
//! The callback may be invoked from any native thread. The supplied closure
//! must therefore be `Send + Sync`. Records from different threads have no
//! guaranteed ordering relative to one another. The callback should be cheap;
//! a slow sink blocks the emitting native thread.
//!
//! The mandatory native patch synchronizes logger callback/user-data pairs.
//! Native emission snapshots the pair and releases the lock before invoking
//! the callback. First installation and Rust replacements are safe during
//! inference. Fit helpers use scoped thread-local overrides and never publish
//! stack-local user data process-wide.
//!
//! # Panics and recursion
//!
//! A panic inside the callback is caught and reported to stderr; it does not
//! cross the FFI boundary. Rust's panic hook still runs before the panic is
//! caught, so the panic message may appear as well. If the callback causes
//! another llama.cpp log record to be emitted on the same thread, the nested
//! record is dropped to prevent unbounded recursion.
//!
//! # Example
//!
//! ```no_run
//! use llama_cpp::{logging, LogLevel};
//!
//! logging::set_log_callback(|level, message| {
//!     let tag = match level {
//!         LogLevel::Error => "error",
//!         LogLevel::Warn => "warn",
//!         LogLevel::Info | LogLevel::None => "info",
//!         LogLevel::Debug => "debug",
//!         LogLevel::Continue => "cont",
//!         LogLevel::Unknown(raw) => return eprintln!("unknown [{raw}]: {message}"),
//!     };
//!     eprintln!("[{tag}] {message}");
//! });
//! ```
//!
//! # Relationship to the raw API
//!
//! [`crate::log_set`] and [`crate::log_get`] remain available as unsafe escape
//! hatches. Calling `log_set` directly replaces the dispatcher installed here
//! and leaves this module's state stale; do not mix the two APIs.

use std::borrow::Cow;
use std::cell::Cell;
use std::ffi::{c_void, CStr};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Arc, OnceLock, RwLock, RwLockReadGuard, RwLockWriteGuard};

use llama_cpp_sys as sys;

/// Severity of a native llama.cpp/ggml log record.
///
/// Values map directly to `ggml_log_level`. Unknown values are preserved
/// instead of being silently treated as a known severity, so a newer llama.cpp
/// cannot lose information through this layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LogLevel {
    /// Unlevelled output, often regular program output rather than a warning.
    None,
    /// Debug diagnostics.
    Debug,
    /// Informational messages.
    Info,
    /// Warnings.
    Warn,
    /// Errors.
    Error,
    /// Continuation of the preceding record.
    ///
    /// This is not a severity. Consumers that filter by severity should apply
    /// the decision of the preceding non-continuation record.
    Continue,
    /// A level added by a newer llama.cpp that this crate does not know yet.
    Unknown(u32),
}

impl LogLevel {
    /// Convert the native level without panicking on unrecognised values.
    fn from_raw(raw: sys::ggml_log_level) -> Self {
        match raw {
            sys::GGML_LOG_LEVEL_NONE => Self::None,
            sys::GGML_LOG_LEVEL_DEBUG => Self::Debug,
            sys::GGML_LOG_LEVEL_INFO => Self::Info,
            sys::GGML_LOG_LEVEL_WARN => Self::Warn,
            sys::GGML_LOG_LEVEL_ERROR => Self::Error,
            sys::GGML_LOG_LEVEL_CONT => Self::Continue,
            other => Self::Unknown(other),
        }
    }
}

/// Callback invoked for one native llama.cpp/ggml log record.
///
/// `message` borrows the native buffer and is valid only for the duration of
/// the call. Copy it if it must outlive the callback. A trailing newline, if
/// present, is preserved.
type Callback = dyn for<'a> Fn(LogLevel, &'a str) + Send + Sync + 'static;

/// Process-wide logger state.
#[derive(Default)]
struct State {
    /// The active callback, or `None` before the first registration.
    callback: Option<Arc<Callback>>,
    /// Whether the static dispatcher has been installed natively.
    installed: bool,
}

/// There is exactly one native logger, so there is exactly one state object.
///
/// `OnceLock` avoids initialization races; the `RwLock` lets the dispatcher
/// read the callback concurrently while a later registration replaces it.
fn state() -> &'static RwLock<State> {
    static STATE: OnceLock<RwLock<State>> = OnceLock::new();
    STATE.get_or_init(|| RwLock::new(State::default()))
}

/// Acquire the state for reading, recovering from a poisoned lock.
///
/// The callback is never invoked under this lock, and state replacement is
/// infallible, so recovering the inner value is safer than turning a poisoned
/// lock into a process-wide logging failure.
fn read_state() -> RwLockReadGuard<'static, State> {
    match state().read() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// Acquire the state for writing, recovering from a poisoned lock.
fn write_state() -> RwLockWriteGuard<'static, State> {
    match state().write() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// Install or replace the process-wide native log callback.
///
/// The callback remains active until the next call replaces it. This function
/// intentionally does not restore the callback that was active before the
/// first Rust registration. To silence native logs, install a no-op callback:
///
/// ```no_run
/// llama_cpp::logging::set_log_callback(|_, _| {});
/// ```
///
/// Installation is safe while native threads emit logs. The mandatory native
/// patch snapshots callback pairs under a mutex, invoking them after unlocking.
/// The installed C trampoline and its null user data have static lifetimes.
/// Later Rust replacements retain in-flight callbacks through an `Arc`.
///
/// # Callback contract
///
/// - The message is borrowed and must be copied if it needs to outlive the call.
/// - The callback must not panic. Panics are contained and reported, but the
///   callback's own state may be left inconsistent.
/// - The callback must not block for long; it runs on the native emitting
///   thread.
/// - Nested logging from inside the callback is dropped on the same thread.
///
/// # Example
///
/// ```no_run
/// llama_cpp::logging::set_log_callback(|level, message| {
///     eprintln!("{level:?}: {message}");
/// });
/// ```
pub fn set_log_callback<F>(callback: F)
where
    F: for<'a> Fn(LogLevel, &'a str) + Send + Sync + 'static,
{
    let callback: Arc<Callback> = Arc::new(callback);
    let previous = {
        let mut state = write_state();
        let previous = state.callback.replace(callback);
        if !state.installed {
            // SAFETY: the mandatory patched logger synchronizes pair updates
            // with native emission; dispatch and null user data are static.
            unsafe {
                let _ = sys::llama_cpp_rs_runtime_safety_abi_v1();
                sys::llama_log_set(Some(dispatch), std::ptr::null_mut());
            }
            state.installed = true;
        }
        previous
    };
    // Closure captures may run arbitrary Drop code, including logging or
    // replacing the callback. Never drop them while holding the state lock.
    drop(previous);
}

// Set while a callback is running on this thread, to suppress recursion.
thread_local! {
    static IN_DISPATCH: Cell<bool> = const { Cell::new(false) };
}

/// Clears the thread-local reentrancy flag on every exit path, including panic
/// unwinding before the dispatcher's own `catch_unwind` boundary.
struct ReentryGuard;

impl ReentryGuard {
    /// Enter the callback unless this thread is already dispatching.
    fn enter() -> Option<Self> {
        IN_DISPATCH.with(|flag| if flag.replace(true) { None } else { Some(Self) })
    }
}

impl Drop for ReentryGuard {
    fn drop(&mut self) {
        IN_DISPATCH.with(|flag| flag.set(false));
    }
}

/// Static C trampoline installed with `llama_log_set`.
///
/// # Safety
///
/// llama.cpp must pass a valid, NUL-terminated `text` pointer for the duration
/// of the call, as required by `ggml_log_callback`. The pointer may be null in
/// hostile or buggy callers; this function handles that case explicitly.
unsafe extern "C" fn dispatch(
    level: sys::ggml_log_level,
    text: *const std::ffi::c_char,
    _user_data: *mut c_void,
) {
    // Contain the whole dispatch, including capture destruction after a
    // concurrent replacement. A panic payload can itself panic on Drop, so it
    // must not be destroyed on this extern-C stack.
    let result = catch_unwind(AssertUnwindSafe(|| {
        let Some(_reentry) = ReentryGuard::enter() else {
            return;
        };
        // SAFETY: the native callback supplies a live NUL-terminated buffer.
        unsafe { dispatch_record(level, text) };
    }));
    if let Err(payload) = result {
        std::mem::forget(payload);
    }
}

/// Dispatch one record within the outer panic and reentrancy boundary.
///
/// # Safety
/// Non-null text must be NUL-terminated and valid throughout this call.
unsafe fn dispatch_record(level: sys::ggml_log_level, text: *const std::ffi::c_char) {
    let level = LogLevel::from_raw(level);

    // Keep a borrowed view of the native buffer where possible. Invalid UTF-8
    // is converted lossily instead of failing or panicking: a log record is
    // diagnostic data, and losing a malformed byte is better than losing the
    // whole record or unwinding into C++.
    let message: Cow<'_, str> = if text.is_null() {
        Cow::Borrowed("")
    } else {
        // SAFETY: the callback contract guarantees a valid NUL-terminated
        // string valid for this call. The null case was handled above.
        let c_text = unsafe { CStr::from_ptr(text) };
        match c_text.to_str() {
            Ok(text) => Cow::Borrowed(text),
            Err(_) => c_text.to_string_lossy(),
        }
    };

    let callback = {
        let state = read_state();
        state.callback.clone()
    };

    let Some(callback) = callback else {
        return;
    };

    let result = catch_unwind(AssertUnwindSafe(|| callback(level, message.as_ref())));
    if let Err(payload) = result {
        std::mem::forget(payload);
        write_fallback(level, message.as_ref());
    }
}

/// Best-effort stderr output when the user callback panics.
///
/// There is no caller to return an error to from inside a C callback, and
/// propagating the panic would be undefined behaviour at the FFI boundary.
/// Failure to write is therefore intentionally ignored; the original panic has
/// already been contained.
fn write_fallback(level: LogLevel, message: &str) {
    use std::io::Write;

    let mut stderr = std::io::stderr().lock();
    let _ = writeln!(
        stderr,
        "[llama_cpp {level:?}] {}",
        message.trim_end_matches('\n')
    );
    let _ = stderr.flush();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    /// Logging state is process-global, so tests using it must not overlap.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn test_lock() -> std::sync::MutexGuard<'static, ()> {
        match TEST_LOCK.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    fn emit(level: sys::ggml_log_level, text: &str) {
        let text = CString::new(text).expect("test log text must not contain a NUL");
        // SAFETY: the CString outlives the call, and the dispatcher only reads
        // the text for the duration of the call.
        unsafe {
            sys::ggml_log_dispatch(level, text.as_ptr());
        }
    }

    #[test]
    fn callback_receives_level_and_text() {
        let _lock = test_lock();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen_by_callback = Arc::clone(&seen);

        set_log_callback(move |level, message| {
            if let Ok(mut seen) = seen_by_callback.lock() {
                seen.push((level, message.to_owned()));
            }
        });

        emit(sys::GGML_LOG_LEVEL_INFO, "hello\n");

        let seen = seen.lock().expect("callback must not poison the test lock");
        assert_eq!(seen.as_slice(), &[(LogLevel::Info, "hello\n".to_owned())]);
    }

    #[test]
    fn setting_a_callback_replaces_the_previous_one() {
        let _lock = test_lock();
        let order = Arc::new(Mutex::new(Vec::new()));

        let first_order = Arc::clone(&order);
        set_log_callback(move |_, message| {
            if let Ok(mut order) = first_order.lock() {
                order.push(format!("first:{message}"));
            }
        });

        let second_order = Arc::clone(&order);
        set_log_callback(move |_, message| {
            if let Ok(mut order) = second_order.lock() {
                order.push(format!("second:{message}"));
            }
        });

        emit(sys::GGML_LOG_LEVEL_INFO, "a\n");

        let order = order
            .lock()
            .expect("callback must not poison the test lock");
        assert_eq!(order.as_slice(), &["second:a\n"]);
    }

    #[test]
    fn panic_in_callback_is_contained() {
        let _lock = test_lock();
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_by_callback = Arc::clone(&calls);

        set_log_callback(move |_, _| {
            calls_by_callback.fetch_add(1, Ordering::SeqCst);
            panic!("intentional test panic");
        });

        emit(sys::GGML_LOG_LEVEL_ERROR, "boom\n");
        emit(sys::GGML_LOG_LEVEL_ERROR, "still alive\n");

        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn invalid_utf8_is_converted_lossily() {
        let _lock = test_lock();
        let seen = Arc::new(Mutex::new(String::new()));
        let seen_by_callback = Arc::clone(&seen);

        set_log_callback(move |_, message| {
            if let Ok(mut seen) = seen_by_callback.lock() {
                *seen = message.to_owned();
            }
        });

        let text = CString::new(vec![0xFF, b'x']).expect("test bytes must not contain a NUL");
        // SAFETY: the CString outlives the call.
        unsafe {
            dispatch(
                sys::GGML_LOG_LEVEL_DEBUG,
                text.as_ptr(),
                std::ptr::null_mut(),
            );
        }

        let seen = seen.lock().expect("callback must not poison the test lock");
        assert_eq!(seen.as_str(), "�x");
    }

    #[test]
    fn null_text_is_an_empty_message() {
        let _lock = test_lock();
        let seen = Arc::new(Mutex::new(String::from("not called")));
        let seen_by_callback = Arc::clone(&seen);

        set_log_callback(move |_, message| {
            if let Ok(mut seen) = seen_by_callback.lock() {
                *seen = message.to_owned();
            }
        });

        // SAFETY: the dispatcher explicitly handles a null text pointer.
        unsafe {
            dispatch(
                sys::GGML_LOG_LEVEL_NONE,
                std::ptr::null(),
                std::ptr::null_mut(),
            );
        }

        let seen = seen.lock().expect("callback must not poison the test lock");
        assert!(seen.is_empty());
    }

    #[test]
    fn nested_logging_is_dropped() {
        let _lock = test_lock();
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_by_callback = Arc::clone(&calls);

        set_log_callback(move |_, _| {
            let first = calls_by_callback.fetch_add(1, Ordering::SeqCst) == 0;
            if first {
                emit(sys::GGML_LOG_LEVEL_INFO, "nested\n");
            }
        });

        emit(sys::GGML_LOG_LEVEL_INFO, "outer\n");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn unknown_levels_are_preserved() {
        let _lock = test_lock();
        let seen = Arc::new(Mutex::new(None));
        let seen_by_callback = Arc::clone(&seen);

        set_log_callback(move |level, _| {
            if let Ok(mut seen) = seen_by_callback.lock() {
                *seen = Some(level);
            }
        });

        emit(999, "future\n");

        let seen = seen.lock().expect("callback must not poison the test lock");
        assert_eq!(*seen, Some(LogLevel::Unknown(999)));
    }

    #[test]
    fn native_logs_reach_callback() {
        let _lock = test_lock();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen_by_callback = Arc::clone(&seen);

        set_log_callback(move |level, message| {
            if let Ok(mut seen) = seen_by_callback.lock() {
                seen.push((level, message.to_owned()));
            }
        });

        // Force a deterministic llama.cpp error log without loading a model.
        // The backend is initialised because model loading expects the native
        // backend registry to be available before it can report the file error.
        let backend = crate::LlamaBackend::init().expect("backend should initialise once");
        let path = CString::new("/nonexistent/llama_cpp_logging_test.gguf")
            .expect("test path must not contain a NUL");

        // SAFETY: `path` is a valid NUL-terminated string and the default model
        // parameters are a complete native value. A missing file makes the
        // native call return null after emitting an error log.
        let model = unsafe {
            let params = sys::llama_model_default_params();
            sys::llama_model_load_from_file(path.as_ptr(), params)
        };
        assert!(model.is_null(), "bogus model path unexpectedly loaded");

        let seen = seen.lock().expect("callback must not poison the test lock");
        assert!(
            !seen.is_empty(),
            "native llama.cpp log did not reach the Rust callback"
        );

        drop(seen);
        drop(backend);
    }
    #[test]
    fn replacement_drops_captures_outside_the_state_lock() {
        struct ReentrantDrop(Arc<AtomicUsize>);
        impl Drop for ReentrantDrop {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::Relaxed);
                emit(sys::GGML_LOG_LEVEL_INFO, "capture drop\n");
                set_log_callback(|_, _| {});
            }
        }
        let _lock = test_lock();
        let drops = Arc::new(AtomicUsize::new(0));
        let captured = ReentrantDrop(Arc::clone(&drops));
        set_log_callback(move |_, _| {
            let _keep_capture = &captured;
        });
        set_log_callback(|_, _| {});
        assert_eq!(drops.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn in_flight_capture_destructor_cannot_unwind_through_native_code() {
        struct PanicOnDrop;
        impl Drop for PanicOnDrop {
            fn drop(&mut self) {
                panic!("capture destructor failure");
            }
        }
        let _lock = test_lock();
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let callback_barrier = Arc::clone(&barrier);
        let capture = PanicOnDrop;
        set_log_callback(move |_, _| {
            let _keep_capture = &capture;
            callback_barrier.wait();
            callback_barrier.wait();
        });
        let worker = std::thread::spawn(|| emit(sys::GGML_LOG_LEVEL_INFO, "in-flight\n"));
        barrier.wait();
        set_log_callback(|_, _| {});
        barrier.wait();
        worker
            .join()
            .expect("native dispatcher contains capture Drop panic");
        emit(sys::GGML_LOG_LEVEL_INFO, "still usable\n");
    }
}
