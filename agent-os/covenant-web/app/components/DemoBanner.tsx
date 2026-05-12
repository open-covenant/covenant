export function DemoBanner() {
  if (process.env.NEXT_PUBLIC_DEMO_MODE !== "1") return null;
  return (
    <div className="demo-banner" role="status">
      <span>
        <strong>Public sandbox.</strong> Shared state, resets periodically. Destructive actions are
        disabled.
      </span>
      <a
        className="demo-banner-link"
        href="https://github.com/open-covenant/covenant"
        target="_blank"
        rel="noreferrer"
      >
        Run a real instance →
      </a>
    </div>
  );
}
