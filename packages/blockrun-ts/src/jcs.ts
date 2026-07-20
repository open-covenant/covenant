/**
 * RFC 8785 JSON Canonicalization Scheme, matching the Rust `serde_jcs` the
 * `covenant-blockrun` crate hashes with, so a receipt's digest is identical
 * whether it was built here or in the daemon. Object keys are sorted by UTF-16
 * code unit, arrays keep their order, numbers use the ECMAScript shortest
 * round-trip (what `JSON.stringify` emits), and there is no insignificant
 * whitespace.
 */
export function canonicalize(value: unknown): string {
  if (value === null) return "null";
  if (typeof value === "boolean") return value ? "true" : "false";
  if (typeof value === "number") {
    if (!Number.isFinite(value)) {
      throw new Error("cannot canonicalize a non-finite number");
    }
    return JSON.stringify(value);
  }
  if (typeof value === "string") return JSON.stringify(value);
  if (Array.isArray(value)) {
    return "[" + value.map(canonicalize).join(",") + "]";
  }
  if (typeof value === "object") {
    const obj = value as Record<string, unknown>;
    const keys = Object.keys(obj)
      .filter((k) => obj[k] !== undefined)
      .sort(codeUnitCompare);
    return (
      "{" +
      keys.map((k) => JSON.stringify(k) + ":" + canonicalize(obj[k])).join(",") +
      "}"
    );
  }
  throw new Error(`cannot canonicalize value of type ${typeof value}`);
}

/** Sort by UTF-16 code unit, which is what String comparison already does. */
function codeUnitCompare(a: string, b: string): number {
  return a < b ? -1 : a > b ? 1 : 0;
}
