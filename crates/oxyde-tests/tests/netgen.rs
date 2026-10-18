//! Netgen's volume mesher through `OcctKernel::volume_mesh`: a box, a filleted box and a
//! cylinder, first and second order, checked against the kernel's own geometry. Skipped
//! (with a note on stderr) when the build has no Netgen; then the kernel must say so.

use oxyde_geom::{Curve2, Plane, Point2, Point3, Profile, ProfileSegment, Vec3};
use oxyde_kernel::{
    CurveKind, Kernel, ShapeHandle, SurfaceKind, TET_EDGES, TRIANGLE_EDGES, Topology, VolumeMesh,
    VolumeMeshError, VolumeMeshOptions,
};
use oxyde_occt::OcctKernel;

/// The kernel, or `None` (after checking it reports the mesher missing) without Netgen.
fn kernel_with_netgen() -> Option<OcctKernel> {
    let mut kernel = OcctKernel::new();
    if kernel.has_volume_mesher() {
        assert!(OcctKernel::netgen_version().is_some());
        return Some(kernel);
    }
    assert!(OcctKernel::netgen_version().is_none());
    let shape = kernel
        .make_box(Point3::new(0.0, 0.0, 0.0), Vec3::new(1.0, 1.0, 1.0))
        .unwrap();
    assert_eq!(
        kernel.volume_mesh(shape, &VolumeMeshOptions::default()),
        Err(VolumeMeshError::Unsupported)
    );
    eprintln!("this build has no Netgen: volume mesh tests skipped");
    None
}

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn lerp(a: [f64; 3], b: [f64; 3], t: f64) -> [f64; 3] {
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
    ]
}

fn xyz(p: Point3) -> [f64; 3] {
    [p.x, p.y, p.z]
}

fn vec(v: Vec3) -> [f64; 3] {
    [v.x, v.y, v.z]
}

/// The volume the boundary encloses, `1/3 ∮ x · n dA`, over the triangles as the mesh
/// shapes them: flat for first order, curved through their edge nodes for second order
/// (Dunavant's six-point rule, exact for the degree-4 integrand of a quadratic triangle).
fn enclosed_volume(mesh: &VolumeMesh) -> f64 {
    const RULE: [(f64, f64, f64); 6] = [
        (
            0.445_948_490_915_965,
            0.445_948_490_915_965,
            0.223_381_589_678_011,
        ),
        (
            0.445_948_490_915_965,
            0.108_103_018_168_070,
            0.223_381_589_678_011,
        ),
        (
            0.108_103_018_168_070,
            0.445_948_490_915_965,
            0.223_381_589_678_011,
        ),
        (
            0.091_576_213_509_771,
            0.091_576_213_509_771,
            0.109_951_743_655_322,
        ),
        (
            0.091_576_213_509_771,
            0.816_847_572_980_459,
            0.109_951_743_655_322,
        ),
        (
            0.816_847_572_980_459,
            0.091_576_213_509_771,
            0.109_951_743_655_322,
        ),
    ];
    let mut volume = 0.0;
    for (t, corners) in mesh.triangles.iter().enumerate() {
        let c = corners.map(|n| mesh.nodes[n as usize]);
        let Some(edges) = &mesh.triangle_edge_nodes else {
            volume += dot(c[0], cross(c[1], c[2])) / 6.0;
            continue;
        };
        // Edge nodes on 01, 12, 20; reference coordinates (s, t) = (L1, L2).
        let m = edges[t].map(|n| mesh.nodes[n as usize]);
        for &(s, u, w) in &RULE {
            let l = [1.0 - s - u, s, u];
            let mut x = [0.0; 3];
            let mut ds = [0.0; 3];
            let mut du = [0.0; 3];
            // Shape functions and their derivatives along s and u.
            let terms = [
                (
                    c[0],
                    l[0] * (2.0 * l[0] - 1.0),
                    1.0 - 4.0 * l[0],
                    1.0 - 4.0 * l[0],
                ),
                (c[1], l[1] * (2.0 * l[1] - 1.0), 4.0 * l[1] - 1.0, 0.0),
                (c[2], l[2] * (2.0 * l[2] - 1.0), 0.0, 4.0 * l[2] - 1.0),
                (m[0], 4.0 * l[0] * l[1], 4.0 * (l[0] - l[1]), -4.0 * l[1]),
                (m[1], 4.0 * l[1] * l[2], 4.0 * l[2], 4.0 * l[1]),
                (m[2], 4.0 * l[2] * l[0], -4.0 * l[2], 4.0 * (l[0] - l[2])),
            ];
            for (p, n, dn_s, dn_u) in terms {
                for k in 0..3 {
                    x[k] += n * p[k];
                    ds[k] += dn_s * p[k];
                    du[k] += dn_u * p[k];
                }
            }
            // The reference triangle's area is 1/2.
            volume += w * 0.5 * dot(x, cross(ds, du)) / 3.0;
        }
    }
    volume
}

/// The distance from `p` to face `face`'s surface, for the planes and cylinders these
/// parts are made of; `radius` is every cylinder's.
fn off_surface(topology: &Topology, face: u32, p: [f64; 3], radius: f64) -> f64 {
    let info = &topology.faces[face as usize];
    match info.surface {
        SurfaceKind::Plane => {
            let n = vec(info.normal.unwrap());
            dot(sub(p, xyz(info.centroid)), n).abs()
        }
        SurfaceKind::Cylinder => {
            let axis = info.axis.unwrap();
            let d = vec(axis.direction);
            let r = sub(p, xyz(axis.origin));
            let along = dot(r, d);
            let across = sub(r, [d[0] * along, d[1] * along, d[2] * along]);
            (dot(across, across).sqrt() - radius).abs()
        }
        other => panic!("no check for a {other:?} face"),
    }
}

/// Meshes `shape` with `options` and checks what every mesh must satisfy.
fn mesh_and_check(
    kernel: &OcctKernel,
    shape: ShapeHandle,
    options: &VolumeMeshOptions,
    radius: f64,
) -> VolumeMesh {
    let mesh = kernel.volume_mesh(shape, options).unwrap();
    let topology = kernel.topology(shape).unwrap();
    assert!(!mesh.tets.is_empty() && !mesh.triangles.is_empty());
    assert_eq!(mesh.is_second_order(), options.second_order);
    assert_eq!(mesh.triangle_faces.len(), mesh.triangles.len());

    // Every element the right way round, and not flat.
    let size = options.max_size;
    for t in &mesh.tets {
        let p = t.map(|n| mesh.nodes[n as usize]);
        let volume6 = dot(sub(p[1], p[0]), cross(sub(p[2], p[0]), sub(p[3], p[0])));
        assert!(volume6 > 1e-9 * size.powi(3), "element {t:?}: {volume6}");
    }

    // Every boundary triangle on a face of the shape, every face meshed, and every
    // boundary node (edge nodes included) on its face's true surface.
    let mut meshed = vec![false; topology.faces.len()];
    for (t, corners) in mesh.triangles.iter().enumerate() {
        let face = mesh.triangle_faces[t];
        assert!((face as usize) < topology.faces.len(), "face {face}");
        meshed[face as usize] = true;
        let mut nodes = corners.to_vec();
        if let Some(edges) = &mesh.triangle_edge_nodes {
            nodes.extend(edges[t]);
        }
        for n in nodes {
            let off = off_surface(&topology, face, mesh.nodes[n as usize], radius);
            assert!(off < 1e-5, "node {n} is {off} mm off face {face}");
        }
    }
    assert!(
        meshed.iter().all(|&m| m),
        "faces without triangles: {meshed:?}"
    );

    // Inside the solid an edge node sits at the middle of its edge; on the boundary it may
    // sit on the curved surface instead, so only those of no boundary triangle are checked.
    if let (Some(tet_edges), Some(triangle_edges)) =
        (&mesh.tet_edge_nodes, &mesh.triangle_edge_nodes)
    {
        let on_boundary: std::collections::HashSet<u32> =
            triangle_edges.iter().flatten().copied().collect();
        for (t, edges) in tet_edges.iter().enumerate() {
            for (k, &(a, b)) in TET_EDGES.iter().enumerate() {
                let n = edges[k];
                if on_boundary.contains(&n) {
                    continue;
                }
                let corners = mesh.tets[t];
                let mid = lerp(
                    mesh.nodes[corners[a] as usize],
                    mesh.nodes[corners[b] as usize],
                    0.5,
                );
                let gap = sub(mesh.nodes[n as usize], mid);
                assert!(dot(gap, gap).sqrt() < 1e-9, "edge node {n} off its edge");
            }
        }
        // A boundary triangle's edge nodes lie between its corners, near the middle.
        for (t, corners) in mesh.triangles.iter().enumerate() {
            for (k, &(a, b)) in TRIANGLE_EDGES.iter().enumerate() {
                let p = mesh.nodes[corners[a] as usize];
                let q = mesh.nodes[corners[b] as usize];
                let gap = sub(mesh.nodes[triangle_edges[t][k] as usize], lerp(p, q, 0.5));
                let edge = sub(q, p);
                assert!(
                    dot(gap, gap) < 0.25 * dot(edge, edge),
                    "edge node far off its edge"
                );
            }
        }
    }

    // The volume the boundary encloses is the solid's.
    let expected = kernel.volume(shape).unwrap();
    let enclosed = enclosed_volume(&mesh);
    let tets: f64 = mesh
        .tets
        .iter()
        .map(|t| {
            let p = t.map(|n| mesh.nodes[n as usize]);
            dot(sub(p[1], p[0]), cross(sub(p[2], p[0]), sub(p[3], p[0]))) / 6.0
        })
        .sum();
    eprintln!(
        "{} nodes, {} elements, order {}: volume {enclosed:.3} (straight {tets:.3}) for {expected:.3}",
        mesh.nodes.len(),
        mesh.tets.len(),
        if mesh.is_second_order() { 2 } else { 1 },
    );
    assert!(((enclosed - expected) / expected).abs() < 0.01);
    // The elements fill exactly what their straight-sided boundary encloses.
    let straight = VolumeMesh {
        triangle_edge_nodes: None,
        ..mesh.clone()
    };
    assert!((tets - enclosed_volume(&straight)).abs() < 1e-6 * expected);
    mesh
}

fn options(max_size: f64, second_order: bool) -> VolumeMeshOptions {
    VolumeMeshOptions {
        max_size,
        curvature_safety: 6.0,
        second_order,
        ..VolumeMeshOptions::default()
    }
}

#[test]
fn a_box_is_meshed_exactly() {
    let Some(mut kernel) = kernel_with_netgen() else {
        return;
    };
    let shape = kernel
        .make_box(Point3::new(-5.0, 0.0, 2.0), Vec3::new(40.0, 20.0, 10.0))
        .unwrap();
    for second_order in [false, true] {
        let mesh = mesh_and_check(&kernel, shape, &options(4.0, second_order), 0.0);
        let enclosed = enclosed_volume(&mesh);
        assert!((enclosed - 8000.0).abs() < 1e-6, "{enclosed}");
    }
}

#[test]
fn a_filleted_box_keeps_its_rounds() {
    let Some(mut kernel) = kernel_with_netgen() else {
        return;
    };
    let block = kernel
        .make_box(Point3::new(0.0, 0.0, 0.0), Vec3::new(40.0, 20.0, 10.0))
        .unwrap();
    // The four edges along Z.
    let edges: Vec<u32> = kernel
        .topology(block)
        .unwrap()
        .edges
        .iter()
        .enumerate()
        .filter(|(_, e)| {
            e.curve == CurveKind::Line && e.axis.is_some_and(|a| a.direction.z.abs() > 0.99)
        })
        .map(|(k, _)| k as u32)
        .collect();
    assert_eq!(edges.len(), 4);
    let shape = kernel.fillet(block, &edges, 4.0).unwrap().shape;
    for second_order in [false, true] {
        mesh_and_check(&kernel, shape, &options(3.0, second_order), 4.0);
    }
}

#[test]
fn a_cylinder_is_meshed_on_its_true_surface() {
    let Some(mut kernel) = kernel_with_netgen() else {
        return;
    };
    let profile = Profile::from_curves(&[ProfileSegment {
        id: 0,
        curve: Curve2::Circle {
            center: Point2::new(0.0, 0.0),
            radius: 10.0,
        },
    }])
    .unwrap();
    let shape = kernel
        .extrude(&Plane::FRONT, &profile, Vec3::new(0.0, 0.0, 30.0))
        .unwrap()
        .shape;
    let linear = mesh_and_check(&kernel, shape, &options(3.0, false), 10.0);
    let curved = mesh_and_check(&kernel, shape, &options(3.0, true), 10.0);
    // Curved elements follow the cylinder far closer than flat ones.
    let exact = kernel.volume(shape).unwrap();
    let flat_error = (enclosed_volume(&linear) - exact).abs();
    let curved_error = (enclosed_volume(&curved) - exact).abs();
    assert!(
        curved_error < 0.1 * flat_error,
        "{curved_error} vs {flat_error}"
    );
}

#[test]
fn two_solids_are_refused() {
    let Some(mut kernel) = kernel_with_netgen() else {
        return;
    };
    let a = kernel
        .make_box(Point3::new(0.0, 0.0, 0.0), Vec3::new(10.0, 10.0, 10.0))
        .unwrap();
    let b = kernel
        .make_box(Point3::new(20.0, 0.0, 0.0), Vec3::new(10.0, 10.0, 10.0))
        .unwrap();
    let both = kernel
        .boolean(oxyde_kernel::BooleanOp::Fuse, a, b)
        .unwrap()
        .shape;
    match kernel.volume_mesh(both, &VolumeMeshOptions::with_size(5.0)) {
        Err(VolumeMeshError::Failed(message)) => assert!(message.contains("one solid")),
        other => panic!("{other:?}"),
    }
}
