import Link from "next/link";
import { buildDocsMetadata, buildDocsJsonLd } from "../_meta";

const META_ARGS = ["provenance", "Provenance", 'Provenance envelopes for autonomy tasks, Git commits, changed file evidence, and validation records.'] as const;
export const metadata = buildDocsMetadata(...META_ARGS);

export default function ProvenancePage() {
  return (
    <>
      <script
        type="application/ld+json"
        dangerouslySetInnerHTML={{ __html: JSON.stringify(buildDocsJsonLd(...META_ARGS)) }}
      />
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
        match the task snapshot stored in the subject commit. Release targets
        additionally bind an embedded release-subject manifest
        (<code>releaseSubjectSha256</code>) and an embedded release-scope
        manifest (<code>releaseScopeSha256</code>); the verifier re-validates
        each embedded payload and rejects metadata or digest drift.
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

      <p>
        At release time the same generator binds a release tag, the release-subject
        manifest, and the release-scope manifest so one signature over the audit-root
        covers the audit log, the release artifact set, and the in-scope task set:
      </p>

      <code>{`node agent-os/scripts/provenance.mjs audit-root write \\
  --report audit-report.json \\
  --release <tag> \\
  --release-subject release-subject.json \\
  --release-scope release-scopes/<tag>.json \\
  --commit <commit> \\
  --out docs/provenance/audit-roots/<commit>-audit-root.json`}</code>

      <h2>Status</h2>
      <p>
        Provenance envelopes are experimental. They are consistency evidence,
        not release signatures and not transparency-log entries. Audit-root
        attestations are generated and verified, including the release-target
        bindings to <code>covenant.provenance.release.v1</code> release-subject
        manifests and <code>covenant.release-scope.v1</code> release-scope
        manifests, but they remain unsigned until the project signing workflow
        ships. Key custody is sigstore keyless via cosign; signing-workflow
        wiring for audit-root and release-scope payloads, and
        transparency-log publication, remain planned.
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
          <Link href="/audit">Audit log</Link>: runtime event evidence.
        </li>
        <li>
          <Link href="/security">Security model</Link>: current trust
          boundaries and operator responsibilities.
        </li>
        <li>
          <Link href="/architecture">System architecture</Link>: where
          provenance fits in the operating layer.
        </li>
      </ul>
    </>
  );
}
