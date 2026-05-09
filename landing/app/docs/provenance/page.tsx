import Link from "next/link";

export const metadata = {
  title: "Provenance",
  description:
    "Provenance envelopes for autonomy tasks, Git commits, changed file evidence, and validation records.",
};

export default function ProvenancePage() {
  return (
    <>
      <h1>Provenance</h1>
      <p>
        Covenant provenance envelopes connect an autonomous task to the Git
        commit it produced. The format is plain JSON and is verified from Git
        object data, not from local working-tree state. The same verifier also
        validates unsigned audit-root attestations that bind a local audit
        integrity report to a commit and task or release target.
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

      <h2>Audit-root attestations</h2>
      <p>
        <code>covenant.audit-root-attestation.v1</code> payloads are generated
        from <code>covenant audit verify</code> output. The verifier checks
        that the report is valid, event and anchor counts match, the root hash
        is canonical hex, the subject commit is canonical, and task targets
        match the task snapshot stored in the subject commit.
      </p>

      <code>{`covenant audit verify > audit-report.json

node agent-os/scripts/provenance.mjs audit-root write \\
  --report audit-report.json \\
  --task audit-root-attestation-v1 \\
  --commit HEAD \\
  --out docs/provenance/audit-roots/<commit>-audit-root.json \\
  --validation "covenant audit verify=passed"

node agent-os/scripts/provenance.mjs audit-root verify \\
  --file docs/provenance/audit-roots/<commit>-audit-root.json`}</code>

      <h2>Status</h2>
      <p>
        Provenance envelopes are experimental. They are consistency evidence,
        not release signatures and not transparency-log entries. Audit-root
        attestations are generated and verified, but they remain unsigned until
        a project signing identity is selected. Key custody, release artifact
        subjects, and transparency-log publication are tracked as release
        hardening work.
      </p>

      <p>
        Audit root signing remains a separate release hardening path: attach a
        project-controlled signature to the existing detached root payload, then
        publish it to a transparency log once local signing and verification are
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
