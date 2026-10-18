import { DatabaseSync } from 'node:sqlite';
import { mkdirSync, lstatSync, existsSync, openSync, closeSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { randomUUID } from 'node:crypto';
import { assert } from './proof.mjs';

// A cumulative budget is deliberately conservative: an uncertain authorization
// remains reserved after a crash, expiry or server error. Never auto-re-sign it.
export class Journal {
  constructor(path, scope, ceiling) {
    this.path = resolve(path); this.scope = scope; this.ceiling = ceiling;
    mkdirSync(dirname(this.path), { recursive: true, mode: 0o700 });
    if (!existsSync(this.path)) {
      try { const fd = openSync(this.path, 'wx', 0o600); closeSync(fd); }
      catch (error) { if (error.code !== 'EEXIST') throw error; }
    }
    const stat = lstatSync(this.path);
    assert(stat.isFile() && !stat.isSymbolicLink() && !(stat.mode & 0o077), 'journal_must_be_private_regular_file');
    this.db = new DatabaseSync(this.path);
    this.db.exec('PRAGMA busy_timeout=5000; PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;');
    this.db.exec(`CREATE TABLE IF NOT EXISTS purchases (
      scope TEXT NOT NULL, id TEXT NOT NULL, body TEXT NOT NULL, digest TEXT NOT NULL,
      quote TEXT, amount INTEGER NOT NULL DEFAULT 0, payment TEXT, result TEXT,
      owner TEXT, lease_until INTEGER NOT NULL DEFAULT 0, created INTEGER NOT NULL,
      PRIMARY KEY(scope,id));`);
  }
  transaction(fn) {
    this.db.exec('BEGIN IMMEDIATE');
    try { const value = fn(); this.db.exec('COMMIT'); return value; }
    catch (error) { this.db.exec('ROLLBACK'); throw error; }
  }
  get(id) { return this.db.prepare('SELECT * FROM purchases WHERE scope=? AND id=?').get(this.scope, id); }
  claim(id, body, digest) {
    return this.transaction(() => {
      let row = this.get(id); const now = Date.now(); const owner = randomUUID();
      if (row) {
        assert(row.body === body && row.digest === digest, 'idempotency_request_mismatch');
        if (row.result) return row;
        assert(row.lease_until < now, 'purchase_in_progress');
      } else {
        this.db.prepare('INSERT INTO purchases(scope,id,body,digest,created) VALUES(?,?,?,?,?)').run(this.scope, id, body, digest, now);
      }
      this.db.prepare('UPDATE purchases SET owner=?,lease_until=? WHERE scope=? AND id=?').run(owner, now + 300_000, this.scope, id);
      return this.get(id);
    });
  }
  check(id, owner) { const row = this.get(id); assert(row?.owner === owner && row.lease_until > Date.now(), 'purchase_claim_expired'); return row; }
  reserve(id, owner, quote, amount) {
    return this.transaction(() => {
      const row = this.check(id, owner); assert(!row.quote && !row.payment, 'purchase_already_reserved');
      const used = this.db.prepare('SELECT COALESCE(SUM(amount),0) AS amount FROM purchases WHERE scope=?').get(this.scope).amount;
      assert(used + amount <= this.ceiling, 'cumulative_spend_cap_exceeded');
      this.db.prepare('UPDATE purchases SET quote=?,amount=? WHERE scope=? AND id=?').run(JSON.stringify(quote), amount, this.scope, id);
    });
  }
  signed(id, owner, payment) {
    this.transaction(() => { const row = this.check(id, owner); assert(row.quote && !row.payment, 'invalid_payment_transition');
      this.db.prepare('UPDATE purchases SET payment=? WHERE scope=? AND id=?').run(payment, this.scope, id); });
  }
  complete(id, owner, result) {
    this.transaction(() => { this.check(id, owner); this.db.prepare('UPDATE purchases SET result=? WHERE scope=? AND id=?').run(result, this.scope, id); });
  }
  release(id, owner) { this.db.prepare('UPDATE purchases SET owner=NULL,lease_until=0 WHERE scope=? AND id=? AND owner=?').run(this.scope, id, owner); }
  close() { this.db.close(); }
}
