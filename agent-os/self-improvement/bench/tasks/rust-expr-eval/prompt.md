Implement a single function in a new file named `solution.rs`:

    fn eval(s: &str) -> Option<i64>

It evaluates a flat integer arithmetic expression. The input is a string of one or more non-negative integers separated by the binary operators `+`, `-`, `*`, `/`, with no spaces.

The operators have NO precedence. The expression is evaluated strictly left to right. Division is integer division that truncates toward zero.

Return `None` for any malformed input, including: an empty string, a leading or trailing operator, two operators in a row, a division by zero, or any character that is not a digit or one of the four operators.

Write only `solution.rs` containing `eval` and any private helpers it needs. Do not write a `main`, a module declaration, or tests.
