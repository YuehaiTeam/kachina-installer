import { afterEach } from 'vitest';
import { cleanup } from '@testing-library/preact';
import { vi } from 'vitest';
import zhCn from '../../locales/zh-CN.tsv?raw';
import enUs from '../../locales/en-US.tsv?raw';

type Handler = (ev: { data: unknown }) => void;

const listeners: Handler[] = [];
export const posted: unknown[] = [];
const pending = new Map<number, { ok: boolean; data?: unknown; error?: unknown }>();

export function resetHost() {
  posted.length = 0;
  pending.clear();
}

export function emitEvent(event: string, payload: unknown) {
  const msg = { kind: 'event', event, payload };
  for (const h of [...listeners]) {
    h({ data: msg });
  }
}

export function replyTo(id: number, ok: boolean, data?: unknown) {
  const msg = { kind: 'reply', id, ok, data, error: ok ? undefined : data };
  for (const h of [...listeners]) {
    h({ data: msg });
  }
}

(globalThis as unknown as { chrome: unknown }).chrome = {
  webview: {
    postMessage(msg: { id: number; kind: string; cmd: string; args: unknown }) {
      posted.push(msg);
      queueMicrotask(() => {
        if (msg.kind !== 'invoke') return;
        if (msg.cmd === 'window_show' || msg.cmd === 'plugin_host_ready') {
          replyTo(msg.id, true, null);
          return;
        }
        replyTo(msg.id, true, msg.cmd === 'pick_path' ? 'C:\\picked' : null);
      });
    },
    addEventListener(_type: string, handler: Handler) {
      listeners.push(handler);
    },
  },
};

// The renderer is fed the real locale files merged into the same wide table
// build.rs produces for the `i18n.tsv` asset, so tests cannot drift from the
// shipped copy and column selection by `project.lang` is exercised.
function mergeLocales(files: Record<string, string>): string {
  const langs = Object.keys(files).sort();
  const maps = langs.map((lang) => {
    const map = new Map<string, string>();
    for (const line of files[lang].split(/\r?\n/)) {
      if (!line || line.startsWith('#')) continue;
      const tab = line.indexOf('\t');
      const key = tab < 0 ? line : line.slice(0, tab);
      if (!key || key === 'KEY') continue;
      map.set(key, tab < 0 ? '' : line.slice(tab + 1));
    }
    return map;
  });
  const keys = [...new Set(maps.flatMap((m) => [...m.keys()]))].sort();
  const lines = ['KEY\t' + langs.join('\t')];
  for (const key of keys) {
    lines.push(key + '\t' + maps.map((m) => m.get(key) ?? '').join('\t'));
  }
  return lines.join('\n') + '\n';
}

export const i18nTable = mergeLocales({ 'zh-CN': zhCn, 'en-US': enUs });

vi.stubGlobal(
  'fetch',
  vi.fn(async (url: string) => {
    if (String(url).includes('i18n.tsv')) {
      return { ok: true, text: async () => i18nTable };
    }
    return { ok: false, text: async () => '' };
  }),
);

afterEach(() => {
  cleanup();
});
