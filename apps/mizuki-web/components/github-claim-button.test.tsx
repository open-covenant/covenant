import { renderToStaticMarkup } from 'react-dom/server';
import { describe, expect, it } from 'vitest';
import { GithubClaimButton } from './github-claim-button';

describe('GitHub bounty authentication return path', () => {
  it('keeps the public bounty route as the default', () => {
    const html = renderToStaticMarkup(<GithubClaimButton bountyId="bounty/one" />);

    expect(html).toContain('return_to=%2Fbounties%2Fbounty%252Fone');
  });

  it('returns Workbench claims to the Workbench bounty room', () => {
    const html = renderToStaticMarkup(
      <GithubClaimButton bountyId="bounty-1" returnTo="/app/bounties/bounty-1" />,
    );

    expect(html).toContain('return_to=%2Fapp%2Fbounties%2Fbounty-1');
  });

  it('rejects a protocol-relative return target', () => {
    const html = renderToStaticMarkup(
      <GithubClaimButton bountyId="bounty-1" returnTo="//example.com/steal" />,
    );

    expect(html).toContain('return_to=%2Fbounties%2Fbounty-1');
    expect(html).not.toContain('example.com');
  });
});
