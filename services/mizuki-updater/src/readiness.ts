interface StableReadinessOptions {
  successTtlMs?: number;
  retryDelayMs?: number;
  now?: () => number;
  wait?: (delayMs: number) => Promise<void>;
}

export function createStableReadinessProbe(
  check: () => Promise<void>,
  options: StableReadinessOptions = {},
): () => Promise<void> {
  const successTtlMs = options.successTtlMs ?? 30_000;
  const retryDelayMs = options.retryDelayMs ?? 250;
  const now = options.now ?? Date.now;
  const wait =
    options.wait ?? ((delayMs) => new Promise((resolve) => setTimeout(resolve, delayMs)));
  let lastSuccessAt: number | undefined;
  let inFlight: Promise<void> | undefined;

  return async () => {
    if (lastSuccessAt !== undefined) {
      const ageMs = now() - lastSuccessAt;
      if (ageMs >= 0 && ageMs < successTtlMs) return;
    }

    inFlight ??= run().finally(() => {
      inFlight = undefined;
    });
    return inFlight;
  };

  async function run(): Promise<void> {
    try {
      await check();
    } catch {
      await wait(retryDelayMs);
      await check();
    }
    lastSuccessAt = now();
  }
}
