export const dashboard = `<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width,initial-scale=1">
  <title>Mizuki the Mech — public performance</title>
  <style>
    :root{color-scheme:dark;--ink:#f3f0e8;--muted:#99978f;--line:#292a2d;--accent:#9ef0c5;--bg:#0d0e10}*{box-sizing:border-box}body{margin:0;background:var(--bg);color:var(--ink);font:16px/1.5 ui-monospace,SFMono-Regular,Menlo,monospace}main{width:min(1040px,calc(100% - 40px));margin:0 auto;padding:72px 0}header{display:flex;justify-content:space-between;gap:32px;align-items:start;margin-bottom:72px}.mark{color:var(--accent);font-weight:700;letter-spacing:.12em}.lede{max-width:670px}h1{font:500 clamp(42px,7vw,82px)/.98 ui-sans-serif,system-ui;margin:10px 0 24px;letter-spacing:-.055em}p{color:var(--muted);max-width:62ch}.promise{color:var(--ink)}.grid{display:grid;grid-template-columns:repeat(3,1fr);border-top:1px solid var(--line);border-left:1px solid var(--line)}.cell{min-height:150px;padding:22px;border-right:1px solid var(--line);border-bottom:1px solid var(--line)}.value{font:500 34px/1.1 ui-sans-serif,system-ui;margin-top:28px}.label{color:var(--muted);font-size:12px;text-transform:uppercase;letter-spacing:.08em}.positive{color:var(--accent)}footer{display:flex;justify-content:space-between;margin-top:32px;color:var(--muted);font-size:12px}@media(max-width:760px){header{display:block;margin-bottom:48px}.grid{grid-template-columns:repeat(2,1fr)}main{padding-top:38px}}
  </style>
</head>
<body><main>
  <header><div class="mark">MIZUKI THE MECH / PUBLIC METRICS</div><div class="lede"><h1>Commercial performance.</h1><p class="promise">Track paid jobs, opened and merged pull requests, refunds, external maintainers, and operating results.</p><p>Mizuki works only on small, maintainer-authorized issues in public repositories. Maintainers retain review and merge control.</p><p>Estimated execution costs include recorded AI model usage and sandbox runtime. Provider billing adjustments, Solana and payment fees, and infrastructure costs are not yet included, so gross margin is not verified.</p><p>Token creator-fee distributions are reported separately in SOL. They are not customer-job revenue and do not increase refund capacity.</p></div></header>
  <section class="grid">
    <div class="cell"><div class="label">Paid jobs</div><div class="value" data-key="paidJobs">—</div></div>
    <div class="cell"><div class="label">Pull requests opened</div><div class="value" data-key="deliveredPrs">—</div></div>
    <div class="cell"><div class="label">Pull requests merged</div><div class="value" data-key="mergedPrs">—</div></div>
    <div class="cell"><div class="label">External maintainers</div><div class="value" data-key="externalMaintainers">—</div></div>
    <div class="cell"><div class="label">External repositories</div><div class="value" data-key="externalRepositories">—</div></div>
    <div class="cell"><div class="label">Refund completion rate</div><div class="value" data-key="refundSuccessRate">—</div></div>
    <div class="cell"><div class="label">Recognized revenue</div><div class="value" data-key="recognizedRevenueUsd">—</div></div>
    <div class="cell"><div class="label">Token creator-fee distributions (platform reported)</div><div class="value" data-key="platformReportedCreatorFeesSentLamports">—</div></div>
    <div class="cell"><div class="label">Estimated execution costs</div><div class="value" data-key="variableRouteCostEstimateUsd">—</div></div>
    <div class="cell"><div class="label">Gross margin status</div><div class="value" data-key="grossMarginStatus">—</div></div>
    <div class="cell"><div class="label">Bounty payouts</div><div class="value" data-key="bountiesReleased">—</div></div>
    <div class="cell"><div class="label">Planned improvement allocation</div><div class="value" data-key="plannedImprovementAllocationUsd">—</div></div>
  </section>
  <footer><span>x402 USDC · GitHub App · UsePod</span><span data-updated>Updating metrics…</span></footer>
</main><script>
const money=new Set(['recognizedRevenueUsd','variableRouteCostEstimateUsd','plannedImprovementAllocationUsd']);
function sol(lamports){const value=BigInt(lamports);const whole=value/1000000000n;const fraction=value%1000000000n;return fraction===0n?whole+' SOL':whole+'.'+fraction.toString().padStart(9,'0').replace(/0+$/,'')+' SOL'}
async function refresh(){
  try{
    const response=await fetch('/v1/metrics');
    if(!response.ok)throw new Error('metrics unavailable');
    const metrics=await response.json();
    const attempts=metrics.refundCount+metrics.refundPending;
    if(!Number.isFinite(attempts)||!metrics.updatedAt)throw new Error('metrics incomplete');
    const updates=[];
    for(const element of document.querySelectorAll('[data-key]')){
      const key=element.dataset.key;
      const value=metrics[key];
      if(value===undefined)throw new Error('metrics incomplete');
      const text=key==='refundSuccessRate'
        ?(attempts===0||value===null?'Not yet measured':(value*100).toFixed(0)+'%')
        :key==='platformReportedCreatorFeesSentLamports'
          ?sol(value)
          :key==='grossMarginStatus'
            ?'Not verified'
            :money.has(key)
              ?'$'+value.toFixed(2)
              :String(value);
      updates.push([element,text]);
    }
    for(const [element,value] of updates)element.textContent=value;
    document.querySelector('[data-updated]').textContent='Updated '+new Date(metrics.updatedAt).toLocaleString('en',{timeZone:'UTC',timeZoneName:'short'});
  }catch{
    for(const element of document.querySelectorAll('[data-key]'))element.textContent='Unavailable';
    document.querySelector('[data-updated]').textContent='Live metrics unavailable';
  }
}
refresh();setInterval(refresh,15000);
</script></body></html>`;
