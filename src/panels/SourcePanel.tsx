import { t } from '../i18n';
import { intent, type SourceItem, type UiState } from '../state';
import { Dialog } from '../ui/Dialog';
import { Cloud, CloudPaid, Feedback } from '../ui/icons';

function FallbackIcon({ uri }: { uri: string }) {
  if (uri.includes('=beta')) return <Feedback />;
  if (uri.startsWith('mirrorc://')) return <CloudPaid />;
  return <Cloud />;
}

function SourceIcon({ source }: { source: SourceItem }) {
  const svg = source.icon;
  if (svg && svg.trim().startsWith('<')) {
    return <span class="source-icon" dangerouslySetInnerHTML={{ __html: svg }} />;
  }
  return <FallbackIcon uri={source.uri} />;
}

export function SourcePanel({
  ui,
  onClose,
  onMirrorc,
}: {
  ui: UiState;
  onClose: () => void;
  onMirrorc: () => void;
}) {
  return (
    <Dialog
      title={<div class="title">{t('ready.select_source')}</div>}
      desc={
        <div class="desc">
          {ui.project.title}
          {t('ready.source_multi')}
        </div>
      }
    >
      <div class="card-container">
        {ui.sources.map((s) => (
          <div
            class={`card ${s.uri === ui.options.source_uri ? 'active' : ''}`}
            key={s.id}
            onClick={() => {
              void intent({ kind: 'set_source', uri: s.uri });
              if (s.uri.startsWith('mirrorc://')) {
                onMirrorc();
              } else {
                onClose();
              }
            }}
          >
            <SourceIcon source={s} />
            <span>{s.name}</span>
          </div>
        ))}
      </div>
    </Dialog>
  );
}
