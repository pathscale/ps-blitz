# Blitz native page backend

This unpublished workspace crate owns the ekojit backend and its native page
witness, using the sibling `ekojit` checkout. `ps-blitz-script` supplies the
public `ScriptEngine` execution seam,
`ScriptDocumentBackend` document seam, and `ApplicationDocument` selector.
Select `ScriptBackend::Custom` on `ApplicationDocument<EkojitDocument>` to use
the native backend; `ScriptBackend::default()` selects Boa before bootstrap.

Build the linked witness from the workspace root:

```sh
cargo build -p ps-blitz-script-ekojit --features ekojit-page-witness --bin native-page-host -j 4
```

Pass the resulting `native-page-host` binary to ps-qa's `native-page` command
with `--page qa/ekojit-page/index.html --checks qa/ekojit-page/checks.ron
--engine ekojit --report <output.json>`. The existing
`--plant host-text-append-noop` option must fail only `ekojit-text-append`.
The host also accepts `--engine boa`.
