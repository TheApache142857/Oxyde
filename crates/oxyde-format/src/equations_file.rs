//! Linked equations files, SolidWorks' "Link to external file": a plain text file of global
//! variables that several parts share, one per line:
//!
//! ```text
//! ' Outer dimensions of the case
//! "Width" = 233
//! "Depth" = 300
//! "Height" = "Width" + 57
//! ```
//!
//! Lines starting with `'` are comments, as in SolidWorks; blank lines are ignored. A part
//! or an assembly linked to the file (its `equations_file` in `model.json` or
//! `assembly.json`) keeps a copy of the variables and refreshes them from the file when it
//! is opened, so a document whose file has gone still opens with the values it last read.

use std::path::{Path, PathBuf};

use oxyde_assembly::{Assembly, TopLevelOnly};
use oxyde_model::{LinkedValue, PartDefinition};

use crate::FormatError;

/// Reads the variables of an equations file's text. A line that is not `"Name" = expression`
/// is refused with its number.
pub fn parse_equations_file(text: &str) -> Result<Vec<LinkedValue>, FormatError> {
    let mut values: Vec<LinkedValue> = Vec::new();
    for (number, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('\'') {
            continue;
        }
        let invalid = || {
            FormatError::InvalidModel(oxyde_model::ModelError::Expression(format!(
                "equations file, line {}: expected \"Name\" = expression",
                number + 1
            )))
        };
        let rest = line.strip_prefix('"').ok_or_else(invalid)?;
        let (name, rest) = rest.split_once('"').ok_or_else(invalid)?;
        let expression = rest
            .trim_start()
            .strip_prefix('=')
            .ok_or_else(invalid)?
            .trim();
        if !oxyde_sketch::is_valid_name(name) || expression.is_empty() {
            return Err(invalid());
        }
        // A name given twice takes its last value, as a later line overrides an earlier one.
        values.retain(|v| v.name != name);
        values.push(LinkedValue {
            name: name.to_owned(),
            expression: expression.to_owned(),
        });
    }
    Ok(values)
}

/// The text of an equations file holding `values`, one per line.
pub fn equations_file_text(values: &[LinkedValue]) -> String {
    let mut text = String::new();
    for v in values {
        text.push_str(&format!("\"{}\" = {}\n", v.name, v.expression));
    }
    text
}

/// Reads an equations file.
pub fn read_equations_file(path: &Path) -> Result<Vec<LinkedValue>, FormatError> {
    parse_equations_file(&std::fs::read_to_string(path)?)
}

/// Refuses a value that would not read back from the file: a name that is not a valid
/// variable name, an empty expression, or one spread over several lines (the rest of it
/// would become another line of the file, so it must stay on one).
fn check_value(value: &LinkedValue) -> Result<(), FormatError> {
    let invalid =
        |reason: String| FormatError::InvalidModel(oxyde_model::ModelError::Expression(reason));
    if !oxyde_sketch::is_valid_name(&value.name) {
        return Err(invalid(format!(
            "\"{}\" is not a valid variable name for an equations file",
            value.name
        )));
    }
    let expression = value.expression.trim();
    if expression.is_empty() {
        return Err(invalid(format!("\"{}\" has no expression", value.name)));
    }
    if expression.chars().any(char::is_control) {
        return Err(invalid(format!(
            "the expression of \"{}\" must be on one line",
            value.name
        )));
    }
    Ok(())
}

/// Writes the variables `values` into the equations file at `path`: lines naming one of them
/// get its new expression, the others are added at the end, and every other line (comments,
/// other variables) is kept as it was. The file is created if it does not exist. A value
/// that would not read back (see [`check_value`]) is refused before the file is touched.
pub fn update_equations_file(path: &Path, values: &[LinkedValue]) -> Result<(), FormatError> {
    for value in values {
        check_value(value)?;
    }
    let old = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e.into()),
    };
    // Refuse to rewrite a file that would not read back.
    parse_equations_file(&old)?;
    let mut written = vec![false; values.len()];
    let mut text = String::new();
    for line in old.lines() {
        let name = line
            .trim()
            .strip_prefix('"')
            .and_then(|rest| rest.split_once('"'))
            .map(|(name, _)| name);
        let found = name.and_then(|n| {
            values
                .iter()
                .zip(written.iter_mut())
                .find(|(v, _)| v.name == n)
        });
        match found {
            Some((v, done)) => {
                text.push_str(&format!("\"{}\" = {}\n", v.name, v.expression));
                *done = true;
            }
            None => {
                text.push_str(line);
                text.push('\n');
            }
        }
    }
    for (v, done) in values.iter().zip(written) {
        if !done {
            text.push_str(&format!("\"{}\" = {}\n", v.name, v.expression));
        }
    }
    // And one that would not read back once written.
    parse_equations_file(&text)?;
    crate::write_file(path, text.as_bytes())
}

/// Where the equations file linked to the document (part or assembly) saved at
/// `part_path` is.
pub fn linked_file_path(part_path: &Path, linked_file: &str) -> PathBuf {
    let file = Path::new(linked_file);
    if file.is_absolute() {
        file.to_path_buf()
    } else {
        part_path
            .parent()
            .unwrap_or_else(|| Path::new(""))
            .join(file)
    }
}

/// Refreshes the linked variables of the part saved at `part_path` from its equations file.
/// Returns `Ok(false)` when the part is not linked; a missing or unreadable file is an
/// error, and the part then keeps the values it has.
pub fn refresh_linked_equations(
    part_path: &Path,
    definition: &mut PartDefinition,
) -> Result<bool, FormatError> {
    let Some(file) = definition.equations_text().linked_file else {
        return Ok(false);
    };
    let values = read_equations_file(&linked_file_path(part_path, &file))?;
    definition
        .apply_linked_values(&values)
        .map_err(FormatError::InvalidModel)?;
    Ok(true)
}

/// Refreshes the linked variables of the assembly saved at `assembly_path` from its
/// equations file, as [`refresh_linked_equations`] does for a part.
pub fn refresh_assembly_linked_equations(
    assembly_path: &Path,
    assembly: &mut Assembly,
) -> Result<bool, FormatError> {
    let Some(file) = assembly.equations.linked_file.clone() else {
        return Ok(false);
    };
    let values = read_equations_file(&linked_file_path(assembly_path, &file))?;
    assembly
        .apply_linked_values(&values, &TopLevelOnly)
        .map_err(FormatError::InvalidModel)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_variables_comments_and_expressions() {
        let values = parse_equations_file(
            "' Case\n\n\"Width\" = 233\n  \"Height\"= \"Width\" + 57  \n\"Width\" = 240\n",
        )
        .unwrap();
        assert_eq!(values.len(), 2);
        assert_eq!(values[0].name, "Height");
        assert_eq!(values[0].expression, "\"Width\" + 57");
        assert_eq!(values[1].name, "Width");
        assert_eq!(values[1].expression, "240");
    }

    #[test]
    fn refuses_lines_that_are_not_assignments() {
        assert!(parse_equations_file("Width = 3").is_err());
        assert!(parse_equations_file("\"Width\" 3").is_err());
        assert!(parse_equations_file("\"Width\" =").is_err());
    }

    #[test]
    fn updates_keep_other_lines() {
        let dir = std::env::temp_dir().join(format!("oxyde-eqfile-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("Case.txt");
        std::fs::write(&path, "' Case\n\"Width\" = 233\n\"Depth\" = 300\n").unwrap();
        let set = |name: &str, expression: &str| LinkedValue {
            name: name.to_owned(),
            expression: expression.to_owned(),
        };
        update_equations_file(&path, &[set("Depth", "320"), set("Height", "290")]).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            text,
            "' Case\n\"Width\" = 233\n\"Depth\" = 320\n\"Height\" = 290\n"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A value the file could not give back is refused, and the file is left as it was.
    #[test]
    fn values_that_would_not_read_back_are_refused_before_writing() {
        let dir = std::env::temp_dir().join(format!("oxyde-eqfile-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("Case.txt");
        std::fs::write(&path, "\"Width\" = 233\n").unwrap();
        let set = |name: &str, expression: &str| LinkedValue {
            name: name.to_owned(),
            expression: expression.to_owned(),
        };
        for bad in [
            set("Width", ""),
            set("Width", "30 +\n2"),
            set("Width", "1\n\"Evil\" = 2"),
            set("Wi\"dth", "3"),
            set("D1@Sketch1", "3"),
            set(" Width", "3"),
        ] {
            assert!(
                update_equations_file(&path, std::slice::from_ref(&bad)).is_err(),
                "{bad:?}"
            );
        }
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "\"Width\" = 233\n");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
