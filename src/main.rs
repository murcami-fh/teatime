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
use num_traits::{ToPrimitive, Zero};
use serde::Deserialize;
use std::collections::HashMap;
use std::io::{self, Write};
use std::str::FromStr;

const PROJECT_SHORT_NAME: &'static str = "tt";
const PROJECT_LONG_NAME: &'static str = "🍵 Tea Time";

#[cfg(test)]
mod tests;

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
    String(String),
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
    To(Box<Expr>, ConversionTarget),
}

#[derive(Clone, Debug)]
enum Value {
    Number(BigRational),
    Duration(BigRational),
    String(String),
    Function(FuncDef, Vec<Value>),
    Formatted(Box<Value>, ConversionTarget),
}

impl Value {
    fn unwrap(self) -> (Value, Option<ConversionTarget>) {
        match self {
            Value::Formatted(inner, req) => {
                let (v, inner_req) = inner.unwrap();
                (v, Some(req).or(inner_req))
            }
            _ => (self, None)
        }
    }
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
            Expr::UnaryMinus(e) | Expr::To(e, _) => e.collect_units(units, registry, funcs),
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
    String(String),
    Plus, Minus, Multiply, Divide, Modulo, Underscore, LParen, RParen, To, Now,
    Assign,
    Duration(BigRational),
    Space, 
}

#[derive(Default, Debug, Clone)]
struct ConversionTarget {
    dp: Option<i32>,
    units: Option<Vec<FormatUnit>>,
}

impl ConversionTarget {
    fn is_empty(&self) -> bool {
        self.dp.is_none() && self.units.is_none()
    }
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

fn clean_tokens(tokens: Vec<Token>) -> Vec<Token> {
    let mut res = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        if tokens[i] == Token::Space {
            let left_is_operand = if i > 0 {
                matches!(tokens[i-1], Token::Num(_) | Token::Duration(_) | Token::String(_) | Token::Ident(_) | Token::Now | Token::Underscore | Token::RParen)
            } else { false };
            
            let mut j = i + 1;
            while j < tokens.len() && tokens[j] == Token::Space { j += 1; }
            
            let right_is_operand = if j < tokens.len() {
                matches!(tokens[j], Token::Num(_) | Token::Duration(_) | Token::String(_) | Token::Ident(_) | Token::Now | Token::Underscore | Token::LParen)
            } else { false };

            if left_is_operand && right_is_operand {
                res.push(Token::Space);
            }
            i = j;
        } else {
            res.push(tokens[i].clone());
            i += 1;
        }
    }
    res
}

fn tokenize(input: &str) -> Result<Vec<Token>, String> {
    let mut tokens = Vec::new();
    let mut chars = input.chars().peekable();

    while let Some(&c) = chars.peek() {
        if c.is_whitespace() { 
            chars.next(); 
            while let Some(&nc) = chars.peek() {
                if nc.is_whitespace() { chars.next(); } else { break; }
            }
            tokens.push(Token::Space);
        } 
        else if c == '"' || c == '\'' {
            let quote = c;
            chars.next();
            let mut s = String::new();
            while let Some(&ch) = chars.peek() {
                if ch == quote { chars.next(); break; }
                s.push(ch); chars.next();
            }
            tokens.push(Token::String(s));
        }
        else if c == '+' { tokens.push(Token::Plus); chars.next(); } 
        else if c == '-' { tokens.push(Token::Minus); chars.next(); } 
        else if c == '*' { tokens.push(Token::Multiply); chars.next(); } 
        else if c == '/' { tokens.push(Token::Divide); chars.next(); } 
        else if c == '%' { tokens.push(Token::Modulo); chars.next(); } 
        else if c == '_' { tokens.push(Token::Underscore); chars.next(); } 
        else if c == '(' { tokens.push(Token::LParen); chars.next(); } 
        else if c == ')' { tokens.push(Token::RParen); chars.next(); } 
        else if c == ':' { 
            chars.next();
            if let Some(&'=') = chars.peek() {
                tokens.push(Token::Assign); chars.next();
            } else {
                return Err("Unexpected character: ':' (did you mean ':='?)".to_string());
            }
        } 
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
    Ok(clean_tokens(tokens))
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
                        if let Ok((_, Some(val))) = evaluate(&conf.value, None, &registry, &mut funcs) {
                            let (v, _) = val.unwrap();
                            if let Value::Duration(seconds) = v {
                                registry.push(UnitDef { primary_name: name, factor: seconds, aliases: conf.alias });
                            }
                        }
                    }
                }
                if let Some(functions) = config.functions {
                    for (_, conf) in functions {
                        if let Ok(tokens) = tokenize(&conf.definition) {
                            let (mut rhs_tokens, _) = form_durations(tokens, &registry).unwrap_or((vec![], vec![]));
                            rhs_tokens = combine_contiguous_durations(rhs_tokens);
                            if let Ok((ast, rest)) = parse_expr(&rhs_tokens, &registry) {
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

fn parse_conversion_target<'a>(mut tokens: &'a [Token], registry: &[UnitDef]) -> Result<(ConversionTarget, &'a [Token]), String> {
    let mut target = ConversionTarget::default();
    
    while !tokens.is_empty() && tokens[0] == Token::Space { tokens = &tokens[1..]; }
    if tokens.is_empty() { return Err("Missing target after 'to'".into()); }

    if tokens.len() >= 2 {
        let (num_tok, next_tok, rest) = if tokens[1] == Token::Space && tokens.len() >= 3 {
            (&tokens[0], &tokens[2], &tokens[3..])
        } else {
            (&tokens[0], &tokens[1], &tokens[2..])
        };
        
        if let (Token::Num(n), Token::Ident(u)) = (num_tok, next_tok) {
            if u.eq_ignore_ascii_case("decimal") || u.eq_ignore_ascii_case("dp") {
                target.dp = Some(n.to_integer().to_i32().unwrap_or(0));
                return Ok((target, rest));
            }
        }
    }

    let mut units = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        if tokens[i] == Token::Space { i += 1; continue; }
        if let Token::Ident(ref u) = tokens[i] {
            if let Ok(fmt_unit) = parse_single_unit(u, registry) {
                units.push(fmt_unit);
                i += 1;
            } else { break; }
        } else { break; }
    }
    
    if units.is_empty() { return Err("Invalid conversion target".into()); }
    target.units = Some(units);
    Ok((target, &tokens[i..]))
}

fn form_durations(tokens: Vec<Token>, registry: &[UnitDef]) -> Result<(Vec<Token>, Vec<FormatUnit>), String> {
    let mut new_tokens = Vec::new();
    let mut explicit_units = Vec::new();
    let mut i = 0;

    while i < tokens.len() {
        if let Token::Num(ref val) = tokens[i] {
            let mut j = i + 1;
            while j < tokens.len() && tokens[j] == Token::Space { j += 1; }
            if j < tokens.len() {
                if let Token::Ident(ref unit) = tokens[j] {
                    if let Ok(fmt_unit) = parse_single_unit(unit, registry) {
                        if !explicit_units.contains(&fmt_unit) { explicit_units.push(fmt_unit.clone()); }
                        let (sec, _) = fmt_unit.info();
                        new_tokens.push(Token::Duration(val * sec));
                        i = j + 1;
                        continue;
                    }
                }
            }
            new_tokens.push(Token::Num(val.clone())); 
        } else {
            new_tokens.push(tokens[i].clone());
        }
        i += 1;
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

// Precedence 1: String Concatenation 
fn parse_expr<'a>(tokens: &'a [Token], registry: &[UnitDef]) -> Result<(Expr, &'a [Token]), String> {
    let (mut lhs, mut rest) = parse_to_expr(tokens, registry)?;
    while !rest.is_empty() {
        if rest[0] == Token::Space && rest.len() > 1 && matches!(rest[1], Token::String(_)) {
            let (rhs, new_rest) = parse_primary(&rest[1..], registry)?;
            lhs = Expr::Add(Box::new(lhs), Box::new(rhs));
            rest = new_rest;
        } else if matches!(rest[0], Token::String(_)) {
            let (rhs, new_rest) = parse_primary(rest, registry)?;
            lhs = Expr::Add(Box::new(lhs), Box::new(rhs));
            rest = new_rest;
        } else {
            break;
        }
    }
    Ok((lhs, rest))
}

// Precedence 2: Conversion (to / as)
fn parse_to_expr<'a>(tokens: &'a [Token], registry: &[UnitDef]) -> Result<(Expr, &'a [Token]), String> {
    let (mut lhs, mut rest) = parse_add_expr(tokens, registry)?;
    while !rest.is_empty() {
        if rest[0] == Token::To {
            let (target, new_rest) = parse_conversion_target(&rest[1..], registry)?;
            lhs = Expr::To(Box::new(lhs), target);
            rest = new_rest;
        } else if rest[0] == Token::Space && rest.len() > 1 && rest[1] == Token::To {
            let (target, new_rest) = parse_conversion_target(&rest[2..], registry)?;
            lhs = Expr::To(Box::new(lhs), target);
            rest = new_rest;
        } else {
            break;
        }
    }
    Ok((lhs, rest))
}

// Precedence 3: Addition / Subtraction
fn parse_add_expr<'a>(tokens: &'a [Token], registry: &[UnitDef]) -> Result<(Expr, &'a [Token]), String> {
    let (mut lhs, mut rest) = parse_mul_expr(tokens, registry)?;
    while !rest.is_empty() {
        if rest[0] == Token::Plus {
            let (rhs, new_rest) = parse_mul_expr(&rest[1..], registry)?;
            lhs = Expr::Add(Box::new(lhs), Box::new(rhs));
            rest = new_rest;
        } else if rest[0] == Token::Minus {
            let (rhs, new_rest) = parse_mul_expr(&rest[1..], registry)?;
            lhs = Expr::Sub(Box::new(lhs), Box::new(rhs));
            rest = new_rest;
        } else { break; }
    }
    Ok((lhs, rest))
}

// Precedence 4: Multiplication / Division / Modulo
fn parse_mul_expr<'a>(tokens: &'a [Token], registry: &[UnitDef]) -> Result<(Expr, &'a [Token]), String> {
    let (mut lhs, mut rest) = parse_space_app(tokens, registry)?;
    while !rest.is_empty() {
        if rest[0] == Token::Multiply {
            let (rhs, new_rest) = parse_space_app(&rest[1..], registry)?;
            lhs = Expr::Mul(Box::new(lhs), Box::new(rhs));
            rest = new_rest;
        } else if rest[0] == Token::Divide {
            let (rhs, new_rest) = parse_space_app(&rest[1..], registry)?;
            lhs = Expr::Div(Box::new(lhs), Box::new(rhs));
            rest = new_rest;
        } else if rest[0] == Token::Modulo {
            let (rhs, new_rest) = parse_space_app(&rest[1..], registry)?;
            lhs = Expr::Mod(Box::new(lhs), Box::new(rhs));
            rest = new_rest;
        } else { break; }
    }
    Ok((lhs, rest))
}

// Precedence 5: Semantic Space Application
fn parse_space_app<'a>(tokens: &'a [Token], registry: &[UnitDef]) -> Result<(Expr, &'a [Token]), String> {
    let (mut lhs, mut rest) = parse_nospace_app(tokens, registry)?;
    while !rest.is_empty() {
        // Only apply if the next parameter is NOT a string. Strings get caught by concat precedence 1.
        if rest[0] == Token::Space && rest.len() > 1 && !matches!(rest[1], Token::String(_)) {
            let (rhs, new_rest) = parse_nospace_app(&rest[1..], registry)?;
            lhs = Expr::Apply(Box::new(lhs), Box::new(rhs));
            rest = new_rest;
        } else { break; }
    }
    Ok((lhs, rest))
}

// Precedence 6: Tight Application (Nospace)
fn parse_nospace_app<'a>(tokens: &'a [Token], registry: &[UnitDef]) -> Result<(Expr, &'a [Token]), String> {
    let (mut lhs, mut rest) = parse_primary(tokens, registry)?;
    while !rest.is_empty() {
        match rest[0] {
            Token::Num(_) | Token::Duration(_) | Token::Ident(_) | Token::Now | Token::Underscore | Token::LParen => {
                let (rhs, new_rest) = parse_primary(rest, registry)?;
                lhs = Expr::Apply(Box::new(lhs), Box::new(rhs));
                rest = new_rest;
            }
            _ => break,
        }
    }
    Ok((lhs, rest))
}

// Precedence 7: Primitives
fn parse_primary<'a>(tokens: &'a [Token], registry: &[UnitDef]) -> Result<(Expr, &'a [Token]), String> {
    if tokens.is_empty() { return Err("Unexpected end of expression".into()); }
    match &tokens[0] {
        Token::Num(n) => Ok((Expr::Number(n.clone()), &tokens[1..])),
        Token::Duration(d) => Ok((Expr::Duration(d.clone()), &tokens[1..])), 
        Token::String(s) => Ok((Expr::String(s.clone()), &tokens[1..])),
        Token::Ident(s) => Ok((Expr::Ident(s.clone()), &tokens[1..])),
        Token::Now => Ok((Expr::Now, &tokens[1..])),
        Token::Underscore => Ok((Expr::Underscore, &tokens[1..])),
        Token::LParen => {
            let (expr, rest) = parse_expr(&tokens[1..], registry)?;
            if rest.is_empty() || rest[0] != Token::RParen { return Err("Missing closing parenthesis".into()); }
            Ok((expr, &rest[1..]))
        }
        Token::Minus => {
            let (expr, rest) = parse_primary(&tokens[1..], registry)?;
            Ok((Expr::UnaryMinus(Box::new(expr)), rest))
        }
        Token::Plus => parse_primary(&tokens[1..], registry),
        _ => Err(format!("Unexpected syntax token: {:?}", tokens[0])),
    }
}

// --- 4. Evaluator (Resolves functions, args & math) ---

fn apply_values(
    l: Value, r: Value, 
    env: &HashMap<String, Value>, 
    registry: &[UnitDef], 
    funcs: &[FuncDef], 
    last_val: Option<&Value>, 
    inference_factor: Option<&BigRational>,
    explicit_units: &[FormatUnit]
) -> Result<Value, String> {
    let (l_val, l_req) = l.clone().unwrap();
    let (r_val, r_req) = r.clone().unwrap();
    let req = l_req.or(r_req);

    if matches!(l_val, Value::String(_)) || matches!(r_val, Value::String(_)) {
        let empty = ConversionTarget::default();
        let l_s = format_value(&l, &empty, explicit_units, registry);
        let r_s = format_value(&r, &empty, explicit_units, registry);
        let mut res = Value::String(format!("{}{}", l_s, r_s));
        if let Some(r) = req { res = Value::Formatted(Box::new(res), r); }
        return Ok(res);
    }

    let mut res = match (l_val, r_val) {
        (Value::Number(a), Value::Number(b)) => Value::Number(a * b),
        (Value::Number(a), Value::Duration(b)) => Value::Duration(a * b),
        (Value::Duration(a), Value::Number(b)) => Value::Duration(a * b),
        (Value::Duration(a), Value::Duration(b)) => Value::Duration(a + b), 
        (Value::Function(f, mut args), v) => {
            args.push(v);
            if args.len() == f.args.len() {
                let mut new_env = env.clone();
                for (name, val) in f.args.iter().zip(args.into_iter()) {
                    new_env.insert(name.clone(), val);
                }
                eval(&f.body, &new_env, registry, funcs, last_val, inference_factor, explicit_units)?
            } else {
                Value::Function(f, args)
            }
        }
        _ => return Err("Invalid function application or juxtaposition".into()),
    };
    
    if let Some(r) = req { res = Value::Formatted(Box::new(res), r); }
    Ok(res)
}

fn eval(
    expr: &Expr, 
    env: &HashMap<String, Value>, 
    registry: &[UnitDef], 
    funcs: &[FuncDef], 
    last_val: Option<&Value>, 
    inference_factor: Option<&BigRational>,
    explicit_units: &[FormatUnit]
) -> Result<Value, String> {
    match expr {
        Expr::Number(n) => Ok(Value::Number(n.clone())),
        Expr::Duration(d) => Ok(Value::Duration(d.clone())),
        Expr::String(s) => Ok(Value::String(s.clone())),
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
                    if def.args.is_empty() { 
                        return eval(&def.body, env, registry, funcs, last_val, inference_factor, explicit_units);
                    }
                    return Ok(Value::Function(def.clone(), vec![]));
                }
            }
            Err(format!("Unknown identifier: {}", name))
        }
        Expr::Now => Ok(Value::Duration(BigRational::from_integer(BigInt::from(chrono::Local::now().num_seconds_from_midnight())))),
        Expr::Underscore => last_val.cloned().ok_or("No previous value to reference".into()),
        Expr::UnaryMinus(e) => {
            let (v_val, req) = eval(e, env, registry, funcs, last_val, inference_factor, explicit_units)?.unwrap();
            let mut res = match v_val {
                Value::Number(n) => Value::Number(-n),
                Value::Duration(d) => Value::Duration(-d),
                Value::String(_) => return Err("Cannot negate a string".into()),
                Value::Function(..) => return Err("Cannot negate a function".into()),
                Value::Formatted(..) => unreachable!(), 
            };
            if let Some(r) = req { res = Value::Formatted(Box::new(res), r); }
            Ok(res)
        }
        Expr::To(e, target) => {
            let mut val = eval(e, env, registry, funcs, last_val, inference_factor, explicit_units)?;
            let mut final_target = target.clone();
            
            if let Value::Formatted(inner, req) = val {
                val = *inner;
                if final_target.dp.is_none() { final_target.dp = req.dp; }
                if final_target.units.is_none() { final_target.units = req.units; }
            }
            Ok(Value::Formatted(Box::new(val), final_target))
        }
        Expr::Add(lhs, rhs) => {
            let l_full = eval(lhs, env, registry, funcs, last_val, inference_factor, explicit_units)?;
            let r_full = eval(rhs, env, registry, funcs, last_val, inference_factor, explicit_units)?;
            let (l_val, l_req) = l_full.clone().unwrap();
            let (r_val, r_req) = r_full.clone().unwrap();
            let req = l_req.or(r_req);

            if matches!(l_val, Value::String(_)) || matches!(r_val, Value::String(_)) {
                let empty = ConversionTarget::default();
                let l_s = format_value(&l_full, &empty, explicit_units, registry);
                let r_s = format_value(&r_full, &empty, explicit_units, registry);
                let mut res = Value::String(format!("{}{}", l_s, r_s));
                if let Some(r) = req { res = Value::Formatted(Box::new(res), r); }
                return Ok(res);
            }

            let mut res = match (l_val, r_val) {
                (Value::Number(a), Value::Number(b)) => Value::Number(a + b),
                (Value::Duration(a), Value::Duration(b)) => Value::Duration(a + b),
                (Value::Duration(a), Value::Number(b)) => {
                    if let Some(f) = inference_factor { Value::Duration(a + b * f) } else { return Err("Ambiguous unitless number (cannot infer unit)".into()) }
                },
                (Value::Number(a), Value::Duration(b)) => {
                    if let Some(f) = inference_factor { Value::Duration(a * f + b) } else { return Err("Ambiguous unitless number (cannot infer unit)".into()) }
                },
                _ => return Err("Cannot add these types".into())
            };
            if let Some(r) = req { res = Value::Formatted(Box::new(res), r); }
            Ok(res)
        }
        Expr::Sub(lhs, rhs) => {
            let l_full = eval(lhs, env, registry, funcs, last_val, inference_factor, explicit_units)?;
            let r_full = eval(rhs, env, registry, funcs, last_val, inference_factor, explicit_units)?;
            let (l_val, l_req) = l_full.clone().unwrap();
            let (r_val, r_req) = r_full.clone().unwrap();
            let req = l_req.or(r_req);

            if matches!(l_val, Value::String(_)) || matches!(r_val, Value::String(_)) { return Err("Cannot subtract strings".into()); }

            let mut res = match (l_val, r_val) {
                (Value::Number(a), Value::Number(b)) => Value::Number(a - b),
                (Value::Duration(a), Value::Duration(b)) => Value::Duration(a - b),
                (Value::Duration(a), Value::Number(b)) => {
                    if let Some(f) = inference_factor { Value::Duration(a - b * f) } else { return Err("Ambiguous unitless number (cannot infer unit)".into()) }
                },
                (Value::Number(a), Value::Duration(b)) => {
                    if let Some(f) = inference_factor { Value::Duration(a * f - b) } else { return Err("Ambiguous unitless number (cannot infer unit)".into()) }
                },
                _ => return Err("Cannot subtract these types".into())
            };
            if let Some(r) = req { res = Value::Formatted(Box::new(res), r); }
            Ok(res)
        }
        Expr::Mul(lhs, rhs) => {
            let l_full = eval(lhs, env, registry, funcs, last_val, inference_factor, explicit_units)?;
            let r_full = eval(rhs, env, registry, funcs, last_val, inference_factor, explicit_units)?;
            let (l_val, l_req) = l_full.clone().unwrap();
            let (r_val, r_req) = r_full.clone().unwrap();
            let req = l_req.or(r_req);

            if matches!(l_val, Value::String(_)) || matches!(r_val, Value::String(_)) { return Err("Cannot multiply strings".into()); }

            let mut res = match (l_val, r_val) {
                (Value::Number(a), Value::Number(b)) => Value::Number(a * b),
                (Value::Number(a), Value::Duration(b)) | (Value::Duration(b), Value::Number(a)) => Value::Duration(a * b),
                _ => return Err("Cannot multiply two time durations explicitly".into())
            };
            if let Some(r) = req { res = Value::Formatted(Box::new(res), r); }
            Ok(res)
        }
        Expr::Div(lhs, rhs) => {
            let l_full = eval(lhs, env, registry, funcs, last_val, inference_factor, explicit_units)?;
            let r_full = eval(rhs, env, registry, funcs, last_val, inference_factor, explicit_units)?;
            let (l_val, l_req) = l_full.clone().unwrap();
            let (r_val, r_req) = r_full.clone().unwrap();
            let req = l_req.or(r_req);

            if matches!(l_val, Value::String(_)) || matches!(r_val, Value::String(_)) { return Err("Cannot divide strings".into()); }

            let mut res = match (l_val, r_val) {
                (Value::Number(a), Value::Number(b)) => if b.is_zero() { return Err("Division by zero".into()) } else { Value::Number(a / b) },
                (Value::Duration(a), Value::Number(b)) => if b.is_zero() { return Err("Division by zero".into()) } else { Value::Duration(a / b) },
                (Value::Duration(a), Value::Duration(b)) => if b.is_zero() { return Err("Division by zero".into()) } else { Value::Number(a / b) },
                _ => return Err("Cannot divide a number by a time duration".into())
            };
            if let Some(r) = req { res = Value::Formatted(Box::new(res), r); }
            Ok(res)
        }
        Expr::Mod(lhs, rhs) => {
            let l_full = eval(lhs, env, registry, funcs, last_val, inference_factor, explicit_units)?;
            let r_full = eval(rhs, env, registry, funcs, last_val, inference_factor, explicit_units)?;
            let (l_val, l_req) = l_full.clone().unwrap();
            let (r_val, r_req) = r_full.clone().unwrap();
            let req = l_req.or(r_req);

            if matches!(l_val, Value::String(_)) || matches!(r_val, Value::String(_)) { return Err("Cannot modulo strings".into()); }

            let mut res = match (l_val, r_val) {
                (Value::Number(a), Value::Number(b)) => if b.is_zero() { return Err("Modulo by zero".into()) } else { Value::Number(a % b) },
                (Value::Duration(a), Value::Duration(b)) => if b.is_zero() { return Err("Modulo by zero".into()) } else { Value::Duration(a % b) },
                (Value::Duration(a), Value::Number(b)) => {
                    if let Some(f) = inference_factor {
                        let scaled_b = b * f;
                        if scaled_b.is_zero() { return Err("Modulo by zero".into()) } else { Value::Duration(a % scaled_b) }
                    } else {
                        return Err("Ambiguous unitless number (cannot infer unit)".into())
                    }
                },
                (Value::Number(a), Value::Duration(b)) => {
                    if let Some(f) = inference_factor {
                        let scaled_a = a * f;
                        if b.is_zero() { return Err("Modulo by zero".into()) } else { Value::Duration(scaled_a % b) }
                    } else {
                        return Err("Ambiguous unitless number (cannot infer unit)".into())
                    }
                },
                _ => return Err("Cannot modulo these types".into())
            };
            if let Some(r) = req { res = Value::Formatted(Box::new(res), r); }
            Ok(res)
        }
        Expr::Apply(lhs, rhs) => {
            let l_eval = eval(lhs, env, registry, funcs, last_val, inference_factor, explicit_units)?;
            let r_eval = eval(rhs, env, registry, funcs, last_val, inference_factor, explicit_units)?;
            apply_values(l_eval, r_eval, env, registry, funcs, last_val, inference_factor, explicit_units)
        }
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

fn format_value(result: &Value, top_conv_req: &ConversionTarget, explicit_units: &[FormatUnit], registry: &[UnitDef]) -> String {
    let (val, val_req) = result.clone().unwrap();
    
    let empty_target = ConversionTarget::default();
    let conv_req = if !top_conv_req.is_empty() { top_conv_req } else if let Some(ref r) = val_req { r } else { &empty_target };

    match val {
        Value::String(s) => s,
        Value::Number(n) => {
            let is_neg = n < BigRational::zero();
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
        Value::Formatted(..) => unreachable!(),
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

fn evaluate(input: &str, last_val: Option<Value>, registry: &[UnitDef], funcs: &mut Vec<FuncDef>) -> Result<(String, Option<Value>), String> {
    let tokens = tokenize(input)?;
    if tokens.is_empty() { return Ok(("".to_string(), Some(Value::Number(BigRational::zero())))); }

    if let Some(pos) = tokens.iter().position(|t| *t == Token::Assign) {
        let lhs = &tokens[..pos];
        let rhs = &tokens[pos+1..];
        
        if lhs.is_empty() { return Err("Missing function name".to_string()); }
        
        let mut func_name = String::new();
        let mut args = Vec::new();
        
        for tok in lhs.iter() {
            if *tok == Token::Space { continue; }
            if let Token::Ident(name) = tok {
                if func_name.is_empty() { func_name = name.clone(); }
                else { args.push(name.clone()); }
            } else {
                return Err("Invalid function definition: left side must be identifiers".to_string());
            }
        }
        
        let rhs_tokens = rhs.to_vec();
        let (mut rhs_tokens, _) = form_durations(rhs_tokens, registry)?;
        rhs_tokens = combine_contiguous_durations(rhs_tokens);
        
        let (body, rest) = parse_expr(&rhs_tokens, registry)?;
        if !rest.is_empty() { return Err("Incomplete expression in function body".to_string()); }
        
        funcs.retain(|f| f.name != func_name);
        funcs.push(FuncDef { name: func_name, aliases: vec![], args, body });
        
        return Ok(("".to_string(), None)); 
    }

    let (mut tokens, mut explicit_units) = form_durations(tokens, registry)?;
    tokens = combine_contiguous_durations(tokens);
    
    let (ast, rest) = parse_expr(&tokens, registry)?;
    if !rest.is_empty() { return Err("Incomplete expression".to_string()); }

    ast.collect_units(&mut explicit_units, registry, funcs);
    
    let mut inference_factor = None;
    if explicit_units.len() == 1 { 
        inference_factor = Some(explicit_units[0].factor.clone()); 
    }

    let result = eval(&ast, &HashMap::new(), registry, funcs, last_val.as_ref(), inference_factor.as_ref(), &explicit_units)?;
    
    let (val_unwrapped, req) = result.clone().unwrap();
    let empty_req = ConversionTarget::default();
    let final_req = req.unwrap_or(empty_req);
    
    Ok((format_value(&val_unwrapped, &final_req, &explicit_units, registry), Some(result)))
}

// --- 7. Interactive UI Loop ---

fn main() -> io::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|arg| arg == "-h" || arg == "--help") {
        println!("{}", include_str!("../README.md"));
        return Ok(());
    }
    if args.iter().any(|arg| arg == "-v" || arg == "--version") {
        println!("{} v{}", PROJECT_LONG_NAME, env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    let mut stdout = io::stdout();
    terminal::enable_raw_mode()?;
    let _guard = RawModeGuard;
    
    let (registry, mut funcs) = build_registry();

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
            if let Ok((res, _)) = evaluate(trimmed, last_value.clone(), &registry, &mut funcs) {
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
                        match evaluate(&trimmed, last_value.clone(), &registry, &mut funcs) {
                            Ok((res, val_opt)) => {
                                if !res.is_empty() {
                                    queue!(stdout, Print(format!("{}\r\n", res)))?;
                                }
                                if let Some(val) = val_opt {
                                    last_value = Some(val); 
                                }
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
