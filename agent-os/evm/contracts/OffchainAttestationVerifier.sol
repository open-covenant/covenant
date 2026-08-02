// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

/// @title OffchainAttestationVerifier — Base-side verification for Covenant's
///        EAS off-chain reputation and provenance attestations.
/// @notice Authenticates the two record kinds Covenant's secp256k1 issuer
///         signs as EAS off-chain attestations — an audit-derived reputation
///         score bound to its Solana anchor, and an audit-root provenance
///         record — with a single `ecrecover` over the EAS `Attest` v1
///         EIP-712 digest. No bridge, no gas, no EAS call on the path: an
///         off-chain attestation is a signature, and this contract is the
///         on-chain half of that check.
///
/// @dev    The Rust crate `covenant-evm-signer` builds byte-identical digests
///         (`eip712.rs`; `reputation.rs` encodes the reputation tuple). The
///         frozen vectors in that crate's
///         `tests/fixtures/offchain-attestation.v1.json` pin digest,
///         signature, and signer per record kind and Base network, and
///         `OffchainAttestationVerifier.t.sol` drives this contract with the
///         same bytes. Keep the type strings and field order in exact sync
///         with `eip712.rs`.
///
///         `verifyReputation`/`verifyProvenance` reject malleable signatures
///         (high-S and v outside {27,28}) so the accepted signature set
///         matches the Rust `recover_address`, and the payload checks mirror
///         `ReputationProjection::validate` — an attestation the Rust side
///         refuses to sign is refused here too.
///
///         Statelessness and its limits, explicitly:
///         - Replay/consumption is the consumer's job. This verifier holds no
///           state; a consumer crediting a record must dedupe on the
///           attestation UID or digest, never on signature bytes.
///         - Revocation is not on this path. The schemas are revocable, and a
///           revoked-off-chain attestation still carries a valid signature; a
///           consumer that must honor revocation checks
///           `EAS.getRevokeOffchain(TRUSTED_ATTESTOR, uid)` before trusting a
///           record. The UID travels in the off-chain envelope; this contract
///           does not recompute it.
contract OffchainAttestationVerifier {
    /// @dev keccak256("EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)")
    bytes32 private constant DOMAIN_TYPEHASH =
        keccak256("EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)");

    /// @dev keccak256 of the EAS off-chain `Attest` v1 type; field order
    ///      matches eip712.rs.
    bytes32 private constant ATTEST_TYPEHASH = keccak256(
        "Attest(uint16 version,bytes32 schema,address recipient,uint64 time,uint64 expirationTime,bool revocable,bytes32 refUID,bytes data)"
    );

    /// @dev The EIP-712 domain name every EAS off-chain attestation shares.
    bytes32 private constant DOMAIN_NAME = keccak256("EAS Attestation");

    /// @dev OffchainAttestationVersion 1: `Attest` leads with `uint16 version`.
    uint16 private constant OFFCHAIN_VERSION = 1;

    /// @notice The EAS contract, an OP-Stack predeploy at the same address on
    ///         Base and Base Sepolia; the off-chain domain is scoped to it.
    address public constant EAS = 0x4200000000000000000000000000000000000021;

    /// @notice The registered reputation schema (UID
    ///         0x84738ec346cd136dddd5b09e8df18a3c5cfb2603aaf5a68758c0149aa406cc39,
    ///         `deployments.json` covenantMainnet.reputationSchema).
    string public constant REPUTATION_SCHEMA =
        "uint32 score,uint8 score_decimals,uint64 expiry,string source_chain,bytes32 solana_attestation_pda";

    /// @notice The audit-root provenance schema (covenant-evm-signer's
    ///         COVENANT_SCHEMA).
    string public constant PROVENANCE_SCHEMA = "bytes32 auditRoot,bytes32 credentialHash";

    /// @dev Past 18 decimals `score / 10^decimals` overflows in consumers;
    ///      mirrors ReputationProjection::validate.
    uint8 private constant MAX_SCORE_DECIMALS = 18;

    /// @notice Covenant's secp256k1 issuer address (operator-custodied key).
    address public immutable TRUSTED_ATTESTOR;

    /// @notice getUID(REPUTATION_SCHEMA, no resolver, revocable), derived in
    ///         the constructor so the schema string and its UID cannot drift.
    bytes32 public immutable REPUTATION_SCHEMA_UID;

    /// @notice getUID(PROVENANCE_SCHEMA, no resolver, revocable).
    bytes32 public immutable PROVENANCE_SCHEMA_UID;

    /// @dev keccak256 of the EAS deployment's `version()` string — "1.0.1" on
    ///      Base mainnet, "1.2.0" on Base Sepolia. A wrong version recovers a
    ///      wrong signer, never a false accept.
    bytes32 private immutable VERSION_HASH;

    /// @notice The EAS domain version this verifier is scoped to, kept
    ///         readable for operator cross-checks against `EAS.version()`.
    string public easVersion;

    error UntrustedSigner();
    error MalleableSignature();
    error Expired();
    error NeverExpires();
    error WindowInverted();
    error MissingAnchor();
    error PlaceholderAnchor();
    error EmptySourceChain();
    error ScoreScaleOverflow();
    error ZeroField();

    /// @dev The reputation tuple in schema order, plus `issuedAt` — the EAS
    ///      envelope `time`. `expiry` doubles as the envelope
    ///      `expirationTime`: the Rust signer mirrors the payload bound onto
    ///      the envelope, so one field feeds both slots and they can never
    ///      disagree here.
    struct ReputationRecord {
        uint32 score;
        uint8 scoreDecimals;
        uint64 expiry;
        string sourceChain;
        bytes32 solanaAttestationPda;
        uint64 issuedAt;
    }

    /// @dev The audit-root provenance payload plus its validity window — the
    ///      EAS envelope `time`/`expirationTime`, mirrored from the credential
    ///      the Rust signer verified before attesting.
    struct ProvenanceRecord {
        bytes32 auditRoot;
        bytes32 credentialHash;
        uint64 validFrom;
        uint64 validUntil;
    }

    constructor(address trustedAttestor, string memory easVersion_) {
        require(trustedAttestor != address(0) && bytes(easVersion_).length != 0, "zero config");
        TRUSTED_ATTESTOR = trustedAttestor;
        easVersion = easVersion_;
        VERSION_HASH = keccak256(bytes(easVersion_));
        REPUTATION_SCHEMA_UID = keccak256(abi.encodePacked(REPUTATION_SCHEMA, address(0), true));
        PROVENANCE_SCHEMA_UID = keccak256(abi.encodePacked(PROVENANCE_SCHEMA, address(0), true));
    }

    /// @dev Bound to `block.chainid` and the EAS predeploy, so an attestation
    ///      signed for one chain's EAS cannot verify against another's.
    function domainSeparator() public view returns (bytes32) {
        return keccak256(abi.encode(DOMAIN_TYPEHASH, DOMAIN_NAME, VERSION_HASH, block.chainid, EAS));
    }

    function reputationDigest(ReputationRecord calldata r) public view returns (bytes32) {
        return _digest(
            REPUTATION_SCHEMA_UID,
            r.issuedAt,
            r.expiry,
            abi.encode(r.score, r.scoreDecimals, r.expiry, r.sourceChain, r.solanaAttestationPda)
        );
    }

    function provenanceDigest(ProvenanceRecord calldata p) public view returns (bytes32) {
        return _digest(PROVENANCE_SCHEMA_UID, p.validFrom, p.validUntil, abi.encode(p.auditRoot, p.credentialHash));
    }

    /// @notice Reverts unless the record is a live, well-formed reputation
    ///         attestation signed by the trusted attestor. Payload checks
    ///         mirror `ReputationProjection::validate`, in the same order.
    function verifyReputation(ReputationRecord calldata r, uint8 v, bytes32 sigR, bytes32 sigS)
        external
        view
        returns (bool)
    {
        if (r.expiry == 0) revert NeverExpires();
        if (r.expiry <= r.issuedAt) revert WindowInverted();
        if (r.solanaAttestationPda == bytes32(0)) revert MissingAnchor();
        if (_repeatedSingleByte(r.solanaAttestationPda)) revert PlaceholderAnchor();
        if (bytes(r.sourceChain).length == 0) revert EmptySourceChain();
        if (r.scoreDecimals > MAX_SCORE_DECIMALS) revert ScoreScaleOverflow();
        if (block.timestamp > r.expiry) revert Expired();
        _checkSignature(reputationDigest(r), v, sigR, sigS);
        return true;
    }

    /// @notice Reverts unless the record is a live audit-root provenance
    ///         attestation signed by the trusted attestor. A zero
    ///         `validUntil` is refused — EAS treats it as never-expiring, and
    ///         a stale root trusted forever is this record's worst failure
    ///         mode.
    function verifyProvenance(ProvenanceRecord calldata p, uint8 v, bytes32 sigR, bytes32 sigS)
        external
        view
        returns (bool)
    {
        if (p.auditRoot == bytes32(0) || p.credentialHash == bytes32(0)) revert ZeroField();
        if (p.validUntil == 0) revert NeverExpires();
        if (p.validUntil <= p.validFrom) revert WindowInverted();
        if (block.timestamp > p.validUntil) revert Expired();
        _checkSignature(provenanceDigest(p), v, sigR, sigS);
        return true;
    }

    function _digest(bytes32 schemaUid, uint64 time, uint64 expirationTime, bytes memory data)
        private
        view
        returns (bytes32)
    {
        bytes32 structHash = keccak256(
            abi.encode(
                ATTEST_TYPEHASH,
                uint256(OFFCHAIN_VERSION),
                schemaUid,
                address(0),
                uint256(time),
                uint256(expirationTime),
                true,
                bytes32(0),
                keccak256(data)
            )
        );
        return keccak256(abi.encodePacked(hex"1901", domainSeparator(), structHash));
    }

    /// @dev Reject signature malleability (high-S and v outside {27,28}): the
    ///      Rust signer only emits low-S with v in {27,28}, so anything else
    ///      is not a legitimate attestation, and a consumer deduping on the
    ///      UID or digest never sees two valid forms of one record.
    function _checkSignature(bytes32 digest, uint8 v, bytes32 sigR, bytes32 sigS) private view {
        if (uint256(sigS) > 0x7FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF5D576E7357A4501DDFE92F46681B20A0) {
            revert MalleableSignature();
        }
        if (v != 27 && v != 28) revert MalleableSignature();
        address signer = ecrecover(digest, v, sigR, sigS);
        if (signer == address(0) || signer != TRUSTED_ATTESTOR) revert UntrustedSigner();
    }

    /// @dev 32 repeats of one byte is the staging-placeholder class (0xab..ab
    ///      and kin) — no real Solana account has that shape.
    ///      `type(uint256).max / 255` is 0x0101…01, so multiplying by the
    ///      first byte spreads it across all 32 positions.
    function _repeatedSingleByte(bytes32 word) private pure returns (bool) {
        return word == bytes32(uint256(uint8(word[0])) * (type(uint256).max / 255));
    }
}
