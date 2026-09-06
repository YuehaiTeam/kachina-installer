import { invoke, listen } from './host';
import {
  handleSessionPlugin,
  type SessionPluginRequest,
} from './plugins/bridge';

/**
 * Register the `session-plugin` request listeners. Both the normal installer
 * page and the hidden `?pluginHost=1` window need this: the session emits
 * plugin requests to whichever window hosts its UI, and an unregistered
 * window lets the request time out after 60s as PLUGIN_FAILED.
 */
export async function registerPluginBridge() {
  await listen<SessionPluginRequest>('session-plugin', async (payload) => {
    await handleSessionPlugin(payload, (body) =>
      invoke('answer_session_plugin', body),
    );
  });
}

/** Hidden plugin-window entry (`?pluginHost=1`), used by silent runs. */
export async function startPluginHost() {
  await registerPluginBridge();
  await invoke('plugin_host_ready');
}
