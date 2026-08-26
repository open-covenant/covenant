import type { Metadata } from 'next';
import { IntakeGate } from '@/components/intake-gate';
import { JobLookup } from '@/components/job-lookup';
import { QuoteWorkflow } from '@/components/quote-workflow';
import { getAdmission } from '@/lib/api';
import { pageMetadata } from '@/lib/page-metadata';

export const dynamic = 'force-dynamic';

export const metadata: Metadata = pageMetadata({
  title: 'Submit an issue',
  description:
    'Get a fixed USDC quote for a small, authorized issue in a public GitHub repository.',
  path: '/work',
});

export default async function WorkPage() {
  const admission = await getAdmission();

  return (
    <div className="page-shell">
      <section className="page-hero shell work-hero">
        <div>
          <p className="eyebrow">Fixed-price public-repository maintenance</p>
          <h1>Submit one small, authorized GitHub issue.</h1>
        </div>
        <div className="page-hero-aside">
          <p>
            Mizuki works only in public repositories where both required GitHub Apps are installed
            and a maintainer has explicitly authorized the issue.
          </p>
          <ul>
            <li>2 USDC Micro · up to 3 changed files</li>
            <li>10 USDC Standard · up to 10 changed files</li>
            <li>Quote valid for 15 minutes</li>
            <li>Validated pull request or full refund of the quoted USDC payment</li>
          </ul>
        </div>
      </section>
      <section className="shell work-grid">
        <IntakeGate admission={admission}>
          <QuoteWorkflow />
        </IntakeGate>
        <aside className="scope-policy">
          <p className="eyebrow">Before requesting a quote</p>
          <h2>Authorize one repository</h2>
          <ol className="policy-list onboarding-list">
            <li>
              Install the maintenance App on only the selected public repository. It reads issues
              and checks, creates a scoped branch, and opens the pull request. Mizuki never merges.
            </li>
            <li>
              Install the policy verifier on the same repository. It has read-only access and
              independently verifies repository authorization and pull-request evidence; it cannot
              write code.
            </li>
            <li>
              Open the repository in Mizuki Workbench and authorize the issue with one click. Only
              maintainers with triage access or higher can complete this step.
            </li>
            <li>Paste the issue URL into the quote form.</li>
          </ol>
          <div className="scope-app-actions">
            <a
              href="https://github.com/apps/mizuki-the-mech-core/installations/new"
              target="_blank"
              rel="noreferrer"
            >
              Install maintenance App <span aria-hidden="true">↗</span>
            </a>
            <a
              href="https://github.com/apps/mizuki-the-mech-policy-verifier/installations/new"
              target="_blank"
              rel="noreferrer"
            >
              Install policy verifier <span aria-hidden="true">↗</span>
            </a>
          </div>
          <p className="eyebrow scope-section-label">Supported scope</p>
          <h2>Supported work</h2>
          <ul className="policy-list accepted-list">
            <li>Focused bug fixes</li>
            <li>Tests, fixtures, and documentation</li>
            <li>Lint, type, and configuration repairs</li>
            <li>Repositories with a supported deterministic test command</li>
          </ul>
          <h2>Not supported</h2>
          <ul className="policy-list refused-list">
            <li>Features, migrations, new commands, endpoints, or integrations</li>
            <li>Authentication, secrets, credentials, cryptography, wallets, or payments</li>
            <li>Deployments, production infrastructure, or CI workflows</li>
            <li>Security or vulnerability work</li>
            <li>Lockfiles, generated or vendored files, binaries, or deletions</li>
          </ul>
          <p className="scope-note">
            Mizuki checks the App installations, authorization label, issue text, and repository
            revision again before opening a pull request. If any of them changes after payment,
            delivery stops and the refund process begins.
          </p>
          <p className="scope-note">
            The separate AI review is not a human review, maintainer approval, or security audit.
            Maintainers remain responsible for reviewing, testing, and deciding whether to merge.
          </p>
          <JobLookup />
        </aside>
      </section>
      <section className="shell service-terms" aria-labelledby="service-terms-title">
        <div className="section-heading">
          <p className="eyebrow">Plain-language service terms</p>
          <h2 id="service-terms-title">Know what the fixed price covers before you pay.</h2>
        </div>
        <div className="service-terms-grid">
          <article>
            <h3>What delivery means</h3>
            <p>
              Mizuki opens a pull request against the repository revision captured by the quote,
              within the quoted file limit, after supported repository checks pass and a separate AI
              reviewer approves the patch.
            </p>
          </article>
          <article>
            <h3>What the refund covers</h3>
            <p>
              If Mizuki cannot open that pull request after payment, 100% of the quoted USDC amount
              returns to the wallet that paid. Solana network fees, wallet fees, token price
              changes, and work outside the authorized issue are not included.
            </p>
          </article>
          <article>
            <h3>What data is processed</h3>
            <p>
              Mizuki processes public issue text, public repository code, generated patches, test
              output, GitHub identity, wallet address, and transaction identifiers. Public job,
              bounty, and transaction records are intentionally published. Never submit secrets or
              personal data.
            </p>
          </article>
        </div>
      </section>
    </div>
  );
}
