//! Model-independent metadata ABI tests. Multiple keys, invalid strings and
//! vector growth must preserve the native sentinel and existing values.
use llama_cpp::model::params::{
    kv_overrides::ParamOverrideValue, KvOverrideError, LlamaModelParams,
};
#[test]
fn metadata_overrides_are_bounded_and_transactional() {
    let mut params = std::pin::pin!(LlamaModelParams::default());
    params
        .as_mut()
        .append_kv_override(c"first", ParamOverrideValue::Int(2))
        .expect("first");
    params
        .as_mut()
        .append_kv_override(c"second", ParamOverrideValue::Bool(false))
        .expect("second");
    assert_eq!(params.kv_overrides().into_iter().count(), 2);
    assert_eq!(
        params
            .as_mut()
            .append_kv_override(c"first", ParamOverrideValue::Int(1)),
        Err(KvOverrideError::DuplicateKey)
    );
    assert_eq!(
        params
            .as_mut()
            .append_kv_override(c"", ParamOverrideValue::Int(1)),
        Err(KvOverrideError::InvalidKey)
    );
    assert_eq!(
        params
            .as_mut()
            .append_kv_override(c"bad", ParamOverrideValue::Float(f64::NAN)),
        Err(KvOverrideError::InvalidValue)
    );
    assert_eq!(
        params
            .as_mut()
            .append_kv_override(c"bad", ParamOverrideValue::Str([1; 128])),
        Err(KvOverrideError::InvalidValue)
    );
    let oversized = std::ffi::CString::new("x".repeat(128)).expect("CString");
    assert_eq!(
        params
            .as_mut()
            .append_kv_override(&oversized, ParamOverrideValue::Int(1)),
        Err(KvOverrideError::InvalidKey)
    );
    for index in 2..128 {
        let key = std::ffi::CString::new(format!("k{index}")).expect("key");
        params
            .as_mut()
            .append_kv_override(&key, ParamOverrideValue::Int(index))
            .expect("bounded override");
    }
    assert_eq!(
        params
            .as_mut()
            .append_kv_override(c"overflow", ParamOverrideValue::Int(1)),
        Err(KvOverrideError::Capacity)
    );
    let values = params.kv_overrides().into_iter().collect::<Vec<_>>();
    assert_eq!(values.len(), 128);
    assert_eq!(values[0].1, ParamOverrideValue::Int(2));
    assert_eq!(values[1].1, ParamOverrideValue::Bool(false));
}
