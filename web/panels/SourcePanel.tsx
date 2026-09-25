import { useEffect, useRef, useState } from 'preact/hooks';
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

function sourceVisible(
  s: SourceItem,
  currentUri: string,
  showHidden: boolean,
) {
  return !s.hidden || showHidden || s.uri === currentUri;
}

export function SourcePanel({
  ui,
  onClose,
  onMirrorc,
}: {
  ui: UiState;
  onClose: () => void;
  onMirrorc: (candidateUri: string) => void;
}) {
  const [showHidden, setShowHidden] = useState(false);
  const commas = useRef(0);
  const commaTimer = useRef<number>(0);

  useEffect(() => {
    function onKey(event: KeyboardEvent) {
      if (event.key !== ',' && event.code !== 'Comma') {
        return;
      }
      event.preventDefault();
      if (commaTimer.current) {
        window.clearTimeout(commaTimer.current);
      }
      commas.current += 1;
      if (commas.current >= 5) {
        setShowHidden(true);
        commas.current = 0;
        return;
      }
      commaTimer.current = window.setTimeout(() => {
        commas.current = 0;
        commaTimer.current = 0;
      }, 2000);
    }
    window.addEventListener('keydown', onKey);
    return () => {
      window.removeEventListener('keydown', onKey);
      if (commaTimer.current) {
        window.clearTimeout(commaTimer.current);
      }
    };
  }, []);

  const listed = ui.sources.filter((s) =>
    sourceVisible(s, ui.options.source_uri, showHidden),
  );

  return (
    <Dialog
      title={<div class="title">{t('ready.select_source')}</div>}
      desc={
        <div class="desc">{t('ready.source_multi', { title: ui.project.title })}</div>
      }
    >
      <div class="card-container">
        {listed.map((s) => (
          <div
            class={`card ${s.uri === ui.options.source_uri ? 'active' : ''}`}
            key={s.id}
            onClick={() => {
              if (s.uri.startsWith('mirrorc://')) {
                // Mirror 源和 CDK 都是候选：确定成功才提交，取消等于没点过这张卡片。
                onMirrorc(s.uri);
              } else {
                void intent({ kind: 'set_source', uri: s.uri });
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
