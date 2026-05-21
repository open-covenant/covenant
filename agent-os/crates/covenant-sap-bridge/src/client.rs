//! Top-level bridge client.
//!
//! Holds the resolved [`Config`]. Identity, discovery, and attestation
//! operations live in the per-module submodules and drive the
//! TypeScript bridge worker via [`crate::worker`]. The split mirrors
//! SAP's own module layout so the bridge surface stays one-to-one with
//! the SDK.

use std::sync::Arc;

use crate::config::Config;
use crate::{BridgeError, Result};

#[derive(Clone)]
pub struct SapBridge {
    inner: Arc<Config>,
}

impl SapBridge {
    pub fn new(config: Config) -> Result<Self> {
        Ok(Self {
            inner: Arc::new(config),
        })
    }

    pub fn config(&self) -> &Config {
        &self.inner
    }

    pub(crate) fn require_enabled(&self) -> Result<()> {
        if self.inner.enabled {
            Ok(())
        } else {
            Err(BridgeError::Disabled)
        }
    }
}
