/// <reference types="@rsbuild/core/types" />

declare const process: {
  env: {
    NODE_ENV: 'development' | 'production';
  };
};
