# covenant-oracle

Runtime gating for MPL Core agent assets. An agent asset can only transfer (or,
extended the same way, execute) while its Covenant audit chain is valid and in
policy.

The program owns one oracle account per subject agent and exposes a single
authority-gated knob. Covenant pushes the live verdict; MPL Core enforces it.

## Model

MPL Core's Oracle external plugin reads an `OracleValidation` from an external
account during an asset's lifecycle events. Configure the asset's plugin with:

- `baseAddress` = this program's oracle PDA for the asset (`["oracle", asset]`)
- `resultsOffset` = `Anchor` (read at byte 8, after the account discriminator)
- `lifecycleChecks` = `{ transfer: [CanReject] }`

`OracleState.validation` is the first field, so MPL Core reads it at byte 8.
`valid()` returns `Pass` for every event (defers to the asset's normal owner
authority); `rejected()` returns `Rejected` for `transfer`, which makes MPL Core
veto the transfer (`custom program error 0x9`) until the verdict flips back.

## Instructions

| ix               | args             | effect                                              |
| ---------------- | ---------------- | --------------------------------------------------- |
| `init_oracle`    | `subject: Pubkey`| create the PDA, authority = signer, default valid   |
| `set_validation` | `valid: bool`    | authority-only; `false` -> transfer `Rejected`      |

## Deployed

- devnet: `2PJFAtPsVzgLrmvj2Hwx7x1DuUXSjgW44qSR35MZshaD`

## Build / test

```sh
anchor build -p covenant_oracle_program   # SBF + IDL
cargo test -p covenant-oracle-program     # byte-layout invariants
```

The byte layout is a hard contract with mpl-core 0.12. The unit tests assert the
exact serialized bytes; do not change the enum order or field order without
re-checking them.

## Demo

`demo/` mints a gated agent asset on devnet and proves the gate closes on an
invalid audit and opens on a valid one. See `demo/README.md`.
