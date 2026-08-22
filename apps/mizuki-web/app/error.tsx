'use client';

export default function ErrorPage({
  reset,
}: {
  error: Error & { digest?: string };
  reset: () => void;
}) {
  return (
    <div className="shell fatal-state">
      <p className="eyebrow">Request interrupted</p>
      <h1>This page could not be assembled.</h1>
      <p>
        The public systems remain separate from this interface. Retry the read without repeating any
        financial action.
      </p>
      <button type="button" className="button button-primary" onClick={reset}>
        Retry page
      </button>
    </div>
  );
}
