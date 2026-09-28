use p57_rust_mini::parse_duration_ms;

#[test]
fn units_scale_to_milliseconds() {
    assert_eq!(parse_duration_ms("1500ms"), Ok(1500));
    assert_eq!(parse_duration_ms("90s"), Ok(90_000));
    assert_eq!(parse_duration_ms("2m"), Ok(120_000));
}

#[test]
fn bad_input_is_an_error_not_a_zero() {
    assert!(parse_duration_ms("90").is_err());
    assert!(parse_duration_ms("s").is_err());
    assert!(parse_duration_ms("3d").is_err());
    assert!(parse_duration_ms("18446744073709551615s").is_err());
}
