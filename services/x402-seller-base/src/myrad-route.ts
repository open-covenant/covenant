// Paid Myrad signal handler, extracted from server.ts so tests can exercise it
// without the payment middleware. Reached only after a verified payment.
//
// The endpoint sells a cohort signal together with the Covenant receipt issued
// over it. It does not issue: the attestor key lives with the issuer (the
// covenant-myrad crate or covenantd), and this process only serves what it was
// handed. A buyer verifies against a pinned key, so a compromised endpoint
// cannot forge a receipt.
//
// Bundles come from COVENANT_MYRAD_BUNDLE (a file emitted by
// `cargo run -p covenant-myrad --example emit_bundle -- bundle.json`). Unset,
// the route answers 503 with the placeholder cohort named: the path stays
// exercisable end to end, and >= 400 cancels settlement so nobody is charged for
// a body with no receipt in it. No contributor data ships in this repository
// either way.
import { readFileSync } from "node:fs";
import type { Request, Response } from "express";

export const SIGNAL_SCHEMA = "covenant.myrad.signal.v1";
export const BUNDLE_RESOURCE = "covenant.myrad.signal.bundle.v1";

type Bundle = {
  resource?: string;
  signal?: { cohort_id?: unknown };
  receipt?: { receipt?: { signal?: { cohort_id?: unknown }; integrity?: { status?: unknown } } };
};

const SYNTHETIC_NOTE =
  "synthetic reference cohort: no contributor data. Set COVENANT_MYRAD_BUNDLE to serve issued bundles.";

// Kept deliberately small. It exists so the route answers before Myrad's source
// is connected, not to stand in for one.
const SYNTHETIC: Bundle = {
  resource: BUNDLE_RESOURCE,
  signal: { cohort_id: "reference_cohort" },
};

function cohortOf(bundle: Bundle): string | null {
  const fromSignal = bundle.signal?.cohort_id;
  if (typeof fromSignal === "string") return fromSignal;
  const fromReceipt = bundle.receipt?.receipt?.signal?.cohort_id;
  return typeof fromReceipt === "string" ? fromReceipt : null;
}

/** Read the configured bundles, keyed by cohort. Throws on an unreadable or malformed file. */
export function loadBundles(path: string | undefined): Map<string, Bundle> {
  if (!path) return new Map([["reference_cohort", SYNTHETIC]]);

  const parsed: unknown = JSON.parse(readFileSync(path, "utf8"));
  const list: Bundle[] = Array.isArray(parsed) ? (parsed as Bundle[]) : [parsed as Bundle];
  const bundles = new Map<string, Bundle>();
  for (const bundle of list) {
    const cohort = cohortOf(bundle);
    if (!cohort) throw new Error(`bundle in ${path} carries no cohort_id`);
    // Last-wins would quietly sell one of two bundles claiming the same cohort.
    if (bundles.has(cohort)) throw new Error(`${path} has two bundles for cohort ${cohort}`);
    bundles.set(cohort, bundle);
  }
  if (bundles.size === 0) throw new Error(`${path} contains no bundles`);
  return bundles;
}

export function makeSignalHandler(bundles: Map<string, Bundle>, synthetic: boolean) {
  return (req: Request, res: Response): void => {
    // The placeholder carries no receipt, so it is not the product the route
    // advertises and must never settle a payment.
    if (synthetic) {
      res.status(503).json({ error: "no issued bundle configured", note: SYNTHETIC_NOTE });
      return;
    }
    const bundle = bundles.get(req.params.cohort);
    // 404 cancels settlement, so asking for a cohort that isn't offered is free.
    // The catalog is not echoed back: enumerating it would be free too.
    if (!bundle) {
      res.status(404).json({ error: "unknown cohort" });
      return;
    }
    res.json(bundle);
  };
}
