import { useState } from 'preact/hooks';
import { t } from '../i18n';
import { intent, type UiState } from '../state';
import { invoke } from '../host';
import { Checkbox } from '../ui/Checkbox';
import { IconEdit, IconShield } from '../ui/icons';

export function Ready({
  ui,
  onOpenSource,
  onOpenCdk,
}: {
  ui: UiState;
  onOpenSource: () => void;
  onOpenCdk: () => void;
}) {
  const uninstall = ui.mode === 'uninstall';
  const update = ui.mode === 'update';
  const [agree, setAgree] = useState(update || uninstall);
  const source = ui.sources.find((s) => s.uri === ui.options.source_uri);
  const mirrorc = ui.options.source_uri.startsWith('mirrorc://');
  const markedKey = ui.options.mirrorc_cdk
    ? ui.options.mirrorc_cdk.slice(0, 4) + '****'
    : t('ready.no_cdk');
  const showSources = ui.sources.length > 1 && !uninstall;

  async function pickPath() {
    const path = await invoke<string>('pick_path');
    if (path) {
      await intent({ kind: 'set_path', path });
    }
  }

  const verb = uninstall
    ? t('ready.uninstall')
    : update
      ? t('ready.update')
      : t('ready.install');
  const destLabel = uninstall
    ? t('ready.uninstall_from')
    : update
      ? t('ready.update_to')
      : t('ready.install_to');

  return (
    <div class="actions">
      {!update && !uninstall ? (
        <div class="lnk">
          <Checkbox
            checked={ui.options.create_lnk}
            onChange={(value) => void intent({ kind: 'set_create_lnk', value })}
          />
          {t('ready.create_lnk')}
        </div>
      ) : null}
      {!update && !uninstall ? (
        <div class="read">
          <Checkbox checked={agree} onChange={setAgree} />
          {t('ready.agree')}
          <a>{t('ready.eula')}</a>
        </div>
      ) : null}
      {uninstall ? (
        <div class="read">
          <Checkbox
            checked={ui.options.delete_user_data}
            onChange={(value) => void intent({ kind: 'set_delete_user_data', value })}
          />
          {t('ready.delete_user_data')}
        </div>
      ) : null}
      <div class="more">
        <span>
          {showSources ? (
            <>
              <span>{t('ready.from')} </span>
              <a
                onClick={() => (mirrorc ? onOpenCdk() : onOpenSource())}
                title={t('ready.select_source')}
              >
                {source?.name ?? ui.options.source_uri}
                {mirrorc ? `(${markedKey})` : null}
                <IconEdit />
              </a>
            </>
          ) : null}
          <span> {destLabel} </span>
        </span>
        {uninstall ? (
          <a>{ui.options.install_path}</a>
        ) : (
          <a onClick={() => void pickPath()} title={t('ready.change_path')}>
            {ui.options.install_path}
            <IconEdit />
          </a>
        )}
      </div>
      <button
        class="btn btn-install"
        disabled={!uninstall && !update && !agree}
        onClick={() => void intent({ kind: 'start' })}
      >
        {ui.needs_elevate ? (
          <IconShield />
        ) : null}
        {verb}
      </button>
    </div>
  );
}
