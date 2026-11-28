//! Nested decode spec cursor, field lookup, and diagnostic path formatting.

/// The field the filler expects next: the one after the last match. An
/// encoder writes an object's keys in declaration order, so nearly every
/// key matches here on one compare and the spec is never walked.
pub(super) struct SpecCursor {
    pub(super) pos: usize,
    pub(super) idx: usize,
}

// ---------------------------------------------------------------------------
// the nested-shape filler
//
// a struct whose fields are required scalars or structs of the same shape,
// at any depth, decodes here in one pass over the bytes. the caller (the
// decode lowering, self-host/ir_decode_emit.pith) allocates the whole tree
// of structs ahead of the call, each with its destructor attached and each
// sub-struct already stored in its parent's slot, so this only ever writes
// scalars into slots and recurses into a struct it can read out of one.
// nothing here knows a destructor's address, and every string minted is
// owned by the struct whose slot holds it from the moment it is written.
//
// before this filler a nested shape parsed into std.json's node pool and
// copied its fields out through the object accessors, so its values, its
// error messages and the grammar it accepted were the node parser's. this
// mirrors that parser rather than the generated flat decoder
// (self-host/ir_json_fill_emit.pith, for a struct of scalars alone),
// which keeps the grammar of the flat filler it replaced: string escapes are
// decoded, every colon and comma is required, an object or array must be
// closed and takes no trailing comma, a number is an int only when it has no `.`,
// `e` or `E`, an int past the parser's bound fails the whole parse, and a
// literal is matched by prefix. the messages name the field's kind the way
// the accessors did ("missing string field: city") and come in the
// parser's order: any malformed input is "invalid json object" before a
// field is judged, then the fields in declaration order, depth first.
//
// the spec grammar grows three field kinds for this. `o<name>(<sub-spec>)`
// is a struct field whose slot holds a pre-allocated struct laid out by the
// sub-spec. `l<name>(<elem>)` is a list field and `m<name>(<elem>)` a
// `Map[String, T]` field, an object whose every key is kept: `<elem>` is a
// scalar letter, `l(<elem>)` for a list, `m(<elem>)` for a map, or
// `o#<k>(<sub-spec>)` for a struct, so the grammar is recursive and a list
// of maps of lists of structs parses like anything else. a collection is
// what this filler builds itself: the element structs cannot be allocated
// ahead of the call because their count is the document's, so the lowering
// hands over a table of destructor addresses and `#<k>` names the entry an
// element struct gets (0 for a struct that needs none). the same
// `o<name>#<k>(<sub-spec>)` form marks a struct field inside a collection
// element, which has no pre-allocated slot either. the scalar letters are
// `s`, `i`, `b` and `f`: an `f` slot takes any json number and holds the
// f64 bit pattern, an `i` slot only an integer.
//
// ownership while a collection is built: the list or map handle is stored
// into its slot (or pushed into, or inserted under its key in, its parent)
// before the first element is read, and an element struct is pushed or
// inserted, with its destructor set, before its first field is read. so at
// every moment the root struct owns everything built so far, and the one
// release the entry point does on failure drops it all: the list or map
// releases its elements, an element struct releases its strings and
// sub-structs. there is no separate cleanup path to get wrong. the leak
// cases tests/leaks/leak_json_fill_list_fail and
// tests/leaks/leak_json_fill_collections_fail pin this for a failure at the
// first, second and last element, in the inner list of the third outer
// element, and in the second member of a map.
//
// depth: every recursion below carries the value's nesting level and
// refuses to descend past NESTED_MAX_DEPTH, the node parser's bound, so a
// hostile document a hundred thousand deep stops at the bound with a named
// error rather than on the stack.
// ---------------------------------------------------------------------------

/// std.json's MAX_PARSE_DEPTH: a value nested deeper than this fails the
/// parse, root value at depth 1. The filler counts the same way (the root
/// object is depth 1, a field's value depth 2), so a document the parser
/// takes the filler takes, and one the parser refuses the filler refuses
/// with `NESTED_TOO_DEEP`. Every recursion below carries its depth and
/// checks it before descending, so a hostile `[[[[...` a hundred thousand
/// deep stops here rather than on the stack.
pub(super) const NESTED_MAX_DEPTH: usize = 128;

/// The decode error for a document nested past `NESTED_MAX_DEPTH`.
pub(super) const NESTED_TOO_DEEP: &[u8] = b"json nested deeper than 128 levels";

/// Why a fill gave up on the whole document: a byte the grammar does not
/// take (`invalid json object`, what the node parser reports), or a value
/// nested past the depth bound. Either outranks any field error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum NestedAbort {
    Malformed,
    TooDeep,
}

impl From<()> for NestedAbort {
    fn from(_: ()) -> Self {
        NestedAbort::Malformed
    }
}

impl NestedAbort {
    pub(super) fn message(self) -> &'static [u8] {
        match self {
            NestedAbort::Malformed => b"invalid json object",
            NestedAbort::TooDeep => NESTED_TOO_DEEP,
        }
    }
}

/// One field of the nested spec: its type char, its name, for an `o`, `l`
/// or `m` field the sub-spec between its parentheses, and for a struct the
/// filler allocates itself (`o<name>#<k>(...)`) the destructor table index
/// `k`; `dtor` is -1 for a struct whose slot the lowering pre-allocated.
/// `end` is the index just past the field, where a `,` or the end of the
/// spec sits. parentheses rather than braces because the IR's string
/// literals read `{{` and `}}` as one escaped brace, which folds a doubly
/// nested spec's closing pair. an element spec (`i`, `o#2(...)`, `l(s)`,
/// `m(i)`) parses with the same function: its name is empty. a map entry,
/// `m<name>(<elem>)`, is an object with arbitrary keys whose every value is
/// one `<elem>`; the same element grammar as a list's.
pub(super) struct NestedField {
    pub(super) kind: u8,
    pub(super) name_start: usize,
    pub(super) name_end: usize,
    pub(super) dtor: i64,
    pub(super) sub_start: usize,
    pub(super) sub_end: usize,
    pub(super) end: usize,
}

/// Parse the field whose type char sits at `pos`.
#[inline(always)]
pub(super) fn nested_field_at(spec: &[u8], pos: usize) -> NestedField {
    let name_start = pos + 1;
    let mut i = name_start;
    while i < spec.len() && spec[i] != b',' && spec[i] != b'(' && spec[i] != b'#' {
        i += 1;
    }
    let name_end = i;
    let mut dtor: i64 = -1;
    if i < spec.len() && spec[i] == b'#' {
        dtor = 0;
        i += 1;
        while i < spec.len() && spec[i].is_ascii_digit() {
            dtor = dtor * 10 + (spec[i] - b'0') as i64;
            i += 1;
        }
    }
    if i < spec.len() && spec[i] == b'(' {
        let sub_start = i + 1;
        let mut depth = 1usize;
        i += 1;
        while i < spec.len() && depth > 0 {
            match spec[i] {
                b'(' => depth += 1,
                b')' => depth -= 1,
                _ => {}
            }
            i += 1;
        }
        // i is one past the closing parenthesis
        NestedField { kind: spec[pos], name_start, name_end, dtor, sub_start, sub_end: i - 1, end: i }
    } else {
        NestedField { kind: spec[pos], name_start, name_end, dtor, sub_start: 0, sub_end: 0, end: i }
    }
}

/// How many fields a sub-spec lays out: one more than its top-level
/// separators, none for an empty spec. The slot count of a struct the
/// filler allocates for a list element.
pub(super) fn nested_spec_field_count(spec: &[u8]) -> usize {
    if spec.is_empty() {
        return 0;
    }
    let mut count = 1;
    let mut depth = 0usize;
    for &b in spec {
        match b {
            b'(' => depth += 1,
            b')' => depth = depth.saturating_sub(1),
            b',' if depth == 0 => count += 1,
            _ => {}
        }
    }
    count
}

/// Where the field after `field` starts; `spec.len()` when it was the last.
#[inline(always)]
pub(super) fn nested_next_pos(spec: &[u8], field: &NestedField) -> usize {
    if field.end < spec.len() {
        field.end + 1
    } else {
        spec.len()
    }
}

/// Look a key up in the nested spec: the cursor's field first, then a walk
/// from the start. Returns the field and its slot; the cursor moves to the
/// field after the hit.
#[inline(always)]
pub(super) fn nested_lookup(spec: &[u8], key: &[u8], cur: &mut SpecCursor) -> Option<(usize, NestedField)> {
    if cur.pos < spec.len() {
        let field = nested_field_at(spec, cur.pos);
        if &spec[field.name_start..field.name_end] == key {
            let idx = cur.idx;
            cur.pos = nested_next_pos(spec, &field);
            cur.idx += 1;
            return Some((idx, field));
        }
    }
    let mut pos = 0;
    let mut idx = 0;
    while pos < spec.len() {
        let field = nested_field_at(spec, pos);
        if &spec[field.name_start..field.name_end] == key {
            cur.pos = nested_next_pos(spec, &field);
            cur.idx = idx + 1;
            return Some((idx, field));
        }
        pos = nested_next_pos(spec, &field);
        idx += 1;
    }
    None
}

/// The kind word the node accessors put in their messages.
pub(super) fn nested_kind_word(kind: u8) -> &'static [u8] {
    match kind {
        b's' => b"string",
        b'i' => b"int",
        b'b' => b"bool",
        b'f' => b"float",
        b'l' => b"list",
        b'm' => b"map",
        _ => b"object",
    }
}

/// The message for an element of the wrong kind: `expected int element:
/// xs[2]`, the field's kind word and the element's position under the
/// field, so a caller can tell which element and which field.
pub(super) fn nested_element_error(kind: u8, path: &[u8], index: usize) -> Vec<u8> {
    let mut at = path.to_vec();
    nested_push_index(&mut at, index);
    nested_kind_error_at(kind, &at)
}

/// Append `[<index>]` to a path.
pub(super) fn nested_push_index(out: &mut Vec<u8>, index: usize) {
    out.push(b'[');
    out.extend_from_slice(index.to_string().as_bytes());
    out.push(b']');
}

/// A map member's path: `<path>.<key>`, the dotted form `decode_path`
/// reads, so a wrong value under a key is `expected int element:
/// scores.bob` and a struct member's own error `scores.bob: missing int
/// field: x`. A key holding a dot is written as it is; the message is for
/// reading, not for parsing back.
pub(super) fn nested_member_path(path: &[u8], key: &[u8]) -> Vec<u8> {
    let mut out = path.to_vec();
    out.push(b'.');
    out.extend_from_slice(key);
    out
}

/// `expected <kind> element: <path>` for a value that is not of its kind,
/// the path already naming the position (`xs[2]`, `scores.bob`).
pub(super) fn nested_kind_error_at(kind: u8, path: &[u8]) -> Vec<u8> {
    let mut msg = b"expected ".to_vec();
    msg.extend_from_slice(nested_kind_word(kind));
    msg.extend_from_slice(b" element: ");
    msg.extend_from_slice(path);
    msg
}

/// `<path>: <msg>`: a struct's own error placed under its position.
pub(super) fn nested_prefixed_at(path: &[u8], msg: &[u8]) -> Vec<u8> {
    let mut out = path.to_vec();
    out.extend_from_slice(b": ");
    out.extend_from_slice(msg);
    out
}

/// A struct element's own error, placed under its position: `rows[1]:
/// missing string field: name`. A list of lists nests the same way.
pub(super) fn nested_element_prefixed(path: &[u8], index: usize, msg: &[u8]) -> Vec<u8> {
    let mut at = path.to_vec();
    nested_push_index(&mut at, index);
    nested_prefixed_at(&at, msg)
}

pub(super) fn nested_field_error(prefix: &[u8], kind: u8, name: &[u8]) -> Vec<u8> {
    let mut msg = prefix.to_vec();
    msg.extend_from_slice(nested_kind_word(kind));
    msg.extend_from_slice(b" field: ");
    msg.extend_from_slice(name);
    msg
}
/// The kind of value an element spec builds, as the tag a container owns
/// its elements by: a string, a struct, a list or a map is held by count,
/// an int, bool or float by value.
pub(super) fn nested_elem_tag(elem: &[u8]) -> crate::collections::list::ListTypeTag {
    use crate::collections::list::ListTypeTag;
    match elem.first() {
        Some(b's') => ListTypeTag::String,
        Some(b'o') => ListTypeTag::Struct,
        Some(b'l') => ListTypeTag::List,
        Some(b'm') => ListTypeTag::Map,
        _ => ListTypeTag::Primitive,
    }
}
