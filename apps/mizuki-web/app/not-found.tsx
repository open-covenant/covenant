import Link from 'next/link';

export default function NotFound() {
  return (
    <div className="shell fatal-state">
      <p className="eyebrow">404</p>
      <h1>No public record exists here.</h1>
      <p>The identifier may be wrong, or the record may not have reached a public state.</p>
      <Link href="/" className="button button-primary">
        Return home
      </Link>
    </div>
  );
}
