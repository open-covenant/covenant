// Live check: does a Covenant-bound challenge survive into a real TDX quote?
//
// The binding is only useful if the enclave treats it the same as the random
// 64 bytes the ER verifier sends today. So: ask the live mainnet TEE ER for a
// quote using a challenge derived from an agent and a subject commitment, then
// look for those exact bytes in the quote it returns.
//
// No DCAP verification here on purpose. Whether the quote is genuine is
// already answered by services/er-registry/tee.mjs against the Intel PCCS.
// The open question is whether report_data carries our binding, and that is
// answerable from the raw bytes.
import crypto from "node:crypto";

const RPC = process.env.ER_RPC || "https://mainnet-tee.magicblock.app/";
const DOMAIN = "covenant.tee.challenge.v1";

// Must stay byte-identical to Binding::challenge in covenant-tee.
function challenge(agent, subjectCommitment, nonce) {
  const h = crypto.createHash("sha512");
  h.update(Buffer.from(DOMAIN, "utf8"));
  h.update(Buffer.from("|", "utf8"));
  h.update(Buffer.from(agent, "utf8"));
  h.update(Buffer.from("|", "utf8"));
  h.update(Buffer.from(subjectCommitment, "utf8"));
  h.update(Buffer.from("|", "utf8"));
  h.update(Buffer.from(nonce, "utf8"));
  return h.digest();
}

// Pin the JS construction to the same vector covenant-tee's Rust test pins. A
// change to the domain, a separator, the field order, or the encoding here
// fails fast instead of silently producing quotes that no longer bind.
const PINNED =
  "fadd468d8ba05f91c1630cb81d0761cbe39ac21269d7366a992d1bf53a36502b" +
  "48986c5ea9a963f867f953784fd0f9a424339c92e53306fffb0018a85a26c8aa";
if (challenge("Ep7dD7biX7rZ6NSVzy8uEpgEEYipVfQ8ofwHzZmRM8dF", "a".repeat(64), "b".repeat(64)).toString("hex") !== PINNED) {
  throw new Error("challenge construction drifted from covenant-tee");
}

async function quoteFor(challengeBytes) {
  const url = new URL(
    `quote?challenge=${encodeURIComponent(challengeBytes.toString("base64"))}`,
    RPC,
  ).toString();
  const r = await fetch(url, { signal: AbortSignal.timeout(20_000) });
  if (!r.ok) throw new Error(`${url.split("?")[0]} -> ${r.status}`);
  const body = await r.json();
  if (!body.quote) throw new Error("no quote in response");
  return Buffer.from(body.quote, "base64");
}

const agent = "Ep7dD7biX7rZ6NSVzy8uEpgEEYipVfQ8ofwHzZmRM8dF";
const subject = "a".repeat(64);
const nonce = crypto.randomBytes(32).toString("hex");

const bound = challenge(agent, subject, nonce);
console.log(`agent    ${agent}`);
console.log(`subject  ${subject}`);
console.log(`nonce    ${nonce}`);
console.log(`challenge ${bound.toString("hex")}`);
console.log(`rpc      ${RPC}\n`);

const quote = await quoteFor(bound);
// A smoke test, so it scans the whole quote for the 64 bytes; the real verifier
// reads report_data at its parsed offset in the TD report.
const at = quote.indexOf(bound);
console.log(`quote     ${quote.length} bytes`);
console.log(`binding   ${at >= 0 ? `echoed at offset ${at}` : "NOT PRESENT"}`);
if (at < 0) process.exit(1);

// A second quote for a different subject must not carry the first binding,
// which is the property the random challenge could not give us.
const otherSubject = "c".repeat(64);
const otherBound = challenge(agent, otherSubject, nonce);
const otherQuote = await quoteFor(otherBound);
console.log(`\nsecond quote for a different subject, same nonce:`);
console.log(`  its own binding   ${otherQuote.indexOf(otherBound) >= 0 ? "echoed" : "NOT PRESENT"}`);
console.log(`  the first binding ${otherQuote.indexOf(bound) >= 0 ? "PRESENT (bad)" : "absent (correct)"}`);

if (otherQuote.indexOf(otherBound) < 0 || otherQuote.indexOf(bound) >= 0) process.exit(1);
console.log("\nbound challenge survives into report_data, and does not carry across subjects.");
