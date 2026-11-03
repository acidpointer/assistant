//! This is simple abstraction for common models engine

use std::sync::{LazyLock, OnceLock};

use llama_cpp::prelude::*;

use crate::engine::EngineResult;

// llama.cpp allows us to have exactly one instance of backend.
// One backend, but multiple models - this is what we might have in production,
// so ModelEngine will own this single backend and enforce this
// ownership by singleton - super duper easy and error free.
static ENGINE: OnceLock<ModelEngine> = OnceLock::new();

#[derive(Debug)]
pub struct ModelEngine {
    backend: LlamaBackend,
}

impl ModelEngine {
    pub fn init() -> EngineResult<()> {
        let backend = LlamaBackend::init()?;

        let instance = Self { backend };

        match ENGINE.set(instance) {
            Ok(()) => Ok(()),
            Err(_) => Err(super::EngineError::EngineAlreadyInitializedError),
        }
    }

    pub fn get_instance() -> EngineResult<&'static ModelEngine> {
        let inst = ENGINE
            .get()
            .ok_or(super::EngineError::EngineNotInitializedError)?;

        Ok(inst)
    }

    #[inline]
    pub fn backend_ref(&self) -> &LlamaBackend {
        &self.backend
    }
}
