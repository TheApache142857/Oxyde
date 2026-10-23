//! Values typed into fields, as SolidWorks' Modify box and Fusion 360's fields read them:
//! a number, arithmetic on numbers (`80/2`, `(120 - 2*6)/3`, `sqrt(2)*10`), and units
//! after numbers (`1in`, `3/8 in`, `2.5 cm`, `45deg`, `0.5rad`). Lengths come back in
//! millimetres and angles in degrees, whatever unit was typed; a decimal comma is read as
//! a point. The arithmetic is the equations' (`oxyde_model::Expr`), without names.

/// What a field holds, which decides the units it understands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Quantity {
    /// Millimetres; `mm`, `cm`, `m`, `um` (µm), `in` (or `"`), `ft` (or `'`) may follow a
    /// number.
    Length,
    /// Degrees; `deg` (or `°`) and `rad` may follow a number.
    Angle,
    /// A plain number, no unit.
    Number,
}

impl Quantity {
    /// What a dimension of the model holds.
    pub fn of(unit: oxyde_model::DimensionUnit) -> Self {
        match unit {
            oxyde_model::DimensionUnit::Length => Quantity::Length,
            oxyde_model::DimensionUnit::Angle => Quantity::Angle,
            oxyde_model::DimensionUnit::Count => Quantity::Number,
        }
    }
}

/// Millimetres per unit of length.
const LENGTH_UNITS: [(&str, f64); 10] = [
    ("mm", 1.0),
    ("cm", 10.0),
    ("m", 1000.0),
    ("um", 0.001),
    ("µm", 0.001),
    ("in", 25.4),
    ("inch", 25.4),
    ("\"", 25.4),
    ("ft", 304.8),
    ("'", 304.8),
];

/// Degrees per unit of angle.
const ANGLE_UNITS: [(&str, f64); 4] = [
    ("deg", 1.0),
    ("°", 1.0),
    ("rad", 180.0 / std::f64::consts::PI),
    ("grad", 0.9),
];

/// The value of `text` in the field's unit, or `None` when it does not read as one (a
/// unit of the wrong kind, a name, a result that is not a finite number).
pub fn parse(text: &str, quantity: Quantity) -> Option<f64> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let expression = with_units(&decimal_points(text), quantity)?;
    let expr: oxyde_model::Expr = oxyde_model::Expr::parse(&expression, &|_| None).ok()?;
    expr.evaluate(&mut |_, _| Err(String::new()))
        .ok()
        .filter(|v| v.is_finite())
}

/// A comma between two digits is a decimal comma ("2,5"); other commas separate the
/// arguments of `min` and `max`.
fn decimal_points(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    chars
        .iter()
        .enumerate()
        .map(|(i, &c)| {
            let digit = |k: Option<usize>| {
                k.and_then(|k| chars.get(k))
                    .is_some_and(|c| c.is_ascii_digit())
            };
            if c == ',' && digit(i.checked_sub(1)) && digit(Some(i + 1)) {
                '.'
            } else {
                c
            }
        })
        .collect()
}

/// `text` with each unit after a number turned into a factor: "1in + 3" becomes
/// "1*25.4 + 3". Words that are not units of this kind of value are left in, so the
/// expression fails to parse.
fn with_units(text: &str, quantity: Quantity) -> Option<String> {
    let units: &[(&str, f64)] = match quantity {
        Quantity::Length => &LENGTH_UNITS,
        Quantity::Angle => &ANGLE_UNITS,
        Quantity::Number => &[],
    };
    let chars: Vec<char> = text.chars().collect();
    let at = |k: usize| chars.get(k).copied();
    let before = |k: usize| k.checked_sub(1).and_then(at);
    let mut out = String::with_capacity(text.len() + 8);
    let mut i = 0;
    while let Some(c) = at(i) {
        let starts_number =
            c.is_ascii_digit() || (c == '.' && at(i + 1).is_some_and(|d| d.is_ascii_digit()));
        // A number inside a word (a function name such as "log10") is left alone.
        let in_word = before(i).is_some_and(char::is_alphanumeric);
        if !starts_number || in_word {
            out.push(c);
            i += 1;
            continue;
        }
        // The number: digits, a point, and an exponent with its sign.
        while let Some(d) = at(i) {
            let exponent = matches!(d, 'e' | 'E')
                && at(i + 1).is_some_and(|n| n.is_ascii_digit() || n == '+' || n == '-');
            let sign = matches!(d, '+' | '-') && before(i).is_some_and(|p| matches!(p, 'e' | 'E'));
            if d.is_ascii_digit() || d == '.' || exponent || sign {
                out.push(d);
                i += 1;
            } else {
                break;
            }
        }
        // A unit after it, spaces allowed between.
        let mut j = i;
        while at(j) == Some(' ') {
            j += 1;
        }
        let mut k = j;
        match at(j) {
            Some(c) if c.is_alphabetic() => {
                while at(k).is_some_and(char::is_alphabetic) {
                    k += 1;
                }
            }
            Some('"' | '\'' | '°') => k = j + 1,
            _ => {}
        }
        if k > j {
            let word: String = chars.get(j..k)?.iter().collect::<String>().to_lowercase();
            if word == "pi" {
                // "2pi" is a product, as on a calculator.
                out.push('*');
            } else if let Some((_, factor)) = units.iter().find(|(name, _)| *name == word) {
                out.push_str(&format!("*{factor}"));
                i = k;
            }
            // Anything else (a unit of another kind of value) stays as it is, for the
            // parser to refuse.
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(text: &str, quantity: Quantity, want: f64) {
        let got = parse(text, quantity).unwrap_or_else(|| panic!("{text} did not read"));
        assert!((got - want).abs() < 1e-9, "{text}: {got}, wanted {want}");
    }

    #[test]
    fn numbers_and_arithmetic() {
        close("12.5", Quantity::Length, 12.5);
        close(" 2,5 ", Quantity::Length, 2.5);
        close("80/2", Quantity::Length, 40.0);
        close("(120 - 2*6)/3", Quantity::Length, 36.0);
        close("-3", Quantity::Length, -3.0);
        close("1e2", Quantity::Length, 100.0);
        close("2.5e-1", Quantity::Length, 0.25);
        close("max(3, 4,5)", Quantity::Length, 4.5);
        close("sqrt(2)^2", Quantity::Number, 2.0);
        close("=10+5", Quantity::Length, 15.0);
    }

    #[test]
    fn units_are_converted() {
        close("1in", Quantity::Length, 25.4);
        close("3/8 in", Quantity::Length, 3.0 / 8.0 * 25.4);
        close("1\"", Quantity::Length, 25.4);
        close("2.5 cm + 1mm", Quantity::Length, 26.0);
        close("0.1m", Quantity::Length, 100.0);
        close("1ft", Quantity::Length, 304.8);
        close("90deg", Quantity::Angle, 90.0);
        close("45°", Quantity::Angle, 45.0);
        close("pi/2 rad", Quantity::Angle, 90.0);
        close("1 rad", Quantity::Angle, 57.29577951308232);
        close("2pi", Quantity::Number, 2.0 * std::f64::consts::PI);
    }

    #[test]
    fn what_is_not_a_value_is_refused() {
        for (text, quantity) in [
            ("", Quantity::Length),
            ("abc", Quantity::Length),
            ("10 deg", Quantity::Length),
            ("10 mm", Quantity::Angle),
            ("3 mm", Quantity::Number),
            ("\"D1@Sketch1\"", Quantity::Length),
            ("1/0", Quantity::Length),
            ("(2", Quantity::Length),
            ("inf", Quantity::Length),
            ("NaN", Quantity::Length),
        ] {
            assert_eq!(parse(text, quantity), None, "{text}");
        }
    }
}
