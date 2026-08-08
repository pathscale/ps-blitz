# Solid probe

This probe uses the same Rsbuild Babel and Solid plugins as AgencyZero. With AgencyZero's
dependencies already installed, build it without another package installation:

```sh
cd ~/code/blitz-rust
SOLID_PROBE_NODE_MODULES=~/code/agencyzero/apps/gui/frontend/node_modules \
NODE_PATH=~/code/agencyzero/apps/gui/frontend/node_modules \
node ~/code/agencyzero/apps/gui/frontend/node_modules/@rsbuild/core/bin/rsbuild.js \
  build --config examples/solid/rsbuild.config.cjs
```

The bundle is written to `target/solid-probe`. Run the probe with:

```sh
cargo test -p blitz-script --features system-fonts --test solid
```
