//! Model/GPU-independent experimental format and guarded FFI regression tests.
//! Raw-byte tests retain all array storage through each synchronous native call.
#![cfg(feature = "turboquant")]
use llama_cpp::{
    pq3_reference::{Pq3Block, BLOCK_BYTES, BLOCK_VALUES},
    quantize::GgmlType,
};
use llama_cpp_sys as sys;

#[derive(Debug)]
struct MetadataModel(*mut sys::llama_model);
impl Drop for MetadataModel {
    fn drop(&mut self) {
        // SAFETY: the fixture owns this model, all context creation attempts
        // below fail, and the backend lease outlives this guard.
        unsafe {
            sys::llama_model_free(self.0);
        }
    }
}

#[test]
fn live_cache_selection_is_rejected_before_allocation() {
    let _backend = llama_cpp::llama_backend::LlamaBackend::init().expect("CPU fixture backend");
    let descriptor = sys::llama_quant_model_desc {
        architecture: c"llama".as_ptr(),
        n_embd: 512,
        n_ff: 2048,
        n_layer: 1,
        n_head: 4,
        n_head_kv: 4,
        n_expert: 0,
        n_embd_head_k: 128,
        n_embd_head_v: 128,
    };
    // SAFETY: a live backend and complete scalar descriptor construct an owned
    // metadata-only model; the guarded factory contains native exceptions.
    let pointer = unsafe { sys::llama_quant_model_from_metadata_guarded(&raw const descriptor) };
    assert!(!pointer.is_null(), "metadata-only model factory");
    let model = MetadataModel(pointer);
    for key in [true, false] {
        // SAFETY: defaults are returned by value and this owned model remains
        // live. The new reference-only type must be rejected before allocation.
        let context = unsafe {
            let mut params = sys::llama_context_default_params();
            if key {
                params.type_k = sys::GGML_TYPE_PQ3_0;
            } else {
                params.type_v = sys::GGML_TYPE_PQ3_0;
            }
            sys::llama_init_from_model(model.0, params)
        };
        if !context.is_null() {
            // SAFETY: clean up an unexpected owned context before failing.
            unsafe {
                sys::llama_free(context);
            }
            panic!("reference-only PQ3 was accepted for a live cache");
        }
    }
}

#[test]
fn native_ids_and_fixed_layout_match_without_aliasing_f8() {
    assert_eq!(
        GgmlType::try_from(sys::GGML_TYPE_F8_E4M3).expect("native F8 mapping"),
        GgmlType::F8E4M3
    );
    assert_eq!(sys::GGML_TYPE_F8_E4M3, 43);
    for (native, expected) in [
        (sys::GGML_TYPE_NVFP4, GgmlType::NVFP4),
        (sys::GGML_TYPE_Q1_0, GgmlType::Q1_0),
        (sys::GGML_TYPE_Q2_0, GgmlType::Q2_0),
    ] {
        assert_eq!(
            GgmlType::try_from(native).expect("existing type mapping"),
            expected
        );
    }
    assert_eq!(sys::GGML_TYPE_PQ3_0, 44);
    // SAFETY: pure type metadata; no native handles or mutable global state.
    unsafe {
        assert_eq!(sys::ggml_pq3_reference_abi_v1(), 1);
        assert_eq!(sys::ggml_blck_size(sys::GGML_TYPE_PQ3_0), 128);
        assert_eq!(sys::ggml_type_size(sys::GGML_TYPE_PQ3_0), 50);
    }
}
#[test]
fn golden_packing_and_independent_decode_cover_bit_boundaries() {
    let zero = Pq3Block::quantize(&[0.0; BLOCK_VALUES]).expect("zero block");
    assert_eq!(zero.as_bytes(), &[0; BLOCK_BYTES]);
    assert!(zero
        .dequantize()
        .expect("zero decode")
        .iter()
        .all(|v| v.to_bits() == 0));
    let ones = Pq3Block::quantize(&[1.0; BLOCK_VALUES]).expect("constant golden block");
    for group in ones.as_bytes()[..48].chunks_exact(3) {
        assert_eq!(group, [0x6d, 0xdb, 0xb6]);
    }
    assert_eq!(
        u16::from_ne_bytes([ones.as_bytes()[48], ones.as_bytes()[49]]),
        0x49a8
    );
    let mut bytes = [0; BLOCK_BYTES];
    for group in bytes[..48].chunks_exact_mut(3) {
        group.copy_from_slice(&[0x88, 0xc6, 0xfa]);
    }
    bytes[48..].copy_from_slice(&0x3c00_u16.to_ne_bytes());
    let decoded = Pq3Block::from_bytes(bytes)
        .expect("independent encoded ramp")
        .dequantize()
        .expect("decode ramp");
    let expected = [
        -0.188_397_18_f32,
        -0.118_139_766,
        -0.066_585_61,
        -0.021_604_31,
        0.021_604_31,
        0.066_585_61,
        0.118_139_766,
        0.188_397_18,
    ];
    for (i, value) in decoded.iter().enumerate() {
        assert!((*value - expected[i % 8]).abs() < 1e-7);
    }
}
#[test]
fn invalid_numeric_blocks_fail_and_ffi_outputs_remain_unchanged() {
    for value in [
        f32::NAN,
        f32::INFINITY,
        f32::NEG_INFINITY,
        f32::MAX,
        65504.0,
    ] {
        assert!(Pq3Block::quantize(&[value; BLOCK_VALUES]).is_err());
    }
    for norm in [0x7c00_u16, 0xfc00, 0x7e00, 0x8000, 0xbc00] {
        let mut bytes = [0; BLOCK_BYTES];
        bytes[48..].copy_from_slice(&norm.to_ne_bytes());
        assert!(Pq3Block::from_bytes(bytes).is_err());
    }
    let mut noncanonical = [0; BLOCK_BYTES];
    noncanonical[0] = 1;
    assert!(Pq3Block::from_bytes(noncanonical).is_err());
    let mut output = [0xa5; BLOCK_BYTES];
    let input = [f32::NAN; BLOCK_VALUES];
    // SAFETY: arrays have exact native sizes. NaNs are rejected before any write.
    let status = unsafe {
        sys::llama_pq3_encode_block_guarded(
            input.as_ptr(),
            input.len(),
            output.as_mut_ptr(),
            output.len(),
        )
    };
    assert_eq!(status, sys::LLAMA_SHIM_THROWN);
    assert_eq!(output, [0xa5; BLOCK_BYTES]);
    // SAFETY: a null input is explicitly rejected, with no pointer dereference.
    let status = unsafe {
        sys::llama_pq3_encode_block_guarded(std::ptr::null(), 128, output.as_mut_ptr(), 50)
    };
    assert_eq!(status, sys::LLAMA_SHIM_THROWN);
    assert_eq!(output, [0xa5; BLOCK_BYTES]);
}
#[test]
fn tiny_inputs_and_concurrent_calls_are_deterministic() {
    let tiny = Pq3Block::quantize(&[1e-30; BLOCK_VALUES]).expect("underflow policy");
    assert_eq!(tiny.as_bytes(), &[0; BLOCK_BYTES]);
    fn owned<T: Send + Sync>() {}
    owned::<Pq3Block>();
    let input =
        std::array::from_fn(|i| f32::from(u16::try_from(i).expect("small index")) * 0.01 - 0.64);
    let expected = Pq3Block::quantize(&input).expect("repeatable block");
    let jobs: Vec<_> = (0..8)
        .map(|_| {
            std::thread::spawn(move || Pq3Block::quantize(&input).expect("concurrent pure codec"))
        })
        .collect();
    for job in jobs {
        assert_eq!(job.join().expect("codec thread"), expected);
    }
    let score = expected.dot_q8(&input).expect("CPU score");
    assert!(score.is_finite() && score > 0.0);
}
