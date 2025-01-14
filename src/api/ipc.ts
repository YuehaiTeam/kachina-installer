import type { Event } from '@tauri-apps/api/event';
import { invoke, listen } from '../tauri';
import { v4 as uuid } from 'uuid';

export async function ipc<T extends { type: string }, E, Z>(
  arg: T,
  elevate: boolean,
  onProgress: (payload: Event<Z>) => void,
): Promise<E> {
  let id = uuid();
  let unlisten = await listen<Z>(id, onProgress);
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
