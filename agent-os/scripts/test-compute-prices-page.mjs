import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import vm from 'node:vm';
import test from 'node:test';

const html = readFileSync(new URL('../crates/covenant-compute-coordinator/src/prices.html', import.meta.url), 'utf8');
const script = html.match(/<script>([\s\S]*?)<\/script>/)[1].split("for(const button of document.querySelectorAll('[data-days]'))")[0];
class Element {
  constructor(tag) { this.tag=tag;this.children=[];this.attributes={};this.value='';this.textContent=''; }
  append(...children) { this.children.push(...children); }
  replaceChildren(...children) { this.children=children; }
  setAttribute(key,value) { this.attributes[key]=value; }
  set innerHTML(_) { throw new Error('Untrusted HTML insertion is forbidden'); }
  get options() { return this.children; }
}
function page() {
  const elements=new Map();
  const document={getElementById(id) {if(!elements.has(id))elements.set(id,new Element('div'));return elements.get(id);},createElement(tag){return new Element(tag);},createElementNS(_,tag){return new Element(tag);}};
  const context=vm.createContext({document});vm.runInContext(script,context);return {context,elements,document};
}
const point=(source='provider_market_quote',currency='USD')=>({gpu_model:'L40S',provider:'vast.ai',source,currency,day_start_ms:86400000,first_observed_at_ms:86401000,last_observed_at_ms:86402000,sample_count:2,mean_micros_per_gpu_hour:350000,min_micros_per_gpu_hour:300000,max_micros_per_gpu_hour:400000,proof_paths:[]});
const history=points=>({version:1,generated_at_ms:86403000,coordinator_pubkey_b58:'coordinator',settlement_mint_b58:'mint',points});
function texts(node) {return [node.textContent,...node.children.flatMap(texts)].join(' ');}
function nodes(node,tag) {return [...(node.tag===tag?[node]:[]),...node.children.flatMap(child=>nodes(child,tag))];}

test('millionth prices retain exact decimal precision, including sub-cent values',()=>{const {context}=page();for(const [value,expected] of [[0,'0.00'],[1,'0.000001'],[10000,'0.01'],[1200000,'1.20'],[1234567,'1.234567']])assert.equal(context.price(value),expected);});
test('market, provisioning and settled observations stay distinct; labels remain text',()=>{const {context,document}=page();const market=point();market.gpu_model='<img src=x onerror=alert(1)>';const quoted={...market,source:'operator_reported_provider_quote'};const settled={...market,source:'settled_lease_charge',currency:'USDC',mean_micros_per_gpu_hour:600000,min_micros_per_gpu_hour:500000,max_micros_per_gpu_hour:700000};const data=history([market,quoted,settled]);context.validateHistory(data);context.render(data);assert.equal(document.getElementById('quotes').children.length,2);assert.equal(document.getElementById('settlements').children.length,1);assert.match(texts(document.getElementById('quotes')),/Market sample/);assert.match(texts(document.getElementById('quotes')),/Provisioned offer/);assert.match(texts(document.getElementById('quotes')),/<img src=x onerror=alert\(1\)>/);assert.equal(nodes(document.getElementById('quotes'),'img').length,0);assert.match(texts(document.getElementById('settlements')),/0\.60/);});
test('an empty history shows zero samples and no fabricated chart points',()=>{const {context,document}=page();context.render(history([]));assert.equal(document.getElementById('quote-count').textContent,'0');assert.equal(document.getElementById('settled-count').textContent,'0');assert.match(texts(document.getElementById('quotes')),/No provider quotes in this view/);assert.equal(nodes(document.getElementById('quotes'),'circle').length,0);});
test('proof links cannot leave the same-origin evidence routes',()=>{const {context}=page();for(const bad of ['https://evil.test/proof','//evil.test','javascript:alert(1)','/prices/proofs/../admin','/prices/proofs/10000000-0000-0000-0000-000000000001?secret=x']){const p=point('settled_lease_charge','USDC');p.proof_paths=[bad];assert.throws(()=>context.validateHistory(history([p])),/invalid observation/);}assert.equal(context.validProofPath('/prices/proofs/10000000-0000-0000-0000-000000000001'),true);});
test('wrong asset, imprecise integers and inconsistent rates fail closed',()=>{const {context}=page();for(const patch of [{currency:'USDC'},{sample_count:0},{mean_micros_per_gpu_hour:1.1},{mean_micros_per_gpu_hour:Number.MAX_SAFE_INTEGER+1},{min_micros_per_gpu_hour:500000}])assert.throws(()=>context.validateHistory(history([{...point(),...patch}])),/invalid observation/);});
test('missing days stay empty instead of connected or zero-filled',()=>{const {context}=page();const p=point();const chart=context.makeChart([p,{...p,day_start_ms:7*86400000}],'USD',0,10*86400000);assert.equal(nodes(chart,'circle').length,2);assert.equal(nodes(chart,'polyline').length,0);assert.equal(nodes(chart,'path').length,0);});
test('provider outage is visible and cannot be described as empty capacity',()=>{const {context,document}=page();const status={identity:{provider:'vast.ai',gpu_models:['L40S']},state:'provider_unavailable',stale:true,persistence_healthy:true,last_success_at_ms:86400000,last_attempt_at_ms:86403000,last_sample_offer_count:8,sample_interval_seconds:3600,maximum_offers_per_model:8};context.validateHistory({...history([]),provider_sampling:status});context.renderSampling(status);assert.match(texts(document.getElementById('sampling-status')),/unavailable/);assert.match(texts(document.getElementById('sampling-status')),/does not mean provider capacity is empty/);assert.match(document.getElementById('sampling-status').className,/warning/);});
test('a failed refresh removes previously displayed rates and reports unavailable',async()=>{const {context,document}=page();context.render(history([point()]));context.fetch=async()=>({ok:false,status:503});context.AbortController=AbortController;context.setTimeout=setTimeout;context.clearTimeout=clearTimeout;await context.load();assert.equal(document.getElementById('quote-count').textContent,'—');assert.match(texts(document.getElementById('quotes')),/History unavailable/);assert.equal(nodes(document.getElementById('quotes'),'circle').length,0);assert.match(document.getElementById('state').textContent,/HTTP 503/);assert.equal(document.getElementById('refresh').disabled,false);});
