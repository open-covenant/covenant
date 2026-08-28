'use client';

import {
  createContext,
  useContext,
  useEffect,
  useId,
  useRef,
  useState,
  type ReactNode,
} from 'react';
import { truncateAddress } from '@/lib/format';
import { paymentWalletNetwork, useStandardWallet } from '@/lib/wallet-standard';

export type WorkbenchWalletSession = ReturnType<typeof useStandardWallet>;

const WorkbenchWalletContext = createContext<WorkbenchWalletSession | null>(null);

export function WorkbenchWalletProvider({ children }: { children: ReactNode }) {
  const wallet = useStandardWallet('transaction');
  return (
    <WorkbenchWalletContext.Provider value={wallet}>{children}</WorkbenchWalletContext.Provider>
  );
}

export function useWorkbenchWallet(): WorkbenchWalletSession {
  const wallet = useContext(WorkbenchWalletContext);
  if (!wallet) throw new Error('WorkbenchWalletProvider is required');
  return wallet;
}

export function WorkbenchWalletControl() {
  return <WorkbenchWalletControlView {...useWorkbenchWallet()} />;
}

export function WorkbenchWalletControlView({
  wallets,
  connected,
  ready,
  connecting,
  error,
  connect,
  disconnect,
}: WorkbenchWalletSession) {
  const active = connected && ready ? connected : null;
  const [open, setOpen] = useState(false);
  const root = useRef<HTMLDivElement>(null);
  const trigger = useRef<HTMLButtonElement>(null);
  const panel = useRef<HTMLElement>(null);
  const firstAction = useRef<HTMLButtonElement>(null);
  const previousActive = useRef(active);
  const menuId = useId();

  useEffect(() => {
    const connectedNow = Boolean(active && previousActive.current !== active);
    previousActive.current = active;
    if (!connectedNow || !open) return;
    setOpen(false);
    trigger.current?.focus();
  }, [active, open]);

  useEffect(() => {
    if (!open) return;
    const frame = requestAnimationFrame(() => (firstAction.current ?? panel.current)?.focus());
    return () => cancelAnimationFrame(frame);
  }, [open, active]);

  useEffect(() => {
    if (!open) return;

    function closeOnPointerDown(event: PointerEvent) {
      if (event.target instanceof Node && !root.current?.contains(event.target)) setOpen(false);
    }

    function closeOnEscape(event: KeyboardEvent) {
      if (event.key !== 'Escape') return;
      setOpen(false);
      trigger.current?.focus();
    }

    document.addEventListener('pointerdown', closeOnPointerDown);
    document.addEventListener('keydown', closeOnEscape);
    return () => {
      document.removeEventListener('pointerdown', closeOnPointerDown);
      document.removeEventListener('keydown', closeOnEscape);
    };
  }, [open]);

  const label = connecting
    ? 'Connecting…'
    : active
      ? truncateAddress(active.account.address, 5)
      : 'Connect';

  return (
    <div className="workbench-wallet-control" ref={root}>
      <button
        ref={trigger}
        type="button"
        className="workbench-wallet-trigger"
        aria-controls={menuId}
        aria-expanded={open}
        aria-haspopup="dialog"
        aria-label={active ? `Payment wallet ${active.account.address}` : 'Connect payment wallet'}
        disabled={Boolean(connecting)}
        onClick={() => setOpen((current) => !current)}
      >
        <span className={active ? 'workbench-wallet-dot connected' : 'workbench-wallet-dot'} />
        <span>{label}</span>
        <span className="workbench-wallet-chevron" aria-hidden="true">
          {open ? '↑' : '↓'}
        </span>
      </button>

      <section
        ref={panel}
        id={menuId}
        className="workbench-wallet-menu"
        role="dialog"
        aria-label="Payment wallet"
        tabIndex={-1}
        hidden={!open}
      >
        <header>
          <span>Payment wallet</span>
          <h2>{active ? 'Wallet connected' : 'Choose a Solana wallet'}</h2>
          <p>Used only when you approve a fixed-price USDC payment.</p>
        </header>

        {active ? (
          <>
            <dl className="workbench-wallet-summary">
              <div>
                <dt>Wallet</dt>
                <dd>{active.wallet.name}</dd>
              </div>
              <div>
                <dt>Account</dt>
                <dd title={active.account.address}>{truncateAddress(active.account.address, 8)}</dd>
              </div>
              <div>
                <dt>Network</dt>
                <dd>{paymentWalletNetwork().label}</dd>
              </div>
            </dl>
            <div className="workbench-wallet-connected-actions">
              <button
                ref={firstAction}
                type="button"
                onClick={() => {
                  void disconnect().then(() => firstAction.current?.focus());
                }}
              >
                Change wallet
              </button>
              <button
                type="button"
                onClick={() => {
                  setOpen(false);
                  trigger.current?.focus();
                  void disconnect();
                }}
              >
                Disconnect
              </button>
            </div>
          </>
        ) : wallets.length > 0 ? (
          <div className="workbench-wallet-list">
            {wallets.map((wallet, index) => (
              <button
                ref={index === 0 ? firstAction : undefined}
                type="button"
                key={wallet.name}
                disabled={Boolean(connecting)}
                onClick={() => void connect(wallet)}
              >
                <span aria-hidden="true">{String(index + 1).padStart(2, '0')}</span>
                <span>
                  <strong>{wallet.name}</strong>
                  <small>
                    {wallet.name === 'WalletConnect'
                      ? 'Scan a QR code or open a mobile wallet'
                      : 'Connect the browser wallet'}
                  </small>
                </span>
                <span aria-hidden="true">{connecting === wallet.name ? '…' : '↗'}</span>
              </button>
            ))}
          </div>
        ) : (
          <p className="workbench-wallet-empty">
            No compatible wallet is available. Install a Wallet Standard wallet or use WalletConnect
            on a supported device.
          </p>
        )}

        {error && (
          <p className="workbench-wallet-error" role="alert">
            {error}
          </p>
        )}
      </section>
    </div>
  );
}
