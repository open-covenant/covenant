import { renderToStaticMarkup } from 'react-dom/server';
import { describe, expect, it } from 'vitest';
import { mizukiTokenMint, TokenDisclosure } from './token-disclosure';

describe('TokenDisclosure', () => {
  it('publishes the verified mint and canonical explorers', () => {
    const markup = renderToStaticMarkup(<TokenDisclosure />);

    expect(markup).toContain(mizukiTokenMint);
    expect(markup).toContain(`https://pump.fun/coin/${mizukiTokenMint}`);
    expect(markup).toContain(`https://solscan.io/token/${mizukiTokenMint}`);
    expect(markup).not.toContain('Token activity is not available yet');
  });
});
