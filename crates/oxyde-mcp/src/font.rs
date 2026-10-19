//! Text as outlines: a small TrueType reader (the `glyf` outlines of a `.ttf` file, its
//! character map, advances and kerning) and the layout of a line of text into closed
//! polylines, which the sketch builder turns into lines. No dependency: the tables read are
//! few and their layout is fixed (Apple's and Microsoft's TrueType specifications).
//!
//! Glyph curves are quadratic Béziers; they are flattened to within `tolerance` mm, since
//! sketches hold lines, arcs and circles only.

use std::collections::BTreeMap;

use crate::args::Result;

/// A TrueType font file read into memory.
pub(crate) struct Font {
    data: Vec<u8>,
    tables: BTreeMap<[u8; 4], (usize, usize)>,
    units_per_em: f64,
    long_offsets: bool,
    glyphs: usize,
    metrics: usize,
    /// Character to glyph.
    cmap: BTreeMap<u32, u16>,
    /// Kerning between two glyphs, font units.
    kerning: BTreeMap<(u16, u16), i16>,
}

fn u8_at(d: &[u8], o: usize) -> Result<u8> {
    d.get(o)
        .copied()
        .ok_or_else(|| "the font file is cut short".to_owned())
}

fn u16_at(d: &[u8], o: usize) -> Result<u16> {
    Ok(u16::from(u8_at(d, o)?) << 8 | u16::from(u8_at(d, o + 1)?))
}

fn i16_at(d: &[u8], o: usize) -> Result<i16> {
    Ok(u16_at(d, o)? as i16)
}

fn u32_at(d: &[u8], o: usize) -> Result<u32> {
    Ok(u32::from(u16_at(d, o)?) << 16 | u32::from(u16_at(d, o + 2)?))
}

/// A glyph outline: closed contours of points in font units, `true` where on the curve.
type Contours = Vec<Vec<(f64, f64, bool)>>;

impl Font {
    pub fn load(path: &std::path::Path) -> Result<Self> {
        let data = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        Self::parse(data)
    }

    pub fn parse(data: Vec<u8>) -> Result<Self> {
        let d = &data;
        let tag = u32_at(d, 0)?;
        if tag == 0x7474_6366 {
            return Err("font collections (.ttc) are not read: use a .ttf file".to_owned());
        }
        if tag != 0x0001_0000 && tag != 0x7472_7565 {
            return Err("not a TrueType font (CFF-based .otf fonts are not read)".to_owned());
        }
        let count = usize::from(u16_at(d, 4)?);
        let mut tables = BTreeMap::new();
        for k in 0..count {
            let entry = 12 + 16 * k;
            let name = [
                u8_at(d, entry)?,
                u8_at(d, entry + 1)?,
                u8_at(d, entry + 2)?,
                u8_at(d, entry + 3)?,
            ];
            let offset = u32_at(d, entry + 8)? as usize;
            let length = u32_at(d, entry + 12)? as usize;
            tables.insert(name, (offset, length));
        }
        let table = |name: &[u8; 4]| {
            tables
                .get(name)
                .copied()
                .ok_or_else(|| format!("the font has no {} table", String::from_utf8_lossy(name)))
        };
        let (head, _) = table(b"head")?;
        let (maxp, _) = table(b"maxp")?;
        let (hhea, _) = table(b"hhea")?;
        table(b"glyf")?;
        table(b"loca")?;
        table(b"hmtx")?;
        let units_per_em = f64::from(u16_at(d, head + 18)?).max(1.0);
        let long_offsets = i16_at(d, head + 50)? != 0;
        let glyphs = usize::from(u16_at(d, maxp + 4)?);
        let metrics = usize::from(u16_at(d, hhea + 34)?);
        let (cmap_at, _) = table(b"cmap")?;
        let cmap = read_cmap(d, cmap_at)?;
        let kerning = match tables.get(b"kern") {
            Some(&(at, _)) => read_kern(d, at).unwrap_or_default(),
            None => BTreeMap::new(),
        };
        Ok(Self {
            data,
            tables,
            units_per_em,
            long_offsets,
            glyphs,
            metrics,
            cmap,
            kerning,
        })
    }

    fn table(&self, name: &[u8; 4]) -> usize {
        self.tables.get(name).map_or(0, |t| t.0)
    }

    fn glyph_range(&self, glyph: u16) -> Result<(usize, usize)> {
        let g = usize::from(glyph);
        if g >= self.glyphs {
            return Err(format!("glyph {g} is not in the font"));
        }
        let loca = self.table(b"loca");
        let (start, end) = if self.long_offsets {
            (
                u32_at(&self.data, loca + 4 * g)? as usize,
                u32_at(&self.data, loca + 4 * g + 4)? as usize,
            )
        } else {
            (
                usize::from(u16_at(&self.data, loca + 2 * g)?) * 2,
                usize::from(u16_at(&self.data, loca + 2 * g + 2)?) * 2,
            )
        };
        let glyf = self.table(b"glyf");
        Ok((glyf + start, glyf + end))
    }

    fn advance(&self, glyph: u16) -> Result<f64> {
        let hmtx = self.table(b"hmtx");
        let g = usize::from(glyph).min(self.metrics.saturating_sub(1));
        Ok(f64::from(u16_at(&self.data, hmtx + 4 * g)?))
    }

    /// The contours of a glyph, composite glyphs flattened, in font units.
    fn outline(&self, glyph: u16, depth: usize) -> Result<Contours> {
        if depth > 8 {
            return Err("the font's composite glyphs nest too deep".to_owned());
        }
        let (start, end) = self.glyph_range(glyph)?;
        if end <= start {
            return Ok(Vec::new());
        }
        let d = &self.data;
        let contours = i16_at(d, start)?;
        if contours >= 0 {
            return simple_glyph(d, start, contours as usize);
        }
        // A composite: other glyphs placed with an offset and maybe a scale.
        let mut out = Vec::new();
        let mut at = start + 10;
        loop {
            let flags = u16_at(d, at)?;
            let index = u16_at(d, at + 2)?;
            at += 4;
            let (dx, dy) = if flags & 0x0001 != 0 {
                let v = (f64::from(i16_at(d, at)?), f64::from(i16_at(d, at + 2)?));
                at += 4;
                v
            } else {
                let v = (
                    f64::from(u8_at(d, at)? as i8),
                    f64::from(u8_at(d, at + 1)? as i8),
                );
                at += 2;
                v
            };
            let f2dot14 = |v: i16| f64::from(v) / 16384.0;
            let (mut a, mut b, mut c, mut e) = (1.0, 0.0, 0.0, 1.0);
            if flags & 0x0008 != 0 {
                a = f2dot14(i16_at(d, at)?);
                e = a;
                at += 2;
            } else if flags & 0x0040 != 0 {
                a = f2dot14(i16_at(d, at)?);
                e = f2dot14(i16_at(d, at + 2)?);
                at += 4;
            } else if flags & 0x0080 != 0 {
                a = f2dot14(i16_at(d, at)?);
                b = f2dot14(i16_at(d, at + 2)?);
                c = f2dot14(i16_at(d, at + 4)?);
                e = f2dot14(i16_at(d, at + 6)?);
                at += 8;
            }
            for contour in self.outline(index, depth + 1)? {
                out.push(
                    contour
                        .into_iter()
                        .map(|(x, y, on)| (a * x + c * y + dx, b * x + e * y + dy, on))
                        .collect(),
                );
            }
            if flags & 0x0020 == 0 {
                break;
            }
        }
        Ok(out)
    }

    /// The glyph of a character, the font's "missing" glyph (0) when it has none.
    fn glyph(&self, ch: char) -> u16 {
        self.cmap.get(&u32::from(ch)).copied().unwrap_or(0)
    }

    /// `text` laid out on a baseline from the origin, `size` mm to the em, `spacing` mm
    /// added between letters, as closed polylines flattened to within `tolerance` mm.
    pub fn layout(
        &self,
        text: &str,
        size: f64,
        spacing: f64,
        tolerance: f64,
    ) -> Result<Vec<Vec<(f64, f64)>>> {
        let scale = size / self.units_per_em;
        let tolerance_units = (tolerance / scale).max(1e-3);
        let mut pen = 0.0;
        let mut previous: Option<u16> = None;
        let mut out = Vec::new();
        for ch in text.chars() {
            let glyph = self.glyph(ch);
            if let Some(p) = previous {
                pen += f64::from(self.kerning.get(&(p, glyph)).copied().unwrap_or(0));
            }
            for contour in self.outline(glyph, 0)? {
                let points = flatten(&contour, tolerance_units);
                if points.len() >= 3 {
                    out.push(
                        points
                            .into_iter()
                            .map(|(x, y)| ((x + pen) * scale, y * scale))
                            .collect(),
                    );
                }
            }
            pen += self.advance(glyph)? + spacing / scale;
            previous = Some(glyph);
        }
        Ok(out)
    }
}

fn simple_glyph(d: &[u8], start: usize, contours: usize) -> Result<Contours> {
    let mut ends = Vec::with_capacity(contours);
    for k in 0..contours {
        ends.push(usize::from(u16_at(d, start + 10 + 2 * k)?));
    }
    let points = ends.last().map_or(0, |e| e + 1);
    let instructions = usize::from(u16_at(d, start + 10 + 2 * contours)?);
    let mut at = start + 12 + 2 * contours + instructions;
    let mut flags = Vec::with_capacity(points);
    while flags.len() < points {
        let f = u8_at(d, at)?;
        at += 1;
        flags.push(f);
        if f & 0x08 != 0 {
            let repeat = u8_at(d, at)?;
            at += 1;
            for _ in 0..repeat {
                flags.push(f);
            }
        }
    }
    flags.truncate(points);
    let mut read = |short: u8, same: u8| -> Result<Vec<f64>> {
        let mut values = Vec::with_capacity(points);
        let mut value = 0i32;
        for f in &flags {
            if f & short != 0 {
                let delta = i32::from(u8_at(d, at)?);
                at += 1;
                value += if f & same != 0 { delta } else { -delta };
            } else if f & same == 0 {
                value += i32::from(i16_at(d, at)?);
                at += 2;
            }
            values.push(f64::from(value));
        }
        Ok(values)
    };
    let xs = read(0x02, 0x10)?;
    let ys = read(0x04, 0x20)?;
    let mut out = Vec::with_capacity(contours);
    let mut first = 0;
    for end in ends {
        let contour: Vec<(f64, f64, bool)> = (first..=end)
            .filter_map(|k| Some((*xs.get(k)?, *ys.get(k)?, flags.get(k)? & 0x01 != 0)))
            .collect();
        out.push(contour);
        first = end + 1;
    }
    Ok(out)
}

/// A TrueType contour (on-curve points, and off-curve control points of quadratic curves,
/// two of which in a row imply an on-curve point between them) as a closed polyline.
fn flatten(contour: &[(f64, f64, bool)], tolerance: f64) -> Vec<(f64, f64)> {
    let n = contour.len();
    if n == 0 {
        return Vec::new();
    }
    let at = |k: usize| contour.get(k % n).copied().unwrap_or((0.0, 0.0, true));
    let mid =
        |a: (f64, f64, bool), b: (f64, f64, bool)| ((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0, true);
    // Start on an on-curve point (or the middle of the first two control points).
    let start = (0..n).find(|&k| at(k).2);
    let (origin, first) = match start {
        Some(k) => (at(k), k),
        None => (mid(at(0), at(1)), 0),
    };
    let mut out = vec![(origin.0, origin.1)];
    let mut current = origin;
    let mut control: Option<(f64, f64, bool)> = None;
    let push_curve = |from: (f64, f64, bool),
                      c: (f64, f64, bool),
                      to: (f64, f64, bool),
                      out: &mut Vec<(f64, f64)>| {
        // Segments enough for the curve's sag to stay within the tolerance.
        let sag = ((from.0 - 2.0 * c.0 + to.0).hypot(from.1 - 2.0 * c.1 + to.1)) / 4.0;
        let steps = ((sag / tolerance).sqrt().ceil() as usize).clamp(1, 32);
        for s in 1..=steps {
            let t = s as f64 / steps as f64;
            let u = 1.0 - t;
            out.push((
                u * u * from.0 + 2.0 * u * t * c.0 + t * t * to.0,
                u * u * from.1 + 2.0 * u * t * c.1 + t * t * to.1,
            ));
        }
    };
    for step in 1..=n {
        let p = at(first + step);
        match (p.2, control) {
            (true, None) => {
                out.push((p.0, p.1));
                current = p;
            }
            (true, Some(c)) => {
                push_curve(current, c, p, &mut out);
                current = p;
                control = None;
            }
            (false, None) => control = Some(p),
            (false, Some(c)) => {
                let m = mid(c, p);
                push_curve(current, c, m, &mut out);
                current = m;
                control = Some(p);
            }
        }
    }
    if let Some(c) = control {
        push_curve(current, c, origin, &mut out);
    }
    // The last point is the first again; points closer than a hundredth of the tolerance
    // are merged.
    let mut clean: Vec<(f64, f64)> = Vec::with_capacity(out.len());
    for p in out {
        if clean
            .last()
            .is_none_or(|q| (p.0 - q.0).hypot(p.1 - q.1) > tolerance * 0.01)
        {
            clean.push(p);
        }
    }
    if let (Some(a), Some(b)) = (clean.first().copied(), clean.last().copied())
        && clean.len() > 1
        && (a.0 - b.0).hypot(a.1 - b.1) <= tolerance * 0.01
    {
        clean.pop();
    }
    clean
}

fn read_cmap(d: &[u8], at: usize) -> Result<BTreeMap<u32, u16>> {
    let count = usize::from(u16_at(d, at + 2)?);
    // The best subtable: Unicode full repertoire (format 12), else the BMP (format 4).
    let mut best: Option<(u16, usize)> = None;
    for k in 0..count {
        let platform = u16_at(d, at + 4 + 8 * k)?;
        let encoding = u16_at(d, at + 6 + 8 * k)?;
        let offset = at + u32_at(d, at + 8 + 8 * k)? as usize;
        let unicode = platform == 0 || (platform == 3 && (encoding == 1 || encoding == 10));
        if !unicode {
            continue;
        }
        let format = u16_at(d, offset)?;
        if (format == 4 || format == 12) && best.is_none_or(|(f, _)| format > f) {
            best = Some((format, offset));
        }
    }
    let mut map = BTreeMap::new();
    match best {
        Some((12, o)) => {
            let groups = u32_at(d, o + 12)? as usize;
            for g in 0..groups.min(1 << 16) {
                let first = u32_at(d, o + 16 + 12 * g)?;
                let last = u32_at(d, o + 20 + 12 * g)?;
                let glyph = u32_at(d, o + 24 + 12 * g)?;
                for c in first..=last.min(first.saturating_add(0xFFFF)) {
                    map.insert(c, (glyph + (c - first)) as u16);
                }
            }
        }
        Some((_, o)) => {
            let segments = usize::from(u16_at(d, o + 6)?) / 2;
            let ends = o + 14;
            let starts = ends + 2 * segments + 2;
            let deltas = starts + 2 * segments;
            let ranges = deltas + 2 * segments;
            for s in 0..segments {
                let end = u16_at(d, ends + 2 * s)?;
                let start = u16_at(d, starts + 2 * s)?;
                let delta = u16_at(d, deltas + 2 * s)?;
                let range_at = ranges + 2 * s;
                let range = u16_at(d, range_at)?;
                if start > end {
                    continue;
                }
                for c in start..=end {
                    let glyph = if range == 0 {
                        c.wrapping_add(delta)
                    } else {
                        let at = range_at + usize::from(range) + 2 * usize::from(c - start);
                        match u16_at(d, at)? {
                            0 => 0,
                            g => g.wrapping_add(delta),
                        }
                    };
                    map.insert(u32::from(c), glyph);
                    if c == u16::MAX {
                        break;
                    }
                }
            }
        }
        None => return Err("the font has no Unicode character map".to_owned()),
    }
    Ok(map)
}

/// Pair kerning from the old `kern` table (format 0), which many TrueType fonts carry.
fn read_kern(d: &[u8], at: usize) -> Result<BTreeMap<(u16, u16), i16>> {
    let mut out = BTreeMap::new();
    if u16_at(d, at)? != 0 {
        return Ok(out);
    }
    let tables = u16_at(d, at + 2)?;
    let mut sub = at + 4;
    for _ in 0..tables {
        let length = usize::from(u16_at(d, sub + 2)?);
        let coverage = u16_at(d, sub + 4)?;
        // Format 0, horizontal kerning.
        if coverage >> 8 == 0 && coverage & 0x1 != 0 {
            let pairs = usize::from(u16_at(d, sub + 6)?);
            for p in 0..pairs {
                let e = sub + 14 + 6 * p;
                out.insert((u16_at(d, e)?, u16_at(d, e + 2)?), i16_at(d, e + 4)?);
            }
        }
        sub += length.max(6);
    }
    Ok(out)
}

/// A font file from a path, or by a family name among the system's fonts.
pub(crate) fn find(name: &str) -> Result<std::path::PathBuf> {
    let path = std::path::PathBuf::from(name);
    if path.is_file() {
        return Ok(path);
    }
    let folders: Vec<std::path::PathBuf> = if cfg!(windows) {
        vec![
            std::path::PathBuf::from(
                std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".to_owned()),
            )
            .join("Fonts"),
        ]
    } else {
        vec![
            "/usr/share/fonts".into(),
            "/usr/local/share/fonts".into(),
            "/Library/Fonts".into(),
            "/System/Library/Fonts".into(),
        ]
    };
    let wanted = name.to_ascii_lowercase().replace(' ', "");
    for folder in folders {
        let Ok(entries) = std::fs::read_dir(&folder) else {
            continue;
        };
        for entry in entries.flatten() {
            let file = entry.file_name().to_string_lossy().to_ascii_lowercase();
            if file.ends_with(".ttf") && file.trim_end_matches(".ttf").replace(' ', "") == wanted {
                return Ok(entry.path());
            }
        }
    }
    Err(format!(
        "no font file \"{name}\" (give a .ttf path or a file name like \"arial\")"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quadratic_contours_are_flattened_closed() {
        // A square with its corners replaced by control points: a rounded diamond.
        let contour = vec![
            (0.0, -10.0, true),
            (10.0, -10.0, false),
            (10.0, 0.0, true),
            (10.0, 10.0, false),
            (0.0, 10.0, true),
            (-10.0, 10.0, false),
            (-10.0, 0.0, true),
            (-10.0, -10.0, false),
        ];
        let points = flatten(&contour, 0.01);
        assert!(points.len() > 16);
        // Every point between the inner diamond and the square.
        for (x, y) in &points {
            let r = x.abs() + y.abs();
            assert!(
                r >= 10.0 - 1e-9 && x.abs() <= 10.0 && y.abs() <= 10.0,
                "{x} {y}"
            );
        }
        // No closing duplicate.
        assert_ne!(points.first(), points.last());
    }

    #[test]
    fn a_system_font_lays_text_out() {
        let Ok(path) = find("arial") else {
            return; // No Arial on this machine: nothing to check.
        };
        let font = Font::load(&path).unwrap();
        let outlines = font.layout("Hi", 10.0, 0.0, 0.01).unwrap();
        // H is one contour, i two (stem and dot).
        assert_eq!(outlines.len(), 3);
        let width = outlines
            .iter()
            .flatten()
            .map(|p| p.0)
            .fold(f64::NEG_INFINITY, f64::max);
        assert!(width > 8.0 && width < 12.0, "{width}");
    }
}
