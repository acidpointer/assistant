//! Stress the patched native logger and thread-local fit overrides.
//!
//! This binary has one test so its initial callback installation really races
//! with native emission. Raw thread-local user data remains valid on its owning
//! thread until its override is cleared; no pointer is shared with emitters.

use std::ffi::{c_void, CString};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};

use llama_cpp::logging::set_log_callback;

fn emit(message: &str) {
    let message = CString::new(message).expect("test log without NUL");
    // SAFETY: the native dispatcher reads the CString synchronously and the
    // managed callback is static with null native user data.
    unsafe {
        llama_cpp_sys::ggml_log_dispatch(llama_cpp_sys::GGML_LOG_LEVEL_INFO, message.as_ptr());
    };
}

unsafe extern "C" fn count_thread_record(
    _: llama_cpp_sys::ggml_log_level,
    _: *const std::ffi::c_char,
    data: *mut c_void,
) {
    // SAFETY: this thread installed a pointer to its live AtomicUsize and clears
    // the override before that value goes out of scope. No other thread sees it.
    let counter = unsafe { &*data.cast::<AtomicUsize>() };
    counter.fetch_add(1, Ordering::Relaxed);
}

#[test]
fn native_install_replacement_and_local_overrides_are_concurrent_safe() {
    let seen = Arc::new(AtomicUsize::new(0));
    let barrier = Arc::new(Barrier::new(5));
    std::thread::scope(|scope| {
        for _ in 0..4 {
            let barrier = Arc::clone(&barrier);
            scope.spawn(move || {
                barrier.wait();
                for _ in 0..128 {
                    emit("install-race\n");
                }
            });
        }
        barrier.wait();
        let count = Arc::clone(&seen);
        set_log_callback(move |_, _| {
            count.fetch_add(1, Ordering::Relaxed);
        });
    });
    seen.store(0, Ordering::Relaxed);
    std::thread::scope(|scope| {
        for _ in 0..4 {
            scope.spawn(|| {
                for _ in 0..512 {
                    emit("replacement-race\n");
                }
            });
        }
        for _ in 0..512 {
            let count = Arc::clone(&seen);
            set_log_callback(move |_, _| {
                count.fetch_add(1, Ordering::Relaxed);
            });
        }
    });
    assert_eq!(
        seen.load(Ordering::Relaxed),
        4 * 512,
        "every native record reaches one callback"
    );

    let local = AtomicUsize::new(0);
    // SAFETY: this stack-local pointer is visible only to this thread. It is
    // removed synchronously before local is dropped, including after workers join.
    unsafe {
        llama_cpp_sys::ggml_log_set_thread(
            Some(count_thread_record),
            std::ptr::from_ref(&local).cast_mut().cast(),
        );
    };
    emit("local\n");
    std::thread::scope(|scope| {
        scope.spawn(|| emit("global\n"));
    });
    // SAFETY: remove this thread's override before its borrowed counter expires.
    unsafe { llama_cpp_sys::ggml_log_set_thread(None, std::ptr::null_mut()) };
    assert_eq!(
        local.load(Ordering::Relaxed),
        1,
        "thread-local data never escapes to a worker"
    );
    assert_eq!(seen.load(Ordering::Relaxed), 4 * 512 + 1);
    set_log_callback(|_, _| {});
}
