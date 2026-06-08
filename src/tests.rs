use super::*;

/// Helper to spin up a clean registry and evaluate a sequence of commands
fn run_session(inputs_and_expected: &[(&str, &str)]) {
    // Create an isolated registry mimicking the default behavior without file IO
    let mut registry = vec![
        UnitDef { primary_name: "d".into(), factor: BigRational::from_integer(BigInt::from(86400)), aliases: vec!["day".into(), "days".into()] },
        UnitDef { primary_name: "h".into(), factor: BigRational::from_integer(BigInt::from(3600)), aliases: vec!["hr".into(), "hrs".into(), "hour".into(), "hours".into()] },
        UnitDef { primary_name: "m".into(), factor: BigRational::from_integer(BigInt::from(60)), aliases: vec!["min".into(), "mins".into(), "minute".into(), "minutes".into()] },
        UnitDef { primary_name: "s".into(), factor: BigRational::from_integer(BigInt::from(1)), aliases: vec!["sec".into(), "secs".into(), "second".into(), "seconds".into()] },
        UnitDef { primary_name: "devday".into(), factor: BigRational::from_integer(BigInt::from(28800)), aliases: vec!["dd".into(), "dev".into(), "devdays".into(), "manday".into(), "mandays".into()] },
    ];
    registry.sort_by(|a, b| b.factor.cmp(&a.factor));
    let mut funcs = Vec::new();
    let mut last_val: Option<Value> = None;

    for (input, expected) in inputs_and_expected {
        match evaluate(input, last_val.clone(), &registry, &mut funcs) {
            Ok((res, val_opt)) => {
                assert_eq!(res, *expected, "\nFailed on input: `{}`", input);
                if let Some(val) = val_opt {
                    last_val = Some(val);
                }
            }
            Err(e) => {
                assert_eq!(e, *expected, "\nError mismatch on input: `{}`", input);
            }
        }
    }
}

#[test]
fn test_basic_arithmetic_and_aliases() {
    run_session(&[
        ("8h - 1h 30m", "6 h 30 m"),
        ("45m + 45m", "1 h 30 m"),
        ("1h - 90m", "-30 m"),
        ("2h30m - 15m", "2 h 15 m"),
        ("1 manday + 1h", "9 h"), // devday (8h) + 1h = 9h
    ]);
}

#[test]
fn test_unit_inference_and_scalars() {
    run_session(&[
        ("2h + 1", "3 h"),
        ("1.2 * 2", "2.4"),
        ("1h / 3", "20 m"), // division cascades natively to next best unit
        ("2h 3m + 1", "Ambiguous unitless number (cannot infer unit)"),
        ("2h 30m as hour + 1", "3.5"),
    ]);
}

#[test]
fn test_formatting_as_and_to() {
    run_session(&[
        ("(10.2h) as h m s", "10 h 12 m"), // Drops s because there is no remainder
        ("10.3h as h m", "10 h 18 m"),
        ("5 min 2h as hour", "approx. 2.083333333 h"),
        ("46m 58s + 1h 44m to h m", "approx. 2 h 31 m"),
        ("1h / 3 to 2dp as h", "approx. 0.33 h"), 
    ]);
}

#[test]
fn test_rounding_precision() {
    run_session(&[
        ("1.44442423h to 3 decimal", "approx. 1.444 h"),
        ("1.44442423h to 3 dp", "approx. 1.444 h"),
        ("1.333h to hour", "1 h"),
        ("1.567h to hour", "2 h"),
        ("0.02 to 1dp", "approx. 0"),
        ("0.02 to 1 dp", "approx. 0"), 
        ("1h 1.1m to 0dp", "approx. 1 h 1 m"),
    ]);
}

#[test]
fn test_strings_and_concatenation() {
    run_session(&[
        ("\"Some text\"", "Some text"),
        ("\"Some\" + \" text\"", "Some text"),
        ("\"Some\" ' ' \"text\"", "Some text"), 
        ("1d 1h", "1 d 1 h"),
        ("1d 1h + \"xyz\"", "1 d 1 hxyz"),
        ("1d 1h to h", "25 h"),
        ("(1d 1h to h) 'foo'", "25 hfoo"),
        ("1/3 'bar'", "approx. 0.333333333bar"),
        ("0.11 '%'", "0.11%"),
        ("0.11 to 1dp '%'", "approx. 0.1%"),
    ]);
}

#[test]
fn test_history_and_modulo() {
    run_session(&[
        ("1h % 30m", "0 s"),
        ("1h % 40m", "20 m"),
        ("1h + 30m", "1 h 30 m"),
        ("_ + 5s", "1 h 30 m 5 s"),
    ]);
}

#[test]
fn test_functions_and_currying() {
    run_session(&[
        ("add a b := a + b", ""),
        ("add 1 2", "3"),
        ("red b := add 1", ""),
        ("red 2", "<function add>"), // Curried function evaluation 
        ("test x := x to 0dp", ""),
        ("test 1", "1"),
        ("test 1.1", "approx. 1"),
        ("test 1.8", "approx. 2"),
        ("dev est tru := (tru - est)/est * 100 to 2 decimal '%'", ""),
        ("dev 3d6h 5d30m", "approx. 54.49%"),
        ("1d add a := a*2", "Invalid function definition: left side must be identifiers"),
    ]);
}

#[test]
fn test_space_associativity() {
    run_session(&[
        ("ident a := a", ""),
        ("ident 5h30m", "5 h 30 m"),
        ("ident 5h 30m", "5 h 30 m"),
    ]);
}
