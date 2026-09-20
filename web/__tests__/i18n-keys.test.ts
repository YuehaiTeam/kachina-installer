import { describe, expect, it } from 'vitest';

const locales = import.meta.glob('../../locales/*.tsv', {
  query: '?raw',
  import: 'default',
  eager: true,
}) as Record<string, string>;

const sources = import.meta.glob('../{screens,panels}/*.tsx', {
  query: '?raw',
  import: 'default',
  eager: true,
});

function keysOf(tsv: string): Set<string> {
  return new Set(
    tsv
      .split(/\r?\n/)
      .map((l) => l.split('\t')[0])
      .filter(Boolean),
  );
}

describe('locale coverage', () => {
  it('screens and panels t() keys exist in every locale file', () => {
    const used = new Set<string>();
    const re = /\bt\(\s*['"]([^'"]+)['"]/g;
    const re2 = /\bt\(\s*`([^`$]+)`/g;
    for (const text of Object.values(sources)) {
      for (const m of text.matchAll(re)) used.add(m[1]);
      for (const m of text.matchAll(re2)) used.add(m[1]);
    }
    expect(Object.keys(sources).length).toBeGreaterThan(0);
    expect(Object.keys(locales).length).toBeGreaterThan(1);
    for (const [file, tsv] of Object.entries(locales)) {
      const keys = keysOf(tsv);
      const missing = [...used].filter((k) => !k.endsWith('.') && !keys.has(k));
      expect(missing, `${file} missing locale keys: ${missing.join(', ')}`).toEqual([]);
    }
  });

  it('every locale file has the same key set as zh-CN', () => {
    const ref = Object.entries(locales).find(([f]) => f.endsWith('/zh-CN.tsv'));
    expect(ref).toBeDefined();
    const refKeys = keysOf(ref![1]);
    for (const [file, tsv] of Object.entries(locales)) {
      const keys = keysOf(tsv);
      const missing = [...refKeys].filter((k) => !keys.has(k));
      const extra = [...keys].filter((k) => !refKeys.has(k));
      expect(missing, `${file} missing: ${missing.join(', ')}`).toEqual([]);
      expect(extra, `${file} extra: ${extra.join(', ')}`).toEqual([]);
    }
  });
});
