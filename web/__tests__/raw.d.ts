// Vite import forms used by the test setup (the `vite/client` types are not
// hoisted by pnpm, so the two we need are declared here).
declare module '*?raw' {
  const text: string;
  export default text;
}

interface ImportMeta {
  glob(
    pattern: string,
    options: { query: string; import: 'default'; eager: true },
  ): Record<string, string>;
}
