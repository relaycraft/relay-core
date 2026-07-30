# RelayCore Release Pitfalls (v0.9.x)

## 1. Three channels, three failure modes

crates.io, npm, GitHub Release fail independently. Windows needs `shell: bash` for `cargo-retry.sh` / `webui-build.sh`.

## 2. WebUI embed + cargo publish

`embed/webui` is gitignored → `cargo publish --allow-dirty` after `webui-build.sh`.

## 3. Tag before validation

fix → CI green → bump → CI green → preflight → one tag.

## 4. Partial success

v0.9.4: crates.io + npm OK, GitHub Release missing → still "failed" to users.

## 5. Re-run Release only

Use `workflow_dispatch` on `release.yml`; do not move tag (re-triggers npm/crates).

## 6. Registry vs Git

npm deprecate / crates yank; cannot erase version numbers.

See full notes in conversation or expand this file locally as needed.
