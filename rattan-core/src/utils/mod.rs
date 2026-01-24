use std::borrow::Cow;

use regex::Regex;

pub mod env_var;
#[cfg(feature = "serde")]
pub(crate) mod serde;
pub mod sync;

pub fn replace_env_var_in_string(input: &str) -> Cow<'_, str> {
    let re = Regex::new(r"\$\{([a-zA-Z_][a-zA-Z0-9_]*)\}").unwrap();
    re.replace_all(input, |caps: &regex::Captures| {
        let var_name = &caps[1];
        std::env::var(var_name).unwrap_or_else(|_| "".to_string())
    })
}

mod tests {

    #[test]
    fn test_replace_env_var_in_string() {
        use super::replace_env_var_in_string;
        std::env::set_var("RATTAN_TEST_VAR", "test_value");
        assert_eq!(
            replace_env_var_in_string("test_${RATTAN_TEST_VAR}_value"),
            "test_test_value_value"
        );
    }
}
