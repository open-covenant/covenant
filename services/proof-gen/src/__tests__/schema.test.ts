import { describe, it, expect } from 'vitest';
import { ProveRequestSchema, JobIdParamsSchema } from '../schema.js';

// 64-char hex string — the hex branch of the Field regex.
const HEX64 = 'ab'.repeat(32);
const arr = (n: number) => Array.from({ length: n }, () => HEX64);

const publicInputs = () => ({
  task_hash: HEX64,
  result_hash: HEX64,
  deadline: '1000',
  submitted_at: '500',
  criteria_root: HEX64,
});

const privateInputs = () => ({
  task_preimage: arr(16),
  result_preimage: arr(32),
  salt: HEX64,
  criteria_satisfied: arr(8),
  criteria_path: arr(3),
  criteria_index: arr(3),
});

const request = () => ({
  circuit_id: 'task_completion.v1',
  public_inputs: publicInputs(),
  private_inputs: privateInputs(),
});

describe('ProveRequestSchema', () => {
  it('accepts a fully valid request', () => {
    expect(ProveRequestSchema.safeParse(request()).success).toBe(true);
  });

  describe('Field element regex (decimal | 32-byte hex)', () => {
    it('accepts a decimal field element', () => {
      const r = request();
      r.public_inputs.task_hash = '12345678901234567890';
      expect(ProveRequestSchema.safeParse(r).success).toBe(true);
    });

    it('accepts a 64-char hex field element', () => {
      const r = request();
      r.public_inputs.task_hash = HEX64;
      expect(ProveRequestSchema.safeParse(r).success).toBe(true);
    });

    it('rejects a non-field string (neither decimal nor hex)', () => {
      const r = request();
      r.public_inputs.task_hash = 'xyz';
      expect(ProveRequestSchema.safeParse(r).success).toBe(false);
    });

    it('rejects a 63-char hex (the hex branch is exactly 64)', () => {
      const r = request();
      r.public_inputs.task_hash = 'a'.repeat(63);
      expect(ProveRequestSchema.safeParse(r).success).toBe(false);
    });
  });

  describe('array lengths bound to circuit constants', () => {
    it('rejects a task_preimage of the wrong length (15, not 16)', () => {
      const r = request();
      r.private_inputs.task_preimage = arr(15);
      expect(ProveRequestSchema.safeParse(r).success).toBe(false);
    });

    it('rejects a result_preimage of the wrong length (31, not 32)', () => {
      const r = request();
      r.private_inputs.result_preimage = arr(31);
      expect(ProveRequestSchema.safeParse(r).success).toBe(false);
    });

    it('rejects a criteria_satisfied of the wrong length (7, not 8)', () => {
      const r = request();
      r.private_inputs.criteria_satisfied = arr(7);
      expect(ProveRequestSchema.safeParse(r).success).toBe(false);
    });

    it('rejects a criteria_path of the wrong length (2, not 3)', () => {
      const r = request();
      r.private_inputs.criteria_path = arr(2);
      expect(ProveRequestSchema.safeParse(r).success).toBe(false);
    });
  });

  it('rejects a circuit_id that is not the task_completion.v1 literal', () => {
    const r = request();
    r.circuit_id = 'task_completion.v2';
    expect(ProveRequestSchema.safeParse(r).success).toBe(false);
  });

  it('rejects a non-numeric deadline', () => {
    const r = request();
    r.public_inputs.deadline = 'soon';
    expect(ProveRequestSchema.safeParse(r).success).toBe(false);
  });
});

describe('JobIdParamsSchema', () => {
  it('accepts a valid uuid', () => {
    expect(JobIdParamsSchema.safeParse({ id: '550e8400-e29b-41d4-a716-446655440000' }).success).toBe(true);
  });

  it('rejects a non-uuid id', () => {
    expect(JobIdParamsSchema.safeParse({ id: 'not-a-uuid' }).success).toBe(false);
  });
});
