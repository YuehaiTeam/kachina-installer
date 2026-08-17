import { invoke, listen } from './tauri';
import { sendInsight } from './api/ipc';
import {
  handleSessionPlugin,
  type SessionPluginRequest,
} from './plugins/bridge';
import { confirmDialog } from './ui';

export type SessionKind = 'install' | 'uninstall';

export type SessionInput = {
  install_path: string;
  source_uri: string;
  create_lnk: boolean;
  delete_user_data: boolean;
  mirrorc_cdk: string | null;
};

export type SessionResult = {
  already_latest: boolean;
  is_update: boolean;
  is_uninstall: boolean;
  cancelled: boolean;
};

export async function runSession(
  kind: SessionKind,
  input: SessionInput,
  insightBaseUrl: string,
  onReopenSource: () => void,
  onProgress: (event: {
    sub_step: number;
    percent: number;
    current: string;
  }) => void,
): Promise<SessionResult> {
  const unlistenProgress = await listen<{
    sub_step: number;
    percent: number;
    current: string;
  }>('session-progress', (e) => {
    onProgress(e.payload);
  });
  const unlistenPrompt = await listen<{
    id: string;
    kind: string;
    title: string;
    message: string;
  }>('session-prompt', async (e) => {
    const ok = await confirmDialog(e.payload.message, e.payload.title);
    await invoke('answer_session_prompt', {
      id: e.payload.id,
      accept: ok,
    });
  });
  const unlistenPlugin = await listen<SessionPluginRequest>(
    'session-plugin',
    async (e) => {
      await handleSessionPlugin(e.payload, (payload) =>
        invoke('answer_session_plugin', payload),
      );
    },
  );
  const unlistenInsight = await listen<{ event: string; data?: unknown }>(
    'session-insight',
    (e) => {
      sendInsight(insightBaseUrl, e.payload.event, e.payload.data);
    },
  );
  const unlistenReopen = await listen('session-reopen-source', () => {
    onReopenSource();
  });
  try {
    return await invoke<SessionResult>(
      kind === 'install' ? 'start_install' : 'start_uninstall',
      { input },
    );
  } finally {
    unlistenProgress();
    unlistenPrompt();
    unlistenPlugin();
    unlistenInsight();
    unlistenReopen();
  }
}
