//! Reading tool arguments: JSON values with errors that say which argument is wrong.

use std::path::PathBuf;

use oxyde_geom::{Point2, Point3, Vec3};
use serde_json::Value;

pub(crate) type Result<T> = std::result::Result<T, String>;

pub(crate) fn get<'a>(args: &'a Value, key: &str) -> Result<&'a Value> {
    args.get(key)
        .filter(|v| !v.is_null())
        .ok_or_else(|| format!("missing argument \"{key}\""))
}

pub(crate) fn opt<'a>(args: &'a Value, key: &str) -> Option<&'a Value> {
    args.get(key).filter(|v| !v.is_null())
}

pub(crate) fn str<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    get(args, key)?
        .as_str()
        .ok_or_else(|| format!("\"{key}\" must be a string"))
}

pub(crate) fn opt_str<'a>(args: &'a Value, key: &str) -> Result<Option<&'a str>> {
    match opt(args, key) {
        None => Ok(None),
        Some(v) => v
            .as_str()
            .map(Some)
            .ok_or_else(|| format!("\"{key}\" must be a string")),
    }
}

pub(crate) fn number(value: &Value, what: &str) -> Result<f64> {
    value
        .as_f64()
        .filter(|v| v.is_finite())
        .ok_or_else(|| format!("{what} must be a number"))
}

pub(crate) fn f64(args: &Value, key: &str) -> Result<f64> {
    number(get(args, key)?, &format!("\"{key}\""))
}

pub(crate) fn opt_f64(args: &Value, key: &str) -> Result<Option<f64>> {
    opt(args, key)
        .map(|v| number(v, &format!("\"{key}\"")))
        .transpose()
}

pub(crate) fn positive(args: &Value, key: &str) -> Result<f64> {
    let value = f64(args, key)?;
    if value > 0.0 {
        Ok(value)
    } else {
        Err(format!("\"{key}\" must be more than 0"))
    }
}

pub(crate) fn bool(args: &Value, key: &str, default: bool) -> Result<bool> {
    match opt(args, key) {
        None => Ok(default),
        Some(v) => v
            .as_bool()
            .ok_or_else(|| format!("\"{key}\" must be true or false")),
    }
}

pub(crate) fn count(args: &Value, key: &str) -> Result<u32> {
    let value = f64(args, key)?;
    if value >= 1.0 && value.fract() == 0.0 && value <= f64::from(u32::MAX) {
        Ok(value as u32)
    } else {
        Err(format!("\"{key}\" must be a whole number of at least 1"))
    }
}

pub(crate) fn path(args: &Value, key: &str) -> Result<PathBuf> {
    Ok(PathBuf::from(str(args, key)?))
}

/// `[x, y]`.
pub(crate) fn point2(value: &Value, what: &str) -> Result<Point2> {
    let numbers = numbers(value, what)?;
    match numbers.as_slice() {
        [x, y] => Ok(Point2::new(*x, *y)),
        _ => Err(format!("{what} must be [x, y]")),
    }
}

/// `[x, y, z]`.
pub(crate) fn point3(value: &Value, what: &str) -> Result<Point3> {
    let numbers = numbers(value, what)?;
    match numbers.as_slice() {
        [x, y, z] => Ok(Point3::new(*x, *y, *z)),
        _ => Err(format!("{what} must be [x, y, z]")),
    }
}

pub(crate) fn vec3(value: &Value, what: &str) -> Result<Vec3> {
    point3(value, what).map(Point3::to_vec)
}

fn numbers(value: &Value, what: &str) -> Result<Vec<f64>> {
    value
        .as_array()
        .ok_or_else(|| format!("{what} must be a list of numbers"))?
        .iter()
        .map(|v| number(v, what))
        .collect()
}

/// A list argument, or a single value taken as a list of one.
pub(crate) fn list(value: &Value) -> Vec<&Value> {
    match value {
        Value::Array(items) => items.iter().collect(),
        Value::Null => Vec::new(),
        other => vec![other],
    }
}

/// Rounds for display: 4 decimals, no `-0`.
pub(crate) fn round(value: f64) -> f64 {
    let r = (value * 1e4).round() / 1e4;
    if r == 0.0 { 0.0 } else { r }
}

pub(crate) fn round3(p: Point3) -> [f64; 3] {
    [round(p.x), round(p.y), round(p.z)]
}

pub(crate) fn round_vec(v: Vec3) -> [f64; 3] {
    [round(v.x), round(v.y), round(v.z)]
}

/// Sets a key of a JSON object (does nothing to a value that is not an object): what
/// `value["key"] = …` does, without its panic on other values.
pub(crate) trait Set {
    fn set(&mut self, key: &str, value: Value);
}

impl Set for Value {
    fn set(&mut self, key: &str, value: Value) {
        if let Some(object) = self.as_object_mut() {
            object.insert(key.to_owned(), value);
        }
    }
}
