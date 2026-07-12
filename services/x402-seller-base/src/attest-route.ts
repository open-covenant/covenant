// The paid /x402/attest handler, extracted from server.ts so tests can exercise
// validation and issuance directly: server.ts wires env, the facilitator, and the
// payment middleware at import time (and exits on missing config), so importing it
// in a test is not an option. Behavior must stay byte-identical to what the
// middleware forwards to after a verified payment — returning >= 400 here cancels
// settlement, so a rejected request is never charged.
import type { Request, Response } from "express";
import type { Attestor } from "./attest.js";

export function makeAttestHandler(attestor: Attestor) {
  return (req: Request, res: Response): void => {
    const { subject, claim } = (req.body ?? {}) as { subject?: unknown; claim?: unknown };
    if (typeof subject !== "string" || !subject || subject.length > 256 || claim === undefined) {
      res.status(400).json({ error: "subject (1-256 char string) and claim are required" });
      return;
    }
    res.json(attestor.attest(subject, claim, Math.floor(Date.now() / 1000)));
  };
}
