Implement a single function in a new file named `solution.rs`:

    fn parse_duration(s: &str) -> Option<u64>

It parses a duration string into a count of seconds. A valid input is a non-negative integer immediately followed by exactly one lowercase unit suffix, with no surrounding spaces:

- `s` = seconds, `m` = minutes (60s), `h` = hours (3600s), `d` = days (86400s)

Examples: `"90s"` -> `Some(90)`, `"5m"` -> `Some(300)`, `"2h"` -> `Some(7200)`, `"1d"` -> `Some(86400)`, `"0s"` -> `Some(0)`.

Return `None` for any malformed input, including: an empty string, a value with no unit, an unknown or uppercase unit, a non-numeric value, a negative number, surrounding whitespace, or a value large enough that the result would overflow a `u64` number of seconds.

Write only `solution.rs` containing `parse_duration` and any private helpers it needs. Do not write a `main`, a module declaration, or tests.
