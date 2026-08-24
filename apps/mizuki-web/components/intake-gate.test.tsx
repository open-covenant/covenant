import { renderToStaticMarkup } from 'react-dom/server';
import { describe, expect, it } from 'vitest';
import { IntakeGate } from './intake-gate';

describe('IntakeGate', () => {
  it('omits transactional controls when paid intake is closed', () => {
    const html = renderToStaticMarkup(
      <IntakeGate admission={{ status: 'ready', data: { intakeEnabled: false } }}>
        <button type="button">Get fixed quote</button>
      </IntakeGate>,
    );

    expect(html).toContain('New paid jobs are temporarily paused');
    expect(html).not.toContain('Get fixed quote');
  });

  it('renders transactional controls only when paid intake is open', () => {
    const html = renderToStaticMarkup(
      <IntakeGate admission={{ status: 'ready', data: { intakeEnabled: true } }}>
        <button type="button">Get fixed quote</button>
      </IntakeGate>,
    );

    expect(html).toContain('Get fixed quote');
    expect(html).not.toContain('New paid jobs are temporarily paused');
  });

  it('fails closed when authoritative admission state is unavailable', () => {
    const html = renderToStaticMarkup(
      <IntakeGate admission={{ status: 'error', error: 'Mizuki API returned 503' }}>
        <button type="button">Get fixed quote</button>
      </IntakeGate>,
    );

    expect(html).toContain('Issue submission is temporarily unavailable');
    expect(html).toContain('Quote and payment controls remain unavailable');
    expect(html).not.toContain('Get fixed quote');
  });
});
