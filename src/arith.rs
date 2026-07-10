//! Integer arithmetic for `$((...))`, `((...))`, and `for ((...))`.
//!
//! A tokenizer, a recursive-descent parser that builds an [`Expr`] tree, and
//! a separate evaluator over `i64` — split into two passes (rather than
//! evaluating while parsing) specifically so `&&`/`||`/`?:` can *actually*
//! short-circuit: `0 && (i=5)` must never run the assignment, which a
//! combined parse-and-evaluate pass can't skip once it's already recursed
//! into the right-hand side.
//!
//! Supports `+ - * / %`, `**` (right-associative, binds tighter than `*`
//! but looser than unary — `-2**2` is `4`, matching real bash, verified
//! directly), bitwise `& | ^ ~ << >>`, unary `+ - ! ~`, comparisons
//! (`== != < <= > >=`), logical `&& ||`, the ternary `?:`, assignment
//! (`= += -= *= /= %= <<= >>= &= ^= |=` — no `**=`, since real bash itself
//! doesn't have one, verified directly), prefix/postfix `++`/`--`,
//! parentheses, and bare variable names (which resolve like `$name`, with
//! unset → `0`). Comparisons and logicals yield `1`/`0`, as in the shell.
//!
//! Not supported: the comma operator (`a=1, b=2`, rare even in real bash),
//! and any lvalue other than a plain variable name (`arr[i]++`,
//! `arr[i] = x` — arithmetic-context array element assignment).

pub fn eval(src: &str) -> Result<i64, String> {
    let tokens = tokenize(src)?;
    let mut parser = Parser { tokens, pos: 0 };
    let expr = parser.parse_assign()?;
    if parser.pos != parser.tokens.len() {
        return Err("syntax error in arithmetic expression".into());
    }
    eval_expr(&expr)
}

#[derive(Debug, PartialEq, Eq, Clone)]
enum Tok {
    Num(i64),
    Ident(String),
    Op(&'static str),
}

fn tokenize(src: &str) -> Result<Vec<Tok>, String> {
    let mut toks = Vec::new();
    let chars: Vec<char> = src.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
        } else if c.is_ascii_digit() {
            let start = i;
            while i < chars.len() && chars[i].is_ascii_digit() {
                i += 1;
            }
            let n: i64 = chars[start..i]
                .iter()
                .collect::<String>()
                .parse()
                .map_err(|_| "number too large".to_string())?;
            toks.push(Tok::Num(n));
        } else if c == '_' || c.is_ascii_alphabetic() {
            let start = i;
            while i < chars.len() && (chars[i] == '_' || chars[i].is_ascii_alphanumeric()) {
                i += 1;
            }
            toks.push(Tok::Ident(chars[start..i].iter().collect()));
        } else {
            // Three-character operators, then two-character, then
            // single-character — longest match wins.
            let three = if i + 2 < chars.len() { Some([chars[i], chars[i + 1], chars[i + 2]]) } else { None };
            let op3 = three.and_then(|p| match p {
                ['<', '<', '='] => Some("<<="),
                ['>', '>', '='] => Some(">>="),
                _ => None,
            });
            if let Some(op) = op3 {
                toks.push(Tok::Op(op));
                i += 3;
                continue;
            }

            let two = if i + 1 < chars.len() { Some([chars[i], chars[i + 1]]) } else { None };
            let op2 = two.and_then(|p| match p {
                ['=', '='] => Some("=="),
                ['!', '='] => Some("!="),
                ['<', '='] => Some("<="),
                ['>', '='] => Some(">="),
                ['&', '&'] => Some("&&"),
                ['|', '|'] => Some("||"),
                ['*', '*'] => Some("**"),
                ['+', '+'] => Some("++"),
                ['-', '-'] => Some("--"),
                ['+', '='] => Some("+="),
                ['-', '='] => Some("-="),
                ['*', '='] => Some("*="),
                ['/', '='] => Some("/="),
                ['%', '='] => Some("%="),
                ['&', '='] => Some("&="),
                ['^', '='] => Some("^="),
                ['|', '='] => Some("|="),
                ['<', '<'] => Some("<<"),
                ['>', '>'] => Some(">>"),
                _ => None,
            });
            if let Some(op) = op2 {
                toks.push(Tok::Op(op));
                i += 2;
                continue;
            }
            let op1 = match c {
                '+' => "+",
                '-' => "-",
                '*' => "*",
                '/' => "/",
                '%' => "%",
                '<' => "<",
                '>' => ">",
                '!' => "!",
                '(' => "(",
                ')' => ")",
                '=' => "=",
                '&' => "&",
                '|' => "|",
                '^' => "^",
                '~' => "~",
                '?' => "?",
                ':' => ":",
                _ => return Err(format!("unexpected character `{c}` in arithmetic")),
            };
            toks.push(Tok::Op(op1));
            i += 1;
        }
    }

    Ok(toks)
}

/// The assignment/increment operators that need a variable *name*, not a
/// computed value, on their left — parsed as a distinct tree shape
/// (`Expr::Assign`/`CompoundAssign`/`Pre`/`PostIncDec`) rather than an
/// ordinary `Expr::Binary`, since evaluating them has the side effect of
/// storing into that name.
#[derive(Debug, Clone)]
enum Expr {
    Num(i64),
    Var(String),
    /// `- + ! ~`, applied to a fully-parsed sub-expression.
    Unary(&'static str, Box<Expr>),
    /// `++name` / `--name`: increments/decrements first, evaluates to the
    /// *new* value.
    PreIncDec(&'static str, String),
    /// `name++` / `name--`: evaluates to the *old* value, then
    /// increments/decrements.
    PostIncDec(&'static str, String),
    /// Any of `+ - * / % ** << >> < <= > >= == != & ^ |` — every binary
    /// operator that always evaluates both sides (no short-circuiting).
    Binary(&'static str, Box<Expr>, Box<Expr>),
    /// `&&` / `||`: the right side is a *thunk* (unevaluated `Expr`, not
    /// yet a value) specifically so it can be skipped — `0 && (i=5)` must
    /// never run the assignment, verified directly against real bash.
    LogAnd(Box<Expr>, Box<Expr>),
    LogOr(Box<Expr>, Box<Expr>),
    /// `cond ? then : else` — same short-circuiting need: only the taken
    /// branch's side effects (if any) should ever run.
    Ternary(Box<Expr>, Box<Expr>, Box<Expr>),
    /// `name = expr`.
    Assign(String, Box<Expr>),
    /// `name OP= expr` — `OP` is the bare underlying operator (`"+"` for
    /// `+=`, etc.), not the compound token itself.
    CompoundAssign(&'static str, String, Box<Expr>),
}

struct Parser {
    tokens: Vec<Tok>,
    pos: usize,
}

impl Parser {
    fn peek_op(&self) -> Option<&'static str> {
        match self.tokens.get(self.pos) {
            Some(Tok::Op(op)) => Some(op),
            _ => None,
        }
    }

    /// Consume the current token if it is one of `ops`.
    fn eat(&mut self, ops: &[&str]) -> Option<&'static str> {
        if let Some(op) = self.peek_op() {
            if ops.contains(&op) {
                self.pos += 1;
                return Some(op);
            }
        }
        None
    }

    /// `name = expr` / `name OP= expr` — the lowest-precedence form,
    /// right-associative (`a = b = 5` assigns `b` first). Only recognized
    /// when the token right here is an identifier immediately followed by
    /// one of the assignment operators; otherwise this is just an ordinary
    /// expression (falls through to [`Self::parse_ternary`]) — so `a + 1`
    /// or a bare `a == b` (not an assignment operator) never takes this
    /// branch.
    fn parse_assign(&mut self) -> Result<Expr, String> {
        if let Some(Tok::Ident(name)) = self.tokens.get(self.pos).cloned() {
            let assign_op = match self.tokens.get(self.pos + 1) {
                Some(Tok::Op(op @ ("=" | "+=" | "-=" | "*=" | "/=" | "%=" | "<<=" | ">>=" | "&=" | "^=" | "|="))) => {
                    Some(*op)
                }
                _ => None,
            };
            if let Some(op) = assign_op {
                self.pos += 2;
                let rhs = self.parse_assign()?;
                return Ok(if op == "=" {
                    Expr::Assign(name, Box::new(rhs))
                } else {
                    // Strip the trailing `=` to get the underlying operator
                    // (`"+="` → `"+"`).
                    let base = match op {
                        "+=" => "+",
                        "-=" => "-",
                        "*=" => "*",
                        "/=" => "/",
                        "%=" => "%",
                        "<<=" => "<<",
                        ">>=" => ">>",
                        "&=" => "&",
                        "^=" => "^",
                        "|=" => "|",
                        _ => unreachable!(),
                    };
                    Expr::CompoundAssign(base, name, Box::new(rhs))
                });
            }
        }
        self.parse_ternary()
    }

    /// `cond ? then : else`, right-associative in the `else` position
    /// (`a ? b : c ? d : e` is `a ? b : (c ? d : e)`) — verified directly.
    fn parse_ternary(&mut self) -> Result<Expr, String> {
        let cond = self.parse_or()?;
        if self.eat(&["?"]).is_some() {
            let then_v = self.parse_assign()?;
            if self.eat(&[":"]).is_none() {
                return Err("expected `:` in ternary expression".into());
            }
            let else_v = self.parse_ternary()?;
            return Ok(Expr::Ternary(Box::new(cond), Box::new(then_v), Box::new(else_v)));
        }
        Ok(cond)
    }

    fn parse_or(&mut self) -> Result<Expr, String> {
        let mut node = self.parse_and()?;
        while self.eat(&["||"]).is_some() {
            let rhs = self.parse_and()?;
            node = Expr::LogOr(Box::new(node), Box::new(rhs));
        }
        Ok(node)
    }

    fn parse_and(&mut self) -> Result<Expr, String> {
        let mut node = self.parse_bitor()?;
        while self.eat(&["&&"]).is_some() {
            let rhs = self.parse_bitor()?;
            node = Expr::LogAnd(Box::new(node), Box::new(rhs));
        }
        Ok(node)
    }

    fn parse_bitor(&mut self) -> Result<Expr, String> {
        let mut node = self.parse_bitxor()?;
        while self.eat(&["|"]).is_some() {
            let rhs = self.parse_bitxor()?;
            node = Expr::Binary("|", Box::new(node), Box::new(rhs));
        }
        Ok(node)
    }

    fn parse_bitxor(&mut self) -> Result<Expr, String> {
        let mut node = self.parse_bitand()?;
        while self.eat(&["^"]).is_some() {
            let rhs = self.parse_bitand()?;
            node = Expr::Binary("^", Box::new(node), Box::new(rhs));
        }
        Ok(node)
    }

    fn parse_bitand(&mut self) -> Result<Expr, String> {
        let mut node = self.parse_equality()?;
        while self.eat(&["&"]).is_some() {
            let rhs = self.parse_equality()?;
            node = Expr::Binary("&", Box::new(node), Box::new(rhs));
        }
        Ok(node)
    }

    fn parse_equality(&mut self) -> Result<Expr, String> {
        let mut node = self.parse_relational()?;
        while let Some(op) = self.eat(&["==", "!="]) {
            let rhs = self.parse_relational()?;
            node = Expr::Binary(op, Box::new(node), Box::new(rhs));
        }
        Ok(node)
    }

    fn parse_relational(&mut self) -> Result<Expr, String> {
        let mut node = self.parse_shift()?;
        while let Some(op) = self.eat(&["<", "<=", ">", ">="]) {
            let rhs = self.parse_shift()?;
            node = Expr::Binary(op, Box::new(node), Box::new(rhs));
        }
        Ok(node)
    }

    fn parse_shift(&mut self) -> Result<Expr, String> {
        let mut node = self.parse_additive()?;
        while let Some(op) = self.eat(&["<<", ">>"]) {
            let rhs = self.parse_additive()?;
            node = Expr::Binary(op, Box::new(node), Box::new(rhs));
        }
        Ok(node)
    }

    fn parse_additive(&mut self) -> Result<Expr, String> {
        let mut node = self.parse_term()?;
        while let Some(op) = self.eat(&["+", "-"]) {
            let rhs = self.parse_term()?;
            node = Expr::Binary(op, Box::new(node), Box::new(rhs));
        }
        Ok(node)
    }

    fn parse_term(&mut self) -> Result<Expr, String> {
        let mut node = self.parse_pow()?;
        while let Some(op) = self.eat(&["*", "/", "%"]) {
            let rhs = self.parse_pow()?;
            node = Expr::Binary(op, Box::new(node), Box::new(rhs));
        }
        Ok(node)
    }

    /// `**`, right-associative (`2**3**2` is `2**(3**2)` = 512) and
    /// binding *tighter* than `* / %` but *looser* than unary (`-2**2` is
    /// `(-2)**2` = 4, `2*3**2` is `2*(3**2)` = 18) — both verified
    /// directly against real bash.
    fn parse_pow(&mut self) -> Result<Expr, String> {
        let base = self.parse_unary()?;
        if self.eat(&["**"]).is_some() {
            let exp = self.parse_pow()?;
            return Ok(Expr::Binary("**", Box::new(base), Box::new(exp)));
        }
        Ok(base)
    }

    fn parse_unary(&mut self) -> Result<Expr, String> {
        if let Some(op) = self.eat(&["-", "+", "!", "~"]) {
            let v = self.parse_unary()?;
            return Ok(Expr::Unary(op, Box::new(v)));
        }
        if let Some(op) = self.eat(&["++", "--"]) {
            let name = self.expect_ident("++/--")?;
            return Ok(Expr::PreIncDec(op, name));
        }
        self.parse_postfix()
    }

    /// A primary followed by an optional postfix `++`/`--` — only valid
    /// directly on a variable name, matching real bash (`(1+2)++` isn't a
    /// valid lvalue).
    fn parse_postfix(&mut self) -> Result<Expr, String> {
        let node = self.parse_primary()?;
        if let Some(op) = self.eat(&["++", "--"]) {
            return match node {
                Expr::Var(name) => Ok(Expr::PostIncDec(op, name)),
                _ => Err("++/--: not a variable".into()),
            };
        }
        Ok(node)
    }

    fn parse_primary(&mut self) -> Result<Expr, String> {
        match self.tokens.get(self.pos).cloned() {
            Some(Tok::Num(n)) => {
                self.pos += 1;
                Ok(Expr::Num(n))
            }
            Some(Tok::Ident(name)) => {
                self.pos += 1;
                Ok(Expr::Var(name))
            }
            Some(Tok::Op("(")) => {
                self.pos += 1;
                // A full expression, including assignment — `(i = 5) + 1`
                // is a real, if unusual, thing to write.
                let v = self.parse_assign()?;
                if self.eat(&[")"]).is_none() {
                    return Err("missing `)` in arithmetic".into());
                }
                Ok(v)
            }
            Some(Tok::Op("-" | "+" | "!" | "~")) => unreachable!("consumed by parse_unary"),
            _ => Err("unexpected end of arithmetic expression".into()),
        }
    }

    fn expect_ident(&mut self, context: &str) -> Result<String, String> {
        match self.tokens.get(self.pos).cloned() {
            Some(Tok::Ident(name)) => {
                self.pos += 1;
                Ok(name)
            }
            _ => Err(format!("{context}: expected a variable name")),
        }
    }
}

fn eval_expr(e: &Expr) -> Result<i64, String> {
    match e {
        Expr::Num(n) => Ok(*n),
        Expr::Var(name) => var_value(name),
        Expr::Unary(op, v) => {
            let val = eval_expr(v)?;
            Ok(match *op {
                "-" => -val,
                "+" => val,
                "!" => bool_int(val == 0),
                "~" => !val,
                _ => unreachable!(),
            })
        }
        Expr::PreIncDec(op, name) => {
            let new = apply_delta(name, op)?;
            Ok(new)
        }
        Expr::PostIncDec(op, name) => {
            let old = var_value(name)?;
            apply_delta(name, op)?;
            Ok(old)
        }
        Expr::Binary(op, l, r) => {
            let lv = eval_expr(l)?;
            let rv = eval_expr(r)?;
            binary_op(op, lv, rv)
        }
        // Short-circuit: the right side is only evaluated — and so only
        // has any side effect it carries — when it actually needs to run.
        Expr::LogAnd(l, r) => {
            if eval_expr(l)? == 0 {
                return Ok(0);
            }
            Ok(bool_int(eval_expr(r)? != 0))
        }
        Expr::LogOr(l, r) => {
            if eval_expr(l)? != 0 {
                return Ok(1);
            }
            Ok(bool_int(eval_expr(r)? != 0))
        }
        Expr::Ternary(c, t, f) => {
            if eval_expr(c)? != 0 {
                eval_expr(t)
            } else {
                eval_expr(f)
            }
        }
        Expr::Assign(name, v) => {
            let val = eval_expr(v)?;
            crate::vars::set(name, &val.to_string());
            Ok(val)
        }
        Expr::CompoundAssign(op, name, v) => {
            let cur = var_value(name)?;
            let rhs = eval_expr(v)?;
            let new = binary_op(op, cur, rhs)?;
            crate::vars::set(name, &new.to_string());
            Ok(new)
        }
    }
}

/// `name += 1` / `name -= 1`, shared by pre/post `++`/`--` — stores and
/// returns the *new* value; the postfix case is left to discard it and use
/// the old value it already captured instead.
fn apply_delta(name: &str, op: &str) -> Result<i64, String> {
    let cur = var_value(name)?;
    let new = if op == "++" { cur + 1 } else { cur - 1 };
    crate::vars::set(name, &new.to_string());
    Ok(new)
}

fn binary_op(op: &str, l: i64, r: i64) -> Result<i64, String> {
    Ok(match op {
        "+" => l + r,
        "-" => l - r,
        "*" => l * r,
        "/" if r == 0 => return Err("division by zero".into()),
        "/" => l / r,
        "%" if r == 0 => return Err("division by zero".into()),
        "%" => l % r,
        "**" => {
            if r < 0 {
                return Err("exponent less than 0".into());
            }
            let exp = u32::try_from(r).map_err(|_| "exponent too large".to_string())?;
            l.checked_pow(exp).ok_or("arithmetic overflow")?
        }
        "<<" => l << r,
        ">>" => l >> r,
        "&" => l & r,
        "^" => l ^ r,
        "|" => l | r,
        "<" => bool_int(l < r),
        "<=" => bool_int(l <= r),
        ">" => bool_int(l > r),
        ">=" => bool_int(l >= r),
        "==" => bool_int(l == r),
        "!=" => bool_int(l != r),
        _ => unreachable!("unhandled binary operator `{op}`"),
    })
}

fn bool_int(b: bool) -> i64 {
    if b { 1 } else { 0 }
}

/// A variable's value as an integer: unset/empty → 0, non-numeric → error.
fn var_value(name: &str) -> Result<i64, String> {
    let raw = crate::vars::get(name)
        .or_else(|| std::env::var(name).ok())
        .unwrap_or_default();
    let s = raw.trim();
    if s.is_empty() {
        return Ok(0);
    }
    s.parse::<i64>()
        .map_err(|_| format!("`{name}`: not an integer (`{raw}`)"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arithmetic() {
        assert_eq!(eval("1 + 2 * 3"), Ok(7));
        assert_eq!(eval("(1 + 2) * 3"), Ok(9));
        assert_eq!(eval("10 / 3"), Ok(3));
        assert_eq!(eval("10 % 3"), Ok(1));
        assert_eq!(eval("-5 + 2"), Ok(-3));
        assert_eq!(eval("2 + 2 == 4"), Ok(1));
        assert_eq!(eval("3 < 2"), Ok(0));
        assert_eq!(eval("1 && 0"), Ok(0));
        assert_eq!(eval("1 || 0"), Ok(1));
        assert_eq!(eval("!0"), Ok(1));
    }

    #[test]
    fn variables() {
        crate::vars::set("RUSH_N", "41");
        assert_eq!(eval("RUSH_N + 1"), Ok(42));
        crate::vars::unset("RUSH_UNSET_ARITH");
        assert_eq!(eval("RUSH_UNSET_ARITH + 5"), Ok(5)); // unset → 0
    }

    #[test]
    fn errors() {
        assert!(eval("1 / 0").is_err());
        assert!(eval("1 +").is_err());
        assert!(eval("2 2").is_err());
    }

    #[test]
    fn exponent_and_bitwise() {
        assert_eq!(eval("2**10"), Ok(1024));
        assert_eq!(eval("2**3**2"), Ok(512)); // right-assoc
        assert_eq!(eval("-2**2"), Ok(4)); // unary binds tighter than **
        assert_eq!(eval("2*3**2"), Ok(18)); // ** binds tighter than *
        assert_eq!(eval("2**-1"), Err("exponent less than 0".into()));
        assert_eq!(eval("5 & 3"), Ok(1));
        assert_eq!(eval("5 | 2"), Ok(7));
        assert_eq!(eval("5 ^ 1"), Ok(4));
        assert_eq!(eval("~5"), Ok(-6));
        assert_eq!(eval("1 << 3"), Ok(8));
        assert_eq!(eval("16 >> 2"), Ok(4));
        assert_eq!(eval("2 + 3 & 1"), Ok(1)); // + binds tighter than &
    }

    #[test]
    fn ternary() {
        assert_eq!(eval("1 ? 2 : 3"), Ok(2));
        assert_eq!(eval("0 ? 2 : 3"), Ok(3));
        assert_eq!(eval("1 ? 0 : 1 ? 2 : 3"), Ok(0)); // right-assoc grouping
        assert_eq!(eval("1 || 0 ? 5 : 6"), Ok(5)); // || binds tighter than ?:
    }

    #[test]
    fn assignment_and_inc_dec() {
        crate::vars::set("RUSH_I", "5");
        assert_eq!(eval("RUSH_I++"), Ok(5)); // postfix: old value
        assert_eq!(crate::vars::get("RUSH_I"), Some("6".into()));
        assert_eq!(eval("++RUSH_I"), Ok(7)); // prefix: new value
        assert_eq!(crate::vars::get("RUSH_I"), Some("7".into()));
        assert_eq!(eval("RUSH_I--"), Ok(7));
        assert_eq!(crate::vars::get("RUSH_I"), Some("6".into()));
        assert_eq!(eval("--RUSH_I"), Ok(5));
        assert_eq!(crate::vars::get("RUSH_I"), Some("5".into()));

        assert_eq!(eval("RUSH_I = 10"), Ok(10));
        assert_eq!(crate::vars::get("RUSH_I"), Some("10".into()));
        assert_eq!(eval("RUSH_I += 5"), Ok(15));
        assert_eq!(crate::vars::get("RUSH_I"), Some("15".into()));

        crate::vars::set("RUSH_A", "5");
        crate::vars::set("RUSH_B", "0");
        // Right-associative chained assignment.
        assert_eq!(eval("RUSH_A = RUSH_B = 7"), Ok(7));
        assert_eq!(crate::vars::get("RUSH_A"), Some("7".into()));
        assert_eq!(crate::vars::get("RUSH_B"), Some("7".into()));
    }

    #[test]
    fn short_circuit_skips_side_effects() {
        crate::vars::set("RUSH_SC", "1");
        assert_eq!(eval("0 && (RUSH_SC = 5)"), Ok(0));
        assert_eq!(crate::vars::get("RUSH_SC"), Some("1".into())); // untouched
        assert_eq!(eval("1 || (RUSH_SC = 5)"), Ok(1));
        assert_eq!(crate::vars::get("RUSH_SC"), Some("1".into())); // untouched
        assert_eq!(eval("0 ? (RUSH_SC = 9) : (RUSH_SC = 7)"), Ok(7));
        assert_eq!(crate::vars::get("RUSH_SC"), Some("7".into())); // only the taken branch ran
    }
}
