import { PRIMITIVES } from "../../_primitives";
import { buildDocsMetadata, buildDocsJsonLd } from "../_meta";

const META_ARGS = [
  "glossary",
  "Glossary",
  "Canonical one-line definitions of the eight Covenant primitives and the core artifacts agents exchange — intent, capability, memory record, audit event, and settlement receipt.",
] as const;
export const metadata = buildDocsMetadata(...META_ARGS);

const TERMSET = {
  "@context": "https://schema.org",
  "@type": "DefinedTermSet",
  "@id": "https://docs.opencovenant.org/glossary#termset",
  name: "Covenant primitives glossary",
  url: "https://docs.opencovenant.org/glossary",
  hasDefinedTerm: PRIMITIVES.map((p) => ({
    "@type": "DefinedTerm",
    "@id": `https://docs.opencovenant.org/glossary#${p.slug}`,
    name: p.term,
    description: p.definition,
    inDefinedTermSet: "https://docs.opencovenant.org/glossary#termset",
    url: p.docHref,
  })),
};

export default function GlossaryPage() {
  return (
    <>
      <script
        type="application/ld+json"
        dangerouslySetInnerHTML={{ __html: JSON.stringify(buildDocsJsonLd(...META_ARGS)) }}
      />
      <script
        type="application/ld+json"
        dangerouslySetInnerHTML={{ __html: JSON.stringify(TERMSET) }}
      />
      <h1>Glossary</h1>
      <p>
        Covenant is built around a fixed vocabulary. These are the canonical
        definitions of the eight host-level primitives and the core artifacts
        agents exchange. Each term links to its reference documentation.
      </p>

      <dl className="not-prose mt-8 space-y-6">
        {PRIMITIVES.map((p) => (
          <div key={p.slug} id={p.slug}>
            <dt className="text-[15px] font-medium text-neutral-100">
              <a href={p.docHref} className="hover:underline">
                {p.term}
              </a>
            </dt>
            <dd className="mt-1 text-[14px] leading-relaxed text-neutral-400">
              {p.definition}
            </dd>
          </div>
        ))}
      </dl>
    </>
  );
}
