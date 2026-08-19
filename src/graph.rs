//! Rendering a workflow without running it.

use std::fmt::Write as _;

use crate::node::Node;

/// Renders a workflow as Graphviz DOT.
///
/// ```
/// use berylx::{task, to_dot, Outcome};
///
/// let workflow = task("a", Outcome::Ok).then(task("b", Outcome::<i32>::Ok));
/// assert!(to_dot(&workflow).contains("\"a\" -> \"b\""));
/// ```
#[must_use]
pub fn to_dot<S>(node: &Node<S>) -> String {
    let mut builder = Builder::new(Dialect::Dot);
    builder.walk(node, &[]);
    let body = builder.finish();
    format!("digraph berylx {{\n  rankdir=LR;\n{body}}}\n")
}

/// Renders a workflow as a Mermaid flowchart.
///
/// ```
/// use berylx::{task, to_mermaid, Outcome};
///
/// let workflow = task("a", Outcome::Ok).then(task("b", Outcome::<i32>::Ok));
/// assert!(to_mermaid(&workflow).starts_with("flowchart TD"));
/// ```
#[must_use]
pub fn to_mermaid<S>(node: &Node<S>) -> String {
    let mut builder = Builder::new(Dialect::Mermaid);
    builder.walk(node, &[]);
    let body = builder.finish();
    format!("flowchart TD\n{body}")
}

#[derive(Clone, Copy)]
enum Dialect {
    Dot,
    Mermaid,
}

struct Builder {
    dialect: Dialect,
    lines: Vec<String>,
    next: usize,
}

/// One emitted vertex: the identifier it was given, and the label it shows.
struct Vertex {
    id: String,
    label: String,
}

impl Builder {
    fn new(dialect: Dialect) -> Self {
        Self { dialect, lines: Vec::new(), next: 0 }
    }

    fn finish(self) -> String {
        self.lines.iter().fold(String::new(), |mut acc, line| {
            let _ = writeln!(acc, "  {line}");
            acc
        })
    }

    fn vertex(&mut self, label: &str, shape: Shape) -> Vertex {
        self.next += 1;
        let id = format!("n{}", self.next);
        match self.dialect {
            Dialect::Dot => {
                let attrs = match shape {
                    Shape::Task => "shape=box",
                    Shape::Control => "shape=diamond",
                    Shape::Recovery => "shape=box, style=dashed",
                };
                self.lines.push(format!("\"{label}\" [{attrs}];"));
                Vertex { id: format!("\"{label}\""), label: label.to_owned() }
            }
            Dialect::Mermaid => {
                let body = match shape {
                    Shape::Task => format!("{id}[\"{label}\"]"),
                    Shape::Control => format!("{id}{{\"{label}\"}}"),
                    Shape::Recovery => format!("{id}(\"{label}\")"),
                };
                self.lines.push(body);
                Vertex { id, label: label.to_owned() }
            }
        }
    }

    fn edge(&mut self, from: &Vertex, to: &Vertex, label: Option<&str>) {
        let line = match (self.dialect, label) {
            (Dialect::Dot, None) => format!("{} -> {};", from.id, to.id),
            (Dialect::Dot, Some(label)) => {
                format!("{} -> {} [label=\"{label}\"];", from.id, to.id)
            }
            (Dialect::Mermaid, None) => format!("{} --> {}", from.id, to.id),
            (Dialect::Mermaid, Some(label)) => {
                format!("{} -->|{}| {}", from.id, label, to.id)
            }
        };
        self.lines.push(line);
    }

    /// Emits `node`, wiring it after every vertex in `entries`, and returns the
    /// vertices that a following node should attach to.
    fn walk<S>(&mut self, node: &Node<S>, entries: &[Vertex]) -> Vec<Vertex> {
        match node {
            Node::Task(t) => {
                let vertex = self.vertex(t.name(), Shape::Task);
                for entry in entries {
                    self.edge(entry, &vertex, None);
                }
                vec![vertex]
            }
            Node::Seq(steps) => {
                let mut current: Vec<Vertex> = entries.iter().map(Vertex::clone_of).collect();
                for step in steps {
                    current = self.walk(step, &current);
                }
                current
            }
            Node::Par { branches, .. } => {
                let mut exits = Vec::new();
                for b in branches {
                    exits.extend(self.walk(b, entries));
                }
                exits
            }
            Node::Branch { arms, otherwise } => {
                let gate = self.vertex("branch", Shape::Control);
                for entry in entries {
                    self.edge(entry, &gate, None);
                }
                let mut exits = Vec::new();
                for arm in arms {
                    let taken = self.walk(arm.body(), &[]);
                    for vertex in &taken {
                        self.edge(&gate, vertex, Some(arm.label()));
                    }
                    exits.extend(taken);
                }
                if let Some(fallback) = otherwise {
                    let taken = self.walk(fallback, &[]);
                    for vertex in &taken {
                        self.edge(&gate, vertex, Some("otherwise"));
                    }
                    exits.extend(taken);
                }
                exits
            }
            Node::Rescue { name, body, .. } => {
                let exits = self.walk(body, entries);
                let handler = self.vertex(name, Shape::Recovery);
                for exit in &exits {
                    self.edge(exit, &handler, Some("on error"));
                }
                vec![handler]
            }
        }
    }
}

impl Vertex {
    fn clone_of(&self) -> Vertex {
        Vertex { id: self.id.clone(), label: self.label.clone() }
    }
}

#[derive(Clone, Copy)]
enum Shape {
    Task,
    Control,
    Recovery,
}
