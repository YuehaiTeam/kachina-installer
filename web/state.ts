import { signal } from '@preact/signals';
import { invoke, listen } from './host';

export type Mode = 'install' | 'update' | 'uninstall';
export type Theme = 'none' | 'image' | 'css' | 'html';
export type PathWritable = 'writable' | 'unwritable' | 'private';

export type Coded = {
  code: string;
  detail: string | null;
  subject: string | null;
  sid: string | null;
  event_id: string | null;
};

/** Arguments of the `error_dialog` command: the `Coded` fields the dialog renders. */
export function errorDialogArgs(coded: Coded) {
  return {
    code: coded.code,
    detail: coded.detail,
    subject: coded.subject,
    sid: coded.sid,
    event_id: coded.event_id,
  };
}

export type ProgressStage =
  | 'prepare' | 'fetch_metadata' | 'scan_files'
  | 'prepare_download' | 'create_download_session' | 'process_files'
  | 'download_archive' | 'verify_archive' | 'extract_archive' | 'commit'
  | 'download_runtime' | 'install_runtime' | 'create_shortcuts'
  | 'write_registry' | 'finalize' | 'uninstall_scan' | 'uninstall_delete';

export type CancelState = 'available' | 'requested' | 'unavailable';
export type ProgressUnit = 'bytes' | 'files' | 'operations';
export type FileAction = 'download' | 'extract' | 'patch' | 'verify' | 'flush' | 'retry';

export type ProgressCounter = {
  unit: ProgressUnit;
  done: number;
  total: number | null;
};

export type ByteProgress = {
  done: number;
  total: number | null;
};

export type FileProgress = {
  id: number;
  name: string;
  action: FileAction;
  bytes: ByteProgress | null;
};

export type Progress = {
  step: number | null;
  stage: ProgressStage;
  subject: string | null;
  percent: number | null;
  cancel: CancelState;
  summary: ProgressCounter | null;
  processing_bps: number | null;
  network_bps: number | null;
  network_pending: boolean;
  files: FileProgress[];
};

export type RunningPhase = { kind: 'running' } & Progress;

export type SessionResult = {
  already_latest: boolean;
  is_update: boolean;
  is_uninstall: boolean;
  cancelled: boolean;
};

export type Phase =
  | { kind: 'ready' }
  | ({ kind: 'running' } & Progress)
  | ({ kind: 'done' } & SessionResult)
  | ({ kind: 'failed' } & Coded);

export type ProjectView = {
  window_title: string;
  title: string;
  description: string;
  borderless: boolean;
  lang: string;
};

export type Options = {
  install_path: string;
  source_uri: string;
  create_lnk: boolean;
  delete_user_data: boolean;
  mirrorc_cdk: string | null;
};

export type SourceItem = {
  id: string;
  name: string;
  uri: string;
  icon: string | null;
  requires_webview: boolean;
};

export type PathState = {
  writable: PathWritable;
  exists: boolean;
  upgrade: boolean;
};

export type CdkStatus =
  | { kind: 'idle' }
  | { kind: 'checking' }
  | { kind: 'ok' }
  | ({ kind: 'invalid' } & Coded);

export type Prompt = {
  id: string;
  kind: string;
  items: string[];
  params: Record<string, string>;
};

export type UiState = {
  phase: Phase;
  mode: Mode;
  /** 离线整包（带 embedded_index）：不提供安装源选择入口。 */
  offline: boolean;
  project: ProjectView;
  options: Options;
  sources: SourceItem[];
  path: PathState;
  needs_elevate: boolean;
  cdk: CdkStatus;
  theme: Theme;
  pending: Prompt | null;
};

export type Intent =
  | { kind: 'set_path'; path: string }
  | { kind: 'set_source'; uri: string }
  | { kind: 'set_create_lnk'; value: boolean }
  | { kind: 'set_delete_user_data'; value: boolean }
  | { kind: 'set_cdk'; cdk: string }
  | { kind: 'start' }
  | { kind: 'cancel' }
  | { kind: 'answer'; id: string; ok: boolean }
  | { kind: 'dismiss' }
  | { kind: 'launch' }
  | { kind: 'advanced' }
  | { kind: 'close' };

export const state = signal<UiState | null>(null);

function applyUiState(payload: UiState) {
  state.value = payload;
}

void listen<UiState>('ui-state', (payload) => {
  applyUiState(payload);
});

export function pullUiState(): Promise<void> {
  return invoke<UiState | null>('get_ui_state')
    .then((s) => {
      if (s && s.phase) {
        applyUiState(s);
      }
    })
    .catch(() => {});
}

void pullUiState();

export function intent(payload: Intent): Promise<unknown> {
  return invoke('intent', payload);
}

export function isRunning(phase: Phase): phase is { kind: 'running' } & Progress {
  return phase.kind === 'running';
}

export function isDone(phase: Phase): phase is { kind: 'done' } & SessionResult {
  return phase.kind === 'done';
}

export function isFailed(phase: Phase): phase is { kind: 'failed' } & Coded {
  return phase.kind === 'failed';
}

export function isCdkInvalid(
  cdk: CdkStatus,
): cdk is { kind: 'invalid' } & Coded {
  return cdk.kind === 'invalid';
}
