import { defineConfig } from '@rsbuild/core';
import { pluginVue } from '@rsbuild/plugin-vue';
import CompressionPlugin from 'compression-webpack-plugin';
import PurgeCSSPlugin from '@fullhuman/postcss-purgecss';

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
  performance: {
    chunkSplit: {
      strategy: 'single-vendor',
    },
  },
  plugins: [pluginVue()],
  tools: {
    bundlerChain: (chain) => {
      // if (process.env.NODE_ENV !== 'development') {
      //   chain.plugin('compress').use(CompressionPlugin, [
      //     {
      //       test: /\.(js|css|svg)$/,
      //       filename: '[path][base].gz',
      //       algorithm: 'gzip',
      //       threshold: 1024,
      //       minRatio: 0.8,
      //       deleteOriginalAssets: true,
      //     },
      //   ]);
      // }
    },
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
          PurgeCSSPlugin({
            safelist: [/^(?!h[1-6]).*$/],
            variables: true,
          }),
        ],
      },
    },
  },
});
