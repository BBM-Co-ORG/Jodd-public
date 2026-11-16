# Test fixtures for `scripts/updater-manifest.mjs`

**Throwaway, test only.** These files were signed on 2026-11-11 with a key made
for these tests (`tauri signer generate`), whose private half was deleted at
once. `test-only.key.pub` is that key's public half. It is **not** Jodd's
updater key and nothing in a release accepts it.

- `Jodd_*` — stand-ins for the three desktop installers, signed with
  `tauri signer sign --app-version 0.47.0`.
- `other-version.bin` — signed with `--app-version 0.46.0`, for the
  "signature names another version" refusal.
