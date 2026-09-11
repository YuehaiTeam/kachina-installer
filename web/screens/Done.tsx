import { t } from '../i18n';
import { intent, type SessionResult } from '../state';
import { CircleSuccess } from '../ui/icons';

export function Done({ result }: { result: SessionResult }) {
  const key = result.is_uninstall
    ? 'done.uninstall'
    : result.already_latest
      ? 'done.latest'
      : result.is_update
        ? 'done.update'
        : 'done.install';
  return (
    <div class="finish">
      <div class="finish-text">
        <CircleSuccess />
        {t(key)}
      </div>
      {result.is_uninstall ? (
        <button class="btn btn-install" onClick={() => void intent({ kind: 'close' })}>
          {t('done.close')}
        </button>
      ) : (
        <button class="btn btn-install" onClick={() => void intent({ kind: 'launch' })}>
          {t('done.launch')}
        </button>
      )}
    </div>
  );
}
