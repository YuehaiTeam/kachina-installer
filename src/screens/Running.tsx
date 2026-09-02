import { formatSize, t } from '../i18n';
import type { Progress, UiState } from '../state';
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

export function Running({ ui, progress }: { ui: UiState; progress: Progress }) {
  const mirrorc = ui.options.source_uri.startsWith('mirrorc://');
  const prefix = mirrorc ? 'step.mirrorc.' : 'step.default.';
  const steps = [0, 1, 2, 3].map((i) => t(prefix + i));
  const status = t('progress.' + progress.stage, {
    subject: progress.subject ?? '',
    done: progress.done != null ? formatSize(progress.done) : '',
    total: progress.total != null ? formatSize(progress.total) : '',
  });
  return (
    <div class="progress">
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
      </div>
      <div class="progress-bar" style={{ width: `${progress.percent}%` }} />
    </div>
  );
}
