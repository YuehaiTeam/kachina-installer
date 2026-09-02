import fs from 'node:fs';
import path from 'node:path';
import { describe, expect, it } from 'vitest';

describe('locale coverage', () => {
  it('screens and panels t() keys exist in zh-CN.tsv', () => {
    const root = path.resolve(__dirname, '../..');
    const tsv = fs.readFileSync(path.join(root, 'locales/zh-CN.tsv'), 'utf8');
    const keys = new Set(
      tsv
        .split(/\r?\n/)
        .map((l) => l.split('\t')[0])
        .filter(Boolean),
    );
    const dirs = ['screens', 'panels'].map((d) => path.join(root, 'src', d));
    const used = new Set<string>();
    const re = /\bt\(\s*['"]([^'"]+)['"]/g;
    const re2 = /\bt\(\s*`([^`$]+)`/g;
    for (const dir of dirs) {
      for (const file of fs.readdirSync(dir)) {
        const text = fs.readFileSync(path.join(dir, file), 'utf8');
        for (const m of text.matchAll(re)) used.add(m[1]);
        for (const m of text.matchAll(re2)) used.add(m[1]);
      }
    }
    const missing = [...used].filter((k) => !k.endsWith('.') && !keys.has(k));
    expect(missing, `missing locale keys: ${missing.join(', ')}`).toEqual([]);
  });
});
