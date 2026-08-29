const path = require("node:path");
const { defineConfig } = require("@rsbuild/core");
const { pluginBabel } = require("@rsbuild/plugin-babel");
const { pluginSolid } = require("@rsbuild/plugin-solid");

const nodeModules =
  process.env.SOLID_PROBE_NODE_MODULES || path.resolve(__dirname, "node_modules");
const solidVersion = require(path.join(nodeModules, "solid-js/package.json")).version;
const webVersion = require(path.join(nodeModules, "@solidjs/web/package.json")).version;
if (solidVersion !== webVersion || !solidVersion.startsWith("2.")) {
  throw new Error(
    `Solid probe requires one matching Solid 2 runtime; resolved solid-js=${solidVersion}, @solidjs/web=${webVersion}`,
  );
}

module.exports = defineConfig({
  root: __dirname,
  plugins: [
    pluginBabel({ include: /\.(?:jsx|tsx|ts)$/ }),
    pluginSolid({ solid: { moduleName: "@solidjs/web" } }),
  ],
  source: {
    entry: {
      index: path.join(__dirname, "src/index.tsx"),
    },
  },
  resolve: {
    alias: {
      "@solidjs/web$": path.join(nodeModules, "@solidjs/web/dist/web.js"),
      "solid-js$": path.join(nodeModules, "solid-js/dist/solid.js"),
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
