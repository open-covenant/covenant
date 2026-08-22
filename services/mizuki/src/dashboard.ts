export const dashboard = `<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width,initial-scale=1">
  <title>Mizuki — maintenance that ships</title>
  <style>
    :root{color-scheme:dark;--ink:#f3f0e8;--muted:#99978f;--line:#292a2d;--accent:#9ef0c5;--bg:#0d0e10}*{box-sizing:border-box}body{margin:0;background:var(--bg);color:var(--ink);font:16px/1.5 ui-monospace,SFMono-Regular,Menlo,monospace}main{width:min(1040px,calc(100% - 40px));margin:0 auto;padding:72px 0}header{display:flex;justify-content:space-between;gap:32px;align-items:start;margin-bottom:72px}.mark{color:var(--accent);font-weight:700;letter-spacing:.12em}.lede{max-width:670px}h1{font:500 clamp(42px,7vw,82px)/.98 ui-sans-serif,system-ui;margin:10px 0 24px;letter-spacing:-.055em}p{color:var(--muted);max-width:62ch}.promise{color:var(--ink)}.grid{display:grid;grid-template-columns:repeat(3,1fr);border-top:1px solid var(--line);border-left:1px solid var(--line)}.cell{min-height:150px;padding:22px;border-right:1px solid var(--line);border-bottom:1px solid var(--line)}.value{font:500 34px/1.1 ui-sans-serif,system-ui;margin-top:28px}.label{color:var(--muted);font-size:12px;text-transform:uppercase;letter-spacing:.08em}.positive{color:var(--accent)}footer{display:flex;justify-content:space-between;margin-top:32px;color:var(--muted);font-size:12px}@media(max-width:760px){header{display:block;margin-bottom:48px}.grid{grid-template-columns:repeat(2,1fr)}main{padding-top:38px}}
  </style>
</head>
<body><main>
  <header><div class="mark">MIZUKI / LIVE</div><div class="lede"><h1>Maintenance that ships.</h1><p class="promise">Give Mizuki a public GitHub issue. Pay a fixed USDC quote. Receive a validated pull request or a full refund.</p><p>Small scope, independent review, visible economics. No unsolicited PRs.</p><p>Variable cost coverage includes priced model tokens and measured sandbox runtime. Provider billing adjustments, chain/facilitator fees, and infrastructure are excluded, so gross margin remains unverified.</p></div></header>
  <section class="grid">
    <div class="cell"><div class="label">Paid jobs</div><div class="value" data-key="paidJobs">—</div></div>
    <div class="cell"><div class="label">Delivered PRs</div><div class="value" data-key="deliveredPrs">—</div></div>
    <div class="cell"><div class="label">Merged PRs</div><div class="value" data-key="mergedPrs">—</div></div>
    <div class="cell"><div class="label">External maintainers</div><div class="value" data-key="externalMaintainers">—</div></div>
    <div class="cell"><div class="label">External repos</div><div class="value" data-key="externalRepositories">—</div></div>
    <div class="cell"><div class="label">Refund success</div><div class="value" data-key="refundSuccessRate">—</div></div>
    <div class="cell"><div class="label">Recognized revenue</div><div class="value" data-key="recognizedRevenueUsd">—</div></div>
    <div class="cell"><div class="label">Platform-reported creator fees</div><div class="value" data-key="platformReportedCreatorFeesSentLamports">—</div></div>
    <div class="cell"><div class="label">Variable cost estimate</div><div class="value" data-key="variableRouteCostEstimateUsd">—</div></div>
    <div class="cell"><div class="label">Gross margin</div><div class="value" data-key="grossMarginStatus">—</div></div>
    <div class="cell"><div class="label">Rescue payouts</div><div class="value" data-key="bountiesReleased">—</div></div>
    <div class="cell"><div class="label">Planned improvement allocation</div><div class="value" data-key="plannedImprovementAllocationUsd">—</div></div>
  </section>
  <footer><span>x402 USDC · GitHub App · UsePod</span><span data-updated>updating…</span></footer>
</main><script>
const money=new Set(['recognizedRevenueUsd','variableRouteCostEstimateUsd','plannedImprovementAllocationUsd']);
function sol(lamports){const value=BigInt(lamports);const whole=value/1000000000n;const fraction=value%1000000000n;return fraction===0n?whole+' SOL':whole+'.'+fraction.toString().padStart(9,'0').replace(/0+$/,'')+' SOL'}
async function refresh(){try{const r=await fetch('/v1/metrics');const m=await r.json();for(const el of document.querySelectorAll('[data-key]')){const k=el.dataset.key;const v=m[k];const attempts=m.refundCount+m.refundPending;el.textContent=k==='refundSuccessRate'?(attempts===0||v===null?'no attempts':(v*100).toFixed(0)+'%'):k==='platformReportedCreatorFeesSentLamports'?sol(v):k==='grossMarginStatus'?'unverified — partial cost coverage':money.has(k)?'$'+v.toFixed(2):String(v)}document.querySelector('[data-updated]').textContent='updated '+new Date(m.updatedAt).toLocaleTimeString()}catch{document.querySelector('[data-updated]').textContent='metrics unavailable'}}refresh();setInterval(refresh,15000);
</script></body></html>`;
