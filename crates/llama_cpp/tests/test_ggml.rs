//! Tests for the ggml graph computation API.
// The crate doc must precede `#![cfg]`: when the feature is off the cfg strips
// the whole crate body, and a doc comment placed after it goes with it —
// leaving an undocumented crate that trips `missing_docs`.
#![cfg(feature = "ggml")]
// Results are exactly-representable values (integers and halves) produced by
// exact f32 operations — add, scale by 2.5, and a view. An epsilon comparison
// here would weaken the tests without making them more correct.
#![allow(clippy::float_cmp)]

use llama_cpp::ggml::*;

#[test]
fn test_context_creation() {
    let ctx = GgmlContext::new(1024 * 1024, true).expect("valid ggml operation");
    assert!(!ctx.as_ptr().is_null());
}

#[test]
fn test_tensor_creation_1d() {
    let ctx = GgmlContext::new(1024 * 1024, true).expect("valid ggml operation");
    let t = ctx
        .new_tensor_1d(llama_cpp_sys::GGML_TYPE_F32, 8)
        .expect("valid ggml operation");
    assert_eq!(t.nelements(), 8);
    assert_eq!(t.ne(), [8, 1, 1, 1]);
}

#[test]
fn test_tensor_creation_2d() {
    let ctx = GgmlContext::new(1024 * 1024, true).expect("valid ggml operation");
    let t = ctx
        .new_tensor_2d(llama_cpp_sys::GGML_TYPE_F32, 4, 3)
        .expect("valid ggml operation");
    assert_eq!(t.nelements(), 12);
    assert_eq!(t.ne()[0], 4);
    assert_eq!(t.ne()[1], 3);
}

#[test]
fn test_tensor_name() {
    let ctx = GgmlContext::new(1024 * 1024, true).expect("valid ggml operation");
    let t = ctx
        .new_tensor_1d(llama_cpp_sys::GGML_TYPE_F32, 4)
        .expect("valid ggml operation");
    t.set_name("my_tensor").expect("valid ggml operation");
    assert_eq!(t.name(), "my_tensor");
}

#[test]
fn test_dup_tensor() {
    let ctx = GgmlContext::new(1024 * 1024, true).expect("valid ggml operation");
    let t = ctx
        .new_tensor_2d(llama_cpp_sys::GGML_TYPE_F32, 4, 3)
        .expect("valid ggml operation");
    let dup = ctx.dup_tensor(&t).expect("valid ggml operation");
    assert_eq!(dup.ne(), t.ne());
    assert_eq!(dup.typ(), t.typ());
}

#[test]
fn test_type_name() {
    let name = type_name(llama_cpp_sys::GGML_TYPE_F32).expect("valid native type");
    assert_eq!(name, "f32");
    let name = type_name(llama_cpp_sys::GGML_TYPE_F16).expect("valid native type");
    assert_eq!(name, "f16");
}

#[test]
fn test_is_quantized() {
    assert!(!is_quantized(llama_cpp_sys::GGML_TYPE_F32).expect("valid native type"));
    assert!(!is_quantized(llama_cpp_sys::GGML_TYPE_F16).expect("valid native type"));
    assert!(is_quantized(llama_cpp_sys::GGML_TYPE_Q4_0).expect("valid native type"));
    assert!(is_quantized(llama_cpp_sys::GGML_TYPE_Q8_0).expect("valid native type"));
}

#[test]
fn test_tensor_overhead() {
    let overhead = tensor_overhead();
    assert!(overhead > 0);
}

#[test]
fn test_graph_overhead() {
    let overhead = graph_overhead();
    assert!(overhead > 0);
}

#[test]
fn test_backend_cpu() {
    let backend = GgmlBackend::cpu().expect("valid ggml operation");
    assert!(!backend.as_ptr().is_null());
}

#[test]
fn test_allocr_creation() {
    let backend = GgmlBackend::cpu().expect("valid ggml operation");
    let _alloc = GgmlAllocr::new(&backend).expect("valid ggml operation");
}

#[test]
fn test_graph_add_two_vectors() {
    // Create context for graph building (no_alloc = true)
    let n = 4_i64;
    let n_tensors = 3; // a, b, sum
    let mem_size = tensor_overhead() * n_tensors + graph_overhead() + 1024;
    let ctx = GgmlContext::new(mem_size, true).expect("valid ggml operation");

    // Create input tensors
    let a = ctx
        .new_tensor_1d(llama_cpp_sys::GGML_TYPE_F32, n)
        .expect("valid ggml operation");
    a.set_name("a").expect("valid ggml operation");
    let b = ctx
        .new_tensor_1d(llama_cpp_sys::GGML_TYPE_F32, n)
        .expect("valid ggml operation");
    b.set_name("b").expect("valid ggml operation");

    // Build graph: sum = a + b
    let sum = ctx.add(&a, &b).expect("valid ggml operation");
    let mut graph = ctx.new_graph().expect("valid ggml operation");
    graph.build_forward(&sum).expect("valid ggml operation");

    // Allocate with backend
    let backend = GgmlBackend::cpu().expect("valid ggml operation");
    let mut alloc = GgmlAllocr::new(&backend).expect("valid ggml operation");
    // SAFETY: no execution is in flight; allocator and backend remain live
    // through all subsequent tensor-data access and graph computation.
    assert!(unsafe { alloc.alloc_graph(&mut graph) });

    // Set input data
    let a_data: [f32; 4] = [1.0, 2.0, 3.0, 4.0];
    let b_data: [f32; 4] = [10.0, 20.0, 30.0, 40.0];
    unsafe {
        tensor_set(&a, bytemuck_cast(&a_data));
        tensor_set(&b, bytemuck_cast(&b_data));
    }

    // Compute
    // SAFETY: all graph buffers remain owned by alloc; initialized inputs and
    // synchronous execution precede allocator/backend/context destruction.
    unsafe { backend.graph_compute(&mut graph) }.expect("compute graph");

    // Read result
    let result_tensor = graph.node(-1).expect("valid ggml operation");
    let mut result_data = [0.0_f32; 4];
    unsafe {
        tensor_get(&result_tensor, bytemuck_cast_mut(&mut result_data));
    }

    assert_eq!(result_data, [11.0, 22.0, 33.0, 44.0]);
}

#[test]
fn test_graph_scale() {
    let n = 3_i64;
    let mem_size = tensor_overhead() * 2 + graph_overhead() + 1024;
    let ctx = GgmlContext::new(mem_size, true).expect("valid ggml operation");

    let a = ctx
        .new_tensor_1d(llama_cpp_sys::GGML_TYPE_F32, n)
        .expect("valid ggml operation");
    let scaled = ctx.scale(&a, 2.5).expect("valid ggml operation");
    let mut graph = ctx.new_graph().expect("valid ggml operation");
    graph.build_forward(&scaled).expect("valid ggml operation");

    let backend = GgmlBackend::cpu().expect("valid ggml operation");
    let mut alloc = GgmlAllocr::new(&backend).expect("valid ggml operation");
    // SAFETY: no execution is in flight; allocator and backend remain live
    // through all subsequent tensor-data access and graph computation.
    assert!(unsafe { alloc.alloc_graph(&mut graph) });

    let a_data: [f32; 3] = [1.0, 2.0, 4.0];
    unsafe { tensor_set(&a, bytemuck_cast(&a_data)) };

    // SAFETY: all graph buffers remain owned by alloc; initialized inputs and
    // synchronous execution precede allocator/backend/context destruction.
    unsafe { backend.graph_compute(&mut graph) }.expect("compute graph");

    let mut result = [0.0_f32; 3];
    unsafe {
        tensor_get(
            &graph.node(-1).expect("valid ggml operation"),
            bytemuck_cast_mut(&mut result),
        );
    };
    assert_eq!(result, [2.5, 5.0, 10.0]);
}

#[test]
fn test_graph_matmul() {
    // 2x2 @ 2x2
    let mem_size = tensor_overhead() * 3 + graph_overhead() + 1024;
    let ctx = GgmlContext::new(mem_size, true).expect("valid ggml operation");

    let a = ctx
        .new_tensor_2d(llama_cpp_sys::GGML_TYPE_F32, 2, 2)
        .expect("valid ggml operation");
    let b = ctx
        .new_tensor_2d(llama_cpp_sys::GGML_TYPE_F32, 2, 2)
        .expect("valid ggml operation");
    let c = ctx.mul_mat(&a, &b).expect("valid ggml operation");
    let mut graph = ctx.new_graph().expect("valid ggml operation");
    graph.build_forward(&c).expect("valid ggml operation");

    let backend = GgmlBackend::cpu().expect("valid ggml operation");
    let mut alloc = GgmlAllocr::new(&backend).expect("valid ggml operation");
    // SAFETY: no execution is in flight; allocator and backend remain live
    // through all subsequent tensor-data access and graph computation.
    assert!(unsafe { alloc.alloc_graph(&mut graph) });

    // identity matrix
    let a_data: [f32; 4] = [1.0, 0.0, 0.0, 1.0];
    let b_data: [f32; 4] = [5.0, 6.0, 7.0, 8.0];
    unsafe {
        tensor_set(&a, bytemuck_cast(&a_data));
        tensor_set(&b, bytemuck_cast(&b_data));
    }

    // SAFETY: all graph buffers remain owned by alloc; initialized inputs and
    // synchronous execution precede allocator/backend/context destruction.
    unsafe { backend.graph_compute(&mut graph) }.expect("compute graph");

    let mut result = [0.0_f32; 4];
    unsafe {
        tensor_get(
            &graph.node(-1).expect("valid ggml operation"),
            bytemuck_cast_mut(&mut result),
        );
    };

    // identity @ B = B
    assert_eq!(result, [5.0, 6.0, 7.0, 8.0]);
}

#[test]
fn test_tensor_iteration() {
    let mem_size = tensor_overhead() * 10 + 1024;
    let ctx = GgmlContext::new(mem_size, true).expect("valid ggml operation");
    ctx.new_tensor_1d(llama_cpp_sys::GGML_TYPE_F32, 4)
        .expect("valid ggml operation")
        .set_name("t1")
        .expect("valid tensor name");
    ctx.new_tensor_1d(llama_cpp_sys::GGML_TYPE_F32, 8)
        .expect("valid ggml operation")
        .set_name("t2")
        .expect("valid tensor name");
    ctx.new_tensor_2d(llama_cpp_sys::GGML_TYPE_F16, 2, 3)
        .expect("valid ggml operation")
        .set_name("t3")
        .expect("valid tensor name");

    let mut names = Vec::new();
    let mut t = ctx.first_tensor();
    while let Some(tensor) = t {
        names.push(tensor.name());
        t = ctx.next_tensor(&tensor).expect("valid ggml operation");
    }
    assert_eq!(names, vec!["t1", "t2", "t3"]);
}

// Helper to cast &[f32] to &[u8] without pulling in bytemuck
fn bytemuck_cast(data: &[f32]) -> &[u8] {
    // SAFETY: f32 has no padding, and this shared byte view covers exactly the
    // initialized source allocation for the same lifetime.
    unsafe { std::slice::from_raw_parts(data.as_ptr().cast(), std::mem::size_of_val(data)) }
}

fn bytemuck_cast_mut(data: &mut [f32]) -> &mut [u8] {
    // SAFETY: the exclusive source borrow covers all writable bytes and f32
    // permits every bit pattern produced by the native float computations.
    unsafe { std::slice::from_raw_parts_mut(data.as_mut_ptr().cast(), std::mem::size_of_val(data)) }
}

/// The pool is not a recoverable resource: a debug ggml aborts on exhaustion
/// and a release ggml returns null, which the constructors turn into a panic.
/// `sized_for` exists so callers never find that out at runtime — it must
/// actually reserve room for what it promises.
#[test]
fn sized_for_reserves_room_for_the_tensors_it_promises() {
    let n = 64;
    let ctx = GgmlContext::sized_for(n, 0, true).expect("valid ggml operation");
    for _ in 0..n {
        let _ = ctx
            .new_tensor_1d(llama_cpp_sys::GGML_TYPE_F32, 16)
            .expect("valid ggml operation");
    }
    // All `n` were created without exhausting the pool.
    assert!(ctx.used_mem() <= ctx.mem_size());
}

#[test]
fn sized_for_accounts_for_graphs_too() {
    let with_graph = GgmlContext::sized_for(8, 1, true)
        .expect("valid ggml operation")
        .mem_size();
    let without = GgmlContext::sized_for(8, 0, true)
        .expect("valid ggml operation")
        .mem_size();
    assert!(
        with_graph > without,
        "a graph must add to the pool: {with_graph} vs {without}"
    );
    assert!(with_graph - without >= llama_cpp::ggml::graph_overhead());
}

/// Headroom must be observable *before* allocating, which is the whole point
/// of exposing it — the failure mode it avoids is uncatchable.
#[test]
fn used_and_free_memory_track_allocation() {
    let ctx = GgmlContext::sized_for(32, 0, true).expect("valid ggml operation");
    let free_before = ctx.free_mem();
    let used_before = ctx.used_mem();

    let _t = ctx
        .new_tensor_2d(llama_cpp_sys::GGML_TYPE_F32, 4, 4)
        .expect("valid ggml operation");

    assert!(
        ctx.used_mem() > used_before,
        "allocating a tensor must consume pool"
    );
    assert!(ctx.free_mem() < free_before, "free memory must shrink");
    assert_eq!(
        ctx.used_mem() + ctx.free_mem(),
        ctx.mem_size(),
        "used + free must account for the whole pool"
    );
}

#[test]
fn mem_size_reports_what_was_requested() {
    // ggml may round up, but never down — a smaller pool than asked for would
    // make `sized_for`'s arithmetic a lie.
    let ctx = GgmlContext::new(64 * 1024, true).expect("valid ggml operation");
    assert!(ctx.mem_size() >= 64 * 1024);
}

#[test]
fn dependencies_cannot_retain_a_foreign_arena() {
    let owner = GgmlContext::sized_for(4, 1, true).expect("owner arena");
    let foreign = GgmlContext::sized_for(2, 0, true).expect("foreign arena");
    let a = owner
        .new_tensor_1d(llama_cpp_sys::GGML_TYPE_F32, 4)
        .expect("owner tensor");
    let b = foreign
        .new_tensor_1d(llama_cpp_sys::GGML_TYPE_F32, 4)
        .expect("foreign tensor");
    assert!(matches!(owner.add(&a, &b), Err(GgmlError::ForeignContext)));
    assert!(matches!(
        owner.scale(&b, 1.0),
        Err(GgmlError::ForeignContext)
    ));
    assert!(matches!(
        owner.next_tensor(&b),
        Err(GgmlError::ForeignContext)
    ));
    let mut graph = owner.new_graph().expect("owner graph");
    assert!(matches!(
        graph.build_forward(&b),
        Err(GgmlError::ForeignContext)
    ));
    assert!(matches!(graph.node(-1), Err(GgmlError::InvalidDimensions)));
    // Copying only shape/type does not retain a dependency and is allowed.
    assert_eq!(
        owner
            .dup_tensor(&b)
            .expect("independent metadata copy")
            .ne(),
        b.ne()
    );
}

#[test]
fn names_are_snapshots_and_invalid_shapes_never_reach_native_allocation() {
    let ctx = GgmlContext::sized_for(2, 0, true).expect("arena");
    let tensor = ctx
        .new_tensor_1d(llama_cpp_sys::GGML_TYPE_F32, 4)
        .expect("tensor");
    tensor.set_name("before").expect("initial name");
    let snapshot = tensor.name();
    let alias = tensor;
    alias
        .set_name("after")
        .expect("rename through copied handle");
    assert_eq!(snapshot, "before");
    assert_eq!(tensor.name(), "after");
    assert!(matches!(
        tensor.set_name("bad\0name"),
        Err(GgmlError::Nul(_))
    ));
    assert!(matches!(
        tensor.set_name(&"é".repeat(32)),
        Err(GgmlError::NameTooLong)
    ));
    assert!(matches!(
        ctx.new_tensor_1d(u32::MAX, 4),
        Err(GgmlError::InvalidDimensions)
    ));
    assert!(matches!(
        ctx.new_tensor_1d(llama_cpp_sys::GGML_TYPE_F32, -1),
        Err(GgmlError::InvalidDimensions)
    ));
    assert!(matches!(
        ctx.new_tensor(llama_cpp_sys::GGML_TYPE_F32, &[i64::MAX, 2]),
        Err(GgmlError::InvalidDimensions)
    ));
    assert!(matches!(
        GgmlContext::sized_for(usize::MAX, 1, true),
        Err(GgmlError::InvalidDimensions)
    ));
    assert!(matches!(
        type_name(u32::MAX),
        Err(GgmlError::InvalidDimensions)
    ));
    assert!(matches!(
        is_quantized(u32::MAX),
        Err(GgmlError::InvalidDimensions)
    ));
}
