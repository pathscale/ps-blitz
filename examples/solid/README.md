# Solid probe

This probe uses the same Rsbuild Babel plugins and Solid 2 runtime as AgencyZero. With
AgencyZero's dependencies already installed, build it without another package installation:

```sh
cd ~/code/ps-blitz
SOLID_PROBE_NODE_MODULES=~/code/agencyzero/apps/gui/frontend/node_modules \
NODE_PATH=~/code/agencyzero/apps/gui/frontend/node_modules \
node ~/code/agencyzero/apps/gui/frontend/node_modules/@rsbuild/core/bin/rsbuild.js \
  build --config examples/solid/rsbuild.config.cjs
```

The build rejects mismatched `solid-js` and `@solidjs/web` versions. For an isolated
run, `bun install --no-save` resolves both from their shared `next` release channel
without creating a project lockfile.

The bundle is written to `target/solid-probe`. Run the probe with:

```sh
cargo test -p ps-blitz-script --features debug-control --test debug_control
```
