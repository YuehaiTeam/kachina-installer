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

  it('renders running progress with a cancel button', async () => {
    await mount(running());
    expect(screen.getByText(/下载 app.exe/)).toBeTruthy();
    fireEvent.click(screen.getByText('取消'));
    await waitFor(() => expect(lastIntent()).toEqual({ kind: 'cancel' }));
  });

  it('hides cancel during the swap', async () => {
    const ui = running();
    if (ui.phase.kind === 'running') ui.phase.stage = 'commit';
    await mount(ui);
    expect(screen.queryByText('取消')).toBeNull();
  });

  it('hides cancel during runtime install', async () => {
    const ui = running();
    if (ui.phase.kind === 'running') ui.phase.stage = 'runtime_install';
    await mount(ui);
    expect(screen.queryByText('取消')).toBeNull();
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
