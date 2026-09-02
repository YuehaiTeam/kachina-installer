import type { ComponentChildren } from 'preact';

export function Dialog({
  title,
  desc,
  children,
  footer,
  onKeyDown,
}: {
  title: ComponentChildren;
  desc?: ComponentChildren;
  children?: ComponentChildren;
  footer?: ComponentChildren;
  onKeyDown?: (e: KeyboardEvent) => void;
}) {
  return (
    <div class="dialog" onKeyDown={onKeyDown} tabIndex={0}>
      <div class="dialog-title">{title}</div>
      {desc ? <div class="dialog-desc">{desc}</div> : null}
      <div class="dialog-body">{children}</div>
      {footer ? <div class="dialog-footer">{footer}</div> : null}
    </div>
  );
}
