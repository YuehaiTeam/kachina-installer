import type { Event } from '@tauri-apps/api/event';
import { invoke, listen } from '../tauri';
import { v4 as uuid } from 'uuid';

export async function ipc<T extends { type: string }, E, Z>(
  arg: T,
  elevate: boolean,
  onProgress?: (payload: Event<Z>) => void,
): Promise<E> {
  let id = uuid();
  let unlisten = await listen<Z>(id, onProgress || (() => {}));
  let res: { Ok: E } | { Err: string } | E;
  try {
    res = await invoke('managed_operation', {
      ipc: arg,
      id,
      elevate,
    });
    unlisten();
  } catch (e) {
    unlisten();
    throw e;
  }
  if (
    !res ||
    typeof res !== 'object' ||
    (!('Ok' in (res as {})) && !('Err' in (res as {})))
  ) {
    return res as E;
  }
  if (res && typeof res === 'object' && 'Err' in res) {
    throw new Error(res.Err);
  }
  return (res as { Ok: E }).Ok;
}

export async function ipPrepare(elevate = false) {
  await invoke('managed_operation', {
    ipc: { type: 'Ping' },
    id: uuid(),
    elevate,
  });
}

interface IpcWriteRegistry {
  type: 'WriteRegistry';
  reg_name: string;
  name: string;
  version: string;
  exe: string;
  source: string;
  uninstaller: string;
  metadata: string;
  size: number;
  publisher: string;
}
interface IpcCreateLnk {
  type: 'CreateLnk';
  target: string;
  lnk: string;
}
interface IpcCreateUninstaller {
  type: 'CreateUninstaller';
  source: string;
  uninstaller_name: string;
  updater_name: string;
}
interface IpcRunUninstall {
  type: 'RunUninstall';
  source: string;
  files: string[];
  user_data_path: string[];
  extra_uninstall_path: string[];
  reg_name: string;
  uninstall_name: string;
}

interface IpcKillProcess {
  type: 'KillProcess';
  pid: number;
}

interface IpcFindProcessByName {
  type: 'FindProcessByName';
  name: string;
}

interface IpcRmList {
  type: 'RmList';
  list: string[];
}

export async function ipcCreateLnk(
  target: string,
  lnk: string,
  elevate = false,
) {
  return ipc<IpcCreateLnk, void, void>(
    { type: 'CreateLnk', target, lnk },
    elevate,
  );
}

export async function ipcCreateUninstaller(
  source: string,
  uninstaller_name: string,
  updater_name: string,
  elevate = false,
) {
  return ipc<IpcCreateUninstaller, void, void>(
    { type: 'CreateUninstaller', source, uninstaller_name, updater_name },
    elevate,
  );
}

export async function ipcRunUninstall(
  args: Omit<IpcRunUninstall, 'type'>,
  elevate = false,
) {
  return ipc<IpcRunUninstall, void, void>(
    { type: 'RunUninstall', ...args },
    elevate,
  );
}

export async function ipcWriteRegistry(
  args: Omit<IpcWriteRegistry, 'type'>,
  elevate = false,
) {
  return ipc<IpcWriteRegistry, void, void>(
    { type: 'WriteRegistry', ...args },
    elevate,
  );
}

export async function ipcKillProcess(pid: number, elevate = false) {
  return ipc<IpcKillProcess, void, void>({ type: 'KillProcess', pid }, elevate);
}

export async function ipcFindProcessByName(name: string, elevate = false) {
  return ipc<IpcFindProcessByName, [number, string][], void>(
    { type: 'FindProcessByName', name },
    elevate,
  );
}

export async function ipcRmList(list: string[], elevate = false) {
  return ipc<IpcRmList, void, void>({ type: 'RmList', list }, elevate);
}

export function log(...args: any[]) {
  console.log(...args);
  const logstr = args.reduce((acc, arg) => {
    if (typeof arg === 'string') {
      return acc + ' ' + arg;
    }
    return acc + ' ' + JSON.stringify(arg);
  });
  const timestr = new Date().toISOString();
  invoke('log', {
    data: `${timestr}: ${logstr}`,
  });
}

export async function sendInsight(url: string, event?: string, data?: unknown) {
  const res = await fetch('https://77.cocogoat.cn/ev', {
    headers: {
      'content-type': 'application/json',
      ...(localStorage.evCache ? { Authorization: localStorage.evCache } : {}),
    },
    body: JSON.stringify({
      type: 'event',
      payload: {
        website: '16d32274-7313-4db6-80d3-340ce9db7689',
        url: encodeURI(url),
        name: event,
        data,
        screen: `${window.screen.width}x${window.screen.height}`,
        language: navigator.language,
      },
    }),
    method: 'POST',
    mode: 'cors',
    credentials: 'omit',
  });
  const text = await res.text();
  return (localStorage.evCache = text || '');
}
