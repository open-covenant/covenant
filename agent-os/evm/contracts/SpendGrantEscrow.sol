// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import {IERC20} from "./interfaces/IERC20.sol";
import {TokenTransfers} from "./lib/TokenTransfers.sol";

/// @title SpendGrantEscrow — bounded autonomous spend + spec-gated release.
/// @notice The onchain half of the Robinhood-Chain spend wedge. A funder parks
///         USDG behind a `Grant` that bounds what a delegated agent (`spender`)
///         may spend: a per-call ceiling, a total funded cap, an expiry, and a
///         provider allowlist. The agent transacts unattended — every bound is
///         a property of the contract that holds the money, not of a proxy the
///         agent could route around. A rogue or buggy agent physically cannot
///         exceed the ceiling, spend past expiry, or pay a provider the funder
///         never authorized: `chargeCall` reverts.
///
///         Funds for a call sit in a per-call hold. Release is either agent-
///         authorized on delivery (v1, when the grant carries no quality gate)
///         or gated on an attestor's EIP-712 `Quality` verdict (v2, "never pay
///         for junk" — money moves only when the output clears the bar). No
///         passing verdict by the call deadline and anyone may `refundCall` the
///         hold back to the grant. Both guarantees are enforced before payout.
///
/// @dev    Deployed on Robinhood Chain (Arbitrum L2); the live address is
///         recorded in `agent-os/evm/deployments.json`. The `attestor` role is
///         Covenant's daemon signing key: `covenantd/escrow.rs::prove_completion`
///         derives the pass/fail verdict and signs the `Quality` digest this
///         contract recovers. `scopeHash` binds a grant to the off-chain
///         `Capability{action:"wallet.spend.authorize"}` the daemon enforces —
///         the same bound expressed once, enforced twice.
///
///         Signature acceptance mirrors the other Covenant verifiers: low-S and
///         v in {27,28} only, so a consumer that dedups on the signature can
///         never see two valid forms of one verdict. Replay is closed by call
///         state — a callId settles once (Held → Released | Refunded is
///         one-way), so an attestation cannot be replayed against a paid call.
contract SpendGrantEscrow {
    using TokenTransfers for IERC20;

    uint256 private constant SECP256K1N_DIV_2 = 0x7FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF5D576E7357A4501DDFE92F46681B20A0;

    /// @notice Per-grant funding ceiling for the pilot (atomic USDG units).
    uint256 public constant MAX_ESCROW = 50_000_000;
    uint64 public constant MAX_GRANT_WINDOW = 90 days;
    uint64 public constant MAX_CALL_WINDOW = 7 days;
    uint256 public constant MAX_PROVIDERS = 32;
    uint16 public constant PLATFORM_FEE_BPS = 1_000;
    uint16 public constant BPS_DENOMINATOR = 10_000;

    bytes32 public constant DOMAIN_TYPEHASH =
        keccak256("EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)");
    /// @dev Field order is pinned in the Rust attestor's digest builder.
    bytes32 public constant QUALITY_TYPEHASH =
        keccak256("Quality(uint256 callId,bytes32 resultHash,bool passed,bytes32 specId,uint256 deadline)");
    bytes32 private constant DOMAIN_NAME = keccak256("Covenant SpendGrant");
    bytes32 private constant DOMAIN_VERSION = keccak256("1");

    enum GrantStatus {
        None,
        Active,
        Closed
    }

    enum CallStatus {
        None,
        Held,
        Released,
        Refunded
    }

    struct Grant {
        address funder;
        address spender;
        uint128 totalCap;
        uint128 balance;
        uint128 perCallCeiling;
        uint64 expiry;
        bytes32 scopeHash;
        bool requireQualityGate;
        GrantStatus status;
    }

    struct Call {
        uint256 grantId;
        address provider;
        uint128 amount;
        uint64 deadline;
        bytes32 resultHash;
        CallStatus status;
    }

    error AlreadyPaused();
    error CallExists();
    error Expired();
    error InvalidAddress();
    error InvalidAmount();
    error InvalidCall();
    error InvalidDeadline();
    error InvalidExpiry();
    error InvalidGrant();
    error InvalidProviders();
    error InvalidScope();
    error InvalidSignature();
    error InvalidState();
    error LimitExceeded();
    error NotDue();
    error NotFunder();
    error NotPaused();
    error NotSpender();
    error ProviderNotAllowed();
    error QualityGateRequired();
    error Reentrancy();
    error Unauthorized();

    event GrantCreated(
        uint256 indexed grantId,
        address indexed funder,
        address indexed spender,
        uint128 totalCap,
        uint128 perCallCeiling,
        uint64 expiry,
        bytes32 scopeHash,
        bool requireQualityGate
    );
    event GrantClosed(uint256 indexed grantId);
    event GrantWithdrawn(uint256 indexed grantId, uint256 amount);
    event CallCharged(
        uint256 indexed callId, uint256 indexed grantId, address indexed provider, uint128 amount, uint64 deadline
    );
    event CallReleased(
        uint256 indexed callId,
        uint256 indexed grantId,
        address indexed provider,
        uint256 providerPaid,
        uint256 fee,
        bytes32 resultHash,
        bool attested
    );
    event CallRefunded(uint256 indexed callId, uint256 indexed grantId, uint256 amount, bytes32 reasonHash);
    event AttestorSet(address indexed attestor);
    event GatewaySet(address indexed gateway);
    event Paused(address indexed by);
    event Unpaused(address indexed by);

    IERC20 public immutable usd;
    address public immutable admin;
    address public immutable treasury;
    address public immutable emergencyAdmin;
    address public attestor;
    address public gateway;
    bool public paused;
    uint256 public grantCount;
    uint256 private unlocked = 1;
    mapping(uint256 grantId => Grant grant) private grants;
    mapping(uint256 callId => Call call) private calls;
    mapping(uint256 grantId => mapping(address provider => bool allowed)) public allowedProvider;

    constructor(
        IERC20 usd_,
        address admin_,
        address attestor_,
        address treasury_,
        address emergencyAdmin_,
        address gateway_
    ) {
        if (
            address(usd_) == address(0) || admin_ == address(0) || attestor_ == address(0) || treasury_ == address(0)
                || emergencyAdmin_ == address(0) || gateway_ == address(0)
        ) revert InvalidAddress();
        usd = usd_;
        admin = admin_;
        attestor = attestor_;
        treasury = treasury_;
        emergencyAdmin = emergencyAdmin_;
        gateway = gateway_;
        paused = true;
    }

    modifier whenNotPaused() {
        if (paused) revert AlreadyPaused();
        _;
    }

    modifier nonReentrant() {
        if (unlocked != 1) revert Reentrancy();
        unlocked = 2;
        _;
        unlocked = 1;
    }

    /// @notice Fund a bounded spend grant. Pulls `totalCap` USDG from the caller
    ///         and authorizes `spender` to draw it down within the bounds.
    function createGrant(
        address spender,
        uint128 totalCap,
        uint128 perCallCeiling,
        uint64 expiry,
        address[] calldata providers,
        bytes32 scopeHash,
        bool requireQualityGate
    ) external whenNotPaused nonReentrant returns (uint256 grantId) {
        if (spender == address(0)) revert InvalidAddress();
        if (totalCap == 0 || totalCap > MAX_ESCROW) revert LimitExceeded();
        if (perCallCeiling == 0 || perCallCeiling > totalCap) revert InvalidAmount();
        if (expiry <= block.timestamp || expiry > block.timestamp + MAX_GRANT_WINDOW) {
            revert InvalidExpiry();
        }
        if (providers.length == 0 || providers.length > MAX_PROVIDERS) revert InvalidProviders();
        if (scopeHash == bytes32(0)) revert InvalidScope();

        grantId = ++grantCount;
        for (uint256 i = 0; i < providers.length; ++i) {
            address provider = providers[i];
            if (provider == address(0) || allowedProvider[grantId][provider]) {
                revert InvalidProviders();
            }
            allowedProvider[grantId][provider] = true;
        }

        grants[grantId] = Grant({
            funder: msg.sender,
            spender: spender,
            totalCap: totalCap,
            balance: totalCap,
            perCallCeiling: perCallCeiling,
            expiry: expiry,
            scopeHash: scopeHash,
            requireQualityGate: requireQualityGate,
            status: GrantStatus.Active
        });
        usd.pull(msg.sender, totalCap);
        emit GrantCreated(grantId, msg.sender, spender, totalCap, perCallCeiling, expiry, scopeHash, requireQualityGate);
    }

    /// @notice Draw a per-call hold from a grant. Reverts unless the caller is
    ///         the grant's spender and the spend is within every bound — the
    ///         unbypassable "bounded autonomous spend" guarantee.
    function chargeCall(uint256 grantId, address provider, uint128 amount, uint256 callId, uint64 deadline)
        external
        whenNotPaused
        nonReentrant
    {
        Grant storage grant = _grant(grantId);
        if (grant.status != GrantStatus.Active) revert InvalidState();
        if (msg.sender != grant.spender) revert NotSpender();
        if (block.timestamp >= grant.expiry) revert Expired();
        if (!allowedProvider[grantId][provider]) revert ProviderNotAllowed();
        if (amount == 0 || amount > grant.perCallCeiling || amount > grant.balance) {
            revert InvalidAmount();
        }
        if (deadline <= block.timestamp || deadline > block.timestamp + MAX_CALL_WINDOW) {
            revert InvalidDeadline();
        }
        if (callId == 0) revert InvalidCall();
        if (calls[callId].status != CallStatus.None) revert CallExists();

        grant.balance -= amount;
        calls[callId] = Call({
            grantId: grantId,
            provider: provider,
            amount: amount,
            deadline: deadline,
            resultHash: bytes32(0),
            status: CallStatus.Held
        });
        emit CallCharged(callId, grantId, provider, amount, deadline);
    }

    /// @notice Release a held call to its provider on delivery (v1). Only the
    ///         grant's spender or the gateway may call, and only when the grant
    ///         carries no quality gate — a gated grant must use the attested
    ///         path so money never moves without a passing verdict.
    function releaseCall(uint256 callId, bytes32 resultHash) external nonReentrant {
        Call storage call_ = _call(callId);
        if (call_.status != CallStatus.Held) revert InvalidState();
        Grant storage grant = grants[call_.grantId];
        if (grant.requireQualityGate) revert QualityGateRequired();
        if (msg.sender != grant.spender && msg.sender != gateway) revert Unauthorized();
        _payout(callId, call_, resultHash, false);
    }

    /// @notice Release a held call against an attestor `Quality` verdict (v2).
    ///         Pays the provider only if `attestor` signed `passed = true` over
    ///         this exact call and result — "never pay for junk," enforced.
    /// @dev    Payout is barred once the call deadline passes: after that the
    ///         hold can only refund, so the funder's time exposure is bounded
    ///         regardless of when an attestation lands.
    function releaseCallAttested(
        uint256 callId,
        bytes32 resultHash,
        bytes32 specId,
        uint256 deadline,
        bytes calldata signature
    ) external nonReentrant {
        Call storage call_ = _call(callId);
        if (call_.status != CallStatus.Held) revert InvalidState();
        if (block.timestamp > call_.deadline || block.timestamp > deadline) revert Expired();
        if (!_verifyQuality(callId, resultHash, true, specId, deadline, signature)) {
            revert InvalidSignature();
        }
        _payout(callId, call_, resultHash, true);
    }

    /// @notice Refund a held call immediately on an attestor `passed = false`
    ///         verdict — the junk path settles without waiting out the deadline.
    function refundCallAttested(
        uint256 callId,
        bytes32 resultHash,
        bytes32 specId,
        uint256 deadline,
        bytes calldata signature
    ) external nonReentrant {
        Call storage call_ = _call(callId);
        if (call_.status != CallStatus.Held) revert InvalidState();
        if (block.timestamp > deadline) revert Expired();
        if (!_verifyQuality(callId, resultHash, false, specId, deadline, signature)) {
            revert InvalidSignature();
        }
        _refund(callId, call_, resultHash);
    }

    /// @notice Return a held call to its grant after the call deadline passes.
    ///         Permissionless: any keeper can unwind an unreleased hold so the
    ///         funder's capital is never stranded by a silent provider.
    function refundCall(uint256 callId, bytes32 reasonHash) external nonReentrant {
        Call storage call_ = _call(callId);
        if (call_.status != CallStatus.Held) revert InvalidState();
        if (block.timestamp <= call_.deadline) revert NotDue();
        _refund(callId, call_, reasonHash);
    }

    /// @notice Emergency unwind of a stuck hold back to its grant, regardless of
    ///         deadline. Scoped to one call — never a global drain.
    function adminClose(uint256 callId, bytes32 reasonHash) external nonReentrant {
        if (msg.sender != emergencyAdmin) revert Unauthorized();
        Call storage call_ = _call(callId);
        if (call_.status != CallStatus.Held) revert InvalidState();
        _refund(callId, call_, reasonHash);
    }

    /// @notice Withdraw unlocked grant balance back to the funder. Touches only
    ///         funds not held by an in-flight call, so a running agent's holds
    ///         are never clawed out from under it.
    function withdrawUnused(uint256 grantId, uint128 amount) external nonReentrant {
        Grant storage grant = _grant(grantId);
        if (msg.sender != grant.funder) revert NotFunder();
        if (amount == 0 || amount > grant.balance) revert InvalidAmount();
        grant.balance -= amount;
        usd.push(grant.funder, amount);
        emit GrantWithdrawn(grantId, amount);
    }

    /// @notice Stop new charges against a grant. Existing holds still release or
    ///         refund; the funder can `withdrawUnused` the remaining balance.
    function closeGrant(uint256 grantId) external {
        Grant storage grant = _grant(grantId);
        if (msg.sender != grant.funder) revert NotFunder();
        if (grant.status != GrantStatus.Active) revert InvalidState();
        grant.status = GrantStatus.Closed;
        emit GrantClosed(grantId);
    }

    function pause() external {
        if (msg.sender != emergencyAdmin && msg.sender != admin) revert Unauthorized();
        if (paused) revert AlreadyPaused();
        paused = true;
        emit Paused(msg.sender);
    }

    function unpause() external {
        if (msg.sender != admin) revert Unauthorized();
        if (!paused) revert NotPaused();
        paused = false;
        emit Unpaused(msg.sender);
    }

    function setAttestor(address attestor_) external {
        if (msg.sender != admin) revert Unauthorized();
        if (attestor_ == address(0)) revert InvalidAddress();
        attestor = attestor_;
        emit AttestorSet(attestor_);
    }

    function setGateway(address gateway_) external {
        if (msg.sender != admin) revert Unauthorized();
        if (gateway_ == address(0)) revert InvalidAddress();
        gateway = gateway_;
        emit GatewaySet(gateway_);
    }

    function getGrant(uint256 grantId) external view returns (Grant memory) {
        return _grant(grantId);
    }

    function getCall(uint256 callId) external view returns (Call memory) {
        return _call(callId);
    }

    function domainSeparator() external view returns (bytes32) {
        return _domainSeparator();
    }

    function _payout(uint256 callId, Call storage call_, bytes32 resultHash, bool attested) private {
        uint256 amount = call_.amount;
        uint256 fee = amount * PLATFORM_FEE_BPS / BPS_DENOMINATOR;
        uint256 providerPaid = amount - fee;
        address provider = call_.provider;

        call_.status = CallStatus.Released;
        call_.resultHash = resultHash;
        if (providerPaid != 0) usd.push(provider, providerPaid);
        if (fee != 0) usd.push(treasury, fee);
        emit CallReleased(callId, call_.grantId, provider, providerPaid, fee, resultHash, attested);
    }

    function _refund(uint256 callId, Call storage call_, bytes32 reasonHash) private {
        uint256 grantId = call_.grantId;
        uint128 amount = call_.amount;
        call_.status = CallStatus.Refunded;
        grants[grantId].balance += amount;
        emit CallRefunded(callId, grantId, amount, reasonHash);
    }

    function _verifyQuality(
        uint256 callId,
        bytes32 resultHash,
        bool passed,
        bytes32 specId,
        uint256 deadline,
        bytes calldata signature
    ) private view returns (bool) {
        bytes32 digest = _hashTypedData(
            keccak256(abi.encode(QUALITY_TYPEHASH, callId, resultHash, passed, specId, deadline))
        );
        address signer = _recover(digest, signature);
        return signer != address(0) && signer == attestor;
    }

    function _grant(uint256 grantId) private view returns (Grant storage grant) {
        grant = grants[grantId];
        if (grant.status == GrantStatus.None) revert InvalidGrant();
    }

    function _call(uint256 callId) private view returns (Call storage call_) {
        call_ = calls[callId];
        if (call_.status == CallStatus.None) revert InvalidCall();
    }

    function _hashTypedData(bytes32 structHash) private view returns (bytes32) {
        return keccak256(abi.encodePacked("\x19\x01", _domainSeparator(), structHash));
    }

    function _domainSeparator() private view returns (bytes32) {
        return keccak256(abi.encode(DOMAIN_TYPEHASH, DOMAIN_NAME, DOMAIN_VERSION, block.chainid, address(this)));
    }

    function _recover(bytes32 digest, bytes calldata signature) private pure returns (address signer) {
        if (signature.length != 65) return address(0);
        bytes32 r;
        bytes32 s;
        uint8 v;
        assembly ("memory-safe") {
            r := calldataload(signature.offset)
            s := calldataload(add(signature.offset, 32))
            v := byte(0, calldataload(add(signature.offset, 64)))
        }
        if (v < 27) v += 27;
        if ((v != 27 && v != 28) || uint256(s) > SECP256K1N_DIV_2) return address(0);
        signer = ecrecover(digest, v, r, s);
    }
}
