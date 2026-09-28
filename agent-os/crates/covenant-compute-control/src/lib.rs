//! The Covenant Compute control plane.
//!
//! This is the customer-facing GPU-workspace API: a caller presents a
//! bearer token, browses a catalog of apps and the GPU offers that can
//! run them, and launches a bounded session that bills by the second.
//! It is a thin front end. The money, the escrow, the operator market
//! and the metered settlement all live in the coordinator's lease
//! engine; the control plane translates a launch into a signed lease
//! and reads the meter back out as a job.
//!
//! Two things live here that the lease engine deliberately does not:
//! the app catalog (a curated set of runnable images and their
//! resource floors) and the beta-token identity a customer authenticates
//! with. Everything else is a projection of a lease onto the
//! [`ComputeJob`] shape a workspace client already speaks.
//!
//! This module is the wire contract on its own — the types, the
//! catalog, and the plan validation a launch must clear. The server that
//! serves it and the engine binding that fulfils it are built on top.

#![forbid(unsafe_code)]

mod auth;
mod catalog;
mod engine;
mod plan;
mod provider;
mod server;
mod service;
mod store;
mod web;
mod wire;

pub use auth::{AuthConfigError, AuthError, AuthRegistry, BetaCredential, Principal};
pub use catalog::{AppCatalog, CatalogConfigError};
pub use engine::EngineProvider;
pub use plan::{resolve_plan, validate_plan, PlanRejection, ResolveRejection};
pub use provider::{ProviderBackend, ProviderError};
pub use server::{engine_provider_from_environment, serve, ServerConfig, StartupError};
pub use service::ControlPlane;
pub use web::router;
pub use wire::{
    quote_maximum, AppAvailability, AppKind, ComputeApp, ComputeError, ComputeJob, ComputeOffer,
    ComputeReceipt, GpuSpec, JobStatus, LaunchPlan, LaunchRequest, TrustClass, MIN_DURATION_SECS,
};
