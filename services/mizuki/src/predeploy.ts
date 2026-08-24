import type { MizukiStore } from './store.js';

type Store = Pick<MizukiStore, 'operatorControls' | 'close'>;

interface Dependencies<TStore extends Store> {
  connect: () => Promise<TStore>;
}

export async function runPredeploy<TStore extends Store>(
  deps: Dependencies<TStore>,
): Promise<void> {
  const store = await deps.connect();
  try {
    const controls = await store.operatorControls();
    if (!controls.intakeEnabled && !controls.claimsEnabled) return;
    throw new Error('Mizuki admission must be closed before deployment');
  } finally {
    await store.close();
  }
}
