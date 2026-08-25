'use client';

import { useState } from 'react';
import { useRouter } from 'next/navigation';
import type { SolanaSignMessageFeature } from '@solana/wallet-standard-features';
import { bytesToBase64, useStandardWallet } from '@/lib/wallet-standard';
import { truncateAddress } from '@/lib/format';
import { sessionCsrfToken } from '@/lib/workbench-client';

type Challenge = { challengeId?: string; id?: string; message: string };

class CustomerClaimError extends Error {}

async function responseJson(
  response: Response,
  action: 'challenge' | 'claim',
): Promise<Record<string, unknown>> {
  const value = (await response.json().catch(() => ({}))) as Record<string, unknown>;
  if (!response.ok) {
    if (response.status === 401)
      throw new CustomerClaimError('Sign in with GitHub again, then retry the claim.');
    if (response.status === 409) {
      throw new CustomerClaimError(
        'This bounty is no longer available to claim. Refresh the page.',
      );
    }
    throw new CustomerClaimError(
      action === 'challenge'
        ? 'We could not prepare the wallet verification message. Try again.'
        : 'We could not confirm the claim. Refresh the page before trying again.',
    );
  }
  return value;
}

export function WalletProof({
  bountyId,
  disabled = false,
  onMutated,
}: {
  bountyId: string;
  disabled?: boolean;
  onMutated?: () => void | Promise<void>;
}) {
  const router = useRouter();
  const {
    wallets,
    connected,
    connecting,
    error: walletError,
    connect,
  } = useStandardWallet('message');
  const [state, setState] = useState<'idle' | 'challenging' | 'signing' | 'claiming' | 'complete'>(
    'idle',
  );
  const [error, setError] = useState<string | null>(null);

  async function proveAndClaim() {
    if (!connected) return;
    setError(null);
    try {
      setState('challenging');
      const csrfToken = await sessionCsrfToken();
      const challengeResponse = await fetch(
        `/api/mizuki/v1/bounties/${encodeURIComponent(bountyId)}/wallet-proof`,
        {
          method: 'POST',
          credentials: 'include',
          headers: {
            'content-type': 'application/json',
            'x-mizuki-csrf-token': csrfToken,
          },
          body: JSON.stringify({ address: connected.account.address }),
        },
      );
      const challenge = (await responseJson(challengeResponse, 'challenge')) as Challenge;
      if (typeof challenge.message !== 'string' || !challenge.message)
        throw new Error('The API returned an invalid wallet challenge');

      setState('signing');
      const feature = connected.wallet.features[
        'solana:signMessage'
      ] as SolanaSignMessageFeature['solana:signMessage'];
      const [signed] = await feature.signMessage({
        account: connected.account,
        message: new TextEncoder().encode(challenge.message),
      });
      if (!signed) throw new Error('The wallet did not return a signature');

      setState('claiming');
      const claimResponse = await fetch(
        `/api/mizuki/v1/bounties/${encodeURIComponent(bountyId)}/claim`,
        {
          method: 'POST',
          credentials: 'include',
          headers: {
            'content-type': 'application/json',
            'idempotency-key': crypto.randomUUID(),
            'x-mizuki-csrf-token': csrfToken,
          },
          body: JSON.stringify({
            challenge_id: challenge.challengeId || challenge.id,
            signature: bytesToBase64(signed.signature),
          }),
        },
      );
      await responseJson(claimResponse, 'claim');
      setState('complete');
      if (onMutated) await onMutated();
      else router.refresh();
    } catch (cause) {
      setState('idle');
      setError(
        cause instanceof CustomerClaimError
          ? cause.message
          : 'We could not verify this wallet or complete the claim. No transfer was authorized. Refresh the page and try again.',
      );
    }
  }

  if (disabled) {
    return <p className="claim-unavailable">This bounty is not accepting new claims.</p>;
  }

  return (
    <div className="wallet-proof">
      <div className="claim-step-heading">
        <span>02</span>
        <div>
          <strong>Verify your payout wallet</strong>
          <p>
            This signs a message only. It does not authorize a payment or allow Mizuki to move
            funds. The verified address becomes the payout address for this claim.
          </p>
        </div>
      </div>

      {!connected ? (
        wallets.length > 0 ? (
          <div className="wallet-options" aria-label="Compatible Solana wallets">
            {wallets.map((wallet) => (
              <button
                type="button"
                key={wallet.name}
                disabled={Boolean(connecting)}
                onClick={() => void connect(wallet)}
              >
                <span>{wallet.name}</span>
                <span>{connecting === wallet.name ? 'Connecting…' : 'Connect'}</span>
              </button>
            ))}
          </div>
        ) : (
          <div className="wallet-missing">
            <strong>No compatible wallet detected</strong>
            <p>
              Install or enable a Solana wallet that supports message signing, then reload this
              page.
            </p>
          </div>
        )
      ) : (
        <div className="connected-wallet">
          <div>
            <span>Connected account</span>
            <strong>{truncateAddress(connected.account.address, 7)}</strong>
          </div>
          <button
            className="button button-primary"
            type="button"
            onClick={() => void proveAndClaim()}
            disabled={state !== 'idle'}
          >
            {state === 'challenging'
              ? 'Preparing proof…'
              : state === 'signing'
                ? 'Confirm in wallet…'
                : state === 'claiming'
                  ? 'Claiming…'
                  : state === 'complete'
                    ? 'Bounty claimed'
                    : 'Verify wallet and claim bounty'}
          </button>
        </div>
      )}
      {(walletError || error) && (
        <p className="form-error" role="alert">
          {error ||
            (walletError
              ? 'We could not connect to that wallet. Check the wallet and try again.'
              : '')}
        </p>
      )}
      {state === 'complete' && (
        <p className="form-success">Bounty claimed. Your 48-hour work period has started.</p>
      )}
    </div>
  );
}
