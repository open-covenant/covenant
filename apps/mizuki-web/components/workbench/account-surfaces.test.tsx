import { renderToStaticMarkup } from 'react-dom/server';
import { describe, expect, it } from 'vitest';
import { MobileMoreNavigation } from './account-surfaces';

describe('Workbench mobile More navigation', () => {
  it('links directly to every section omitted from the mobile tab bar', () => {
    const html = renderToStaticMarkup(<MobileMoreNavigation />);

    expect(html).toContain('href="/app/billing"');
    expect(html).toContain('href="/app/integrations"');
    expect(html).toContain('href="/app/settings"');
  });
});
