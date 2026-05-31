import Link from "next/link";
import { buildDocsMetadata, buildDocsJsonLd } from "../_meta";

const META_ARGS = ["alpha-release", "Alpha release contract", 'Source-alpha release boundary, evidence bundle requirements, and human-owned release decisions.'] as const;
export const metadata = buildDocsMetadata(...META_ARGS);

export default function AlphaReleasePage() {
  return (
    <>
      <script
        type="application/ld+json"
        dangerouslySetInnerHTML={{ __html: JSON.stringify(buildDocsJsonLd(...META_ARGS)) }}
      />
      <h1>Alpha release contract</h1>
      <p>
        Covenant alpha releases are source-built local infrastructure releases.
        They provide a reproducible daemon, CLI, policy, memory, audit,
        provenance, and workflow substrate for inspection and extension. They
        are not binary distribution releases, SDK stability commitments, or
        public signing events.
      </p>

      <h2>Boundary</h2>
      <ul>
        <li>Builds from source on supported developer hosts.</li>
        <li>Uses the documented validation profile for local control-plane surfaces.</li>
        <li>Records live-test prerequisites instead of hiding skipped boundaries.</li>
        <li>Keeps distributed settlement, installers, SDK publication, release-scope and audit-root signing, and transparency publication as planned work until implemented.</li>
      </ul>

      <h2>Evidence bundle</h2>
      <p>
        Public alpha candidates surface against the validation profile below.
        The readiness report, bundle scaffold, and bundle validator that produce
        the recorded evidence currently live in engineering-loop tooling and are
        not part of the public scripts directory.
      </p>

      <code>{`bash agent-os/scripts/validate.sh --quick
node agent-os/scripts/provenance.mjs verify-all
pnpm --dir landing build
git diff --check`}</code>

      <p>
        Accepted bundles require alpha readiness to be clear. Draft blocker
        review can use an explicit blocked-readiness override without turning
        that draft into accepted release evidence.
      </p>

      <h2>Human-owned decisions</h2>
      <p>
        Release id, tag creation, artifact upload, signing workflow access
        (branch protection rules), and public announcement language remain
        human-owned until the project has explicit automation policy and
        neutral project credentials.
      </p>

      <h2>Related</h2>
      <ul>
        <li>
          <Link href="/validation">Validation profile</Link> — release gates and
          operating surfaces.
        </li>
        <li>
          <Link href="/provenance">Provenance</Link> — consistency evidence and
          audit-root attestations.
        </li>
      </ul>
    </>
  );
}
