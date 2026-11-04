//! Owned native snapshots for diagnostics and benchmark artifacts. Collection
//! runs on the context owner; reports contain no handles or prompt/model paths.
//! Buffer accounting includes recurrent state and excludes driver allocations.
//! Every byte sum is checked; unknown KV layouts have no payload estimate.
use super::{EngineError, EngineResult, ExecutionMode, LLMModel};
use llama_cpp::{
    context::{
        LlamaContext,
        cache::{CacheLayerKind, ContextCacheInfo, KvCacheConfig},
        memory_breakdown::MemoryBreakdownEntry,
    },
    quantize::GgmlType,
};

/// Host-selected context allocation and placement settings before native padding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextRequest {
    /// Requested token capacity.
    pub context_size: u32,
    /// Requested logical batch.
    pub batch_size: u32,
    /// Explicit microbatch, or native-compatible automatic resolution.
    pub microbatch_size: Option<u32>,
    /// Requested weight-layer offload limit.
    pub gpu_layers: u32,
    /// KQV/operator placement policy.
    pub execution: ExecutionMode,
}

/// Requested policy, resolved native settings and complete buffer accounting.
#[derive(Debug, Clone)]
pub struct CacheDiagnostics {
    /// Host-selected cache policy.
    pub requested: KvCacheConfig,
    /// Requested capacity/batching and weight/operator placement policy.
    pub context_request: ContextRequest,
    /// Devices selected for weights; does not establish operator execution.
    pub weight_devices: std::collections::BTreeSet<String>,
    /// Native algorithm and arena settings; does not establish GPU placement.
    pub effective: ContextCacheInfo,
    /// Model, context (including recurrent state), and compute bytes by buffer.
    pub memory: Vec<MemoryBreakdownEntry>,
    /// Formula-derived K/V payload only, excluding padding/rotations/recurrent state.
    pub kv_payload_estimate_bytes: Option<u64>,
}
impl CacheDiagnostics {
    /// Combine contexts borrowing the same resident model. Shared weights are
    /// counted once per buffer; context and compute allocations are summed.
    /// # Errors
    /// Rejects inconsistent weight reports and checked byte-sum overflow.
    pub fn aggregate_memory(reports: &[Self]) -> EngineResult<Vec<MemoryBreakdownEntry>> {
        let mut buffers = std::collections::BTreeMap::<String, MemoryBreakdownEntry>::new();
        for report in reports {
            for entry in &report.memory {
                let total = buffers.entry(entry.buft_name.clone()).or_insert_with(|| {
                    MemoryBreakdownEntry {
                        buft_name: entry.buft_name.clone(),
                        model: entry.model,
                        context: 0,
                        compute: 0,
                    }
                });
                if total.model != entry.model {
                    return Err(EngineError::Configuration(
                        "contexts do not share identical model accounting",
                    ));
                }
                total.context = total
                    .context
                    .checked_add(entry.context)
                    .ok_or(EngineError::Limit("native memory accounting overflow"))?;
                total.compute = total
                    .compute
                    .checked_add(entry.compute)
                    .ok_or(EngineError::Limit("native memory accounting overflow"))?;
                total
                    .checked_total()
                    .ok_or(EngineError::Limit("native memory accounting overflow"))?;
            }
        }
        Ok(buffers.into_values().collect())
    }
    pub(super) fn collect(native: &LlamaContext<'_>, model: &LLMModel<'_>) -> EngineResult<Self> {
        let effective = native.cache_info()?;
        let topology = model.model_ref().cache_topology()?;
        let mut payload = Some(0_u64);
        for arena in &effective.arenas {
            let layers: Vec<_> = topology
                .layers
                .iter()
                .filter(|layer| layer.kind == arena.kind)
                .collect();
            if layers.len() != arena.layers as usize {
                payload = None;
                break;
            }
            for layer in layers {
                for (dimension, format) in [
                    (layer.key_head_dim, arena.key_type),
                    (layer.value_head_dim, arena.value_type),
                ] {
                    let row = match format {
                        GgmlType::F16 => Some(u64::from(dimension) * 2),
                        GgmlType::Q8_0 if dimension % 32 == 0 => {
                            Some(u64::from(dimension / 32) * 34)
                        }
                        #[cfg(feature = "turboquant")]
                        GgmlType::PQ3_0 => effective.pq3.as_ref().and_then(|info| {
                            (dimension % info.block_values == 0).then(|| {
                                u64::from(dimension / info.block_values)
                                    * u64::from(info.block_bytes)
                            })
                        }),
                        GgmlType::Q4_0 if dimension % 32 == 0 => {
                            Some(u64::from(dimension / 32) * 18)
                        }
                        _ => None,
                    };
                    payload = payload.and_then(|total| {
                        row?.checked_mul(u64::from(layer.kv_heads))?
                            .checked_mul(u64::from(arena.cells))?
                            .checked_mul(u64::from(arena.streams))?
                            .checked_add(total)
                    });
                }
            }
        }
        if !effective.inspection_available
            || topology
                .layers
                .iter()
                .any(|l| matches!(l.kind, CacheLayerKind::Full | CacheLayerKind::Sliding))
                && effective.arenas.is_empty()
        {
            payload = None;
        }
        let report = Self {
            requested: model.config().cache,
            context_request: ContextRequest {
                context_size: model.config().context_size,
                batch_size: model.config().batch_size,
                microbatch_size: model
                    .config()
                    .microbatch_size
                    .map(std::num::NonZeroU32::get),
                gpu_layers: model.config().gpu_layers,
                execution: model.config().execution,
            },
            weight_devices: topology
                .layers
                .iter()
                .map(|l| l.device_name.clone())
                .collect(),
            effective,
            memory: native.try_memory_breakdown()?,
            kv_payload_estimate_bytes: payload,
        };
        report.context_compute_bytes()?;
        Ok(report)
    }
    /// Sum context and compute allocations, excluding shared model weights.
    /// # Errors
    /// Returns an error if native byte accounting overflows.
    pub fn context_compute_bytes(&self) -> EngineResult<usize> {
        self.memory.iter().try_fold(0_usize, |sum, entry| {
            sum.checked_add(
                entry
                    .checked_context_compute()
                    .ok_or(EngineError::Limit("native memory accounting overflow"))?,
            )
            .ok_or(EngineError::Limit("native memory accounting overflow"))
        })
    }
    /// Path-free schema used by CLI diagnostics and benchmark artifacts.
    #[must_use]
    pub fn to_json(&self) -> serde_json::Value {
        let e = &self.effective;
        #[allow(unused_mut)] // Only the opt-in codec appends its versioned diagnostics.
        let mut value = serde_json::json!({
            "requested": {"key":self.requested.key.name(),"value":self.requested.value.name(),"swa":format!("{:?}",self.requested.swa),"flash_attention":format!("{:?}",self.requested.flash_attention),"rotation":format!("{:?}",self.requested.rotation),"context_size":self.context_request.context_size,"batch_size":self.context_request.batch_size,"microbatch_size":self.context_request.microbatch_size,"gpu_layers":self.context_request.gpu_layers,"execution":format!("{:?}",self.context_request.execution)},
            "weight_devices":self.weight_devices,
            "effective": {"context_size":e.context_size,"batch_size":e.batch_size,"microbatch_size":e.microbatch_size,"sequences":e.sequences,"flash_attention":e.flash_attention,"offload_kqv":e.offload_kqv,"op_offload":e.op_offload,"inspection_available":e.inspection_available,"arenas":e.arenas.iter().map(|a|serde_json::json!({"kind":format!("{:?}",a.kind),"cells":a.cells,"layers":a.layers,"streams":a.streams,"key_type":format!("{:?}",a.key_type),"value_type":format!("{:?}",a.value_type),"key_rotation":a.key_rotation,"value_rotation":a.value_rotation})).collect::<Vec<_>>()},
            "memory":self.memory.iter().map(|m|serde_json::json!({"buffer":m.buft_name,"model_bytes":m.model,"context_bytes":m.context,"compute_bytes":m.compute})).collect::<Vec<_>>(),
            "kv_payload_estimate_bytes":self.kv_payload_estimate_bytes,
            "gpu_placement":"not_observed", "peak_vram_bytes":null
        });
        #[cfg(feature = "turboquant")]
        if let Some(codec) = &e.pq3 {
            value["effective"]["codec"] = serde_json::json!({
                "name":"pq3_0", "row_version":codec.row_version,
                "graph_version":codec.graph_version, "transform_version":codec.transform_version,
                "block_values":codec.block_values, "block_bytes":codec.block_bytes,
                "execution":format!("{:?}",codec.execution),
                "observed_attention_paths":codec.observed_paths().map(|p|format!("{p:?}")).collect::<Vec<_>>(),
                "completed_indexed_writes":codec.indexed_writes,
                "completed_attention_operations":codec.attention_operations,
                "state_io":"unsupported", "position_shifting":"unsupported"
            });
            if codec.indexed_writes > 0 && codec.attention_operations > 0 {
                value["gpu_placement"] =
                    serde_json::json!("pq3_cache_writes_and_attention_observed");
            }
        }
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn snapshots_are_send_and_sync() {
        fn owned<T: Send + Sync>() {}
        owned::<super::CacheDiagnostics>();
    }
    #[test]
    fn aggregation_counts_shared_weights_once_and_checks_overflow() {
        let effective = ContextCacheInfo {
            context_size: 256,
            batch_size: 8,
            microbatch_size: 8,
            sequences: 1,
            flash_attention: false,
            offload_kqv: false,
            op_offload: false,
            inspection_available: false,
            arenas: Vec::new(),
            #[cfg(feature = "turboquant")]
            pq3: None,
        };
        let report = CacheDiagnostics {
            requested: KvCacheConfig::default(),
            context_request: ContextRequest {
                context_size: 256,
                batch_size: 8,
                microbatch_size: None,
                gpu_layers: 0,
                execution: ExecutionMode::Cpu,
            },
            weight_devices: std::collections::BTreeSet::from(["CPU".into()]),
            effective,
            memory: vec![MemoryBreakdownEntry {
                buft_name: "CPU".into(),
                model: 10,
                context: 20,
                compute: 30,
            }],
            kv_payload_estimate_bytes: None,
        };
        let mut reports = vec![report.clone(), report];
        let memory = CacheDiagnostics::aggregate_memory(&reports).expect("shared model accounting");
        assert_eq!(
            (memory[0].model, memory[0].context, memory[0].compute),
            (10, 40, 60)
        );
        reports[1].memory[0].context = usize::MAX;
        assert!(CacheDiagnostics::aggregate_memory(&reports).is_err());
        reports[1].memory[0].context = 0;
        reports[1].memory[0].model = 11;
        assert!(CacheDiagnostics::aggregate_memory(&reports).is_err());
    }
}
