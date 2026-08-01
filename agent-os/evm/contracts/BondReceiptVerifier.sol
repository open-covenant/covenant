// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

/// @title BondReceiptVerifier — the on-chain half of multichain-30.
/// @notice Reference verifier for a Covenant-signed agent bond receipt. It
///         authenticates the receipt with a single `ecrecover` over an
///         EIP-712 digest — no bridge, light client, or Solana read on the
///         path. A default agent bond is chain-local USDC (never $CVNT);
///         Covenant confirms it off-chain and this contract checks it.
///
/// @dev    Deployed on Base mainnet; the live address is recorded in
///         `agent-os/evm/deployments.json`. Wiring a USDC bond escrow on top
///         is a separate operator decision. The Rust crate
///         `covenant-attestation` (`bond.rs`) builds byte-identical digests;
///         its `eip712_encoding_is_pinned` test pins the two typehashes and a
///         domain separator below, and its `live_bond_verifier` test proves
///         the same `(digest, v, r, s)` recovers `TRUSTED_ATTESTOR` through
///         the `ecrecover` precompile on live Base Sepolia. Keep the type
///         strings and field order in exact sync with `bond.rs`.
///
///         `verify` rejects malleable signatures (high-S and v outside
///         {27,28}) so the accepted signature set matches `bond.rs`, and a
///         consumer that dedups on the signature can never see two valid forms
///         of one receipt. Replay protection is still the consuming escrow's
///         job: this verifier is stateless, so the escrow must record each
///         spent receipt (by nonce or digest) and must not dedup on signature
///         bytes.
contract BondReceiptVerifier {
    /// @dev keccak256("EIP712Domain(string name,string version,uint256 chainId)")
    ///      == 0xc2f8787176b8ac6bf7215b4adcc1e069bf4ab82d9ab1df05a57a91d425935b6e
    bytes32 private constant DOMAIN_TYPEHASH = keccak256("EIP712Domain(string name,string version,uint256 chainId)");

    /// @dev keccak256 of the BondReceipt type; field order matches bond.rs.
    ///      == 0x2a1ad6a6ecf1ab273f9cd50e3f3a22f0eb1d9eb1dd75063a6a489aaa63415135
    bytes32 private constant BOND_RECEIPT_TYPEHASH = keccak256(
        "BondReceipt(bytes32 subject,address bondToken,uint256 bondAmount,address agentReturn,address slashBeneficiary,uint256 slashBeneficiaryBps,bytes32 nonce,uint256 issuedAt,uint256 expiry)"
    );

    bytes32 private constant DOMAIN_NAME = keccak256("Covenant Bond Receipt");
    bytes32 private constant DOMAIN_VERSION = keccak256("1");

    uint256 private constant BPS_DENOMINATOR = 10_000;

    /// @notice Covenant's secp256k1 issuer address (operator-custodied key).
    address public immutable TRUSTED_ATTESTOR;
    /// @notice The one USDC contract bonds are denominated in on this chain.
    address public immutable USDC;

    error UntrustedSigner();
    error Expired();
    error TokenMismatch();
    error ZeroBond();
    error SelfSlash();
    error SlashSplit();
    error WindowInverted();
    error ZeroField();
    error MalleableSignature();

    struct BondReceipt {
        bytes32 subject; // agent's Solana identity (ed25519/PDA) — the binding
        address bondToken;
        uint256 bondAmount;
        address agentReturn;
        address slashBeneficiary;
        uint256 slashBeneficiaryBps;
        bytes32 nonce;
        uint256 issuedAt;
        uint256 expiry;
    }

    constructor(address trustedAttestor, address usdc) {
        require(trustedAttestor != address(0) && usdc != address(0), "zero config");
        TRUSTED_ATTESTOR = trustedAttestor;
        USDC = usdc;
    }

    /// @dev Bound to `block.chainid`, so a receipt signed for another chain
    ///      cannot verify here. `verifyingContract` is intentionally omitted,
    ///      matching bond.rs — the receipt survives verifier upgrades.
    function domainSeparator() public view returns (bytes32) {
        return keccak256(abi.encode(DOMAIN_TYPEHASH, DOMAIN_NAME, DOMAIN_VERSION, block.chainid));
    }

    function digest(BondReceipt calldata r) public view returns (bytes32) {
        bytes32 structHash = keccak256(
            abi.encode(
                BOND_RECEIPT_TYPEHASH,
                r.subject,
                r.bondToken,
                r.bondAmount,
                r.agentReturn,
                r.slashBeneficiary,
                r.slashBeneficiaryBps,
                r.nonce,
                r.issuedAt,
                r.expiry
            )
        );
        return keccak256(abi.encodePacked(hex"1901", domainSeparator(), structHash));
    }

    /// @notice Reverts unless the receipt is a live, well-formed bond signed
    ///         by the trusted attestor. Same fail-closed checks as
    ///         `SignedBondReceipt::verify`, in the same order.
    function verify(BondReceipt calldata r, uint8 v, bytes32 sigR, bytes32 sigS) external view returns (bool) {
        if (r.bondToken != USDC) revert TokenMismatch();
        if (r.bondAmount == 0) revert ZeroBond();
        if (r.subject == bytes32(0)) revert ZeroField();
        if (r.agentReturn == address(0) || r.slashBeneficiary == address(0)) revert ZeroField();
        if (r.slashBeneficiary == r.agentReturn) revert SelfSlash();
        if (r.slashBeneficiaryBps == 0 || r.slashBeneficiaryBps >= BPS_DENOMINATOR) revert SlashSplit();
        if (r.expiry <= r.issuedAt) revert WindowInverted();
        if (block.timestamp > r.expiry) revert Expired();

        // Reject signature malleability: high-S and v outside {27,28}. Raw
        // `ecrecover` accepts both, but `bond.rs` only ever emits low-S with
        // v in {27,28}, so anything else is not a legitimate receipt. This
        // keeps the on-chain and off-chain accepted-signature sets identical.
        if (uint256(sigS) > 0x7FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF5D576E7357A4501DDFE92F46681B20A0) {
            revert MalleableSignature();
        }
        if (v != 27 && v != 28) revert MalleableSignature();

        address signer = ecrecover(digest(r), v, sigR, sigS);
        if (signer == address(0) || signer != TRUSTED_ATTESTOR) revert UntrustedSigner();
        return true;
    }
}
