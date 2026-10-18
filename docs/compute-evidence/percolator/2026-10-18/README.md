# Percolator work through Covenant Compute

This record connects actual Covenant jobs to the code and proof results they produced. The archives contain exact signed orders and operator receipts, source objects, returned artifacts and an offline verifier. Each archive directory is named by the SHA-256 of its `inventory.json`.

## Completed work

| Work | Receipt evidence | Scope |
| --- | --- | --- |
| Kani runner component authored through Covenant | [Authorship and linked checker](9166c9ac0f89d1d4f7761cf3474d59e929a048764fff8d007f1f8946806c5fcf/README.md) | Accepted runner patch and 39 fixture tests; later local integration changes are recorded separately. |
| Native check and Kani admission canaries | [Canary receipts](d1c6dc6b0da9ce9995caf04c69e8824dbc9872c21b53aeaf477dc315f943922d/README.md) | A generic source check and a tiny Kani proof validate the receipt path. |
| Percolator #241 public withdrawal at and after expiry | [Both accepted proof receipts](bbd7a6c440e5f8ba7cd8e1e7232eb2275fe206100d842a175da7f11f94f61ca2/README.md) | Existing source `035d4cb25eff1fce25d0a385ab92b700d8da3469`; both bounded properties proven and required covers reached. Verification only: original fix and harnesses were authored earlier. |

The #241 orders are `b7a20990-a245-486e-ac4b-44d1331c272a` and `0425f25b-83bc-45ee-8fb9-cdcbc94d7b09`, each offering 50,000 micro-USDC. Both request the immutable Kani image and 27,648 MiB / 6 CPU / 8,192 MiB scratch profile recorded in their signed plans. Each fixes expiry at slot 10 and current slot at 10 or 11, with backing 1 through 8, withdrawal 1 through backing, and claim 0 through backing minus withdrawal.

## Failed executions retained

The [first #241 attempt](e45e484093bd11eda6784f3b13b73b4d352bdf77263a9512f457ea25cface48d/README.md) exposed a non-executable scratch mount. The [second attempt](97e7fd29b7390e08298e6ec24d86e4e6ef4e3cb93f1dd189835d21fd41d3196d/README.md) exposed a missing required test feature. Both produced signed `tool_error` results, not proofs. The corrected executor and explicit `fuzz` plan were used by the distinct successful jobs above; the failure records remain unchanged.

## Verification and provenance

Follow each archive's README to build the pinned verifier and check its inventory, signatures, source mapping and complete raw Kani evidence. Archive collection-state labels describe the preserved snapshot; this repository commit supplies its publication location. The verifier does not purchase compute. An optional paid rerun requires the matching image and resource capability; a smaller provider profile cannot silently substitute for it.

The native execution path is implemented in Covenant PRs [#322](https://github.com/open-covenant/covenant/pull/322), [#328](https://github.com/open-covenant/covenant/pull/328) and [#331](https://github.com/open-covenant/covenant/pull/331). The pilot node runs `4dd036a4d59612326095768ff4b4f272aa48562d`; the compatible coordinator runs `983172ddd04ac2de70136125e2088f22c6edf505`. See the proof archive for actual CLI/verifier build hashes and scoped source equivalence. The older public `compute-v0.2.0` download lacks these native commands; the archive documents the pinned source build.

Operator signatures authenticate the bound claims and artifacts. They do not independently establish hardware isolation, a complete coordinator job inventory, or blockchain settlement. A proven result covers its selected harness and bounds. Authorship, ordinary acceptance, Kani verification, deployment and publication are recorded as separate facts.
