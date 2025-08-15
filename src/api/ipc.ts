import type { Event } from '@tauri-apps/api/event';
import { invoke, listen } from '../tauri';
import { v4 as uuid } from 'uuid';
import { addNetworkInsight } from '../networkInsights';
import {
  InsightItem,
  InstallResult,
  InvokeDeepReaddirWithMetadataRes,
  InvokeGetDfsMetadataRes,
  TAError,
  TAErrorData,
} from '../types';

export async function ipc<T extends { type: string }, E, Z>(
  arg: T,
  elevate: boolean,
  onProgress?: (payload: Event<Z>) => void,
): Promise<E> {
  const id = uuid();
  const unlisten = await listen<Z>(id, onProgress || (() => {}));
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
    (!('Ok' in (res as object)) && !('Err' in (res as object)))
  ) {
    return res as E;
  }
  if (res && typeof res === 'object' && 'Err' in res) {
    const errorData = res.Err;
    if (typeof errorData === 'object' && 'message' in errorData) {
      const taError = TAError.fromErrorData(errorData as TAErrorData);
      // 始终收集错误场景的网络统计
      if (taError.insight) {
        addNetworkInsight(taError.insight);
      }
      throw taError;
    } else {
      throw new TAError(errorData as string);
    }
  }

  const result = (res as { Ok: E }).Ok;

  // 始终收集成功场景的网络统计
  if (result && typeof result === 'object' && 'insight' in result) {
    // Handle both single InstallResult and multi-install {results, insight} format
    if ('results' in result) {
      // Multi-install format: {results: TAResult[], insight: InsightItem}
      const multiResult = result as {
        results: unknown[];
        insight: InsightItem;
      };
      if (multiResult.insight) {
        addNetworkInsight(multiResult.insight);
      }
    } else {
      // Single InstallResult format
      const installResult = result as InstallResult;
      if (installResult.insight) {
        addNetworkInsight(installResult.insight);
      }
    }
  }

  return result;
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

interface IpcInstallRuntime {
  type: 'InstallRuntime';
  tag: string;
  offset?: number;
  size?: number;
}

interface IpcCheckLocalFiles {
  type: 'CheckLocalFiles';
  source: string;
  hash_algorithm: string;
  file_list: string[];
}

interface RunMirrorcDownload {
  type: 'RunMirrorcDownload';
  url: string;
  zip_path: string;
}

interface RunMirrorcInstall {
  type: 'RunMirrorcInstall';
  zip_path: string;
  target_path: string;
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

export async function ipcInstallRuntime(
  tag: string,
  offset: number | undefined,
  size: number | undefined,
  cb: (p: Event<[number, number]>) => void,
  elevate = false,
) {
  return ipc<IpcInstallRuntime, void, [number, number]>(
    { type: 'InstallRuntime', tag, offset, size },
    elevate,
    cb,
  );
}

export async function ipcCheckLocalFiles(
  args: Omit<IpcCheckLocalFiles, 'type'>,
  cb: (p: Event<[number, number]>) => void,
  elevate = false,
) {
  return ipc<
    IpcCheckLocalFiles,
    InvokeDeepReaddirWithMetadataRes,
    [number, number]
  >({ type: 'CheckLocalFiles', ...args }, elevate, cb);
}

type MirrorcStatus =
  | {
      type: 'delete';
      file: 'string';
    }
  | {
      type: 'download';
      downloaded: number;
      total: number;
    }
  | {
      type: 'extract';
      file: string;
      count: number;
      total: number;
    };

interface MirrorcChangeset {
  added?: string[];
  deleted?: string[];
  modified?: string[];
}
export interface MirrorcUpdate {
  code: number;
  data?: {
    /**
     * 更新频道，stable | beta | alpha
     */
    channel: string;
    /**
     * 自定义数据
     */
    custom_data: string;
    /**
     * 发版日志
     */
    release_note: string;
    /**
     * sha256
     */
    sha256?: string;
    /**
     * 更新包类型，incremental | full
     */
    update_type?: string;
    /**
     * 下载地址
     */
    url?: string;
    /**
     * 资源版本名称
     */
    version_name: string;
    /**
     * 资源版本号仅内部使用
     */
    version_number: number;
  };
  msg: string;
}

export async function ipcRunMirrorcDownload(
  url: string,
  zip_path: string,
  notify: (value: Event<MirrorcStatus>) => void,
  elevate = false,
) {
  return ipc<RunMirrorcDownload, void, MirrorcStatus>(
    { type: 'RunMirrorcDownload', url, zip_path },
    elevate,
    notify,
  );
}

export async function ipcRunMirrorcInstall(
  zip_path: string,
  target_path: string,
  notify: (value: Event<MirrorcStatus>) => void,
  elevate = false,
) {
  return ipc<
    RunMirrorcInstall,
    [InvokeGetDfsMetadataRes, MirrorcChangeset],
    MirrorcStatus
  >({ type: 'RunMirrorcInstall', zip_path, target_path }, elevate, notify);
}

export function log(...args: unknown[]) {
  console.log(...args);
  const logstr = args.reduce((acc, arg) => {
    if (typeof arg === 'string') {
      return acc + ' ' + arg;
    }
    return (
      acc +
      ' ' +
      (arg instanceof Error || arg instanceof TAError
        ? arg.toString()
        : JSON.stringify(arg))
    );
  }, '');
  invoke('log', { data: logstr });
}
export function warn(...args: unknown[]) {
  console.warn(...args);
  const logstr = args.reduce((acc, arg) => {
    if (typeof arg === 'string') {
      return acc + ' ' + arg;
    }
    return (
      acc +
      ' ' +
      (arg instanceof Error || arg instanceof TAError
        ? arg.toString()
        : JSON.stringify(arg))
    );
  }, '');
  invoke('warn', { data: logstr });
}
export function error(...args: unknown[]): string {
  console.error(...args);
  const logstr = args.reduce((acc, arg) => {
    if (typeof arg === 'string') {
      return acc + ' ' + arg;
    }
    return (
      acc +
      ' ' +
      (arg instanceof Error || arg instanceof TAError
        ? arg.toString()
        : JSON.stringify(arg))
    );
  }, '');
  invoke('error', { data: logstr });
  return logstr as string;
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
