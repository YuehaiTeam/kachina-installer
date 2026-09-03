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

// Same list as `BYTE_STAGES` in src-tauri/src/session/state.rs: these stages report
// bytes in done/total, every other stage reports item counts.
const BYTE_STAGES = new Set(['download', 'runtime_download', 'mirrorc_download']);

function counter(progress: Progress): string | null {
  if (progress.done == null || progress.total == null) return null;
  const fmt = BYTE_STAGES.has(progress.stage) ? formatSize : String;
  return `${fmt(progress.done)} / ${fmt(progress.total)}`;
}

export function Running({ ui, progress }: { ui: UiState; progress: Progress }) {
  const mirrorc = ui.options.source_uri.startsWith('mirrorc://');
  const prefix = mirrorc ? 'step.mirrorc.' : 'step.default.';
  // The step titles describe the install pipeline; uninstall only has a status line.
  const steps = ui.mode === 'uninstall' ? [] : [0, 1, 2, 3].map((i) => t(prefix + i));
  const status = t('progress.' + progress.stage, { subject: progress.subject ?? '' });
  const stat = counter(progress);
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
        {stat ? <span class="current-stat">{stat}</span> : null}
      </div>
      <div class="progress-bar" style={{ width: `${progress.percent}%` }} />
    </div>
  );
}
