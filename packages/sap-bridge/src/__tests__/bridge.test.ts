import { describe, it, expect } from 'vitest';
import { Keypair } from '@solana/web3.js';
import {
  BridgeDisabledError,
  BridgeVerifierRequiredError,
  SapBridge,
  resolveSynapseConfig,
  type SapKeypair,
} from '../index.js';

// Minimal stand-in for a loaded keypair. The verifier/signer guards run
// before any SDK load, so these tests never touch the network.
const fakeKeypair: SapKeypair = {
  publicKey: { toBase58: () => 'Fake1111111111111111111111111111111111111111', toBuffer: () => Buffer.alloc(32) },
  secretKey: new Uint8Array(64),
};

// A real keypair whose pubkey round-trips through web3.PublicKey. The
// attestAgent input guards run after loadSdk() but before any RPC, so a
// real verifier + valid agent PDA reaches them without touching the
// network (createSapClient builds a lazy Connection).
const realVerifier = (): SapKeypair => Keypair.generate() as unknown as SapKeypair;
const validAgentPda = () => Keypair.generate().publicKey.toBase58();
const validRoot = '00'.repeat(32);

const enabledConfig = () =>
  resolveSynapseConfig({ COVENANT_SAP_ENABLED: 'true', COVENANT_SOLANA_CLUSTER: 'devnet' });

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

  it('reports verifier presence in the status snapshot', () => {
    const config = enabledConfig();
    expect(new SapBridge({ config }).status().hasVerifier).toBe(false);
    expect(new SapBridge({ config, verifier: fakeKeypair }).status().hasVerifier).toBe(true);
  });

  it('attestAgent is a disabled no-op when the bridge is off', async () => {
    const bridge = new SapBridge({ config: resolveSynapseConfig({}), verifier: fakeKeypair });
    await expect(
      bridge.attestAgent({ agentPda: 'Agent111', rootHashHex: '00'.repeat(32) }),
    ).rejects.toBeInstanceOf(BridgeDisabledError);
  });

  it('attestAgent requires a verifier keypair when enabled', async () => {
    const bridge = new SapBridge({ config: enabledConfig() });
    await expect(
      bridge.attestAgent({ agentPda: 'Agent111', rootHashHex: '00'.repeat(32) }),
    ).rejects.toBeInstanceOf(BridgeVerifierRequiredError);
  });

  it('attestAgent rejects an audit root that is not 32 bytes of hex', async () => {
    const bridge = new SapBridge({ config: enabledConfig(), verifier: realVerifier() });
    await expect(
      bridge.attestAgent({ agentPda: validAgentPda(), rootHashHex: '00'.repeat(16) }),
    ).rejects.toThrow('audit root must be a 32-byte hex string');
    await expect(
      bridge.attestAgent({ agentPda: validAgentPda(), rootHashHex: 'zz' + '00'.repeat(31) }),
    ).rejects.toThrow('audit root must be a 32-byte hex string');
  });

  it('attestAgent rejects an attestationType outside 1..=32 characters', async () => {
    const bridge = new SapBridge({ config: enabledConfig(), verifier: realVerifier() });
    await expect(
      bridge.attestAgent({ agentPda: validAgentPda(), rootHashHex: validRoot, attestationType: '' }),
    ).rejects.toThrow('attestationType must be 1..=32 characters');
    await expect(
      bridge.attestAgent({ agentPda: validAgentPda(), rootHashHex: validRoot, attestationType: 'x'.repeat(33) }),
    ).rejects.toThrow('attestationType must be 1..=32 characters');
  });
});
