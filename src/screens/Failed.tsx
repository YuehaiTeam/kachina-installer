import { useEffect, useRef } from 'preact/hooks';
import { invoke } from '../host';
import { intent, type Coded } from '../state';

const CDK_CODES = new Set([
  'MIRRORC_CDK_MISSING',
  'MIRRORC_CDK_EXPIRED',
  'MIRRORC_CDK_INVALID',
  'MIRRORC_CDK_MISMATCH',
  'MIRRORC_CDK_QUOTA_EXCEEDED',
  'MIRRORC_CDK_BANNED',
]);

export function Failed({
  coded,
  onOpenCdk,
}: {
  coded: Coded;
  onOpenCdk: () => void;
}) {
  const ran = useRef(false);
  useEffect(() => {
    if (ran.current) return;
    ran.current = true;
    const code = coded.code;
    void (async () => {
      await invoke('error_dialog', {
        code: coded.code,
        detail: coded.detail,
        subject: coded.subject,
      });
      await intent({ kind: 'dismiss' });
      if (CDK_CODES.has(code)) {
        onOpenCdk();
      }
    })();
  }, [coded, onOpenCdk]);
  return <div class="finish" />;
}
