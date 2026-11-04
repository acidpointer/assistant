//! Validated cache policies and owned snapshots of native cache state.
//!
//! Policies contain no handles and may cross threads. Inspection borrows live
//! model/context storage only during the guarded shim call; results own all
//! names and scalar data. Context inspection stays on its owning thread.
//! An enabled attention algorithm does not prove a particular GPU placement.

use super::memory_breakdown::{decode_name, read_records};
use super::params::{LlamaContextParams, LlamaFlashAttnType};
use crate::{quantize::GgmlType, ShimError};
use llama_cpp_sys as sys;

/// Initially supported, independently selectable cache formats.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum KvCacheType {
    /// Unquantized half precision.
    #[default]
    F16,
    /// Eight bits plus a half-precision scale per 32 values.
    Q8_0,
    /// Four bits plus a half-precision scale per 32 values.
    Q4_0,
}
impl KvCacheType {
    /// Native storage type.
    #[must_use]
    pub const fn ggml_type(self) -> GgmlType {
        match self {
            Self::F16 => GgmlType::F16,
            Self::Q8_0 => GgmlType::Q8_0,
            Self::Q4_0 => GgmlType::Q4_0,
        }
    }
    /// Stable configuration spelling.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::F16 => "f16",
            Self::Q8_0 => "q8_0",
            Self::Q4_0 => "q4_0",
        }
    }
    /// Number of values per storage block.
    #[must_use]
    pub const fn block_size(self) -> u32 {
        match self {
            Self::F16 => 1,
            _ => 32,
        }
    }
}

/// Allocation policy for sliding-window layers; does not change their mask.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum SwaCacheMode {
    /// Retain a full context of window state for compatibility/rollback.
    #[default]
    Full,
    /// Allocate a bounded window plus microbatch allowance.
    Compact,
}

/// Context-local attention-rotation preference.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum AttentionRotation {
    /// Permit native rotation; model and process opt-outs still apply.
    #[default]
    Auto,
    /// Disable rotation for this context without environment mutation.
    Disabled,
}

/// Immutable policy selected before context allocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KvCacheConfig {
    /// Key precision.
    pub key: KvCacheType,
    /// Value precision.
    pub value: KvCacheType,
    /// Sliding-window allocation.
    pub swa: SwaCacheMode,
    /// Native attention algorithm policy, independent of execution placement.
    pub flash_attention: LlamaFlashAttnType,
    /// Context-local rotation preference.
    pub rotation: AttentionRotation,
}
impl Default for KvCacheConfig {
    fn default() -> Self {
        Self {
            key: KvCacheType::F16,
            value: KvCacheType::F16,
            swa: SwaCacheMode::Full,
            flash_attention: LlamaFlashAttnType::Auto,
            rotation: AttentionRotation::Auto,
        }
    }
}

/// A cache policy cannot be represented safely for this model.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum CacheConfigError {
    /// Quantized V has no supported non-flash layout.
    #[error("quantized value cache requires flash attention")]
    FlashRequired,
    /// MLA and certain compressed-attention architectures share K/V storage.
    #[error("this model requires identical key and value cache formats")]
    EqualTypesRequired,
    /// A storage block does not divide an attention head.
    #[error("layer {layer}: {side} head dimension {dimension} is incompatible with cache block size {block}")]
    HeadAlignment {
        /// Model layer index.
        layer: u32,
        /// Key or value side.
        side: &'static str,
        /// Model head dimension.
        dimension: u32,
        /// Required storage block alignment.
        block: u32,
    },
}
impl KvCacheConfig {
    /// Validate portable policy combinations.
    /// # Errors
    /// Rejects quantized V with explicitly disabled flash attention.
    pub fn validate(&self) -> Result<(), CacheConfigError> {
        if self.value != KvCacheType::F16 && self.flash_attention == LlamaFlashAttnType::Disabled {
            return Err(CacheConfigError::FlashRequired);
        }
        Ok(())
    }
    /// Validate attention layers only; recurrent zero-KV layers are excluded.
    /// # Errors
    /// Returns incompatible K/V pairing or head-block alignment.
    pub fn validate_for(&self, model: &ModelCacheTopology) -> Result<(), CacheConfigError> {
        self.validate()?;
        if model.requires_equal_kv && self.key != self.value {
            return Err(CacheConfigError::EqualTypesRequired);
        }
        for layer in &model.layers {
            if !matches!(layer.kind, CacheLayerKind::Full | CacheLayerKind::Sliding) {
                continue;
            }
            for (side, dimension, format) in [
                ("key", layer.key_head_dim, self.key),
                ("value", layer.value_head_dim, self.value),
            ] {
                if dimension == 0 || dimension % format.block_size() != 0 {
                    return Err(CacheConfigError::HeadAlignment {
                        layer: layer.layer,
                        side,
                        dimension,
                        block: format.block_size(),
                    });
                }
            }
        }
        Ok(())
    }
    /// Apply this policy to owned native parameters. Validate model geometry
    /// with [`Self::validate_for`] before creating the context.
    /// # Errors
    /// Returns incompatible portable policy combinations.
    pub fn apply(
        &self,
        params: LlamaContextParams,
    ) -> Result<LlamaContextParams, CacheConfigError> {
        self.validate()?;
        Ok(params
            .with_cache_type_k(self.key.ggml_type())
            .with_cache_type_v(self.value.ggml_type())
            .with_swa_full(self.swa == SwaCacheMode::Full)
            .with_flash_attn_type(self.flash_attention)
            .with_attn_rot_disabled(self.rotation == AttentionRotation::Disabled))
    }
}

/// Role of a model layer or inspected cache arena.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum CacheLayerKind {
    /// No attention/recurrent cache.
    None,
    /// Full attention.
    Full,
    /// Sliding-window attention.
    Sliding,
    /// Recurrent state, excluded from attention quantization.
    Recurrent,
}
fn kind(value: u32) -> Result<CacheLayerKind, ShimError> {
    match value {
        0 => Ok(CacheLayerKind::None),
        1 => Ok(CacheLayerKind::Full),
        2 => Ok(CacheLayerKind::Sliding),
        3 => Ok(CacheLayerKind::Recurrent),
        _ => Err(ShimError::CorruptResult),
    }
}

/// Per-layer geometry; owns its device name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelCacheLayer {
    /// Model layer index.
    pub layer: u32,
    /// Memory role.
    pub kind: CacheLayerKind,
    /// KV heads (zero for recurrent layers).
    pub kv_heads: u32,
    /// Key dimension per head.
    pub key_head_dim: u32,
    /// Value dimension per head.
    pub value_head_dim: u32,
    /// Attention window, zero for full attention.
    pub window: u32,
    /// Device selected for this layer's weights.
    pub device_name: String,
}
/// Full-model topology used for validation; no native handles escape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelCacheTopology {
    /// Layer snapshots, including non-attention layers.
    pub layers: Vec<ModelCacheLayer>,
    /// Whether the native architecture requires identical K/V cache types.
    pub requires_equal_kv: bool,
}

/// Effective state of one nonempty native attention arena.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheArenaInfo {
    /// Full or sliding arena.
    pub kind: CacheLayerKind,
    /// Allocated cells per stream.
    pub cells: u32,
    /// Number of participating layer entries.
    pub layers: u32,
    /// Number of physical cache streams.
    pub streams: u32,
    /// Actual key storage type.
    pub key_type: GgmlType,
    /// Actual value storage type.
    pub value_type: GgmlType,
    /// Key rotation size, zero when absent.
    pub key_rotation: u32,
    /// Value rotation size, zero when absent.
    pub value_rotation: u32,
}
/// Effective algorithm/allocation settings; does not prove GPU execution.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)] // Independent native snapshot flags, not policy parameters.
pub struct ContextCacheInfo {
    /// Actual context capacity.
    pub context_size: u32,
    /// Actual logical batch limit.
    pub batch_size: u32,
    /// Actual physical microbatch limit.
    pub microbatch_size: u32,
    /// Native sequence limit.
    pub sequences: u32,
    /// Native flash-attention algorithm selected.
    pub flash_attention: bool,
    /// KQV offloading requested by native parameters.
    pub offload_kqv: bool,
    /// Operator offloading permitted.
    pub op_offload: bool,
    /// False for memory implementations this inspector cannot describe.
    pub inspection_available: bool,
    /// Known attention arenas; an empty list with unavailable inspection is
    /// not evidence that the model allocated no cache.
    pub arenas: Vec<CacheArenaInfo>,
}

pub(crate) fn inspect_model(
    model: *const sys::llama_model,
) -> Result<ModelCacheTopology, ShimError> {
    let mut requires_equal_kv = 0;
    let empty = sys::llama_cache_layer_info {
        layer: 0,
        kind: 0,
        kv_heads: 0,
        key_head_dim: 0,
        value_head_dim: 0,
        window: 0,
        device_name: [0; 128],
    };
    // SAFETY: the model owner retains initialized immutable weights/geometry;
    // read_records supplies writable output and count storage.
    let records = read_records(empty, |out, cap, count| unsafe {
        sys::llama_model_cache_layers_collect(model, out, cap, count, &raw mut requires_equal_kv)
    })?;
    let layers = records
        .iter()
        .map(|v| {
            Ok(ModelCacheLayer {
                layer: v.layer,
                kind: kind(v.kind)?,
                kv_heads: v.kv_heads,
                key_head_dim: v.key_head_dim,
                value_head_dim: v.value_head_dim,
                window: v.window,
                device_name: decode_name(&v.device_name)?,
            })
        })
        .collect::<Result<_, ShimError>>()?;
    Ok(ModelCacheTopology {
        layers,
        requires_equal_kv: requires_equal_kv != 0,
    })
}

pub(crate) fn inspect_context(
    ctx: *const sys::llama_context,
) -> Result<ContextCacheInfo, ShimError> {
    let mut info = sys::llama_context_cache_info {
        context_size: 0,
        batch_size: 0,
        microbatch_size: 0,
        sequences: 0,
        flash_attention: 0,
        offload_kqv: 0,
        op_offload: 0,
        inspection_available: 0,
    };
    let empty = sys::llama_cache_state_info {
        kind: 0,
        cells: 0,
        layers: 0,
        streams: 0,
        key_type: 0,
        value_type: 0,
        key_rotation: 0,
        value_rotation: 0,
    };
    // SAFETY: the calling context owner excludes concurrent native mutation;
    // every output is a scalar snapshot copied into owned Rust storage.
    let records = read_records(empty, |out, cap, count| unsafe {
        sys::llama_context_cache_collect(ctx, &raw mut info, out, cap, count)
    })?;
    let arenas = records
        .iter()
        .map(|v| {
            Ok(CacheArenaInfo {
                kind: kind(v.kind)?,
                cells: v.cells,
                layers: v.layers,
                streams: v.streams,
                key_type: GgmlType::try_from(
                    sys::ggml_type::try_from(v.key_type).map_err(|_| ShimError::CorruptResult)?,
                )
                .map_err(|_| ShimError::CorruptResult)?,
                value_type: GgmlType::try_from(
                    sys::ggml_type::try_from(v.value_type).map_err(|_| ShimError::CorruptResult)?,
                )
                .map_err(|_| ShimError::CorruptResult)?,
                key_rotation: v.key_rotation,
                value_rotation: v.value_rotation,
            })
        })
        .collect::<Result<_, ShimError>>()?;
    Ok(ContextCacheInfo {
        context_size: info.context_size,
        batch_size: info.batch_size,
        microbatch_size: info.microbatch_size,
        sequences: info.sequences,
        flash_attention: info.flash_attention != 0,
        offload_kqv: info.offload_kqv != 0,
        op_offload: info.op_offload != 0,
        inspection_available: info.inspection_available != 0,
        arenas,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn guarded_collectors_reject_null_owners_and_preserve_count_storage() {
        let mut count = 17;
        // SAFETY: invalid/null input is explicitly rejected by the guarded ABI;
        // count is writable and no native object is dereferenced on this path.
        let status = unsafe {
            sys::llama_memory_breakdown_collect_v2(
                std::ptr::null(),
                std::ptr::null_mut(),
                0,
                &raw mut count,
            )
        };
        assert_eq!(status, sys::LLAMA_SHIM_INVALID_ARG);
        assert_eq!(count, 17);
        // SAFETY: a null context is a documented invalid-input case.
        assert_eq!(
            unsafe { sys::llama_context_synchronize_guarded(std::ptr::null_mut()) },
            sys::LLAMA_SHIM_INVALID_ARG
        );
    }
    #[test]
    fn validation_excludes_recurrent_layers_and_rejects_incompatible_attention() {
        let mut layer = ModelCacheLayer {
            layer: 0,
            kind: CacheLayerKind::Recurrent,
            kv_heads: 0,
            key_head_dim: 0,
            value_head_dim: 0,
            window: 0,
            device_name: "CPU".into(),
        };
        let mut topology = ModelCacheTopology {
            layers: vec![layer.clone()],
            requires_equal_kv: false,
        };
        let config = KvCacheConfig {
            key: KvCacheType::Q8_0,
            value: KvCacheType::Q4_0,
            ..Default::default()
        };
        config
            .validate_for(&topology)
            .expect("recurrent layer excluded");
        layer.kind = CacheLayerKind::Full;
        layer.key_head_dim = 128;
        layer.value_head_dim = 40;
        topology.layers.push(layer);
        assert!(matches!(
            config.validate_for(&topology),
            Err(CacheConfigError::HeadAlignment { side: "value", .. })
        ));
        topology.requires_equal_kv = true;
        assert_eq!(
            config.validate_for(&topology),
            Err(CacheConfigError::EqualTypesRequired)
        );
        assert_eq!(
            KvCacheConfig {
                flash_attention: LlamaFlashAttnType::Disabled,
                ..config
            }
            .validate(),
            Err(CacheConfigError::FlashRequired)
        );
    }
}
