import { createHash } from 'node:crypto';
import { Pool, type PoolClient, type QueryResultRow } from 'pg';
import type {
  BindChallenge,
  GitHubIdentityGrant,
  OperationRecord,
  RefundLiability,
  RepositoryAdmission,
  ReserveOperation,
} from './domain.js';
import { PolicyError } from './domain.js';
import type {
  OperationPatch,
  OperationStore,
  RefundLiabilityDeliveryBinding,
  StoreStats,
} from './store.js';

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
        repository text NOT NULL,
        issue_number integer NOT NULL CHECK (issue_number > 0),
        base_ref text NOT NULL,
        base_sha text NOT NULL CHECK (base_sha ~ '^[a-f0-9]{40,64}$'),
        repository_authorized_at timestamptz NOT NULL,
        authorization_evidence_hash text NOT NULL CHECK (authorization_evidence_hash ~ '^[a-f0-9]{64}$'),
        reviewed_head_sha text CHECK (reviewed_head_sha ~ '^[a-f0-9]{40,64}$'),
        reviewed_base_sha text CHECK (reviewed_base_sha ~ '^[a-f0-9]{40,64}$'),
        reviewed_base_ref text,
        reviewed_diff_hash text CHECK (reviewed_diff_hash ~ '^[a-f0-9]{64}$'),
        delivery_bound_at timestamptz,
        delivery_binding_idempotency_key text,
        delivery_binding_request_hash text,
        delivery_binding_hash text,
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
        ADD COLUMN IF NOT EXISTS repository text,
        ADD COLUMN IF NOT EXISTS issue_number integer,
        ADD COLUMN IF NOT EXISTS base_ref text,
        ADD COLUMN IF NOT EXISTS base_sha text,
        ADD COLUMN IF NOT EXISTS repository_authorized_at timestamptz,
        ADD COLUMN IF NOT EXISTS authorization_evidence_hash text,
        ADD COLUMN IF NOT EXISTS reviewed_head_sha text,
        ADD COLUMN IF NOT EXISTS reviewed_base_sha text,
        ADD COLUMN IF NOT EXISTS reviewed_base_ref text,
        ADD COLUMN IF NOT EXISTS reviewed_diff_hash text,
        ADD COLUMN IF NOT EXISTS delivery_bound_at timestamptz,
        ADD COLUMN IF NOT EXISTS delivery_binding_idempotency_key text,
        ADD COLUMN IF NOT EXISTS delivery_binding_request_hash text,
        ADD COLUMN IF NOT EXISTS delivery_binding_hash text,
        ADD COLUMN IF NOT EXISTS discharged_at timestamptz,
        ADD COLUMN IF NOT EXISTS discharge_evidence_hash text,
        ADD COLUMN IF NOT EXISTS discharge_evidence jsonb,
        ADD COLUMN IF NOT EXISTS discharge_idempotency_key text,
        ADD COLUMN IF NOT EXISTS discharge_request_hash text;
      DO $$
      BEGIN
        IF EXISTS (
          SELECT 1
            FROM mizuki_signer_refund_liabilities
           WHERE repository IS NULL
              OR issue_number IS NULL
              OR base_ref IS NULL
              OR base_sha IS NULL
              OR repository_authorized_at IS NULL
              OR authorization_evidence_hash IS NULL
        ) THEN
          RAISE EXCEPTION 'existing refund liabilities lack immutable delivery policy';
        END IF;
      END $$;
      ALTER TABLE mizuki_signer_refund_liabilities
        ALTER COLUMN repository SET NOT NULL,
        ALTER COLUMN issue_number SET NOT NULL,
        ALTER COLUMN base_ref SET NOT NULL,
        ALTER COLUMN base_sha SET NOT NULL,
        ALTER COLUMN repository_authorized_at SET NOT NULL,
        ALTER COLUMN authorization_evidence_hash SET NOT NULL;
      CREATE UNIQUE INDEX IF NOT EXISTS mizuki_signer_refund_discharge_idempotency
        ON mizuki_signer_refund_liabilities (discharge_idempotency_key)
        WHERE discharge_idempotency_key IS NOT NULL;
      CREATE UNIQUE INDEX IF NOT EXISTS mizuki_signer_refund_delivery_idempotency
        ON mizuki_signer_refund_liabilities (delivery_binding_idempotency_key)
        WHERE delivery_binding_idempotency_key IS NOT NULL;
      ALTER TABLE mizuki_signer_refund_liabilities
        DROP CONSTRAINT IF EXISTS mizuki_signer_refund_delivery_binding_complete;
      ALTER TABLE mizuki_signer_refund_liabilities
        ADD CONSTRAINT mizuki_signer_refund_delivery_binding_complete CHECK (
          (delivery_bound_at IS NULL
            AND reviewed_head_sha IS NULL
            AND reviewed_base_sha IS NULL
            AND reviewed_base_ref IS NULL
            AND reviewed_diff_hash IS NULL
            AND delivery_binding_idempotency_key IS NULL
            AND delivery_binding_request_hash IS NULL
            AND delivery_binding_hash IS NULL)
          OR
          (delivery_bound_at IS NOT NULL
            AND reviewed_head_sha ~ '^[a-f0-9]{40,64}$'
            AND reviewed_base_sha ~ '^[a-f0-9]{40,64}$'
            AND reviewed_base_ref IS NOT NULL
            AND reviewed_diff_hash ~ '^[a-f0-9]{64}$'
            AND delivery_binding_idempotency_key IS NOT NULL
            AND delivery_binding_request_hash ~ '^[a-f0-9]{64}$'
            AND delivery_binding_hash ~ '^[a-f0-9]{64}$')
        );
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

const REPOSITORY_ADMISSION_SCHEMA = `
      CREATE TABLE IF NOT EXISTS mizuki_signer_repository_admissions (
        id uuid PRIMARY KEY,
        idempotency_key text NOT NULL UNIQUE,
        request_hash text NOT NULL CHECK (request_hash ~ '^[a-f0-9]{64}$'),
        quote_id uuid NOT NULL UNIQUE,
        repository text NOT NULL,
        issue_number integer NOT NULL CHECK (issue_number > 0),
        base_ref text NOT NULL,
        base_sha text NOT NULL CHECK (base_sha ~ '^[a-f0-9]{40,64}$'),
        reservation_key_hash text NOT NULL UNIQUE CHECK (reservation_key_hash ~ '^[a-f0-9]{64}$'),
        payment_authorization_hash text NOT NULL UNIQUE CHECK (payment_authorization_hash ~ '^[a-f0-9]{64}$'),
        verifier_app_id text NOT NULL CHECK (verifier_app_id ~ '^[1-9][0-9]{0,15}$'),
        installation_id bigint NOT NULL CHECK (installation_id > 0),
        repository_selection text NOT NULL CHECK (repository_selection = 'selected'),
        permissions jsonb NOT NULL,
        token_repositories integer NOT NULL CHECK (token_repositories = 1),
        token_expires_at timestamptz NOT NULL,
        admitted_at timestamptz NOT NULL,
        evidence_hash text NOT NULL UNIQUE CHECK (evidence_hash ~ '^[a-f0-9]{64}$'),
        CHECK (token_expires_at > admitted_at)
      );`;

const DELAYED_LIABILITY_SAFETY_SCHEMA = `
      ALTER TABLE mizuki_signer_repository_admissions
        ADD COLUMN IF NOT EXISTS settlement_message_hash char(64),
        ADD COLUMN IF NOT EXISTS settlement_client_signature varchar(88),
        ADD COLUMN IF NOT EXISTS settlement_fee_payer varchar(44),
        ADD COLUMN IF NOT EXISTS settlement_raw_amount numeric(20, 0),
        ADD COLUMN IF NOT EXISTS payment_window_start bigint,
        ADD COLUMN IF NOT EXISTS payment_window_end bigint;
      DO $$
      BEGIN
        IF EXISTS (
          SELECT 1
            FROM mizuki_signer_repository_admissions
           WHERE settlement_message_hash IS NULL
              OR settlement_client_signature IS NULL
              OR settlement_fee_payer IS NULL
              OR settlement_raw_amount IS NULL
              OR payment_window_start IS NULL
              OR payment_window_end IS NULL
        ) THEN
          RAISE EXCEPTION 'existing repository admissions lack non-replayable settlement bindings';
        END IF;
      END $$;
      ALTER TABLE mizuki_signer_repository_admissions
        ALTER COLUMN settlement_message_hash SET NOT NULL,
        ALTER COLUMN settlement_client_signature SET NOT NULL,
        ALTER COLUMN settlement_fee_payer SET NOT NULL,
        ALTER COLUMN settlement_raw_amount SET NOT NULL,
        ALTER COLUMN payment_window_start SET NOT NULL,
        ALTER COLUMN payment_window_end SET NOT NULL;
      ALTER TABLE mizuki_signer_repository_admissions
        DROP CONSTRAINT IF EXISTS mizuki_signer_admission_settlement_binding_check;
      ALTER TABLE mizuki_signer_repository_admissions
        ADD CONSTRAINT mizuki_signer_admission_settlement_binding_check CHECK (
          settlement_message_hash ~ '^[a-f0-9]{64}$'
          AND settlement_client_signature ~ '^[1-9A-HJ-NP-Za-km-z]{64,88}$'
          AND settlement_fee_payer ~ '^[1-9A-HJ-NP-Za-km-z]{32,44}$'
          AND settlement_raw_amount > 0
          AND payment_window_end > payment_window_start
        );
      ALTER TABLE mizuki_signer_refund_liabilities
        ADD COLUMN IF NOT EXISTS repository_admission_id uuid;
      DO $$
      BEGIN
        IF EXISTS (
          SELECT 1
            FROM mizuki_signer_refund_liabilities
           WHERE repository_admission_id IS NULL
        ) THEN
          RAISE EXCEPTION 'existing refund liabilities lack repository admission IDs';
        END IF;
      END $$;
      ALTER TABLE mizuki_signer_refund_liabilities
        ALTER COLUMN repository_admission_id SET NOT NULL;
      ALTER TABLE mizuki_signer_refund_liabilities
        DROP CONSTRAINT IF EXISTS mizuki_signer_refund_repository_admission_fk;
      ALTER TABLE mizuki_signer_refund_liabilities
        ADD CONSTRAINT mizuki_signer_refund_repository_admission_fk
        FOREIGN KEY (repository_admission_id)
        REFERENCES mizuki_signer_repository_admissions(id);`;

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
      const migrations = [
        { version: 1, name: 'policy-and-custody-core', sql: SIGNER_SCHEMA },
        { version: 2, name: 'repository-admission-receipts', sql: REPOSITORY_ADMISSION_SCHEMA },
        { version: 3, name: 'delayed-liability-safety', sql: DELAYED_LIABILITY_SAFETY_SCHEMA },
      ].map((migration) => ({
        ...migration,
        checksum: createHash('sha256').update(migration.sql).digest('hex'),
      }));
      if (
        applied.rows.some(
          (row) => !migrations.some((migration) => migration.version === Number(row.version)),
        )
      ) {
        throw new Error('policy-signer database contains an unknown schema migration');
      }
      for (const migration of migrations) {
        const current = applied.rows.find((row) => Number(row.version) === migration.version);
        if (
          current &&
          (current.name !== migration.name || current.checksum !== migration.checksum)
        ) {
          throw new Error('policy-signer database migration does not match this build');
        }
        if (current) continue;
        await client.query(migration.sql);
        await client.query(
          `INSERT INTO mizuki_schema_migrations (component, version, name, checksum)
           VALUES ($1, $2, $3, $4)`,
          ['policy-signer', migration.version, migration.name, migration.checksum],
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

  async registerRepositoryAdmission(admission: RepositoryAdmission): Promise<RepositoryAdmission> {
    const client = await this.pool.connect();
    try {
      await client.query('BEGIN ISOLATION LEVEL SERIALIZABLE');
      await client.query(
        "SELECT pg_advisory_xact_lock(hashtext('mizuki-signer-repository-admissions'))",
      );
      const idempotent = await client.query(
        'SELECT * FROM mizuki_signer_repository_admissions WHERE idempotency_key = $1',
        [admission.idempotencyKey],
      );
      if (idempotent.rows[0]) {
        const existing = mapRepositoryAdmission(idempotent.rows[0]);
        if (existing.requestHash !== admission.requestHash) {
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
        `SELECT id FROM mizuki_signer_repository_admissions
          WHERE quote_id = $1 OR reservation_key_hash = $2 OR payment_authorization_hash = $3`,
        [admission.quoteId, admission.reservationKeyHash, admission.paymentAuthorizationHash],
      );
      if (conflict.rows[0]) {
        throw new PolicyError(
          'repository_admission_conflict',
          'Quote, reservation, or payment proof already has a different admission',
          409,
        );
      }
      const result = await client.query(
        `INSERT INTO mizuki_signer_repository_admissions (
           id, idempotency_key, request_hash, quote_id, repository, issue_number,
           base_ref, base_sha, reservation_key_hash, payment_authorization_hash,
           settlement_message_hash, settlement_client_signature, settlement_fee_payer,
           settlement_raw_amount, payment_window_start, payment_window_end, verifier_app_id,
           installation_id, repository_selection, permissions, token_repositories,
           token_expires_at, admitted_at, evidence_hash
         ) VALUES (
           $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13,
           $14, $15, $16, $17, $18, $19, $20::jsonb, $21, $22, $23, $24
         ) RETURNING *`,
        [
          admission.id,
          admission.idempotencyKey,
          admission.requestHash,
          admission.quoteId,
          admission.repository,
          admission.issueNumber,
          admission.baseRef,
          admission.baseSha,
          admission.reservationKeyHash,
          admission.paymentAuthorizationHash,
          admission.settlementMessageHash,
          admission.settlementClientSignature,
          admission.settlementFeePayer,
          admission.settlementRawAmount,
          admission.paymentWindowStartUnixSeconds,
          admission.paymentWindowEndUnixSeconds,
          admission.verifierAppId,
          admission.installationId,
          admission.repositorySelection,
          JSON.stringify(admission.permissions),
          admission.tokenRepositories,
          admission.tokenExpiresAt,
          admission.admittedAt,
          admission.evidenceHash,
        ],
      );
      await client.query('COMMIT');
      return mapRepositoryAdmission(result.rows[0]);
    } catch (error) {
      await client.query('ROLLBACK').catch(() => undefined);
      throw error;
    } finally {
      client.release();
    }
  }

  async getRepositoryAdmission(id: string): Promise<RepositoryAdmission | null> {
    const result = await this.pool.query(
      'SELECT * FROM mizuki_signer_repository_admissions WHERE id = $1',
      [id],
    );
    return result.rows[0] ? mapRepositoryAdmission(result.rows[0]) : null;
  }

  async getRepositoryAdmissionByIdempotencyKey(key: string): Promise<RepositoryAdmission | null> {
    const result = await this.pool.query(
      'SELECT * FROM mizuki_signer_repository_admissions WHERE idempotency_key = $1',
      [key],
    );
    return result.rows[0] ? mapRepositoryAdmission(result.rows[0]) : null;
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
           id, idempotency_key, request_hash, job_id, repository_admission_id,
           settlement_signature, payer, repository, issue_number, base_ref, base_sha,
           repository_authorized_at, authorization_evidence_hash, treasury, mint,
           raw_amount, decimals, settlement_slot, amount_usd_cents,
           settlement_block_time, created_at
         ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $20, clock_timestamp())
         RETURNING *`,
        [
          liability.id,
          liability.idempotencyKey,
          liability.requestHash,
          liability.jobId,
          liability.repositoryAdmissionId,
          liability.settlementSignature,
          liability.payer,
          liability.repository,
          liability.issueNumber,
          liability.baseRef,
          liability.baseSha,
          liability.repositoryAuthorizedAt,
          liability.authorizationEvidenceHash,
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

  async bindRefundLiabilityDelivery(
    liabilityId: string,
    idempotencyKey: string,
    requestHash: string,
    bindingHash: string,
    binding: RefundLiabilityDeliveryBinding,
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
          WHERE delivery_binding_idempotency_key = $1 AND id <> $2`,
        [idempotencyKey, liabilityId],
      );
      if (idempotent.rows[0]) {
        throw new PolicyError(
          'idempotency_conflict',
          'Idempotency key was already used for a different request',
          409,
        );
      }
      if (liability.deliveryBoundAt) {
        if (
          liability.deliveryBindingIdempotencyKey === idempotencyKey &&
          liability.deliveryBindingRequestHash === requestHash
        ) {
          await client.query('COMMIT');
          return liability;
        }
        throw new PolicyError(
          'refund_liability_delivery_bound',
          'Refund liability already has an immutable delivery binding',
          409,
        );
      }
      if (liability.dischargedAt) {
        throw new PolicyError(
          'refund_liability_discharged',
          'Discharged refund liability cannot be rebound',
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
          'Refund liability cannot be bound after refund execution starts',
          409,
        );
      }
      const updated = await client.query(
        `UPDATE mizuki_signer_refund_liabilities
            SET reviewed_head_sha = $2,
                reviewed_base_sha = $3,
                reviewed_base_ref = $4,
                reviewed_diff_hash = $5,
                delivery_bound_at = date_trunc('second', clock_timestamp()),
                delivery_binding_idempotency_key = $6,
                delivery_binding_request_hash = $7,
                delivery_binding_hash = $8
          WHERE id = $1
        RETURNING *`,
        [
          liabilityId,
          binding.reviewedHeadSha,
          binding.reviewedBaseSha,
          binding.reviewedBaseRef,
          binding.reviewedDiffHash,
          idempotencyKey,
          requestHash,
          bindingHash,
        ],
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
    repositoryAdmissionId: row.repository_admission_id,
    settlementSignature: row.settlement_signature,
    repository: row.repository,
    issueNumber: row.issue_number,
    baseRef: row.base_ref,
    baseSha: row.base_sha,
    repositoryAuthorizedAt: new Date(row.repository_authorized_at),
    authorizationEvidenceHash: row.authorization_evidence_hash,
    reviewedHeadSha: row.reviewed_head_sha,
    reviewedBaseSha: row.reviewed_base_sha,
    reviewedBaseRef: row.reviewed_base_ref,
    reviewedDiffHash: row.reviewed_diff_hash,
    deliveryBoundAt: row.delivery_bound_at ? new Date(row.delivery_bound_at) : null,
    deliveryBindingIdempotencyKey: row.delivery_binding_idempotency_key,
    deliveryBindingRequestHash: row.delivery_binding_request_hash,
    deliveryBindingHash: row.delivery_binding_hash,
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

function mapRepositoryAdmission(row: QueryResultRow): RepositoryAdmission {
  return {
    id: row.id,
    idempotencyKey: row.idempotency_key,
    requestHash: row.request_hash,
    quoteId: row.quote_id,
    repository: row.repository,
    issueNumber: row.issue_number,
    baseRef: row.base_ref,
    baseSha: row.base_sha,
    reservationKeyHash: row.reservation_key_hash,
    paymentAuthorizationHash: row.payment_authorization_hash,
    settlementMessageHash: row.settlement_message_hash,
    settlementClientSignature: row.settlement_client_signature,
    settlementFeePayer: row.settlement_fee_payer,
    settlementRawAmount: String(row.settlement_raw_amount),
    paymentWindowStartUnixSeconds: Number(row.payment_window_start),
    paymentWindowEndUnixSeconds: Number(row.payment_window_end),
    verifierAppId: row.verifier_app_id,
    installationId: Number(row.installation_id),
    repositorySelection: row.repository_selection,
    permissions: row.permissions,
    tokenRepositories: row.token_repositories,
    tokenExpiresAt: new Date(row.token_expires_at),
    admittedAt: new Date(row.admitted_at),
    evidenceHash: row.evidence_hash,
  };
}
