//! An OpenFOAM mesh (`constant/polyMesh`) read back: points, faces, the cells on each side
//! of every face, and the boundary patches, with the geometry worked out as OpenFOAM does.

use std::fs;
use std::path::Path;

use crate::case::M_PER_MM;
use crate::foam::{FoamError, Lexer, header};
use crate::results::ReadError;
use crate::study::{add, cross, dot, norm, scale, sub};

/// A boundary patch of the mesh: a run of faces.
#[derive(Debug, Clone, PartialEq)]
pub struct BoundaryPatch {
    pub name: String,
    /// `wall`, `patch`…
    pub kind: String,
    /// The first face, and how many.
    pub start: usize,
    pub count: usize,
}

/// A polyhedral mesh. Internal faces come first, each between its owner and its
/// neighbour cell, its normal pointing from owner to neighbour; boundary faces follow,
/// patch by patch, their normals pointing out of the fluid.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PolyMesh {
    /// Points, mm.
    pub points: Vec<[f64; 3]>,
    /// Face `f`'s points are `face_points[face_offsets[f]..face_offsets[f + 1]]`.
    pub face_offsets: Vec<usize>,
    pub face_points: Vec<u32>,
    pub owner: Vec<u32>,
    pub neighbour: Vec<u32>,
    pub patches: Vec<BoundaryPatch>,
    /// The number of cells.
    pub cells: usize,
}

impl PolyMesh {
    /// Reads `constant/polyMesh` from a case folder.
    pub fn read(case_dir: &Path) -> Result<Self, ReadError> {
        let dir = case_dir.join("constant").join("polyMesh");
        let read = |name: &str| {
            let path = dir.join(name);
            fs::read(&path).map_err(|error| ReadError::Io { path, error })
        };
        let parse_err = |name: &str| {
            let path = dir.join(name);
            move |e: FoamError| ReadError::parse(&path, e)
        };

        let text = read("points")?;
        let mut lex = Lexer::new(&text);
        header(&mut lex).map_err(parse_err("points"))?;
        let points: Vec<[f64; 3]> = lex
            .list(|l| l.vector())
            .map_err(parse_err("points"))?
            .into_iter()
            .map(|p| scale(p, 1.0 / M_PER_MM))
            .collect();

        let text = read("faces")?;
        let mut lex = Lexer::new(&text);
        let class = header(&mut lex).map_err(parse_err("faces"))?.class;
        let (face_offsets, face_points) = if class == "faceCompactList" {
            let offsets = lex.list(|l| l.label()).map_err(parse_err("faces"))?;
            let points = lex
                .list(|l| l.label().map(|i| i as u32))
                .map_err(parse_err("faces"))?;
            (offsets, points)
        } else {
            let faces = lex
                .list(|l| l.list(|m| m.label().map(|i| i as u32)))
                .map_err(parse_err("faces"))?;
            let mut offsets = Vec::with_capacity(faces.len() + 1);
            let mut flat = Vec::new();
            offsets.push(0);
            for face in faces {
                flat.extend(face);
                offsets.push(flat.len());
            }
            (offsets, flat)
        };

        let labels = |name: &str| -> Result<Vec<u32>, ReadError> {
            let text = read(name)?;
            let mut lex = Lexer::new(&text);
            header(&mut lex).map_err(parse_err(name))?;
            lex.list(|l| l.label().map(|i| i as u32))
                .map_err(parse_err(name))
        };
        let owner = labels("owner")?;
        let neighbour = labels("neighbour")?;

        let text = read("boundary")?;
        let mut lex = Lexer::new(&text);
        header(&mut lex).map_err(parse_err("boundary"))?;
        let patches = lex
            .list(|l| {
                let name = l.word()?.to_string();
                l.expect(b'{')?;
                let mut patch = BoundaryPatch {
                    name,
                    kind: String::new(),
                    start: 0,
                    count: 0,
                };
                while !l.eat(b'}') {
                    match l.word()? {
                        "type" => {
                            patch.kind = l.word()?.to_string();
                            l.expect(b';')?;
                        }
                        "nFaces" => {
                            patch.count = l.label()?;
                            l.expect(b';')?;
                        }
                        "startFace" => {
                            patch.start = l.label()?;
                            l.expect(b';')?;
                        }
                        _ => l.skip_value()?,
                    }
                }
                Ok(patch)
            })
            .map_err(parse_err("boundary"))?;

        let cells = owner
            .iter()
            .chain(&neighbour)
            .map(|&c| c as usize + 1)
            .max()
            .unwrap_or(0);
        let mesh = Self {
            points,
            face_offsets,
            face_points,
            owner,
            neighbour,
            patches,
            cells,
        };
        mesh.check().map_err(|detail| ReadError::Inconsistent {
            path: dir.clone(),
            detail,
        })?;
        Ok(mesh)
    }

    /// Checks that every index points somewhere, so that the geometry can index freely.
    fn check(&self) -> Result<(), String> {
        let faces = self.faces();
        if self.face_offsets.first() != Some(&0)
            || self
                .face_offsets
                .windows(2)
                .any(|w| matches!(w, [a, b] if a > b))
            || self.face_offsets.last() != Some(&self.face_points.len())
        {
            return Err("the faces' point lists do not add up".to_string());
        }
        if let Some(&p) = self
            .face_points
            .iter()
            .find(|&&p| p as usize >= self.points.len())
        {
            return Err(format!("a face uses point {p}, which does not exist"));
        }
        if self.owner.len() != faces || self.neighbour.len() > faces {
            return Err(format!(
                "{faces} faces but {} owners and {} neighbours",
                self.owner.len(),
                self.neighbour.len()
            ));
        }
        for patch in &self.patches {
            if patch.start < self.neighbour.len() || patch.start + patch.count > faces {
                return Err(format!(
                    "patch {} lies outside the boundary faces",
                    patch.name
                ));
            }
        }
        Ok(())
    }

    /// The number of faces.
    pub fn faces(&self) -> usize {
        self.face_offsets.len().saturating_sub(1)
    }

    /// The number of internal faces; boundary faces come after them.
    pub fn internal_faces(&self) -> usize {
        self.neighbour.len()
    }

    /// Face `f`'s points.
    pub fn face(&self, f: usize) -> &[u32] {
        match (self.face_offsets.get(f), self.face_offsets.get(f + 1)) {
            (Some(&a), Some(&b)) => self.face_points.get(a..b).unwrap_or_default(),
            _ => &[],
        }
    }

    /// A patch by name.
    pub fn patch(&self, name: &str) -> Option<&BoundaryPatch> {
        self.patches.iter().find(|p| p.name == name)
    }

    /// Face `f`'s centre (mm) and area vector (mm², along its normal), as OpenFOAM works
    /// them out: triangles fanned from the points' average.
    pub fn face_geometry(&self, f: usize) -> ([f64; 3], [f64; 3]) {
        let pts: Vec<[f64; 3]> = self
            .face(f)
            .iter()
            .filter_map(|&p| self.points.get(p as usize).copied())
            .collect();
        let n = pts.len();
        if n < 3 {
            return ([0.0; 3], [0.0; 3]);
        }
        let average = scale(pts.iter().fold([0.0; 3], |a, &p| add(a, p)), 1.0 / n as f64);
        if let [a, b, c] = pts[..] {
            return (average, scale(cross(sub(b, a), sub(c, a)), 0.5));
        }
        let mut sum_n = [0.0; 3];
        let mut sum_a = 0.0;
        let mut sum_ac = [0.0; 3];
        for (&this, &next) in pts.iter().zip(pts.iter().cycle().skip(1)) {
            let c = add(add(this, next), average);
            let normal = cross(sub(next, this), sub(average, this));
            let a = norm(normal);
            sum_n = add(sum_n, normal);
            sum_a += a;
            sum_ac = add(sum_ac, scale(c, a));
        }
        let centre = if sum_a > 0.0 {
            scale(sum_ac, 1.0 / (3.0 * sum_a))
        } else {
            average
        };
        (centre, scale(sum_n, 0.5))
    }

    /// Every cell's centre (mm) and volume (mm³), as OpenFOAM works them out: pyramids
    /// from an estimated centre to each face.
    pub fn cell_geometry(&self) -> (Vec<[f64; 3]>, Vec<f64>) {
        let faces: Vec<([f64; 3], [f64; 3])> =
            (0..self.faces()).map(|f| self.face_geometry(f)).collect();
        let mut estimate = vec![[0.0; 3]; self.cells];
        let mut count = vec![0usize; self.cells];
        let sides = |f: usize| {
            let own = self.owner.get(f).map(|&c| c as usize);
            let nei = self.neighbour.get(f).map(|&c| c as usize);
            [own, nei]
        };
        for (f, (centre, _)) in faces.iter().enumerate() {
            for c in sides(f).into_iter().flatten() {
                if let (Some(e), Some(n)) = (estimate.get_mut(c), count.get_mut(c)) {
                    *e = add(*e, *centre);
                    *n += 1;
                }
            }
        }
        for (e, &n) in estimate.iter_mut().zip(&count) {
            if n > 0 {
                *e = scale(*e, 1.0 / n as f64);
            }
        }
        let mut centres = vec![[0.0; 3]; self.cells];
        let mut volumes = vec![0.0; self.cells];
        for (f, (centre, area)) in faces.iter().enumerate() {
            let [own, nei] = sides(f);
            for (c, outward) in [(own, true), (nei, false)] {
                let Some(c) = c else { continue };
                let Some(&est) = estimate.get(c) else {
                    continue;
                };
                let h = dot(*area, sub(*centre, est));
                let pyramid = if outward { h } else { -h };
                let at = add(scale(*centre, 0.75), scale(est, 0.25));
                if let (Some(sum), Some(v)) = (centres.get_mut(c), volumes.get_mut(c)) {
                    *sum = add(*sum, scale(at, pyramid));
                    *v += pyramid;
                }
            }
        }
        for ((centre, volume), est) in centres.iter_mut().zip(volumes.iter_mut()).zip(&estimate) {
            *centre = if volume.abs() > 0.0 {
                scale(*centre, 1.0 / *volume)
            } else {
                *est
            };
            *volume /= 3.0;
        }
        (centres, volumes)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Writes a mesh of two unit cubes (in metres) side by side along x, the first face
    /// between them, then the boundary as two patches: `inlet_0` (x = 0), `wall_1` (the
    /// rest).
    pub(crate) fn write_two_cubes(dir: &Path, compact: bool) {
        let poly = dir.join("constant/polyMesh");
        fs::create_dir_all(&poly).unwrap();
        let head = |class: &str, object: &str| {
            format!(
                "FoamFile\n{{\n    format ascii;\n    class {class};\n    object {object};\n}}\n"
            )
        };
        // Points: x in {0, 1, 2}, y and z in {0, 1}; index = x + 3 (y + 2 z).
        let mut points = String::from("12\n(\n");
        for z in 0..2 {
            for y in 0..2 {
                for x in 0..3 {
                    points.push_str(&format!("({x} {y} {z})\n"));
                }
            }
        }
        points.push_str(")\n");
        fs::write(poly.join("points"), head("vectorField", "points") + &points).unwrap();
        let p = |x: u32, y: u32, z: u32| x + 3 * (y + 2 * z);
        let faces: Vec<Vec<u32>> = vec![
            // Internal, x = 1, normal +x.
            vec![p(1, 0, 0), p(1, 1, 0), p(1, 1, 1), p(1, 0, 1)],
            // inlet_0: x = 0, normal −x, owner 0.
            vec![p(0, 0, 0), p(0, 0, 1), p(0, 1, 1), p(0, 1, 0)],
            // wall_1: x = 2 (owner 1), then y = 0, y = 1, z = 0, z = 1 of each cube.
            vec![p(2, 0, 0), p(2, 1, 0), p(2, 1, 1), p(2, 0, 1)],
            vec![p(0, 0, 0), p(1, 0, 0), p(1, 0, 1), p(0, 0, 1)],
            vec![p(1, 0, 0), p(2, 0, 0), p(2, 0, 1), p(1, 0, 1)],
            vec![p(0, 1, 0), p(0, 1, 1), p(1, 1, 1), p(1, 1, 0)],
            vec![p(1, 1, 0), p(1, 1, 1), p(2, 1, 1), p(2, 1, 0)],
            vec![p(0, 0, 0), p(0, 1, 0), p(1, 1, 0), p(1, 0, 0)],
            vec![p(1, 0, 0), p(1, 1, 0), p(2, 1, 0), p(2, 0, 0)],
            vec![p(0, 0, 1), p(1, 0, 1), p(1, 1, 1), p(0, 1, 1)],
            vec![p(1, 0, 1), p(2, 0, 1), p(2, 1, 1), p(1, 1, 1)],
        ];
        let text = if compact {
            let mut offsets = vec![0usize];
            let mut flat: Vec<u32> = Vec::new();
            for f in &faces {
                flat.extend(f);
                offsets.push(flat.len());
            }
            let list = |v: Vec<String>| format!("{}\n(\n{}\n)\n", v.len(), v.join("\n"));
            head("faceCompactList", "faces")
                + &list(offsets.iter().map(|o| o.to_string()).collect())
                + &list(flat.iter().map(|o| o.to_string()).collect())
        } else {
            let body: Vec<String> = faces
                .iter()
                .map(|f| {
                    let ids: Vec<String> = f.iter().map(|i| i.to_string()).collect();
                    format!("{}({})", f.len(), ids.join(" "))
                })
                .collect();
            head("faceList", "faces") + &format!("{}\n(\n{}\n)\n", faces.len(), body.join("\n"))
        };
        fs::write(poly.join("faces"), text).unwrap();
        fs::write(
            poly.join("owner"),
            head("labelList", "owner") + "11(0 0 1 0 1 0 1 0 1 0 1)\n",
        )
        .unwrap();
        fs::write(
            poly.join("neighbour"),
            head("labelList", "neighbour") + "1(1)\n",
        )
        .unwrap();
        fs::write(
            poly.join("boundary"),
            head("polyBoundaryMesh", "boundary")
                + "3\n(\n    domain { type wall; inGroups 1(wall); nFaces 0; startFace 1; }\n    inlet_0 { type patch; nFaces 1; startFace 1; }\n    wall_1 { type wall; inGroups 1(wall); nFaces 9; startFace 2; }\n)\n",
        )
        .unwrap();
    }

    fn close(a: [f64; 3], b: [f64; 3]) -> bool {
        (0..3).all(|k| (a[k] - b[k]).abs() < 1e-9)
    }

    #[test]
    fn two_cubes_read_in_both_face_formats() {
        for compact in [false, true] {
            let dir = std::env::temp_dir()
                .join(format!("oxyde-flow-mesh-{compact}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            write_two_cubes(&dir, compact);
            let mesh = PolyMesh::read(&dir).unwrap();
            assert_eq!(mesh.cells, 2);
            assert_eq!(mesh.faces(), 11);
            assert_eq!(mesh.internal_faces(), 1);
            assert_eq!(mesh.patches.len(), 3);
            assert_eq!(mesh.patch("wall_1").unwrap().count, 9);
            // Metres became millimetres.
            assert_eq!(mesh.points[1], [1000.0, 0.0, 0.0]);

            let (centre, area) = mesh.face_geometry(0);
            assert!(close(centre, [1000.0, 500.0, 500.0]), "{centre:?}");
            assert!(close(area, [1e6, 0.0, 0.0]), "{area:?}");
            // The inlet faces out of the fluid, towards −x.
            let (_, inlet) = mesh.face_geometry(1);
            assert!(close(inlet, [-1e6, 0.0, 0.0]), "{inlet:?}");

            let (centres, volumes) = mesh.cell_geometry();
            assert!(close(centres[0], [500.0, 500.0, 500.0]), "{:?}", centres[0]);
            assert!(close(centres[1], [1500.0, 500.0, 500.0]));
            assert!((volumes[0] - 1e9).abs() < 1e-3 && (volumes[1] - 1e9).abs() < 1e-3);
            let _ = fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn a_mesh_pointing_nowhere_is_refused() {
        let dir = std::env::temp_dir().join(format!("oxyde-flow-badmesh-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        write_two_cubes(&dir, false);
        let owner = dir.join("constant/polyMesh/owner");
        fs::write(
            &owner,
            "FoamFile { format ascii; class labelList; object owner; }\n3(0 0 1)\n",
        )
        .unwrap();
        assert!(matches!(
            PolyMesh::read(&dir),
            Err(ReadError::Inconsistent { .. })
        ));
        let _ = fs::remove_dir_all(&dir);
    }
}
