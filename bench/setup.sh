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
clone_if_missing ripgrep https://github.com/BurntSushi/ripgrep.git  14.1.1
# Python baseline: modern codebase with extensive type hints and decorators
clone_if_missing fastapi  https://github.com/fastapi/fastapi.git    0.115.6
# TypeScript baseline: web framework, exercises all TS symbol kinds
clone_if_missing hono     https://github.com/honojs/hono.git        v4.7.4
# C baseline: heavy macro/typedef/struct usage, large file count
clone_if_missing redis    https://github.com/redis/redis.git        7.4.2
# C++ baseline: clean idiomatic C++ with abstract classes and namespaces
clone_if_missing leveldb  https://github.com/google/leveldb.git     1.23
# Go baseline: popular HTTP web framework, idiomatic Go with structs and interfaces
clone_if_missing gin      https://github.com/gin-gonic/gin.git      v1.10.0
# Java baseline: Google core libraries, rich mix of classes, interfaces, and generics
clone_if_missing guava    https://github.com/google/guava.git       v33.4.8
# Bash baseline: popular Bash testing framework, idiomatic multi-file shell scripts
clone_if_missing bats     https://github.com/bats-core/bats-core.git v1.11.1
# C# baseline: most popular C# JSON library, rich mix of classes, interfaces, generics
clone_if_missing newtonsoft https://github.com/JamesNK/Newtonsoft.Json.git 13.0.3
# Ruby baseline: de-facto Ruby linter, hundreds of cop classes and modules
clone_if_missing rubocop    https://github.com/rubocop/rubocop.git          v1.65.0
# Swift baseline: de-facto Swift linter, hundreds of rule structs and protocol implementations
clone_if_missing swiftlint  https://github.com/realm/SwiftLint.git          0.57.0
# Objective-C baseline: widely-used image loading library, rich mix of classes, protocols, and categories
clone_if_missing sdwebimage https://github.com/SDWebImage/SDWebImage.git    5.19.0
# PHP baseline: most popular PHP framework, rich mix of classes, interfaces, and traits
clone_if_missing laravel    https://github.com/laravel/framework.git        v11.9.2
# Zig baseline: de-facto Zig language server, rich mix of structs, enums, and methods
clone_if_missing zls        https://github.com/zigtools/zls.git             0.13.0
echo ""
echo "Done. Run benchmarks with:"
echo ""
echo "  # Memory, disk, and token efficiency (all repos):"
echo "  cargo run --release --features memory-bench --bin memory_bench"
echo "  # Memory, disk, and token efficiency (one or more repos by name):"
echo "  cargo run --release --features memory-bench --bin memory_bench -- hono"
echo "  cargo run --release --features memory-bench --bin memory_bench -- ripgrep fastapi"
echo ""
echo "  # Indexing throughput — Criterion (all repos):"
echo "  cargo bench --bench indexing"
echo "  # Indexing throughput — Criterion (one or more repos):"
echo "  cargo bench --bench indexing -- hono"
echo "  cargo bench --bench indexing -- \"ripgrep|gin\""
echo ""
echo "  # Query latency — Criterion (all repos):"
echo "  cargo bench --bench queries"
echo "  # Query latency — Criterion (one or more repos):"
echo "  cargo bench --bench queries -- hono"
echo "  cargo bench --bench queries -- \"ripgrep|gin\""
