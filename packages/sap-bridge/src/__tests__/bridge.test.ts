import { describe, it, expect } from 'vitest';
import {
  BridgeDisabledError,
  SapBridge,
  resolveSynapseConfig,
} from '../index.js';

describe('SapBridge', () => {
  it('exposes a status snapshot when disabled', () => {
    const bridge = new SapBridge({ config: resolveSynapseConfig({}) });
    const status = bridge.status();
    expect(status.enabled).toBe(false);
    expect(status.programId).toBe('SAPpUhsWLJG1FfkGRcXagEDMrMsWGjbky7AyhGpFETZ');
  });

  it('throws BridgeDisabledError on network calls when disabled', async () => {
    const bridge = new SapBridge({ config: resolveSynapseConfig({}) });
    await expect(bridge.publishAgent({
      name: 'demo',
      capabilities: [],
      pricing: [],
      protocols: [],
    })).rejects.toBeInstanceOf(BridgeDisabledError);
    await expect(bridge.findAgentsByProtocol('jupiter'))
      .rejects.toBeInstanceOf(BridgeDisabledError);
  });

  it('reflects opt-in env in the status snapshot', () => {
    const config = resolveSynapseConfig({
      COVENANT_SAP_ENABLED: 'true',
      COVENANT_SOLANA_CLUSTER: 'devnet',
    });
    const bridge = new SapBridge({ config });
    const status = bridge.status();
    expect(status.enabled).toBe(true);
    expect(status.cluster).toBe('devnet');
    expect(status.rpcUrl).toBe('https://api.devnet.solana.com');
  });
});
