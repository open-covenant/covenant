import Link from "next/link";

export const metadata = {
  title: "Provenance",
  description:
    "Alpha provenance envelopes for autonomy tasks, Git commits, changed file evidence, and validation records.",
};

export default function ProvenancePage() {
  return (
    <>
      <h1>Provenance</h1>
      <p>
        Covenant provenance envelopes connect an autonomous task to the Git
        commit it produced. The alpha format is plain JSON and is verified from
        Git object data, not from local working-tree state.
      </p>

      <h2>Envelope contents</h2>
      <ul>
        <li>Subject commit hash.</li>
        <li>Changed file list with Git blob ids and SHA-256 digests.</li>
        <li>Autonomy task snapshot digest from the subject commit.</li>
        <li>Transition events for that task from the subject commit.</li>
        <li>Recorded validation commands and pass/fail/skipped status.</li>
        <li>Explicit limits for claims that are not implemented yet.</li>
      </ul>

      <h2>Verification</h2>
      <p>
        The verifier recomputes file evidence, task evidence, transition
        events, and the envelope payload digest. It also rejects local home
        paths, personal email addresses, private SSH key names, and the
        Covenant SSH host alias.
      </p>

      <code>{`node agent-os/scripts/provenance.mjs verify-all
node agent-os/scripts/provenance.mjs verify --file docs/provenance/attestations/20ff55e-memory-drift-reports.json`}</code>

      <h2>Status</h2>
      <p>
        Provenance envelopes are experimental. They are consistency evidence,
        not release signatures and not transparency-log entries. Public signing
        identity, key custody, release artifact subjects, and transparency-log
        publication remain future work.
      </p>

      <p>
        Audit root signing is planned as a separate release hardening path:
        detached root attestations signed by a project identity, followed by
        transparency-log publication once local signing and verification are
        stable.
      </p>

      <h2>Related</h2>
      <ul>
        <li>
          <Link href="/audit">Audit log</Link> — runtime event evidence.
        </li>
        <li>
          <Link href="/security">Security model</Link> — current trust
          boundaries and operator responsibilities.
        </li>
        <li>
          <Link href="/architecture">System architecture</Link> — where
          provenance fits in the operating layer.
        </li>
      </ul>
    </>
  );
}
