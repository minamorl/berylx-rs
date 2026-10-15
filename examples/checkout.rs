//! A checkout saga: the payment goes through, the shipping does not, and the
//! compensation can see that a refund is genuinely owed.
//!
//! Run with `cargo run --example checkout`.

// A saga's state really is a row of "did this step happen yet" flags; that is
// the shape being illustrated, so the usual advice to fold them into an enum
// would obscure the example rather than improve it.
#![allow(clippy::struct_excessive_bools)]

use berylx::{branch, task, to_mermaid, Conflict, Error, Merge, Node, Outcome, Root};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Checkout {
    charged_cents: u32,
    refunded_cents: u32,
    reserved: bool,
    invoiced: bool,
    shipped: bool,
    notified: bool,
}

impl Merge for Checkout {
    /// The two parallel branches touch disjoint fields, so combining them is
    /// taking whichever side actually did something.
    fn merge(_base: &Self, left: Self, right: Self) -> Result<Self, Conflict> {
        Ok(Checkout {
            charged_cents: left.charged_cents.max(right.charged_cents),
            refunded_cents: left.refunded_cents.max(right.refunded_cents),
            reserved: left.reserved || right.reserved,
            invoiced: left.invoiced || right.invoiced,
            shipped: left.shipped || right.shipped,
            notified: left.notified || right.notified,
        })
    }
}

fn workflow() -> Node<Checkout> {
    let charge = task("charge", |mut c: Checkout| {
        c.charged_cents = 4_200;
        Outcome::Ok(c)
    });

    let reserve = task("reserve_stock", |mut c: Checkout| {
        c.reserved = true;
        Outcome::Ok(c)
    });

    let invoice = task("issue_invoice", |mut c: Checkout| {
        c.invoiced = true;
        Outcome::Ok(c)
    });

    // This is the step that fails.
    let ship = task("ship", |c: Checkout| {
        Outcome::Err(c, Error::new("no_courier", "no courier serves this address"))
    });

    let notify = task("notify", |mut c: Checkout| {
        c.notified = true;
        Outcome::Ok(c)
    });

    charge
        .then(reserve.par(invoice))
        .then(ship)
        .rescue_with("compensate", |mut c: Checkout, error| {
            // The state here is what the run actually reached. We are not
            // guessing that a charge happened — we can see the amount.
            println!("  compensating after {}: {}", error.code(), error.message());
            println!("  route was {:?}", error.trace());

            if c.charged_cents > 0 && c.refunded_cents == 0 {
                println!("  refunding {} cents that were really taken", c.charged_cents);
                c.refunded_cents = c.charged_cents;
            }
            if c.reserved {
                println!("  releasing the stock reservation");
                c.reserved = false;
            }
            Outcome::Ok(c)
        })
        .then(
            branch()
                .when("refunded", |c: &Checkout| c.refunded_cents > 0, notify.clone())
                .otherwise(notify),
        )
}

fn main() {
    let workflow = workflow();

    println!("the workflow, before running any of it:\n");
    println!("{}", to_mermaid(&workflow));

    println!("running:");
    let mut root = Root::new(Checkout::default());
    let outcome = root.run(&workflow);

    println!("\noutcome ok? {}", outcome.is_ok());
    println!("committed state: {:?}", root.state());

    assert!(outcome.is_ok(), "the saga recovered, so it committed");
    assert_eq!(root.state().refunded_cents, 4_200);
    assert!(!root.state().reserved);
    assert!(root.state().notified);
}
