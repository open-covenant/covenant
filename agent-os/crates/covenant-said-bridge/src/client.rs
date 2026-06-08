use std::sync::Arc;

use crate::config::Config;
use crate::{BridgeError, Result};

#[derive(Clone)]
pub struct SaidBridge {
    inner: Arc<Config>,
}

impl SaidBridge {
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

    #[allow(dead_code)]
    pub(crate) fn require_paid(&self, instruction: &'static str, allowed: bool) -> Result<()> {
        self.require_enabled()?;
        if allowed {
            Ok(())
        } else {
            Err(BridgeError::PaidGateClosed { instruction })
        }
    }
}
