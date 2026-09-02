import { afterEach } from 'vitest';
import { cleanup } from '@testing-library/preact';
import { vi } from 'vitest';

type Handler = (ev: { data: unknown }) => void;

const listeners: Handler[] = [];
export const posted: unknown[] = [];
const pending = new Map<number, { ok: boolean; data?: unknown; error?: unknown }>();

export function resetHost() {
  posted.length = 0;
  pending.clear();
}

export function emitEvent(event: string, payload: unknown) {
  const msg = { kind: 'event', event, payload };
  for (const h of [...listeners]) {
    h({ data: msg });
  }
}

export function replyTo(id: number, ok: boolean, data?: unknown) {
  const msg = { kind: 'reply', id, ok, data, error: ok ? undefined : data };
  for (const h of [...listeners]) {
    h({ data: msg });
  }
}

(globalThis as unknown as { chrome: unknown }).chrome = {
  webview: {
    postMessage(msg: { id: number; kind: string; cmd: string; args: unknown }) {
      posted.push(msg);
      queueMicrotask(() => {
        if (msg.kind !== 'invoke') return;
        if (msg.cmd === 'window_show' || msg.cmd === 'plugin_host_ready') {
          replyTo(msg.id, true, null);
          return;
        }
        replyTo(msg.id, true, msg.cmd === 'pick_path' ? 'C:\\picked' : null);
      });
    },
    addEventListener(_type: string, handler: Handler) {
      listeners.push(handler);
    },
  },
};

vi.stubGlobal(
  'fetch',
  vi.fn(async (url: string) => {
    if (String(url).includes('i18n.tsv')) {
      return {
        ok: true,
        text: async () =>
          [
            'KEY\tzh-CN',
            'ready.install\t安装',
            'ready.update\t更新',
            'ready.uninstall\t卸载',
            'ready.create_lnk\t创建桌面快捷方式',
            'ready.agree\t我已阅读并同意',
            'ready.eula\t用户协议',
            'ready.delete_user_data\t同时删除用户数据',
            'ready.from\t从',
            'ready.install_to\t安装到',
            'ready.update_to\t更新到',
            'ready.uninstall_from\t卸载自',
            'ready.change_path\t更改安装位置',
            'ready.select_source\t选择安装源',
            'ready.source_multi\t支持多种在线安装方式。',
            'ready.no_cdk\t无CDK',
            'done.install\t安装完成',
            'done.update\t更新完成',
            'done.latest\t您已安装最新版本',
            'done.uninstall\t卸载成功',
            'done.launch\t启动',
            'done.close\t关闭',
            'progress.download\t下载 {subject} ……',
            'step.default.0\t获取最新版本',
            'step.default.1\t校验更新内容',
            'step.default.2\t下载和解压文件',
            'step.default.3\t准备运行环境',
            'step.mirrorc.0\t从 Mirror酱 获取最新版本',
            'step.mirrorc.1\t下载数据包',
            'step.mirrorc.2\t解压文件',
            'step.mirrorc.3\t准备运行环境',
            'prompt.process_running.title\t提示',
            'prompt.process_running.message\t检测到{items}正在运行',
            'prompt.occupied_files.title\t提示',
            'prompt.occupied_files.message\t文件被占用 {items}',
            'prompt.version_mismatch.title\t提示',
            'prompt.version_mismatch.message\t版本不一致',
            'dialog.ok\t确定',
            'dialog.cancel\t取消',
            'dialog.mirrorc_cdk_title\t设置 Mirror酱 CDK',
            'dialog.mirrorc_cdk_hint\t输入 CDK',
            'dialog.mirrorc_cdk_placeholder\t请输入 Mirror酱 CDK',
            'dialog.mirrorc_cdk_get\t获取 CDK',
          ].join('\n'),
      };
    }
    return { ok: false, text: async () => '' };
  }),
);

afterEach(() => {
  cleanup();
});
