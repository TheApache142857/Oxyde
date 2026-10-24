//! ISO 10303-21, the clear-text exchange structure STEP files are written in, read and
//! written from the standard's grammar: a header section, then a data section of numbered
//! entity instances, each a simple record `#12 = NAME(params);` or a complex one of several
//! records `#12 = (A(..) B(..));`, their parameters integers, reals, strings, enumerations,
//! references to other instances, typed values `LENGTH_MEASURE(1.)`, lists, `$` (unset) and
//! `*` (derived).

use std::collections::HashMap;
use std::fmt::Write as _;

/// A parameter of an entity instance.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Param {
    Integer(i64),
    Real(f64),
    String(String),
    /// `.NAME.` (`.T.` and `.F.` among them).
    Enum(String),
    /// `#12`.
    Ref(u64),
    List(Vec<Param>),
    /// `NAME(value)`: a value of a defined type, as a select names it.
    Typed(String, Box<Param>),
    /// `$`.
    Unset,
    /// `*`.
    Derived,
    /// `"0123"`: a binary, as written.
    Binary(String),
}

/// One record of an entity instance: its name and its parameters.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Record {
    pub name: String,
    pub params: Vec<Param>,
}

/// An entity instance: one record, or a complex instance of several (each its own part of
/// the entity's supertype tree, in the order written).
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Instance {
    Simple(Record),
    Complex(Vec<Record>),
}

impl Instance {
    /// The record of the given name: the instance's own, or one of a complex instance's.
    pub fn record(&self, name: &str) -> Option<&Record> {
        match self {
            Instance::Simple(r) => (r.name == name).then_some(r),
            Instance::Complex(rs) => rs.iter().find(|r| r.name == name),
        }
    }

    /// Whether the instance is, or has a part, of the given name.
    pub fn is(&self, name: &str) -> bool {
        self.record(name).is_some()
    }
}

/// A STEP file read: its header's records and its data section's instances, by number.
#[derive(Debug, Clone, Default)]
pub(crate) struct Exchange {
    pub header: Vec<Record>,
    pub data: HashMap<u64, Instance>,
}

/// Why a file could not be read.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ParseError {
    pub at: usize,
    pub message: String,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} (at byte {})", self.message, self.at)
    }
}

struct Reader<'a> {
    text: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn error<T>(&self, message: &str) -> Result<T, ParseError> {
        Err(ParseError {
            at: self.at,
            message: message.to_owned(),
        })
    }

    /// Past white space and comments.
    fn skip(&mut self) {
        loop {
            while self.at < self.text.len() && self.text[self.at].is_ascii_whitespace() {
                self.at += 1;
            }
            if self.text[self.at..].starts_with(b"/*") {
                match self.text[self.at + 2..].windows(2).position(|w| w == b"*/") {
                    Some(end) => self.at += end + 4,
                    None => self.at = self.text.len(),
                }
                continue;
            }
            break;
        }
    }

    fn peek(&mut self) -> Option<u8> {
        self.skip();
        self.text.get(self.at).copied()
    }

    fn expect(&mut self, c: u8) -> Result<(), ParseError> {
        if self.peek() == Some(c) {
            self.at += 1;
            Ok(())
        } else {
            self.error(&format!("expected '{}'", c as char))
        }
    }

    fn keyword(&mut self) -> Result<String, ParseError> {
        self.skip();
        let start = self.at;
        while self.at < self.text.len()
            && (self.text[self.at].is_ascii_alphanumeric()
                || self.text[self.at] == b'_'
                || self.text[self.at] == b'!')
        {
            self.at += 1;
        }
        if self.at == start {
            return self.error("expected a keyword");
        }
        Ok(String::from_utf8_lossy(&self.text[start..self.at]).to_ascii_uppercase())
    }

    fn integer(&mut self) -> Result<u64, ParseError> {
        let start = self.at;
        while self.at < self.text.len() && self.text[self.at].is_ascii_digit() {
            self.at += 1;
        }
        std::str::from_utf8(&self.text[start..self.at])
            .ok()
            .and_then(|s| s.parse().ok())
            .map_or_else(|| self.error("expected a number"), Ok)
    }

    fn string(&mut self) -> Result<String, ParseError> {
        // At the opening quote.
        self.at += 1;
        let mut raw = Vec::new();
        loop {
            match self.text.get(self.at) {
                None => return self.error("a string that does not end"),
                Some(b'\'') if self.text.get(self.at + 1) == Some(&b'\'') => {
                    raw.push(b'\'');
                    self.at += 2;
                }
                Some(b'\'') => {
                    self.at += 1;
                    break;
                }
                Some(&c) => {
                    raw.push(c);
                    self.at += 1;
                }
            }
        }
        Ok(decode(&raw))
    }

    fn param(&mut self) -> Result<Param, ParseError> {
        match self.peek() {
            Some(b'$') => {
                self.at += 1;
                Ok(Param::Unset)
            }
            Some(b'*') => {
                self.at += 1;
                Ok(Param::Derived)
            }
            Some(b'#') => {
                self.at += 1;
                Ok(Param::Ref(self.integer()?))
            }
            Some(b'\'') => Ok(Param::String(self.string()?)),
            Some(b'"') => {
                self.at += 1;
                let start = self.at;
                while self.at < self.text.len() && self.text[self.at] != b'"' {
                    self.at += 1;
                }
                let s = String::from_utf8_lossy(&self.text[start..self.at]).into_owned();
                self.at += 1;
                Ok(Param::Binary(s))
            }
            Some(b'.') => {
                self.at += 1;
                let start = self.at;
                while self.at < self.text.len() && self.text[self.at] != b'.' {
                    self.at += 1;
                }
                let s = String::from_utf8_lossy(&self.text[start..self.at]).to_ascii_uppercase();
                self.at += 1;
                Ok(Param::Enum(s))
            }
            Some(b'(') => Ok(Param::List(self.list()?)),
            Some(c) if c == b'-' || c == b'+' || c.is_ascii_digit() => self.number(),
            Some(c) if c.is_ascii_alphabetic() || c == b'_' => {
                let name = self.keyword()?;
                self.expect(b'(')?;
                let value = self.param()?;
                self.expect(b')')?;
                Ok(Param::Typed(name, Box::new(value)))
            }
            _ => self.error("expected a parameter"),
        }
    }

    fn number(&mut self) -> Result<Param, ParseError> {
        let start = self.at;
        if matches!(self.text.get(self.at), Some(b'-' | b'+')) {
            self.at += 1;
        }
        let mut real = false;
        while let Some(&c) = self.text.get(self.at) {
            if c.is_ascii_digit() {
                self.at += 1;
            } else if c == b'.' || c == b'E' || c == b'e' {
                real = true;
                self.at += 1;
                if (c == b'E' || c == b'e') && matches!(self.text.get(self.at), Some(b'-' | b'+')) {
                    self.at += 1;
                }
            } else {
                break;
            }
        }
        let s = std::str::from_utf8(&self.text[start..self.at]).unwrap_or("");
        if real {
            // "1." and "1.E-07" are reals as Rust reads them once a digit follows the point.
            let fixed = if s.contains(".E") || s.contains(".e") {
                s.replacen(".E", ".0E", 1).replacen(".e", ".0e", 1)
            } else if s.ends_with('.') {
                format!("{s}0")
            } else {
                s.to_owned()
            };
            fixed
                .parse()
                .map(Param::Real)
                .map_or_else(|_| self.error("a real that does not read"), Ok)
        } else {
            s.parse()
                .map(Param::Integer)
                .map_or_else(|_| self.error("an integer that does not read"), Ok)
        }
    }

    fn list(&mut self) -> Result<Vec<Param>, ParseError> {
        self.expect(b'(')?;
        let mut out = Vec::new();
        if self.peek() == Some(b')') {
            self.at += 1;
            return Ok(out);
        }
        loop {
            out.push(self.param()?);
            match self.peek() {
                Some(b',') => self.at += 1,
                Some(b')') => {
                    self.at += 1;
                    return Ok(out);
                }
                _ => return self.error("expected ',' or ')' in a list"),
            }
        }
    }

    fn record(&mut self) -> Result<Record, ParseError> {
        let name = self.keyword()?;
        let params = self.list()?;
        Ok(Record { name, params })
    }

    fn instance(&mut self) -> Result<Instance, ParseError> {
        if self.peek() == Some(b'(') {
            self.at += 1;
            let mut records = Vec::new();
            while self.peek() != Some(b')') {
                if self.peek().is_none() {
                    return self.error("a complex instance that does not end");
                }
                records.push(self.record()?);
            }
            self.at += 1;
            Ok(Instance::Complex(records))
        } else {
            Ok(Instance::Simple(self.record()?))
        }
    }
}

/// The text of a string as written: `\X2\…\X0\` (UTF-16), `\X\HH` (ISO 8859-1), `\S\c`
/// (the upper half), `\\` read; the rest as it is.
fn decode(raw: &[u8]) -> String {
    let mut out = String::new();
    let mut i = 0;
    let hex =
        |b: &[u8]| -> Option<u32> { u32::from_str_radix(std::str::from_utf8(b).ok()?, 16).ok() };
    while i < raw.len() {
        if raw[i] == b'\\' {
            if raw[i..].starts_with(b"\\X2\\") || raw[i..].starts_with(b"\\X4\\") {
                let width = if raw[i + 2] == b'2' { 4 } else { 8 };
                i += 4;
                let mut units = Vec::new();
                while i + width <= raw.len() && !raw[i..].starts_with(b"\\X0\\") {
                    match hex(&raw[i..i + width]) {
                        Some(u) => units.push(u),
                        None => break,
                    }
                    i += width;
                }
                if width == 4 {
                    let u16s: Vec<u16> = units.iter().map(|&u| u as u16).collect();
                    out.push_str(&String::from_utf16_lossy(&u16s));
                } else {
                    out.extend(units.iter().filter_map(|&u| char::from_u32(u)));
                }
                if raw[i..].starts_with(b"\\X0\\") {
                    i += 4;
                }
                continue;
            }
            if raw[i..].starts_with(b"\\X\\") && i + 5 <= raw.len() {
                if let Some(c) = hex(&raw[i + 3..i + 5]).and_then(char::from_u32) {
                    out.push(c);
                }
                i += 5;
                continue;
            }
            if raw[i..].starts_with(b"\\S\\") && i + 4 <= raw.len() {
                if let Some(c) = char::from_u32(u32::from(raw[i + 3]) + 128) {
                    out.push(c);
                }
                i += 4;
                continue;
            }
            if raw[i..].starts_with(b"\\\\") {
                out.push('\\');
                i += 2;
                continue;
            }
        }
        // Plain text, read as UTF-8 where it is (some writers write it so), else as bytes.
        let start = i;
        while i < raw.len() && raw[i] != b'\\' {
            i += 1;
        }
        out.push_str(&String::from_utf8_lossy(&raw[start..i]));
        if i < raw.len() && raw[i] == b'\\' && start == i {
            out.push('\\');
            i += 1;
        }
    }
    out
}

/// Reads a STEP file.
pub(crate) fn parse(text: &[u8]) -> Result<Exchange, ParseError> {
    let mut r = Reader { text, at: 0 };
    let start = r.keyword()?;
    if start != "ISO-10303-21" && !start.starts_with("ISO") {
        // "ISO-10303-21" reads as the keyword "ISO" then "-10303-21".
    }
    // Past "ISO-10303-21;".
    match text.windows(1).skip(r.at).position(|w| w == b";") {
        Some(p) => r.at += p + 1,
        None => return r.error("not a STEP file"),
    }
    let mut out = Exchange::default();
    loop {
        r.skip();
        if r.at >= text.len() {
            return r.error("a file without its END-ISO-10303-21");
        }
        let word = r.keyword()?;
        match word.as_str() {
            "HEADER" => {
                r.expect(b';')?;
                loop {
                    r.skip();
                    if r.text[r.at..].starts_with(b"ENDSEC") {
                        r.keyword()?;
                        r.expect(b';')?;
                        break;
                    }
                    out.header.push(r.record()?);
                    r.expect(b';')?;
                }
            }
            "DATA" => {
                // DATA; or DATA('name', (schemas));
                if r.peek() == Some(b'(') {
                    r.list()?;
                }
                r.expect(b';')?;
                loop {
                    match r.peek() {
                        Some(b'#') => {
                            r.at += 1;
                            let id = r.integer()?;
                            r.expect(b'=')?;
                            let instance = r.instance()?;
                            r.expect(b';')?;
                            out.data.insert(id, instance);
                        }
                        Some(_) => {
                            r.keyword()?;
                            r.expect(b';')?;
                            break;
                        }
                        None => return r.error("a data section that does not end"),
                    }
                }
            }
            "END" => {
                return Ok(out);
            }
            _ => {
                // "END-ISO-10303-21;" reads as "END" (above); anything else is skipped to its ';'.
                match text.iter().skip(r.at).position(|&c| c == b';') {
                    Some(p) => r.at += p + 1,
                    None => return Ok(out),
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------------------
// Writing.

/// A STEP file being written: instances numbered as they are added.
#[derive(Debug, Default)]
pub(crate) struct Writer {
    lines: Vec<(u64, String)>,
    next: u64,
}

impl Param {
    pub fn str(s: &str) -> Param {
        Param::String(s.to_owned())
    }

    pub fn reals(values: &[f64]) -> Param {
        Param::List(values.iter().map(|&v| Param::Real(v)).collect())
    }

    pub fn refs(ids: &[u64]) -> Param {
        Param::List(ids.iter().map(|&i| Param::Ref(i)).collect())
    }

    pub fn bool(b: bool) -> Param {
        Param::Enum(if b { "T".to_owned() } else { "F".to_owned() })
    }

    pub fn enumerated(name: &str) -> Param {
        Param::Enum(name.to_owned())
    }
}

/// `x` as STEP writes a real: always with a point, as short as it reads back the same.
fn real(x: f64) -> String {
    if !x.is_finite() {
        return "0.".to_owned();
    }
    let s = format!("{x:?}");
    let s = if s.contains('e') {
        // 1e-7 → 1.E-07 style is not needed; "1.0E-7" reads everywhere.
        let (m, e) = s.split_once('e').unwrap_or((&s, "0"));
        let m = if m.contains('.') {
            m.to_owned()
        } else {
            format!("{m}.")
        };
        format!("{m}E{e}")
    } else {
        s
    };
    s.strip_suffix(".0").map_or(s.clone(), |t| format!("{t}."))
}

/// A string as STEP writes it: quotes doubled, backslashes doubled, anything beyond ASCII
/// as `\X2\…\X0\`.
fn quoted(s: &str) -> String {
    let mut out = String::from("'");
    for c in s.chars() {
        match c {
            '\'' => out.push_str("''"),
            '\\' => out.push_str("\\\\"),
            c if (' '..='~').contains(&c) => out.push(c),
            c => {
                let mut buf = [0u16; 2];
                out.push_str("\\X2\\");
                for u in c.encode_utf16(&mut buf) {
                    let _ = write!(out, "{:04X}", u);
                }
                out.push_str("\\X0\\");
            }
        }
    }
    out.push('\'');
    out
}

fn write_param(out: &mut String, p: &Param) {
    match p {
        Param::Integer(i) => {
            let _ = write!(out, "{i}");
        }
        Param::Real(x) => out.push_str(&real(*x)),
        Param::String(s) => out.push_str(&quoted(s)),
        Param::Enum(e) => {
            let _ = write!(out, ".{e}.");
        }
        Param::Ref(r) => {
            let _ = write!(out, "#{r}");
        }
        Param::List(items) => {
            out.push('(');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_param(out, item);
            }
            out.push(')');
        }
        Param::Typed(name, value) => {
            out.push_str(name);
            out.push('(');
            write_param(out, value);
            out.push(')');
        }
        Param::Unset => out.push('$'),
        Param::Derived => out.push('*'),
        Param::Binary(b) => {
            let _ = write!(out, "\"{b}\"");
        }
    }
}

fn write_record(out: &mut String, name: &str, params: &[Param]) {
    out.push_str(name);
    out.push('(');
    for (i, p) in params.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        write_param(out, p);
    }
    out.push(')');
}

impl Writer {
    pub fn new() -> Writer {
        Writer {
            lines: Vec::new(),
            next: 1,
        }
    }

    /// Adds a simple instance and gives its number.
    pub fn add(&mut self, name: &str, params: Vec<Param>) -> u64 {
        let id = self.next;
        self.next += 1;
        let mut s = String::new();
        write_record(&mut s, name, &params);
        self.lines.push((id, s));
        id
    }

    /// Adds a complex instance of these records, in the order given (the standard's: by
    /// name).
    pub fn add_complex(&mut self, records: Vec<(&str, Vec<Param>)>) -> u64 {
        let id = self.next;
        self.next += 1;
        let mut s = String::from("(");
        for (i, (name, params)) in records.iter().enumerate() {
            if i > 0 {
                s.push(' ');
            }
            write_record(&mut s, name, params);
        }
        s.push(')');
        self.lines.push((id, s));
        id
    }

    /// The whole file, with its header.
    pub fn finish(&self, description: &str, name: &str, schema: &str) -> String {
        let mut out = String::new();
        out.push_str("ISO-10303-21;\nHEADER;\n");
        let _ = writeln!(out, "FILE_DESCRIPTION(({}),'2;1');", quoted(description));
        let _ = writeln!(
            out,
            "FILE_NAME({},'',(''),(''),'Oxyde native kernel','Oxyde','');",
            quoted(name)
        );
        let _ = writeln!(out, "FILE_SCHEMA(({}));", quoted(schema));
        out.push_str("ENDSEC;\nDATA;\n");
        for (id, line) in &self.lines {
            let _ = writeln!(out, "#{id} = {line};");
        }
        out.push_str("ENDSEC;\nEND-ISO-10303-21;\n");
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_reads_and_writes_back() {
        let text = br"ISO-10303-21;
HEADER;
FILE_DESCRIPTION(('A test'),'2;1');
FILE_NAME('x','2026-10-24',('a'),('b'),'c','d','e');
FILE_SCHEMA(('AUTOMOTIVE_DESIGN { 1 0 10303 214 1 1 1 1 }'));
ENDSEC;
DATA;
#1 = CARTESIAN_POINT('',(0.,-1.5,2.E-07));
/* a comment */ #2 = DIRECTION('n',(0.,0.,1.));
#3 = ( LENGTH_UNIT() NAMED_UNIT(*) SI_UNIT(.MILLI.,.METRE.) );
#4 = UNCERTAINTY_MEASURE_WITH_UNIT(LENGTH_MEASURE(1.E-07),#3,'it''s',$);
#5 = PRODUCT('\X2\00E9\X0\t\X\E9','',$,(#1,#2));
ENDSEC;
END-ISO-10303-21;
";
        let x = parse(text).unwrap();
        assert_eq!(x.header.len(), 3);
        assert_eq!(
            x.data[&1],
            Instance::Simple(Record {
                name: "CARTESIAN_POINT".to_owned(),
                params: vec![Param::str(""), Param::reals(&[0.0, -1.5, 2e-7])],
            })
        );
        assert!(x.data[&3].is("SI_UNIT") && x.data[&3].is("LENGTH_UNIT"));
        let Instance::Simple(r) = &x.data[&4] else {
            panic!()
        };
        assert_eq!(
            r.params[0],
            Param::Typed("LENGTH_MEASURE".to_owned(), Box::new(Param::Real(1e-7)))
        );
        assert_eq!(r.params[2], Param::str("it's"));
        assert_eq!(r.params[3], Param::Unset);
        let Instance::Simple(p) = &x.data[&5] else {
            panic!()
        };
        assert_eq!(p.params[0], Param::str("été"));
        // Written, then read again: the same.
        let mut w = Writer::new();
        let a = w.add(
            "CARTESIAN_POINT",
            vec![Param::str("é'"), Param::reals(&[1.0, -2.5e-12, 3e20])],
        );
        w.add_complex(vec![
            ("LENGTH_UNIT", vec![]),
            ("NAMED_UNIT", vec![Param::Derived]),
        ]);
        w.add(
            "X",
            vec![Param::Ref(a), Param::bool(true), Param::Integer(-3)],
        );
        let back = parse(w.finish("d", "n", "S").as_bytes()).unwrap();
        let Instance::Simple(p) = &back.data[&1] else {
            panic!()
        };
        assert_eq!(
            p.params,
            vec![Param::str("é'"), Param::reals(&[1.0, -2.5e-12, 3e20])]
        );
        assert!(back.data[&2].is("NAMED_UNIT"));
        let Instance::Simple(q) = &back.data[&3] else {
            panic!()
        };
        assert_eq!(
            q.params,
            vec![Param::Ref(1), Param::bool(true), Param::Integer(-3)]
        );
    }
}
