# berylx

**Workflows whose failures carry the state they reached.**

[![CI](https://github.com/minamorl/berylx-rs/actions/workflows/ci.yml/badge.svg)](https://github.com/minamorl/berylx-rs/actions/workflows/ci.yml)
[![Rust 1.70+](https://img.shields.io/badge/rust-1.70%2B-orange.svg)](https://www.rust-lang.org/)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

A multi-step business process fails halfway through. `Result<T, E>` throws away
the `T` at that moment — so the code that has to clean up receives an error and
nothing else, and is left to infer what already happened from local variables
that are no longer in scope.

Berylx changes exactly one thing:

```text
Task : S -> Outcome<S>          where  Outcome<S> = Ok(S) | Err(S, Error)
```

The failure variant carries the state too. `Outcome::state()` is **total**:
there is no result of a berylx workflow from which the state is missing.
Compensation is written against data you have.

```rust
use berylx::{task, Error, Outcome, Root};

#[derive(Clone, Debug)]
struct Checkout { charged: bool, refunded: bool, notified: bool }

let charge = task("charge", |mut c: Checkout| {
    c.charged = true;                       // the money did move
    Outcome::Err(c, Error::new("shipping_unavailable", "no courier"))
});

let workflow = charge
    .rescue_with("refund", |mut c: Checkout, _e| {
        assert!(c.charged);                 // <- the point: this is real data,
        c.refunded = true;                  //    not a guess about what ran
        Outcome::Ok(c)
    })
    .then(task("notify", |mut c: Checkout| { c.notified = true; Outcome::Ok(c) }));

let mut root = Root::new(Checkout { charged: false, refunded: false, notified: false });
let outcome = root.run(&workflow);

assert!(outcome.is_ok());
assert!(root.state().refunded);
```

Without the `rescue_with`, the run returns `Err` with `charged: true` in its
state while `root.state()` stays where it was — the failure state remains
available as data without silently becoming committed application state.

## The rest of the crate exists to make that compositional

```rust
use berylx::{branch, run, task, Merge, Outcome};

let workflow = task("reserve", reserve)
    .par(task("invoice", invoice))          // concurrently, then merged
    .then(
        branch()
            .when("domestic", |o: &Order| o.domestic, task("ship_local", ship_local))
            .otherwise(task("ship_intl", ship_intl)),
    )
    .rescue_with("cancel", cancel_everything);
```

- **One `Root`** owns committed state for an application boundary. It advances
  only when a run succeeds.
- **Named tasks** are the unit of composition, so failures can say _where_ they
  happened. `Error` carries a machine-readable code, the failing node, the route
  taken to reach it, sibling failures, and free-form metadata.
- **The composed workflow is a value.** `Node<S>` is a tree you can inspect,
  count, and render as Graphviz DOT or a Mermaid flowchart — without running any
  of it.

## Combinators are methods, not operators

The Ruby and TypeScript versions of berylx spell sequencing `>>` and parallel
`&`. This crate does not, and the reason is a rule rather than a taste.

Rust's [API guidelines][overload] ask that an overloaded operator

> bears some resemblance to \[the original\] **and shares the expected
> properties, e.g. associativity**

Berylx's parallel composition is _not_ commutative — branches fold in written
order, so swapping them swaps the result. Spelling it `&` would be a promise the
crate cannot keep. So: `.then()`, `.par()`, `.rescue_with()`.

## Parallel branches are concurrent but deterministic

Branches run on real threads, at the same time. Their results are folded in the
order they were **written**, never the order they finished, so a workflow that
succeeds does so reproducibly on a loaded machine.

```rust
// Both fail; `second` fails sooner in wall-clock terms. `first` is still the
// reported failure, because that is the order you wrote them in.
let workflow = slow_failure("first").par(instant_failure("second"));
```

Two failure modes are available, chosen on the node itself rather than by the
interpreter:

| mode                     | behaviour                                                                                                    |
| ------------------------ | ------------------------------------------------------------------------------------------------------------ |
| `ShortCircuit` (default) | report the first failure in written order                                                                    |
| `.accumulate()`          | run every branch, gather all failures into `Error::parallel_errors`, and keep the state every branch reached |

Because merging two states of your own type has no safe default — anything
automatic silently drops one branch's work — you say what it means, either by
implementing `Merge` or by passing a closure to `.par_with()`.

## Retry, dry-run and audit are wrapped on, not written in

The workflow tree says _what_ the work is. An `Interpreter` says what running a
task means. They are separate, so aspects are added without touching the
workflow:

```rust
use berylx::{run_with, Audit, DryRun, Real, Retry};

run_with(&workflow, state, &Real);                     // do it
run_with(&workflow, state, &DryRun::new());            // list the plan, touch nothing
run_with(&workflow, state, &Retry::new(Real, 3));      // retry each task
run_with(&workflow, state, &Audit::new(Real));         // record every task run
```

`Retry` hands each attempt the state the previous one reached — the same
property that makes compensation possible makes retry meaningful.

Interpreters compose by wrapping: `Audit::new(Retry::new(Real, 3))`.

## Install

```toml
[dependencies]
berylx = "0.1"
```

**No dependencies.** Not "few" — zero, including for the concurrency, which uses
`std::thread::scope`.

## What this is, and what it is not a port of

berylx-rs is a redesign for Rust, not a transliteration of the
[Ruby gem](https://github.com/minamorl/berylx). Deliberately **not** carried
over:

- **`Lay` / `Focus`** — the Ruby version navigates a frozen hash by path,
  because Ruby has no immutable partial update. Rust has struct update syntax
  (`S { charged: true, ..s }`), so your own type _is_ the state. You keep
  compile-time field checking, and connections, handles and `Decimal` can live
  in state — none of which survive a `serde_json::Value`.
- **The three-way `strict` merge** — it detects conflicting concurrent writes by
  walking hash keys, which a typed `S` cannot do. `Merge` is yours to define.
- **The darkcore Freer effect substrate** — berylx already requires the workflow
  to be an inspectable tree, and Rust can walk a tree with `enum` and `match`.
  A second interpretive layer would have bought nothing that `Interpreter`
  does not already give.
- **`async`** — `async fn` in traits [is not `dyn`-safe][asyncfn], which is
  incompatible with a workflow being a `Box<dyn>`-shaped value. Sync only in
  0.1.

## License

MIT

[overload]: https://rust-lang.github.io/api-guidelines/predictability.html#c-overload
[asyncfn]: https://blog.rust-lang.org/2023/12/21/async-fn-rpit-in-traits/
