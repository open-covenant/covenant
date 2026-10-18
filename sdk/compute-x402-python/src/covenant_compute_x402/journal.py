"""SQLite-compatible with the accepted Node client; uncertain spend stays held."""
from __future__ import annotations

import os
from pathlib import Path
import sqlite3
import stat
import time
from contextlib import contextmanager
from uuid import uuid4

from .proof import compact, require


def now_ms() -> int:
    return time.time_ns() // 1_000_000


class Journal:
    def __init__(self, path: str | Path, scope: str, ceiling: int):
        # Do not resolve symlinks before lstat; the journal itself must be private.
        self.path = Path(os.path.abspath(path))
        self.scope, self.ceiling = scope, ceiling
        self.path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
        try:
            fd = os.open(self.path, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
            os.close(fd)
        except FileExistsError:
            pass
        mode = self.path.lstat().st_mode
        require(stat.S_ISREG(mode) and not stat.S_ISLNK(mode) and not mode & 0o077, "journal_must_be_private_regular_file")
        self.db = sqlite3.connect(self.path, timeout=5, isolation_level=None)
        self.db.row_factory = sqlite3.Row
        self.db.executescript("""PRAGMA busy_timeout=5000; PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;
          CREATE TABLE IF NOT EXISTS purchases (
          scope TEXT NOT NULL, id TEXT NOT NULL, body TEXT NOT NULL, digest TEXT NOT NULL,
          quote TEXT, amount INTEGER NOT NULL DEFAULT 0, payment TEXT, result TEXT,
          owner TEXT, lease_until INTEGER NOT NULL DEFAULT 0, created INTEGER NOT NULL,
          PRIMARY KEY(scope,id));""")

    @contextmanager
    def transaction(self):
        self.db.execute("BEGIN IMMEDIATE")
        try:
            yield
            self.db.execute("COMMIT")
        except BaseException:
            self.db.execute("ROLLBACK")
            raise

    def get(self, request_id: str) -> dict | None:
        row = self.db.execute("SELECT * FROM purchases WHERE scope=? AND id=?", (self.scope, request_id)).fetchone()
        return dict(row) if row else None

    def claim(self, request_id: str, body: str, digest: str) -> dict:
        with self.transaction():
            row, now, owner = self.get(request_id), now_ms(), str(uuid4())
            if row:
                require(row["body"] == body and row["digest"] == digest, "idempotency_request_mismatch")
                if row["result"]:
                    return row
                require(row["lease_until"] < now, "purchase_in_progress")
            else:
                self.db.execute("INSERT INTO purchases(scope,id,body,digest,created) VALUES(?,?,?,?,?)", (self.scope, request_id, body, digest, now))
            self.db.execute("UPDATE purchases SET owner=?,lease_until=? WHERE scope=? AND id=?", (owner, now + 300_000, self.scope, request_id))
            return self.get(request_id)

    def check(self, request_id: str, owner: str) -> dict:
        row = self.get(request_id)
        require(row and row["owner"] == owner and row["lease_until"] > now_ms(), "purchase_claim_expired")
        return row

    def reserve(self, request_id: str, owner: str, quote: dict, amount: int) -> None:
        with self.transaction():
            row = self.check(request_id, owner)
            require(not row["quote"] and not row["payment"], "purchase_already_reserved")
            used = self.db.execute("SELECT COALESCE(SUM(amount),0) FROM purchases WHERE scope=?", (self.scope,)).fetchone()[0]
            require(used + amount <= self.ceiling, "cumulative_spend_cap_exceeded")
            self.db.execute("UPDATE purchases SET quote=?,amount=? WHERE scope=? AND id=?", (compact(quote), amount, self.scope, request_id))

    def signed(self, request_id: str, owner: str, payment: str) -> None:
        with self.transaction():
            row = self.check(request_id, owner)
            require(row["quote"] and not row["payment"], "invalid_payment_transition")
            self.db.execute("UPDATE purchases SET payment=? WHERE scope=? AND id=?", (payment, self.scope, request_id))

    def complete(self, request_id: str, owner: str, result: str) -> None:
        with self.transaction():
            self.check(request_id, owner)
            self.db.execute("UPDATE purchases SET result=? WHERE scope=? AND id=?", (result, self.scope, request_id))

    def release(self, request_id: str, owner: str) -> None:
        self.db.execute("UPDATE purchases SET owner=NULL,lease_until=0 WHERE scope=? AND id=? AND owner=?", (self.scope, request_id, owner))

    def close(self) -> None:
        self.db.close()
