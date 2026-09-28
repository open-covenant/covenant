//! The set of apps the control plane will launch. The catalog is curated,
//! not open: every entry is validated on load and its id is unique, so a
//! launch plan can be checked against it by equality. A launch names an
//! app the catalog holds and the control plane confirms the plan still
//! matches the released entry byte for byte.

use std::collections::HashSet;
use std::sync::Arc;

use thiserror::Error;

use crate::wire::{AppAvailability, AppKind, ComputeApp, ComputeError, TrustClass};

/// An immutable, validated set of launchable apps, shareable across the
/// server's request handlers.
#[derive(Debug, Clone)]
pub struct AppCatalog {
    apps: Arc<[ComputeApp]>,
}

impl AppCatalog {
    /// Validates every entry and rejects a duplicate id. A catalog that
    /// does not load is a configuration error surfaced at startup, never
    /// at launch time.
    pub fn new(apps: Vec<ComputeApp>) -> Result<Self, ComputeError> {
        let mut seen = HashSet::new();
        for app in &apps {
            app.validate()?;
            if !seen.insert(app.id.clone()) {
                return Err(ComputeError::DuplicateApp(app.id.clone()));
            }
        }
        Ok(Self { apps: apps.into() })
    }

    /// Loads a deployment's catalog from a JSON array of apps, the form a
    /// deployment configures instead of the built-in set. Every entry is
    /// validated and its id checked for uniqueness by [`AppCatalog::new`],
    /// so a malformed image reference or a duplicate id fails startup with
    /// the reason rather than serving a broken catalog. An empty array is
    /// refused: a control plane that offers nothing to launch is a
    /// misconfiguration, not a deployment; leave the setting unset to serve
    /// the built-in catalog.
    pub fn from_json(json: &str) -> Result<Self, CatalogConfigError> {
        let apps: Vec<ComputeApp> = serde_json::from_str(json)?;
        if apps.is_empty() {
            return Err(CatalogConfigError::Empty);
        }
        Ok(Self::new(apps)?)
    }

    /// The catalog shipped when the operator configures none: a released
    /// GPU workspace plus two previewed apps that carry no launch image
    /// yet.
    pub fn builtin() -> Self {
        Self::new(vec![
            ComputeApp {
                id: "comfyui".into(),
                name: "ComfyUI".into(),
                summary: "Create images with a visual generative workflow.".into(),
                kind: AppKind::Image,
                availability: AppAvailability::Preview,
                image: None,
                min_vram_mib: 16_384,
                min_trust: TrustClass::Open,
                default_duration_secs: 1_800,
                max_duration_secs: 14_400,
                default_max_usdc_micros: 500_000,
            },
            ComputeApp {
                id: "open-webui".into(),
                name: "Open WebUI".into(),
                summary: "Run an open-model chat session on a dedicated GPU.".into(),
                kind: AppKind::Chat,
                availability: AppAvailability::Preview,
                image: None,
                min_vram_mib: 16_384,
                min_trust: TrustClass::Open,
                default_duration_secs: 3_600,
                max_duration_secs: 21_600,
                default_max_usdc_micros: 1_000_000,
            },
            ComputeApp {
                id: "gpu-workspace".into(),
                name: "GPU Workspace".into(),
                summary: "Open a bounded CUDA and Jupyter workspace on a dedicated GPU.".into(),
                kind: AppKind::Workspace,
                availability: AppAvailability::Available,
                image: Some(
                    "docker.io/nvidia/cuda@sha256:cff3a0d82d2c2b47bab252d67fa9b34a20ef4c50781d98501b5c7367ea9afd10"
                        .into(),
                ),
                min_vram_mib: 16_384,
                min_trust: TrustClass::Open,
                default_duration_secs: 1_800,
                max_duration_secs: 21_600,
                default_max_usdc_micros: 500_000,
            },
        ])
        .expect("built-in compute catalog must be valid")
    }

    pub fn apps(&self) -> &[ComputeApp] {
        &self.apps
    }

    /// Looks an app up by id, or reports it unknown — the first thing a
    /// launch plan is checked against.
    pub fn app(&self, id: &str) -> Result<&ComputeApp, ComputeError> {
        self.apps
            .iter()
            .find(|app| app.id == id)
            .ok_or_else(|| ComputeError::UnknownApp(id.to_owned()))
    }
}

/// Why a configured catalog could not be loaded. Each arm names the fault
/// so a deployment with a bad catalog fails to start with the reason.
#[derive(Debug, Error)]
pub enum CatalogConfigError {
    #[error("the compute catalog is not valid JSON: {0}")]
    Malformed(#[from] serde_json::Error),
    #[error(
        "the compute catalog is empty; configure at least one app, or leave it unset for the \
         built-in catalog"
    )]
    Empty,
    #[error(transparent)]
    Invalid(#[from] ComputeError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_builtin_catalog_loads_and_exposes_its_apps() {
        let catalog = AppCatalog::builtin();
        assert_eq!(catalog.apps().len(), 3);
        assert!(catalog.app("gpu-workspace").is_ok());
        assert!(matches!(
            catalog.app("nope"),
            Err(ComputeError::UnknownApp(_))
        ));
    }

    #[test]
    fn only_the_released_app_carries_a_launch_image() {
        let catalog = AppCatalog::builtin();
        let workspace = catalog.app("gpu-workspace").unwrap();
        assert_eq!(workspace.availability, AppAvailability::Available);
        assert!(workspace.image.is_some());
        for previewed in ["comfyui", "open-webui"] {
            let app = catalog.app(previewed).unwrap();
            assert_eq!(app.availability, AppAvailability::Preview);
            assert!(app.image.is_none());
        }
    }

    #[test]
    fn a_duplicate_id_is_refused() {
        let one = AppCatalog::builtin().apps()[0].clone();
        assert!(matches!(
            AppCatalog::new(vec![one.clone(), one]),
            Err(ComputeError::DuplicateApp(_))
        ));
    }

    #[test]
    fn a_configured_catalog_round_trips_through_json() {
        // The configured form is exactly the served form: a deployment can
        // read the built-in catalog out, edit it, and load it back.
        let json = serde_json::to_string(AppCatalog::builtin().apps()).unwrap();
        let configured = AppCatalog::from_json(&json).unwrap();
        assert_eq!(configured.apps(), AppCatalog::builtin().apps());
    }

    #[test]
    fn a_malformed_or_empty_catalog_is_refused() {
        assert!(matches!(
            AppCatalog::from_json("not json"),
            Err(CatalogConfigError::Malformed(_))
        ));
        assert!(matches!(
            AppCatalog::from_json("[]"),
            Err(CatalogConfigError::Empty)
        ));
    }

    #[test]
    fn a_configured_catalog_clears_the_same_validation_as_the_builtin() {
        // A released app must carry a digest-pinned image, and ids stay
        // unique — the same checks the built-in catalog passes, now applied
        // to a deployment's own JSON.
        let mut unpinned = AppCatalog::builtin().app("gpu-workspace").unwrap().clone();
        unpinned.image = Some("docker.io/nvidia/cuda:latest".into());
        assert!(matches!(
            AppCatalog::from_json(&serde_json::to_string(&[unpinned]).unwrap()),
            Err(CatalogConfigError::Invalid(ComputeError::ImageNotPinned))
        ));

        let one = AppCatalog::builtin().app("gpu-workspace").unwrap().clone();
        assert!(matches!(
            AppCatalog::from_json(&serde_json::to_string(&[one.clone(), one]).unwrap()),
            Err(CatalogConfigError::Invalid(ComputeError::DuplicateApp(_)))
        ));
    }
}
