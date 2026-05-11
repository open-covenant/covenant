export type LeaseRequest = {
  gpuHours: number;
  durationSecs: number;
  capabilityHints?: string[];
};

export type LeaseReservation = {
  leaseId: string;
  gpuHours: number;
  expiresAt: number;
  pricedUsdMicro: number;
};

export interface ComputeProvider {
  readonly name: 'ionet' | 'akash';
  reserve(req: LeaseRequest): Promise<LeaseReservation>;
  activate(leaseId: string): Promise<void>;
  cancel(leaseId: string): Promise<{ refundUsdMicro: number }>;
  reclaim(leaseId: string): Promise<void>;
  status(leaseId: string): Promise<'reserved' | 'active' | 'cancelled' | 'reclaimed'>;
}

type IonetConfig = {
  apiUrl: string;
  apiKey: string;
};

async function ionetFetch(cfg: IonetConfig, path: string, init?: RequestInit): Promise<Response> {
  const controller = new AbortController();
  const timeout = setTimeout(() => controller.abort(), 30_000);
  try {
    const res = await fetch(`${cfg.apiUrl}${path}`, {
      ...init,
      signal: controller.signal,
      headers: {
        'content-type': 'application/json',
        authorization: `Bearer ${cfg.apiKey}`,
        ...init?.headers,
      },
    });
    if (!res.ok) {
      const body = await res.text().catch(() => '');
      throw new Error(`io.net ${init?.method ?? 'GET'} ${path} → ${res.status}: ${body}`);
    }
    return res;
  } finally {
    clearTimeout(timeout);
  }
}

class IonetProvider implements ComputeProvider {
  readonly name = 'ionet' as const;
  private cfg: IonetConfig;

  constructor(apiUrl: string, apiKey: string) {
    this.cfg = { apiUrl, apiKey };
  }

  async reserve(req: LeaseRequest): Promise<LeaseReservation> {
    const res = await ionetFetch(this.cfg, '/leases/reserve', {
      method: 'POST',
      body: JSON.stringify({
        gpu_hours: req.gpuHours,
        duration_secs: req.durationSecs,
        capability_hints: req.capabilityHints,
      }),
    });
    const data = (await res.json()) as {
      lease_id: string;
      gpu_hours: number;
      expires_at: number;
      priced_usd_micro: number;
    };
    return {
      leaseId: data.lease_id,
      gpuHours: data.gpu_hours,
      expiresAt: data.expires_at,
      pricedUsdMicro: data.priced_usd_micro,
    };
  }

  async activate(leaseId: string): Promise<void> {
    await ionetFetch(this.cfg, `/leases/${leaseId}/activate`, { method: 'POST' });
  }

  async cancel(leaseId: string): Promise<{ refundUsdMicro: number }> {
    const res = await ionetFetch(this.cfg, `/leases/${leaseId}/cancel`, { method: 'POST' });
    const data = (await res.json()) as { refund_usd_micro: number };
    return { refundUsdMicro: data.refund_usd_micro };
  }

  async reclaim(leaseId: string): Promise<void> {
    await ionetFetch(this.cfg, `/leases/${leaseId}/reclaim`, { method: 'POST' });
  }

  async status(leaseId: string): Promise<'reserved' | 'active' | 'cancelled' | 'reclaimed'> {
    const res = await ionetFetch(this.cfg, `/leases/${leaseId}`, { method: 'GET' });
    const data = (await res.json()) as { status: string };
    const s = data.status as ReturnType<ComputeProvider['status']> extends Promise<infer T> ? T : never;
    return s;
  }
}

type AkashConfig = {
  rpcUrl: string;
  wallet: string;
};

async function akashFetch(cfg: AkashConfig, path: string, init?: RequestInit): Promise<Response> {
  const controller = new AbortController();
  const timeout = setTimeout(() => controller.abort(), 30_000);
  try {
    const res = await fetch(`${cfg.rpcUrl}${path}`, {
      ...init,
      signal: controller.signal,
      headers: { 'content-type': 'application/json', ...init?.headers },
    });
    if (!res.ok) {
      const body = await res.text().catch(() => '');
      throw new Error(`akash ${init?.method ?? 'GET'} ${path} → ${res.status}: ${body}`);
    }
    return res;
  } finally {
    clearTimeout(timeout);
  }
}

class AkashProvider implements ComputeProvider {
  readonly name = 'akash' as const;
  private cfg: AkashConfig;

  constructor(rpcUrl: string, wallet: string) {
    this.cfg = { rpcUrl, wallet };
  }

  async reserve(req: LeaseRequest): Promise<LeaseReservation> {
    const res = await akashFetch(this.cfg, '/deployments', {
      method: 'POST',
      body: JSON.stringify({
        wallet: this.cfg.wallet,
        gpu_hours: req.gpuHours,
        duration_secs: req.durationSecs,
        capability_hints: req.capabilityHints,
      }),
    });
    const data = (await res.json()) as {
      deployment_id: string;
      gpu_hours: number;
      expires_at: number;
      priced_usd_micro: number;
    };
    return {
      leaseId: data.deployment_id,
      gpuHours: data.gpu_hours,
      expiresAt: data.expires_at,
      pricedUsdMicro: data.priced_usd_micro,
    };
  }

  async activate(leaseId: string): Promise<void> {
    await akashFetch(this.cfg, `/deployments/${leaseId}/activate`, {
      method: 'POST',
      body: JSON.stringify({ wallet: this.cfg.wallet }),
    });
  }

  async cancel(leaseId: string): Promise<{ refundUsdMicro: number }> {
    const res = await akashFetch(this.cfg, `/deployments/${leaseId}/close`, {
      method: 'POST',
      body: JSON.stringify({ wallet: this.cfg.wallet }),
    });
    const data = (await res.json()) as { refund_usd_micro: number };
    return { refundUsdMicro: data.refund_usd_micro };
  }

  async reclaim(leaseId: string): Promise<void> {
    await akashFetch(this.cfg, `/deployments/${leaseId}/reclaim`, {
      method: 'POST',
      body: JSON.stringify({ wallet: this.cfg.wallet }),
    });
  }

  async status(leaseId: string): Promise<'reserved' | 'active' | 'cancelled' | 'reclaimed'> {
    const res = await akashFetch(this.cfg, `/deployments/${leaseId}`, { method: 'GET' });
    const data = (await res.json()) as { status: string };
    return data.status as 'reserved' | 'active' | 'cancelled' | 'reclaimed';
  }
}

export function createProviders(cfg: {
  ionetApiUrl: string;
  ionetApiKey?: string;
  akashRpcUrl: string;
  akashWallet?: string;
}): { ionet: ComputeProvider; akash: ComputeProvider } {
  const ionet = cfg.ionetApiKey
    ? new IonetProvider(cfg.ionetApiUrl, cfg.ionetApiKey)
    : new IonetProviderUnconfigured();
  const akash = cfg.akashWallet
    ? new AkashProvider(cfg.akashRpcUrl, cfg.akashWallet)
    : new AkashProviderUnconfigured();
  return { ionet, akash };
}

class IonetProviderUnconfigured implements ComputeProvider {
  readonly name = 'ionet' as const;
  private fail(): never {
    throw new Error('io.net provider not configured: set IONET_API_KEY');
  }
  async reserve(): Promise<LeaseReservation> { this.fail(); }
  async activate(): Promise<void> { this.fail(); }
  async cancel(): Promise<{ refundUsdMicro: number }> { this.fail(); }
  async reclaim(): Promise<void> { this.fail(); }
  async status(): Promise<'reserved' | 'active' | 'cancelled' | 'reclaimed'> { this.fail(); }
}

class AkashProviderUnconfigured implements ComputeProvider {
  readonly name = 'akash' as const;
  private fail(): never {
    throw new Error('akash provider not configured: set AKASH_WALLET');
  }
  async reserve(): Promise<LeaseReservation> { this.fail(); }
  async activate(): Promise<void> { this.fail(); }
  async cancel(): Promise<{ refundUsdMicro: number }> { this.fail(); }
  async reclaim(): Promise<void> { this.fail(); }
  async status(): Promise<'reserved' | 'active' | 'cancelled' | 'reclaimed'> { this.fail(); }
}

export function selectProvider(
  requested: 'ionet' | 'akash',
  providers: { ionet: ComputeProvider; akash: ComputeProvider },
): ComputeProvider {
  return requested === 'ionet' ? providers.ionet : providers.akash;
}
