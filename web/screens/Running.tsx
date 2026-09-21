import { useEffect, useState } from 'preact/hooks';
import { formatSize, t } from '../i18n';
import { intent, type Progress, type UiState } from '../state';
import { Dialog } from '../ui/Dialog';
import { CircleSuccess } from '../ui/icons';
import { Spinner } from '../ui/Spinner';

function counter(value: { done: number; total: number | null }, bytes: boolean): string {
  const fmt = bytes ? formatSize : String;
  return value.total === null ? fmt(value.done) : `${fmt(value.done)} / ${fmt(value.total)}`;
}

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
  const [sending, setSending] = useState(false);
  const [confirming, setConfirming] = useState(false);
  const mirrorc = ui.options.source_uri.startsWith('mirrorc://');
  const prefix = mirrorc ? 'step.mirrorc.' : 'step.default.';
  // The step titles describe the install pipeline; uninstall only has a status line.
  const steps = progress.step === null ? [] : [0, 1, 2, 3].map((i) => t(prefix + i));
  const status = progress.cancel === 'requested' ? t('running.cancelling')
    : t('progress.' + progress.stage, { subject: progress.subject ?? '' });
  const stat = progress.summary && counter(progress.summary, progress.summary.unit === 'bytes');
  const speed = progress.network_pending ? progress.network_bps : progress.processing_bps;
  const speedLabel = progress.network_pending ? t('running.network_speed') : t('running.processing_speed');
  const canCancel = progress.cancel === 'available' && !sending;

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
          i <= (progress.step ?? -1) ? (
            <div class={`substep ${i < (progress.step ?? -1) ? 'done' : ''}`} key={label}>
              {i === (progress.step ?? -1) ? (
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
      <div class="current-status">{status}</div>
      <div class="progress-summary">
        {stat !== null ? <span>{stat}</span> : null}
        {speed !== null ? <span title={speedLabel}>{formatSize(speed)}/s</span> : null}
      </div>
      <div class="active-files">
        {progress.files.map((file) => (
          <div class="active-file" key={file.id}>
            <span class="active-file-name" title={file.name}>{file.name}</span>
            <span>{t('file_action.' + file.action)}</span>
            {file.bytes !== null ? <span class="active-file-counter">{counter(file.bytes, true)}</span> : null}
          </div>
        ))}
      </div>
      <div class={`progress-bar ${progress.percent === null ? 'indeterminate' : ''}`}
        role="progressbar" aria-valuenow={progress.percent ?? undefined}
        style={progress.percent === null ? undefined : { width: `${progress.percent}%` }} />
      {confirming ? (
        <CancelConfirm
          onYes={() => {
            setConfirming(false);
            setSending(true);
            void intent({ kind: 'cancel' }).finally(() => setSending(false));
          }}
          onNo={() => setConfirming(false)}
        />
      ) : null}
    </div>
  );
}
