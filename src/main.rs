use chrono::Timelike;
use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute, queue,
    style::{Color, Print, ResetColor, SetForegroundColor},
    terminal::{self, Clear, ClearType},
};
use directories::ProjectDirs;
use num_bigint::BigInt;
use num_rational::BigRational;
use num_traits::{One, ToPrimitive, Zero};
use serde::Deserialize;
use std::collections::HashMap;
use std::io::{self, Write};
use std::str::FromStr;

const PROJECT_SHORT_NAME: &'static str = "tt";
const PROJECT_LONG_NAME: &'static str = "Tea Time";

struct RawModeGuard;
impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode();
    }
}

// --- 0. Configuration & Registry ---

#[derive(Deserialize, Debug)]
struct Config {
    units: Option<HashMap<String, CustomUnitConfig>>,
    functions: Option<HashMap<String, FuncConfig>>,
}

#[derive(Deserialize, Debug)]
struct CustomUnitConfig {
    value: String,
    #[serde(default)]
    alias: Vec<String>,
}

#[derive(Deserialize, Debug)]
struct FuncConfig {
    name: String,
    #[serde(default)]
    aliases: Vec<String>,
    #[serde(default)]
    arguments: Vec<String>,
    definition: String,
}

#[derive(Debug, Clone)]
struct UnitDef {
    primary_name: String,
    factor: BigRational,
    aliases: Vec<String>,
}

#[derive(Debug, Clone)]
struct FuncDef {
    name: String,
    aliases: Vec<String>,
    args: Vec<String>,
    body: Expr,
}

#[derive(Debug, Clone, PartialEq)]
struct FormatUnit {
    name: String,
    factor: BigRational,
}

impl FormatUnit {
    fn info(&self) -> (&BigRational, &str) {
        (&self.factor, &self.name)
    }
}

// --- 1. Abstract Syntax Tree (AST) & Values ---

#[derive(Clone, Debug)]
enum Expr {
    Number(BigRational),
    Duration(BigRational), 
    Ident(String),
    Now,
    Underscore,
    UnaryMinus(Box<Expr>),
    Add(Box<Expr>, Box<Expr>),
    Sub(Box<Expr>, Box<Expr>),
    Mul(Box<Expr>, Box<Expr>),
    Div(Box<Expr>, Box<Expr>),
    Mod(Box<Expr>, Box<Expr>),
    Apply(Box<Expr>, Box<Expr>),
}

#[derive(Clone, Debug)]
enum Value {
    Number(BigRational),
    Duration(BigRational),
    Function(FuncDef, Vec<Value>),
}

impl Expr {
    fn collect_units(&self, units: &mut Vec<FormatUnit>, registry: &[UnitDef], funcs: &[FuncDef]) {
        match self {
            Expr::Ident(name) => {
                if let Ok(u) = parse_single_unit(name, registry) {
                    let is_func = funcs.iter().any(|f| f.name.eq_ignore_ascii_case(name) || f.aliases.iter().any(|a| a.eq_ignore_ascii_case(name)));
                    if !is_func && !units.iter().any(|existing| existing.name == u.name) {
                        units.push(u);
                    }
                }
            }
            Expr::UnaryMinus(e) => e.collect_units(units, registry, funcs),
            Expr::Add(a, b) | Expr::Sub(a, b) | Expr::Mul(a, b) | Expr::Div(a, b) | Expr::Mod(a, b) | Expr::Apply(a, b) => {
                a.collect_units(units, registry, funcs);
                b.collect_units(units, registry, funcs);
            }
            _ => {}
        }
    }
}

// --- 2. Tokenization & Loading ---

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Num(BigRational),
    Ident(String),
    Plus, Minus, Multiply, Divide, Modulo, Underscore, LParen, RParen, To, Now,
    Duration(BigRational),
}

#[derive(Default, Debug, Clone)]
struct ConversionTarget {
    dp: Option<i32>,
    units: Option<Vec<FormatUnit>>,
}

fn parse_decimal(s: &str) -> Result<BigRational, String> {
    let s = s.trim();
    let (sign, s) = if s.starts_with('-') { (BigInt::from(-1), &s[1..]) } else if s.starts_with('+') { (BigInt::from(1), &s[1..]) } else { (BigInt::from(1), s) };

    if let Some((int_part, fract_part)) = s.split_once('.') {
        let int_str = if int_part.is_empty() { "0" } else { int_part };
        let int_num = BigInt::from_str(int_str).map_err(|_| "invalid integer part")?;
        let fract_num = if fract_part.is_empty() { BigInt::zero() } else { BigInt::from_str(fract_part).map_err(|_| "invalid fractional part")? };
        let ten = BigInt::from(10);
        let denom = ten.pow(fract_part.len() as u32);
        Ok(BigRational::new(sign * (int_num * &denom + fract_num), denom))
    } else {
        let int_num = BigInt::from_str(s).map_err(|_| "invalid number")?;
        Ok(BigRational::from_integer(sign * int_num))
    }
}

fn tokenize(input: &str) -> Result<Vec<Token>, String> {
    let mut tokens = Vec::new();
    let mut chars = input.chars().peekable();

    while let Some(&c) = chars.peek() {
        if c.is_whitespace() { chars.next(); } 
        else if c == '+' { tokens.push(Token::Plus); chars.next(); } 
        else if c == '-' { tokens.push(Token::Minus); chars.next(); } 
        else if c == '*' { tokens.push(Token::Multiply); chars.next(); } 
        else if c == '/' { tokens.push(Token::Divide); chars.next(); } 
        else if c == '%' { tokens.push(Token::Modulo); chars.next(); } 
        else if c == '_' { tokens.push(Token::Underscore); chars.next(); } 
        else if c == '(' { tokens.push(Token::LParen); chars.next(); } 
        else if c == ')' { tokens.push(Token::RParen); chars.next(); } 
        else if c.is_ascii_digit() || c == '.' {
            let mut num_str = String::new();
            while let Some(&ch) = chars.peek() {
                if ch.is_ascii_digit() || ch == '.' { num_str.push(ch); chars.next(); } else { break; }
            }
            tokens.push(Token::Num(parse_decimal(&num_str)?));
        } else if c.is_alphabetic() {
            let mut unit_str = String::new();
            while let Some(&ch) = chars.peek() {
                if ch.is_alphabetic() { unit_str.push(ch); chars.next(); } else { break; }
            }
            if unit_str.eq_ignore_ascii_case("as") || unit_str.eq_ignore_ascii_case("to") { tokens.push(Token::To); } 
            else if unit_str.eq_ignore_ascii_case("now") { tokens.push(Token::Now); } 
            else { tokens.push(Token::Ident(unit_str)); }
        } else {
            return Err(format!("Unexpected character: '{}'", c));
        }
    }
    Ok(tokens)
}

fn build_registry() -> (Vec<UnitDef>, Vec<FuncDef>) {
    let mut registry = vec![
        UnitDef { primary_name: "d".into(), factor: BigRational::from_integer(BigInt::from(86400)), aliases: vec!["day".into(), "days".into()] },
        UnitDef { primary_name: "h".into(), factor: BigRational::from_integer(BigInt::from(3600)), aliases: vec!["hr".into(), "hrs".into(), "hour".into(), "hours".into()] },
        UnitDef { primary_name: "m".into(), factor: BigRational::from_integer(BigInt::from(60)), aliases: vec!["min".into(), "mins".into(), "minute".into(), "minutes".into()] },
        UnitDef { primary_name: "s".into(), factor: BigRational::from_integer(BigInt::from(1)), aliases: vec!["sec".into(), "secs".into(), "second".into(), "seconds".into()] },
    ];
    let mut funcs = Vec::new();

    if let Some(proj_dirs) = ProjectDirs::from("", "", PROJECT_SHORT_NAME) {
        let config_path = proj_dirs.config_dir().join("config.toml");
        if let Ok(config_str) = std::fs::read_to_string(config_path) {
            if let Ok(config) = toml::from_str::<Config>(&config_str) {
                if let Some(units) = config.units {
                    for (name, conf) in units {
                        if let Ok((_, Value::Duration(seconds))) = evaluate(&conf.value, None, &registry, &funcs) {
                            registry.push(UnitDef { primary_name: name, factor: seconds, aliases: conf.alias });
                        }
                    }
                }
                if let Some(functions) = config.functions {
                    for (_, conf) in functions {
                        if let Ok(tokens) = tokenize(&conf.definition) {
                            if let Ok((ast, rest)) = parse_expr(&tokens) {
                                if rest.is_empty() {
                                    funcs.push(FuncDef { name: conf.name, aliases: conf.aliases, args: conf.arguments, body: ast });
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    registry.sort_by(|a, b| b.factor.cmp(&a.factor));
    (registry, funcs)
}

// --- 3. Parsing (Recursive Descent AST) ---

fn parse_single_unit(u: &str, registry: &[UnitDef]) -> Result<FormatUnit, String> {
    let u_lower = u.to_lowercase();
    for def in registry {
        if u_lower == def.primary_name.to_lowercase() || def.aliases.iter().any(|a| a.to_lowercase() == u_lower) {
            return Ok(FormatUnit { name: def.primary_name.clone(), factor: def.factor.clone() });
        }
    }
    Err(format!("Unknown unit: '{}'", u))
}

fn extract_keywords(tokens: &mut Vec<Token>, registry: &[UnitDef]) -> Result<ConversionTarget, String> {
    let mut target = ConversionTarget::default();
    let mut i = 0;
    while i < tokens.len() {
        if tokens[i] == Token::To {
            tokens.remove(i);
            let mut args = Vec::new();
            while i < tokens.len() && !matches!(tokens[i], Token::Plus | Token::Minus | Token::Multiply | Token::Divide | Token::Modulo | Token::LParen | Token::RParen | Token::To) {
                args.push(tokens.remove(i));
            }
            
            if args.is_empty() {
                return Err("Missing target after conversion keyword".to_string());
            }

            if args.len() == 2 {
                if let (Token::Num(n), Token::Ident(u)) = (&args[0], &args[1]) {
                    if u.eq_ignore_ascii_case("decimal") || u.eq_ignore_ascii_case("dp") {
                        target.dp = Some(n.to_integer().to_i32().unwrap_or(0));
                        continue;
                    }
                }
            }
            
            let mut units = Vec::new();
            for tok in args {
                if let Token::Ident(u) = tok { units.push(parse_single_unit(&u, registry)?); } else { return Err("Invalid conversion target".into()); }
            }
            target.units = Some(units);
        } else {
            i += 1;
        }
    }
    Ok(target)
}

fn form_durations(tokens: Vec<Token>, registry: &[UnitDef]) -> Result<(Vec<Token>, Vec<FormatUnit>), String> {
    let mut new_tokens = Vec::new();
    let mut explicit_units = Vec::new();
    let mut iter = tokens.into_iter().peekable();

    while let Some(tok) = iter.next() {
        if let Token::Num(val) = tok {
            if let Some(Token::Ident(unit)) = iter.peek() {
                if let Ok(fmt_unit) = parse_single_unit(unit, registry) {
                    if !explicit_units.contains(&fmt_unit) { explicit_units.push(fmt_unit.clone()); }
                    let (sec, _) = fmt_unit.info();
                    new_tokens.push(Token::Duration(val * sec));
                    iter.next(); 
                    continue;
                }
            }
            new_tokens.push(Token::Num(val)); 
        } else {
            new_tokens.push(tok);
        }
    }
    Ok((new_tokens, explicit_units))
}

fn combine_contiguous_durations(tokens: Vec<Token>) -> Vec<Token> {
    let mut new_tokens: Vec<Token> = Vec::new();
    for tok in tokens {
        if let Token::Duration(val) = tok {
            if let Some(Token::Duration(last_val)) = new_tokens.last_mut() {
                *last_val = last_val.clone() + val; 
            } else {
                new_tokens.push(Token::Duration(val));
            }
        } else {
            new_tokens.push(tok);
        }
    }
    new_tokens
}

fn parse_expr(tokens: &[Token]) -> Result<(Expr, &[Token]), String> {
    let (mut lhs, mut rest) = parse_mul_expr(tokens)?;
    while !rest.is_empty() {
        if rest[0] == Token::Plus {
            let (rhs, new_rest) = parse_mul_expr(&rest[1..])?;
            lhs = Expr::Add(Box::new(lhs), Box::new(rhs));
            rest = new_rest;
        } else if rest[0] == Token::Minus {
            let (rhs, new_rest) = parse_mul_expr(&rest[1..])?;
            lhs = Expr::Sub(Box::new(lhs), Box::new(rhs));
            rest = new_rest;
        } else { break; }
    }
    Ok((lhs, rest))
}

fn parse_mul_expr(tokens: &[Token]) -> Result<(Expr, &[Token]), String> {
    let (mut lhs, mut rest) = parse_app_expr(tokens)?;
    while !rest.is_empty() {
        if rest[0] == Token::Multiply {
            let (rhs, new_rest) = parse_app_expr(&rest[1..])?;
            lhs = Expr::Mul(Box::new(lhs), Box::new(rhs));
            rest = new_rest;
        } else if rest[0] == Token::Divide {
            let (rhs, new_rest) = parse_app_expr(&rest[1..])?;
            lhs = Expr::Div(Box::new(lhs), Box::new(rhs));
            rest = new_rest;
        } else if rest[0] == Token::Modulo {
            let (rhs, new_rest) = parse_app_expr(&rest[1..])?;
            lhs = Expr::Mod(Box::new(lhs), Box::new(rhs));
            rest = new_rest;
        } else { break; }
    }
    Ok((lhs, rest))
}

fn parse_app_expr(tokens: &[Token]) -> Result<(Expr, &[Token]), String> {
    let (mut lhs, mut rest) = parse_primary(tokens)?;
    while !rest.is_empty() {
        match rest[0] {
            Token::Num(_) | Token::Duration(_) | Token::Ident(_) | Token::Now | Token::Underscore | Token::LParen => {
                let (rhs, new_rest) = parse_primary(rest)?;
                lhs = Expr::Apply(Box::new(lhs), Box::new(rhs));
                rest = new_rest;
            }
            _ => break,
        }
    }
    Ok((lhs, rest))
}

fn parse_primary(tokens: &[Token]) -> Result<(Expr, &[Token]), String> {
    if tokens.is_empty() { return Err("Unexpected end of expression".into()); }
    match &tokens[0] {
        Token::Num(n) => Ok((Expr::Number(n.clone()), &tokens[1..])),
        Token::Duration(d) => Ok((Expr::Duration(d.clone()), &tokens[1..])), 
        Token::Ident(s) => Ok((Expr::Ident(s.clone()), &tokens[1..])),
        Token::Now => Ok((Expr::Now, &tokens[1..])),
        Token::Underscore => Ok((Expr::Underscore, &tokens[1..])),
        Token::LParen => {
            let (expr, rest) = parse_expr(&tokens[1..])?;
            if rest.is_empty() || rest[0] != Token::RParen { return Err("Missing closing parenthesis".into()); }
            Ok((expr, &rest[1..]))
        }
        Token::Minus => {
            let (expr, rest) = parse_primary(&tokens[1..])?;
            Ok((Expr::UnaryMinus(Box::new(expr)), rest))
        }
        Token::Plus => parse_primary(&tokens[1..]),
        _ => Err(format!("Unexpected syntax token: {:?}", tokens[0])),
    }
}

// --- 4. Evaluator (Resolves functions, args & math) ---

fn apply_values(l: Value, r: Value, env: &HashMap<String, Value>, registry: &[UnitDef], funcs: &[FuncDef], last_val: Option<&Value>, inference_factor: Option<&BigRational>) -> Result<Value, String> {
    match (l, r) {
        (Value::Number(a), Value::Number(b)) => Ok(Value::Number(a * b)),
        (Value::Number(a), Value::Duration(b)) => Ok(Value::Duration(a * b)),
        (Value::Duration(a), Value::Number(b)) => Ok(Value::Duration(a * b)),
        (Value::Duration(a), Value::Duration(b)) => Ok(Value::Duration(a + b)), 
        (Value::Function(f, mut args), v) => {
            args.push(v);
            if args.len() == f.args.len() {
                let mut new_env = env.clone();
                for (name, val) in f.args.iter().zip(args.into_iter()) {
                    new_env.insert(name.clone(), val);
                }
                eval(&f.body, &new_env, registry, funcs, last_val, inference_factor)
            } else {
                Ok(Value::Function(f, args))
            }
        }
        _ => Err("Invalid function application or juxtaposition".into()),
    }
}

fn eval(expr: &Expr, env: &HashMap<String, Value>, registry: &[UnitDef], funcs: &[FuncDef], last_val: Option<&Value>, inference_factor: Option<&BigRational>) -> Result<Value, String> {
    match expr {
        Expr::Number(n) => Ok(Value::Number(n.clone())),
        Expr::Duration(d) => Ok(Value::Duration(d.clone())),
        Expr::Ident(name) => {
            if let Some(val) = env.get(name) { return Ok(val.clone()); }
            let name_lower = name.to_lowercase();
            for def in registry {
                if def.primary_name.to_lowercase() == name_lower || def.aliases.iter().any(|a| a.to_lowercase() == name_lower) {
                    return Ok(Value::Duration(def.factor.clone()));
                }
            }
            for def in funcs {
                if def.name.to_lowercase() == name_lower || def.aliases.iter().any(|a| a.to_lowercase() == name_lower) {
                    if def.args.is_empty() { return eval(&def.body, env, registry, funcs, last_val, inference_factor); }
                    return Ok(Value::Function(def.clone(), vec![]));
                }
            }
            Err(format!("Unknown identifier: {}", name))
        }
        Expr::Now => Ok(Value::Duration(BigRational::from_integer(BigInt::from(chrono::Local::now().num_seconds_from_midnight())))),
        Expr::Underscore => last_val.cloned().ok_or("No previous value to reference".into()),
        Expr::UnaryMinus(e) => {
            match eval(e, env, registry, funcs, last_val, inference_factor)? {
                Value::Number(n) => Ok(Value::Number(-n)),
                Value::Duration(d) => Ok(Value::Duration(-d)),
                Value::Function(..) => Err("Cannot negate a function".into()),
            }
        }
        Expr::Add(lhs, rhs) => {
            match (eval(lhs, env, registry, funcs, last_val, inference_factor)?, eval(rhs, env, registry, funcs, last_val, inference_factor)?) {
                (Value::Number(a), Value::Number(b)) => Ok(Value::Number(a + b)),
                (Value::Duration(a), Value::Duration(b)) => Ok(Value::Duration(a + b)),
                (Value::Duration(a), Value::Number(b)) => {
                    if let Some(f) = inference_factor { Ok(Value::Duration(a + b * f)) } else { Err("Ambiguous unitless number (cannot infer unit)".into()) }
                },
                (Value::Number(a), Value::Duration(b)) => {
                    if let Some(f) = inference_factor { Ok(Value::Duration(a * f + b)) } else { Err("Ambiguous unitless number (cannot infer unit)".into()) }
                },
                _ => Err("Cannot add these types".into())
            }
        }
        Expr::Sub(lhs, rhs) => {
            match (eval(lhs, env, registry, funcs, last_val, inference_factor)?, eval(rhs, env, registry, funcs, last_val, inference_factor)?) {
                (Value::Number(a), Value::Number(b)) => Ok(Value::Number(a - b)),
                (Value::Duration(a), Value::Duration(b)) => Ok(Value::Duration(a - b)),
                (Value::Duration(a), Value::Number(b)) => {
                    if let Some(f) = inference_factor { Ok(Value::Duration(a - b * f)) } else { Err("Ambiguous unitless number (cannot infer unit)".into()) }
                },
                (Value::Number(a), Value::Duration(b)) => {
                    if let Some(f) = inference_factor { Ok(Value::Duration(a * f - b)) } else { Err("Ambiguous unitless number (cannot infer unit)".into()) }
                },
                _ => Err("Cannot subtract these types".into())
            }
        }
        Expr::Mul(lhs, rhs) => {
            match (eval(lhs, env, registry, funcs, last_val, inference_factor)?, eval(rhs, env, registry, funcs, last_val, inference_factor)?) {
                (Value::Number(a), Value::Number(b)) => Ok(Value::Number(a * b)),
                (Value::Number(a), Value::Duration(b)) | (Value::Duration(b), Value::Number(a)) => Ok(Value::Duration(a * b)),
                _ => Err("Cannot multiply two time durations explicitly".into())
            }
        }
        Expr::Div(lhs, rhs) => {
            match (eval(lhs, env, registry, funcs, last_val, inference_factor)?, eval(rhs, env, registry, funcs, last_val, inference_factor)?) {
                (Value::Number(a), Value::Number(b)) => if b.is_zero() { Err("Division by zero".into()) } else { Ok(Value::Number(a / b)) },
                (Value::Duration(a), Value::Number(b)) => if b.is_zero() { Err("Division by zero".into()) } else { Ok(Value::Duration(a / b)) },
                (Value::Duration(a), Value::Duration(b)) => if b.is_zero() { Err("Division by zero".into()) } else { Ok(Value::Number(a / b)) },
                _ => Err("Cannot divide a number by a time duration".into())
            }
        }
        Expr::Mod(lhs, rhs) => {
            match (eval(lhs, env, registry, funcs, last_val, inference_factor)?, eval(rhs, env, registry, funcs, last_val, inference_factor)?) {
                (Value::Number(a), Value::Number(b)) => if b.is_zero() { Err("Modulo by zero".into()) } else { Ok(Value::Number(a % b)) },
                (Value::Duration(a), Value::Duration(b)) => if b.is_zero() { Err("Modulo by zero".into()) } else { Ok(Value::Duration(a % b)) },
                (Value::Duration(a), Value::Number(b)) => {
                    if let Some(f) = inference_factor {
                        let scaled_b = b * f;
                        if scaled_b.is_zero() { Err("Modulo by zero".into()) } else { Ok(Value::Duration(a % scaled_b)) }
                    } else {
                        Err("Ambiguous unitless number (cannot infer unit)".into())
                    }
                },
                (Value::Number(a), Value::Duration(b)) => {
                    if let Some(f) = inference_factor {
                        let scaled_a = a * f;
                        if b.is_zero() { Err("Modulo by zero".into()) } else { Ok(Value::Duration(scaled_a % b)) }
                    } else {
                        Err("Ambiguous unitless number (cannot infer unit)".into())
                    }
                },
                _ => Err("Cannot modulo these types".into())
            }
        }
        Expr::Apply(lhs, rhs) => apply_values(eval(lhs, env, registry, funcs, last_val, inference_factor)?, eval(rhs, env, registry, funcs, last_val, inference_factor)?, env, registry, funcs, last_val, inference_factor)
    }
}

// --- 5. Formatting Helpers & Output ---

fn format_exact_decimal(r: &BigRational, places: i32) -> (String, bool) {
    if places < 0 { return (r.to_integer().to_string(), false); }
    let mult = BigInt::from(10).pow(places as u32);
    let shifted = r * BigRational::from_integer(mult);
    let val_int = shifted.to_integer();
    let fract = &shifted - BigRational::from_integer(val_int.clone());
    
    let half = BigRational::new(BigInt::from(1), BigInt::from(2));
    let rounded = if fract >= half { val_int + BigInt::from(1) } else { val_int };
    let is_approx = !fract.is_zero();
    
    let mut s = rounded.to_string();
    let places_usize = places as usize;
    
    if places_usize > 0 {
        while s.len() <= places_usize { s.insert(0, '0'); }
        let split_idx = s.len() - places_usize;
        s.insert(split_idx, '.');
        s = s.trim_end_matches('0').to_string();
        if s.ends_with('.') { s.pop(); }
    }
    if s.is_empty() { s = "0".to_string(); }
    (s, is_approx)
}

fn format_output(result: &Value, conv_req: &ConversionTarget, explicit_units: &[FormatUnit], registry: &[UnitDef]) -> String {
    match result {
        Value::Number(n) => {
            let is_neg = n < &BigRational::zero();
            let abs_n = if is_neg { -n } else { n.clone() };
            let places = conv_req.dp.unwrap_or(9);
            let (val_str, is_approx) = format_exact_decimal(&abs_n, places);
            
            let mut is_approx_final = is_approx;
            if conv_req.dp.is_some() {
                let parsed = parse_decimal(&val_str).unwrap_or(BigRational::zero());
                is_approx_final = abs_n != parsed;
            }
            
            let prefix = if is_approx_final { "approx. " } else { "" };
            format!("{}{}{}", prefix, if is_neg && abs_n > BigRational::zero() { "-" } else { "" }, val_str)
        }
        Value::Duration(d) => format_duration(d.clone(), conv_req, explicit_units, registry),
        Value::Function(f, _) => format!("<function {}>", f.name),
    }
}

fn format_duration(mut total_seconds: BigRational, conv_req: &ConversionTarget, explicit_units: &[FormatUnit], registry: &[UnitDef]) -> String {
    let is_neg = total_seconds < BigRational::zero();
    if is_neg { total_seconds = -total_seconds; }

    let cascade = if let Some(units) = &conv_req.units {
        units.clone()
    } else {
        let mut c = vec![
            parse_single_unit("d", registry).unwrap(), 
            parse_single_unit("h", registry).unwrap(), 
            parse_single_unit("m", registry).unwrap(), 
            parse_single_unit("s", registry).unwrap()
        ];
        
        if conv_req.dp.is_some() && !explicit_units.is_empty() {
            let min_factor = explicit_units.iter().map(|u| &u.factor).min().unwrap().clone();
            c.retain(|u| u.factor >= min_factor);
        }
        c
    };

    let mut parts = Vec::new();
    let mut remaining = total_seconds.clone();
    let mut reconstructed = BigRational::zero(); 

    for (i, unit) in cascade.iter().enumerate() {
        let is_last = i == cascade.len() - 1;
        let factor = &unit.factor;

        if is_last {
            let val = &remaining / factor;
            if let Some(places) = conv_req.dp {
                let (val_str, _) = format_exact_decimal(&val, places);
                
                if val_str != "0" || parts.is_empty() { 
                    parts.push(format!("{} {}", val_str, unit.name)); 
                }
                
                if let Ok(parsed_rounded) = parse_decimal(&val_str) {
                    reconstructed += parsed_rounded * factor;
                }
            } else {
                if conv_req.units.is_some() && cascade.len() == 1 {
                    let (val_str, _) = format_exact_decimal(&val, 9);
                    parts.push(format!("{} {}", val_str, unit.name));
                    
                    if let Ok(parsed) = parse_decimal(&val_str) {
                        reconstructed += parsed * factor;
                    }
                } else {
                    let val_int = val.to_integer();
                    let fract = val - BigRational::from_integer(val_int.clone());
                    let half = BigRational::new(BigInt::from(1), BigInt::from(2));
                    let rounded = if fract >= half { val_int + BigInt::from(1) } else { val_int };
                    
                    if !rounded.is_zero() || parts.is_empty() { 
                        parts.push(format!("{} {}", rounded, unit.name)); 
                    }
                    reconstructed += BigRational::from_integer(rounded) * factor;
                }
            }
        } else {
            let val = (&remaining / factor).to_integer(); 
            if !val.is_zero() {
                parts.push(format!("{} {}", val, unit.name));
                reconstructed += BigRational::from_integer(val.clone()) * factor;
            }
            remaining = remaining - BigRational::from_integer(val) * factor;
        }
    }
    
    let is_approx = total_seconds != reconstructed;
    let prefix = if is_approx { "approx. " } else { "" };
    let sign_str = if is_neg && total_seconds > BigRational::zero() { "-" } else { "" };
    
    format!("{}{}{}", prefix, sign_str, parts.join(" "))
}

// --- 6. Evaluator Entry ---

fn evaluate(input: &str, last_val: Option<Value>, registry: &[UnitDef], funcs: &[FuncDef]) -> Result<(String, Value), String> {
    let mut tokens = tokenize(input)?;
    if tokens.is_empty() { return Ok(("".to_string(), Value::Number(BigRational::zero()))); }

    let conv_req = extract_keywords(&mut tokens, registry)?;
    let (mut tokens, mut explicit_units) = form_durations(tokens, registry)?;
    
    tokens = combine_contiguous_durations(tokens);
    
    let (ast, rest) = parse_expr(&tokens)?;
    if !rest.is_empty() { return Err("Incomplete expression".to_string()); }

    ast.collect_units(&mut explicit_units, registry, funcs);
    
    let mut inference_factor = None;
    if let Some(units) = &conv_req.units {
        if !units.is_empty() { inference_factor = Some(units[0].factor.clone()); }
    }
    if inference_factor.is_none() && explicit_units.len() == 1 { 
        inference_factor = Some(explicit_units[0].factor.clone()); 
    }

    let result = eval(&ast, &HashMap::new(), registry, funcs, last_val.as_ref(), inference_factor.as_ref())?;
    Ok((format_output(&result, &conv_req, &explicit_units, registry), result))
}

// --- 7. Interactive UI Loop ---

fn main() -> io::Result<()> {
    let mut stdout = io::stdout();
    terminal::enable_raw_mode()?;
    let _guard = RawModeGuard;
    
    let (registry, funcs) = build_registry();

    let mut history: Vec<String> = Vec::new();
    let mut history_index: usize = 0;
    let mut input = String::new();
    let mut cursor_pos: usize = 0;
    
    let mut last_value: Option<Value> = None;

    execute!(
        stdout,
        Print(format!("{}\r\n", PROJECT_LONG_NAME)),
        Print("Type 'exit' or 'quit' to close.\r\n\n")
    )?;

    loop {
        queue!(stdout, cursor::MoveToColumn(0), Clear(ClearType::FromCursorDown))?;
        queue!(stdout, Print("> "), Print(&input))?;
        
        let trimmed = input.trim();
        if !trimmed.is_empty() {
            if let Ok((res, _)) = evaluate(trimmed, last_value.clone(), &registry, &funcs) {
                if !res.is_empty() {
                    queue!(
                        stdout,
                        Print("\r\n"),
                        SetForegroundColor(Color::DarkGrey),
                        Print(&res),
                        ResetColor,
                        cursor::MoveUp(1)
                    )?;
                }
            }
        }
        
        queue!(stdout, cursor::MoveToColumn((cursor_pos + 2) as u16))?;
        stdout.flush()?;

        if let Event::Key(key_event) = event::read()? {
            if key_event.kind != KeyEventKind::Press { continue; }
            
            match key_event.code {
                KeyCode::Char('c') if key_event.modifiers.contains(KeyModifiers::CONTROL) => break,
                KeyCode::Char('d') if key_event.modifiers.contains(KeyModifiers::CONTROL) => { if input.is_empty() { break; } }
                KeyCode::Char(c) => { input.insert(cursor_pos, c); cursor_pos += 1; }
                KeyCode::Backspace => { if cursor_pos > 0 { cursor_pos -= 1; input.remove(cursor_pos); } }
                KeyCode::Delete => { if cursor_pos < input.len() { input.remove(cursor_pos); } }
                KeyCode::Left => { if cursor_pos > 0 { cursor_pos -= 1; } }
                KeyCode::Right => { if cursor_pos < input.len() { cursor_pos += 1; } }
                KeyCode::Up => {
                    if !history.is_empty() && history_index > 0 {
                        history_index -= 1;
                        input = history[history_index].clone();
                        cursor_pos = input.len();
                    }
                }
                KeyCode::Down => {
                    if history_index + 1 < history.len() {
                        history_index += 1;
                        input = history[history_index].clone();
                        cursor_pos = input.len();
                    } else if history_index + 1 == history.len() {
                        history_index += 1;
                        input.clear();
                        cursor_pos = 0;
                    }
                }
                KeyCode::Enter => {
                    let trimmed = input.trim().to_string();
                    if trimmed.eq_ignore_ascii_case("exit") || trimmed.eq_ignore_ascii_case("quit") { break; }
                    
                    queue!(stdout, cursor::MoveToColumn(0), Clear(ClearType::FromCursorDown))?;
                    queue!(stdout, Print(format!("> {}\r\n", input)))?;
                    
                    if !trimmed.is_empty() {
                        match evaluate(&trimmed, last_value.clone(), &registry, &funcs) {
                            Ok((res, val)) => {
                                queue!(stdout, Print(format!("{}\r\n", res)))?;
                                last_value = Some(val); 
                            }
                            Err(_) => {}
                        }
                        if history.last() != Some(&trimmed) { history.push(trimmed); }
                    }
                    
                    history_index = history.len();
                    input.clear();
                    cursor_pos = 0;
                }
                _ => {}
            }
        }
    }
    
    Ok(())
}
