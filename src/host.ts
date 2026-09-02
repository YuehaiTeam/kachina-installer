type InvokeMsg = {
  id: number;
  kind: 'invoke';
  cmd: string;
  args: unknown;
};

type ReplyMsg = {
  id: number;
  kind: 'reply';
  ok: boolean;
  data?: unknown;
  error?: {
    code?: string | null;
    detail?: string | null;
    subject?: string | null;
    insight?: unknown;
    message?: string;
  };
};

type EventMsg = {
  kind: 'event';
  event: string;
  payload: unknown;
};

type HostMsg = ReplyMsg | EventMsg;

type Pending = {
  resolve: (value: unknown) => void;
  reject: (error: unknown) => void;
};

const pending = new Map<number, Pending>();
const listeners = new Map<string, Set<(payload: unknown) => void>>();
let nextId = 1;

function webview(): {
  postMessage: (msg: unknown) => void;
  addEventListener: (
    type: string,
    handler: (ev: { data: HostMsg }) => void,
  ) => void;
} {
  const wv = (window as unknown as { chrome?: { webview?: unknown } }).chrome
    ?.webview as
    | {
        postMessage: (msg: unknown) => void;
        addEventListener: (
          type: string,
          handler: (ev: { data: HostMsg }) => void,
        ) => void;
      }
    | undefined;
  if (!wv) {
    throw new Error('chrome.webview is not available');
  }
  return wv;
}

let listening = false;
function ensureListen() {
  if (listening) return;
  listening = true;
  webview().addEventListener('message', (ev) => {
    const msg = ev.data;
    if (!msg || typeof msg !== 'object') return;
    if (msg.kind === 'reply') {
      const waiter = pending.get(msg.id);
      if (!waiter) return;
      pending.delete(msg.id);
      if (msg.ok) {
        waiter.resolve(msg.data);
      } else {
        waiter.reject(msg.error ?? { code: null, detail: 'invoke failed' });
      }
      return;
    }
    if (msg.kind === 'event') {
      const set = listeners.get(msg.event);
      if (!set) return;
      for (const cb of set) {
        cb(msg.payload);
      }
    }
  });
}

export async function invoke<T = unknown>(
  cmd: string,
  args?: unknown,
): Promise<T> {
  ensureListen();
  const id = nextId++;
  return new Promise<T>((resolve, reject) => {
    pending.set(id, {
      resolve: (value) => resolve(value as T),
      reject,
    });
    const msg: InvokeMsg = { id, kind: 'invoke', cmd, args: args ?? {} };
    webview().postMessage(msg);
  });
}

export async function listen<T>(
  event: string,
  handler: (payload: T) => void,
): Promise<() => void> {
  ensureListen();
  let set = listeners.get(event);
  if (!set) {
    set = new Set();
    listeners.set(event, set);
  }
  const wrapped = (payload: unknown) => handler(payload as T);
  set.add(wrapped);
  return () => {
    set?.delete(wrapped);
  };
}

function formatLog(args: unknown[]): string {
  return args.reduce((acc: string, arg) => {
    if (typeof arg === 'string') {
      return acc + ' ' + arg;
    }
    return acc + ' ' + (arg instanceof Error ? arg.toString() : JSON.stringify(arg));
  }, '');
}

export function log(...args: unknown[]) {
  console.log(...args);
  void invoke('log', { data: formatLog(args) });
}

export function warn(...args: unknown[]) {
  console.warn(...args);
  void invoke('warn', { data: formatLog(args) });
}

export function error(...args: unknown[]): string {
  console.error(...args);
  const logstr = formatLog(args);
  void invoke('error', { data: logstr });
  return logstr;
}
