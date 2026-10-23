//! Writing OpenFOAM's text files: the header every file starts with, and numbers.

use std::fmt::Write as _;

/// The `FoamFile` header of a file of `class` holding `object`.
pub(crate) fn header(class: &str, object: &str) -> String {
    let mut s = String::new();
    s.push_str(
        "/*--------------------------------*- C++ -*----------------------------------*\\\n",
    );
    s.push_str("  Written by Oxyde for OpenFOAM (openfoam.com). Edits are lost on the next run.\n");
    s.push_str(
        "\\*---------------------------------------------------------------------------*/\n",
    );
    s.push_str("FoamFile\n{\n");
    s.push_str("    version     2.0;\n");
    s.push_str("    format      ascii;\n");
    let _ = writeln!(s, "    class       {class};");
    let _ = writeln!(s, "    object      {object};");
    s.push_str("}\n\n");
    s
}

/// A number as OpenFOAM reads it: plain where that is short, with an exponent otherwise.
/// Non-finite numbers never reach here (studies are checked first); they are written as 0.
pub(crate) fn num(x: f64) -> String {
    if !x.is_finite() {
        return "0".to_string();
    }
    let a = x.abs();
    if a == 0.0 {
        "0".to_string()
    } else if (1e-4..1e7).contains(&a) {
        format!("{x}")
    } else {
        format!("{x:e}")
    }
}

/// A vector, `(x y z)`.
pub(crate) fn vector(v: [f64; 3]) -> String {
    format!("({} {} {})", num(v[0]), num(v[1]), num(v[2]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_read_back_exactly() {
        for x in [
            0.0,
            1.0,
            -2.5,
            1.5e-5,
            998.2,
            1.0e-12,
            3.0e9,
            0.1 + 0.2,
            -7.25e-4,
        ] {
            let text = num(x);
            assert_eq!(text.parse::<f64>().unwrap(), x, "{text}");
        }
        assert_eq!(num(1.5e-5), "1.5e-5");
        assert_eq!(num(0.001), "0.001");
        assert_eq!(num(f64::NAN), "0");
        assert_eq!(vector([1.0, -0.5, 0.0]), "(1 -0.5 0)");
    }

    #[test]
    fn the_header_names_class_and_object() {
        let h = header("volVectorField", "U");
        assert!(h.contains("    class       volVectorField;\n"));
        assert!(h.contains("    object      U;\n"));
        assert!(h.contains("format      ascii;"));
    }
}
