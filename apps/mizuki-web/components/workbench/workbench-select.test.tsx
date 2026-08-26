import { readdirSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { renderToStaticMarkup } from 'react-dom/server';
import { describe, expect, it } from 'vitest';
import { WorkbenchSelect } from './workbench-select';

describe('WorkbenchSelect', () => {
  it('renders a styled listbox trigger instead of a browser select', () => {
    const html = renderToStaticMarkup(
      <WorkbenchSelect
        id="organization"
        labelledBy="organization-label"
        value="open-covenant"
        placeholder="Choose an organization"
        options={[
          { value: 'mizuki0x', label: 'mizuki0x' },
          { value: 'open-covenant', label: 'open-covenant' },
        ]}
        onChange={() => undefined}
      />,
    );

    expect(html).toContain('class="workbench-select-trigger"');
    expect(html).toContain('aria-haspopup="listbox"');
    expect(html).toContain('aria-expanded="false"');
    expect(html).toContain('open-covenant');
    expect(html).not.toContain('<select');
  });

  it('keeps browser-native selects out of the web interface', () => {
    const files = [
      ...sourceFiles(join(process.cwd(), 'app')),
      ...sourceFiles(join(process.cwd(), 'components')),
    ];
    const offenders = files.filter((file) => /<select(?:\s|>)/i.test(readFileSync(file, 'utf8')));

    expect(offenders).toEqual([]);
  });
});

function sourceFiles(directory: string): string[] {
  return readdirSync(directory, { withFileTypes: true }).flatMap((entry) => {
    const path = join(directory, entry.name);
    if (entry.isDirectory()) return sourceFiles(path);
    return entry.name.endsWith('.tsx') && !entry.name.endsWith('.test.tsx') ? [path] : [];
  });
}
