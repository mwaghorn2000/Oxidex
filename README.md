# Oxidex

Oxidex is a zero-copy search engine implemented in Rust. The project showcases an end-to-end information retrieval stack—from document ingestion and indexing to a custom query language and BM25 ranking—while emphasizing systems-level efficiency and ergonomics.

## Why Oxidex is different

Most student IR projects simply wrap an off-the-shelf index or expose a thin REST API over an existing library. Oxidex is meant to be an educational yet production-grade search stack that highlights three uncommon aspects:

1. **Zero-copy indexing and retrieval** – Uses memory-mapped files and cache-friendly data structures so search execution works directly over on-disk postings without redundant heap allocations.
2. **Custom query language** – Ships with its own parser, AST, and optimizer to express Boolean, phrase, proximity, and weighted field queries in a single declarative syntax.
3. **IR-focused ranking** – Implements classical and modern IR scoring functions (BM25, pivoted length normalization, field boosting) with hooks for SIMD token statistics.

## Core components

| Component | Description |
|-----------|-------------|
| Tokenizer | SIMD-accelerated tokenizer that emits normalized terms, positions, and optional field IDs. |
| Indexer | Builds an inverted index backed by zero-copy postings lists and field-length metadata stored in memory-mapped segments. |
| Query parser | Pratt-parser that produces a typed AST for the Oxidex Query Language (OQL). |
| Planner & executor | Rewrites AST into iterator pipelines, leveraging skip pointers, tiered indexes, and roaring bitmaps. |
| Ranker | Provides BM25 + pluggable scorers with term-frequency caching. |
| API layer | Simple gRPC/JSON endpoint plus CLI tooling for indexing and querying. |

## Oxidex Query Language (OQL)

Example query:

```
title:"zero copy" & body:(rust | "bm25") -tag:draft @boost(1.5)
```

Features include:

- Field qualifiers (`title:`, `body:`) with default analyzers.
- Phrase (`"zero copy"`) and proximity operators (`NEAR/5`).
- Boolean operators (`&`, `|`, `-`).
- Boost modifiers (`@boost(1.5)`) and per-clause scoring weights.
- Macros for reusable query snippets (planned).

## Data layout & zero-copy strategy

- **Segmented store:** Documents are chunked into immutable segments containing postings, lexicon blocks, and document vectors.
- **Memory mapping:** Segments rely on `memmap2` and borrow-checked slices to access postings without copying.
- **Cache-aware postings:** Variable-byte encoded docIDs with skip blocks sized for page boundaries; optionally compressed using SIMD-BP128.

## Ranking pipeline

1. Collect top-K candidates per query clause via document-at-a-time iteration.
2. Compute BM25 scores using cached term statistics from the lexicon.
3. Combine clause scores using boost weights and field-specific normalization.
4. Optionally rerank the top candidates via lightweight embedding similarity.

## Roadmap

1. **Milestone 0:** Scaffold crate, define data schemas, and implement CLI skeleton.
2. **Milestone 1:** SIMD tokenizer + document ingester writing raw postings.
3. **Milestone 2:** Zero-copy postings reader, lexicon, and BM25 scorer.
4. **Milestone 3:** Full OQL parser, planner, and executor.
5. **Milestone 4:** API server, CLI, and evaluation scripts (TREC-style).

## Development setup

- Rust 1.77+ (MSRV TBD)
- `cargo fmt`, `cargo clippy`, `cargo test` for validation
- Optional: `criterion` for benchmarking query execution

## License

TBD (likely Apache-2.0 or MIT).
