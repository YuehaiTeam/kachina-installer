import { describe, expect, it } from 'vitest';
import zhCn from '../../locales/zh-CN.tsv?raw';

const sources = import.meta.glob('../{screens,panels}/*.tsx', {
  query: '?raw',
  import: 'default',
  eager: true,
});

describe('locale coverage', () => {
  it('screens and panels t() keys exist in zh-CN.tsv', () => {
    const keys = new Set(
      zhCn
        .split(/\r?\n/)
        .map((l) => l.split('\t')[0])
        .filter(Boolean),
    );
    const used = new Set<string>();
    const re = /\bt\(\s*['"]([^'"]+)['"]/g;
    const re2 = /\bt\(\s*`([^`$]+)`/g;
    for (const text of Object.values(sources)) {
      for (const m of text.matchAll(re)) used.add(m[1]);
      for (const m of text.matchAll(re2)) used.add(m[1]);
    }
    expect(Object.keys(sources).length).toBeGreaterThan(0);
    const missing = [...used].filter((k) => !k.endsWith('.') && !keys.has(k));
    expect(missing, `missing locale keys: ${missing.join(', ')}`).toEqual([]);
  });
});
