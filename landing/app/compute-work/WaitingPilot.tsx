export function WaitingPilot() {
  return <div className="mt-10 sm:mt-12">
    <section aria-label="Pilot status" className="border-y border-line py-6 sm:py-7">
      <p className="text-[11px] uppercase tracking-[0.18em] text-fg">Awaiting funding</p>
      <p className="mt-3 max-w-2xl text-[13px] leading-[1.8] text-fg-soft sm:text-[14px]">The pool is not funded yet. Work starts once collected fees cover an approved task.</p>
    </section>

    <section className="mt-10 sm:mt-12" aria-labelledby="pilot-how-title">
      <h2 id="pilot-how-title" className="text-[12px] uppercase tracking-[0.2em] text-fg">How it will work</h2>
      <ol className="mt-6 grid gap-5 md:grid-cols-3">{[
        ["01", "Earn fees", "Our liquidity position will earn a share of the fees when people trade CVNT and NVDAx through the pool."],
        ["02", "Fund a task", "We’ll convert collected NVDAx fees to USDC and pay Compute agents for tasks we approve in advance, each with a fixed budget."],
        ["03", "Publish the result", "Each result will include the code, test results, actual cost and funding receipts. Failed attempts and their costs will be public too."],
      ].map(([number, title, description]) => <li key={number} className="border border-line p-5 sm:p-6">
        <span className="text-[10px] text-muted">{number}</span>
        <h3 className="mt-4 text-[17px] text-fg">{title}</h3>
        <p className="mt-3 text-[13px] leading-[1.8] text-fg-soft">{description}</p>
      </li>)}</ol>
    </section>

    <div className="mt-8 max-w-3xl space-y-4 text-[12px] leading-[1.8] text-muted">
      <p>The pool uses existing CVNT and NVDAx. We’ll retain CVNT fees and keep deposited assets out of the work budget. Fees depend on trading and may be zero.</p>
      <p>This pilot has no NVIDIA affiliation. Holding CVNT gives you no NVIDIA stock, stock-redemption rights or access to GPUs.</p>
    </div>
    <nav aria-label="Compute resources" className="mt-8 flex flex-wrap gap-x-6 gap-y-3 text-[12px] text-muted">
      <a href="/compute" className="underline decoration-line-strong underline-offset-4 transition-colors hover:text-fg">Explore Covenant Compute ↗</a>
      <a href="https://docs.opencovenant.org/compute" className="underline decoration-line-strong underline-offset-4 transition-colors hover:text-fg">Compute documentation ↗</a>
    </nav>
  </div>;
}
