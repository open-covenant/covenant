// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

/// @title CovenantReputationRegistry — the on-chain half of multichain-31.
/// @notice Reference verifier and thin store for a Covenant-signed reputation
///         score. A score is Covenant's audit-derived compliance ratio for an
///         agent (see the Rust `covenant-audit::reputation`): of the governed
///         actions it attempted, the fraction that stayed within its
///         authority. This contract authenticates that score with a single
///         `ecrecover` over an EIP-712 digest — no bridge, light client, or
///         Solana read on the path — and can post it so any explorer, indexer,
///         or contract on this chain reads an agent's current standing.
///
/// @dev    The EIP-712 domain uses a constant `salt`, not `chainId` /
///         `verifyingContract` (unlike `SpendGrantEscrow`, whose quality
///         verdict authorizes a payout from one specific escrow and so binds
///         both). A reputation score states a fact about an agent, true on
///         every chain at once, so one issuer signature is portable — the same
///         `(digest, v, r, s)` verifies here, on Base, and off-chain. The Rust
///         crate `covenant-attestation` (`reputation.rs`) builds byte-identical
///         digests; its `eip712_encoding_is_pinned` test pins the typehashes
///         and domain separator below, and its `live_rh_reputation_registry`
///         test drives this deployed contract's `view` methods on 4663. Keep
///         the type strings and field order in exact sync with `reputation.rs`.
///
///         `verify` and `postReputation` reject malleable signatures (high-S
///         and v outside {27,28}) so the accepted set matches `reputation.rs`.
///         `postReputation` keeps only the freshest score per subject, ordered
///         by `validUntil`, so a stale score cannot be replayed over a newer
///         one; consumers that need history read the `ReputationPosted` log.
contract CovenantReputationRegistry {
    /// @dev keccak256("EIP712Domain(string name,string version,bytes32 salt)")
    bytes32 private constant DOMAIN_TYPEHASH =
        keccak256("EIP712Domain(string name,string version,bytes32 salt)");

    /// @dev keccak256 of the Reputation type; field order matches reputation.rs.
    bytes32 private constant REPUTATION_TYPEHASH = keccak256(
        "Reputation(bytes32 subject,uint32 score,uint8 scoreDecimals,uint64 validUntil,string sourceChain,bytes32 solanaAttestation)"
    );

    bytes32 private constant DOMAIN_NAME = keccak256("Covenant Reputation");
    bytes32 private constant DOMAIN_VERSION = keccak256("1");
    /// @dev keccak256("covenant/reputation/v1"), the constant that makes the
    ///      domain chain-agnostic.
    bytes32 private constant DOMAIN_SALT = keccak256("covenant/reputation/v1");

    uint256 private constant SECP256K1N_DIV_2 =
        0x7FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF5D576E7357A4501DDFE92F46681B20A0;
    uint8 private constant MAX_SCORE_DECIMALS = 18;

    /// @notice Covenant's secp256k1 issuer address (operator-custodied key),
    ///         the same canonical attestor as the Base and 4663 bond verifiers.
    address public immutable TRUSTED_ATTESTOR;

    struct Reputation {
        bytes32 subject; // agent's Solana identity (ed25519/PDA) — the binding
        uint32 score;
        uint8 scoreDecimals;
        uint64 validUntil;
        string sourceChain;
        bytes32 solanaAttestation;
    }

    struct Record {
        uint32 score;
        uint8 scoreDecimals;
        uint64 validUntil;
        bytes32 solanaAttestation;
        uint64 postedAt;
    }

    /// @notice The freshest posted score per subject; `postedAt == 0` means none.
    mapping(bytes32 => Record) public latest;

    error UntrustedSigner();
    error Expired();
    error ZeroSubject();
    error ZeroField();
    error ScoreDecimalsTooLarge();
    error ScoreOutOfRange();
    error MalleableSignature();
    error StaleUpdate();

    event ReputationPosted(
        bytes32 indexed subject,
        uint32 score,
        uint8 scoreDecimals,
        uint64 validUntil,
        bytes32 solanaAttestation
    );

    constructor(address trustedAttestor) {
        require(trustedAttestor != address(0), "zero attestor");
        TRUSTED_ATTESTOR = trustedAttestor;
    }

    /// @dev Chain-agnostic: bound to a constant salt, not `block.chainid` or
    ///      `address(this)`, matching reputation.rs. One signature is portable.
    function domainSeparator() public pure returns (bytes32) {
        return keccak256(abi.encode(DOMAIN_TYPEHASH, DOMAIN_NAME, DOMAIN_VERSION, DOMAIN_SALT));
    }

    function digest(Reputation calldata r) public pure returns (bytes32) {
        bytes32 structHash = keccak256(
            abi.encode(
                REPUTATION_TYPEHASH,
                r.subject,
                uint256(r.score),
                uint256(r.scoreDecimals),
                uint256(r.validUntil),
                keccak256(bytes(r.sourceChain)),
                r.solanaAttestation
            )
        );
        return keccak256(abi.encodePacked(hex"1901", domainSeparator(), structHash));
    }

    /// @notice Reverts unless `r` is a live, well-formed score signed by the
    ///         trusted attestor; returns the authenticated score and its scale.
    ///         Same fail-closed checks as `SignedReputationAttestation::verify`.
    function verify(Reputation calldata r, uint8 v, bytes32 sigR, bytes32 sigS)
        external
        view
        returns (uint32 score, uint8 scoreDecimals)
    {
        _checkForm(r);
        if (block.timestamp > r.validUntil) revert Expired();
        _checkSigner(r, v, sigR, sigS);
        return (r.score, r.scoreDecimals);
    }

    /// @notice Verify `r`, then record it as the subject's current score and
    ///         emit `ReputationPosted`. Rejects a score no fresher than the one
    ///         already stored (by `validUntil`), so an old score cannot be
    ///         replayed over a newer one. Callable by anyone holding a
    ///         canonical signature — authenticity is the signature, not the
    ///         sender.
    function postReputation(Reputation calldata r, uint8 v, bytes32 sigR, bytes32 sigS) external {
        _checkForm(r);
        if (block.timestamp > r.validUntil) revert Expired();
        _checkSigner(r, v, sigR, sigS);

        Record storage prev = latest[r.subject];
        if (prev.postedAt != 0 && r.validUntil <= prev.validUntil) revert StaleUpdate();

        latest[r.subject] = Record({
            score: r.score,
            scoreDecimals: r.scoreDecimals,
            validUntil: r.validUntil,
            solanaAttestation: r.solanaAttestation,
            postedAt: uint64(block.timestamp)
        });
        emit ReputationPosted(r.subject, r.score, r.scoreDecimals, r.validUntil, r.solanaAttestation);
    }

    function _checkForm(Reputation calldata r) private pure {
        if (r.subject == bytes32(0)) revert ZeroSubject();
        if (r.validUntil == 0 || r.solanaAttestation == bytes32(0) || bytes(r.sourceChain).length == 0) {
            revert ZeroField();
        }
        if (r.scoreDecimals > MAX_SCORE_DECIMALS) revert ScoreDecimalsTooLarge();
        if (uint256(r.score) > 10 ** uint256(r.scoreDecimals)) revert ScoreOutOfRange();
    }

    function _checkSigner(Reputation calldata r, uint8 v, bytes32 sigR, bytes32 sigS) private view {
        if (uint256(sigS) > SECP256K1N_DIV_2) revert MalleableSignature();
        if (v != 27 && v != 28) revert MalleableSignature();
        address signer = ecrecover(digest(r), v, sigR, sigS);
        if (signer == address(0) || signer != TRUSTED_ATTESTOR) revert UntrustedSigner();
    }
}
