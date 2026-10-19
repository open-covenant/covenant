import { OperatorStatus } from "./OperatorStatus";

export function WaitingPilot() {
  return <div className="mt-10 sm:mt-12">
    <OperatorStatus />
    <section className="mt-10 sm:mt-12" aria-labelledby="pilot-how-title">
      <h2 id="pilot-how-title" className="text-[12px] uppercase tracking-[0.2em] text-fg">What Compute will do</h2>
      <ol className="mt-6 grid gap-5 md:grid-cols-3">{[
        ["01", "Compare routes", "Check small swaps against existing routes in both directions. The pool needs to offer traders a competitive price."],
        ["02", "Help operate the pool", "Watch prices, inventory and costs. Recommend changes within fixed limits, with every transaction approved by the owner."],
        ["03", "Show the results", "Publish the checks, decisions, costs and receipts. Real trading fees would help pay for ongoing Compute work."],
      ].map(([number, title, description]) => <li key={number} className="border border-line p-5 sm:p-6">
        <span className="text-[10px] text-muted">{number}</span>
        <h3 className="mt-4 text-[17px] text-fg">{title}</h3>
        <p className="mt-3 text-[13px] leading-[1.8] text-fg-soft">{description}</p>
      </li>)}</ol>
    </section>
    <section aria-label="Pilot budget" className="mt-8 grid gap-4 border-y border-line py-6 sm:grid-cols-3">
      {[["$1,000", "Total pilot cap"], ["$850", "Intended liquidity"], ["$150", "Costs and reserve"]].map(([value, label]) => <div key={label}>
        <p className="text-[20px] text-fg">{value}</p><p className="mt-2 text-[11px] text-muted">{label}</p>
      </div>)}
    </section>
    <div className="mt-6 max-w-3xl space-y-3 text-[12px] leading-[1.8] text-muted">
      <p>Initial Compute work uses a capped operating budget. Pool assets stay separate. Fees depend on external trading and may be zero; the liquidity can lose value.</p>
      <p>No NVIDIA affiliation, stock-redemption rights or GPU entitlement.</p>
    </div>
    <nav aria-label="Compute resources" className="mt-8 flex flex-wrap gap-x-6 gap-y-3 text-[12px] text-muted">
      <a href="/compute" className="underline decoration-line-strong underline-offset-4 transition-colors hover:text-fg">Explore Covenant Compute ↗</a>
      <a href="https://docs.opencovenant.org/compute" className="underline decoration-line-strong underline-offset-4 transition-colors hover:text-fg">Compute documentation ↗</a>
    </nav>
  </div>;
}
