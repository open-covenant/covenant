import type { Metadata } from "next";
import { pageMetadata } from "@/app/_metadata";
import { TAGLINE } from "../../_brand";
import { CopyButton } from "../CopyButton";
import { Chip, MoreSections, PageHead, Panel, Part, Spec } from "../_kit";

const DESCRIPTION =
  "How Covenant speaks: positioning, voice, tone by audience, style conventions, taglines, boilerplate and vocabulary.";

export const metadata: Metadata = pageMetadata("/brand/messaging", { title: "Messaging", section: "Brand guidelines" }, {
  title: "Messaging: Covenant brand guidelines",
  description: DESCRIPTION,
  alternates: { canonical: "/brand/messaging" },
  openGraph: {
    type: "website",
    url: "https://opencovenant.org/brand/messaging",
    title: "Messaging · Covenant brand guidelines",
    description: DESCRIPTION,
  },
});

const VOICE = [
  {
    trait: "Exact",
    is: "Calls things what the system calls them: grants, receipts, primitives, capabilities.",
    not: "Vague verbs, loose nouns.",
  },
  {
    trait: "Plain",
    is: "Short sentences, common words. New ideas explained through familiar ones.",
    not: "Jargon for its own sake. Unexpanded acronyms.",
  },
  {
    trait: "Proven",
    is: "Links the evidence: the commit, the audit entry, the transaction, the test.",
    not: "Claims without evidence. Numbers without a source.",
  },
  {
    trait: "Calm",
    is: "Level and certain. The facts carry the weight.",
    not: "Exclamation marks, superlatives, urgency.",
  },
  {
    trait: "Open",
    is: "Credits others, publishes the source, states what is unfinished.",
    not: "Vague about status.",
  },
];

const TONE = [
  {
    who: "Builders",
    where: "Docs, CLI, GitHub",
    how: "Exact and imperative. Lead with the call or the command.",
    example: "Send request bodies as JSON; responses default to JSON.",
  },
  {
    who: "Everyone",
    where: "The website, product pages",
    how: "Plain and confident. What it does, then why it matters.",
    example: "Covenant lets people and agents share one computer safely. Every agent runs under a signed grant, so it can only do what you have allowed.",
  },
  {
    who: "Community",
    where: "X, Telegram, the blog",
    how: "Short and concrete. One idea, one link.",
    example: "Covenant Compute is live on Solana mainnet.",
  },
  {
    who: "Partners & press",
    where: "Announcements, the partners page",
    how: "Factual and specific. Both products, and what the integration does.",
    example: "Covenant publishes signed attestations of its audit root, a fingerprint of the full audit log, to Synapse on Solana, so other parties can verify an agent's identity.",
  },
];

const CONVENTIONS: [string, React.ReactNode][] = [
  ["Name", <>Covenant, capital C, in running text. COVENANT in capitals only in the lockup and in tracked labels.</>],
  ["Category", <>Call Covenant an operating layer, not an operating system. The phrase appears only in the headline, &ldquo;An operating system that builds itself,&rdquo; where it works as a comparison.</>],
  ["Products", <>Covenant Guard, Covenant Compute, Covenant Desk on first mention; Guard, Compute, Desk after it.</>],
  ["Token", <>$CVNT: capitals, with the dollar sign.</>],
  ["Protocols", <>MCP, A2A, x402 (lowercase x), Solana.</>],
  ["Hyphens", <>Always hyphenate on-chain, agent-native, local-first and hash-chained. Hyphenate open-source only before a noun: open-source code, but Covenant is open source.</>],
  ["Headlines", <>Write headlines in sentence case; the display style sets them in capitals, and screen readers keep the original casing.</>],
  ["Labels", <>Two to four words in tracked JetBrains Mono capitals: Built in the open, What you plug in.</>],
  ["Figures", <>Numerals for data, prices and measurements, tabular in tables: 4 protocols, $3.00, 150 ms. In running text, spell out one to nine: eight primitives.</>],
  ["Punctuation", <>No exclamation marks. One idea per sentence.</>],
  ["Emoji", <>None on the site or in product; at most one in a social post.</>],
];

const TAGLINES = [
  { line: "An operating system that builds itself.", use: "The headline. Home page, share card." },
  { line: "Permission, not trust.", use: "The idea in three words. Talks, merchandise." },
  { line: "A receipt for every decision.", use: "The proof. Pairs with Permission, not trust." },
  { line: "One computer, safely shared by humans and agents.", use: "What the eight primitives add up to." },
  { line: `${TAGLINE}.`, use: "The descriptor. Footers, bios, directory listings." },
  { line: "Covenant runs under the same rules it gives your agents.", use: "How Covenant is built. Home page, live build log." },
];

const BOILERPLATE = [
  {
    label: "One line",
    text: "Covenant is the operating layer for agentic software: every agent runs under a signed grant, and every action leaves a receipt.",
  },
  {
    label: "Short",
    text: "Covenant is the open, local-first operating layer for agentic software. It gives humans and agents eight host-level primitives (intent, runtime, memory, identity, permissions, communication, a compositor, and on-chain settlement) so they can safely share one computer.",
  },
  {
    label: "Long",
    text: "Covenant is the open, local-first operating layer for agentic software. It gives humans and agents eight host-level primitives (intent, runtime, memory, identity, permissions, communication, a compositor, and on-chain settlement) so they can safely share one computer. Every agent runs under a signed grant, every action leaves a receipt in a hash-chained audit log, and Covenant's own code is written in the open by agents running on it. Covenant is open source under Apache-2.0. Learn more at opencovenant.org.",
  },
];

const VOCABULARY: [string, string][] = [
  ["agent", "bot, AI buddy"],
  ["operating layer", "chatbot, agent framework"],
  ["signed grant, capability", "access, trust us"],
  ["receipt, audit log", "logs, tracking"],
  ["verify, verifiable", "trustless"],
  ["on-chain settlement", "web3"],
  ["open source, Apache-2.0", "community-driven (without a link)"],
  ["live, ships, shipped", "revolutionary, game-changing, next-gen"],
];

const EXAMPLES = [
  {
    good: "Before your agent pays or trusts another agent, it checks an on-chain track record, confirms a real identity, and verifies a signed claim.",
    bad: "Guard is a revolutionary, AI-powered trust solution for the next generation of web3 agents!",
    why: "Says what happens, in order, in the system's own nouns.",
  },
  {
    good: "Every commit to Covenant is made under a signed grant, checked before it goes through, and leaves a receipt in a hash-chained audit log.",
    bad: "Our cutting-edge autonomous engine keeps everything secure and transparent.",
    why: "A mechanism the reader can check, not an adjective they have to believe.",
  },
  {
    good: "Covenant Compute is live on Solana mainnet.",
    bad: "BIG NEWS 🚀🚀 Compute is finally here, and it changes everything!!",
    why: "Status, product, place. Nothing else needed.",
  },
];

export default function BrandMessaging() {
  return (
    <article>
      <PageHead
        slug="messaging"
        intro={
          <p>
            Covenant communicates the way it operates: every claim is backed by something the reader can
            verify. We keep one voice across documentation, posts and announcements, and adapt the tone to
            the audience.
          </p>
        }
      />

      <Part
        id="positioning"
        kicker="Messaging"
        title="Positioning"
        text={
          <>
            <p>Our positioning fits in one sentence. When there is room for only one message, lead with it.</p>
          </>
        }
      >
        <Panel
          tone="ground"
          label="The one-liner"
          actions={<CopyButton value={BOILERPLATE[0].text} label="Copy the one-liner" />}
        >
          <div className="flex flex-col gap-10 p-6 sm:p-9">
            <p className="type-display max-w-[22ch] text-[clamp(1.5rem,3.3vw,2.4rem)] text-fg">
              Every agent runs under a <span className="text-ramp">signed grant</span>, and every action leaves a receipt.
            </p>
            <Spec
              rows={[
                ["Category", "Open agent-native operating layer"],
                ["Promise", "Permission, not trust"],
                ["Proof", "A receipt for every decision"],
                ["Audience", "Builders first, then everyone who runs agents"],
              ]}
            />
          </div>
        </Panel>
      </Part>

      <Part
        id="voice"
        kicker="Messaging"
        title="Voice"
        text={
          <p>The voice stays the same across every channel. Only the tone changes.</p>
        }
      >
        <ul className="grid gap-3 sm:grid-cols-2">
          {VOICE.map((v, i) => (
            <li key={v.trait} className={`rounded-[6px] border border-line bg-raised p-5 ${i === VOICE.length - 1 ? "sm:col-span-2" : ""}`}>
              <p className="font-mono text-[12px] font-semibold uppercase tracking-[0.2em] text-fg">{v.trait}</p>
              <p className="mt-3 text-[12.5px] leading-relaxed text-fg-soft">{v.is}</p>
              <p className="mt-3 border-t border-line-soft pt-3 text-[11.5px] leading-relaxed text-muted-2">
                <span className="font-mono uppercase tracking-[0.16em] text-bad">Not</span> · {v.not}
              </p>
            </li>
          ))}
        </ul>
      </Part>

      <Part
        id="tone"
        kicker="Messaging"
        title="Tone by audience"
        wide
        text={<p>The reader sets the depth and the pace.</p>}
      >
        <div className="overflow-x-auto rounded-[6px] border border-line">
          <table className="w-full min-w-[40rem] border-collapse text-left">
            <thead>
              <tr className="bg-raised">
                {["Audience", "Where", "Tone", "Example"].map((h) => (
                  <th key={h} className="border-b border-line px-4 py-3 font-mono text-[10px] font-medium uppercase tracking-[0.18em] text-muted">
                    {h}
                  </th>
                ))}
              </tr>
            </thead>
            <tbody>
              {TONE.map((t) => (
                <tr key={t.who} className="align-top">
                  <td className="border-b border-line-soft px-4 py-4 font-mono text-[11.5px] font-semibold uppercase tracking-[0.14em] text-fg">{t.who}</td>
                  <td className="border-b border-line-soft px-4 py-4 font-mono text-[11px] text-muted">{t.where}</td>
                  <td className="border-b border-line-soft px-4 py-4 text-[12px] leading-relaxed text-fg-soft">{t.how}</td>
                  <td className="border-b border-line-soft px-4 py-4 text-[12px] leading-relaxed text-fg">&ldquo;{t.example}&rdquo;</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      </Part>

      <Part id="conventions" kicker="Messaging" title="Style conventions" text={<p>The small decisions, made once.</p>}>
        <Spec rows={CONVENTIONS} />
      </Part>

      <Part
        id="taglines"
        kicker="Messaging"
        title="Taglines"
        text={<p>Each line has one job. Use them as written.</p>}
      >
        <ul className="flex flex-col gap-2">
          {TAGLINES.map((t) => (
            <li key={t.line}>
              <CopyButton
                value={t.line}
                label={`Copy “${t.line}”`}
                className="w-full justify-between rounded-[6px] border border-line bg-raised px-5 py-4 hover:border-line-strong"
              >
                <span className="flex min-w-0 flex-col gap-2">
                  <span className="type-display text-[clamp(1.05rem,2vw,1.35rem)] text-fg">{t.line}</span>
                  <span className="text-[11.5px] text-muted">{t.use}</span>
                </span>
              </CopyButton>
            </li>
          ))}
        </ul>
      </Part>

      <Part
        id="boilerplate"
        kicker="Messaging"
        title="Boilerplate"
        text={<p>For bios, directories, press and partner pages. Choose the length; keep the facts.</p>}
      >
        <div className="flex flex-col gap-4">
          {BOILERPLATE.map((b) => (
            <Panel key={b.label} label={`${b.label} · ${b.text.split(/\s+/).length} words`} actions={<CopyButton value={b.text} label={`Copy the ${b.label.toLowerCase()} boilerplate`} />}>
              <p className="p-5 pr-14 text-[12.5px] leading-[1.75] text-fg-soft">{b.text}</p>
            </Panel>
          ))}
        </div>
      </Part>

      <Part id="vocabulary" kicker="Messaging" title="Vocabulary" text={<p>The words that carry the idea, and the ones that dilute it.</p>}>
        <div className="overflow-hidden rounded-[6px] border border-line">
          <div className="grid grid-cols-2 bg-raised">
            <span className="border-b border-line px-4 py-3 font-mono text-[10px] uppercase tracking-[0.18em] text-ok">Use</span>
            <span className="border-b border-l border-line px-4 py-3 font-mono text-[10px] uppercase tracking-[0.18em] text-bad">Avoid</span>
          </div>
          {VOCABULARY.map(([say, not]) => (
            <div key={say} className="grid grid-cols-2 border-b border-line-soft last:border-b-0">
              <span className="px-4 py-3 font-mono text-[12px] text-fg">{say}</span>
              <span className="border-l border-line-soft px-4 py-3 font-mono text-[12px] text-muted-2 line-through decoration-faint">{not}</span>
            </div>
          ))}
        </div>
      </Part>

      <Part id="examples" kicker="Messaging" title="In practice" wide>
        <div className="flex flex-col gap-4">
          {EXAMPLES.map((e) => (
            <div key={e.good} className="grid gap-3 md:grid-cols-2">
              <div className="flex flex-col gap-3 rounded-[6px] border border-line bg-raised p-5">
                <Chip className="text-ok">✓ Do</Chip>
                <p className="text-[13px] leading-relaxed text-fg">{e.good}</p>
                <p className="mt-auto text-[11.5px] leading-relaxed text-muted">{e.why}</p>
              </div>
              <div className="flex flex-col gap-3 rounded-[6px] border border-line bg-ground p-5">
                <Chip className="text-bad">✕ Don&apos;t</Chip>
                <p className="text-[13px] leading-relaxed text-muted-2">{e.bad}</p>
              </div>
            </div>
          ))}
        </div>
      </Part>

      <MoreSections slug="messaging" />
    </article>
  );
}
