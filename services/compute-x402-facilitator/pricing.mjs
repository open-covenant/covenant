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

export function createPricingPolicy({ priceAtomic, tariffRows, tariffConfigError = false }) {
  try {
    require(!tariffConfigError && validPaymentAmount(priceAtomic) && BigInt(priceAtomic) <= MAX_SAFE);
    const legacy = BigInt(priceAtomic), retail = [], models = new Set();
    if (tariffRows !== undefined) {
      require(Array.isArray(tariffRows) && tariffRows.length > 0 && tariffRows.length <= 32);
      for (const row of tariffRows) {
        require(keysAre(row, ['native', 'retail']));
        const native = tariff(row.native), sale = tariff(row.retail);
        require(native.model === sale.model && native.cap === sale.cap && native.from === sale.from && native.until === sale.until && !models.has(sale.model));
        // Match the gateway's coverage check, including asymmetric rounding.
        for (let cap = 1n; cap <= sale.cap; cap++) require(price(sale, cap) >= price(native, cap));
        models.add(sale.model); retail.push(sale);
      }
    }
    return {
      valid: true,
      allows(amount, now) {
        if (!validPaymentAmount(amount) || !Number.isSafeInteger(now) || now < 0) return false;
        const value = BigInt(amount);
        return value === legacy || retail.some(t => t.from <= now && now < t.until && hasPrice(t, value));
      },
    };
  } catch {
    // Bad optional configuration closes fresh authorization, not recovery of
    // an already sealed payment whose original terms remain authoritative.
    return { valid: false, allows: () => false };
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
