/// <reference types="@rsbuild/core/types" />

declare module '*.vue' {
  import type { DefineComponent } from 'vue';

  // biome-ignore lint/complexity/noBannedTypes: reason
  const component: DefineComponent<{}, {}, any>;
  export default component;
}

// process.env.NODE_ENV is defined by the environment
declare const process: {
  env: {
    NODE_ENV: 'development' | 'production';
  };
};
