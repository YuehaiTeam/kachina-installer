import { getCurrentWindow, invoke } from './tauri';
import type { InstallerConfig, ProjectConfig } from './types';

export async function dialogError(
  message: string,
  title = '出错了',
  closeOnSilent = false,
): Promise<void> {
  await invoke('error_dialog', {
    message: message.replace(new RegExp(location.origin, 'g'), ''),
    title,
  });
  if (closeOnSilent) {
    getCurrentWindow().close();
  }
}

export async function confirmDialog(
  message: string,
  title = '提示',
): Promise<boolean> {
  return await invoke<boolean>('confirm_dialog', { message, title });
}

export function insightBase(
  installer: InstallerConfig,
  project: ProjectConfig,
): string {
  const qs = new URLSearchParams();
  if (installer.args.non_interactive) {
    qs.set('non_interactive', '1');
  }
  if (installer.args.silent) {
    qs.set('silent', '1');
  }
  if (installer.args.uninstall) {
    qs.set('uninstall', '1');
  }
  if (installer.args.online) {
    qs.set('online', '1');
  }
  if ((installer.embedded_index?.length || 0) > 0) {
    qs.set('pack', '1');
  }
  return `/${project.appName}?${qs.toString()}`;
}

export function uacNeeded(
  state: 'Unwritable' | 'Writable' | 'Private',
  uacStrategy: ProjectConfig['uacStrategy'],
): boolean {
  switch (uacStrategy) {
    case 'force':
      return true;
    case 'prefer-admin':
      return state !== 'Private';
    case 'prefer-user':
      return state === 'Unwritable';
    default:
      return false;
  }
}

export function stringifyError(e: unknown): string {
  if (e instanceof Error) {
    return e.message || e.toString();
  }
  return typeof e === 'string' ? e : JSON.stringify(e);
}

export function stringifyErrorLog(e: unknown): string {
  if (e instanceof Error) {
    return e.stack || e.toString();
  }
  return stringifyError(e);
}
