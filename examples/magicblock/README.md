# Covenant × MagicBlock demo

A read-only mainnet demo of the Covenant trust layer over MagicBlock. It takes no
keys and moves no funds. It reads public on-chain state and reproduces two claims
from the [integration guide](../../docs/magicblock-integration.md):

1. **Which MagicBlock ERs are Covenant-verified.** Queries the Magic Router for the
   live set of ERs, then resolves each one against its Covenant attestation in the
   Solana Attestation Service. The TEE ER (`mainnet-tee`) comes back verified.
2. **That an agent's on-chain record is its real work.** Takes the five answers the
   Covenant demo agent gave (`work-items.json`), hashes each into a receipt, folds
   them into a provenance root, and checks it against the root stored on chain. They
   match, so the on-chain root is the hash-chain of the actual answers.

## Run

```
npm install
node verify.mjs
```

If the default public RPC rate-limits, pass your own:

```
RPC=https://your-mainnet-rpc node verify.mjs
```

## Expected output

```
1. Which MagicBlock ERs are Covenant-verified?
   unverified  https://as.magicblock.app/  MAS1Dt9q...
   unverified  https://eu.magicblock.app/  MEUGGrYP...
   verified    https://mainnet-tee.magicblock.app/  MTEWGuqx...
   unverified  https://us.magicblock.app/  MUS3hc9T...

2. Is the reference agent's on-chain record its real work?
   recomputed from 5 answers : 2769ee46c8c7dc49...
   on-chain provenance_root  : 2769ee46c8c7dc49...
   match, the on-chain root is the hash-chain of the agent's real answers
```

Everything the demo reads is public: the settlement program
`cov9UDypG7nsryxdgMcKhKU2spRVWLVjxT2iTv6do5Y` (OtterSec-verified), the SAS
attestation, and the agent's credit account. Change `work-items.json` and the
recomputed root stops matching, which is the point.
