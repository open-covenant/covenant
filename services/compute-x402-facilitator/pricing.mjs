import { lstat, readFile } from 'node:fs/promises';
import { isAbsolute } from 'node:path';

const MILLION = 1_000_000n;
const MAX_SAFE = BigInt(Number.MAX_SAFE_INTEGER);
const MAX_U64 = (1n << 64n) - 1n;
const FIELDS = ['schema_version', 'id', 'revision', 'model', 'base_micro_usdc', 'output_cap_micro_usdc_per_million', 'max_output_tokens', 'valid_from_ms', 'valid_until_ms'];
const INTEGER_FIELDS = FIELDS.filter(key => !['id', 'model'].includes(key));
const keysAre = (value, fields) => value && typeof value === 'object' && !Array.isArray(value)
  && Object.keys(value).sort().join(',') === [...fields].sort().join(',');
const require = condition => { if (!condition) throw new Error('invalid pricing configuration'); };
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;
const uuid = value => typeof value === 'string' && UUID.test(value) && value !== '00000000-0000-0000-0000-000000000000';
const model = value => typeof value === 'string' && value.trim() && value !== 'any'
  && Buffer.byteLength(value) <= 128 && !/[\u0000-\u001f\u007f-\u009f]/u.test(value);

// Structural validation also applies to sealed-payment recovery, independently
// of current tariff configuration. Every child identity and price is frozen.
export function batchPricingTotal(manifest) {
  if (!keysAre(manifest, ['version', 'batch_id', 'children']) || manifest.version !== 1
    || !uuid(manifest.batch_id) || !Array.isArray(manifest.children)
    || manifest.children.length < 1 || manifest.children.length > 32) return undefined;
  let total = 0n;
  const ids = new Set();
  for (const child of manifest.children) {
    if (!(keysAre(child, ['job_id', 'model', 'max_output_tokens', 'amount_atomic'])
        || keysAre(child, ['job_id', 'model', 'max_output_tokens', 'amount_atomic', 'pricing']))
      || !uuid(child.job_id) || ids.has(child.job_id) || !model(child.model)
      || !Number.isSafeInteger(child.max_output_tokens) || child.max_output_tokens < 1 || child.max_output_tokens > 32_768
      || !Number.isSafeInteger(child.amount_atomic) || child.amount_atomic < 1
      || (child.pricing != null && child.pricing !== 'model_tariff_v1')) return undefined;
    ids.add(child.job_id);
    total += BigInt(child.amount_atomic);
    if (total > MAX_SAFE) return undefined;
  }
  return total;
}

// Saved sealed payments may predate public SDK-safe tariff configuration.
// Their amount is still a canonical positive u64, never a coerced JS number.
export function validPaymentAmount(amount) {
  return typeof amount === 'string' && /^[1-9][0-9]{0,19}$/.test(amount) && BigInt(amount) <= MAX_U64;
}

function price(tariff, cap) {
  return tariff.base + (cap * tariff.rate + MILLION - 1n) / MILLION;
}
function tariff(value) {
  require(keysAre(value, FIELDS));
  require(INTEGER_FIELDS.every(key => Number.isSafeInteger(value[key]) && value[key] >= 0));
  require(value.schema_version === 1 && value.revision > 0
    && typeof value.id === 'string' && /^[A-Za-z0-9_.-]{1,128}$/.test(value.id)
    && typeof value.model === 'string' && value.model.trim() && value.model !== 'any'
    && Buffer.byteLength(value.model) <= 128 && !/[\u0000-\u001f\u007f-\u009f]/u.test(value.model)
    && value.max_output_tokens > 0 && value.max_output_tokens <= 32_768
    && value.valid_until_ms > value.valid_from_ms);
  const result = { model: value.model, base: BigInt(value.base_micro_usdc), rate: BigInt(value.output_cap_micro_usdc_per_million),
    cap: BigInt(value.max_output_tokens), from: value.valid_from_ms, until: value.valid_until_ms };
  require(price(result, 1n) > 0n && price(result, result.cap) <= MAX_SAFE);
  return result;
}

// Solve ceil(cap * rate / 1e6) = amount - base for an integer cap. Merely
// accepting the min/max range would allow prices no configured cap produces.
function hasPrice(tariff, amount) {
  const variable = amount - tariff.base;
  if (tariff.rate === 0n) return variable === 0n;
  if (variable <= 0n) return false;
  const first = (variable - 1n) * MILLION / tariff.rate + 1n;
  const last = variable * MILLION / tariff.rate;
  return first <= last && first <= tariff.cap && last >= 1n;
}

export function createPricingPolicy({ priceAtomic, tariffRows, tariffConfigError = false, batchEnabled = false }) {
  try {
    require(!tariffConfigError && validPaymentAmount(priceAtomic) && BigInt(priceAtomic) <= MAX_SAFE);
    const legacy = BigInt(priceAtomic), retail = [], definitions = [], models = new Set();
    if (tariffRows !== undefined) {
      require(Array.isArray(tariffRows) && tariffRows.length > 0 && tariffRows.length <= 32);
      for (const row of tariffRows) {
        require(keysAre(row, ['native', 'retail']));
        const native = tariff(row.native), sale = tariff(row.retail);
        require(native.model === sale.model && native.cap === sale.cap && native.from === sale.from && native.until === sale.until && !models.has(sale.model));
        // Match the gateway's coverage check, including asymmetric rounding.
        for (let cap = 1n; cap <= sale.cap; cap++) require(price(sale, cap) >= price(native, cap));
        models.add(sale.model); retail.push(sale); definitions.push(structuredClone(row.retail));
      }
    }
    return {
      valid: true,
      // Return a copy of the policy actually loaded, never caller-owned config.
      snapshot: () => ({ legacyPriceAtomic: priceAtomic, retailTariffs: structuredClone(definitions), batchEnabled: batchEnabled === true }),
      allows(amount, now) {
        if (!validPaymentAmount(amount) || !Number.isSafeInteger(now) || now < 0) return false;
        const value = BigInt(amount);
        return value === legacy || retail.some(t => t.from <= now && now < t.until && hasPrice(t, value));
      },
      allowsBatch(manifest, amount, now) {
        if (batchEnabled !== true || !validPaymentAmount(amount) || !Number.isSafeInteger(now) || now < 0
          || batchPricingTotal(manifest) !== BigInt(amount) || manifest.children.length > 2) return false;
        return manifest.children.every(child => child.pricing == null
          ? BigInt(child.amount_atomic) === legacy
          : retail.some(t => t.model === child.model && t.from <= now && now < t.until
            && BigInt(child.max_output_tokens) <= t.cap
            && price(t, BigInt(child.max_output_tokens)) === BigInt(child.amount_atomic)));
      },
    };
  } catch {
    // Bad optional configuration closes fresh authorization, not recovery of
    // an already sealed payment whose original terms remain authoritative.
    return { valid: false, allows: () => false, allowsBatch: () => false };
  }
}

export async function readTariffConfig(file) {
  if (file === undefined) return {};
  try {
    require(typeof file === 'string' && isAbsolute(file));
    const metadata = await lstat(file);
    require(metadata.isFile() && !metadata.isSymbolicLink() && metadata.size <= 262_144);
    const bytes = await readFile(file);
    require(bytes.length <= 262_144);
    return { tariffRows: JSON.parse(bytes.toString('utf8')) };
  } catch {
    return { tariffConfigError: true };
  }
}
