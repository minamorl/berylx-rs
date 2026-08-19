//! A workflow can be rendered without being run.

mod common;

use berylx::{branch, task, to_dot, to_mermaid, Node, Outcome};
use common::Trail;

fn step(name: &'static str) -> Node<Trail> {
    task(name, move |mut t: Trail| {
        t.push(name);
        Outcome::Ok(t)
    })
}

fn sample() -> Node<Trail> {
    step("charge")
        .then(step("reserve").par(step("invoice")))
        .then(
            branch()
                .when("premium", |t: &Trail| t.0.len() > 2, step("gift"))
                .otherwise(step("plain")),
        )
        .rescue_with("refund", |t: Trail, e| Outcome::Err(t, e))
}

#[test]
fn dot_contains_every_task_and_the_recovery_boundary() {
    let dot = to_dot(&sample());

    for name in ["charge", "reserve", "invoice", "gift", "plain", "refund"] {
        assert!(dot.contains(name), "DOT output is missing {name}:\n{dot}");
    }
    assert!(dot.starts_with("digraph berylx {"));
    assert!(dot.contains("style=dashed"), "the rescue boundary should be dashed");
}

#[test]
fn dot_labels_the_branch_arms() {
    let dot = to_dot(&sample());
    assert!(dot.contains("label=\"premium\""));
    assert!(dot.contains("label=\"otherwise\""));
}

#[test]
fn mermaid_is_a_flowchart_naming_every_task() {
    let mermaid = to_mermaid(&sample());

    assert!(mermaid.starts_with("flowchart TD"));
    for name in ["charge", "reserve", "invoice", "gift", "plain", "refund"] {
        assert!(mermaid.contains(name), "Mermaid output is missing {name}:\n{mermaid}");
    }
    assert!(mermaid.contains("-->|premium|"));
}

#[test]
fn rendering_runs_nothing() {
    // Rendering a workflow whose tasks would panic proves the tasks are not
    // touched: the tree alone carries the shape.
    let exploding: Node<Trail> = task("explode", |_: Trail| panic!("must not run"));
    let workflow = step("safe").then(exploding);

    let dot = to_dot(&workflow);
    assert!(dot.contains("explode"));
}

#[test]
fn tasks_are_listed_in_written_order() {
    let workflow = sample();
    let names: Vec<&str> = workflow.tasks().iter().map(|t| t.name()).collect();
    assert_eq!(names, ["charge", "reserve", "invoice", "gift", "plain"]);
}
