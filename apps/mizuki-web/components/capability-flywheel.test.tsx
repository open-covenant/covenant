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

    expect(html).toContain('Example data');
    expect(html).not.toContain('Live service data');
  });
});
