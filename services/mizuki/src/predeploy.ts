import type { MizukiStore } from './store.js';

type Store = Pick<MizukiStore, 'operatorControls' | 'close'>;

interface Dependencies<TStore extends Store> {
  connect: () => Promise<TStore>;
  assertStaticConfig: () => void;
  checkReadiness: (store: TStore) => Promise<{ ready: boolean }>;
}

export async function runPredeploy<TStore extends Store>(
  deps: Dependencies<TStore>,
): Promise<void> {
  const store = await deps.connect();
  try {
    const controls = await store.operatorControls();
    if (!controls.intakeEnabled && !controls.claimsEnabled) return;

    deps.assertStaticConfig();
    const readiness = await deps.checkReadiness(store);
    if (!readiness.ready)
      throw new Error('Mizuki dependencies are not ready for an open deployment');
  } finally {
    await store.close();
  }
}
