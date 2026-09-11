import { describe, expect, it } from 'vitest';
import { startPluginHost } from '../plugin-host';
import { posted, resetHost } from './setup';

describe('plugin host', () => {
  it('does not fetch i18n and calls plugin_host_ready', async () => {
    resetHost();
    const fetchMock = globalThis.fetch as unknown as { mock?: { calls: unknown[] } };
    const callsBefore = (fetchMock as { mock?: { calls: unknown[] } }).mock?.calls.length ?? 0;
    await startPluginHost();
    expect(posted.some((m) => (m as { cmd?: string }).cmd === 'plugin_host_ready')).toBe(true);
    const callsAfter = (fetchMock as { mock?: { calls: unknown[] } }).mock?.calls.length ?? 0;
    expect(callsAfter).toBe(callsBefore);
  });
});
