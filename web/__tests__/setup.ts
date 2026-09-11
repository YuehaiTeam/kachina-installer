import { afterEach } from 'vitest';
import { cleanup } from '@testing-library/preact';
import { vi } from 'vitest';
import zhCn from '../../locales/zh-CN.tsv?raw';

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

// The renderer is fed the real single-language table (same bytes build.rs merges
// into the `i18n.tsv` asset) so tests cannot drift from the shipped copy.
vi.stubGlobal(
  'fetch',
  vi.fn(async (url: string) => {
    if (String(url).includes('i18n.tsv')) {
      return { ok: true, text: async () => zhCn };
    }
    return { ok: false, text: async () => '' };
  }),
);

afterEach(() => {
  cleanup();
});
