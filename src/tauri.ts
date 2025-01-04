import type { invoke as invokeType } from '@tauri-apps/api/core';
import type { listen as listenType } from '@tauri-apps/api/event';
import type { sep as sepType } from '@tauri-apps/api/path';
import type { getCurrentWindow as getCurrentWindowType } from '@tauri-apps/api/window';
const TAURI = (window as any).__TAURI__;
export const invoke = TAURI.core.invoke as typeof invokeType;
export const listen = TAURI.event.listen as typeof listenType;
export const sep = TAURI.path.sep as typeof sepType;
export const getCurrentWindow = TAURI.window
  .getCurrentWindow as typeof getCurrentWindowType;
