//! Embedded, local-first HTTP transport plugin. The core owns interactions and
//! bounded state; this crate owns HTTP, SSE, browser policy and one HTML asset.
//! Preparation binds without serving. Activation uses the ready application;
//! shutdown stops admission, closes streams and joins the HTTP task.

mod api;
mod http;
mod service;

use assistant::{
    chat::ChatResult,
    pipeline::{Plugin, PluginRegistrar},
};
use std::{
    net::{IpAddr, Ipv4Addr},
    sync::Arc,
};

/// HTTP listener configuration. Port zero requests an OS-assigned port.
#[derive(Debug, Clone)]
pub struct WebConfig {
    /// Bind address. The default confines access to this machine.
    pub host: IpAddr,
    /// TCP listener port.
    pub port: u16,
}
impl Default for WebConfig {
    fn default() -> Self {
        Self {
            host: IpAddr::V4(Ipv4Addr::LOCALHOST),
            port: 8080,
        }
    }
}

/// Built-in transport registered by the daemon unless `--no-web` is supplied.
#[derive(Debug)]
pub struct WebPlugin {
    config: WebConfig,
}
impl WebPlugin {
    /// Configure without performing I/O.
    #[must_use]
    pub fn new(config: WebConfig) -> Self {
        Self { config }
    }
}
impl Plugin for WebPlugin {
    fn configure(&self, registrar: &mut PluginRegistrar) -> ChatResult<()> {
        registrar.runtime_service(Arc::new(service::WebFactory(self.config.clone())))
    }
}
