'use client';

export default function ErrorPage({
  reset,
}: {
  error: Error & { digest?: string };
  reset: () => void;
}) {
  return (
    <div className="shell fatal-state">
      <p className="eyebrow">Page temporarily unavailable</p>
      <h1>We couldn&apos;t load this page.</h1>
      <p>
        This display error does not change payment or job state. Try again without resubmitting a
        payment.
      </p>
      <button type="button" className="button button-primary" onClick={reset}>
        Try again
      </button>
    </div>
  );
}
