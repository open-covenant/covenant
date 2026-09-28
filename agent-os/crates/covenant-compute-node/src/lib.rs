//! Operator node core for the Covenant compute network (codename
//! compute). A covenant user runs this to plug in idle GPU/compute and
//! earn SOL/USDC for real agent jobs — see
//! `docs/internal/compute-network/design-01-operator-node.md` for the
//! full design this crate is Phase 1 of.
//!
//! Ships both the library and the operator-facing binary (`main.rs`) —
//! a standalone sibling process (design-01 §4 — never a `covenantd`
//! mode) that loads a persistent identity, registers, heartbeats, and
//! loops [`node::Node::run_once`] over
//! [`http_client::HttpCoordinatorClient`].
//!
//! Module map: [`coordinator`] is the outbound long-poll transport
//! contract (plus the in-memory [`MockCoordinator`] for tests);
//! [`http_client`] is the real `impl Coordinator` against a running
//! `covenant-compute-coordinator`; [`admission`] is the fail-closed
//! pre-execution gate; [`executor`] is the pluggable job-execution
//! trait plus the mock and the real subprocess-sandbox implementations;
//! [`benchmark`] proves the declared capability profile against the
//! real executor before it is ever registered; [`earnings`] is the
//! greenfield credit-side ledger; [`outbox`] is the durable redelivery
//! queue for results the coordinator never acknowledged; [`accepted`]
//! is the durable book of accepted-but-unfinished jobs a restart
//! re-serves; [`setup`] is the first-run onboarding wizard behind
//! `covenant-compute-node setup`; [`node`] wires all of the above
//! into one per-job orchestration loop.

#![deny(unsafe_code)]

pub mod accepted;
pub mod admission;
pub mod benchmark;
pub mod broker;
pub mod container;
pub mod coordinator;
pub mod earnings;
pub mod executor;
pub mod gpu;
pub mod http_client;
pub mod lease;
pub mod node;
pub mod ollama;
pub mod openai_compat;
pub mod outbox;
pub mod service;
pub mod setup;
pub mod tts;
pub mod whisper;

pub use accepted::{AcceptedBook, AcceptedEntry, AcceptedError};
pub use admission::{AdmissionContext, AdmissionError};
pub use benchmark::{run_benchmark, BenchmarkProbe, BenchmarkSpec, ProbeStats};
pub use broker::{BrokerConfig, BrokerSessionBackend, READY_POLL_INTERVAL, READY_TIMEOUT};
pub use container::{ContainerConfig, ContainerJobExecutor};
pub use coordinator::{Coordinator, CoordinatorError, MockCoordinator};
pub use earnings::{
    audit_paid_entry, reconcile_paid_rows, EarningsAuditError, EarningsEntry, EarningsError,
    EarningsLedger, EarningsStatus, InMemoryEarningsLedger, JsonlEarningsLedger,
};
pub use executor::{
    ChunkSink, EchoExecutor, ExecutionOutcome, ExecutorError, JobExecutor, SubprocessJobExecutor,
};
pub use gpu::{detect_gpu, detect_nvidia_gpu, parse_hardware, DetectedGpu};
pub use http_client::{HttpCoordinatorClient, OperatorJobRow, PayoutConfirmation};
pub use lease::{LeaseControl, LeaseExecutor, SessionBackend, StubSessionBackend};
pub use node::{JobOutcome, Node, NodeConfig, NodeError};
pub use ollama::{OllamaExecutor, DEFAULT_OLLAMA_URL};
pub use openai_compat::{OpenAiCompatExecutor, DEFAULT_OPENAI_COMPAT_URL};
pub use outbox::{OutboxEntry, OutboxError, ResultOutbox};
pub use service::{parse_service_args, run_service, ServiceAction, SERVICE_LABEL, SERVICE_USAGE};
pub use setup::{
    ensure_owner_only, parse_env_file, parse_setup_args, run_setup, unset_pairs, SetupOptions,
    DEFAULT_PRICE_MICRO_USDC, ENV_FILE, SETUP_USAGE,
};
pub use tts::{SayExecutor, DEFAULT_SAY_BIN};
pub use whisper::{WhisperExecutor, DEFAULT_WHISPER_BIN};
