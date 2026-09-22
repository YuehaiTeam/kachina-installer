import { describe, expect, it } from 'vitest';
import { sourceVisible } from '../panels/SourcePanel';
import type { SourceItem } from '../state';

function src(over: Partial<SourceItem> = {}): SourceItem {
  return {
    id: 'http',
    name: 'HTTP',
    uri: 'https://example.com/a.json',
    icon: null,
    requires_webview: false,
    hidden: false,
    ...over,
  };
}

describe('sourceVisible', () => {
  it('lists ordinary sources', () => {
    expect(sourceVisible(src(), 'other', false)).toBe(true);
  });

  it('hides extra hidden sources until revealed', () => {
    const hidden = src({ id: 'stub', hidden: true, uri: 'plugin-stub+x' });
    expect(sourceVisible(hidden, 'https://example.com/a.json', false)).toBe(false);
    expect(sourceVisible(hidden, 'https://example.com/a.json', true)).toBe(true);
  });

  it('keeps the current hidden source listed', () => {
    const hidden = src({ hidden: true, uri: 'plugin-stub+x' });
    expect(sourceVisible(hidden, 'plugin-stub+x', false)).toBe(true);
  });
});
