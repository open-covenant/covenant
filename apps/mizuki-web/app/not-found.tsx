import Link from 'next/link';

export default function NotFound() {
  return (
    <div className="shell fatal-state">
      <p className="eyebrow">404</p>
      <h1>Record not found.</h1>
      <p>Check the link. The record may not exist or may not yet be public.</p>
      <Link href="/" className="button button-primary">
        Return home
      </Link>
    </div>
  );
}
