import { useCallback, useEffect, useMemo, useRef, useState } from 'react';

import { computeApi, isDemoMode } from './api';
import { AppCard } from './components/AppCard';
import { BrandMark } from './components/BrandMark';
import { Icon } from './components/Icon';
import { JobPanel } from './components/JobPanel';
import type {
  ComputeApp,
  ComputeJob,
  ComputeOffer,
  LaunchPlan,
  LaunchRequest,
  RuntimeStatus,
  TrustClass,
} from './domain';
import {
  errorMessage,
  formatDuration,
  formatTrust,
  formatUsdc,
  formatVram,
  showPrivateBetaAccess,
  terminalStatuses,
  trustClasses,
} from './domain';

const initialRuntime: RuntimeStatus = {
  state: 'offline',
  endpoint_label: null,
  message: 'Connecting to the local runtime…',
  authentication: { source: 'none' },
  token_required: false,
};

function requestFor(
  app: ComputeApp,
  durationMinutes: number,
  budgetUsdc: string,
  trust: TrustClass,
): LaunchRequest {
  const budget = Number(budgetUsdc);
  if (!Number.isFinite(durationMinutes) || durationMinutes <= 0) {
    throw new Error('Duration must be greater than zero.');
  }
  if (!Number.isFinite(budget) || budget <= 0) {
    throw new Error('Allowance cap must be greater than zero.');
  }

  const duration_secs = Math.round(durationMinutes * 60);
  if (duration_secs > app.max_duration_secs) {
    throw new Error(`Duration cannot exceed ${formatDuration(app.max_duration_secs)}.`);
  }

  return {
    app_id: app.id,
    duration_secs,
    max_usdc_micros: Math.round(budget * 1_000_000),
    min_trust: trust,
  };
}

export default function App() {
  const [runtime, setRuntime] = useState<RuntimeStatus>(initialRuntime);
  const [connecting, setConnecting] = useState(true);
  const [apps, setApps] = useState<ComputeApp[]>([]);
  const [offers, setOffers] = useState<ComputeOffer[]>([]);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [durationMinutes, setDurationMinutes] = useState(30);
  const [budgetUsdc, setBudgetUsdc] = useState('0.50');
  const [trust, setTrust] = useState<TrustClass>('open');
  const [plan, setPlan] = useState<LaunchPlan | null>(null);
  const [planKey, setPlanKey] = useState<string | null>(null);
  const [job, setJob] = useState<ComputeJob | null>(null);
  const [quotePending, setQuotePending] = useState(false);
  const [launchPending, setLaunchPending] = useState(false);
  const [cancelPending, setCancelPending] = useState(false);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);
  const [accessToken, setAccessToken] = useState('');
  const [accessPending, setAccessPending] = useState(false);
  const [accessError, setAccessError] = useState<string | null>(null);
  const refreshGeneration = useRef(0);

  const selectedApp = apps.find((app) => app.id === selectedId);
  const onlineOffers = offers.filter((offer) => offer.online);
  const lowestRate = onlineOffers.length
    ? Math.min(...onlineOffers.map((offer) => offer.rate_usdc_micros_per_hour))
    : null;
  const compatibleOffers = selectedApp
    ? onlineOffers.filter(
        (offer) =>
          offer.gpu.vram_mib >= selectedApp.min_vram_mib &&
          trustClasses.indexOf(offer.trust_class) >= trustClasses.indexOf(trust),
      )
    : [];

  const refresh = useCallback(async () => {
    const generation = ++refreshGeneration.current;
    setConnecting(true);
    setLoadError(null);

    const [runtimeResult, appsResult, offersResult, jobsResult] = await Promise.allSettled([
      computeApi.runtimeStatus(),
      computeApi.listApps(),
      computeApi.listOffers(),
      computeApi.listJobs(),
    ]);
    if (generation !== refreshGeneration.current) return;

    const nextRuntime =
      runtimeResult.status === 'fulfilled'
        ? runtimeResult.value
        : {
            state: 'offline',
            endpoint_label: null,
            message: errorMessage(runtimeResult.reason),
            authentication: { source: 'none' },
            token_required: false,
          } satisfies RuntimeStatus;
    setRuntime(nextRuntime);

    if (appsResult.status === 'fulfilled') {
      setApps(appsResult.value);
      setSelectedId((current) => {
        if (current && appsResult.value.some((app) => app.id === current)) return current;
        return (
          appsResult.value.find((app) => app.availability === 'available')?.id ??
          appsResult.value[0]?.id ??
          null
        );
      });
    } else {
      setLoadError(`Could not load the app catalog. ${errorMessage(appsResult.reason)}`);
    }

    if (offersResult.status === 'fulfilled') {
      setOffers(offersResult.value);
    } else {
      setOffers([]);
      if (appsResult.status === 'fulfilled' && !nextRuntime.token_required) {
        setLoadError(
          `Catalog loaded, but offers are unavailable. ${errorMessage(offersResult.reason)}`,
        );
      }
    }

    if (jobsResult.status === 'fulfilled' && jobsResult.value.length) {
      setJob(
        (current) =>
          current ??
          jobsResult.value.find((candidate) => !terminalStatuses.has(candidate.status)) ??
          jobsResult.value.at(-1) ??
          null,
      );
    }

    setConnecting(false);
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  useEffect(() => {
    if (!selectedApp) return;
    setDurationMinutes(Math.ceil(selectedApp.default_duration_secs / 60));
    setBudgetUsdc((selectedApp.default_max_usdc_micros / 1_000_000).toFixed(2));
    setTrust(selectedApp.min_trust);
    setPlan(null);
    setPlanKey(null);
    setActionError(null);
  }, [selectedApp?.id]);

  useEffect(() => {
    if (!job || terminalStatuses.has(job.status)) return;

    const timer = window.setInterval(() => {
      void computeApi
        .getJob(job.id)
        .then(setJob)
        .catch((error: unknown) => setActionError(`Status refresh failed. ${errorMessage(error)}`));
    }, 2_500);

    return () => window.clearInterval(timer);
  }, [job?.id, job?.status]);

  const trustOptions = useMemo(() => {
    if (!selectedApp) return trustClasses;
    const minimum = trustClasses.indexOf(selectedApp.min_trust);
    return trustClasses.slice(minimum);
  }, [selectedApp]);

  function updateRequest(update: () => void) {
    update();
    setPlan(null);
    setPlanKey(null);
    setActionError(null);
  }

  function selectApp(app: ComputeApp) {
    setSelectedId(app.id);
  }

  async function reviewQuote() {
    if (!selectedApp) return;
    setQuotePending(true);
    setActionError(null);
    try {
      const request = requestFor(selectedApp, durationMinutes, budgetUsdc, trust);
      const idempotencyKey = crypto.randomUUID();
      setPlan(await computeApi.planJob(request, idempotencyKey));
      setPlanKey(idempotencyKey);
    } catch (error) {
      setPlan(null);
      setPlanKey(null);
      setActionError(errorMessage(error));
    } finally {
      setQuotePending(false);
    }
  }

  async function launch() {
    if (!selectedApp || !plan || !planKey) return;
    setLaunchPending(true);
    setActionError(null);
    try {
      const request = requestFor(selectedApp, durationMinutes, budgetUsdc, trust);
      setJob(await computeApi.launchJob(request, planKey));
      setPlan(null);
      setPlanKey(null);
    } catch (error) {
      setActionError(errorMessage(error));
    } finally {
      setLaunchPending(false);
    }
  }

  async function cancel() {
    if (!job) return;
    setCancelPending(true);
    setActionError(null);
    try {
      setJob(await computeApi.cancelJob(job.id));
    } catch (error) {
      setActionError(errorMessage(error));
    } finally {
      setCancelPending(false);
    }
  }

  async function openWorkspace() {
    if (!job) return;
    setActionError(null);
    try {
      await computeApi.openAccessUrl(job.id);
    } catch (error) {
      setActionError(errorMessage(error));
    }
  }

  async function configureAccess() {
    if (!accessToken || accessPending || launchPending || cancelPending) return;
    const token = accessToken;
    setAccessToken('');
    setAccessPending(true);
    setAccessError(null);
    setJob(null);
    setPlan(null);
    setPlanKey(null);
    try {
      await computeApi.configureSessionToken(token);
      await refresh();
    } catch (error) {
      setAccessError(errorMessage(error));
    } finally {
      setAccessPending(false);
    }
  }

  async function clearAccess() {
    if (accessPending || launchPending || cancelPending) return;
    setAccessPending(true);
    setAccessError(null);
    setJob(null);
    setPlan(null);
    setPlanKey(null);
    try {
      await computeApi.clearSessionToken();
      await refresh();
    } catch (error) {
      setAccessError(errorMessage(error));
    } finally {
      setAccessPending(false);
    }
  }

  const runtimeLabel = connecting ? 'Connecting' : runtime.state;
  const canQuote =
    runtime.state === 'connected' &&
    !connecting &&
    selectedApp?.availability === 'available' &&
    compatibleOffers.length > 0;
  const showAccess = showPrivateBetaAccess(runtime, isDemoMode);
  const accessChangeBlocked = accessPending || launchPending || cancelPending;

  return (
    <div className="app-shell">
      <header className="topbar">
        <div className="brand">
          <BrandMark />
          <div>
            <span>Covenant</span>
            <strong>Compute</strong>
          </div>
        </div>
        <div className="runtime-control">
          {isDemoMode && <span className="demo-badge">Simulation</span>}
          <div className={`connection connection--${connecting ? 'checking' : runtime.state}`}>
            <span className="status-dot" />
            <span>
              <small>Runtime</small>
              <strong>{runtimeLabel}</strong>
            </span>
          </div>
          <button
            aria-label="Refresh runtime and offers"
            className="icon-button icon-button--large"
            disabled={connecting}
            onClick={() => void refresh()}
            type="button"
          >
            <Icon className={connecting ? 'spin' : undefined} name="refresh" />
          </button>
        </div>
      </header>

      <main>
        <section className="hero">
          <div className="hero__copy">
            <p className="eyebrow">
              <span className="eyebrow__line" />
              Bounded GPU compute
            </p>
            <h1>
              A GPU when you need it.
              <span>An explicit allowance before it starts.</span>
            </h1>
            <p>
              Choose a workload, set a USDC-denominated allowance cap, and review the
              exact machine and quote before anything launches.
            </p>
          </div>
          <div className="capacity-card">
            <div className="capacity-card__icon">
              <Icon name="server" />
            </div>
            <div>
              <strong>{connecting ? '—' : onlineOffers.length}</strong>
              <span>GPUs ready</span>
            </div>
            <div>
              <strong>{lowestRate === null ? '—' : formatUsdc(lowestRate)}</strong>
              <span>from / hour</span>
            </div>
          </div>
        </section>

        {runtime.message && (
          <div className={`runtime-message runtime-message--${runtime.state}`}>
            <Icon name={runtime.state === 'connected' ? 'check' : 'server'} />
            <span>
              {runtime.endpoint_label && <strong>{runtime.endpoint_label}</strong>}
              {runtime.message}
            </span>
          </div>
        )}

        {showAccess && (
          <section className="access-panel" aria-label="Private beta access">
            <div>
              <p className="step-label">Private beta access</p>
              {runtime.authentication.source === 'session' && !runtime.token_required ? (
                <p>
                  <strong>Session token configured.</strong> It is held only in app
                  memory until you exit.
                </p>
              ) : (
                <p>
                  Enter your invite token. It is held only in app memory until you exit
                  and is never saved to disk.
                </p>
              )}
            </div>
            {runtime.authentication.source === 'session' && !runtime.token_required ? (
              <button
                className="access-panel__clear"
                disabled={accessChangeBlocked}
                onClick={() => void clearAccess()}
                type="button"
              >
                {accessPending ? 'Clearing…' : 'Clear session'}
              </button>
            ) : (
              <form
                className="access-panel__form"
                onSubmit={(event) => {
                  event.preventDefault();
                  void configureAccess();
                }}
              >
                <label className="sr-only" htmlFor="private-beta-token">
                  Private beta access token
                </label>
                <input
                  autoCapitalize="none"
                  autoComplete="off"
                  id="private-beta-token"
                  maxLength={4_096}
                  disabled={accessChangeBlocked}
                  onChange={(event) => {
                    setAccessToken(event.target.value);
                    setAccessError(null);
                  }}
                  placeholder="Access token"
                  spellCheck={false}
                  type="password"
                  value={accessToken}
                />
                <button disabled={!accessToken || accessChangeBlocked} type="submit">
                  {accessPending ? 'Connecting…' : 'Connect'}
                </button>
                {runtime.authentication.source === 'session' && (
                  <button
                    className="access-panel__clear"
                    disabled={accessChangeBlocked}
                    onClick={() => void clearAccess()}
                    type="button"
                  >
                    Clear session
                  </button>
                )}
              </form>
            )}
            {accessError && (
              <p className="access-panel__error" role="alert">
                {accessError}
              </p>
            )}
          </section>
        )}

        {loadError && (
          <div className="inline-alert inline-alert--error" role="alert">
            {loadError}
            <button onClick={() => void refresh()} type="button">
              Try again
            </button>
          </div>
        )}

        <div className="product-grid">
          <section className="catalog">
            <div className="section-heading">
              <div>
                <p className="step-label">01 / Choose an app</p>
                <h2>What do you want to run?</h2>
              </div>
              <span>{apps.length} apps</span>
            </div>

            {connecting && apps.length === 0 ? (
              <div className="app-grid app-grid--loading">
                {[0, 1, 2].map((item) => (
                  <div className="app-card app-card--skeleton" key={item} />
                ))}
              </div>
            ) : (
              <div className="app-grid">
                {apps.map((app) => (
                  <AppCard
                    app={app}
                    key={app.id}
                    onSelect={selectApp}
                    selected={app.id === selectedId}
                  />
                ))}
              </div>
            )}

            <div className="trust-note">
              <Icon name="shield" />
              <p>
                <strong>The beta account cannot be charged past its allowance.</strong>
                The control plane requests deletion at the selected duration and retries
                until Vast confirms it. Provider billing may continue during an outage;
                this build does not connect a wallet or move user funds.
                <button
                  className="trust-note__link"
                  onClick={() => void computeApi.openJupyterSetupGuide()}
                  type="button"
                >
                  First workspace: install Vast’s Jupyter certificate
                </button>
              </p>
            </div>
          </section>

          <aside className="control-panel">
            {job ? (
              <>
                <JobPanel
                  app={apps.find((app) => app.id === job.app_id)}
                  busy={cancelPending}
                  job={job}
                  onCancel={cancel}
                  onOpen={openWorkspace}
                />
                {terminalStatuses.has(job.status) && (
                  <button
                    className="secondary-button secondary-button--full"
                    onClick={() => {
                      setJob(null);
                      setActionError(null);
                    }}
                    type="button"
                  >
                    Start another workload
                    <Icon name="arrow" />
                  </button>
                )}
                {actionError && (
                  <p className="inline-alert inline-alert--error" role="alert">
                    {actionError}
                  </p>
                )}
              </>
            ) : selectedApp?.availability === 'preview' ? (
              <div className="preview-panel">
                <span className="preview-panel__icon">
                  <Icon name={selectedApp.kind === 'image' ? 'image' : 'chat'} />
                </span>
                <p className="step-label">Release preview</p>
                <h2>{selectedApp.name} is not launchable yet</h2>
                <p>
                  It will move to Available only after its pinned runtime image,
                  health check, access route, and cancellation path pass release
                  validation.
                </p>
                <ul>
                  <li><Icon name="check" /> App experience designed</li>
                  <li><span /> Runtime image validation pending</li>
                  <li><span /> End-to-end cancellation pending</li>
                </ul>
              </div>
            ) : selectedApp ? (
              <div className="launcher">
                <div className="section-heading section-heading--compact">
                  <div>
                    <p className="step-label">02 / Set limits</p>
                    <h2>Configure launch</h2>
                  </div>
                  <Icon name="gpu" />
                </div>

                <div className="field-group">
                  <div className="field-heading">
                    <label htmlFor="duration">Duration</label>
                    <span>Maximum {formatDuration(selectedApp.max_duration_secs)}</span>
                  </div>
                  <div className="segmented">
                    {[30, 60, 120].map((minutes) => (
                      <button
                        aria-pressed={durationMinutes === minutes}
                        className={durationMinutes === minutes ? 'active' : ''}
                        disabled={minutes * 60 > selectedApp.max_duration_secs}
                        key={minutes}
                        onClick={() => updateRequest(() => setDurationMinutes(minutes))}
                        type="button"
                      >
                        {minutes < 60 ? `${minutes}m` : `${minutes / 60}h`}
                      </button>
                    ))}
                    <label className="segmented__custom">
                      <span className="sr-only">Custom duration in minutes</span>
                      <input
                        id="duration"
                        max={Math.floor(selectedApp.max_duration_secs / 60)}
                        min="1"
                        onChange={(event) =>
                          updateRequest(() => setDurationMinutes(Number(event.target.value)))
                        }
                        type="number"
                        value={durationMinutes}
                      />
                      <span>min</span>
                    </label>
                  </div>
                </div>

                <div className="field-row">
                  <div className="field-group">
                    <div className="field-heading">
                      <label htmlFor="budget">USDC allowance cap</label>
                    </div>
                    <label className="money-input">
                      <span>$</span>
                      <input
                        id="budget"
                        inputMode="decimal"
                        min="0.01"
                        onChange={(event) =>
                          updateRequest(() => setBudgetUsdc(event.target.value))
                        }
                        step="0.01"
                        type="number"
                        value={budgetUsdc}
                      />
                      <strong>USDC</strong>
                    </label>
                  </div>

                  <div className="field-group">
                    <div className="field-heading">
                      <label htmlFor="trust">Minimum trust</label>
                    </div>
                    <label className="select-input">
                      <Icon name="shield" />
                      <select
                        id="trust"
                        onChange={(event) =>
                          updateRequest(() => setTrust(event.target.value as TrustClass))
                        }
                        value={trust}
                      >
                        {trustOptions.map((value) => (
                          <option key={value} value={value}>
                            {formatTrust(value)}
                          </option>
                        ))}
                      </select>
                    </label>
                  </div>
                </div>

                <div className="offer-summary">
                  <span className="offer-summary__pulse" />
                  <span>
                    <strong>{compatibleOffers.length}</strong> compatible GPU
                    {compatibleOffers.length === 1 ? '' : 's'} online
                  </span>
                  <span>{formatVram(selectedApp.min_vram_mib)} minimum</span>
                </div>

                {trust === 'open' && (
                  <div className="inline-alert inline-alert--warning">
                    <Icon name="shield" />
                    Open executors can observe workload data. Do not include private
                    keys or sensitive files.
                  </div>
                )}

                {!plan ? (
                  <button
                    className="primary-button"
                    disabled={!canQuote || quotePending}
                    onClick={() => void reviewQuote()}
                    type="button"
                  >
                    {quotePending ? 'Finding a match…' : 'Review exact quote'}
                    <Icon name="arrow" />
                  </button>
                ) : (
                  <div className="quote">
                    <div className="quote__header">
                      <div>
                        <p className="step-label">03 / Review and launch</p>
                        <h3>Exact match</h3>
                      </div>
                      <span className="tag tag--available">
                        <Icon name="check" />
                        Within cap
                      </span>
                    </div>
                    <dl className="quote__details">
                      <div>
                        <dt>GPU</dt>
                        <dd>{plan.offer.gpu.model}</dd>
                        <small>{formatVram(plan.offer.gpu.vram_mib)} VRAM</small>
                      </div>
                      <div>
                        <dt>Rate</dt>
                        <dd>{formatUsdc(plan.offer.rate_usdc_micros_per_hour)}</dd>
                        <small>per hour</small>
                      </div>
                      <div>
                        <dt>Duration</dt>
                        <dd>{formatDuration(plan.duration_secs)}</dd>
                        <small>requested window</small>
                      </div>
                    </dl>
                    <div className="quote__total">
                      <span>
                        Maximum allowance
                        <small>Caps beta-account usage, not the provider invoice.</small>
                      </span>
                      <strong>{formatUsdc(plan.maximum_usdc_micros)}</strong>
                    </div>
                    <button
                      className="primary-button primary-button--launch"
                      disabled={launchPending || !planKey}
                      onClick={() => void launch()}
                      type="button"
                    >
                      <Icon name="workspace" />
                      {launchPending ? 'Launching…' : 'Launch with allowance'}
                    </button>
                    <button
                      className="text-button"
                      disabled={launchPending}
                      onClick={() => setPlan(null)}
                      type="button"
                    >
                      Change limits
                    </button>
                  </div>
                )}

                {actionError && (
                  <p className="inline-alert inline-alert--error" role="alert">
                    {actionError}
                  </p>
                )}
              </div>
            ) : (
              <div className="empty-panel">
                <Icon name="server" />
                <h2>No app selected</h2>
                <p>Select an app to configure a bounded GPU workload.</p>
              </div>
            )}
          </aside>
        </div>
      </main>

      <footer>
        <span>Covenant Compute alpha</span>
        <span>Usage priced in USDC; wallet settlement is not active</span>
        <span>Terminal usage produces provider evidence</span>
      </footer>
    </div>
  );
}
