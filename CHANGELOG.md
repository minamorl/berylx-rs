# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- `DryRun::plan(&workflow, state)` dry-runs one workflow and returns the plan of
  that run alone, so nothing an earlier run reached carries over.
- `Conflict`, which a merge returns when two parallel branches changed the
  state in ways that cannot both stand.
- `keep_left` and `keep_right`, reducers that work for any state type. Neither
  is a default; pass one to `Node::par_with` to choose it.

### Changed

- **Breaking:** merging parallel branches is now a three-way join that can
  fail. `Merge::merge` is `fn merge(base: &Self, left: Self, right: Self) ->
  Result<Self, Conflict>`, and the closure given to `Node::par_with` has the
  same shape. Every call receives the state all branches started from, still
  as a binary left fold in the order the branches were written.
- A merge conflict fails the parallel node with the code `"merge_conflict"`,
  carrying the state all branches started from. Under `OnErr::Accumulate` the
  branch failures are still attached as `parallel_errors`.
- `Root::run` documents that it commits once per run, after the whole workflow
  has succeeded, and that neither a commit nor a failure undoes side effects
  outside the state.

## [0.1.0] - 2026-08-20

First release. A redesign of the [Ruby berylx gem](https://github.com/minamorl/berylx)
for Rust rather than a transliteration of it.

### Added

- `Outcome<S>` — the result envelope whose failure variant carries the state the
  run reached, making `Outcome::state()` total.
- `Error` — a structured failure with a machine-readable code, the failing node,
  the route taken to reach it, sibling failures and free-form metadata.
- `Node<S>` — a composed workflow as an inspectable value, built with `.then()`,
  `.par()` / `.par_with()`, `branch()` and `.rescue_with()`.
- `Merge` — the trait that says what combining two parallel states means.
- `Root<S>` — the single owner of committed state, which advances only on
  success.
- `Interpreter` with `Real`, `DryRun`, `Audit` and `Retry`, so aspects are added
  by wrapping rather than by editing a workflow. `Retry` backs off exponentially
  with jitter by default; `Retry::without_backoff` retries immediately.
- `to_dot` / `to_mermaid` — rendering a workflow without running it.
- Zero dependencies, including for concurrency (`std::thread::scope`).

### Deliberately not carried over from the Ruby gem

- `Lay` / `Focus` path navigation — your own `S` is the state, since Rust has
  struct update syntax.
- The three-way `strict` merge, which needs dynamic hash keys.
- The darkcore Freer effect substrate — the workflow tree is already the IR.
- `async`, because `async fn` in traits is not `dyn`-safe.

[0.1.0]: https://github.com/minamorl/berylx-rs/releases/tag/v0.1.0
