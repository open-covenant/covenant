import { renderToStaticMarkup } from 'react-dom/server';
import { describe, expect, it } from 'vitest';
import { mizukiTokenMint, TokenNote } from './token-disclosure';

describe('TokenNote', () => {
  it('publishes the verified mint and canonical explorers', () => {
    const markup = renderToStaticMarkup(<TokenNote />);

    expect(markup).toContain(mizukiTokenMint);
    expect(markup).toContain(
      'https://clawpump.tech/marketplace/agents/711fa8b1-5f37-4451-b7a7-bfcb9a021f6d',
    );
    expect(markup).toContain(`https://solscan.io/token/${mizukiTokenMint}`);
    expect(markup).not.toContain('Token activity is not available yet');
  });
});
