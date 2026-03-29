#!/usr/bin/env bash
# Clone benchmark repositories at pinned release tags.
# Re-running is safe — already-cloned repos are left untouched.
#
# Usage:
#   bash bench/setup.sh
set -euo pipefail

DIR="$(cd "$(dirname "$0")" && pwd)"
REPOS="$DIR/repos"
mkdir -p "$REPOS"

clone_if_missing() {
    local name="$1" url="$2" tag="$3"
    local dest="$REPOS/$name"

    if [ -d "$dest/.git" ]; then
        echo "  $name: already present ($(git -C "$dest" rev-parse --short HEAD))"
        return
    fi

    echo "  $name: cloning $url @ $tag ..."
    git clone --depth 1 --branch "$tag" "$url" "$dest"
    echo "  $name: done ($(git -C "$dest" rev-parse HEAD))"
}

echo "Setting up benchmark repositories in $REPOS/ ..."
echo ""
# Rust baseline: medium-sized, well-structured, widely known
clone_if_missing ripgrep https://github.com/BurntSushi/ripgrep.git 14.1.1
# Python baseline: modern codebase with extensive type hints and decorators
clone_if_missing fastapi  https://github.com/fastapi/fastapi.git    0.115.6
# TypeScript baseline: web framework, exercises all TS symbol kinds
clone_if_missing hono     https://github.com/honojs/hono.git        v4.7.4
# C baseline: heavy macro/typedef/struct usage, large file count
clone_if_missing redis    https://github.com/redis/redis.git         7.4.2
# C++ baseline: clean idiomatic C++ with abstract classes and namespaces
clone_if_missing leveldb  https://github.com/google/leveldb.git      1.23
echo ""
echo "Done. Run benchmarks with:"
echo "  cargo bench --bench indexing"
echo "  cargo bench --bench queries"
echo "  cargo run --bin memory_bench -- bench/repos/ripgrep"
echo "  cargo run --bin memory_bench -- bench/repos/fastapi"
echo "  cargo run --bin memory_bench -- bench/repos/hono"
echo "  cargo run --bin memory_bench -- bench/repos/redis"
echo "  cargo run --bin memory_bench -- bench/repos/leveldb"
