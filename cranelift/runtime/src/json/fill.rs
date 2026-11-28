//! Recursive JSON object, array, and map fillers.

use super::scanner::{
    float_of_span, nested_after_value, nested_expect_colon, nested_is_number_start,
    nested_read_number, nested_read_string, nested_skip_value, number_span_end, skip_ws,
    NestedSep,
};
use super::spec::{
    nested_element_error, nested_element_prefixed, nested_field_at, nested_field_error,
    nested_kind_error_at, nested_lookup, nested_member_path, nested_next_pos, nested_prefixed_at,
    nested_push_index, NestedAbort, SpecCursor, NESTED_MAX_DEPTH,
};
use super::targets::{DtorTable, FieldBits, NestedFill, NestedList, NestedMap, ObjectTarget};

/// Fill the struct behind `target` from the object opening at `pos` (the byte
/// is `{`). A key the spec knows writes its slot; one it does not is consumed
/// with the parser's grammar. A key that repeats has its last value win, as the
/// node accessors read the last entry: a string slot already holding a value is
/// released before it is overwritten, a pre-allocated struct slot is filled
/// again in place with a fresh mask, and a list slot or a filler-allocated
/// struct slot has its first value released before the second is built.
pub(super) fn nested_fill_object<'a>(
    input: &[u8],
    pos: usize,
    target: ObjectTarget<'a>,
    dtors: &DtorTable<'_>,
    depth: usize,
) -> NestedFill {
    let spec = target.spec;
    let mut filled = FieldBits::new();
    let mut wrong = FieldBits::new();
    let mut deferred: Vec<(usize, Vec<u8>)> = Vec::new();
    let mut cur = SpecCursor { pos: 0, idx: 0 };
    let inner_depth = depth + 1;

    let mut pos = skip_ws(input, pos + 1);
    let mut done = pos < input.len() && input[pos] == b'}';
    if done {
        pos += 1;
    }
    while !done {
        if input.get(pos) != Some(&b'"') {
            return Err(NestedAbort::Malformed);
        }
        let (key, after_key) = nested_read_string(input, pos)?;
        pos = nested_expect_colon(input, after_key)?;
        if inner_depth > NESTED_MAX_DEPTH {
            return Err(NestedAbort::TooDeep);
        }
        pos = skip_ws(input, pos);
        if pos >= input.len() {
            return Err(NestedAbort::Malformed);
        }
        let b = input[pos];
        match nested_lookup(spec, &key, &mut cur) {
            None => {
                pos = nested_skip_value(input, pos, inner_depth)?;
            }
            Some((idx, field)) => {
                let mut ok = false;
                match field.kind {
                    b's' if b == b'"' => {
                        let (value, end) = nested_read_string(input, pos)?;
                        target.replace_string(idx, &value);
                        pos = end;
                        ok = true;
                    }
                    b'i' if nested_is_number_start(b) => {
                        let (value, end) = nested_read_number(input, pos)?;
                        if let Some(value) = value {
                            target.write_slot(idx, value);
                            ok = true;
                        }
                        pos = end;
                    }
                    b'f' if nested_is_number_start(b) => {
                        // the node parser's float branch: the whole number
                        // span, an integer one included, converted by the
                        // runtime. a span that is not a finite number fails
                        // the parse the way the parser's float branch does.
                        let end = number_span_end(input, pos + 1);
                        let Some(value) = float_of_span(&input[pos..end]) else {
                            return Err(NestedAbort::Malformed);
                        };
                        target.write_slot(idx, value.to_bits() as i64);
                        pos = end;
                        ok = true;
                    }
                    b'b' if input[pos..].starts_with(b"true") => {
                        target.write_slot(idx, 1);
                        pos += 4;
                        ok = true;
                    }
                    b'b' if input[pos..].starts_with(b"false") => {
                        target.write_slot(idx, 0);
                        pos += 5;
                        ok = true;
                    }
                    b'o' if b == b'{' && target.has_object_slot(idx, field.dtor) => {
                        let sub_spec = &spec[field.sub_start..field.sub_end];
                        let child = if field.dtor >= 0 {
                            // a struct field under a list element: no
                            // pre-allocated slot, so allocate it here and
                            // store it before it is filled, and drop what a
                            // repeated key built before.
                            let Some(dtor) = dtors.get(field.dtor) else {
                                return Err(NestedAbort::Malformed);
                            };
                            target.clear_struct(idx);
                            let Some(sub) = dtor.alloc_object(sub_spec) else {
                                return Err(NestedAbort::Malformed);
                            };
                            target.write_slot(idx, sub.as_handle());
                            sub
                        } else {
                            target.child_object(idx, sub_spec)
                        };
                        let (end, sub_err) = nested_fill_object(input, pos, child, dtors, inner_depth)?;
                        deferred.retain(|(slot_idx, _)| *slot_idx != idx);
                        if let Some(msg) = sub_err {
                            deferred.push((idx, msg));
                        }
                        pos = end;
                        ok = true;
                    }
                    b'l' if b == b'[' => {
                        // a repeated list key: the first list and everything
                        // in it go before the second is built.
                        target.clear_list(idx);
                        let elem = &spec[field.sub_start..field.sub_end];
                        let Some(list) = NestedList::new(elem) else {
                            return Err(NestedAbort::Malformed);
                        };
                        // owned by the struct from here: a failure anywhere
                        // below releases the root and the list with it.
                        target.write_slot(idx, list.as_handle());
                        let name = &spec[field.name_start..field.name_end];
                        let (end, elem_err) = nested_fill_list(input, pos, elem, name, list, dtors, inner_depth)?;
                        deferred.retain(|(slot_idx, _)| *slot_idx != idx);
                        if let Some(msg) = elem_err {
                            deferred.push((idx, msg));
                        }
                        pos = end;
                        ok = true;
                    }
                    b'm' if b == b'{' => {
                        // a repeated map key: the first map and its values
                        // go before the second is built, as for a list.
                        target.clear_map(idx);
                        let elem = &spec[field.sub_start..field.sub_end];
                        let Some(map) = NestedMap::new(elem) else {
                            return Err(NestedAbort::Malformed);
                        };
                        // owned by the struct before the first member is
                        // read, so a failure below cascades from the root.
                        target.write_slot(idx, map.as_handle());
                        let name = &spec[field.name_start..field.name_end];
                        let (end, member_err) = nested_fill_map(input, pos, elem, name, map, dtors, inner_depth)?;
                        deferred.retain(|(slot_idx, _)| *slot_idx != idx);
                        if let Some(msg) = member_err {
                            deferred.push((idx, msg));
                        }
                        pos = end;
                        ok = true;
                    }
                    _ => {
                        pos = nested_skip_value(input, pos, inner_depth)?;
                    }
                }
                if ok {
                    filled.set(idx);
                    wrong.clear(idx);
                } else {
                    filled.clear(idx);
                    wrong.set(idx);
                    deferred.retain(|(slot_idx, _)| *slot_idx != idx);
                }
            }
        }
        pos = skip_ws(input, pos);
        match input.get(pos) {
            Some(&b',') => pos = skip_ws(input, pos + 1),
            Some(&b'}') => {
                pos += 1;
                done = true;
            }
            _ => return Err(NestedAbort::Malformed),
        }
    }

    // the fields in declaration order: a wrong type, then a miss, then
    // whatever a nested struct reported, the first of which is the error.
    let mut spec_pos = 0;
    let mut idx = 0;
    while spec_pos < spec.len() {
        let field = nested_field_at(spec, spec_pos);
        let name = &spec[field.name_start..field.name_end];
        if wrong.contains(idx) {
            return Ok((pos, Some(nested_field_error(b"expected ", field.kind, name))));
        }
        if !filled.contains(idx) {
            return Ok((pos, Some(nested_field_error(b"missing ", field.kind, name))));
        }
        if field.kind == b'o' || field.kind == b'l' || field.kind == b'm' {
            if let Some((_, msg)) = deferred.iter().find(|(slot_idx, _)| *slot_idx == idx) {
                return Ok((pos, Some(msg.clone())));
            }
        }
        spec_pos = nested_next_pos(spec, &field);
        idx += 1;
    }
    Ok((pos, None))
}

/// Fill `list` from the array opening at `pos` (the byte is `[`), one
/// element per `elem` spec, pushing each as it is built. The grammar is
/// the node parser's array branch: elements separated by commas, with no
/// trailing comma, and the closing bracket required. Returns the
/// position after the array and the first element error by position, if
/// any; like a field error it waits with the caller, because a malformed
/// byte anywhere in the document outranks it. Elements after a failed one
/// are still read, so the grammar check covers the whole array.
fn nested_fill_list(
    input: &[u8],
    pos: usize,
    elem: &[u8],
    path: &[u8],
    list: NestedList,
    dtors: &DtorTable<'_>,
    depth: usize,
) -> NestedFill {
    let mut first_err: Option<Vec<u8>> = None;
    let elem_depth = depth + 1;
    let mut pos = skip_ws(input, pos + 1);
    if pos < input.len() && input[pos] == b']' {
        return Ok((pos + 1, None));
    }
    let mut index = 0usize;
    loop {
        if pos >= input.len() {
            return Err(NestedAbort::Malformed);
        }
        if elem_depth > NESTED_MAX_DEPTH {
            return Err(NestedAbort::TooDeep);
        }
        let (end, elem_err) = nested_fill_element(input, pos, elem, path, index, list, dtors, elem_depth)?;
        if first_err.is_none() {
            first_err = elem_err;
        }
        match nested_after_value(input, end, b']')? {
            NestedSep::More(next) => pos = next,
            NestedSep::Closed(after) => return Ok((after, first_err)),
        }
        index += 1;
    }
}

/// Read one array element at `pos` (not whitespace, not the end) by the
/// element spec and push it into `list`. A scalar of the right kind is
/// pushed by value or as a fresh string the list owns; a struct element is
/// allocated with the destructor its `#<k>` names, pushed, and then filled
/// in place, so the list owns it before its first field is read; a list
/// element is built the same way through `nested_fill_list`. An element of
/// the wrong kind is consumed with the parser's grammar and reported as
/// `expected <kind> element: <path>[<index>]`; a struct element's own
/// field error comes back as `<path>[<index>]: <its message>`.
#[allow(clippy::too_many_arguments)]
fn nested_fill_element(
    input: &[u8],
    pos: usize,
    elem: &[u8],
    path: &[u8],
    index: usize,
    list: NestedList,
    dtors: &DtorTable<'_>,
    depth: usize,
) -> NestedFill {
    let b = input[pos];
    let field = nested_field_at(elem, 0);
    match field.kind {
        b's' if b == b'"' => {
            let (value, end) = nested_read_string(input, pos)?;
            let s = crate::pith_copy_bytes_to_cstring(&value) as i64;
            list.push_owned(s);
            Ok((end, None))
        }
        b'i' if nested_is_number_start(b) => {
            let (value, end) = nested_read_number(input, pos)?;
            match value {
                Some(value) => {
                    list.push_value(value);
                    Ok((end, None))
                }
                None => Ok((end, Some(nested_element_error(b'i', path, index)))),
            }
        }
        b'f' if nested_is_number_start(b) => {
            let end = number_span_end(input, pos + 1);
            let Some(value) = float_of_span(&input[pos..end]) else {
                return Err(NestedAbort::Malformed);
            };
            list.push_value(value.to_bits() as i64);
            Ok((end, None))
        }
        b'b' if input[pos..].starts_with(b"true") => {
            list.push_value(1);
            Ok((pos + 4, None))
        }
        b'b' if input[pos..].starts_with(b"false") => {
            list.push_value(0);
            Ok((pos + 5, None))
        }
        b'o' if b == b'{' => {
            let sub_spec = &elem[field.sub_start..field.sub_end];
            let Some(dtor) = dtors.get(field.dtor) else {
                return Err(NestedAbort::Malformed);
            };
            let Some(sub) = dtor.alloc_object(sub_spec) else {
                return Err(NestedAbort::Malformed);
            };
            // the list takes the one count the allocation minted, before
            // any field of the element is read
            list.push_owned(sub.as_handle());
            let (end, sub_err) = nested_fill_object(input, pos, sub, dtors, depth)?;
            Ok((end, sub_err.map(|msg| nested_element_prefixed(path, index, &msg))))
        }
        b'l' if b == b'[' => {
            let inner_elem = &elem[field.sub_start..field.sub_end];
            let Some(inner) = NestedList::new(inner_elem) else {
                return Err(NestedAbort::Malformed);
            };
            list.push_owned(inner.as_handle());
            let mut inner_path = path.to_vec();
            nested_push_index(&mut inner_path, index);
            nested_fill_list(input, pos, inner_elem, &inner_path, inner, dtors, depth)
        }
        b'm' if b == b'{' => {
            let inner_elem = &elem[field.sub_start..field.sub_end];
            let Some(inner) = NestedMap::new(inner_elem) else {
                return Err(NestedAbort::Malformed);
            };
            list.push_owned(inner.as_handle());
            let mut inner_path = path.to_vec();
            nested_push_index(&mut inner_path, index);
            nested_fill_map(input, pos, inner_elem, &inner_path, inner, dtors, depth)
        }
        kind => {
            let end = nested_skip_value(input, pos, depth)?;
            Ok((end, Some(nested_element_error(kind, path, index))))
        }
    }
}

/// Fill `map` from the object opening at `pos` (the byte is `{`), one
/// member per key, each value by the `elem` spec, inserting each as it is
/// built. The grammar is the node parser's object branch, as in
/// `nested_fill_object`; every key is kept, none is looked up in a spec. A
/// key that repeats has its last value win and the map releases the
/// earlier one at the insert, as the node accessors read the last entry.
/// Returns the position after the object and the first member error in
/// document order, if any, for a key whose last value carried it; like an
/// element error it waits with the caller.
fn nested_fill_map(
    input: &[u8],
    pos: usize,
    elem: &[u8],
    path: &[u8],
    map: NestedMap,
    dtors: &DtorTable<'_>,
    depth: usize,
) -> NestedFill {
    let mut deferred: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
    let member_depth = depth + 1;
    let mut pos = skip_ws(input, pos + 1);
    let mut done = pos < input.len() && input[pos] == b'}';
    if done {
        pos += 1;
    }
    while !done {
        if input.get(pos) != Some(&b'"') {
            return Err(NestedAbort::Malformed);
        }
        let (key, after_key) = nested_read_string(input, pos)?;
        pos = nested_expect_colon(input, after_key)?;
        if member_depth > NESTED_MAX_DEPTH {
            return Err(NestedAbort::TooDeep);
        }
        pos = skip_ws(input, pos);
        if pos >= input.len() {
            return Err(NestedAbort::Malformed);
        }
        let (end, member_err) = nested_fill_member(input, pos, elem, path, &key, map, dtors, member_depth)?;
        // the last value under a key decides its error, as it decides its value
        deferred.retain(|(k, _)| k.as_slice() != key.as_ref());
        if let Some(msg) = member_err {
            deferred.push((key.into_owned(), msg));
        }
        pos = skip_ws(input, end);
        match input.get(pos) {
            Some(&b',') => pos = skip_ws(input, pos + 1),
            Some(&b'}') => {
                pos += 1;
                done = true;
            }
            _ => return Err(NestedAbort::Malformed),
        }
    }
    Ok((pos, deferred.into_iter().next().map(|(_, msg)| msg)))
}

/// Read one map member's value at `pos` (not whitespace, not the end) by
/// the element spec and insert it under `key`. A scalar of the right kind
/// is inserted by value or as a fresh string the map owns; a struct is
/// allocated with the destructor its `#<k>` names, inserted, then filled
/// in place; a list or a map value is built the same way through its
/// filler. So the map owns every value before the value's first byte is
/// read, and a failure anywhere below cascades from the root. A value of
/// the wrong kind is consumed with the parser's grammar and reported as
/// `expected <kind> element: <path>.<key>`; a struct's own field error
/// comes back as `<path>.<key>: <its message>`.
#[allow(clippy::too_many_arguments)]
fn nested_fill_member(
    input: &[u8],
    pos: usize,
    elem: &[u8],
    path: &[u8],
    key: &[u8],
    map: NestedMap,
    dtors: &DtorTable<'_>,
    depth: usize,
) -> NestedFill {
    let b = input[pos];
    let field = nested_field_at(elem, 0);
    match field.kind {
        b's' if b == b'"' => {
            let (value, end) = nested_read_string(input, pos)?;
            let s = crate::pith_copy_bytes_to_cstring(&value) as i64;
            map.insert(elem, key, s);
            Ok((end, None))
        }
        b'i' if nested_is_number_start(b) => {
            let (value, end) = nested_read_number(input, pos)?;
            match value {
                Some(value) => {
                    map.insert(elem, key, value);
                    Ok((end, None))
                }
                None => Ok((end, Some(nested_kind_error_at(b'i', &nested_member_path(path, key))))),
            }
        }
        b'f' if nested_is_number_start(b) => {
            let end = number_span_end(input, pos + 1);
            let Some(value) = float_of_span(&input[pos..end]) else {
                return Err(NestedAbort::Malformed);
            };
            map.insert(elem, key, value.to_bits() as i64);
            Ok((end, None))
        }
        b'b' if input[pos..].starts_with(b"true") => {
            map.insert(elem, key, 1);
            Ok((pos + 4, None))
        }
        b'b' if input[pos..].starts_with(b"false") => {
            map.insert(elem, key, 0);
            Ok((pos + 5, None))
        }
        b'o' if b == b'{' => {
            let sub_spec = &elem[field.sub_start..field.sub_end];
            let Some(dtor) = dtors.get(field.dtor) else {
                return Err(NestedAbort::Malformed);
            };
            let Some(sub) = dtor.alloc_object(sub_spec) else {
                return Err(NestedAbort::Malformed);
            };
            // the map takes the one count the allocation minted, before
            // any field of the member is read
            map.insert(elem, key, sub.as_handle());
            let (end, sub_err) = nested_fill_object(input, pos, sub, dtors, depth)?;
            Ok((end, sub_err.map(|msg| nested_prefixed_at(&nested_member_path(path, key), &msg))))
        }
        b'l' if b == b'[' => {
            let inner_elem = &elem[field.sub_start..field.sub_end];
            let Some(inner) = NestedList::new(inner_elem) else {
                return Err(NestedAbort::Malformed);
            };
            map.insert(elem, key, inner.as_handle());
            nested_fill_list(input, pos, inner_elem, &nested_member_path(path, key), inner, dtors, depth)
        }
        b'm' if b == b'{' => {
            let inner_elem = &elem[field.sub_start..field.sub_end];
            let Some(inner) = NestedMap::new(inner_elem) else {
                return Err(NestedAbort::Malformed);
            };
            map.insert(elem, key, inner.as_handle());
            nested_fill_map(input, pos, inner_elem, &nested_member_path(path, key), inner, dtors, depth)
        }
        kind => {
            let end = nested_skip_value(input, pos, depth)?;
            Ok((end, Some(nested_kind_error_at(kind, &nested_member_path(path, key)))))
        }
    }
}
