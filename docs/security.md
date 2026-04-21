# Security

`pitlane-mcp` is a local code-navigation tool. It does not require network calls for normal operation, but it does have filesystem visibility consistent with the privileges of the process running it.

## Filesystem Scope

By default, indexing and file-oriented tools can inspect any supported source file accessible to the current OS user.

If you need confinement, set `PITLANE_ALLOWED_ROOTS` to a platform-native path list:

- `:`-separated on Unix
- `;`-separated on Windows

Example:

```bash
export PITLANE_ALLOWED_ROOTS="/home/alice/src:/home/alice/work"
```

When set, `pitlane-mcp` rejects project paths outside those roots, and file-level tools reject traversal outside the indexed project root.

## Intentional Safety Properties

- Only supported source-file extensions are indexed or read
- Symbolic links are not followed
- Files larger than 1 MiB are skipped
- `index_project` enforces a configurable `max_files` cap to prevent accidental full-filesystem walks

## Index Storage

Indexes are stored under:

```text
~/.pitlane/indexes/{project_hash}/
```

They are stored unencrypted on disk. If another local process can write to your home directory, it can tamper with index files. Deserialization failures are handled as errors and are not intended to execute arbitrary code.

## Practical Recommendation

If you run `pitlane-mcp` in an environment where prompt injection is a concern, treat it as having read access to any supported source file readable by that OS user and configure `PITLANE_ALLOWED_ROOTS` accordingly.
