export function WaitingPilot() {
  return <div className="mt-10 sm:mt-12">
    <section aria-label="Pilot status" className="border-y border-line py-6 sm:py-7">
      <p className="text-[11px] uppercase tracking-[0.18em] text-fg">Awaiting funding</p>
      <p className="mt-3 max-w-2xl text-[13px] leading-[1.8] text-fg-soft sm:text-[14px]">Pool funding is pending. Paid work can start once collected fees cover an approved task.</p>
    </section>

    <section className="mt-10 sm:mt-12" aria-labelledby="pilot-how-title">
      <h2 id="pilot-how-title" className="text-[12px] uppercase tracking-[0.2em] text-fg">How it will work</h2>
      <ol className="mt-6 grid gap-5 md:grid-cols-3">{[
        ["01", "Trading fees", "People trade CVNT and NVDAx. Our pool position can earn a share of the trading fees."],
        ["02", "Agent work", "We convert collected NVDAx fees to USDC, a dollar-linked token, to pay Compute agents for useful software work. Each task needs operator approval and a budget."],
        ["03", "Public receipts", "We’ll publish the code, test results, actual cost and funding receipts, including failed attempts."],
      ].map(([number, title, description]) => <li key={number} className="border border-line p-5 sm:p-6">
        <span className="text-[10px] text-muted">{number}</span>
        <h3 className="mt-4 text-[17px] text-fg">{title}</h3>
        <p className="mt-3 text-[13px] leading-[1.8] text-fg-soft">{description}</p>
      </li>)}</ol>
    </section>

    <div className="mt-8 max-w-3xl space-y-4 text-[12px] leading-[1.8] text-muted">
      <p>This uses existing tokens; we’re not launching a new project token. CVNT fees are retained. The assets put into the pool won’t pay for jobs. Fees depend on actual trading and may be zero.</p>
      <p>This pilot has no NVIDIA affiliation. Holding CVNT gives you no NVIDIA stock, stock-redemption rights or access to GPUs.</p>
    </div>
    <nav aria-label="Compute resources" className="mt-8 flex flex-wrap gap-x-6 gap-y-3 text-[12px] text-muted">
      <a href="/compute" className="underline decoration-line-strong underline-offset-4 transition-colors hover:text-fg">Explore Covenant Compute ↗</a>
      <a href="https://docs.opencovenant.org/compute" className="underline decoration-line-strong underline-offset-4 transition-colors hover:text-fg">Compute documentation ↗</a>
    </nav>
  </div>;
}
