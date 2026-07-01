import type { Metadata } from "next";

const DESCRIPTION =
  "Covenant is a verifiable trust layer over MagicBlock execution. Discover a Covenant-verified ephemeral rollup, meter agent work into an on-chain provenance root, and bond agents with slashable stake. Live on Solana mainnet.";

export const metadata: Metadata = {
  title: "Covenant × MagicBlock Integration Guide",
  description: DESCRIPTION,
  alternates: { canonical: "/magicblock" },
  openGraph: {
    type: "website",
    url: "https://opencovenant.org/magicblock",
    title: "Covenant × MagicBlock Integration Guide",
    description: DESCRIPTION,
    images: [{ url: "/opengraph-image.jpg", width: 1200, height: 630 }],
  },
};

const PROGRAM = "cov9UDypG7nsryxdgMcKhKU2spRVWLVjxT2iTv6do5Y";
const SAS = "22zoJMtdu4tQc2PzL74ZUT7FrwgB1Udec8DdW4yw4BdG";
const ISSUER = "AdChcSmDKX57rU9qChMJ3MKnqNZbmiQAjuns9VCjzqRb";

const CSS = `
#ig {
  --fg: #ededed; --muted: #9a9a9a; --faint: #6f6f6f; --line: #262626; --accent: #3a3a3a;
  --mono: ui-monospace, "SF Mono", Menlo, Consolas, monospace;
  background: #030303; color: var(--fg); min-height: 100dvh;
}
#ig .page { max-width: 880px; margin: 0 auto; padding: clamp(32px, 6vw, 76px); }
#ig .mast { padding-bottom: 8px; }
#ig .deck { font-family: var(--mono); font-size: 11px; letter-spacing: 0.32em; text-indent: 0.32em; color: var(--muted); text-transform: uppercase; margin: 0 0 18px; }
#ig h1 { font-weight: 200; font-size: clamp(30px, 5vw, 44px); letter-spacing: -0.01em; margin: 0 0 22px; line-height: 1.1; }
#ig .lede { font-size: 16px; line-height: 1.72; color: #d0d0d0; margin: 0 0 10px; }
#ig .lede strong { color: var(--fg); font-weight: 600; }
#ig hr { border: 0; border-top: 1px solid var(--line); margin: 40px 0; }
#ig .seclabel { font-family: var(--mono); font-size: 11px; letter-spacing: 0.32em; text-transform: uppercase; color: var(--muted); margin: 0 0 16px; }
#ig .seclabel .n { color: var(--faint); margin-right: 12px; }
#ig section { margin: 38px 0; }
#ig p { font-size: 14px; line-height: 1.7; color: var(--muted); margin: 0 0 16px; }
#ig p b { color: var(--fg); font-weight: 600; }
#ig p .lit { color: #d0d0d0; }
#ig code { font-family: var(--mono); font-size: 12.5px; color: #cfcfcf; word-break: break-all; }
#ig pre { background: #070707; border: 1px solid var(--line); border-left: 1px solid var(--accent); padding: 16px 18px; overflow-x: auto; margin: 0 0 16px; }
#ig pre code { color: #c8c8c8; font-size: 12px; line-height: 1.65; word-break: normal; white-space: pre; }
#ig .cmt { color: #6f6f6f; }
#ig dl { margin: 0; }
#ig .ref { display: grid; grid-template-columns: 190px 1fr; gap: 6px 20px; font-size: 13px; line-height: 1.55; }
#ig .ref dt { color: var(--muted); }
#ig .ref dd { margin: 0; color: #cfcfcf; font-family: var(--mono); font-size: 12px; word-break: break-all; }
#ig a { color: #d0d0d0; text-decoration: none; border-bottom: 1px solid var(--accent); }
#ig a:hover { color: var(--fg); border-color: var(--muted); }
#ig .foot { display: flex; justify-content: space-between; align-items: center; margin-top: 48px; padding-top: 18px; border-top: 1px solid var(--line); font-family: var(--mono); font-size: 11px; letter-spacing: 0.16em; color: var(--faint); text-transform: uppercase; }
#ig .foot .tag { color: #bdbdbd; margin-right: 6px; }
@media (max-width: 640px) { #ig .ref { grid-template-columns: 1fr; gap: 2px; } #ig .ref dd { margin-bottom: 10px; } }
`;

export default function Page() {
  return (
    <main id="ig">
      <style dangerouslySetInnerHTML={{ __html: CSS }} />
      <div className="page">
        <header className="mast">
          <p className="deck">Integration Guide</p>
          <h1>Covenant × MagicBlock</h1>
          <p className="lede">
            <strong>Covenant is a verifiable trust layer over MagicBlock execution.</strong> MagicBlock runs agent work
            fast and private in ephemeral rollups. Covenant makes that work accountable: an agent bonds a slashable stake,
            every metered action folds into an on-chain provenance root, and any client can check that the rollup it runs
            on is a genuine, attested enclave.
          </p>
          <p className="lede">
            The settlement program is deployed and OtterSec-verified on Solana mainnet. Everything below is live, not a
            proposal.
          </p>
        </header>

        <hr />

        <section>
          <p className="seclabel"><span className="n">01</span>Discover a Covenant-verified ER</p>
          <p>
            The Magic Router returns a set of ephemeral rollups, each identified by a validator pubkey. Covenant publishes
            a <b>verified-ER attestation</b> through the Solana Attestation Service, keyed to that validator identity. An
            agent picking where to run resolves the trust signal in one account read. No router change, no indexer.
          </p>
          <pre><code>{`import { deriveCredentialPda, deriveSchemaPda, deriveAttestationPda,
  fetchMaybeAttestation, fetchSchema, deserializeAttestationData } from "sas-lib";

const ISSUER = "${ISSUER}";              // Covenant credential authority
const [credential] = await deriveCredentialPda({ authority: ISSUER, name: "covenant" });
const [schema]     = await deriveSchemaPda({ credential, name: "er-verified", version: 1 });

`}<span className="cmt">{`// validator = an ER identity from the router's getRoutes`}</span>{`
const [attestation] = await deriveAttestationPda({ credential, schema, nonce: validator });
const acct = await fetchMaybeAttestation(rpc, attestation);

const verified = acct.exists &&
  deserializeAttestationData(await fetchSchema(rpc, schema), acct.data.data).verified;`}</code></pre>
          <p>
            The attestation carries the enclave&rsquo;s DCAP result (TCB status, <code>mr_td</code>) and is signed by the
            Covenant issuer. A relying party trusts it by checking the signer is an authorized signer of the credential.
            Validator identities do not rotate, so the key is stable.
          </p>
        </section>

        <section>
          <p className="seclabel"><span className="n">02</span>Meter work into an on-chain provenance root</p>
          <p>
            A credit account carries a <code>provenance_root</code>. Each <code>consume_credits(amount, receipt_hash)</code>{" "}
            folds the receipt into a hash-chain, gaslessly while the account is delegated to the ER, and commits to L1 on
            undelegate. The root becomes a real-time, tamper-evident record of what the agent did.
          </p>
          <pre><code>{`provenance_root = sha256(provenance_root || receipt_hash)   `}<span className="cmt">{`// genesis = 32 zero bytes`}</span></code></pre>
          <p>
            Because the fold is deterministic, anyone can recompute it from the receipts and check it against the on-chain
            root. If a single action is altered, added, or dropped, the roots diverge. The receipt is yours to define:
            hash the actual work product (the intent and the agent&rsquo;s output) and the on-chain record <em>is</em> the
            work.
          </p>
        </section>

        <section>
          <p className="seclabel"><span className="n">03</span>Bond an agent, slash against its record</p>
          <p>
            <code>register_agent</code> binds an agent identity to an operator. <code>stake</code> locks a slashable CVNT
            position against it. When an agent misbehaves, <code>slash_for_actions</code> burns the bond with the reason
            read straight from the agent&rsquo;s on-chain <code>provenance_root</code>, via the seed-bound credit account.
            There is no caller-supplied reason to forge; the penalty is anchored to the verifiable record of what the agent
            actually did.
          </p>
          <pre><code>{`register_agent(agent_key, metadata_hash, capability_hash)   `}<span className="cmt">{`// bond the identity`}</span>{`
stake(amount, lock_until)                                  `}<span className="cmt">{`// slashable CVNT position`}</span>{`
slash_for_actions(amount)                                  `}<span className="cmt">{`// reason = the on-chain provenance root`}</span></code></pre>
        </section>

        <section>
          <p className="seclabel"><span className="n">04</span>Verify the enclave</p>
          <p>
            The <code>covenant-tee</code> crate pulls a TDX quote from a MagicBlock Private ER, verifies it with Intel DCAP
            against the Phala PCCS, and binds an agent plus its provenance root into the 64-byte quote challenge. The result
            is a signed Covenant attestation proving a specific agent&rsquo;s record came from a genuine, attested enclave.
            This is the same verification behind the verified-ER lookup in section 01.
          </p>
        </section>

        <hr />

        <section>
          <p className="seclabel"><span className="n">05</span>Live on mainnet</p>
          <p>
            Proven end to end with our own agent: the Covenant demo agent answered real prompts, each metered on the
            verified ER (<span className="lit">mainnet-tee</span>), and the on-chain provenance root equals the hash-chain
            of those exact answers. The agent is bonded and was slashed against that record. All addresses below are public
            and independently checkable.
          </p>
          <dl className="ref">
            <dt>Settlement program</dt>
            <dd>{PROGRAM}</dd>
            <dt>Verification</dt>
            <dd><a href={`https://verify.osec.io/status/${PROGRAM}`}>verify.osec.io &middot; OtterSec verified</a></dd>
            <dt>Attestation service</dt>
            <dd>{SAS}</dd>
            <dt>Covenant issuer</dt>
            <dd>{ISSUER}</dd>
            <dt>Credential / schema</dt>
            <dd>covenant / er-verified v1</dd>
            <dt>Source</dt>
            <dd><a href="https://github.com/open-covenant/covenant">github.com/open-covenant/covenant</a></dd>
          </dl>
        </section>

        <p>
          Building an agent on MagicBlock and want verifiable bonds, slashing, or the verified-ER lookup wired in? Reach
          out at <a href="https://opencovenant.org/contact">opencovenant.org/contact</a>.
        </p>

        <div className="foot">
          <span><span className="tag">{"</>"}</span>opencovenant.org</span>
          <span>Covenant × MagicBlock</span>
        </div>
      </div>
    </main>
  );
}
