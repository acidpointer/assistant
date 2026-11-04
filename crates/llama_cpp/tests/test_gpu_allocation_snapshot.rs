//! Guarded allocation inspection must reject invalid ownership without native reads.
#[test]
fn invalid_gpu_allocation_query_fails_without_publishing_a_count() {
    let mut required = 37_usize;
    // SAFETY: null model/context are intentional invalid arguments. The guarded
    // entry point validates them before dereference; required is writable storage.
    let status = unsafe {
        llama_cpp_sys::llama_gpu_allocations_collect_v1(
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null_mut(),
            0,
            &raw mut required,
        )
    };
    assert_ne!(status, 0);
    assert_eq!(
        required, 37,
        "failed inspection cannot claim CPU-only placement"
    );
}

#[test]
fn gpu_snapshots_own_their_data_and_cross_threads_without_device_handles() {
    fn send_sync<T: Send + Sync>() {}
    send_sync::<llama_cpp::context::memory_breakdown::GpuAllocation>();
}
