import { render, screen, fireEvent, waitFor, cleanup } from '@testing-library/preact';
import { beforeEach, describe, expect, it } from 'vitest';
import { App } from '../App';
import { state } from '../state';
import { posted, resetHost } from './setup';
import {
  doneInstall,
  doneLatest,
  doneUninstall,
  doneUpdate,
  failed,
  pendingOccupied,
  pendingProcess,
  pendingVersion,
  ready,
  readyUninstall,
  readyUpdate,
  running,
} from './fixtures';

async function mount(ui = ready()) {
  cleanup();
  resetHost();
  state.value = ui;
  const view = render(<App />);
  await waitFor(() => {
    expect(screen.queryByText('Demo App')).not.toBeNull();
  });
  return view;
}

function lastIntent() {
  const msgs = posted.filter(
    (m): m is { cmd: string; args: { kind: string } } =>
      typeof m === 'object' && m !== null && (m as { cmd?: string }).cmd === 'intent',
  );
  return msgs[msgs.length - 1]?.args;
}

describe('screens', () => {
  beforeEach(() => {
    resetHost();
    state.value = null;
  });

  it('renders ready install', async () => {
    await mount(ready());
    expect(screen.getByText('安装')).toBeTruthy();
    expect(screen.getByText('创建桌面快捷方式')).toBeTruthy();
  });

  it('renders ready update', async () => {
    await mount(readyUpdate());
    expect(screen.getByText('更新')).toBeTruthy();
  });

  it('renders ready uninstall', async () => {
    await mount(readyUninstall());
    expect(screen.getByText('卸载')).toBeTruthy();
    expect(screen.getByText('同时删除用户数据')).toBeTruthy();
  });

  it('renders running progress; cancel asks for confirmation first', async () => {
    await mount(running());
    expect(screen.getByText('app.exe')).toBeTruthy();
    expect(screen.getByText('正在处理文件……')).toBeTruthy();
    fireEvent.click(screen.getByText('取消'));
    expect(screen.getByText('确定要取消吗？')).toBeTruthy();
    expect(lastIntent()).toBeUndefined();
    fireEvent.click(screen.getByText('否，继续'));
    expect(screen.queryByText('确定要取消吗？')).toBeNull();
    expect(lastIntent()).toBeUndefined();
    fireEvent.click(screen.getByText('取消'));
    fireEvent.click(screen.getByText('是，取消'));
    await waitFor(() => expect(lastIntent()).toEqual({ kind: 'cancel' }));
    const cancelling = running();
    if (cancelling.phase.kind === 'running') cancelling.phase.cancel = 'requested';
    await mount(cancelling);
    expect(screen.getByText('正在取消，等待任务结束并清理……')).toBeTruthy();
    expect((screen.getByText('取消') as HTMLButtonElement).disabled).toBe(true);
  });

  it('shows active files, explicit units and zero network speed before offline processing speed', async () => {
    const ui = running();
    if (ui.phase.kind !== 'running') throw new Error('fixture');
    ui.phase.files.push({ id: 2, name: 'assets/app.exe', action: 'verify', bytes: null });
    await mount(ui);
    expect(screen.getByTitle('网络接收速度').textContent).toBe('0B/s');
    expect(screen.getByTitle('assets/app.exe')).toBeTruthy();
    expect(screen.getByText('校验')).toBeTruthy();
    ui.phase.network_pending = false;
    ui.phase.summary = { unit: 'files', done: 0, total: 0 };
    ui.phase.percent = null;
    await mount(ui);
    expect(screen.getByTitle('文件处理速度').textContent).toBe('1KB/s');
    expect(screen.getByText('0 / 0')).toBeTruthy();
    expect(screen.getByRole('progressbar').getAttribute('aria-valuenow')).toBeNull();
    ui.phase.summary = { unit: 'bytes', done: 0, total: null };
    ui.phase.files = [];
    await mount(ui);
    expect(screen.queryByTitle('assets/app.exe')).toBeNull();
    expect(screen.getByText('0B')).toBeTruthy();
  });

  it('keeps the cancel button in place but disabled during the swap', async () => {
    const ui = running();
    if (ui.phase.kind === 'running') { ui.phase.stage = 'commit'; ui.phase.cancel = 'unavailable'; }
    await mount(ui);
    const btn = screen.getByText('取消') as HTMLButtonElement;
    expect(btn.disabled).toBe(true);
    expect(btn.classList.contains('btn-install-2rd')).toBe(false);
  });

  it('disables cancel during runtime install', async () => {
    const ui = running();
    if (ui.phase.kind === 'running') { ui.phase.stage = 'install_runtime'; ui.phase.cancel = 'unavailable'; }
    await mount(ui);
    expect((screen.getByText('取消') as HTMLButtonElement).disabled).toBe(true);
  });

  it('has no cancel button while uninstalling', async () => {
    const ui = running();
    ui.mode = 'uninstall';
    await mount(ui);
    expect(screen.queryByText('取消')).toBeNull();
  });

  it('always renders the side column; default art when the package has no image', async () => {
    await mount(ready({ theme: 'none' }));
    expect(document.querySelector('.image .image-default svg')).not.toBeNull();
    expect(document.querySelector('.image img')).toBeNull();
    await mount(ready({ theme: 'image' }));
    expect(document.querySelector('.image img')).not.toBeNull();
    expect(document.querySelector('.image-default')).toBeNull();
  });

  it('renders English copy when project.lang is en-US', async () => {
    const ui = ready();
    ui.project = { ...ui.project, lang: 'en-US' };
    await mount(ui);
    expect(screen.getByText('Install')).toBeTruthy();
    expect(screen.getByText('Create a desktop shortcut')).toBeTruthy();
    fireEvent.click(screen.getByTitle('Select a source'));
    expect(screen.getByText('Demo App supports multiple online install sources.')).toBeTruthy();
  });

  it('renders done variants', async () => {
    await mount(doneInstall());
    expect(screen.getByText('安装完成')).toBeTruthy();
    await mount(doneUpdate());
    expect(screen.getByText('更新完成')).toBeTruthy();
    await mount(doneLatest());
    expect(screen.getByText('您已安装最新版本')).toBeTruthy();
    await mount(doneUninstall());
    expect(screen.getByText('卸载成功')).toBeTruthy();
  });

  it('renders failed then posts error_dialog then dismiss', async () => {
    await mount(failed('PKG_BROKEN'));
    await waitFor(() => {
      expect(posted.some((m) => (m as { cmd?: string }).cmd === 'error_dialog')).toBe(true);
    });
    await waitFor(() => expect(lastIntent()).toEqual({ kind: 'dismiss' }));
  });

  it('renders process_running prompt', async () => {
    await mount(pendingProcess());
    expect(screen.getByText('确定')).toBeTruthy();
    fireEvent.click(screen.getByText('确定'));
    await waitFor(() => {
      expect(lastIntent()).toEqual({ kind: 'answer', id: 'p1', ok: true });
    });
  });

  it('renders occupied_files prompt cancel', async () => {
    await mount(pendingOccupied());
    fireEvent.click(screen.getByText('取消'));
    await waitFor(() => {
      expect(lastIntent()).toEqual({ kind: 'answer', id: 'p2', ok: false });
    });
  });

  it('renders version_mismatch prompt', async () => {
    await mount(pendingVersion());
    expect(screen.getByText(/当前安装包不是最新版本/)).toBeTruthy();
  });
});

describe('intents', () => {
  beforeEach(() => {
    resetHost();
    state.value = null;
  });

  it('Start from install button', async () => {
    await mount(ready({ mode: 'update' }));
    fireEvent.click(screen.getByText('更新'));
    await waitFor(() => expect(lastIntent()?.kind).toBe('start'));
  });

  it('SetCreateLnk from checkbox', async () => {
    await mount(ready());
    const boxes = document.querySelectorAll('input[type="checkbox"]');
    fireEvent.click(boxes[0]);
    await waitFor(() => expect(lastIntent()?.kind).toBe('set_create_lnk'));
  });

  it('SetDeleteUserData from uninstall checkbox', async () => {
    await mount(readyUninstall());
    const boxes = document.querySelectorAll('input[type="checkbox"]');
    fireEvent.click(boxes[0]);
    await waitFor(() => expect(lastIntent()?.kind).toBe('set_delete_user_data'));
  });

  it('pick_path then SetPath', async () => {
    await mount(ready());
    fireEvent.click(screen.getByTitle('更改安装位置'));
    await waitFor(() => {
      expect(posted.some((m) => (m as { cmd?: string }).cmd === 'pick_path')).toBe(true);
    });
    await waitFor(() => expect(lastIntent()).toEqual({ kind: 'set_path', path: 'C:\\picked' }));
  });

  it('SetSource from source panel', async () => {
    await mount(ready());
    fireEvent.click(screen.getByTitle('选择安装源'));
    fireEvent.click(document.querySelector('.card') as HTMLElement);
    await waitFor(() =>
      expect(lastIntent()).toEqual({
        kind: 'set_source',
        uri: 'https://example.com/app.json',
      }),
    );
  });

  it('Launch from done', async () => {
    await mount(doneInstall());
    fireEvent.click(screen.getByText('启动'));
    await waitFor(() => expect(lastIntent()?.kind).toBe('launch'));
  });

  it('Close from uninstall done', async () => {
    await mount(doneUninstall());
    fireEvent.click(screen.getByText('关闭'));
    await waitFor(() => expect(lastIntent()?.kind).toBe('close'));
  });

  it('SetCdk from cdk panel', async () => {
    await mount(ready());
    fireEvent.click(screen.getByTitle('选择安装源'));
    fireEvent.click(document.querySelectorAll('.card')[1] as HTMLElement);
    const input = document.querySelector('input[type="text"]') as HTMLInputElement;
    fireEvent.input(input, { target: { value: 'cdk-1' } });
    fireEvent.click(screen.getByText('确定'));
    await waitFor(() => expect(lastIntent()).toEqual({ kind: 'set_cdk', cdk: 'cdk-1' }));
  });
});
