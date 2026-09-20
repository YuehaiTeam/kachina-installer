import { useEffect, useState } from 'preact/hooks';
import { formatSize, t } from '../i18n';
import { intent, type Progress, type UiState } from '../state';
import { Dialog } from '../ui/Dialog';
import { CircleSuccess } from '../ui/icons';
import { Spinner } from '../ui/Spinner';

const ELLIPSIS = new Set([
  'download',
  'extract',
  'patch',
  'delete',
  'mirrorc_download',
  'uninstall_delete',
]);

// Same list as `BYTE_STAGES` in native/session/state.rs: these stages report
// bytes in done/total, every other stage reports item counts.
const BYTE_STAGES = new Set(['download', 'runtime_download', 'mirrorc_download']);

function counter(progress: Progress): string | null {
  if (progress.done == null || progress.total == null) return null;
  const fmt = BYTE_STAGES.has(progress.stage) ? formatSize : String;
  return `${fmt(progress.done)} / ${fmt(progress.total)}`;
}

// Phase two does not observe the cancel token.
const NO_CANCEL = new Set([
  'commit',
  'finalize',
  'shortcut',
  'registry',
  'install_done',
  'already_latest',
  'runtime_download',
  'runtime_install',
]);

function CancelConfirm({ onYes, onNo }: { onYes: () => void; onNo: () => void }) {
  return (
    <Dialog
      title={<div class="title">{t('running.cancel_title')}</div>}
      desc={<div class="desc">{t('running.cancel_message')}</div>}
      footer={
        <>
          <button class="btn btn-install btn-install-2rd neutral" onClick={onYes}>
            {t('running.cancel_yes')}
          </button>
          <button class="btn btn-install" onClick={onNo}>
            {t('running.cancel_no')}
          </button>
        </>
      }
    />
  );
}

export function Running({ ui, progress }: { ui: UiState; progress: Progress }) {
  const [cancelling, setCancelling] = useState(false);
  const [confirming, setConfirming] = useState(false);
  const mirrorc = ui.options.source_uri.startsWith('mirrorc://');
  const prefix = mirrorc ? 'step.mirrorc.' : 'step.default.';
  // The step titles describe the install pipeline; uninstall only has a status line.
  const steps = ui.mode === 'uninstall' ? [] : [0, 1, 2, 3].map((i) => t(prefix + i));
  const status = t('progress.' + progress.stage, { subject: progress.subject ?? '' });
  const stat = counter(progress);
  const canCancel = !NO_CANCEL.has(progress.stage) && !cancelling;

  // The swap may start while the confirmation is open; there is nothing left to cancel.
  useEffect(() => {
    if (!canCancel) setConfirming(false);
  }, [canCancel]);

  return (
    <div class="progress">
      {ui.mode !== 'uninstall' ? (
        <button
          class="btn btn-install neutral"
          disabled={!canCancel}
          onClick={() => setConfirming(true)}
        >
          {t('dialog.cancel')}
        </button>
      ) : null}
      <div class="step-desc">
        {steps.map((label, i) =>
          i <= progress.sub_step ? (
            <div class={`substep ${i < progress.sub_step ? 'done' : ''}`} key={label}>
              {i === progress.sub_step ? (
                <Spinner size={16} />
              ) : (
                <span class="substep-done">
                  <CircleSuccess />
                </span>
              )}
              <div>{label}</div>
            </div>
          ) : null,
        )}
      </div>
      <div class={`current-status ${ELLIPSIS.has(progress.stage) ? 'ellipsis' : ''}`}>
        {status}
        {stat ? <span class="current-stat">{stat}</span> : null}
      </div>
      <div class="progress-bar" style={{ width: `${progress.percent}%` }} />
      {confirming ? (
        <CancelConfirm
          onYes={() => {
            setConfirming(false);
            setCancelling(true);
            void intent({ kind: 'cancel' });
          }}
          onNo={() => setConfirming(false)}
        />
      ) : null}
    </div>
  );
}
