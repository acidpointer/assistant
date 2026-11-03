//! Verify Vulkan cache cleanup in an isolated process, without model weights.
//!
//! Each native test process owns its backend epoch. Buffer-type descriptors
//! must survive cache cleanup; live backends, buffers, events and pinned memory
//! must prevent it. Set `LLAMA_TEST_REQUIRE_VULKAN=1` to require GPU coverage.
#![cfg(all(feature = "vulkan", not(target_vendor = "apple")))]

use llama_cpp::llama_backend::LlamaBackend;
use llama_cpp_sys as sys;
use std::mem::size_of_val;

/// Query the initialized registry: direct Vulkan discovery can throw when no ICD
/// is usable, whereas registration catches that failure and omits the backend.
fn vulkan_available() -> bool {
    // SAFETY: callers initialize the backend epoch first. A non-null registry is
    // process-stable and has already completed successful Vulkan initialization.
    unsafe {
        let registry = sys::ggml_backend_reg_by_name(c"Vulkan".as_ptr());
        !registry.is_null() && sys::ggml_backend_reg_dev_count(registry) > 0
    }
}

/// Child-process coverage reaches the driver's real process-exit handlers.
#[test]
fn exit_after_gpu_graph() {
    const CHILD: &str = "LLAMA_VULKAN_EXIT_TEST_CHILD";
    if std::env::var_os(CHILD).is_none() {
        for _ in 0..3 {
            let output = std::process::Command::new(std::env::current_exe().expect("test binary"))
                .args(["--exact", "exit_after_gpu_graph", "--nocapture"])
                .env(CHILD, "1")
                .output()
                .expect("run Vulkan exit child");
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                output.status.success(),
                "Vulkan exit child failed: {stderr}"
            );
            assert!(
                !stderr.contains("NVVM compilation failed"),
                "driver compiler failed during process exit: {stderr}"
            );
        }
        return;
    }
    let _backend = LlamaBackend::init().expect("child backend epoch");
    // SAFETY: this process owns every native handle below. The graph and tensors
    // remain alive through compute/readback; buffers precede backend destruction.
    unsafe {
        if !vulkan_available() {
            assert!(
                std::env::var_os("LLAMA_TEST_REQUIRE_VULKAN").is_none(),
                "Vulkan exit coverage was required but no device was found"
            );
            eprintln!("SKIP: Vulkan exit child needs a supported device");
            return;
        }
        let backend = sys::ggml_backend_vk_init(0);
        assert!(!backend.is_null(), "child Vulkan backend");
        let context = sys::ggml_init(sys::ggml_init_params {
            mem_size: 65_536,
            mem_buffer: std::ptr::null_mut(),
            no_alloc: true,
        });
        assert!(!context.is_null(), "child graph metadata");
        let a = sys::ggml_new_tensor_1d(context, sys::GGML_TYPE_F32, 32);
        let b = sys::ggml_new_tensor_1d(context, sys::GGML_TYPE_F32, 32);
        assert!(!a.is_null() && !b.is_null(), "child input tensors");
        let sum = sys::ggml_add(context, a, b);
        let graph = sys::ggml_new_graph_custom(context, 32, false);
        assert!(!sum.is_null() && !graph.is_null(), "child compute graph");
        sys::ggml_build_forward_expand(graph, sum);
        let buffer = sys::ggml_backend_alloc_ctx_tensors(context, backend);
        assert!(!buffer.is_null(), "child graph storage");
        let input = [1.0_f32; 32];
        sys::ggml_backend_tensor_set(a, input.as_ptr().cast(), 0, size_of_val(&input));
        sys::ggml_backend_tensor_set(b, input.as_ptr().cast(), 0, size_of_val(&input));
        assert_eq!(
            sys::ggml_backend_graph_compute(backend, graph),
            sys::GGML_STATUS_SUCCESS,
            "child Vulkan graph executes"
        );
        let mut output = [0.0_f32; 32];
        sys::ggml_backend_tensor_get(sum, output.as_mut_ptr().cast(), 0, size_of_val(&output));
        assert_eq!(output, [2.0; 32], "child graph result");
        sys::ggml_backend_buffer_free(buffer);
        sys::ggml_backend_free(backend);
        sys::ggml_free(context);
        // Do not call explicit Vulkan cleanup: the normal owner-release path
        // and exit hook must retire device/compiler resources on their own.
    }
}

#[test]
fn cleanup_refuses_live_owners_and_preserves_buffer_types() {
    let _backend = LlamaBackend::init().expect("initialize registered backends");
    // SAFETY: this isolated test owns the backend epoch. Handles are used only
    // before release; cleanup must refuse objects that still retain a device.
    unsafe {
        if !vulkan_available() {
            assert!(
                std::env::var_os("LLAMA_TEST_REQUIRE_VULKAN").is_none(),
                "Vulkan coverage was required but no device was found"
            );
            eprintln!("SKIP: Vulkan lifecycle test needs a supported device");
            return;
        }
        assert!(sys::ggml_backend_vk_cleanup(), "empty cache is releasable");
        assert!(sys::ggml_backend_vk_cleanup(), "cleanup is idempotent");

        let buft = sys::ggml_backend_vk_buffer_type(0);
        assert!(!buft.is_null(), "device has a stable buffer type");
        let device = sys::ggml_backend_buft_get_device(buft);
        assert!(!device.is_null(), "buffer type has a registered device");

        for _ in 0..3 {
            let backend = sys::ggml_backend_dev_init(device, std::ptr::null());
            assert!(!backend.is_null(), "create a Vulkan backend");
            assert!(!sys::ggml_backend_vk_cleanup(), "live backend pins device");
            sys::ggml_backend_free(backend);

            // ggml short-circuits zero-sized allocations into a generic dummy.
            // It holds only the stable descriptor and must survive retirement.
            let empty = sys::ggml_backend_buft_alloc_buffer(buft, 0);
            assert!(!empty.is_null(), "allocate empty buffer");
            assert!(sys::ggml_backend_vk_cleanup(), "dummy has no GPU resources");
            assert_eq!(sys::ggml_backend_buffer_get_type(empty), buft);
            sys::ggml_backend_buffer_free(empty);

            let buffer = sys::ggml_backend_buft_alloc_buffer(buft, 4096);
            assert!(!buffer.is_null(), "allocate device buffer");
            assert!(!sys::ggml_backend_vk_cleanup(), "live buffer pins device");
            sys::ggml_backend_buffer_clear(buffer, 0x5a);
            let context = sys::ggml_init(sys::ggml_init_params {
                mem_size: 4096,
                mem_buffer: std::ptr::null_mut(),
                no_alloc: true,
            });
            assert!(!context.is_null(), "allocate tensor metadata");
            let tensor = sys::ggml_new_tensor_1d(context, sys::GGML_TYPE_F32, 16);
            assert!(!tensor.is_null(), "create tensor metadata");
            assert_eq!(
                sys::ggml_backend_tensor_alloc(
                    buffer,
                    tensor,
                    sys::ggml_backend_buffer_get_base(buffer)
                ),
                sys::GGML_STATUS_SUCCESS
            );
            // With host-visible VRAM disabled this creates the device-owned
            // staging buffer. Cleanup must break its internal ownership cycle.
            let input = [1.0_f32; 16];
            let mut output = [0.0_f32; 16];
            sys::ggml_backend_tensor_set(tensor, input.as_ptr().cast(), 0, size_of_val(&input));
            sys::ggml_backend_tensor_get(
                tensor,
                output.as_mut_ptr().cast(),
                0,
                size_of_val(&output),
            );
            assert_eq!(input, output, "tensor transfers survive rejected cleanup");
            sys::ggml_backend_buffer_free(buffer);
            sys::ggml_free(context);

            let event = sys::ggml_backend_event_new(device);
            assert!(!event.is_null(), "allocate a Vulkan event");
            assert!(!sys::ggml_backend_vk_cleanup(), "live event pins device");
            sys::ggml_backend_event_free(event);

            let host_type = sys::ggml_backend_vk_host_buffer_type();
            let host_buffer = sys::ggml_backend_buft_alloc_buffer(host_type, 4096);
            assert!(!host_buffer.is_null(), "allocate pinned host memory");
            assert!(
                !sys::ggml_backend_vk_cleanup(),
                "live pinned allocation prevents device cleanup"
            );
            sys::ggml_backend_buffer_free(host_buffer);

            assert!(
                sys::ggml_backend_vk_cleanup(),
                "all external owners released"
            );
            assert_eq!(
                sys::ggml_backend_vk_buffer_type(0),
                buft,
                "cleanup must preserve previously borrowed descriptors"
            );
            assert!(sys::ggml_backend_buft_get_alignment(buft) > 0);
            assert!(
                sys::ggml_backend_vk_cleanup(),
                "recreated cache is releasable"
            );
        }

        // Acquiring/releasing a native owner must serialize with cache cleanup.
        // Each worker creates and frees its own handle; no pointer crosses threads.
        for _ in 0..4 {
            let (ready_tx, ready_rx) = std::sync::mpsc::channel();
            let (release_tx, release_rx) = std::sync::mpsc::channel();
            let worker = std::thread::spawn(move || {
                let backend = sys::ggml_backend_vk_init(0);
                assert!(!backend.is_null(), "concurrent native backend");
                ready_tx.send(()).expect("notify owner is live");
                release_rx.recv().expect("wait for release");
                sys::ggml_backend_free(backend);
            });
            ready_rx.recv().expect("worker owns device");
            assert!(!sys::ggml_backend_vk_cleanup(), "concurrent owner is live");
            release_tx.send(()).expect("allow release");
            for _ in 0..32 {
                sys::ggml_backend_vk_cleanup();
                std::thread::yield_now();
            }
            worker.join().expect("join native owner");
            assert!(
                sys::ggml_backend_vk_cleanup(),
                "concurrent release completed"
            );
        }
    }
}
