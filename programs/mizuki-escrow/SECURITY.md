# Security model

## Assets and trust

The vault holds native SOL principal for one bounty. The configured external policy signer controls the authority key and is the only actor allowed to bind or resolve. GitHub identity, wallet proof, review, and merge evidence are verified off-chain and committed on-chain as hashes.

The program assumes an attacker controls every submitted instruction byte, account, account order, transaction ordering, and non-authority signature. It does not assume RPC responses, transaction submission, or an HTTP response means finality.

## Enforced invariants

- State, vault, and guard are canonical PDAs with canonical bumps.
- System-owned, data-empty PDAs remain initializable after third-party prefunding; occupied accounts are rejected.
- The authority is an immutable system wallet stored at funding and must sign every mutation.
- `fund` creates an unbound, fully funded vault before publication.
- `bind` moves `Funded -> Bound` once and stores an immutable non-zero claimant and proof commitment.
- Release pays exactly the stored principal to exactly the stored claimant.
- Refund pays exactly the stored principal to exactly the stored authority.
- Release is valid only before claim expiry; bound refund is valid only at or after it.
- The full state commitment in the guard must match before each mutation.
- Terminal resolution updates the guard before closing state and vault; transaction atomicity rolls all changes back on any failure.
- The permanent terminal guard makes a replayed fund, bind, release, refund, revival, or second claimant fail.
- Extra instruction accounts and trailing instruction bytes are rejected.
- Vault donations cannot block payout or increase claimant principal. Any surplus returns to the authority when the vault closes.

## Intentionally absent paths

There is no claimant instruction, timeout claim, dispute resolver, partial withdrawal, arbitrary destination, mutable authority, configuration PDA, emergency drain, state close entrypoint, arbitrary CPI, token program, or program-owned upgrade instruction.

## External signer requirements

The signer must:

- Load and test against the committed ABI file rather than duplicate account ordering by hand.
- Allowlist one finalized program ID, the immutable program-data state, and the executable hash.
- Build all instruction data itself; never accept serialized transactions or arbitrary accounts from the API caller.
- Verify GitHub identity and wallet proof before bind.
- Verify the independent review and finalized merge before release.
- Reject release when either request time or GitHub `merged_at` is at or after `claim_expires_at`.
- Persist exact signed bytes before broadcast and reconcile through finalized status on two independent RPC providers.
- Serialize resolution attempts per bounty. The on-chain time boundary prevents release/refund overlap, but the signer should still avoid wasteful races.

## Remaining risks

- The authority can choose not to release before expiry. This is a policy-signer and operational availability risk, not a claimant escape hatch.
- The permanent guard locks its rent forever by design. Removing it would reopen replay and rebind paths.
- Native SOL is the only supported asset. Token support would add mint, account, and token-program substitution risks and is out of scope.
- The program has not received an independent third-party audit. Devnet adversarial canaries and immutable deployment verification remain release gates.
