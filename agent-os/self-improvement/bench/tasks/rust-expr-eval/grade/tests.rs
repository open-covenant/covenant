include!("solution.rs");

#[test] fn single() { assert_eq!(eval("2"), Some(2)); }
#[test] fn add() { assert_eq!(eval("2+3"), Some(5)); }
#[test] fn ltr_add_mul() { assert_eq!(eval("2+3*4"), Some(20)); }   // (2+3)*4 = 20, NOT 14
#[test] fn ltr_sub_mul() { assert_eq!(eval("4-2*3"), Some(6)); }    // (4-2)*3 = 6, NOT -2
#[test] fn ltr_chain() { assert_eq!(eval("10-2-3"), Some(5)); }
#[test] fn int_div() { assert_eq!(eval("7/2"), Some(3)); }
#[test] fn div_chain() { assert_eq!(eval("8/2/2"), Some(2)); }
#[test] fn multi_digit() { assert_eq!(eval("12+34"), Some(46)); }
#[test] fn empty() { assert_eq!(eval(""), None); }
#[test] fn leading_op() { assert_eq!(eval("+2"), None); }
#[test] fn trailing_op() { assert_eq!(eval("2+"), None); }
#[test] fn double_op() { assert_eq!(eval("2++3"), None); }
#[test] fn div_zero() { assert_eq!(eval("5/0"), None); }
#[test] fn bad_char() { assert_eq!(eval("2+a"), None); }
