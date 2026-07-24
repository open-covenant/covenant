// The paid /x402/verify handler, extracted from server.ts so tests can exercise
// it without the payment middleware (server.ts wires env + facilitator at import
// and exits on missing config). Returning >= 400 cancels settlement, so a
// malformed request is never charged.
//
// A buyer pays USDC and gets back a Covenant-signed statement that their BlockRun
// receipt is internally consistent: its digest, whether the stated verdict matches
// the requested/served pair, and whether it carries a settlement tx. The signed
// attestation verifies against the published pubkey with no trust in this server.
import type { Request, Response } from "express";
import type { Attestor } from "./attest.js";
import { looksLikeReceipt, verifyReceipt } from "./receipt.js";

export function makeVerifyHandler(attestor: Attestor) {
  return (req: Request, res: Response): void => {
    const body = (req.body ?? {}) as { receipt?: unknown; expectedDigest?: unknown };
    if (!looksLikeReceipt(body.receipt)) {
      res.status(400).json({
        error:
          "receipt is required and must carry verdict, modelRequested, modelServed, inputSha256, outputSha256",
      });
      return;
    }
    if (body.expectedDigest !== undefined && typeof body.expectedDigest !== "string") {
      res.status(400).json({ error: "expectedDigest, if given, must be a string" });
      return;
    }
    // A pathologically nested receipt can overflow the recursive canonicalizer.
    // Catch it as the contracted "malformed receipt is free" 400 rather than an
    // uncaught 500 (returning >= 400 cancels settlement either way).
    let result;
    try {
      result = verifyReceipt(body.receipt, body.expectedDigest);
    } catch {
      res.status(400).json({ error: "receipt could not be canonicalized" });
      return;
    }
    // Sign the verification over the receipt digest as subject, so the buyer can
    // pin the pubkey and check the statement independently.
    const attestation = attestor.attest(result.digest, result, Math.floor(Date.now() / 1000));
    res.json({ result, attestation });
  };
}
