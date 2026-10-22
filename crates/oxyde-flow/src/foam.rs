//! Reading OpenFOAM's ASCII files: the `FoamFile` header, dictionaries, lists (with a
//! count, `N(…)`, or uniform, `N{…}`), and the fields a solver writes.
//!
//! Only what the mesh and field files hold is understood; binary files are refused (the
//! cases this crate writes ask for ASCII).

use std::fmt;

/// Why a file could not be read.
#[derive(Debug, Clone, PartialEq)]
pub enum FoamError {
    /// The text is not what was expected at byte `at`.
    Syntax { at: usize, expected: String },
    /// The file is written in binary.
    Binary,
}

impl fmt::Display for FoamError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FoamError::Syntax { at, expected } => write!(f, "expected {expected} at byte {at}"),
            FoamError::Binary => write!(f, "the file is binary, not ASCII"),
        }
    }
}

impl std::error::Error for FoamError {}

/// A cursor over a file's text that skips white space and comments.
pub struct Lexer<'a> {
    text: &'a [u8],
    at: usize,
}

fn is_delimiter(b: u8) -> bool {
    b.is_ascii_whitespace() || matches!(b, b';' | b'{' | b'}' | b'(' | b')' | b'[' | b']' | b'"')
}

impl<'a> Lexer<'a> {
    pub fn new(text: &'a [u8]) -> Self {
        Self { text, at: 0 }
    }

    fn error(&self, expected: &str) -> FoamError {
        FoamError::Syntax {
            at: self.at,
            expected: expected.to_string(),
        }
    }

    fn byte(&self, i: usize) -> Option<u8> {
        self.text.get(i).copied()
    }

    /// Moves past white space and comments.
    fn skip(&mut self) {
        loop {
            while self.byte(self.at).is_some_and(|b| b.is_ascii_whitespace()) {
                self.at += 1;
            }
            match (self.byte(self.at), self.byte(self.at + 1)) {
                (Some(b'/'), Some(b'/')) => {
                    while self.byte(self.at).is_some_and(|b| b != b'\n') {
                        self.at += 1;
                    }
                }
                (Some(b'/'), Some(b'*')) => {
                    self.at += 2;
                    while self.at < self.text.len()
                        && !(self.byte(self.at) == Some(b'*')
                            && self.byte(self.at + 1) == Some(b'/'))
                    {
                        self.at += 1;
                    }
                    self.at = (self.at + 2).min(self.text.len());
                }
                _ => return,
            }
        }
    }

    /// The next byte after white space and comments, without taking it.
    pub fn peek(&mut self) -> Option<u8> {
        self.skip();
        self.byte(self.at)
    }

    /// Takes `c` if it comes next.
    pub fn eat(&mut self, c: u8) -> bool {
        if self.peek() == Some(c) {
            self.at += 1;
            true
        } else {
            false
        }
    }

    /// Takes `c`, which must come next.
    pub fn expect(&mut self, c: u8) -> Result<(), FoamError> {
        if self.eat(c) {
            Ok(())
        } else {
            Err(self.error(&format!("'{}'", c as char)))
        }
    }

    /// Whether the text is all read.
    pub fn at_end(&mut self) -> bool {
        self.peek().is_none()
    }

    /// The next word: a run of bytes up to white space or punctuation (names, numbers,
    /// `List<scalar>`), or a quoted string's content.
    pub fn word(&mut self) -> Result<&'a str, FoamError> {
        self.skip();
        if self.byte(self.at) == Some(b'"') {
            let start = self.at + 1;
            let mut end = start;
            while self.byte(end).is_some_and(|b| b != b'"') {
                end += 1;
            }
            let text = self.text.get(start..end).unwrap_or_default();
            self.at = (end + 1).min(self.text.len());
            return std::str::from_utf8(text).map_err(|_| self.error("text"));
        }
        let start = self.at;
        while self.byte(self.at).is_some_and(|b| !is_delimiter(b)) {
            self.at += 1;
        }
        if self.at == start {
            return Err(self.error("a word"));
        }
        let text = self.text.get(start..self.at).unwrap_or_default();
        std::str::from_utf8(text).map_err(|_| self.error("text"))
    }

    /// The next number.
    pub fn scalar(&mut self) -> Result<f64, FoamError> {
        let at = self.at;
        let word = self.word()?;
        match word {
            "nan" | "-nan" | "NaN" => Ok(f64::NAN),
            _ => word.parse().map_err(|_| FoamError::Syntax {
                at,
                expected: format!("a number, not {word:?}"),
            }),
        }
    }

    /// The next whole number.
    pub fn label(&mut self) -> Result<usize, FoamError> {
        let at = self.at;
        let word = self.word()?;
        word.parse().map_err(|_| FoamError::Syntax {
            at,
            expected: format!("a whole number, not {word:?}"),
        })
    }

    /// A vector, `(x y z)`.
    pub fn vector(&mut self) -> Result<[f64; 3], FoamError> {
        self.expect(b'(')?;
        let v = [self.scalar()?, self.scalar()?, self.scalar()?];
        self.expect(b')')?;
        Ok(v)
    }

    /// Skips an entry's value: everything up to its `;` outside brackets, or a `{…}` block.
    pub fn skip_value(&mut self) -> Result<(), FoamError> {
        if self.eat(b'{') {
            return self.skip_block();
        }
        let mut depth = 0usize;
        loop {
            match self.peek() {
                None => return Err(self.error("';'")),
                Some(b';') => {
                    self.at += 1;
                    if depth == 0 {
                        return Ok(());
                    }
                }
                Some(b'(' | b'[' | b'{') => {
                    depth += 1;
                    self.at += 1;
                }
                Some(b')' | b']' | b'}') => {
                    depth = depth.checked_sub(1).ok_or_else(|| self.error("';'"))?;
                    self.at += 1;
                }
                // Words, numbers and quoted strings (which may hold `;`).
                Some(_) => {
                    self.word()?;
                }
            }
        }
    }

    /// Skips the rest of a `{…}` block whose `{` has been taken.
    fn skip_block(&mut self) -> Result<(), FoamError> {
        let mut depth = 1usize;
        while depth > 0 {
            match self.peek() {
                None => return Err(self.error("'}'")),
                Some(b'{') => {
                    depth += 1;
                    self.at += 1;
                }
                Some(b'}') => {
                    depth -= 1;
                    self.at += 1;
                }
                Some(b) if b != b'"' && is_delimiter(b) => self.at += 1,
                Some(_) => {
                    self.word()?;
                }
            }
        }
        Ok(())
    }

    /// A list of `item`s: `N(…)`, `N{item}` (N times the same), or `(…)` without a count.
    pub fn list<T: Clone>(
        &mut self,
        mut item: impl FnMut(&mut Self) -> Result<T, FoamError>,
    ) -> Result<Vec<T>, FoamError> {
        if self.peek() == Some(b'(') {
            self.at += 1;
            let mut items = Vec::new();
            while !self.eat(b')') {
                if self.at_end() {
                    return Err(self.error("')'"));
                }
                items.push(item(self)?);
            }
            return Ok(items);
        }
        let count = self.label()?;
        if self.eat(b'{') {
            let one = item(self)?;
            self.expect(b'}')?;
            return Ok(vec![one; count]);
        }
        self.expect(b'(')?;
        // A wrong count must not reserve more than the text could hold.
        let mut items = Vec::with_capacity(count.min(self.text.len() / 2 + 1));
        for _ in 0..count {
            items.push(item(self)?);
        }
        self.expect(b')')?;
        Ok(items)
    }
}

/// The `FoamFile` header's entries that matter here.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Header {
    pub class: String,
    pub object: String,
}

/// Reads the `FoamFile { … }` header, refusing binary files.
pub fn header(lex: &mut Lexer) -> Result<Header, FoamError> {
    if lex.word()? != "FoamFile" {
        return Err(lex.error("FoamFile"));
    }
    lex.expect(b'{')?;
    let mut header = Header::default();
    while !lex.eat(b'}') {
        let key = lex.word()?;
        match key {
            "format" => {
                if lex.word()? != "ascii" {
                    return Err(FoamError::Binary);
                }
                lex.expect(b';')?;
            }
            "class" => {
                header.class = lex.word()?.to_string();
                lex.expect(b';')?;
            }
            "object" => {
                header.object = lex.word()?.to_string();
                lex.expect(b';')?;
            }
            _ => lex.skip_value()?,
        }
    }
    Ok(header)
}

/// A field's values over cells or a patch's faces.
#[derive(Debug, Clone, PartialEq)]
pub enum Values<T> {
    /// The same value everywhere.
    Uniform(T),
    /// One value per cell or face.
    List(Vec<T>),
}

impl<T: Clone> Values<T> {
    /// The values, `n` of them.
    pub fn expand(&self, n: usize) -> Option<Vec<T>> {
        match self {
            Values::Uniform(v) => Some(vec![v.clone(); n]),
            Values::List(list) if list.len() == n => Some(list.clone()),
            Values::List(_) => None,
        }
    }
}

/// A patch's entry in a field's `boundaryField`.
#[derive(Debug, Clone, PartialEq)]
pub struct PatchField<T> {
    pub name: String,
    /// The boundary condition, `fixedValue`, `zeroGradient`…
    pub kind: String,
    /// The face values, when the file holds them (a zero-gradient patch's are its cells').
    pub value: Option<Values<T>>,
}

/// A field as a solver writes it: values in the cells, and on each patch.
#[derive(Debug, Clone, PartialEq)]
pub struct Field<T> {
    pub class: String,
    pub internal: Values<T>,
    pub boundary: Vec<PatchField<T>>,
}

/// `uniform v` or `nonuniform List<…> N(…)`.
fn values<T: Clone>(
    lex: &mut Lexer,
    item: &mut impl FnMut(&mut Lexer) -> Result<T, FoamError>,
) -> Result<Values<T>, FoamError> {
    match lex.word()? {
        "uniform" => Ok(Values::Uniform(item(lex)?)),
        "nonuniform" => {
            // The list's type, `List<scalar>`.
            lex.word()?;
            Ok(Values::List(lex.list(&mut *item)?))
        }
        _ => Err(lex.error("uniform or nonuniform")),
    }
}

/// Reads a field file, with `item` reading one value (a scalar or a vector).
pub fn field<T: Clone>(
    text: &[u8],
    mut item: impl FnMut(&mut Lexer) -> Result<T, FoamError>,
) -> Result<Field<T>, FoamError> {
    let mut lex = Lexer::new(text);
    let class = header(&mut lex)?.class;
    let mut internal = None;
    let mut boundary = Vec::new();
    while !lex.at_end() {
        match lex.word()? {
            "internalField" => {
                internal = Some(values(&mut lex, &mut item)?);
                lex.expect(b';')?;
            }
            "boundaryField" => {
                lex.expect(b'{')?;
                while !lex.eat(b'}') {
                    let name = lex.word()?.to_string();
                    lex.expect(b'{')?;
                    let mut kind = String::new();
                    let mut value = None;
                    while !lex.eat(b'}') {
                        match lex.word()? {
                            "type" => {
                                kind = lex.word()?.to_string();
                                lex.expect(b';')?;
                            }
                            "value" => {
                                value = Some(values(&mut lex, &mut item)?);
                                lex.expect(b';')?;
                            }
                            _ => lex.skip_value()?,
                        }
                    }
                    boundary.push(PatchField { name, kind, value });
                }
            }
            _ => lex.skip_value()?,
        }
    }
    let internal = internal.ok_or_else(|| lex.error("internalField"))?;
    Ok(Field {
        class,
        internal,
        boundary,
    })
}

/// A scalar field file.
pub fn scalar_field(text: &[u8]) -> Result<Field<f64>, FoamError> {
    field(text, |lex| lex.scalar())
}

/// A vector field file.
pub fn vector_field(text: &[u8]) -> Result<Field<[f64; 3]>, FoamError> {
    field(text, |lex| lex.vector())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_come_in_three_shapes() {
        let mut lex = Lexer::new(b"3(1 2 3) 2{4.5} (7 8) // the end");
        assert_eq!(lex.list(|l| l.label()).unwrap(), vec![1, 2, 3]);
        assert_eq!(lex.list(|l| l.scalar()).unwrap(), vec![4.5, 4.5]);
        assert_eq!(lex.list(|l| l.label()).unwrap(), vec![7, 8]);
        assert!(lex.at_end());
    }

    #[test]
    fn faces_are_lists_of_lists() {
        let mut lex = Lexer::new(b"2\n(\n4(0 1 2 3)\n3(4 5 6)\n)\n");
        let faces = lex.list(|l| l.list(|m| m.label())).unwrap();
        assert_eq!(faces, vec![vec![0, 1, 2, 3], vec![4, 5, 6]]);
    }

    #[test]
    fn headers_skip_what_they_do_not_need() {
        let text = br#"/* banner */
FoamFile
{
    version     2.0;
    format      ascii;
    arch        "LSB;label=32;scalar=64";
    class       labelList;
    note        "nPoints:8  nCells:1  nFaces:6  nInternalFaces:0";
    location    "constant/polyMesh";
    object      owner;
}
6(0 0 0 0 0 0)
"#;
        let mut lex = Lexer::new(text);
        let h = header(&mut lex).unwrap();
        assert_eq!(h.class, "labelList");
        assert_eq!(h.object, "owner");
        assert_eq!(lex.list(|l| l.label()).unwrap(), vec![0; 6]);

        let binary = b"FoamFile { version 2.0; format binary; class labelList; }";
        assert_eq!(header(&mut Lexer::new(binary)), Err(FoamError::Binary));
    }

    #[test]
    fn a_vector_field_with_every_kind_of_patch() {
        let text = br#"FoamFile { format ascii; class volVectorField; object U; }
dimensions      [0 1 -1 0 0 0 0];

internalField   nonuniform List<vector> 2((1 0 0) (2 0.5 -1e-3));

boundaryField
{
    inlet_0
    {
        type            flowRateInletVelocity;
        volumetricFlowRate constant 1e-06;
        value           nonuniform List<vector> 1((0 0 1));
    }
    wall_1
    {
        type            noSlip;
    }
    outlet_2
    {
        type            pressureInletOutletVelocity;
        value           uniform (0 0 0);
    }
    domain
    {
        type            noSlip;
        value           nonuniform List<vector> 0();
    }
}
"#;
        let f = vector_field(text).unwrap();
        assert_eq!(f.class, "volVectorField");
        assert_eq!(
            f.internal,
            Values::List(vec![[1.0, 0.0, 0.0], [2.0, 0.5, -1e-3]])
        );
        assert_eq!(f.boundary.len(), 4);
        assert_eq!(f.boundary[0].kind, "flowRateInletVelocity");
        assert_eq!(
            f.boundary[0].value,
            Some(Values::List(vec![[0.0, 0.0, 1.0]]))
        );
        assert_eq!(f.boundary[1].value, None);
        assert_eq!(
            f.boundary[2].value.as_ref().unwrap().expand(3),
            Some(vec![[0.0; 3]; 3])
        );
        assert_eq!(f.boundary[3].value, Some(Values::List(vec![])));
    }

    #[test]
    fn a_scalar_field_with_a_uniform_inside() {
        let text = b"FoamFile { format ascii; class volScalarField; object p; }
internalField uniform 0.25;
boundaryField { outlet { type fixedValue; value uniform 0; } in { type zeroGradient; } }";
        let f = scalar_field(text).unwrap();
        assert_eq!(f.internal.expand(3), Some(vec![0.25; 3]));
        assert_eq!(f.boundary[0].value, Some(Values::Uniform(0.0)));
    }

    #[test]
    fn errors_say_where() {
        let text = b"FoamFile { format ascii; class volScalarField; }\ninternalField uniform x;";
        let e = scalar_field(text).unwrap_err();
        assert!(matches!(e, FoamError::Syntax { .. }), "{e}");
        assert!(e.to_string().contains("a number"), "{e}");
        // A count larger than the list stops at the end of the text, without reserving it.
        let mut lex = Lexer::new(b"1000000000000(1 2)");
        assert!(lex.list(|l| l.label()).is_err());
    }
}
