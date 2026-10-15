//! Bill of materials: the parts of an assembly with their quantities, at the top level, as
//! a flat list of parts through every sub-assembly, or indented.

use crate::{Assembly, Component};

/// How sub-assemblies show in the bill of materials.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BomMode {
    /// The assembly's own components; a sub-assembly is one line.
    TopLevel,
    /// Only parts, through every level, with total quantities.
    PartsOnly,
    /// Each sub-assembly followed by its own components (items 1, 1.1, 1.2…), quantities
    /// per sub-assembly.
    Indented,
}

/// One row of a bill of materials.
#[derive(Debug, Clone, PartialEq)]
pub struct BomRow {
    /// "1", "2"… or "2.1", "2.2"… when indented.
    pub item: String,
    /// 0 at the top; 1 inside a sub-assembly, and so on.
    pub level: usize,
    /// The file, as the caller names it (see [`rows`]).
    pub key: String,
    pub document_id: String,
    pub quantity: usize,
    pub is_assembly: bool,
}

/// What the caller knows about a component's file: a key naming it (for example its
/// absolute path), and its content when it is an assembly.
pub type Open<'a> = dyn Fn(&Component, &str) -> (String, Option<&'a Assembly>) + 'a;

/// The bill of materials of `root`. `open(component, parent_key)` names a component's file
/// given the key of the assembly holding it (`root_key` for the root), and gives the
/// sub-assembly when it is one.
pub fn rows<'a>(root: &'a Assembly, root_key: &str, mode: BomMode, open: &Open<'a>) -> Vec<BomRow> {
    let mut out = Vec::new();
    match mode {
        BomMode::TopLevel => level(root, root_key, "", 0, false, open, &mut out),
        BomMode::Indented => level(root, root_key, "", 0, true, open, &mut out),
        BomMode::PartsOnly => {
            let mut parts: Vec<BomRow> = Vec::new();
            flatten(root, root_key, 1, 0, open, &mut parts);
            for (i, row) in parts.iter_mut().enumerate() {
                row.item = (i + 1).to_string();
            }
            out = parts;
        }
    }
    out
}

/// Deeper than this, an assembly is surely including itself.
const MAX_DEPTH: usize = 32;

/// One assembly's components grouped by file.
fn grouped<'a>(
    assembly: &'a Assembly,
    key: &str,
    open: &Open<'a>,
) -> Vec<(String, String, usize, Option<&'a Assembly>)> {
    let mut groups: Vec<(String, String, usize, Option<&'a Assembly>)> = Vec::new();
    for c in &assembly.components {
        let (k, sub) = open(c, key);
        let same = |g: &(String, String, usize, Option<&Assembly>)| {
            g.0 == k || (!c.document_id.is_empty() && g.1 == c.document_id)
        };
        match groups.iter_mut().find(|g| same(g)) {
            Some(g) => g.2 += 1,
            None => groups.push((k, c.document_id.clone(), 1, sub)),
        }
    }
    groups
}

fn level<'a>(
    assembly: &'a Assembly,
    key: &str,
    prefix: &str,
    depth: usize,
    indented: bool,
    open: &Open<'a>,
    out: &mut Vec<BomRow>,
) {
    for (i, (k, document_id, quantity, sub)) in grouped(assembly, key, open).into_iter().enumerate()
    {
        let item = format!("{prefix}{}", i + 1);
        out.push(BomRow {
            item: item.clone(),
            level: depth,
            key: k.clone(),
            document_id,
            quantity,
            is_assembly: sub.is_some(),
        });
        if let (true, Some(sub)) = (indented && depth < MAX_DEPTH, sub) {
            level(sub, &k, &format!("{item}."), depth + 1, true, open, out);
        }
    }
}

fn flatten<'a>(
    assembly: &'a Assembly,
    key: &str,
    times: usize,
    depth: usize,
    open: &Open<'a>,
    out: &mut Vec<BomRow>,
) {
    for (k, document_id, quantity, sub) in grouped(assembly, key, open) {
        match sub {
            Some(sub) if depth < MAX_DEPTH => {
                flatten(sub, &k, times * quantity, depth + 1, open, out)
            }
            Some(_) => {}
            None => {
                let same = |r: &BomRow| {
                    r.key == k || (!document_id.is_empty() && r.document_id == document_id)
                };
                match out.iter_mut().find(|r| same(r)) {
                    Some(row) => row.quantity += times * quantity,
                    None => out.push(BomRow {
                        item: String::new(),
                        level: 0,
                        key: k,
                        document_id,
                        quantity: times * quantity,
                        is_assembly: false,
                    }),
                }
            }
        }
    }
}

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
