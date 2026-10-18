import fs from 'node:fs';
import path from 'node:path';

const P='COVENANT_INFERENCE_PROVIDER_';
export class PolicyError extends Error { constructor(field){super(`Invalid provider policy: ${field}`);this.name='PolicyError';} }
const positive=(env,key,max=1_000_000_000_000,fallback)=>{
 const raw=env[P+key]??fallback;
 if(typeof raw!=='string'||!/^\d+$/.test(raw))throw new PolicyError(key);
 const n=Number(raw);if(!Number.isSafeInteger(n)||n<1||n>max)throw new PolicyError(key);return n;
};
const secret=(env,key)=>{
 if(env[key]&&env[`${key}_FILE`])throw new PolicyError(key);
 let value=env[key];
 if(env[`${key}_FILE`]){try{const file=env[`${key}_FILE`];if(!path.isAbsolute(file)||!fs.lstatSync(file).isFile())throw new Error();value=fs.readFileSync(file,'utf8').trim();}catch{throw new PolicyError(`${key}_FILE`);}}
 if(typeof value!=='string'||value.length<24||value.length>1024||/[\s\x00-\x1f\x7f]/.test(value))throw new PolicyError(key);
 return value;
};
export function configFromEnv(env=process.env){
 const enabled=env[P+'ENABLED']??'false';if(!['true','false'].includes(enabled))throw new PolicyError('ENABLED');
 const port=positive(env,'PORT',65535,'4044');
 if(enabled==='false')return Object.freeze({enabled:false,port});
 const model=env[P+'MODEL'];if(typeof model!=='string'||!model.length||model.length>160||!/^[A-Za-z0-9._:/-]+$/.test(model))throw new PolicyError('MODEL');
 const dbPath=env[P+'DB_PATH'];if(!dbPath||!path.isAbsolute(dbPath)||dbPath===':memory:')throw new PolicyError('DB_PATH');
 const config={enabled:true,port,model,dbPath,token:secret(env,'USEPOD_TOKEN'),bearer:secret(env,P+'BEARER_TOKEN'),
  lifetimeBudget:positive(env,'LIFETIME_BUDGET_MICRO_USDC'),dailyBudget:positive(env,'DAILY_BUDGET_MICRO_USDC'),
  inputPrice:positive(env,'MAX_PRICE_INPUT_MICRO_USDC_PER_M'),outputPrice:positive(env,'MAX_PRICE_OUTPUT_MICRO_USDC_PER_M'),
  maxInputBytes:positive(env,'MAX_INPUT_BYTES',65_536),maxInputTokens:positive(env,'MAX_INPUT_TOKENS',2_000_000),
  maxOutputTokens:positive(env,'MAX_OUTPUT_TOKENS',32_768),timeoutMs:positive(env,'UPSTREAM_TIMEOUT_MS',120_000,'60000')};
 if(config.dailyBudget>config.lifetimeBudget)throw new PolicyError('DAILY_BUDGET_MICRO_USDC');
 if(config.maxInputTokens<config.maxInputBytes+1024)throw new PolicyError('MAX_INPUT_TOKENS must cover input bytes and framing');
 return Object.freeze(config);
}
