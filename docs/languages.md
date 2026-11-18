# Languages and Symbol Kinds

This page describes the language coverage exposed by `pitlane-mcp`.

## Supported Languages

| Language | Extensions | Symbol kinds |
|---|---|---|
| Rust | `.rs` | function, method, struct, enum, trait, impl, mod, macro, const, type alias |
| Python | `.py` | function, method, class |
| JavaScript | `.js`, `.jsx`, `.mjs`, `.cjs` | function, class, method |
| TypeScript | `.ts`, `.tsx`, `.mts`, `.cts` | function, class, method, interface, type alias, enum |
| Svelte | `.svelte` | function, class, method, interface, type alias, enum |
| C | `.c`, `.h` (header language resolved by project evidence) | function, struct, enum, type alias, macro |
| C++ | `.cpp`, `.cc`, `.cxx`, `.hpp`, `.hxx`, `.hh`, `.ipp`, `.tpp`, `.inl` | function, method, class, struct, enum, type alias, macro |
| Go | `.go` | function, method, struct, interface, type alias |
| Java | `.java` | class, interface, enum, method |
| C# | `.cs` | class, struct, interface, enum, method, type alias |
| Bash | `.sh`, `.bash` | function |
| Ruby | `.rb` | class, module, method |
| Swift | `.swift` | class, struct, enum, protocol, method, function, init, type alias |
| Objective-C | `.m`, `.mm` | class, protocol, method, function, type alias |
| PHP | `.php` | class, interface, enum, method, function |
| Zig | `.zig` | function, method, struct, enum, const |
| Lua | `.luau`, `.lua` | function, method, type alias |
| Kotlin | `.kt`, `.kts` | class, interface, enum, object, function, method, type alias |
| Solidity | `.sol` | contract, interface, library, function, method, modifier, constructor, event, error, struct, enum |

## Notes

- TypeScript declaration files (`.d.ts`, `.d.mts`, `.d.cts`) are skipped automatically.
- C/C++ headers: `.h` is language-neutral. It is classified as C++ when an eligible project file has a C++ extension (`.cpp`, `.cc`, `.cxx`, `.hpp`, `.hxx`, `.hh`, `.ipp`, `.tpp`, `.inl`, `.cu`, `.cuh`) anywhere in the tree, and as C otherwise; with no evidence it stays C. The indexer applies the file-size limit, built-in exclusions, project ignore rules and configured exclude patterns before choosing the header language. The `language` filter follows the same rule, so `language=cpp` includes `.h` in a C++ project and `language=c` includes it in a C project. Pure C projects keep C header classification.
- The rule is applied consistently by indexing, incremental re-indexing (adding the first or removing the last C++ source reclassifies existing headers), and the `language` filters. Persisted indexes record the rule version and are invalidated when it changes.
- CUDA: `.cu`/`.cuh` are indexed syntactically as C++ only. This is not CUDA semantic support — no device/host distinction, kernel launch syntax or CUDA headers are understood.
- Svelte indexing covers embedded `<script>` and `<script lang="ts">` blocks only. Template and style sections are not indexed.
- Coverage is intentionally symbol-oriented rather than full semantic compilation. The goal is practical navigation for agents, not a full compiler front-end.
