// SPDX-License-Identifier: MIT
pragma solidity ^0.8.19;

/// @dev ENSIP-10 wildcard resolver entry point.
interface IExtendedResolver {
    function resolve(bytes memory name, bytes memory data) external view returns (bytes memory);
}

/// @dev The call the gateway answers off chain. Its selector (0x9061b923) is the
/// same `resolve(bytes,bytes)` the resolver re-encodes into its OffchainLookup.
interface IResolverService {
    function resolve(bytes calldata name, bytes calldata data)
        external
        view
        returns (bytes memory result, uint64 expires, bytes memory sig);
}

/// An off-chain ENS resolver (ENSIP-10 + EIP-3668) for `*.opencovenant.eth`.
///
/// `resolve` cannot hold a Solana address on L1 cheaply, so it reverts
/// `OffchainLookup` and defers to a gateway. The gateway reads the address out
/// of the Solana MPL Agent registry / SAS, signs it, and `resolveWithProof`
/// recovers the signer and checks it against `signers`.
///
/// The signature scheme is EIP-191 version 0x00 (intended-validator), pinned to
/// ensdomains/offchain-resolver commit 099b7e98, and matches the gateway signing
/// core in covenant-evm-signer (resolver.rs): a golden response produced there
/// verifies here. Malleable signatures are rejected so the on-chain and off-chain
/// accepted sets stay identical.
contract OffchainResolver is IExtendedResolver {
    string public url;
    mapping(address => bool) public signers;
    address public owner;

    event NewSigners(address[] signers);
    event NewOwner(address owner);

    error OffchainLookup(
        address sender,
        string[] urls,
        bytes callData,
        bytes4 callbackFunction,
        bytes extraData
    );
    error SignatureExpired();
    error MalleableSignature();
    error UntrustedSigner();
    error NotOwner();

    constructor(string memory _url, address[] memory _signers) {
        url = _url;
        owner = msg.sender;
        for (uint256 i = 0; i < _signers.length; i++) {
            signers[_signers[i]] = true;
        }
        emit NewSigners(_signers);
        emit NewOwner(msg.sender);
    }

    modifier onlyOwner() {
        if (msg.sender != owner) revert NotOwner();
        _;
    }

    function setSigner(address signer, bool allowed) external onlyOwner {
        signers[signer] = allowed;
    }

    function setUrl(string calldata _url) external onlyOwner {
        url = _url;
    }

    function transferOwnership(address newOwner) external onlyOwner {
        owner = newOwner;
        emit NewOwner(newOwner);
    }

    function makeSignatureHash(address target, uint64 expires, bytes memory request, bytes memory result)
        public
        pure
        returns (bytes32)
    {
        return keccak256(abi.encodePacked(hex"1900", target, expires, keccak256(request), keccak256(result)));
    }

    function resolve(bytes calldata name, bytes calldata data)
        external
        view
        override
        returns (bytes memory)
    {
        bytes memory callData = abi.encodeWithSelector(IResolverService.resolve.selector, name, data);
        string[] memory urls = new string[](1);
        urls[0] = url;
        revert OffchainLookup(
            address(this),
            urls,
            callData,
            OffchainResolver.resolveWithProof.selector,
            callData
        );
    }

    /// EIP-3668 callback. `extraData` is the original request the gateway signed;
    /// `response` is `abi.encode(bytes result, uint64 expires, bytes sig)`.
    function resolveWithProof(bytes calldata response, bytes calldata extraData)
        external
        view
        returns (bytes memory)
    {
        (bytes memory result, uint64 expires, bytes memory sig) =
            abi.decode(response, (bytes, uint64, bytes));
        if (expires < block.timestamp) revert SignatureExpired();
        address signer = recover(makeSignatureHash(address(this), expires, extraData, result), sig);
        if (!signers[signer]) revert UntrustedSigner();
        return result;
    }

    function recover(bytes32 hash, bytes memory sig) internal pure returns (address) {
        if (sig.length != 65) revert MalleableSignature();
        bytes32 r;
        bytes32 s;
        uint8 v;
        assembly {
            r := mload(add(sig, 0x20))
            s := mload(add(sig, 0x40))
            v := byte(0, mload(add(sig, 0x60)))
        }
        if (uint256(s) > 0x7FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF5D576E7357A4501DDFE92F46681B20A0) {
            revert MalleableSignature();
        }
        if (v != 27 && v != 28) revert MalleableSignature();
        address signer = ecrecover(hash, v, r, s);
        if (signer == address(0)) revert UntrustedSigner();
        return signer;
    }

    function supportsInterface(bytes4 interfaceId) external pure returns (bool) {
        return interfaceId == type(IExtendedResolver).interfaceId || interfaceId == 0x01ffc9a7;
    }
}
