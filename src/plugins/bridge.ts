import { pluginManager } from './index';
import { registerAllPlugins } from './registry';

registerAllPlugins();

export type SessionPluginRequest = {
  id: string;
  method: string;
  name: string;
  url: string;
  range?: string | null;
  diffchunks?: string[] | null;
  insights?: unknown;
};

export async function handleSessionPlugin(
  req: SessionPluginRequest,
  answer: (payload: {
    id: string;
    ok: boolean;
    data?: unknown;
    error?: string;
    unimplemented?: boolean;
  }) => Promise<unknown>,
) {
  try {
    const plugin = pluginManager.findPlugin(`plugin-${req.name}+${req.url}`);
    if (!plugin) {
      await answer({
        id: req.id,
        ok: false,
        error: `Plugin "${req.name}" not found`,
      });
      return;
    }
    switch (req.method) {
      case 'getMetadata': {
        if (!plugin.getMetadata) {
          await answer({ id: req.id, ok: true, unimplemented: true });
          return;
        }
        const data = await plugin.getMetadata(req.url);
        await answer({ id: req.id, ok: true, data });
        return;
      }
      case 'createSession': {
        if (!plugin.createSession) {
          await answer({ id: req.id, ok: true, unimplemented: true });
          return;
        }
        const data = await plugin.createSession(req.url, req.diffchunks || []);
        await answer({ id: req.id, ok: true, data });
        return;
      }
      case 'getChunkUrl': {
        const data = await plugin.getChunkUrl(req.url, req.range || '');
        await answer({ id: req.id, ok: true, data });
        return;
      }
      case 'endSession': {
        if (!plugin.endSession) {
          await answer({ id: req.id, ok: true, unimplemented: true });
          return;
        }
        await plugin.endSession(req.url, req.insights);
        await answer({ id: req.id, ok: true, data: null });
        return;
      }
      default:
        await answer({
          id: req.id,
          ok: false,
          error: `Unknown plugin method: ${req.method}`,
        });
    }
  } catch (e) {
    const message = e instanceof Error ? e.message || e.toString() : String(e);
    await answer({
      id: req.id,
      ok: false,
      error: message,
    });
  }
}
