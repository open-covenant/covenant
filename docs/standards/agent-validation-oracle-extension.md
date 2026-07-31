# Optional Oracle Enforcement Extension for Agent Validation Records

Status: Experimental, non-normative extension.

This extension is separate from the Agent Validation Record v1 proposal. A
validation record is passive evidence. Oracle enforcement changes the behavior
of an MPL Core asset and introduces an additional program, authority, upgrade,
availability, and recovery trust boundary. A record can conform without an
Oracle plugin, and an Oracle-gated asset does not become a conforming record
merely because it is gated.

## Purpose

An application can read a validation record and decide whether to act. When an
agent identity should also be constrained at the asset layer, an MPL Core
Oracle external plugin can reject selected Core lifecycle events according to
an external on-chain account.

This extension does not gate agent execution, wallet signing, network calls, or
program invocations. Those are not MPL Core lifecycle events. An application
must check its own policy before performing them.

## Reference layout

The reference derives one oracle account per agent:

```text
PDA(["oracle", agent_asset], expected_oracle_program)
```

The agent's Oracle external plugin pins:

- `base_address` to that PDA;
- `results_offset` to `ValidationResultsOffset::Anchor`; and
- explicit lifecycle checks, currently `transfer: [CanReject]`.

The account bytes used by the reference are:

```text
[0..8)  Anchor account discriminator
[8]     OracleValidation tag (1 = V1)
[9]     create   ExternalValidationResult
[10]    transfer ExternalValidationResult
[11]    burn     ExternalValidationResult
[12]    update   ExternalValidationResult
[13..45) authority Pubkey
[45..77) subject   Pubkey
[77]     canonical PDA bump

ExternalValidationResult:
  Approved = 0
  Rejected = 1
  Pass     = 2
```

For the reference transfer policy:

- `Pass` means the oracle abstains and Core continues with its ordinary
  authority checks.
- `Rejected` vetoes the transfer.

## Verification

A verifier of this extension MUST:

1. pin the expected oracle program ID;
2. derive the oracle PDA and canonical bump from the expected seeds and agent
   asset;
3. require the account owner to equal the expected oracle program;
4. require the exact 78-byte account length, then decode and validate the
   account discriminator, OracleValidation tag, and each result enum;
5. require the stored subject to equal the expected agent asset;
6. require the stored bump to equal the derived canonical bump;
7. require the stored authority to equal a verdict authority accepted by local
   policy, including any authority-rotation or governance policy;
8. require the agent's Oracle adapter `base_address` to equal the derived PDA;
9. compare the exact lifecycle-check configuration with local policy; and
10. assess the program's current upgrade authority, deployed bytes, and source
    verification independently of the record's AppData authority.

Reading byte 10 without the owner, PDA, discriminator, stored authority,
subject, bump, adapter, and program checks is not a verification.

## Security and recovery

- An upgrade authority can change the meaning of the oracle account after a
  source verification. Consumers that require immutable enforcement must
  require an immutable program or an explicitly accepted upgrade-governance
  policy.
- A compromised verdict authority can lock or unlock the gated lifecycle
  events. The authority, rotation process, and emergency recovery path must be
  documented before attaching the plugin.
- A missing, stale, malformed, closed, or unavailable oracle account can make
  an agent asset unusable. Implementations need a tested recovery procedure.
- Incorrect offsets or result ordering can gate the wrong event. Verifiers
  must decode the complete typed layout rather than inspect one byte in
  isolation.
- Gating transfer can strand ownership. Gating update or burn can also prevent
  remediation. Each added lifecycle check requires a separate threat review.
- PDA pre-funding or malformed accounts at the expected address must not be
  accepted as initialized state. The initializer and verifier must require the
  correct owner, discriminator, seeds, and canonical bump.

## Live reference

The Covenant reference currently exposes:

| Item           | Address                                        |
| -------------- | ---------------------------------------------- |
| Agent asset    | `4XtUrwvPWAzMGnsKenMpTMATXN3e2quJV11Jg2dab2dc` |
| Oracle program | `2PJFAtPsVzgLrmvj2Hwx7x1DuUXSjgW44qSR35MZshaD` |
| Oracle account | `4iQbGGLyLXed6aoKfrPAPUd7wxHaS3SPCUURVb3gUho3` |

The deployed program bytes were
[source-verified at commit `0afaf437e34f6f20d52835eb1927b4844b59768e`](https://github.com/open-covenant/covenant/tree/0afaf437e34f6f20d52835eb1927b4844b59768e).
The [verification service](https://verify.osec.io/status/2PJFAtPsVzgLrmvj2Hwx7x1DuUXSjgW44qSR35MZshaD)
also reports that the program is not frozen, so source verification does not
remove the upgrade-authority risk.

The verified source also documents an unresolved initialization risk:
`init_oracle` derives `["oracle", subject]`, but it does not require the
subject asset's owner, update authority, or another subject-bound authority to
authorize initialization. The first signer can therefore initialize an
unclaimed subject PDA and become its verdict authority. The existing reference
wiring checks the already-created account's expected authority, but that does
not make the initializer safe for arbitrary new agents. A production extension
MUST close this PDA-squatting path before general deployment.

The current `set_authority` instruction also accepts any public key without a
non-zero check. Setting the default public key would leave no usable signer and
can make verdict updates unrecoverable without a program upgrade. Production
operators and any successor implementation must reject unusable authority
values and test recovery before attaching lifecycle gates.
