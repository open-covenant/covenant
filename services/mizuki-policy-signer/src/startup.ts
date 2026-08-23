export const STARTUP_READINESS_TIMEOUT_MS = 20_000;

export async function startupReadinessPasses(
  probe: () => Promise<{ healthy: boolean }>,
  timeoutMs = STARTUP_READINESS_TIMEOUT_MS,
): Promise<boolean> {
  if (!Number.isInteger(timeoutMs) || timeoutMs <= 0) {
    throw new RangeError('Startup readiness timeout must be a positive integer');
  }

  let timer: ReturnType<typeof setTimeout> | undefined;
  const timedOut = new Promise<false>((resolve) => {
    timer = setTimeout(() => resolve(false), timeoutMs);
  });
  try {
    return await Promise.race([
      Promise.resolve()
        .then(probe)
        .then(
          (readiness) => readiness.healthy,
          () => false,
        ),
      timedOut,
    ]);
  } finally {
    if (timer) clearTimeout(timer);
  }
}
