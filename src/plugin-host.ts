import { invoke, listen } from './host';
import {
  handleSessionPlugin,
  type SessionPluginRequest,
} from './plugins/bridge';

export async function startPluginHost() {
  await listen<SessionPluginRequest>('session-plugin', async (payload) => {
    await handleSessionPlugin(payload, (body) =>
      invoke('answer_session_plugin', body),
    );
  });
  await invoke('plugin_host_ready');
}
