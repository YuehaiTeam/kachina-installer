export function Spinner({ size = 16 }: { size?: number }) {
  return (
    <span
      class="fui-Spinner__spinner"
      style={{ width: `${size}px`, height: `${size}px` }}
    >
      <span class="fui-Spinner__spinnerTail" />
    </span>
  );
}
