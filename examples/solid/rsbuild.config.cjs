const path = require("node:path");
const { defineConfig } = require("@rsbuild/core");
const { pluginBabel } = require("@rsbuild/plugin-babel");
const { pluginSolid } = require("@rsbuild/plugin-solid");

const nodeModules =
  process.env.SOLID_PROBE_NODE_MODULES || path.resolve(__dirname, "node_modules");

module.exports = defineConfig({
  root: __dirname,
  plugins: [pluginBabel({ include: /\.(?:jsx|tsx|ts)$/ }), pluginSolid()],
  source: {
    entry: {
      index: path.join(__dirname, "src/index.tsx"),
    },
  },
  resolve: {
    alias: {
      "solid-js$": path.join(nodeModules, "solid-js/dist/solid.js"),
      "solid-js/web$": path.join(nodeModules, "solid-js/web/dist/web.js"),
    },
  },
  html: {
    mountId: "app",
    title: "Blitz Solid probe",
  },
  tools: {
    rspack: {
      optimization: {
        splitChunks: false,
        runtimeChunk: false,
      },
    },
  },
  output: {
    distPath: {
      root: path.resolve(__dirname, "../../target/solid-probe"),
    },
    cleanDistPath: true,
    assetPrefix: "./",
    legalComments: "none",
    minify: false,
    sourceMap: {
      js: "source-map",
    },
  },
});
