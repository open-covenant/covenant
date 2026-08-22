import type { Metadata } from 'next';
import { JobLookup } from '@/components/job-lookup';
import { QuoteWorkflow } from '@/components/quote-workflow';

export const metadata: Metadata = {
  title: 'Hire Mizuki',
  description: 'Submit one bounded public GitHub issue for a fixed $2 or $10 quote.',
};

export default function WorkPage() {
  return (
    <div className="page-shell">
      <section className="page-hero shell work-hero">
        <div>
          <p className="eyebrow">One issue. One fixed outcome.</p>
          <h1>Put a bounded maintenance issue to work.</h1>
        </div>
        <div className="page-hero-aside">
          <p>
            Mizuki accepts public repositories, explicit authorization, and narrowly scoped
            maintenance work.
          </p>
          <ul>
            <li>$2 Micro or $10 Standard</li>
            <li>Independent model review</li>
            <li>Validated pull request or full refund</li>
          </ul>
        </div>
      </section>
      <section className="shell work-grid">
        <QuoteWorkflow />
        <aside className="scope-policy">
          <p className="eyebrow">Scope discipline</p>
          <h2>What Mizuki will accept</h2>
          <ul className="policy-list accepted-list">
            <li>Focused bug fixes</li>
            <li>Small test additions</li>
            <li>Documentation corrections</li>
            <li>Lint and type repairs</li>
            <li>Bounded maintenance chores</li>
          </ul>
          <h2>What he refuses</h2>
          <ul className="policy-list refused-list">
            <li>Features or migrations</li>
            <li>Secrets and authentication changes</li>
            <li>Security-sensitive work</li>
            <li>Dependency overhauls</li>
            <li>Production incidents</li>
          </ul>
          <p className="scope-note">
            The GitHub App must already be installed. Mizuki never opens unsolicited pull requests.
          </p>
          <JobLookup />
        </aside>
      </section>
    </div>
  );
}
