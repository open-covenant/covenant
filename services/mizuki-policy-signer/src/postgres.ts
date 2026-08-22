import { createHash } from 'node:crypto';
import { Pool, type PoolClient, type QueryResultRow } from 'pg';
import type {
  BindChallenge,
  GitHubIdentityGrant,
  OperationRecord,
  RefundLiability,
  ReserveOperation,
} from './domain.js';
import { PolicyError } from './domain.js';
import type { OperationPatch, OperationStore, StoreStats } from './store.js';

const SIGNER_SCHEMA = `
      CREATE TABLE IF NOT EXISTS mizuki_signer_operations (
        id uuid PRIMARY KEY,
        idempotency_key text NOT NULL UNIQUE,
        resource_key text NOT NULL UNIQUE,
        request_hash text NOT NULL,
        kind text NOT NULL,
        status text NOT NULL CHECK (status IN ('reserved', 'prepared', 'broadcasting', 'submitted', 'reconciling', 'finalized', 'rejected')),
        amount_usd_cents integer NOT NULL CHECK (amount_usd_cents >= 0),
        spend_bucket text NOT NULL CHECK (spend_bucket IN ('refund', 'escrow', 'none')),
        asset text NOT NULL,
        recipient text NOT NULL,
        details jsonb NOT NULL,
        prepared jsonb,
        transaction_signature text,
        error_code text,
        error_message text,
        lease_owner text,
        lease_expires_at timestamptz,
        version integer NOT NULL DEFAULT 0,
        created_at timestamptz NOT NULL,
        updated_at timestamptz NOT NULL
      );
      ALTER TABLE mizuki_signer_operations
        ADD COLUMN IF NOT EXISTS spend_bucket text;
      UPDATE mizuki_signer_operations
         SET spend_bucket = CASE
           WHEN kind = 'refund' THEN 'refund'
           WHEN kind IN ('escrow_create', 'escrow_reserve') THEN 'escrow'
           ELSE 'none'
         END
       WHERE spend_bucket IS NULL;
      UPDATE mizuki_signer_operations SET kind = 'escrow_reserve' WHERE kind = 'escrow_create';
      ALTER TABLE mizuki_signer_operations ALTER COLUMN spend_bucket SET NOT NULL;
      ALTER TABLE mizuki_signer_operations
        DROP CONSTRAINT IF EXISTS mizuki_signer_operations_kind_check;
      ALTER TABLE mizuki_signer_operations
        ADD CONSTRAINT mizuki_signer_operations_kind_check
        CHECK (kind IN ('refund', 'escrow_reserve', 'escrow_bind', 'escrow_release', 'escrow_refund'));
      ALTER TABLE mizuki_signer_operations
        DROP CONSTRAINT IF EXISTS mizuki_signer_operations_spend_bucket_check;
      ALTER TABLE mizuki_signer_operations
        ADD CONSTRAINT mizuki_signer_operations_spend_bucket_check
        CHECK (spend_bucket IN ('refund', 'escrow', 'none'));
      CREATE TABLE IF NOT EXISTS mizuki_signer_github_identity_grants (
        id uuid PRIMARY KEY,
        github_id text NOT NULL,
        login text NOT NULL,
        issued_at timestamptz NOT NULL,
        expires_at timestamptz NOT NULL,
        consumed_at timestamptz,
        challenge_id uuid UNIQUE,
        CHECK (expires_at > issued_at)
      );
      CREATE TABLE IF NOT EXISTS mizuki_signer_refund_liabilities (
        id uuid PRIMARY KEY,
        idempotency_key text NOT NULL UNIQUE,
        request_hash text NOT NULL,
        job_id text NOT NULL UNIQUE,
        settlement_signature text NOT NULL UNIQUE,
        payer text NOT NULL,
        treasury text NOT NULL,
        mint text NOT NULL,
        raw_amount numeric(78, 0) NOT NULL CHECK (raw_amount > 0),
        decimals integer NOT NULL CHECK (decimals BETWEEN 0 AND 18),
        amount_usd_cents integer NOT NULL CHECK (amount_usd_cents > 0),
        settlement_slot bigint NOT NULL CHECK (settlement_slot >= 0),
        settlement_block_time bigint NOT NULL CHECK (settlement_block_time > 0),
        created_at timestamptz NOT NULL,
        discharged_at timestamptz,
        discharge_evidence_hash text,
        discharge_evidence jsonb,
        discharge_idempotency_key text,
        discharge_request_hash text
      );
      ALTER TABLE mizuki_signer_refund_liabilities
        ADD COLUMN IF NOT EXISTS discharged_at timestamptz,
        ADD COLUMN IF NOT EXISTS discharge_evidence_hash text,
        ADD COLUMN IF NOT EXISTS discharge_evidence jsonb,
        ADD COLUMN IF NOT EXISTS discharge_idempotency_key text,
        ADD COLUMN IF NOT EXISTS discharge_request_hash text;
      CREATE UNIQUE INDEX IF NOT EXISTS mizuki_signer_refund_discharge_idempotency
        ON mizuki_signer_refund_liabilities (discharge_idempotency_key)
        WHERE discharge_idempotency_key IS NOT NULL;
      CREATE TABLE IF NOT EXISTS mizuki_signer_bind_challenges (
        id uuid PRIMARY KEY,
        escrow_operation_id uuid NOT NULL REFERENCES mizuki_signer_operations(id),
        binding_hash text NOT NULL,
        claimant_wallet text NOT NULL,
        claimant_github_id text NOT NULL,
        claimant_github_login text NOT NULL,
        message text NOT NULL,
        claim_expires_at timestamptz NOT NULL,
        issued_at timestamptz NOT NULL,
        expires_at timestamptz NOT NULL,
        consumed_at timestamptz,
        bind_operation_id uuid UNIQUE REFERENCES mizuki_signer_operations(id),
        CHECK (expires_at > issued_at),
        CHECK (claim_expires_at > issued_at)
      );
      ALTER TABLE mizuki_signer_bind_challenges
        ADD COLUMN IF NOT EXISTS claimant_github_id text;
      DO $$
      BEGIN
        IF EXISTS (
          SELECT 1 FROM mizuki_signer_bind_challenges WHERE claimant_github_id IS NULL
        ) THEN
          RAISE EXCEPTION 'existing binding challenges lack immutable GitHub identity IDs';
        END IF;
      END $$;
      ALTER TABLE mizuki_signer_bind_challenges
        ALTER COLUMN claimant_github_id SET NOT NULL;
      CREATE INDEX IF NOT EXISTS mizuki_signer_operations_spend_window
        ON mizuki_signer_operations (created_at)
        WHERE status <> 'rejected' AND amount_usd_cents > 0;
      CREATE INDEX IF NOT EXISTS mizuki_signer_operations_recovery
        ON mizuki_signer_operations (created_at)
        WHERE status NOT IN ('finalized', 'rejected');`;

export class PostgresOperationStore implements OperationStore {
  private readonly pool: Pool;

  constructor(connectionString: string) {
    this.pool = new Pool({
      connectionString,
      max: 10,
      statement_timeout: 15_000,
      idle_in_transaction_session_timeout: 15_000,
    });
  }

  async migrate(): Promise<void> {
    const client = await this.pool.connect();
    try {
      await client.query('BEGIN');
      await client.query("SELECT pg_advisory_xact_lock(hashtext('mizuki-policy-signer-schema'))");
      await client.query(`
        CREATE TABLE IF NOT EXISTS mizuki_schema_migrations (
          component text NOT NULL,
          version integer NOT NULL CHECK (version > 0),
          name text NOT NULL,
          checksum text NOT NULL CHECK (checksum ~ '^[a-f0-9]{64}$'),
          applied_at timestamptz NOT NULL DEFAULT now(),
          PRIMARY KEY (component, version)
        )
      `);
      const applied = await client.query<{ version: number; name: string; checksum: string }>(
        'SELECT version, name, checksum FROM mizuki_schema_migrations WHERE component = $1',
        ['policy-signer'],
      );
      const checksum = createHash('sha256').update(SIGNER_SCHEMA).digest('hex');
      if (applied.rows.some((row) => Number(row.version) !== 1)) {
        throw new Error('policy-signer database contains an unknown schema migration');
      }
      const current = applied.rows.find((row) => Number(row.version) === 1);
      if (
        current &&
        (current.name !== 'policy-and-custody-core' || current.checksum !== checksum)
      ) {
        throw new Error('policy-signer database migration does not match this build');
      }
      if (!current) {
        await client.query(SIGNER_SCHEMA);
        await client.query(
          `INSERT INTO mizuki_schema_migrations (component, version, name, checksum)
           VALUES ($1, $2, $3, $4)`,
          ['policy-signer', 1, 'policy-and-custody-core', checksum],
        );
      }
      await client.query('COMMIT');
    } catch (error) {
      await client.query('ROLLBACK');
      throw error;
    } finally {
      client.release();
    }
  }

  async registerRefundLiability(
    liability: RefundLiability,
    maxOutstandingRaw: string,
    dailyLimitUsdCents: number,
    _now: Date,
  ): Promise<RefundLiability> {
    const client = await this.pool.connect();
    try {
      await client.query('BEGIN ISOLATION LEVEL SERIALIZABLE');
      await client.query(
        "SELECT pg_advisory_xact_lock(hashtext('mizuki-signer-refund-liabilities'))",
      );
      const idempotent = await client.query(
        'SELECT * FROM mizuki_signer_refund_liabilities WHERE idempotency_key = $1',
        [liability.idempotencyKey],
      );
      if (idempotent.rows[0]) {
        const existing = mapLiability(idempotent.rows[0]);
        if (existing.requestHash !== liability.requestHash) {
          throw new PolicyError(
            'idempotency_conflict',
            'Idempotency key was already used for a different request',
            409,
          );
        }
        await client.query('COMMIT');
        return existing;
      }
      const conflict = await client.query(
        `SELECT settlement_signature, job_id
           FROM mizuki_signer_refund_liabilities
          WHERE settlement_signature = $1 OR job_id = $2`,
        [liability.settlementSignature, liability.jobId],
      );
      if (conflict.rows.some((row) => row.settlement_signature === liability.settlementSignature)) {
        throw new PolicyError(
          'settlement_liability_conflict',
          'Settlement is already registered to a refund liability',
          409,
        );
      }
      if (conflict.rows.length > 0) {
        throw new PolicyError(
          'job_liability_conflict',
          'Job is already registered to a refund liability',
          409,
        );
      }
      const rollingSpend = await client.query<{ total: string }>(
        `SELECT COALESCE(SUM(amount_usd_cents), 0)::text AS total
           FROM mizuki_signer_refund_liabilities
          WHERE created_at >= clock_timestamp() - interval '24 hours'`,
      );
      if (
        Number(rollingSpend.rows[0]?.total ?? 0) + liability.amountUsdCents >
        dailyLimitUsdCents
      ) {
        throw new PolicyError(
          'daily_limit_exceeded',
          'Rolling 24-hour refund liability limit exceeded',
          429,
          true,
        );
      }
      const outstanding = await client.query<{ total: string }>(
        `SELECT COALESCE(SUM(liability.raw_amount), 0)::text AS total
           FROM mizuki_signer_refund_liabilities liability
      LEFT JOIN mizuki_signer_operations operation
             ON operation.resource_key = 'refund:' || liability.settlement_signature
            AND operation.status = 'finalized'
          WHERE operation.id IS NULL AND liability.discharged_at IS NULL`,
      );
      if (
        BigInt(outstanding.rows[0]?.total ?? 0) + BigInt(liability.rawAmount) >
        BigInt(maxOutstandingRaw)
      ) {
        throw new PolicyError(
          'refund_pool_insufficient',
          'Protected refund pool cannot cover all registered liabilities',
          503,
          true,
        );
      }
      const result = await client.query(
        `INSERT INTO mizuki_signer_refund_liabilities (
           id, idempotency_key, request_hash, job_id, settlement_signature, payer,
           treasury, mint, raw_amount, decimals, settlement_slot,
           amount_usd_cents, settlement_block_time, created_at
         ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, clock_timestamp())
         RETURNING *`,
        [
          liability.id,
          liability.idempotencyKey,
          liability.requestHash,
          liability.jobId,
          liability.settlementSignature,
          liability.payer,
          liability.treasury,
          liability.mint,
          liability.rawAmount,
          liability.decimals,
          liability.settlementSlot,
          liability.amountUsdCents,
          liability.settlementBlockTimeUnixSeconds,
        ],
      );
      await client.query('COMMIT');
      return mapLiability(result.rows[0]);
    } catch (error) {
      await client.query('ROLLBACK').catch(() => undefined);
      throw error;
    } finally {
      client.release();
    }
  }

  async getRefundLiability(settlementSignature: string): Promise<RefundLiability | null> {
    const result = await this.pool.query(
      'SELECT * FROM mizuki_signer_refund_liabilities WHERE settlement_signature = $1',
      [settlementSignature],
    );
    return result.rows[0] ? mapLiability(result.rows[0]) : null;
  }

  async dischargeRefundLiability(
    liabilityId: string,
    idempotencyKey: string,
    requestHash: string,
    evidenceHash: string,
    evidence: Record<string, unknown>,
    _now: Date,
  ): Promise<RefundLiability> {
    const client = await this.pool.connect();
    try {
      await client.query('BEGIN ISOLATION LEVEL SERIALIZABLE');
      const result = await client.query(
        'SELECT * FROM mizuki_signer_refund_liabilities WHERE id = $1 FOR UPDATE',
        [liabilityId],
      );
      if (!result.rows[0]) {
        throw new PolicyError('refund_liability_not_found', 'Refund liability was not found', 404);
      }
      const liability = mapLiability(result.rows[0]);
      const idempotent = await client.query<{ id: string }>(
        `SELECT id FROM mizuki_signer_refund_liabilities
          WHERE discharge_idempotency_key = $1 AND id <> $2`,
        [idempotencyKey, liabilityId],
      );
      if (idempotent.rows[0]) {
        throw new PolicyError(
          'idempotency_conflict',
          'Idempotency key was already used for a different request',
          409,
        );
      }
      if (liability.dischargedAt) {
        if (
          liability.dischargeIdempotencyKey === idempotencyKey &&
          liability.dischargeRequestHash === requestHash
        ) {
          await client.query('COMMIT');
          return liability;
        }
        throw new PolicyError(
          'refund_liability_discharged',
          'Refund liability is already discharged',
          409,
        );
      }
      const refund = await client.query<{ id: string }>(
        `SELECT id FROM mizuki_signer_operations
          WHERE resource_key = $1 AND status <> 'rejected'`,
        [`refund:${liability.settlementSignature}`],
      );
      if (refund.rows[0]) {
        throw new PolicyError(
          'refund_already_started',
          'Refund liability cannot be discharged after refund execution starts',
          409,
        );
      }
      const updated = await client.query(
        `UPDATE mizuki_signer_refund_liabilities
            SET discharged_at = clock_timestamp(),
                discharge_evidence_hash = $2,
                discharge_evidence = $3,
                discharge_idempotency_key = $4,
                discharge_request_hash = $5
          WHERE id = $1
        RETURNING *`,
        [liabilityId, evidenceHash, evidence, idempotencyKey, requestHash],
      );
      await client.query('COMMIT');
      return mapLiability(updated.rows[0]);
    } catch (error) {
      await client.query('ROLLBACK').catch(() => undefined);
      throw error;
    } finally {
      client.release();
    }
  }

  async reserveRefund(
    input: ReserveOperation,
    liabilityId: string,
    _now: Date,
  ): Promise<OperationRecord> {
    const client = await this.pool.connect();
    try {
      await client.query('BEGIN ISOLATION LEVEL SERIALIZABLE');
      const liabilityResult = await client.query(
        'SELECT * FROM mizuki_signer_refund_liabilities WHERE id = $1 FOR UPDATE',
        [liabilityId],
      );
      const liability = liabilityResult.rows[0] ? mapLiability(liabilityResult.rows[0]) : null;
      if (!liability || liability.settlementSignature !== input.details.settlementSignature) {
        throw new PolicyError('refund_liability_not_found', 'Refund liability was not found', 404);
      }
      const idempotent = await this.findOne(client, 'idempotency_key', input.idempotencyKey);
      if (idempotent) {
        if (idempotent.requestHash !== input.requestHash) {
          throw new PolicyError(
            'idempotency_conflict',
            'Idempotency key was already used for a different request',
            409,
          );
        }
        await client.query('COMMIT');
        return idempotent;
      }
      if (liability.dischargedAt) {
        throw new PolicyError(
          'refund_liability_discharged',
          'Discharged refund liability cannot be executed',
          409,
        );
      }
      const resource = await this.findOne(client, 'resource_key', input.resourceKey);
      if (resource) {
        if (resource.status !== 'rejected') {
          throw new PolicyError(
            'resource_conflict',
            'Resource is already bound to a different idempotency key',
            409,
          );
        }
        await this.archiveRejectedResource(client, resource);
      }
      const clock = await client.query<{ now: Date }>('SELECT clock_timestamp() AS now');
      const now = new Date(clock.rows[0].now);
      const inserted = await client.query(
        `INSERT INTO mizuki_signer_operations (
           id, idempotency_key, resource_key, request_hash, kind, status,
           amount_usd_cents, spend_bucket, asset, recipient, details, created_at, updated_at
         ) VALUES ($1, $2, $3, $4, $5, 'reserved', $6, $7, $8, $9, $10, $11, $11)
         RETURNING *`,
        [
          input.id,
          input.idempotencyKey,
          input.resourceKey,
          input.requestHash,
          input.kind,
          input.amountUsdCents,
          input.spendBucket,
          input.asset,
          input.recipient,
          input.details,
          now,
        ],
      );
      await client.query('COMMIT');
      return mapRow(inserted.rows[0]);
    } catch (error) {
      await client.query('ROLLBACK').catch(() => undefined);
      throw error;
    } finally {
      client.release();
    }
  }

  async reserve(
    input: ReserveOperation,
    dailyLimitUsdCents: number,
    _now: Date,
  ): Promise<OperationRecord> {
    const client = await this.pool.connect();
    try {
      await client.query('BEGIN ISOLATION LEVEL SERIALIZABLE');
      await client.query("SELECT pg_advisory_xact_lock(hashtext('mizuki-signer-spend-policy'))");
      const clock = await client.query<{ now: Date }>('SELECT clock_timestamp() AS now');
      const now = new Date(clock.rows[0].now);

      const idempotent = await this.findOne(client, 'idempotency_key', input.idempotencyKey);
      if (idempotent) {
        if (idempotent.requestHash !== input.requestHash) {
          throw new PolicyError(
            'idempotency_conflict',
            'Idempotency key was already used for a different request',
            409,
          );
        }
        await client.query('COMMIT');
        return idempotent;
      }

      const resource = await this.findOne(client, 'resource_key', input.resourceKey);
      if (resource) {
        if (resource.status !== 'rejected') {
          throw new PolicyError(
            'resource_conflict',
            'Resource is already bound to a different idempotency key',
            409,
          );
        }
        await this.archiveRejectedResource(client, resource);
      }

      const cutoff = new Date(now.getTime() - 24 * 60 * 60 * 1000);
      const spend = await client.query<{ total: string }>(
        `SELECT COALESCE(SUM(amount_usd_cents), 0)::text AS total
           FROM mizuki_signer_operations
          WHERE created_at >= $1 AND status <> 'rejected' AND spend_bucket = $2`,
        [cutoff, input.spendBucket],
      );
      if (
        input.spendBucket !== 'none' &&
        Number(spend.rows[0]?.total ?? 0) + input.amountUsdCents > dailyLimitUsdCents
      ) {
        throw new PolicyError(
          'daily_limit_exceeded',
          'Rolling 24-hour spending limit exceeded',
          429,
          true,
        );
      }

      const inserted = await client.query(
        `INSERT INTO mizuki_signer_operations (
           id, idempotency_key, resource_key, request_hash, kind, status,
           amount_usd_cents, spend_bucket, asset, recipient, details, created_at, updated_at
         ) VALUES ($1, $2, $3, $4, $5, 'reserved', $6, $7, $8, $9, $10, $11, $11)
         RETURNING *`,
        [
          input.id,
          input.idempotencyKey,
          input.resourceKey,
          input.requestHash,
          input.kind,
          input.amountUsdCents,
          input.spendBucket,
          input.asset,
          input.recipient,
          input.details,
          now,
        ],
      );
      await client.query('COMMIT');
      return mapRow(inserted.rows[0]);
    } catch (error) {
      await client.query('ROLLBACK').catch(() => undefined);
      throw error;
    } finally {
      client.release();
    }
  }

  async issueGitHubIdentityGrant(grant: GitHubIdentityGrant): Promise<GitHubIdentityGrant> {
    const result = await this.pool.query(
      `INSERT INTO mizuki_signer_github_identity_grants (
         id, github_id, login, issued_at, expires_at, consumed_at, challenge_id
       ) VALUES ($1, $2, $3, $4, $5, NULL, NULL)
       RETURNING *`,
      [grant.id, grant.githubId, grant.login, grant.issuedAt, grant.expiresAt],
    );
    return mapGrant(result.rows[0]);
  }

  async getGitHubIdentityGrant(id: string): Promise<GitHubIdentityGrant | null> {
    const result = await this.pool.query(
      'SELECT * FROM mizuki_signer_github_identity_grants WHERE id = $1',
      [id],
    );
    return result.rows[0] ? mapGrant(result.rows[0]) : null;
  }

  async issueBindChallenge(
    challenge: BindChallenge,
    grantId: string,
    _now: Date,
  ): Promise<BindChallenge> {
    const client = await this.pool.connect();
    try {
      await client.query('BEGIN ISOLATION LEVEL SERIALIZABLE');
      const clock = await client.query<{ now: Date }>('SELECT clock_timestamp() AS now');
      const now = new Date(clock.rows[0].now);
      const grantResult = await client.query(
        'SELECT * FROM mizuki_signer_github_identity_grants WHERE id = $1 FOR UPDATE',
        [grantId],
      );
      const grant = grantResult.rows[0] ? mapGrant(grantResult.rows[0]) : null;
      if (
        !grant ||
        grant.githubId !== challenge.claimantGitHubId ||
        grant.login !== challenge.claimantGitHubLogin
      ) {
        throw new PolicyError('github_grant_invalid', 'GitHub identity grant is invalid', 422);
      }
      if (grant.consumedAt) {
        throw new PolicyError(
          'github_grant_consumed',
          'GitHub identity grant was already consumed',
          409,
        );
      }
      if (now.getTime() >= grant.expiresAt.getTime()) {
        throw new PolicyError('github_grant_expired', 'GitHub identity grant has expired', 422);
      }
      const result = await client.query(
        `INSERT INTO mizuki_signer_bind_challenges (
           id, escrow_operation_id, binding_hash, claimant_wallet, claimant_github_id,
           claimant_github_login, message, claim_expires_at, issued_at, expires_at,
           consumed_at, bind_operation_id
         ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, NULL, NULL)
         RETURNING *`,
        [
          challenge.id,
          challenge.escrowOperationId,
          challenge.bindingHash,
          challenge.claimantWallet,
          challenge.claimantGitHubId,
          challenge.claimantGitHubLogin,
          challenge.message,
          challenge.claimExpiresAt,
          challenge.issuedAt,
          challenge.expiresAt,
        ],
      );
      await client.query(
        `UPDATE mizuki_signer_github_identity_grants
            SET consumed_at = $2, challenge_id = $3
          WHERE id = $1`,
        [grantId, now, challenge.id],
      );
      await client.query('COMMIT');
      return mapChallenge(result.rows[0]);
    } catch (error) {
      await client.query('ROLLBACK').catch(() => undefined);
      throw error;
    } finally {
      client.release();
    }
  }

  async getBindChallenge(id: string): Promise<BindChallenge | null> {
    const result = await this.pool.query(
      'SELECT * FROM mizuki_signer_bind_challenges WHERE id = $1',
      [id],
    );
    return result.rows[0] ? mapChallenge(result.rows[0]) : null;
  }

  async reserveWithBindChallenge(
    input: ReserveOperation,
    challengeId: string,
    bindingHash: string,
    _now: Date,
  ): Promise<OperationRecord> {
    const client = await this.pool.connect();
    try {
      await client.query('BEGIN ISOLATION LEVEL SERIALIZABLE');
      const idempotent = await this.findOne(client, 'idempotency_key', input.idempotencyKey);
      if (idempotent) {
        if (idempotent.requestHash !== input.requestHash) {
          throw new PolicyError(
            'idempotency_conflict',
            'Idempotency key was already used for a different request',
            409,
          );
        }
        await client.query('COMMIT');
        return idempotent;
      }

      const clock = await client.query<{ now: Date }>('SELECT clock_timestamp() AS now');
      const now = new Date(clock.rows[0].now);
      const challengeResult = await client.query(
        'SELECT * FROM mizuki_signer_bind_challenges WHERE id = $1 FOR UPDATE',
        [challengeId],
      );
      const challenge = challengeResult.rows[0] ? mapChallenge(challengeResult.rows[0]) : null;
      if (!challenge || challenge.bindingHash !== bindingHash) {
        throw new PolicyError('challenge_invalid', 'Binding challenge is invalid', 422);
      }
      if (challenge.consumedAt) {
        throw new PolicyError('challenge_consumed', 'Binding challenge was already consumed', 409);
      }
      if (now.getTime() >= challenge.expiresAt.getTime()) {
        throw new PolicyError('challenge_expired', 'Binding challenge has expired', 422);
      }
      const resource = await this.findOne(client, 'resource_key', input.resourceKey);
      if (resource) {
        if (resource.status !== 'rejected') {
          throw new PolicyError(
            'resource_conflict',
            'Resource is already bound to a different idempotency key',
            409,
          );
        }
        await this.archiveRejectedResource(client, resource);
      }

      const inserted = await client.query(
        `INSERT INTO mizuki_signer_operations (
           id, idempotency_key, resource_key, request_hash, kind, status,
           amount_usd_cents, spend_bucket, asset, recipient, details, created_at, updated_at
         ) VALUES ($1, $2, $3, $4, $5, 'reserved', $6, $7, $8, $9, $10, $11, $11)
         RETURNING *`,
        [
          input.id,
          input.idempotencyKey,
          input.resourceKey,
          input.requestHash,
          input.kind,
          input.amountUsdCents,
          input.spendBucket,
          input.asset,
          input.recipient,
          input.details,
          now,
        ],
      );
      await client.query(
        `UPDATE mizuki_signer_bind_challenges
            SET consumed_at = $2, bind_operation_id = $3
          WHERE id = $1`,
        [challengeId, now, input.id],
      );
      await client.query('COMMIT');
      return mapRow(inserted.rows[0]);
    } catch (error) {
      await client.query('ROLLBACK').catch(() => undefined);
      throw error;
    } finally {
      client.release();
    }
  }

  async get(id: string): Promise<OperationRecord | null> {
    const result = await this.pool.query('SELECT * FROM mizuki_signer_operations WHERE id = $1', [
      id,
    ]);
    return result.rows[0] ? mapRow(result.rows[0]) : null;
  }

  async getByIdempotencyKey(key: string): Promise<OperationRecord | null> {
    const result = await this.pool.query(
      'SELECT * FROM mizuki_signer_operations WHERE idempotency_key = $1',
      [key],
    );
    return result.rows[0] ? mapRow(result.rows[0]) : null;
  }

  async getByResourceKey(key: string): Promise<OperationRecord | null> {
    const result = await this.pool.query(
      'SELECT * FROM mizuki_signer_operations WHERE resource_key = $1',
      [key],
    );
    return result.rows[0] ? mapRow(result.rows[0]) : null;
  }

  async acquireLease(
    id: string,
    owner: string,
    _now: Date,
    leaseMs: number,
  ): Promise<OperationRecord | null> {
    const result = await this.pool.query(
      `UPDATE mizuki_signer_operations
          SET lease_owner = $2,
              lease_expires_at = clock_timestamp() + ($3::bigint * interval '1 millisecond'),
              updated_at = clock_timestamp(),
              version = version + 1
        WHERE id = $1
          AND (
            lease_owner IS NULL OR
            lease_owner = $2 OR
            lease_expires_at <= clock_timestamp()
          )
      RETURNING *`,
      [id, owner, leaseMs],
    );
    return result.rows[0] ? mapRow(result.rows[0]) : null;
  }

  async update(
    id: string,
    owner: string,
    expectedVersion: number,
    patch: OperationPatch,
  ): Promise<OperationRecord> {
    const assignments: string[] = [];
    const values: unknown[] = [id, owner, expectedVersion];
    const set = (column: string, value: unknown): void => {
      values.push(value);
      assignments.push(`${column} = $${values.length}`);
    };
    if (patch.status !== undefined) set('status', patch.status);
    if (patch.prepared !== undefined) set('prepared', patch.prepared);
    if (patch.transactionSignature !== undefined) {
      set('transaction_signature', patch.transactionSignature);
    }
    if (patch.errorCode !== undefined) set('error_code', patch.errorCode);
    if (patch.errorMessage !== undefined) set('error_message', patch.errorMessage);
    if (patch.details !== undefined) set('details', patch.details);
    assignments.push('updated_at = now()', 'version = version + 1');

    const result = await this.pool.query(
      `UPDATE mizuki_signer_operations SET ${assignments.join(', ')}
        WHERE id = $1 AND lease_owner = $2 AND version = $3
      RETURNING *`,
      values,
    );
    if (!result.rows[0]) {
      const exists = await this.get(id);
      if (!exists) throw new PolicyError('operation_not_found', 'Operation was not found', 404);
      throw new PolicyError('version_conflict', 'Operation changed concurrently', 409, true);
    }
    return mapRow(result.rows[0]);
  }

  async releaseLease(id: string, owner: string): Promise<void> {
    await this.pool.query(
      `UPDATE mizuki_signer_operations
          SET lease_owner = NULL, lease_expires_at = NULL, updated_at = now(), version = version + 1
        WHERE id = $1 AND lease_owner = $2`,
      [id, owner],
    );
  }

  async listRecoverable(limit: number): Promise<OperationRecord[]> {
    const result = await this.pool.query(
      `SELECT * FROM mizuki_signer_operations
        WHERE status NOT IN ('finalized', 'rejected')
        ORDER BY created_at ASC
        LIMIT $1`,
      [limit],
    );
    return result.rows.map(mapRow);
  }

  async stats(): Promise<StoreStats> {
    const result = await this.pool.query<{ status: OperationRecord['status']; count: string }>(
      'SELECT status, COUNT(*)::text AS count FROM mizuki_signer_operations GROUP BY status',
    );
    const byStatus: StoreStats['byStatus'] = {};
    let total = 0;
    for (const row of result.rows) {
      const count = Number(row.count);
      byStatus[row.status] = count;
      total += count;
    }
    return { total, byStatus };
  }

  async pendingRefundRawAmount(): Promise<string> {
    const result = await this.pool.query<{ total: string }>(
      `SELECT COALESCE(SUM(liability.raw_amount), 0)::text AS total
         FROM mizuki_signer_refund_liabilities liability
    LEFT JOIN mizuki_signer_operations operation
           ON operation.resource_key = 'refund:' || liability.settlement_signature
          AND operation.status = 'finalized'
        WHERE operation.id IS NULL AND liability.discharged_at IS NULL`,
    );
    return result.rows[0]?.total ?? '0';
  }

  async rollingSpendUsdCents(bucket: 'refund' | 'escrow', _now: Date): Promise<number> {
    if (bucket === 'refund') {
      const result = await this.pool.query<{ total: string }>(
        `SELECT COALESCE(SUM(amount_usd_cents), 0)::text AS total
           FROM mizuki_signer_refund_liabilities
          WHERE created_at >= clock_timestamp() - interval '24 hours'`,
      );
      return Number(result.rows[0]?.total ?? 0);
    }
    const result = await this.pool.query<{ total: string }>(
      `SELECT COALESCE(SUM(amount_usd_cents), 0)::text AS total
         FROM mizuki_signer_operations
        WHERE spend_bucket = $1
          AND status <> 'rejected'
          AND created_at >= clock_timestamp() - interval '24 hours'`,
      [bucket],
    );
    return Number(result.rows[0]?.total ?? 0);
  }

  async ping(): Promise<void> {
    await this.pool.query('SELECT 1');
  }

  async close(): Promise<void> {
    await this.pool.end();
  }

  private async findOne(
    client: PoolClient,
    column: 'idempotency_key' | 'resource_key',
    value: string,
  ): Promise<OperationRecord | null> {
    const result = await client.query(
      `SELECT * FROM mizuki_signer_operations WHERE ${column} = $1`,
      [value],
    );
    return result.rows[0] ? mapRow(result.rows[0]) : null;
  }

  private async archiveRejectedResource(
    client: PoolClient,
    record: OperationRecord,
  ): Promise<void> {
    await client.query(
      `UPDATE mizuki_signer_operations
          SET resource_key = $2, updated_at = clock_timestamp(), version = version + 1
        WHERE id = $1 AND status = 'rejected'`,
      [record.id, `rejected:${record.id}:${record.resourceKey}`],
    );
  }
}

function mapRow(row: QueryResultRow): OperationRecord {
  return {
    id: row.id,
    idempotencyKey: row.idempotency_key,
    resourceKey: row.resource_key,
    requestHash: row.request_hash,
    kind: row.kind,
    status: row.status,
    amountUsdCents: row.amount_usd_cents,
    spendBucket: row.spend_bucket,
    asset: row.asset,
    recipient: row.recipient,
    details: row.details,
    prepared: row.prepared,
    transactionSignature: row.transaction_signature,
    errorCode: row.error_code,
    errorMessage: row.error_message,
    leaseOwner: row.lease_owner,
    leaseExpiresAt: row.lease_expires_at ? new Date(row.lease_expires_at) : null,
    createdAt: new Date(row.created_at),
    updatedAt: new Date(row.updated_at),
    version: row.version,
  };
}

function mapChallenge(row: QueryResultRow): BindChallenge {
  return {
    id: row.id,
    escrowOperationId: row.escrow_operation_id,
    bindingHash: row.binding_hash,
    claimantWallet: row.claimant_wallet,
    claimantGitHubId: row.claimant_github_id,
    claimantGitHubLogin: row.claimant_github_login,
    message: row.message,
    claimExpiresAt: new Date(row.claim_expires_at),
    issuedAt: new Date(row.issued_at),
    expiresAt: new Date(row.expires_at),
    consumedAt: row.consumed_at ? new Date(row.consumed_at) : null,
    bindOperationId: row.bind_operation_id,
  };
}

function mapGrant(row: QueryResultRow): GitHubIdentityGrant {
  return {
    id: row.id,
    githubId: row.github_id,
    login: row.login,
    issuedAt: new Date(row.issued_at),
    expiresAt: new Date(row.expires_at),
    consumedAt: row.consumed_at ? new Date(row.consumed_at) : null,
    challengeId: row.challenge_id,
  };
}

function mapLiability(row: QueryResultRow): RefundLiability {
  return {
    id: row.id,
    idempotencyKey: row.idempotency_key,
    requestHash: row.request_hash,
    jobId: row.job_id,
    settlementSignature: row.settlement_signature,
    payer: row.payer,
    treasury: row.treasury,
    mint: row.mint,
    rawAmount: String(row.raw_amount),
    decimals: row.decimals,
    amountUsdCents: row.amount_usd_cents,
    settlementSlot: Number(row.settlement_slot),
    settlementBlockTimeUnixSeconds: Number(row.settlement_block_time),
    createdAt: new Date(row.created_at),
    dischargedAt: row.discharged_at ? new Date(row.discharged_at) : null,
    dischargeEvidenceHash: row.discharge_evidence_hash,
    dischargeEvidence: row.discharge_evidence,
    dischargeIdempotencyKey: row.discharge_idempotency_key,
    dischargeRequestHash: row.discharge_request_hash,
  };
}
