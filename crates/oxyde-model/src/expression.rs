//! Expressions of equations and global variables, SolidWorks style:
//! `"D1@Sketch1" * 2 + 5`, `sqrt("Width"^2 + "Height"^2)`.
//!
//! Names are written between double quotes: a global variable (`"Width"`) or a dimension
//! (`"D1@Sketch1"`). Once parsed, a name is held by stable reference ([`Operand`]), so
//! renaming a feature, a dimension or a variable never breaks an expression; it is printed
//! again with the current names.
//!
//! Values are in display units: millimetres, and degrees for angles (trigonometric functions
//! work in degrees too, as in SolidWorks).

use std::fmt;

use crate::dimensions::DimensionRef;

/// Stable identity of a global variable. Never reused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct GlobalId(pub u32);

/// What a quoted name refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Operand {
    Global(GlobalId),
    Dimension(DimensionRef),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Pow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Function {
    Sin,
    Cos,
    Tan,
    Asin,
    Acos,
    Atan,
    Sqrt,
    Abs,
    Exp,
    /// Natural logarithm (`log` in SolidWorks).
    Log,
    Log10,
    /// Integer part.
    Int,
    Round,
    Sgn,
    Min,
    Max,
}

const FUNCTIONS: [(&str, Function, usize); 16] = [
    ("sin", Function::Sin, 1),
    ("cos", Function::Cos, 1),
    ("tan", Function::Tan, 1),
    ("asin", Function::Asin, 1),
    ("acos", Function::Acos, 1),
    ("atan", Function::Atan, 1),
    ("sqrt", Function::Sqrt, 1),
    ("abs", Function::Abs, 1),
    ("exp", Function::Exp, 1),
    ("log", Function::Log, 1),
    ("log10", Function::Log10, 1),
    ("int", Function::Int, 1),
    ("round", Function::Round, 1),
    ("sgn", Function::Sgn, 1),
    ("min", Function::Min, 2),
    ("max", Function::Max, 2),
];

impl Function {
    fn name(self) -> &'static str {
        FUNCTIONS
            .iter()
            .find(|(_, f, _)| *f == self)
            .map_or("?", |(name, _, _)| name)
    }

    fn arity(self) -> usize {
        FUNCTIONS
            .iter()
            .find(|(_, f, _)| *f == self)
            .map_or(1, |(_, _, n)| *n)
    }

    fn lookup(name: &str) -> Option<Self> {
        let name = name.to_ascii_lowercase();
        // SolidWorks spellings.
        let name = match name.as_str() {
            "arcsin" => "asin",
            "arccos" => "acos",
            "arctan" | "atn" => "atan",
            "sqr" => "sqrt",
            "ln" => "log",
            other => other,
        };
        FUNCTIONS
            .iter()
            .find(|(n, _, _)| *n == name)
            .map(|(_, f, _)| *f)
    }
}

/// A parsed expression.
#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Number(f64),
    /// π.
    Pi,
    /// A quoted name. `operand` is `None` when the name matched nothing when it was read
    /// (e.g. from a file whose dimension was deleted); `name` is the name as last known, so
    /// the expression can still be shown and saved, and is reported as broken.
    Name {
        operand: Option<Operand>,
        name: String,
    },
    Neg(Box<Expr>),
    Binary(BinaryOp, Box<Expr>, Box<Expr>),
    Call(Function, Vec<Expr>),
}

impl Expr {
    /// Parses `text`, resolving each quoted name with `resolve`. Names that do not resolve
    /// are kept with `operand: None` (see [`Expr::unresolved`]).
    pub fn parse(text: &str, resolve: &dyn Fn(&str) -> Option<Operand>) -> Result<Expr, String> {
        let tokens = tokenize(text)?;
        let mut parser = Parser {
            tokens,
            position: 0,
            depth: 0,
            resolve,
        };
        let expr = parser.expression()?;
        match parser.peek() {
            None => Ok(expr),
            Some(token) => Err(format!("unexpected {token}")),
        }
    }

    /// Names that matched nothing.
    pub fn unresolved(&self) -> Vec<&str> {
        let mut out = Vec::new();
        self.visit(&mut |e| {
            if let Expr::Name {
                operand: None,
                name,
            } = e
            {
                out.push(name.as_str());
            }
        });
        out
    }

    /// Every reference in the expression.
    pub fn operands(&self) -> Vec<Operand> {
        let mut out = Vec::new();
        self.visit(&mut |e| {
            if let Expr::Name {
                operand: Some(operand),
                ..
            } = e
            {
                out.push(*operand);
            }
        });
        out
    }

    /// Updates the remembered names (after a rename), with `name_of` returning the current
    /// name of a reference, or `None` if it no longer exists.
    pub fn refresh_names(&mut self, name_of: &dyn Fn(Operand) -> Option<String>) {
        self.visit_mut(&mut |e| {
            if let Expr::Name {
                operand: Some(operand),
                name,
            } = e
            {
                if let Some(current) = name_of(*operand) {
                    *name = current;
                }
            }
        });
    }

    /// Evaluates the expression; `value_of` gives the value of a reference in display units.
    pub fn evaluate(
        &self,
        value_of: &mut dyn FnMut(Operand, &str) -> Result<f64, String>,
    ) -> Result<f64, String> {
        let value = match self {
            Expr::Number(v) => *v,
            Expr::Pi => std::f64::consts::PI,
            Expr::Name {
                operand: Some(operand),
                name,
            } => value_of(*operand, name)?,
            Expr::Name {
                operand: None,
                name,
            } => return Err(format!("\"{name}\" does not exist")),
            Expr::Neg(e) => -e.evaluate(value_of)?,
            Expr::Binary(op, a, b) => {
                let (a, b) = (a.evaluate(value_of)?, b.evaluate(value_of)?);
                match op {
                    BinaryOp::Add => a + b,
                    BinaryOp::Sub => a - b,
                    BinaryOp::Mul => a * b,
                    BinaryOp::Div => {
                        if b == 0.0 {
                            return Err("division by zero".to_owned());
                        }
                        a / b
                    }
                    BinaryOp::Pow => a.powf(b),
                }
            }
            Expr::Call(function, args) => {
                let mut values = Vec::with_capacity(args.len());
                for arg in args {
                    values.push(arg.evaluate(value_of)?);
                }
                let x = values.first().copied().unwrap_or(0.0);
                let y = values.get(1).copied().unwrap_or(0.0);
                match function {
                    Function::Sin => x.to_radians().sin(),
                    Function::Cos => x.to_radians().cos(),
                    Function::Tan => x.to_radians().tan(),
                    Function::Asin => x.asin().to_degrees(),
                    Function::Acos => x.acos().to_degrees(),
                    Function::Atan => x.atan().to_degrees(),
                    Function::Sqrt => x.sqrt(),
                    Function::Abs => x.abs(),
                    Function::Exp => x.exp(),
                    Function::Log => x.ln(),
                    Function::Log10 => x.log10(),
                    Function::Int => x.trunc(),
                    Function::Round => x.round(),
                    Function::Sgn => {
                        if x > 0.0 {
                            1.0
                        } else if x < 0.0 {
                            -1.0
                        } else {
                            0.0
                        }
                    }
                    Function::Min => x.min(y),
                    Function::Max => x.max(y),
                }
            }
        };
        if value.is_finite() {
            Ok(value)
        } else {
            Err("the result is not a number".to_owned())
        }
    }

    fn visit<'a>(&'a self, f: &mut dyn FnMut(&'a Expr)) {
        f(self);
        match self {
            Expr::Neg(e) => e.visit(f),
            Expr::Binary(_, a, b) => {
                a.visit(f);
                b.visit(f);
            }
            Expr::Call(_, args) => args.iter().for_each(|a| a.visit(f)),
            Expr::Number(_) | Expr::Pi | Expr::Name { .. } => {}
        }
    }

    fn visit_mut(&mut self, f: &mut dyn FnMut(&mut Expr)) {
        f(self);
        match self {
            Expr::Neg(e) => e.visit_mut(f),
            Expr::Binary(_, a, b) => {
                a.visit_mut(f);
                b.visit_mut(f);
            }
            Expr::Call(_, args) => args.iter_mut().for_each(|a| a.visit_mut(f)),
            Expr::Number(_) | Expr::Pi | Expr::Name { .. } => {}
        }
    }

    /// Binding strength, for printing the fewest parentheses.
    fn precedence(&self) -> u8 {
        match self {
            Expr::Binary(BinaryOp::Add | BinaryOp::Sub, ..) => 1,
            Expr::Binary(BinaryOp::Mul | BinaryOp::Div, ..) => 2,
            Expr::Neg(_) => 3,
            Expr::Binary(BinaryOp::Pow, ..) => 4,
            Expr::Number(v) if *v < 0.0 => 3,
            Expr::Number(_) | Expr::Pi | Expr::Name { .. } | Expr::Call(..) => 5,
        }
    }

    fn write(&self, f: &mut fmt::Formatter<'_>, required: u8) -> fmt::Result {
        let parens = self.precedence() < required;
        if parens {
            write!(f, "(")?;
        }
        match self {
            Expr::Number(v) => write!(f, "{v}")?,
            Expr::Pi => write!(f, "pi")?,
            Expr::Name { name, .. } => write!(f, "\"{name}\"")?,
            Expr::Neg(e) => {
                write!(f, "-")?;
                e.write(f, 3)?;
            }
            Expr::Binary(op, a, b) => {
                let (symbol, p) = match op {
                    BinaryOp::Add => (" + ", 1),
                    BinaryOp::Sub => (" - ", 1),
                    BinaryOp::Mul => (" * ", 2),
                    BinaryOp::Div => (" / ", 2),
                    BinaryOp::Pow => ("^", 4),
                };
                if *op == BinaryOp::Pow {
                    // Right-associative; the exponent may be signed: 2^-1.
                    a.write(f, 5)?;
                    write!(f, "{symbol}")?;
                    b.write(f, 3)?;
                } else {
                    a.write(f, p)?;
                    write!(f, "{symbol}")?;
                    b.write(f, p + 1)?;
                }
            }
            Expr::Call(function, args) => {
                write!(f, "{}(", function.name())?;
                for (i, arg) in args.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    arg.write(f, 0)?;
                }
                write!(f, ")")?;
            }
        }
        if parens {
            write!(f, ")")?;
        }
        Ok(())
    }
}

/// Prints the expression with the names it remembers; call [`Expr::refresh_names`] first
/// for the current ones.
impl fmt::Display for Expr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.write(f, 0)
    }
}

// ---- Tokens --------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Number(f64),
    Name(String),
    Ident(String),
    Op(char),
}

impl fmt::Display for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Token::Number(v) => write!(f, "number {v}"),
            Token::Name(n) => write!(f, "\"{n}\""),
            Token::Ident(i) => write!(f, "\"{i}\" (names go between double quotes)"),
            Token::Op(c) => write!(f, "'{c}'"),
        }
    }
}

fn tokenize(text: &str) -> Result<Vec<Token>, String> {
    let mut chars = text.trim().chars().peekable();
    // A leading '=' is accepted, as typed in the SolidWorks equation editor.
    if chars.peek() == Some(&'=') {
        chars.next();
    }
    let mut tokens = Vec::new();
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
        } else if c.is_ascii_digit() || c == '.' {
            let mut number = String::new();
            while let Some(&d) = chars.peek() {
                let exponent_sign =
                    (d == '+' || d == '-') && matches!(number.chars().last(), Some('e' | 'E'));
                if d.is_ascii_digit() || d == '.' || d == 'e' || d == 'E' || exponent_sign {
                    number.push(d);
                    chars.next();
                } else {
                    break;
                }
            }
            let value = number
                .parse::<f64>()
                .map_err(|_| format!("\"{number}\" is not a number"))?;
            tokens.push(Token::Number(value));
        } else if c == '"' {
            chars.next();
            let mut name = String::new();
            loop {
                match chars.next() {
                    Some('"') => break,
                    Some(ch) => name.push(ch),
                    None => return Err(format!("missing closing quote after \"{name}")),
                }
            }
            if name.is_empty() {
                return Err("empty name \"\"".to_owned());
            }
            tokens.push(Token::Name(name));
        } else if c.is_alphabetic() || c == '_' {
            let mut ident = String::new();
            while let Some(&d) = chars.peek() {
                if d.is_alphanumeric() || d == '_' {
                    ident.push(d);
                    chars.next();
                } else {
                    break;
                }
            }
            tokens.push(Token::Ident(ident));
        } else if "+-*/^(),".contains(c) {
            tokens.push(Token::Op(c));
            chars.next();
        } else {
            return Err(format!("unexpected character '{c}'"));
        }
    }
    if tokens.is_empty() {
        return Err("the expression is empty".to_owned());
    }
    Ok(tokens)
}

// ---- Parser --------------------------------------------------------------------------------

/// Deepest nesting of parentheses, signs and powers an expression may have. Parsing,
/// evaluating and printing recurse that deep: a file holding thousands of nested
/// parentheses must be refused, not overflow the stack.
const MAX_DEPTH: usize = 200;

struct Parser<'a> {
    tokens: Vec<Token>,
    position: usize,
    /// Nesting of the `unary` rule being parsed, which every recursion goes through.
    depth: usize,
    resolve: &'a dyn Fn(&str) -> Option<Operand>,
}

impl Parser<'_> {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.position)
    }

    fn next(&mut self) -> Option<Token> {
        let token = self.tokens.get(self.position).cloned();
        self.position += 1;
        token
    }

    fn eat(&mut self, op: char) -> bool {
        if self.peek() == Some(&Token::Op(op)) {
            self.position += 1;
            true
        } else {
            false
        }
    }

    fn expect(&mut self, op: char) -> Result<(), String> {
        if self.eat(op) {
            Ok(())
        } else {
            match self.peek() {
                Some(token) => Err(format!("expected '{op}', found {token}")),
                None => Err(format!("expected '{op}' at the end")),
            }
        }
    }

    /// sum := product (('+' | '-') product)*
    fn expression(&mut self) -> Result<Expr, String> {
        let mut left = self.product()?;
        loop {
            let op = if self.eat('+') {
                BinaryOp::Add
            } else if self.eat('-') {
                BinaryOp::Sub
            } else {
                return Ok(left);
            };
            let right = self.product()?;
            left = Expr::Binary(op, Box::new(left), Box::new(right));
        }
    }

    /// product := unary (('*' | '/') unary)*
    fn product(&mut self) -> Result<Expr, String> {
        let mut left = self.unary()?;
        loop {
            let op = if self.eat('*') {
                BinaryOp::Mul
            } else if self.eat('/') {
                BinaryOp::Div
            } else {
                return Ok(left);
            };
            let right = self.unary()?;
            left = Expr::Binary(op, Box::new(left), Box::new(right));
        }
    }

    /// unary := ('-' | '+') unary | power
    fn unary(&mut self) -> Result<Expr, String> {
        if self.depth >= MAX_DEPTH {
            return Err("the expression is nested too deeply".to_owned());
        }
        self.depth += 1;
        let result = self.signed();
        self.depth -= 1;
        result
    }

    fn signed(&mut self) -> Result<Expr, String> {
        if self.eat('-') {
            return Ok(Expr::Neg(Box::new(self.unary()?)));
        }
        if self.eat('+') {
            return self.unary();
        }
        self.power()
    }

    /// power := atom ('^' unary)?
    fn power(&mut self) -> Result<Expr, String> {
        let base = self.atom()?;
        if self.eat('^') {
            let exponent = self.unary()?;
            return Ok(Expr::Binary(
                BinaryOp::Pow,
                Box::new(base),
                Box::new(exponent),
            ));
        }
        Ok(base)
    }

    fn atom(&mut self) -> Result<Expr, String> {
        match self.next() {
            Some(Token::Number(v)) => Ok(Expr::Number(v)),
            Some(Token::Name(name)) => Ok(Expr::Name {
                operand: (self.resolve)(&name),
                name,
            }),
            Some(Token::Op('(')) => {
                let inner = self.expression()?;
                self.expect(')')?;
                Ok(inner)
            }
            Some(Token::Ident(ident)) => {
                if ident.eq_ignore_ascii_case("pi") {
                    return Ok(Expr::Pi);
                }
                let Some(function) = Function::lookup(&ident) else {
                    return Err(format!(
                        "unknown function {ident} (names go between double quotes)"
                    ));
                };
                self.expect('(')?;
                let mut args = vec![self.expression()?];
                while self.eat(',') {
                    args.push(self.expression()?);
                }
                self.expect(')')?;
                if args.len() != function.arity() {
                    return Err(format!(
                        "{}() takes {} argument(s)",
                        function.name(),
                        function.arity()
                    ));
                }
                Ok(Expr::Call(function, args))
            }
            Some(token) => Err(format!("unexpected {token}")),
            None => Err("the expression ends too early".to_owned()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolve(name: &str) -> Option<Operand> {
        match name {
            "Width" => Some(Operand::Global(GlobalId(1))),
            "Height" => Some(Operand::Global(GlobalId(2))),
            _ => None,
        }
    }

    fn eval(text: &str) -> Result<f64, String> {
        let expr = Expr::parse(text, &resolve)?;
        expr.evaluate(&mut |operand, _| match operand {
            Operand::Global(GlobalId(1)) => Ok(30.0),
            Operand::Global(GlobalId(2)) => Ok(40.0),
            _ => Err("unknown".to_owned()),
        })
    }

    #[test]
    fn arithmetic_follows_the_usual_rules() {
        assert_eq!(eval("1 + 2 * 3").unwrap(), 7.0);
        assert_eq!(eval("(1 + 2) * 3").unwrap(), 9.0);
        assert_eq!(eval("10 - 4 - 3").unwrap(), 3.0);
        assert_eq!(eval("2 ^ 3 ^ 2").unwrap(), 512.0);
        assert_eq!(eval("-2 ^ 2").unwrap(), -4.0);
        assert_eq!(eval("2 ^ -1").unwrap(), 0.5);
        assert_eq!(eval("= 1.5e2 / 3").unwrap(), 50.0);
    }

    #[test]
    fn names_functions_and_degrees() {
        assert_eq!(eval("sqrt(\"Width\"^2 + \"Height\"^2)").unwrap(), 50.0);
        assert!((eval("sin(30)").unwrap() - 0.5).abs() < 1e-12);
        assert!((eval("atan(1)").unwrap() - 45.0).abs() < 1e-12);
        assert_eq!(eval("max(\"Width\", 35)").unwrap(), 35.0);
        assert_eq!(eval("int(7.9) + sgn(-3)").unwrap(), 6.0);
        assert!((eval("2 * PI").unwrap() - std::f64::consts::TAU).abs() < 1e-12);
    }

    #[test]
    fn errors_are_explained() {
        assert!(eval("").is_err());
        assert!(eval("1 +").is_err());
        assert!(eval("(1").is_err());
        assert!(eval("Width * 2").unwrap_err().contains("double quotes"));
        assert!(
            eval("\"Depth\" * 2")
                .unwrap_err()
                .contains("does not exist")
        );
        assert!(eval("1 / 0").unwrap_err().contains("division"));
        assert!(eval("sqrt(-1)").is_err());
        assert!(eval("min(1)").unwrap_err().contains("argument"));
        assert!(eval("\"Width").unwrap_err().contains("quote"));
        // Deep nesting is refused instead of overflowing the stack.
        let deep = format!("{}1{}", "(".repeat(100_000), ")".repeat(100_000));
        assert!(eval(&deep).unwrap_err().contains("nested"));
        assert!(eval(&"-".repeat(100_000)).unwrap_err().contains("nested"));
        let fine = format!("{}1{}", "(".repeat(50), ")".repeat(50));
        assert_eq!(eval(&fine).unwrap(), 1.0);
    }

    #[test]
    fn printing_round_trips_with_few_parentheses() {
        for text in [
            "\"Width\" * 2 + 5",
            "(\"Width\" + 5) * 2",
            "10 - (4 - 3)",
            "-(\"Width\" + 1)",
            "2^3^2",
            "(2^3)^2",
            "sqrt(\"Width\"^2 + \"Height\"^2)",
            "max(1, 2) / 3",
        ] {
            let expr = Expr::parse(text, &resolve).unwrap();
            assert_eq!(expr.to_string(), text);
            assert_eq!(Expr::parse(&expr.to_string(), &resolve).unwrap(), expr);
        }
    }

    #[test]
    fn renamed_references_print_their_new_name() {
        let mut expr = Expr::parse("\"Width\" * 2", &resolve).unwrap();
        expr.refresh_names(&|operand| match operand {
            Operand::Global(GlobalId(1)) => Some("Breadth".to_owned()),
            _ => None,
        });
        assert_eq!(expr.to_string(), "\"Breadth\" * 2");
        assert_eq!(expr.operands(), vec![Operand::Global(GlobalId(1))]);
    }
}
