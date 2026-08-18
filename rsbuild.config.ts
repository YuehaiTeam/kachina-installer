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
    inlineScripts: true,
    inlineStyles: true,
    legalComments: 'none',
    dataUriLimit: Number.MAX_SAFE_INTEGER,
  },
  html: {
    title: 'Kachina Installer',
    inject: 'body',
  },
  performance: {
    chunkSplit: {
      strategy: 'all-in-one',
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
      module: {
        parser: {
          javascript: {
            dynamicImportMode: 'eager',
          },
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
