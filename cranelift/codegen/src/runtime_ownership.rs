//! What each runtime function does with the reference counts of its
//! arguments and its result, from cranelift/runtime-abi/runtime_ownership.txt.
//! The count checker reads this instead of trusting the emitter, which is the
//! thing it checks. build.rs refuses a runtime function without a row.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArgOwnership {
    /// not a counted value
    Plain,
    /// read during the call; the caller's count is left alone
    Borrow,
    /// the callee takes over the caller's count
    Consume,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResultValue {
    /// void, or not a counted value
    Plain,
    /// the caller gets a count it must give up
    Own,
    /// valid only while argument N lives and is unchanged
    Borrow(usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResultOwnership {
    /// the function returns the (flag, payload) register pair, and `value`
    /// describes the payload
    pub pair: bool,
    pub value: ResultValue,
}

#[derive(Clone, Copy, Debug)]
pub struct RuntimeOwnership {
    pub key: &'static str,
    pub args: &'static [ArgOwnership],
    pub result: ResultOwnership,
}

include!(concat!(env!("OUT_DIR"), "/runtime_ownership_table.rs"));

/// the row for a runtime function, by the name the IR calls it
pub fn runtime_ownership(key: &str) -> Option<&'static RuntimeOwnership> {
    RUNTIME_OWNERSHIP.iter().find(|row| row.key == key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_runtime_function_has_a_row() {
        // build.rs already refuses a missing row; this keeps the guarantee
        // visible from the test suite too
        for key in crate::runtime_imports::runtime_function_keys() {
            assert!(
                runtime_ownership(key).is_some(),
                "no ownership row for {key}"
            );
        }
    }

    #[test]
    fn the_rows_the_count_checker_leans_on() {
        let push = runtime_ownership("pith_list_push_value_owned").expect("row");
        assert_eq!(push.args, &[ArgOwnership::Borrow, ArgOwnership::Consume]);
        let get = runtime_ownership("pith_list_get_value").expect("row");
        assert_eq!(get.result.value, ResultValue::Borrow(0));
        let concat = runtime_ownership("pith_concat_cstr").expect("row");
        assert_eq!(concat.args, &[ArgOwnership::Borrow, ArgOwnership::Borrow]);
        assert_eq!(concat.result.value, ResultValue::Own);
        let release = runtime_ownership("pith_cstring_release").expect("row");
        assert_eq!(release.args, &[ArgOwnership::Consume]);
    }
}
