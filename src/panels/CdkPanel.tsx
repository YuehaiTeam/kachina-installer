import { useEffect, useRef, useState } from 'preact/hooks';
import { t } from '../i18n';
import { invoke } from '../host';
import { intent, isCdkInvalid, type UiState } from '../state';
import { Dialog } from '../ui/Dialog';
import { Input } from '../ui/Input';
import { Spinner } from '../ui/Spinner';

export function CdkPanel({ ui, onClose }: { ui: UiState; onClose: () => void }) {
  const [value, setValue] = useState(ui.options.mirrorc_cdk ?? '');
  const checking = ui.cdk.kind === 'checking';

  useEffect(() => {
    if (isCdkInvalid(ui.cdk)) {
      void invoke('error_dialog', {
        code: ui.cdk.code,
        detail: ui.cdk.detail,
        subject: ui.cdk.subject,
      });
    }
  }, [ui.cdk]);

  const prevKind = useRef(ui.cdk.kind);
  useEffect(() => {
    if (prevKind.current === 'checking' && ui.cdk.kind === 'ok') {
      onClose();
    }
    prevKind.current = ui.cdk.kind;
  }, [ui.cdk, onClose]);

  function submit() {
    void intent({ kind: 'set_cdk', cdk: value });
  }

  return (
    <Dialog
      title={<div class="title">{t('dialog.mirrorc_cdk_title')}</div>}
      desc={<div class="desc">{t('dialog.mirrorc_cdk_hint')}</div>}
      footer={
        <>
          <button class="btn btn-install btn-install-2rd neutral" onClick={onClose}>
            {t('dialog.cancel')}
          </button>
          <button class="btn btn-install" disabled={checking} onClick={submit}>
            {checking ? <Spinner size={16} /> : null}
            {t('dialog.ok')}
          </button>
        </>
      }
    >
      <Input
        class="cdk-input"
        value={value}
        placeholder={t('dialog.mirrorc_cdk_placeholder')}
        onInput={setValue}
        onBlur={() => {
          if (value !== (ui.options.mirrorc_cdk ?? '')) {
            void intent({ kind: 'set_cdk', cdk: value });
          }
        }}
      />
      <div class="desc">
        <a
          style={{ cursor: 'pointer' }}
          onClick={() =>
            void invoke('launch', {
              path: `https://mirrorchyan.com/?source=Kachina${ui.project.title}`,
            })
          }
        >
          {t('dialog.mirrorc_cdk_get')}
        </a>
      </div>
    </Dialog>
  );
}
