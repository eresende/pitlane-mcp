# Development Notes

Practical notes for building, testing, and releasing `pitlane-mcp` locally.

## Stale binaries after `cargo publish`

`cargo publish` (including `--dry-run` and `cargo package`) compiles the crate
from the **packaged snapshot** in `target/package/pitlane-mcp-<version>/`, and
the resulting fingerprint can poison subsequent local builds: `cargo build`
tracks the *packaged* copies of the sources instead of the workspace `src/`,
reports `Finished` instantly, and silently keeps serving an old binary. The
giveaway is a dep-info file pointing into `target/package/`:

```console
$ head -1 target/debug/pitlane.d
/home/eresende/projects/pitlane-mcp/target/package/pitlane-mcp-0.13.1/src/bin/pitlane.rs ...
```

If your changes "are not taking effect" after running `cargo publish`, clean
the package artifacts and rebuild:

```console
$ cargo clean -p pitlane-mcp
$ cargo build
```

Verify the rebuilt binary actually contains your change before drawing
conclusions from end-to-end runs (`pitlane --version`, or check a behaviour
that only exists in your working tree).

## Releasing

1. Bump `version` in `Cargo.toml` (and `Cargo.lock` via a build) and commit to `main`.
2. Push a tag `vX.Y.Z` — `.github/workflows/release.yml` builds five platform
   binaries, creates the GitHub release, and updates the Homebrew tap.
3. Replace the generated release notes with hand-written notes matching the
   format of previous releases (title with hook, intro paragraph, themed
   sections, validation, compare link).
4. `cargo publish` to crates.io. Note the quirk above if you keep building
   locally afterwards.

## Tests

- `cargo test --lib` runs the unit and integration suite; property tests are
  included and run by default.
- Clippy (`--all-targets`) and `cargo fmt --check` must be clean; CI enforces
  both alongside test runs on five platforms.
