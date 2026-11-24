//! Shared time-budget policy for the grass performance tests.

/// CI runners vary; the strict budget applies to local and release runs.
fn grass_budget_allowance_for_ci(ci: Option<&str>) -> f64 {
    if ci == Some("true") { 1.5 } else { 1.0 }
}

/// Read the CI-only allowance without changing the process environment.
pub fn grass_budget_allowance() -> f64 {
    grass_budget_allowance_for_ci(std::env::var("CI").ok().as_deref())
}

#[test]
/// Only exact CI=true receives an allowance; unset and other values stay strict.
fn grass_budget_allowance_is_ci_only() {
    assert_eq!(grass_budget_allowance_for_ci(Some("true")), 1.5);
    for ci in [None, Some("false"), Some(""), Some("TRUE"), Some("1")] {
        assert_eq!(grass_budget_allowance_for_ci(ci), 1.0, "CI={ci:?}");
    }
}
