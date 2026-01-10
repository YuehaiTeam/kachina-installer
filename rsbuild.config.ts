import { defineConfig } from '@rsbuild/core';
import { pluginVue } from '@rsbuild/plugin-vue';
import { purgeCSSPlugin } from '@fullhuman/postcss-purgecss';

export default defineConfig({
  server: {
    port: 1420,
  },
  source: {
    define: {
      'process.env.NODE_ENV': JSON.stringify(process.env.NODE_ENV),
    },
  },
  output: {
    overrideBrowserslist: ['edge >= 100'],
  },
  html: {
    title: 'Kachina Installer',
  },
  performance: {
    chunkSplit: {
      strategy: 'single-vendor',
    },
  },
  plugins: [pluginVue()],
  tools: {
    rspack: {
      experiments: {
        rspackFuture: {
          bundlerInfo: { force: false },
        },
      },
    },
    postcss: {
      postcssOptions: {
        plugins: [
          purgeCSSPlugin({
            safelist: [/^(?!h[1-6]).*$/],
            variables: true,
          }),
        ],
      },
    },
  },
});
