# Releasing WordCraft

> **Last updated:** 2026-10-26 · **Change:** minor (optional compact Linux builds; default release unchanged).

The canonical recipe is [craftrules `release/playbook.md`](https://github.com/storytold/craftrules/blob/main/release/playbook.md);
this file has the WordCraft specifics.

1. Bump the version on `main`: `cargo xtask version set 0.2.0` (the one source of truth is
   `[workspace.package] version`; the app shows it in `--version` and About, with the commit from
   `WORDCRAFT_BUILD_SHA`).
2. Merge `main` into `release` and push. `.github/workflows/release.yml` builds:

| Platform | Artifacts | Signing |
|---|---|---|
| macOS | universal `WordCraft.app` in a `.dmg`, universal CLI zip | Developer ID + notarization (`APPLE_*` secrets) |
| Windows | x64, x86 and arm64 `.msi` (one dual-scope package: per-user by default, needs no admin rights; "Everyone on this computer" installs per machine) + portable `.zip` | Azure Trusted Signing (`AZURE_*` secrets) |
| Linux | x86_64 and aarch64 `.AppImage` (+ `.zsync` for AppImageUpdate), `.flatpak` bundle, `.deb`, `.rpm`, `.tar.gz`; Flathub manifest | checksums |
| Linux riscv64 | `.tar.gz` (cross-compiled on `ubuntu-24.04`, glibc >= 2.39; CLI smoke-tested under QEMU) | checksums |
| FreeBSD | x86_64 `.tar.gz` | checksums |
| Web | `wordcraft-web-<version>.zip` (static site: `index.html`, wasm, glue) | none |

3. The run drafts a GitHub Release `WordCraft v<version>` with `SHA256SUMS.txt`; review and publish it.

Signing secrets live in the repository's `release` environment (deployment branch: `release` only),
synced from the release manager's machine (`craftrules/release/signing-setup.md`). Missing secrets
produce unsigned builds with a warning, so forks still build.

Local packaging: `packaging/macos/package.sh --arch universal`, `packaging/windows/package.ps1`,
`packaging/linux/package.sh` (then `packaging/linux/flatpak-bundle.sh` for the Flatpak), `packaging/web/package.sh`.

Dry run: `gh workflow run release.yml --ref <branch>` builds the unsigned jobs (version, Linux,
Linux riscv64, Flatpak, FreeBSD, web) on any branch; the signing jobs are refused there by the `release`
environment's branch rule, so no draft release is made.

## Optional compact builds

`release` remains the speed-oriented default. To compare a size-oriented desktop/CLI build:

```sh
DIST="$PWD/dist/compact" packaging/linux/package.sh --profile compact --formats rpm
```

This uses the `compact` Cargo profile: size optimization (`opt-level = "s"`), one codegen unit,
thin LTO and stripped symbols. Panic unwinding and the session recovery guard remain enabled;
no file formats, fonts or assets are removed. Size optimization can reduce runtime performance,
so compare representative documents before choosing it for distribution. Use a separate `DIST`
because both profiles intentionally produce the same package filenames. `--skip-build` uses
the selected profile's existing binaries; do not use it for a fresh comparison.

Measured on Fedora 44 x86_64, 2026-10-26, from the same working tree (both packages stripped):

| Artifact | Release (bytes) | Compact (bytes) | Reduction |
|---|---:|---:|---:|
| GUI binary | 77,627,432 | 54,985,360 | 29.2% |
| CLI binary | 37,583,056 | 27,737,328 | 26.2% |
| RPM | 50,333,270 | 36,263,505 | 28.0% |

A synthetic sample-to-PDF smoke check succeeds in both profiles and yields identical extracted
text. Short timings collected during concurrent compilation are not a performance benchmark;
large-document latency and native UI performance still need representative measurement. Neither
this comparison nor the compact profile changes the installed application automatically.

`target/` is a rebuildable compilation cache, not part of the installed app or RPM. Debug
incremental artifacts account for much of its size. Cleaning them trades disk space for slower
rebuilds; never clean while another build is using the directory. No automatic cleanup is performed.

## Revision history

| Date | Change | Summary |
|---|---|---|
| 2026-10-26 | minor | Optional compact profile and Linux packaging flag; document cache/package distinction and tradeoffs. |
