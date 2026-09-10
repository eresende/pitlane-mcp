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
5. Smoke-test the published artifact and refresh your local install — see
   [Post-release verification](#post-release-verification).

## Post-release verification

The release workflow builds from tagged source on five platforms, but a quick
check of what agents will actually download is worth one minute per release:

1. Fetch the pre-built artifact for your platform and unpack it (plain `curl`
   needs no auth; `gh release download vX.Y.Z --pattern 'pitlane-mcp-<os>-<arch>'`
   also works, but only from inside a checkout of this repo):

   ```console
   $ mkdir /tmp/pitlane-check && cd /tmp/pitlane-check
   $ curl -fsSL -O https://github.com/eresende/pitlane-mcp/releases/download/vX.Y.Z/pitlane-mcp-linux-x86_64.tar.gz
   $ tar xzf pitlane-mcp-*.tar.gz
   ```

2. The artifact ships both `pitlane` and `pitlane-mcp`. Check the CLI version:

   ```console
   $ ./pitlane --version
   pitlane vX.Y.Z
   ```

3. Probe the MCP server over stdio: an initialize round-trip plus one
   `tools/call` exercising a behavior new in this release (for v0.15+, omitting
   project-path must return the friendly error, not raw serde noise):

   ```console
   $ printf '%s\n' \
       '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"check","version":"0"}}}' \
       '{"jsonrpc":"2.0","method":"notifications/initialized"}' \
       '{"jsonrpc":"2.0","id":9,"method":"tools/call","params":{"name":"locate_code","arguments":{}}}' | ./pitlane-mcp
   ```

   The `id: 1` response completes the handshake, and for v0.15+ the `id: 9`
   result carries `isError: true` with "Missing required project-path parameter.
   Use either 'project' or 'path'. Example...".

4. Refresh the local install from source so `~/.cargo/bin` — which other agent
   harnesses reference directly — runs what you just shipped, and confirm it:

   ```console
   $ cargo install --path . --locked
   $ command -v pitlane && pitlane --version
   /home/eresende/.cargo/bin/pitlane
   pitlane vX.Y.Z
   ```

Already-running MCP server processes keep serving the previous build until they
are restarted; only new launches pick up the fresh binary.

## Tests

- `cargo test --lib` runs the unit and integration suite; property tests are
  included and run by default.
- Clippy (`--all-targets`) and `cargo fmt --check` must be clean; CI enforces
  both alongside test runs on five platforms.
