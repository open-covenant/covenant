include!("solution.rs");

#[test] fn secs() { assert_eq!(parse_duration("90s"), Some(90)); }
#[test] fn mins() { assert_eq!(parse_duration("5m"), Some(300)); }
#[test] fn hours() { assert_eq!(parse_duration("2h"), Some(7200)); }
#[test] fn days() { assert_eq!(parse_duration("1d"), Some(86400)); }
#[test] fn zero() { assert_eq!(parse_duration("0s"), Some(0)); }
#[test] fn multi_digit() { assert_eq!(parse_duration("120s"), Some(120)); }
#[test] fn empty() { assert_eq!(parse_duration(""), None); }
#[test] fn no_unit() { assert_eq!(parse_duration("100"), None); }
#[test] fn unknown_unit() { assert_eq!(parse_duration("5x"), None); }
#[test] fn uppercase_unit() { assert_eq!(parse_duration("5M"), None); }
#[test] fn non_numeric() { assert_eq!(parse_duration("abc"), None); }
#[test] fn negative() { assert_eq!(parse_duration("-5m"), None); }
#[test] fn whitespace_prefix() { assert_eq!(parse_duration(" 5m"), None); }
#[test] fn overflow_on_multiply() { assert_eq!(parse_duration("9999999999999999d"), None); }
