'use client';

export function GithubClaimButton({ bountyId }: { bountyId: string }) {
  const base = process.env.NEXT_PUBLIC_MIZUKI_GITHUB_OAUTH_URL || '/api/mizuki/v1/auth/github';
  const separator = base.includes('?') ? '&' : '?';
  const returnTo = `/bounties/${encodeURIComponent(bountyId)}`;
  const href = `${base}${separator}return_to=${encodeURIComponent(returnTo)}&bounty_id=${encodeURIComponent(bountyId)}`;

  return (
    <a className="button button-secondary claim-action" href={href}>
      Continue with GitHub <span aria-hidden="true">↗</span>
    </a>
  );
}
