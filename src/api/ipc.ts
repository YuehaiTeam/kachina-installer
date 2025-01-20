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
  let res: E;
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
  return res;
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
