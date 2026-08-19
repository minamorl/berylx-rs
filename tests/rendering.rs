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

#[test]
fn the_same_task_used_twice_is_two_vertices() {
    // Reusing a task is legitimate — those are two steps in the workflow, and
    // a graph that collapses them into one vertex is lying about the shape.
    let notify = step("notify");
    let workflow = notify.clone().then(step("middle")).then(notify);

    let dot = to_dot(&workflow);
    assert_eq!(
        dot.matches("label=\"notify\"").count(),
        2,
        "DOT collapsed the repeated task:\n{dot}"
    );

    let mermaid = to_mermaid(&workflow);
    assert_eq!(
        mermaid.matches("\"notify\"").count(),
        2,
        "Mermaid collapsed the repeated task:\n{mermaid}"
    );
}

#[test]
fn both_dialects_identify_vertices_per_occurrence() {
    let workflow = step("a").then(step("b"));

    let dot = to_dot(&workflow);
    let mermaid = to_mermaid(&workflow);

    // Same identifier scheme in both dialects: sequential, never name-derived.
    assert!(dot.contains("n1 -> n2"), "{dot}");
    assert!(mermaid.contains("n1 --> n2"), "{mermaid}");
}

#[test]
fn a_quote_in_a_task_name_does_not_break_the_output() {
    let odd: Node<Trail> = task("say \"hi\"", |t: Trail| Outcome::Ok(t));
    let workflow = step("before").then(odd);

    let dot = to_dot(&workflow);
    let mermaid = to_mermaid(&workflow);

    assert!(dot.contains(r#"label="say \"hi\"""#), "unescaped quote in DOT:\n{dot}");
    assert!(mermaid.contains(r#""say \"hi\"""#), "unescaped quote in Mermaid:\n{mermaid}");
}
