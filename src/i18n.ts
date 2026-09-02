import { state } from './state';

let tableText: string | null = null;
let parsedLang: string | null = null;
let rows = new Map<string, string[]>();
let langs: string[] = [];
let col = 0;

function parseTsv(text: string) {
  const lines = text.split(/\r?\n/).filter((l) => l.length > 0 && !l.startsWith('#'));
  rows = new Map();
  langs = [];
  if (lines.length === 0) return;
  const first = lines[0].split('\t');
  let start = 0;
  if (first[0] === 'KEY') {
    langs = first.slice(1);
    start = 1;
  } else {
    langs = [''];
  }
  for (let i = start; i < lines.length; i++) {
    const cells = lines[i].split('\t');
    if (!cells[0]) continue;
    rows.set(cells[0], cells.slice(1));
  }
}

function pickColumn(lang: string) {
  const idx = langs.findIndex((l) => l === lang);
  col = idx >= 0 ? idx : 0;
  parsedLang = lang;
}

let loaded: Promise<void> | null = null;

function load(): Promise<void> {
  if (!loaded) {
    loaded = fetch('/i18n.tsv')
      .then((res) => (res.ok ? res.text() : ''))
      .then((text) => {
        tableText = text;
        if (text) parseTsv(text);
      })
      .catch(() => {
        tableText = '';
      });
  }
  return loaded;
}

export function t(key: string, params?: Record<string, string>): string {
  const lang = state.value?.project.lang ?? '';
  if (tableText && lang !== parsedLang) {
    pickColumn(lang);
  }
  const vals = rows.get(key);
  let text = key;
  if (vals && vals.length) {
    const chosen =
      (vals[col] && vals[col].length > 0 ? vals[col] : undefined) ??
      vals.find((v) => v.length > 0);
    if (chosen) text = chosen;
  }
  if (params) {
    for (const [name, value] of Object.entries(params)) {
      text = text.replaceAll(`{${name}}`, value);
    }
  }
  return text;
}

export function ready(): Promise<void> {
  return load();
}

export function formatSize(size: number): string {
  if (size >= 1024 * 1024) {
    return `${(size / 1024 / 1024).toFixed(1)}MB`;
  }
  if (size >= 1024) {
    return `${(size / 1024).toFixed(0)}KB`;
  }
  return `${size}B`;
}
