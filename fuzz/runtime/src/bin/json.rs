//! The typed json fillers in json.rs, driven the way the decode lowering
//! (self-host/ir_decode_emit.pith) drives them. The front of the input
//! describes a family of struct types, from which the harness writes the
//! spec the emitter would intern, allocates the struct tree the lowering
//! would allocate (destructors attached), builds the destructor table for
//! a spec with collections, and picks the entry point the lowering picks:
//! the flat filler for all-scalar fields, the nested one for struct
//! fields, the collection one when a list or map appears anywhere. The
//! rest of the input is the document. A filled tree is walked, then
//! released, so a slot holding a freed or foreign value shows up.
#![no_main]

use std::cell::RefCell;

use pith_runtime::collections::list::{pith_list_get_value, pith_list_len, pith_list_release_handle, PithList};
use pith_runtime::collections::map::{pith_map_get_cstr, pith_map_keys_cstr, pith_map_release_handle};
use pith_runtime::json::{
    pith_json_decode_missing_error, pith_json_fill_struct, pith_json_fill_struct_lists, pith_json_fill_struct_nested,
};
use pith_runtime::{pith_cstring_release, pith_struct_alloc, pith_struct_release, pith_struct_set_dtor};
use pith_runtime_fuzz::{cstr_vec, fuzz_target, Input, PBytes, PStr};

const MAX_SHAPES: usize = 4;
const MAX_FIELDS: usize = 6;
const MAX_ELEM_DEPTH: usize = 3;
const NAMES: [&str; 8] = ["a", "b", "id", "name", "x", "tags", "v", "key"];

#[derive(Clone, Debug)]
enum Elem {
    Scalar(u8),
    Obj(usize),
    List(Box<Elem>),
    Map(Box<Elem>),
}

#[derive(Clone, Debug)]
enum Kind {
    Scalar(u8),
    Obj(usize),
    List(Elem),
    Map(Elem),
}

#[derive(Debug)]
struct Shape {
    names: Vec<&'static str>,
    kinds: Vec<Kind>,
}

impl Shape {
    fn needs_dtor(&self) -> bool {
        self.kinds.iter().any(|k| !matches!(k, Kind::Scalar(c) if *c != b's'))
    }
}

thread_local! {
    /// The shapes of the current input, read by the destructors.
    static SHAPES: RefCell<Vec<Shape>> = const { RefCell::new(Vec::new()) };
}

/// A compiled struct destructor for shape `K`: release every counted slot.
unsafe extern "C" fn dtor<const K: usize>(ptr: i64) {
    let kinds = SHAPES.with(|s| s.borrow()[K].kinds.clone());
    let slots = ptr as *const i64;
    for (i, kind) in kinds.iter().enumerate() {
        let v = *slots.add(i);
        match kind {
            Kind::Scalar(b's') => pith_cstring_release(v as *const i8),
            Kind::Scalar(_) => {}
            Kind::Obj(_) => pith_struct_release(v),
            Kind::List(_) => pith_list_release_handle(v),
            Kind::Map(_) => pith_map_release_handle(v),
        }
    }
}

const DTORS: [unsafe extern "C" fn(i64); MAX_SHAPES] = [dtor::<0>, dtor::<1>, dtor::<2>, dtor::<3>];

fn scalar_of(b: u8) -> u8 {
    b"isbf"[(b / 8) as usize % 4]
}

fn gen_elem(input: &mut Input, i: usize, n: usize, depth: usize) -> Elem {
    let b = input.byte();
    match b % 6 {
        3 if i + 1 < n => Elem::Obj(i + 1 + (b / 8) as usize % (n - i - 1)),
        4 if depth < MAX_ELEM_DEPTH => Elem::List(Box::new(gen_elem(input, i, n, depth + 1))),
        5 if depth < MAX_ELEM_DEPTH => Elem::Map(Box::new(gen_elem(input, i, n, depth + 1))),
        _ => Elem::Scalar(scalar_of(b)),
    }
}

fn gen_shapes(input: &mut Input) -> Vec<Shape> {
    let n = 1 + input.byte() as usize % MAX_SHAPES;
    let mut shapes = Vec::new();
    for i in 0..n {
        let fields = 1 + input.byte() as usize % MAX_FIELDS;
        let mut names = Vec::new();
        let mut kinds = Vec::new();
        for _ in 0..fields {
            let b = input.byte();
            let kind = match b % 8 {
                4 | 5 if i + 1 < n => Kind::Obj(i + 1 + (b / 8) as usize % (n - i - 1)),
                6 => Kind::List(gen_elem(input, i, n, 1)),
                7 => Kind::Map(gen_elem(input, i, n, 1)),
                _ => Kind::Scalar(scalar_of(b)),
            };
            // struct field names are distinct
            let mut pick = input.byte() as usize % NAMES.len();
            while names.contains(&NAMES[pick]) {
                pick = (pick + 1) % NAMES.len();
            }
            names.push(NAMES[pick]);
            kinds.push(kind);
        }
        shapes.push(Shape { names, kinds });
    }
    shapes
}

/// The spec the emitter writes for shape `k`. A struct reached through a
/// collection is allocated by the filler, so it carries its destructor
/// index (`#k`); one reached through struct fields alone is pre-allocated.
fn spec(shapes: &[Shape], k: usize, in_collection: bool) -> String {
    let shape = &shapes[k];
    let fields: Vec<String> = shape
        .names
        .iter()
        .zip(&shape.kinds)
        .map(|(name, kind)| match kind {
            Kind::Scalar(c) => format!("{}{name}", *c as char),
            Kind::Obj(j) if in_collection => format!("o{name}#{j}({})", spec(shapes, *j, true)),
            Kind::Obj(j) => format!("o{name}({})", spec(shapes, *j, false)),
            Kind::List(e) => format!("l{name}({})", elem_spec(shapes, e)),
            Kind::Map(e) => format!("m{name}({})", elem_spec(shapes, e)),
        })
        .collect();
    fields.join(",")
}

fn elem_spec(shapes: &[Shape], e: &Elem) -> String {
    match e {
        Elem::Scalar(c) => (*c as char).to_string(),
        Elem::Obj(j) => format!("o#{j}({})", spec(shapes, *j, true)),
        Elem::List(inner) => format!("l({})", elem_spec(shapes, inner)),
        Elem::Map(inner) => format!("m({})", elem_spec(shapes, inner)),
    }
}

fn has_collection(shapes: &[Shape], k: usize) -> bool {
    shapes[k].kinds.iter().any(|kind| match kind {
        Kind::List(_) | Kind::Map(_) => true,
        Kind::Obj(j) => has_collection(shapes, *j),
        Kind::Scalar(_) => false,
    })
}

/// `ir_emit_json_nested_alloc`: the struct, its destructor, and each struct
/// field's own tree in its slot.
unsafe fn prealloc(shapes: &[Shape], k: usize) -> i64 {
    let shape = &shapes[k];
    let ptr = pith_struct_alloc(shape.kinds.len() as i64);
    assert_ne!(ptr, 0);
    if shape.needs_dtor() {
        pith_struct_set_dtor(ptr, DTORS[k] as usize as i64);
    }
    for (i, kind) in shape.kinds.iter().enumerate() {
        if let Kind::Obj(j) = kind {
            *(ptr as *mut i64).add(i) = prealloc(shapes, *j);
        }
    }
    ptr
}

unsafe fn walk_scalar(c: u8, v: i64) {
    match c {
        b's' => {
            cstr_vec(v as *const i8).expect("string slot");
        }
        b'b' => assert!(v == 0 || v == 1, "bool slot holds {v}"),
        b'f' => assert!(f64::from_bits(v as u64).is_finite(), "float slot holds {v:#x}"),
        _ => {}
    }
}

unsafe fn walk_elem(shapes: &[Shape], e: &Elem, v: i64) {
    match e {
        Elem::Scalar(c) => walk_scalar(*c, v),
        Elem::Obj(j) => walk(shapes, *j, v),
        Elem::List(inner) => walk_list(shapes, inner, v),
        Elem::Map(inner) => walk_map(shapes, inner, v),
    }
}

unsafe fn walk_list(shapes: &[Shape], e: &Elem, handle: i64) {
    assert_ne!(handle, 0);
    let list = PithList { ptr: handle as *mut () };
    for i in 0..pith_list_len(list) {
        walk_elem(shapes, e, pith_list_get_value(list, i));
    }
}

unsafe fn walk_map(shapes: &[Shape], e: &Elem, handle: i64) {
    assert_ne!(handle, 0);
    let keys = pith_map_keys_cstr(handle);
    let list = PithList { ptr: keys as *mut () };
    for i in 0..pith_list_len(list) {
        let key = pith_list_get_value(list, i) as *const i8;
        walk_elem(shapes, e, pith_map_get_cstr(handle, key));
    }
    pith_list_release_handle(keys);
}

/// Read every slot of a filled struct of shape `k`.
unsafe fn walk(shapes: &[Shape], k: usize, ptr: i64) {
    assert_ne!(ptr, 0);
    let slots = ptr as *const i64;
    for (i, kind) in shapes[k].kinds.iter().enumerate() {
        let v = *slots.add(i);
        match kind {
            Kind::Scalar(c) => walk_scalar(*c, v),
            Kind::Obj(j) => walk(shapes, *j, v),
            Kind::List(e) => walk_list(shapes, e, v),
            Kind::Map(e) => walk_map(shapes, e, v),
        }
    }
}

/// Take a decode result box: walk and release the struct on success, read
/// and release the error on failure.
unsafe fn take_result(shapes: &[Shape], result: i64, root: Option<i64>) {
    assert_ne!(result, 0);
    let tuple = result as *const i64;
    if *tuple == 1 {
        let value = *tuple.add(1);
        if let Some(root) = root {
            assert_eq!(value, root);
        }
        walk(shapes, 0, value);
        pith_struct_release(value);
    } else {
        assert_eq!(*tuple, 0);
        let error = *tuple.add(2);
        let message = cstr_vec(*(error as *const i64) as *const i8).expect("error message");
        assert!(!message.is_empty());
        pith_struct_release(error);
    }
    pith_struct_release(result);
}

fuzz_target!(|data| {
    let mut input = Input::new(data);
    let shapes = gen_shapes(&mut input);
    let check_utf8 = input.byte() as i64 & 1;
    let doc = PBytes::new(input.rest());
    let root_spec = spec(&shapes, 0, false);
    let literal = PStr::literal(root_spec.as_bytes());
    let spec_ptr = literal.ptr() as i64;
    SHAPES.with(|s| *s.borrow_mut() = shapes);
    SHAPES.with(|s| {
        let shapes = &*s.borrow();
        unsafe {
            let all_scalar = shapes[0].kinds.iter().all(|k| matches!(k, Kind::Scalar(_)));
            let root = prealloc(shapes, 0);
            if all_scalar {
                // ir_emit_json_decode_scan_fields
                let n = shapes[0].kinds.len();
                let mask = pith_json_fill_struct(doc.0, spec_ptr, root);
                assert!(mask == -1 || (0..1 << n).contains(&mask), "mask {mask:#x} for {n} fields");
                if mask == (1 << n) - 1 {
                    walk(shapes, 0, root);
                    pith_struct_release(root);
                } else {
                    pith_struct_release(root);
                    take_result(shapes, pith_json_decode_missing_error(mask, spec_ptr), None);
                }
            } else if !has_collection(shapes, 0) {
                take_result(shapes, pith_json_fill_struct_nested(doc.0, spec_ptr, root, check_utf8), Some(root));
            } else {
                // the destructor table, when the spec names an index
                let count = if root_spec.contains('#') { shapes.len() } else { 0 };
                let table = if count == 0 { 0 } else { pith_struct_alloc(count as i64) };
                for k in 0..count {
                    let entry = if shapes[k].needs_dtor() { DTORS[k] as usize as i64 } else { 0 };
                    *(table as *mut i64).add(k) = entry;
                }
                let result = pith_json_fill_struct_lists(doc.0, spec_ptr, root, check_utf8, table, count as i64);
                if table != 0 {
                    pith_struct_release(table);
                }
                take_result(shapes, result, Some(root));
            }
        }
    });
    SHAPES.with(|s| s.borrow_mut().clear());
});
