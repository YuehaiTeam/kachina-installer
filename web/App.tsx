import { useEffect, useState } from 'preact/hooks';
import { invoke, listen } from './host';
import { ready as i18nReady, t } from './i18n';
import {
  errorDialogArgs,
  intent,
  isDone,
  isFailed,
  isRunning,
  state,
  type Coded,
  type UiState,
} from './state';
import { Ready } from './screens/Ready';
import { Running } from './screens/Running';
import { Done } from './screens/Done';
import { Failed } from './screens/Failed';
import { SourcePanel } from './panels/SourcePanel';
import { CdkPanel } from './panels/CdkPanel';
import { registerPluginBridge } from './plugin-host';
import { Dialog } from './ui/Dialog';
import { IconClose, IconMinimize, WizardArt } from './ui/icons';
import { Spinner } from './ui/Spinner';

type Panel = 'source' | 'cdk' | null;

function PromptModal({ ui }: { ui: UiState }) {
  const prompt = ui.pending;
  if (!prompt) return null;
  const title = t(`prompt.${prompt.kind}.title`, prompt.params);
  const message = t(`prompt.${prompt.kind}.message`, {
    ...prompt.params,
    items: prompt.items.join('\n'),
  });
  return (
    <Dialog
      title={<div class="title">{title}</div>}
      desc={<div class="desc">{message}</div>}
      footer={
        <>
          <button
            class="btn btn-install btn-install-2rd neutral"
            onClick={() => void intent({ kind: 'answer', id: prompt.id, ok: false })}
          >
            {t('dialog.cancel')}
          </button>
          <button
            class="btn btn-install"
            onClick={() => void intent({ kind: 'answer', id: prompt.id, ok: true })}
          >
            {t('dialog.ok')}
          </button>
        </>
      }
    >
      {prompt.items.length ? (
        <ul class="prompt-items">
          {prompt.items.map((item) => (
            <li key={item}>{item}</li>
          ))}
        </ul>
      ) : null}
    </Dialog>
  );
}

function Screen({
  ui,
  onOpenSource,
  onOpenCdk,
}: {
  ui: UiState;
  onOpenSource: () => void;
  onOpenCdk: () => void;
}) {
  const { phase } = ui;
  if (phase.kind === 'ready') {
    return <Ready ui={ui} onOpenSource={onOpenSource} />;
  }
  if (isRunning(phase)) {
    return <Running ui={ui} progress={phase} />;
  }
  if (isDone(phase)) {
    return <Done result={phase} />;
  }
  if (isFailed(phase)) {
    return <Failed coded={phase} onOpenCdk={onOpenCdk} />;
  }
  return null;
}

export function App() {
  const ui = state.value;
  const [panel, setPanel] = useState<Panel>(null);
  // Mirror酱走"确认后才生效"：记录打开 CDK 面板前的来源，取消时恢复，
  // 保证取消不会改变原本的选择。
  const [cdkFrom, setCdkFrom] = useState<string | null>(null);
  const [copyReady, setCopyReady] = useState(false);

  useEffect(() => {
    void i18nReady().then(() => setCopyReady(true));
    void invoke('window_show');
    let unsub: (() => void) | undefined;
    // session-plugin 监听必须先于 frontend_ready 就位，否则自动启动的
    // 会话发出插件请求时无人应答，只能等 60s 超时。
    void registerPluginBridge()
      .then(() =>
        listen<Coded>('ui-notice', (coded) => {
          void invoke('error_dialog', errorDialogArgs(coded));
        }),
      )
      .then((fn) => {
        unsub = fn;
        // 监听已就绪，向 host 看门狗上报页面可用。
        void invoke('frontend_ready');
      });
    return () => {
      unsub?.();
    };
  }, []);

  if (!ui || !copyReady) {
    return (
      <div class="main">
        <div class="init-loading">
          <Spinner size={40} />
        </div>
      </div>
    );
  }

  return (
    <div class="main">
      {ui.theme === 'css' ? <link rel="stylesheet" href="/theme.css" /> : null}
      <div class={`content ${ui.project.borderless ? 'borderless' : ''}`}>
        {ui.project.borderless ? (
          <div class="controls">
            <button class="cont-minimize" onClick={() => void invoke('window_minimize')}>
              <IconMinimize />
            </button>
            <button class="cont-close" onClick={() => void invoke('window_close')}>
              <IconClose />
            </button>
          </div>
        ) : null}
        <div class="image">
          {ui.theme === 'image' ? (
            <img src="/theme.webp" alt={ui.project.title} />
          ) : (
            <div class="image-default">
              <WizardArt />
            </div>
          )}
        </div>
        <div class="right">
          <div class="title">{ui.project.title}</div>
          <div class="desc">{ui.project.description}</div>
          <Screen
            ui={ui}
            onOpenSource={() => setPanel('source')}
            onOpenCdk={() => setPanel('cdk')}
          />
        </div>
      </div>
      {panel === 'source' ? (
        <SourcePanel
          ui={ui}
          onClose={() => setPanel(null)}
          onMirrorc={(from) => {
            setCdkFrom(from);
            setPanel('cdk');
          }}
        />
      ) : null}
      {panel === 'cdk' ? (
        <CdkPanel
          ui={ui}
          onCancel={() => {
            const from = cdkFrom;
            setCdkFrom(null);
            setPanel(null);
            if (from) {
              void intent({ kind: 'set_source', uri: from });
            }
          }}
          onConfirmed={() => {
            setCdkFrom(null);
            setPanel(null);
          }}
        />
      ) : null}
      {ui.pending ? <PromptModal ui={ui} /> : null}
    </div>
  );
}
