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

// The step label already says what these stages do; their counter takes the
// status line.
const COUNTED_STAGES = new Set(['scan_files', 'process_files', 'download_archive']);

// The lines under the step list. Fetching metadata shows none: the step
// label is the same text. A byte counter of any other stage follows its
// status, as for a runtime download.
function statusLines(progress: Progress, stat: string | null): string[] {
  if (progress.cancel === 'requested') return [t('running.cancelling')];
  if (stat !== null && COUNTED_STAGES.has(progress.stage)) return [stat];
  if (progress.stage === 'fetch_metadata') return [];
  const status = t('progress.' + progress.stage, { subject: progress.subject ?? '' });
  return stat !== null && progress.summary?.unit === 'bytes' ? [status, stat] : [status];
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
  const summary = progress.summary;
  const speed = progress.network_pending ? progress.network_bps : progress.processing_bps;
  const stat = summary && counter(summary, summary.unit === 'bytes')
    + (speed !== null ? ` (${formatSize(speed)}/s)` : '');
  const lines = statusLines(progress, stat);
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
      <div class="current-status">
        {lines.map((line, i) => (
          <div class="status-line" key={i}>{line}</div>
        ))}
        {progress.files.length > 0 ? (
          <div class="active-files">
            {progress.files.map((file) => (
              <div class="active-file" key={file.id} title={file.name}>
                <span class="active-file-name">{file.name.split(/[\\/]/).pop()}</span>
                <span>{`${formatSize(file.done)}/${formatSize(file.total)}`}</span>
              </div>
            ))}
          </div>
        ) : null}
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
