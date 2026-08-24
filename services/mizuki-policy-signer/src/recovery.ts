export const RECOVERY_SHUTDOWN_GRACE_MS = 30_000;
export const PROCESS_SHUTDOWN_DEADLINE_MS = 90_000;

export class RecoveryRunner {
  private task: Promise<void> | null = null;

  constructor(
    private readonly recover: (limit?: number) => Promise<void>,
    private readonly onFailure: () => void,
  ) {}

  run(limit?: number): Promise<void> {
    if (this.task) return this.task;
    this.task = Promise.resolve()
      .then(() => this.recover(limit))
      .catch(() => this.onFailure())
      .finally(() => {
        this.task = null;
      });
    return this.task;
  }

  active(): Promise<void> | null {
    return this.task;
  }
}

export async function waitForRecovery(
  task: Promise<void> | null,
  timeoutMs = RECOVERY_SHUTDOWN_GRACE_MS,
): Promise<boolean> {
  if (!task) return true;
  if (!Number.isInteger(timeoutMs) || timeoutMs <= 0) {
    throw new RangeError('Recovery shutdown grace must be a positive integer');
  }

  let timer: ReturnType<typeof setTimeout> | undefined;
  const expired = new Promise<false>((resolve) => {
    timer = setTimeout(() => resolve(false), timeoutMs);
  });
  try {
    return await Promise.race([
      task.then(
        () => true,
        () => true,
      ),
      expired,
    ]);
  } finally {
    if (timer) clearTimeout(timer);
  }
}

export async function shutdownResources(
  recovery: Promise<void> | null,
  closeHttp: (force: boolean) => Promise<void>,
  closeStore: () => Promise<void>,
  recoveryGraceMs = RECOVERY_SHUTDOWN_GRACE_MS,
): Promise<boolean> {
  const recoverySettled = await waitForRecovery(recovery, recoveryGraceMs);
  await closeHttp(!recoverySettled);
  if (!recoverySettled) return false;
  await closeStore();
  return true;
}

export async function waitForShutdown(
  task: Promise<boolean>,
  timeoutMs = PROCESS_SHUTDOWN_DEADLINE_MS,
): Promise<boolean> {
  if (!Number.isInteger(timeoutMs) || timeoutMs <= 0) {
    throw new RangeError('Process shutdown deadline must be a positive integer');
  }

  let timer: ReturnType<typeof setTimeout> | undefined;
  const expired = new Promise<false>((resolve) => {
    timer = setTimeout(() => resolve(false), timeoutMs);
  });
  try {
    return await Promise.race([task, expired]);
  } finally {
    if (timer) clearTimeout(timer);
  }
}
