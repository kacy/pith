// cyclic_graph_strong: rust counterpart of bench/cyclic_graph_strong.pith.
//
// identical to bench/cyclic_graph.rs except the back edge is a strong Rc,
// matching the plain `parent` field in the pith variant. `next` owns
// forward and `parent` owns back, so neither refcount reaches zero and every
// ring leaks, as it does in pith.
//
// build: rustc -O -o bench/cyclic_graph_strong_rust bench/cyclic_graph_strong.rs
// run:   ./bench/cyclic_graph_strong_rust 10000000
use std::cell::RefCell;
use std::rc::Rc;

struct Node {
    value: i64,
    next: RefCell<Option<Rc<Node>>>,
    parent: RefCell<Option<Rc<Node>>>,
}

fn new_node(value: i64) -> Rc<Node> {
    Rc::new(Node {
        value,
        next: RefCell::new(None),
        parent: RefCell::new(None),
    })
}

// build one ring and drop it. both edges are strong, so the ring is a real
// cycle that never reaches refcount zero.
fn build_ring(seed: i64) -> i64 {
    let a = new_node(seed);
    let b = new_node(seed + 1);
    *a.next.borrow_mut() = Some(Rc::clone(&b));
    *b.parent.borrow_mut() = Some(Rc::clone(&a));
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
