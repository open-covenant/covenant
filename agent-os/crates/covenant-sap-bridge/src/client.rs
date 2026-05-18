//! Top-level bridge client.
//!
//! Skeleton: holds config + a reqwest client. Identity, discovery,
//! and attestation operations land in the per-module submodules. The
//! split mirrors SAP's own module layout so the bridge surface stays
//! one-to-one with the SDK.

use reqwest::Client as HttpClient;
use std::sync::Arc;

use crate::config::Config;
use crate::{BridgeError, Result};

#[derive(Clone)]
pub struct SapBridge {
    inner: Arc<Inner>,
}

#[allow(dead_code)] // `http` is wired up in follow-up commits.
struct Inner {
    config: Config,
    http: HttpClient,
}

impl SapBridge {
    pub fn new(config: Config) -> Result<Self> {
        let http = HttpClient::builder()
            .user_agent(concat!("covenant-sap-bridge/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self {
            inner: Arc::new(Inner { config, http }),
        })
    }

    pub fn config(&self) -> &Config {
        &self.inner.config
    }

    #[allow(dead_code)] // exposed for the per-module RPC code in follow-ups.
    pub(crate) fn http(&self) -> &HttpClient {
        &self.inner.http
    }

    pub(crate) fn require_enabled(&self) -> Result<()> {
        if self.inner.config.enabled {
            Ok(())
        } else {
            Err(BridgeError::Disabled)
        }
    }
}
