import { invoke, listen } from './tauri';
import {
  handleSessionPlugin,
  type SessionPluginRequest,
} from './plugins/bridge';

export async function startPluginHost() {
  await listen<SessionPluginRequest>('session-plugin', async (e) => {
    await handleSessionPlugin(e.payload, (payload) =>
      invoke('answer_session_plugin', payload),
    );
  });
  await invoke('plugin_host_ready');
}
