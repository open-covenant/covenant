export interface Cursor {
  ms: number;
  id: string;
}

export const START: Cursor = { ms: 0, id: '' };

export function encodeCursor(ms: number, id: string): string {
  return Buffer.from(`${ms}:${id}`, 'utf8').toString('base64url');
}

// Accepts our opaque token, a bare epoch-ms, or an ISO-8601 string. The latter
// two resolve to `{ ms, id: '' }`, which `after` treats as inclusive of that ms.
export function decodeCursor(raw: string): Cursor {
  if (/^\d+$/.test(raw)) return { ms: Number(raw), id: '' };

  let decoded = '';
  try {
    decoded = Buffer.from(raw, 'base64url').toString('utf8');
  } catch {
    decoded = '';
  }
  const sep = decoded.indexOf(':');
  if (sep > 0 && /^\d+$/.test(decoded.slice(0, sep))) {
    return { ms: Number(decoded.slice(0, sep)), id: decoded.slice(sep + 1) };
  }

  const t = Date.parse(raw);
  if (!Number.isNaN(t)) return { ms: t, id: '' };
  throw new Error('invalid cursor');
}

export function after(ms: number, id: string, c: Cursor): boolean {
  return ms > c.ms || (ms === c.ms && id > c.id);
}
