// cyclic_graph: rust counterpart of bench/cyclic_graph.pith.
//
// builds many two-node parent<->child rings and drops them. `next` is an
// owning Rc edge and `parent` is a Weak back edge, the same shape as the
// pith `weak parent` field: the ring holds no strong cycle, so both nodes
// are freed when build_ring returns and resident memory stays flat. the
// strong-back-edge variant is bench/cyclic_graph_strong.rs.
//
// build: rustc -O -o bench/cyclic_graph_rust bench/cyclic_graph.rs
// run:   ./bench/cyclic_graph_rust 10000000
use std::cell::RefCell;
use std::rc::{Rc, Weak};

struct Node {
    value: i64,
    next: RefCell<Option<Rc<Node>>>,
    parent: RefCell<Option<Weak<Node>>>,
}

fn new_node(value: i64) -> Rc<Node> {
    Rc::new(Node {
        value,
        next: RefCell::new(None),
        parent: RefCell::new(None),
    })
}

// build one ring and drop it. the checksum reads the node values directly,
// as the pith version does.
fn build_ring(seed: i64) -> i64 {
    let a = new_node(seed);
    let b = new_node(seed + 1);
    *a.next.borrow_mut() = Some(Rc::clone(&b));
    *b.parent.borrow_mut() = Some(Rc::downgrade(&a));
    a.value + b.value
}

fn main() {
    let rings: i64 = std::env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .filter(|&v: &i64| v > 0)
        .unwrap_or(1000000);
    let mut checksum: i64 = 0;
    for i in 0..rings {
        checksum += build_ring(i);
    }
    println!("rings: {} checksum: {}", rings, checksum);
}
