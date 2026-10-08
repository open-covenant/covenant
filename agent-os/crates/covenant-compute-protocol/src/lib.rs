//! Shared wire protocol for the Covenant compute network (codename
//! compute): the signed job envelope, GPU/compute capability-profile
//! schema, the operator's `WorkReceipt`, coordinator<->operator
//! long-poll wire messages, and the `FederationEscrow` trait.
//!
//! Pure types and signing helpers — no I/O, no transport. Every party
//! that speaks the wire — `covenant-compute-node`,
//! `covenant-compute-coordinator`, `covenant-compute-buyer` — depends
//! on this crate, so the sides can never drift on wire shape.

#![deny(unsafe_code)]

mod address;
mod agent;
mod bond;
mod cancel;
mod capability;
mod chat;
mod dispute;
mod embedding;
mod envelope;
mod escrow;
mod fees;
mod generation;
mod lease;
mod lease_close;
mod logprobs;
mod merkle;
mod payout;
mod proof;
mod read_auth;
mod receipt;
mod sign;
mod speech;
mod stream;
mod tool;
mod transcription;
mod vault;
mod version;
mod wire;
mod withdrawal;

pub use address::validate_address_b58;
pub use agent::{
    agent_check_input, agent_check_output, agent_task_input, agent_task_output, check_commands,
    parse_agent_check, parse_agent_check_verdict, parse_agent_task, parse_agent_task_output,
    sha256_hex, AcceptanceSpec, AgentCheckSpec, AgentCheckVerdict, AgentRuntime, AgentSkill,
    AgentTaskOutput, AgentTaskSpec, CommandOutcome, HiddenChecks, HiddenFile, RepoSource,
    AGENT_CHECK_SLACK_MS, MAX_ACCEPTANCE_COMMANDS, MAX_ACCEPTANCE_TIMEOUT_SECS,
    MAX_BUNDLE_B64_BYTES, MAX_HIDDEN_FILES, MAX_OUTPUT_TAIL_BYTES, MAX_PATCH_BYTES,
    MAX_SUMMARY_BYTES, MAX_TASK_BYTES,
};
pub use bond::{
    bond_memo_for, bond_refund_memo_for, parse_bond_refund_memo, UnbondRequest, BOND_MEMO_PREFIX,
    BOND_REFUND_MEMO_PREFIX, UNBOND_MAX_SKEW_MS,
};
pub use cancel::{CancelRequest, CancelView, CANCEL_MAX_SKEW_MS};
pub use capability::{
    canonical_model, CapabilityProfile, CapabilityRequirement, CapacityEntry, CapacityView,
    HardwareClass, JobKind, KindAsk, KindModels, PriceAsk, PriceUnit,
};
pub use chat::{
    chat_input, parse_chat_input, ChatMessage, ChatRole, FunctionCall, ToolCall, ToolCallKind,
};
pub use dispute::{DisputeRequest, DISPUTE_MAX_SKEW_MS, MAX_DISPUTE_REASON_BYTES};
pub use embedding::{
    embedding_output, embedding_texts, parse_embedding_output, parse_embedding_output_expecting,
    EmbeddingResult,
};
pub use envelope::{JobEnvelopePayload, SignedJobEnvelope};
pub use escrow::{
    EscrowError, EscrowHoldAttestation, EscrowStatus, FederationEscrow, FundingSource,
    RefundReason, DEPOSIT_MEMO_PREFIX,
};
pub use fees::{fee_take_micro_usdc, MarketplaceFee, MAX_FEE_BPS};
pub use generation::{
    generation_input, parse_generation_params, GenerationParams, ResponseFormat, MAX_SCHEMA_BYTES,
    MAX_SCHEMA_NAME_BYTES, MAX_STOP_SEQUENCES, MAX_STOP_SEQUENCE_BYTES, MAX_TOP_LOGPROBS,
};
pub use lease::{
    lease_input, parse_lease_terms, LeaseTerms, LEASE_DEADLINE_SLACK_MS, MAX_CLIENT_KEY_BYTES,
    MAX_LEASE_DURATION_SECS,
};
pub use lease_close::{
    lease_access_chunk, parse_lease_access, LeaseAccess, LeaseCloseRequest, LeaseView,
    LEASE_CLOSE_MAX_SKEW_MS, MAX_ACCESS_ENDPOINT_BYTES,
};
pub use logprobs::{logprobs_block, parse_logprobs_output, TokenLogprob, TopLogprob};
pub use payout::{
    payout_memo_for, payout_transaction_rpc_request, verify_payout_transaction, PayoutProof,
};
pub use proof::{BatchInclusionProof, SettlementBatch, SettlementProof};
pub use read_auth::{
    sign_read, verify_read, READ_SIGNATURE_HEADER, READ_SIGNED_AT_HEADER, SIGNED_READ_MAX_SKEW_MS,
};
pub use receipt::{
    output_hash_hex, parse_payout_memo, FinishReason, JobMeter, SignedWorkReceipt,
    WorkReceiptPayload, PAYOUT_MEMO_PREFIX,
};
pub use sign::ProtocolError;
pub use speech::{
    parse_speech_input, parse_speech_output, speech_input, speech_output, SpeechInput,
    SpeechResult, MAX_SPEECH_SPEED, MAX_SPEECH_TEXT_CHARS, MIN_SPEECH_SPEED,
};
pub use stream::{StreamChunk, StreamPush};
pub use tool::{
    assistant_output, parse_assistant_output, parse_tools_input, tools_input, AssistantReply,
    FunctionDefinition, NamedFunction, NamedToolChoice, RequestTools, ToolChoice, ToolChoiceMode,
    ToolDefinition, ToolKind, MAX_TOOLS,
};
pub use transcription::{
    parse_transcription_input, parse_transcription_output, transcription_input,
    transcription_output, TranscriptionInput, TranscriptionResult, TranscriptionSegment,
    MAX_AUDIO_B64_BYTES,
};
pub use vault::{
    open as vault_open, seal as vault_seal, sign_vault, vault_list_path, vault_secret_path,
    vault_signing_path, verify_vault, SealedSecret, SecretMeta, VaultError, VaultKey,
    MAX_SECRET_BYTES, VAULT_MAX_SKEW_MS, VAULT_SIGNATURE_HEADER, VAULT_SIGNED_AT_HEADER,
};
pub use version::{PROTOCOL_VERSION, PROTOCOL_VERSION_HEADER};
pub use wire::{
    coordinator_reason, HeartbeatRequest, HeartbeatResponse, JobAccept, JobOffer, JobResultAck,
    JobResultMessage, OperatorStatus, RegisterRequest, RegisterResponse, ResultSettlement,
    HEARTBEAT_MAX_SKEW_MS,
};
pub use withdrawal::{
    parse_withdrawal_memo, withdrawal_memo_for, WithdrawalRequest, WITHDRAWAL_MAX_SKEW_MS,
    WITHDRAWAL_MEMO_PREFIX,
};
