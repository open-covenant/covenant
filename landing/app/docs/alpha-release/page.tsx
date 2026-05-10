import Link from "next/link";

export const metadata = {
  title: "Alpha release contract",
  description:
    "Source-alpha release boundary, evidence bundle requirements, and human-owned release decisions.",
};

export default function AlphaReleasePage() {
  return (
    <>
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
        <li>Keeps distributed settlement, installers, SDK publication, signed artifacts, and transparency publication as planned work until implemented.</li>
      </ul>

      <h2>Evidence bundle</h2>
      <p>
        Each candidate should record the evidence helper output, validation
        outcomes, sanitized alpha readiness blockers, skipped live prerequisites,
        provenance links, audit-root attestations when present, and the release
        decision.
      </p>

      <code>{`node agent-os/scripts/alpha-release-readiness.mjs
node agent-os/scripts/alpha-release-bundle.mjs v0.1.0-alpha.1
node agent-os/scripts/alpha-release-validate-bundle.mjs v0.1.0-alpha.1
node agent-os/scripts/validate-alpha-release-evidence.mjs
bash agent-os/scripts/validate.sh --quick
pnpm --dir landing build
git diff --check`}</code>

      <p>
        Accepted bundles require alpha readiness to be clear. Draft blocker
        review can use an explicit blocked-readiness override without turning
        that draft into accepted release evidence.
      </p>

      <h2>Human-owned decisions</h2>
      <p>
        Release id, tag creation, artifact upload, project signing keys, key
        rotation, and public announcement language remain human-owned until the
        project has explicit automation policy and neutral project credentials.
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
