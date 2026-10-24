//! Booleans: fuse, cut and common of a target with tools.
//!
//! Every solid is cut face by face along where the faces meet ([`crate::curved`]), planes
//! as much as curved faces: one engine for all. A result is then held to what the
//! operation can do at all before it is used, the checks SolidCraft's kernel makes of its
//! booleans (`boolean_guard.rs` there): it is closed (checked when the kernel keeps it), a
//! fuse never loses volume, a cut or a common never gains any, and nothing reaches beyond
//! the inputs. A result that fails is refused with an error: a wrong result that looks right
//! is worse.

use oxyde_geom::{LINEAR_TOLERANCE, Point3};
use oxyde_kernel::{BooleanOp, OpHistory};

use crate::NativeError;
use crate::brep::Shape;
use crate::curved;

/// `target` (input 0) fused with, cut by or in common with `tools` (inputs 1, 2…): a fuse
/// joins them all, a cut removes them all, a common keeps what the target shares with any
/// of them. The history names, for each face of the result, the input faces it is made of.
pub(crate) fn boolean(
    op: BooleanOp,
    target: &Shape,
    tools: &[&Shape],
) -> Result<(Shape, OpHistory), NativeError> {
    if tools.is_empty() {
        return Err(NativeError::Failed("a boolean needs a tool".to_owned()));
    }
    let (shape, history) = curved::boolean(op, target, tools)?;
    check(op, target, tools, &shape)?;
    Ok((shape, history))
}

/// What a boolean can make of its inputs, checked on its result.
fn check(
    op: BooleanOp,
    target: &Shape,
    tools: &[&Shape],
    result: &Shape,
) -> Result<(), NativeError> {
    let wrong = |what: &str| -> Result<(), NativeError> {
        Err(NativeError::Internal(format!(
            "the boolean's result {what}, and was refused"
        )))
    };
    let volume = |shape: &Shape| shape.volume().map_err(NativeError::Internal);
    let before = volume(target)?;
    let mut largest = before.abs().max(1.0);
    for tool in tools {
        largest = largest.max(volume(tool)?.abs());
    }
    let slack = 1e-6 * largest;
    let after = volume(result)?;
    if after < -slack {
        return wrong("has a negative volume");
    }
    if op == BooleanOp::Fuse && after < before - slack {
        return wrong("is smaller than the target");
    }
    if op != BooleanOp::Fuse && after > before + slack {
        return wrong("is larger than the target");
    }
    // A cut or a common stays within the target; a fuse within the target and the tools.
    let reach = if op == BooleanOp::Fuse {
        tools
            .iter()
            .filter_map(|t| t.bounds())
            .fold(target.bounds(), |acc, b| {
                Some(acc.map_or(b, |a| hull(a, b)))
            })
    } else {
        target.bounds()
    };
    if let Some((lo, hi)) = result.bounds() {
        let Some((reach_lo, reach_hi)) = reach else {
            return wrong("is not empty, though its inputs are");
        };
        let t = 10.0 * LINEAR_TOLERANCE;
        let outside = lo.x < reach_lo.x - t
            || lo.y < reach_lo.y - t
            || lo.z < reach_lo.z - t
            || hi.x > reach_hi.x + t
            || hi.y > reach_hi.y + t
            || hi.z > reach_hi.z + t;
        if outside {
            return wrong("reaches beyond its inputs");
        }
    }
    Ok(())
}

fn hull(a: (Point3, Point3), b: (Point3, Point3)) -> (Point3, Point3) {
    (
        Point3::new(a.0.x.min(b.0.x), a.0.y.min(b.0.y), a.0.z.min(b.0.z)),
        Point3::new(a.1.x.max(b.1.x), a.1.y.max(b.1.y), a.1.z.max(b.1.z)),
    )
}
