//! Bill of materials: the parts of an assembly with their quantities.

use crate::Assembly;

/// One line of the bill of materials: a part file and how many times it is used.
#[derive(Debug, Clone, PartialEq)]
pub struct BomLine {
    /// 1, 2, 3… in the order the parts first appear in the assembly.
    pub item: usize,
    pub path: String,
    pub document_id: String,
    pub quantity: usize,
    /// Instance names, `Bracket<1>`, `Bracket<2>`…
    pub instances: Vec<String>,
}

/// The parts of `assembly` with their quantities. Hidden components count: hiding is a
/// display choice, the part is still in the product.
pub fn lines(assembly: &Assembly) -> Vec<BomLine> {
    let mut out: Vec<BomLine> = Vec::new();
    for c in &assembly.components {
        // The same document under two paths is still the same part.
        let same = |l: &BomLine| {
            l.path == c.path || (!c.document_id.is_empty() && l.document_id == c.document_id)
        };
        match out.iter_mut().find(|l| same(l)) {
            Some(line) => {
                line.quantity += 1;
                line.instances.push(c.name.clone());
            }
            None => {
                let item = out.len() + 1;
                out.push(BomLine {
                    item,
                    path: c.path.clone(),
                    document_id: c.document_id.clone(),
                    quantity: 1,
                    instances: vec![c.name.clone()],
                });
            }
        }
    }
    out
}

/// Rows as CSV (RFC 4180): fields with commas, quotes or line breaks are quoted.
pub fn to_csv(rows: &[Vec<String>]) -> String {
    let field = |f: &String| {
        if f.contains([',', '"', '\n', '\r']) {
            format!("\"{}\"", f.replace('"', "\"\""))
        } else {
            f.clone()
        }
    };
    let mut out = String::new();
    for row in rows {
        out.push_str(&row.iter().map(field).collect::<Vec<_>>().join(","));
        out.push_str("\r\n");
    }
    out
}
