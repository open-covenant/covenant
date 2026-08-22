import { renderToStaticMarkup } from 'react-dom/server';
import { describe, expect, it } from 'vitest';
import { demoCapabilities, demoMetrics, demoTreasury } from '@/lib/demo';
import { CapabilityFlywheel } from './capability-flywheel';

describe('CapabilityFlywheel', () => {
  it('does not describe fixture data as live records', () => {
    const html = renderToStaticMarkup(
      <CapabilityFlywheel
        metrics={demoMetrics}
        treasury={demoTreasury}
        capabilities={demoCapabilities}
        demo
      />,
    );

    expect(html).toContain('Illustrative fixture');
    expect(html).not.toContain('Live backend records only');
  });
});
