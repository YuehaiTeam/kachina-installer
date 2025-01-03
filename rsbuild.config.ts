import { defineConfig } from '@rsbuild/core';
import { pluginVue } from '@rsbuild/plugin-vue';

export default defineConfig({
  server: {
    port: 1420,
  },
  plugins: [pluginVue()],
});
