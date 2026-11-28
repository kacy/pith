use super::scanner::*;
use super::spec::*;
use super::targets::*;
use super::*;


#[test]
fn number_reader_covers_the_whole_i64_range() {
    let read = |text: &str| nested_read_number(text.as_bytes(), 0).map(|(v, _)| v);
    assert_eq!(read("9223372036854775807"), Ok(Some(i64::MAX)));
    assert_eq!(read("-9223372036854775808"), Ok(Some(i64::MIN)));
    assert_eq!(read("1000000000000000000"), Ok(Some(1_000_000_000_000_000_000)));
    assert_eq!(read("9223372036854775808"), Err(()));
    assert_eq!(read("-9223372036854775809"), Err(()));
    assert_eq!(read("99999999999999999999999"), Err(()));
    assert_eq!(read("1.5"), Ok(None));
}

#[test]
fn spec_field_reads_a_list_entry_and_a_destructor_index() {
    let spec = b"stitle,lrows(o#12(iid,slabel)),icount";
    let title = nested_field_at(spec, 0);
    assert_eq!((title.kind, &spec[title.name_start..title.name_end], title.dtor), (b's', &b"title"[..], -1));
    let rows = nested_field_at(spec, title.end + 1);
    assert_eq!((rows.kind, &spec[rows.name_start..rows.name_end], rows.dtor), (b'l', &b"rows"[..], -1));
    let elem = &spec[rows.sub_start..rows.sub_end];
    assert_eq!(elem, b"o#12(iid,slabel)");
    let elem_field = nested_field_at(elem, 0);
    assert_eq!((elem_field.kind, elem_field.name_start, elem_field.name_end, elem_field.dtor), (b'o', 1, 1, 12));
    assert_eq!(&elem[elem_field.sub_start..elem_field.sub_end], b"iid,slabel");
    let count = nested_field_at(spec, rows.end + 1);
    assert_eq!((count.kind, &spec[count.name_start..count.name_end], count.end), (b'i', &b"count"[..], spec.len()));
}

#[test]
fn spec_field_reads_a_struct_under_a_list_with_its_index() {
    // a struct field inside a list element carries `#<k>` after its name
    let spec = b"scode,ogeo#3(ilat,slabel),ltags(s)";
    let geo = nested_field_at(spec, 6);
    assert_eq!((geo.kind, &spec[geo.name_start..geo.name_end], geo.dtor), (b'o', &b"geo"[..], 3));
    assert_eq!(&spec[geo.sub_start..geo.sub_end], b"ilat,slabel");
    let tags = nested_field_at(spec, geo.end + 1);
    assert_eq!((tags.kind, &spec[tags.sub_start..tags.sub_end]), (b'l', &b"s"[..]));
    // a list of lists nests the element form
    let grid = nested_field_at(b"lcells(l(i))", 0);
    assert_eq!(&b"lcells(l(i))"[grid.sub_start..grid.sub_end], b"l(i)");
}

#[test]
fn spec_field_reads_a_map_entry_and_a_map_element() {
    let spec = b"stitle,mscores(i),mrows(o#0(iid,slabel)),lgroups(m(i)),mnested(m(s))";
    let scores = nested_field_at(spec, 7);
    assert_eq!((scores.kind, &spec[scores.name_start..scores.name_end], &spec[scores.sub_start..scores.sub_end]), (b'm', &b"scores"[..], &b"i"[..]));
    let rows = nested_field_at(spec, scores.end + 1);
    assert_eq!((rows.kind, &spec[rows.sub_start..rows.sub_end]), (b'm', &b"o#0(iid,slabel)"[..]));
    let groups = nested_field_at(spec, rows.end + 1);
    assert_eq!((groups.kind, &spec[groups.sub_start..groups.sub_end]), (b'l', &b"m(i)"[..]));
    let elem = nested_field_at(&spec[groups.sub_start..groups.sub_end], 0);
    assert_eq!((elem.kind, elem.name_start, elem.name_end), (b'm', 1, 1));
    let nested = nested_field_at(spec, groups.end + 1);
    assert_eq!((nested.kind, &spec[nested.sub_start..nested.sub_end], nested.end), (b'm', &b"m(s)"[..], spec.len()));
    assert_eq!(nested_spec_field_count(spec), 5);
}

#[test]
fn map_member_paths_and_kind_words() {
    assert_eq!(nested_member_path(b"scores", b"bob"), b"scores.bob".to_vec());
    assert_eq!(nested_member_path(b"rows[1]", b"with space"), b"rows[1].with space".to_vec());
    assert_eq!(nested_kind_error_at(b'i', b"scores.bob"), b"expected int element: scores.bob".to_vec());
    assert_eq!(nested_kind_error_at(b'm', b"groups[1]"), b"expected map element: groups[1]".to_vec());
    assert_eq!(nested_prefixed_at(b"rows.b", b"missing string field: label"), b"rows.b: missing string field: label".to_vec());
    assert!(nested_elem_tag(b"m(i)") == crate::collections::list::ListTypeTag::Map);
    assert!(nested_elem_tag(b"o#2(iid)") == crate::collections::list::ListTypeTag::Struct);
    assert!(nested_elem_tag(b"l(s)") == crate::collections::list::ListTypeTag::List);
    assert!(nested_elem_tag(b"s") == crate::collections::list::ListTypeTag::String);
    assert!(nested_elem_tag(b"f") == crate::collections::list::ListTypeTag::Primitive);
    assert_eq!(NestedAbort::TooDeep.message(), NESTED_TOO_DEEP);
    assert_eq!(NestedAbort::Malformed.message(), b"invalid json object");
}

#[test]
fn spec_field_count_is_the_top_level_separators_plus_one() {
    assert_eq!(nested_spec_field_count(b""), 0);
    assert_eq!(nested_spec_field_count(b"iid"), 1);
    assert_eq!(nested_spec_field_count(b"iid,slabel"), 2);
    assert_eq!(nested_spec_field_count(b"scode,ogeo#3(ilat,slabel),ltags(s)"), 3);
    assert_eq!(nested_spec_field_count(b"lrows(o#0(iid,slabel))"), 1);
}

#[test]
fn element_errors_name_the_position_under_the_field() {
    assert_eq!(nested_element_error(b'i', b"xs", 2), b"expected int element: xs[2]".to_vec());
    assert_eq!(nested_element_error(b'o', b"rows", 0), b"expected object element: rows[0]".to_vec());
    assert_eq!(nested_element_prefixed(b"rows", 1, b"missing string field: name"), b"rows[1]: missing string field: name".to_vec());
    assert_eq!(nested_element_error(b'i', b"grid[1]", 3), b"expected int element: grid[1][3]".to_vec());
}

#[test]
fn float_span_takes_every_json_number_form() {
    assert_eq!(float_of_span(b"1.5"), Some(1.5));
    assert_eq!(float_of_span(b"-2.25"), Some(-2.25));
    assert_eq!(float_of_span(b"1e10"), Some(1e10));
    assert_eq!(float_of_span(b"2.5E-3"), Some(2.5e-3));
    assert_eq!(float_of_span(b"3"), Some(3.0));
    assert_eq!(float_of_span(b"-7"), Some(-7.0));
    assert_eq!(float_of_span(b"1.7976931348623157e308"), Some(f64::MAX));
    assert_eq!(float_of_span(b"5e-324"), Some(f64::from_bits(1)));
}

#[test]
fn negative_zero_keeps_its_sign() {
    let value = float_of_span(b"-0.0").unwrap();
    assert_eq!(value, 0.0);
    assert!(value.is_sign_negative());
}

#[test]
fn float_span_refuses_what_is_not_a_finite_number() {
    assert_eq!(float_of_span(b"1e309"), None);
    assert_eq!(float_of_span(b"-1e309"), None);
    assert_eq!(float_of_span(b"1-2"), None);
    assert_eq!(float_of_span(b"1e"), None);
    assert_eq!(float_of_span(b"1.2.3"), None);
    assert_eq!(float_of_span(b"-"), None);
    assert_eq!(float_of_span(b""), None);
}

#[test]
fn float_bits_converts_a_span_or_refuses_it() {
    let doc = b"x:-0.0,1e309,2.5";
    let ptr = doc.as_ptr() as i64;
    unsafe {
        assert_eq!(pith_json_float_bits(ptr, 2, 6), (-0.0f64).to_bits() as i64);
        assert_eq!(pith_json_float_bits(ptr, 7, 12), JSON_FLOAT_REFUSED);
        assert_eq!(pith_json_float_bits(ptr, 13, 16), 2.5f64.to_bits() as i64);
        assert_eq!(pith_json_float_bits(ptr, 13, 13), JSON_FLOAT_REFUSED);
        assert_eq!(pith_json_float_bits(0, 0, 1), JSON_FLOAT_REFUSED);
    }
    assert!(f64::from_bits(JSON_FLOAT_REFUSED as u64).is_nan());
}

#[test]
fn nested_decode_handles_structs_with_more_than_64_fields() {
    // 70 integer fields: previously `1u64 << idx` wrapped/panicked past 63.
    let mut spec = Vec::new();
    let mut doc = Vec::from(&b"{"[..]);
    for i in 0..70usize {
        if i > 0 {
            spec.push(b',');
            doc.push(b',');
        }
        spec.extend_from_slice(format!("if{i}").as_bytes());
        doc.extend_from_slice(format!("\"f{i}\":{i}").as_bytes());
    }
    doc.push(b'}');
    let dtors = DtorTable(&[]);
    unsafe {
        let struct_ptr = crate::runtime_core::pith_struct_alloc(70);
        assert_ne!(struct_ptr, 0);
        let target = ObjectTarget::from_raw(&spec, struct_ptr);
        assert_eq!(nested_fill_from_input(&doc, target, false, &dtors), Ok(()));
        let base = struct_ptr as *const i64;
        for i in 0..70usize {
            assert_eq!(*base.add(i), i as i64);
        }
        // Missing field #69 must be reported, not aliased onto field #5.
        let mut missing_doc = Vec::from(&b"{"[..]);
        for i in 0..69usize {
            if i > 0 {
                missing_doc.push(b',');
            }
            missing_doc.extend_from_slice(format!("\"f{i}\":{i}").as_bytes());
        }
        missing_doc.push(b'}');
        let target2 = ObjectTarget::from_raw(&spec, struct_ptr);
        let err = nested_fill_from_input(&missing_doc, target2, false, &dtors).unwrap_err();
        assert_eq!(err, b"missing int field: f69");
        crate::pith_struct_release(struct_ptr);
    }
}
