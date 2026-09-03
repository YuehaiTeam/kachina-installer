import { useEffect, useState } from 'preact/hooks';
import { invoke, listen } from './host';
import { ready as i18nReady } from './i18n';
import { t } from './i18n';
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
import { Dialog } from './ui/Dialog';
import { IconClose, IconMinimize } from './ui/icons';
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
    return <Ready ui={ui} onOpenSource={onOpenSource} onOpenCdk={onOpenCdk} />;
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
  const [copyReady, setCopyReady] = useState(false);

  useEffect(() => {
    void i18nReady().then(() => setCopyReady(true));
    void invoke('window_show');
    let unsub: (() => void) | undefined;
    void listen<Coded>('ui-notice', (coded) => {
      void invoke('error_dialog', errorDialogArgs(coded));
    }).then((fn) => {
      unsub = fn;
    });
    return () => unsub?.();
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

  const noImage = ui.theme === 'none' || ui.theme === 'css' || ui.theme === 'html';
  const showImage = ui.theme === 'image';

  return (
    <div class="main">
      {ui.theme === 'css' ? <link rel="stylesheet" href="/theme.css" /> : null}
      <div
        class={`content ${ui.project.borderless ? 'borderless' : ''} ${noImage ? 'no-image' : ''}`}
      >
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
        {showImage ? (
          <div class="image">
            <img src="/theme.webp" alt={ui.project.title} />
          </div>
        ) : null}
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
          onMirrorc={() => setPanel('cdk')}
        />
      ) : null}
      {panel === 'cdk' ? <CdkPanel ui={ui} onClose={() => setPanel(null)} /> : null}
      {ui.pending ? <PromptModal ui={ui} /> : null}
    </div>
  );
}
