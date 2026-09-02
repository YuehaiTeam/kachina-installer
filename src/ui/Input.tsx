import { useState } from 'preact/hooks';

export function Input({
  value,
  onInput,
  onBlur,
  placeholder,
  class: className,
}: {
  value: string;
  onInput: (value: string) => void;
  onBlur?: () => void;
  placeholder?: string;
  class?: string;
}) {
  const [focus, setFocus] = useState(false);
  return (
    <div class={`input-div ${className ?? ''} ${focus ? 'active' : ''}`}>
      <input
        class="input-inner"
        type="text"
        value={value}
        placeholder={placeholder}
        onFocus={() => setFocus(true)}
        onBlur={() => {
          setFocus(false);
          onBlur?.();
        }}
        onInput={(e) => onInput((e.target as HTMLInputElement).value)}
      />
    </div>
  );
}
