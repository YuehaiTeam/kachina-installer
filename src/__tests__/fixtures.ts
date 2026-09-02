import type { UiState } from '../state';

const project = {
  window_title: 'Test',
  title: 'Demo App',
  description: 'A demo',
  borderless: false,
  lang: 'zh-CN',
};

const options = {
  install_path: 'C:\\Games\\Demo',
  source_uri: 'https://example.com/app.json',
  create_lnk: true,
  delete_user_data: false,
  mirrorc_cdk: null as string | null,
};

const sources = [
  {
    id: 'http',
    name: 'HTTP',
    uri: 'https://example.com/app.json',
    icon: null as string | null,
    requires_webview: false,
  },
  {
    id: 'mirrorc',
    name: 'Mirror',
    uri: 'mirrorc://rid',
    icon: '<svg></svg>',
    requires_webview: false,
  },
];

const path = { writable: 'writable' as const, exists: false, upgrade: false };

export function ready(over: Partial<UiState> = {}): UiState {
  return {
    phase: { kind: 'ready' },
    mode: 'install',
    project,
    options,
    sources,
    path,
    needs_elevate: false,
    cdk: { kind: 'idle' },
    theme: 'none',
    pending: null,
    ...over,
  };
}

export function running(): UiState {
  return ready({
    phase: {
      kind: 'running',
      sub_step: 2,
      percent: 40,
      stage: 'download',
      subject: 'app.exe',
      done: 1024,
      total: 2048,
    },
  });
}

export function doneInstall(): UiState {
  return ready({
    phase: {
      kind: 'done',
      already_latest: false,
      is_update: false,
      is_uninstall: false,
      cancelled: false,
    },
  });
}

export function doneLatest(): UiState {
  return ready({
    phase: {
      kind: 'done',
      already_latest: true,
      is_update: true,
      is_uninstall: false,
      cancelled: false,
    },
  });
}

export function doneUpdate(): UiState {
  return ready({
    phase: {
      kind: 'done',
      already_latest: false,
      is_update: true,
      is_uninstall: false,
      cancelled: false,
    },
  });
}

export function doneUninstall(): UiState {
  return ready({
    mode: 'uninstall',
    phase: {
      kind: 'done',
      already_latest: false,
      is_update: false,
      is_uninstall: true,
      cancelled: false,
    },
  });
}

export function failed(code = 'PKG_BROKEN'): UiState {
  return ready({
    phase: { kind: 'failed', code, detail: 'boom', subject: null },
  });
}

export function pendingProcess(): UiState {
  return ready({
    pending: {
      id: 'p1',
      kind: 'process_running',
      items: ['Demo.exe'],
      params: {},
    },
  });
}

export function pendingOccupied(): UiState {
  return ready({
    pending: {
      id: 'p2',
      kind: 'occupied_files',
      items: ['a.dll'],
      params: {},
    },
  });
}

export function pendingVersion(): UiState {
  return ready({
    pending: {
      id: 'p3',
      kind: 'version_mismatch',
      items: [],
      params: { local: '1.0', remote: '2.0' },
    },
  });
}

export function readyUpdate(): UiState {
  return ready({ mode: 'update', path: { ...path, exists: true, upgrade: true } });
}

export function readyUninstall(): UiState {
  return ready({ mode: 'uninstall' });
}
