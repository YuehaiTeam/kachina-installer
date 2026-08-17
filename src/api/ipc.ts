import { invoke } from '../tauri';
import { TAError } from '../types';

export interface MirrorcUpdate {
  code: number;
  data?: {
    channel: string;
    custom_data: string;
    release_note: string;
    sha256?: string;
    update_type?: string;
    url?: string;
    version_name: string;
    version_number: number;
  };
  msg: string;
}

function formatLog(args: unknown[]): string {
  return args.reduce((acc: string, arg) => {
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
}

export function log(...args: unknown[]) {
  console.log(...args);
  invoke('log', { data: formatLog(args) });
}

export function warn(...args: unknown[]) {
  console.warn(...args);
  invoke('warn', { data: formatLog(args) });
}

export function error(...args: unknown[]): string {
  console.error(...args);
  const logstr = formatLog(args);
  invoke('error', { data: logstr });
  return logstr;
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
