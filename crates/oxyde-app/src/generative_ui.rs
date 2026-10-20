//! The Generative Design tab, as Fusion 360's Generative Design workspace has it
//! (`docs/SIMULATION.md` §9): shapes grown between the bodies to keep and the bodies to stay
//! clear of, as stiff as can be for a mass, for each material and each way of making them.
//!
//! A generative study is one of the model's studies (`simulation_studies.rs`), saved with
//! the others in the `.oxysim` and shown in this tab only. Setting one up: give the part's
//! bodies their roles (*Preserve*, *Obstacle*, *Starting Shape*: select a face of each, then
//! the command, or pick the role in the panel), hold and load the preserve bodies as in a
//! static study, set the mass target, the ways of making it and the materials. *Generate*
//! lays the design space on a grid of voxels once and grows an outcome for every material
//! and every method direction on a thread of its own (`oxyde_sim::generative`), on the
//! graphics card when there is one (`oxyde-sim-gpu`). The outcomes show as they come:
//! *Explore* lists them, plots them mass against stress (or stiffness, displacement, safety)
//! and draws the one picked in the viewport, its iterations on a slider; *Export* writes it
//! as STL, OBJ or 3MF. The outcomes are saved beside the study as an `.oxyres` and read back
//! when the study opens, if the model and the study are as they were.

use std::collections::BTreeMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui;
use oxyde_format::results::{
    DensitySnapshot, Fingerprint, GenerativeIteration, GenerativeOutcome, GenerativeResults,
    NodalFields, OutcomeStatus, ResultMesh, ResultsFile, SolverInfo, Summary, VoxelGrid, VoxelKind,
};
use oxyde_format::simulation::{
    AxisDirection, GenerativeMethod, GenerativeObjective, GenerativeObstacle, GenerativeSettings,
    LoadCase, MeshSize, OutcomeMethod, StudyBody, StudyType, SymmetryPlanes,
};
use oxyde_kernel::{FaceMesh, Kernel, Mesh, ShapeHandle, StepMaterial};
use oxyde_model::{BodyId, FeatureStatus, FileData, Import, Material, Mechanical, Part};
use oxyde_render::{Material as RenderMaterial, Scene};
use oxyde_sim::generative::{
    Backend, DesignSpace, Direction, GenerativeError, GenerativeStudy, Grid, MAX_VOXELS, Method,
    Objective, Obstacle, PreserveRegion, Voxel, decimate_holding, exact_surface, generate_on,
    solid_pieces, surface, voxel_size,
};
use oxyde_sim::{Elastic, FixtureKind, LoadKind, Shape, SolveOptions, SurfaceMesh, Symmetry};

use crate::app::OxydeApp;
use crate::simulation_ui::{
    ItemKind, PanelAction, SimFace, Study, Surfaces, format_value, item_row,
};

/// The colours of the bodies' roles while setting up, as Fusion 360 shows them.
pub(crate) const PRESERVE_COLOR: [u8; 3] = [70, 170, 95];
pub(crate) const OBSTACLE_COLOR: [u8; 3] = [215, 75, 65];
pub(crate) const STARTING_COLOR: [u8; 3] = [235, 190, 60];
/// An outcome without a material colour, and the obstacles drawn faintly around one.
const OUTCOME_COLOR: [u8; 3] = [175, 182, 192];
const GHOST_OPACITY: f32 = 0.22;
/// Rounds of Taubin's smoothing an outcome's surface is drawn and exported with.
pub(crate) const SMOOTHING: usize = 12;
/// The triangles an outcome's surface is simplified to for *Create Part*: one face each,
/// as many as the kernel makes a solid of in a few seconds.
const PART_TRIANGLES: usize = 8000;
/// Nor further than this share of a voxel from where it was: where that keeps more
/// triangles, the part has more.
const PART_TOLERANCE: f64 = 0.25;

// ---- Roles ---------------------------------------------------------------------------------

/// What a body of the part is to a generative study.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Role {
    /// Left out.
    None,
    /// Kept as it is: fixtures and loads go on its faces.
    Preserve,
    /// Material stays out of it.
    Obstacle,
    /// The material starts in it and stays inside.
    StartingShape,
}

impl Role {
    pub(crate) const ALL: [Role; 4] = [
        Role::None,
        Role::Preserve,
        Role::Obstacle,
        Role::StartingShape,
    ];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Role::None => "—",
            Role::Preserve => "Preserve",
            Role::Obstacle => "Obstacle",
            Role::StartingShape => "Starting shape",
        }
    }

    fn hint(self) -> &'static str {
        match self {
            Role::None => "Left out of the study",
            Role::Preserve => {
                "Kept as it is: the outcome joins it. Put the fixtures and loads on its faces"
            }
            Role::Obstacle => "No material goes into it (nor within its offset)",
            Role::StartingShape => "The material starts in it and stays inside it",
        }
    }

    fn color(self) -> Option<[u8; 3]> {
        match self {
            Role::None => None,
            Role::Preserve => Some(PRESERVE_COLOR),
            Role::Obstacle => Some(OBSTACLE_COLOR),
            Role::StartingShape => Some(STARTING_COLOR),
        }
    }
}

/// The role `id` has in `g`.
pub(crate) fn role_of(g: &GenerativeSettings, id: &StudyBody) -> Role {
    if g.preserve.contains(id) {
        Role::Preserve
    } else if g.obstacles.iter().any(|o| o.body == *id) {
        Role::Obstacle
    } else if g.starting_shape.as_ref() == Some(id) {
        Role::StartingShape
    } else {
        Role::None
    }
}

/// Gives `id` the role `role`, and no other: a body has one role at most. An obstacle
/// keeps its offset; a new starting shape replaces the one before.
pub(crate) fn set_role(g: &mut GenerativeSettings, id: StudyBody, role: Role) {
    if role_of(g, &id) == role {
        return;
    }
    g.preserve.retain(|b| *b != id);
    g.obstacles.retain(|o| o.body != id);
    if g.starting_shape.as_ref() == Some(&id) {
        g.starting_shape = None;
    }
    match role {
        Role::None => {}
        Role::Preserve => g.preserve.push(id),
        Role::Obstacle => g.obstacles.push(GenerativeObstacle {
            body: id,
            offset: 0.0,
        }),
        Role::StartingShape => g.starting_shape = Some(id),
    }
}

/// The number of body `id` in `part`, as the timeline orders them (`Body 2`).
fn body_number(part: &Part, id: BodyId) -> Option<usize> {
    let mut ids: Vec<BodyId> = part.body()?.solids.iter().map(|s| s.id).collect();
    ids.sort_by_key(|b| (part.index_of(b.feature), b.index));
    ids.iter().position(|b| *b == id).map(|i| i + 1)
}

/// A hash of the roles, to tell when the viewport's colours are out of date.
fn roles_hash(g: &GenerativeSettings) -> u64 {
    let mut h = DefaultHasher::new();
    g.preserve.hash(&mut h);
    g.obstacles
        .iter()
        .map(|o| &o.body)
        .collect::<Vec<_>>()
        .hash(&mut h);
    g.starting_shape.hash(&mut h);
    h.finish()
}

/// [`roles_hash`] and the faces of the preserve regions, painted as preserve bodies are.
fn colours_hash(study: &Study) -> u64 {
    let mut h = DefaultHasher::new();
    roles_hash(&study.generative).hash(&mut h);
    for (_, _, faces) in preserve_items(study) {
        for f in faces {
            (&f.occurrence, &f.face).hash(&mut h);
        }
    }
    h.finish()
}

/// The surface of body `b` among `surfaces`, and its index.
fn solid_of<'a>(surfaces: &'a Surfaces, b: &StudyBody) -> Option<(usize, &'a SurfaceMesh)> {
    let i = surfaces
        .bodies
        .iter()
        .position(|info| info.study_body().as_ref() == Some(b))?;
    Some((i, surfaces.solids.get(i)?))
}

// ---- Methods and materials -----------------------------------------------------------------

/// `+X`, `−Y`…
pub(crate) fn direction_label(d: AxisDirection) -> String {
    format!(
        "{}{}",
        if d.is_positive() { "+" } else { "−" },
        axis_label(d.axis())
    )
}

fn axis_label(axis: usize) -> &'static str {
    match axis {
        0 => "X",
        1 => "Y",
        _ => "Z",
    }
}

/// How an outcome is made, in a few words: `Casting −Y`, `2-axis cutting along Z`.
pub(crate) fn method_label(m: &OutcomeMethod) -> String {
    match m {
        OutcomeMethod::Unrestricted => "Unrestricted".to_owned(),
        OutcomeMethod::Additive { direction } => {
            format!("Additive {}", direction_label(*direction))
        }
        OutcomeMethod::Cutting { direction } => {
            format!("2-axis cutting along {}", axis_label(direction.axis()))
        }
        OutcomeMethod::Casting {
            direction,
            parting: None,
        } => format!("Casting {}", direction_label(*direction)),
        OutcomeMethod::Casting {
            direction,
            parting: Some(_),
        } => format!("Casting ±{}", axis_label(direction.axis())),
        OutcomeMethod::Milling { direction, .. } => {
            format!("Milling from {}", direction_label(*direction))
        }
    }
}

/// The engine's method for `m`, or why it cannot make it yet.
pub(crate) fn engine_method(m: &OutcomeMethod) -> Result<Method, String> {
    match *m {
        OutcomeMethod::Unrestricted => Ok(Method::Unrestricted),
        OutcomeMethod::Cutting { direction } => Ok(Method::Cutting {
            axis: direction.axis(),
        }),
        OutcomeMethod::Casting { direction, parting } => Ok(Method::Casting {
            direction: Direction {
                axis: direction.axis(),
                positive: direction.is_positive(),
            },
            parting,
        }),
        OutcomeMethod::Additive { direction } => Ok(Method::Additive {
            direction: Direction {
                axis: direction.axis(),
                positive: direction.is_positive(),
            },
        }),
        OutcomeMethod::Milling {
            direction,
            tool_diameter,
        } => Ok(Method::Milling {
            direction: Direction {
                axis: direction.axis(),
                positive: direction.is_positive(),
            },
            tool_diameter,
        }),
    }
}

/// The materials outcomes are generated for: the study's, else the part's own, each with
/// what the engine takes or why it cannot be used, and its colour.
pub(crate) fn study_materials(
    g: &GenerativeSettings,
    part: Option<&Material>,
) -> Vec<(String, Result<Elastic, String>, [u8; 3])> {
    let of = |m: &Material| (m.name.clone(), elastic_of(m), m.appearance.color);
    if g.materials.is_empty() {
        part.map(of).into_iter().collect()
    } else {
        g.materials.iter().map(of).collect()
    }
}

/// The engine's material for `m`: its mechanical properties (else typical ones for its
/// name), the factor of safety measured against the yield strength, else the tensile one.
pub(crate) fn elastic_of(m: &Material) -> Result<Elastic, String> {
    if !(m.density.is_finite() && m.density > 0.0) {
        return Err(format!("{} has no density", m.name));
    }
    let p = match m.mechanical {
        Some(p) => p,
        None => {
            let e = Elastic::guess(&m.name, &m.category)
                .ok_or_else(|| format!("{} has no known stiffness", m.name))?;
            Mechanical::new(e.youngs_modulus, e.poisson, e.yield_strength, 0.0)
        }
    };
    Ok(Elastic::new(
        p.elastic_modulus,
        p.poisson_ratio,
        p.strength(),
        m.density,
    ))
}

/// What a generation makes: per material, per method, per direction, each with its id
/// (from 1, in that order).
pub(crate) struct OutcomePlan {
    pub id: u32,
    pub material: String,
    pub elastic: Result<Elastic, String>,
    pub method: OutcomeMethod,
}

pub(crate) fn outcome_plan(g: &GenerativeSettings, part: Option<&Material>) -> Vec<OutcomePlan> {
    let mut out = Vec::new();
    for (name, elastic, _) in study_materials(g, part) {
        for method in g.methods.iter().flat_map(GenerativeMethod::outcomes) {
            out.push(OutcomePlan {
                id: out.len() as u32 + 1,
                material: name.clone(),
                elastic: elastic.clone(),
                method,
            });
        }
    }
    out
}

// ---- The engine's study --------------------------------------------------------------------

/// The study's preserve regions (Preserve Region items, as topology studies have them).
pub(crate) fn preserve_items(study: &Study) -> impl Iterator<Item = (&str, f64, &[SimFace])> {
    study.items.iter().filter_map(|i| match i.kind {
        ItemKind::Preserve { depth } => Some((i.name.as_str(), depth, i.faces.as_slice())),
        _ => None,
    })
}

/// A study's design space on `surfaces`, with the preserve bodies' indices into them and
/// the faces the regions keep, by label.
struct Space {
    space: DesignSpace,
    preserve: Vec<usize>,
    kept: Vec<u32>,
}

/// The design space of `study` on `surfaces`: its bodies by role, and each preserve region
/// on the bodies its faces are on.
fn design_space(study: &Study, surfaces: &Surfaces) -> Result<Space, String> {
    let g = &study.generative;
    let body = |id: &StudyBody| -> Result<usize, String> {
        solid_of(surfaces, id)
            .map(|(i, _)| i)
            .ok_or_else(|| format!("Body {id} is no longer in the model: give its role again"))
    };
    let preserve: Vec<usize> = g.preserve.iter().map(body).collect::<Result<_, _>>()?;
    let mut obstacles = Vec::new();
    let mut obstacle_bodies = Vec::new();
    for o in &g.obstacles {
        let i = body(&o.body)?;
        obstacle_bodies.push(i);
        if let Some(s) = surfaces.solids.get(i) {
            obstacles.push(Obstacle {
                surface: s.clone(),
                offset: o.offset.max(0.0),
            });
        }
    }
    let starting_shape = match &g.starting_shape {
        Some(id) => surfaces.solids.get(body(id)?).cloned(),
        None => None,
    };
    // Each region on each body its faces are on; the bodies once each, in order.
    let mut carriers: Vec<usize> = Vec::new();
    let mut regions = Vec::new();
    let mut kept = Vec::new();
    for (name, depth, faces) in preserve_items(study) {
        let labels = surfaces.labels(faces);
        if labels.is_empty() {
            return Err(format!(
                "{name} is on no face of the part any more: select its faces again"
            ));
        }
        let mut by_body: BTreeMap<usize, Vec<u32>> = BTreeMap::new();
        for l in labels {
            if let Some(&b) = surfaces.face_body.get(l as usize) {
                by_body.entry(b as usize).or_default().push(l);
            }
        }
        for (b, labels) in by_body {
            if obstacle_bodies.contains(&b) {
                return Err(format!(
                    "{name} is on an obstacle: material is kept out of it, so none can be kept there"
                ));
            }
            let at = match carriers.iter().position(|&c| c == b) {
                Some(at) => at,
                None => {
                    carriers.push(b);
                    carriers.len() - 1
                }
            };
            kept.extend(&labels);
            regions.push(PreserveRegion {
                body: at,
                labels,
                depth: depth.max(0.0),
            });
        }
    }
    if preserve.is_empty() && regions.is_empty() {
        return Err(
            "Nothing to keep: select a face of each body to keep, then Preserve; or the \
                    faces to keep (holes, mounting faces), then Preserve Region"
                .to_owned(),
        );
    }
    let space = DesignSpace {
        preserve: preserve
            .iter()
            .filter_map(|&i| surfaces.solids.get(i).cloned())
            .collect(),
        regions,
        region_bodies: carriers
            .iter()
            .filter_map(|&i| surfaces.solids.get(i).cloned())
            .collect(),
        obstacles,
        starting_shape,
        margin: g.margin,
    };
    Ok(Space {
        space,
        preserve,
        kept,
    })
}

/// The engine's study for `study` on `surfaces` (the part's, made for the document as it
/// is), with the faces the fixtures and loads are on checked to be kept: preserve bodies'
/// or preserve regions'.
pub(crate) fn engine_study(study: &Study, surfaces: &Surfaces) -> Result<GenerativeStudy, String> {
    let g = &study.generative;
    let Space {
        space,
        preserve,
        kept,
    } = design_space(study, surfaces)?;

    let mut fixtures = Vec::new();
    let mut loads = Vec::new();
    // Their names, in the same order, for the load cases.
    let mut fixture_names: Vec<&str> = Vec::new();
    let mut load_names: Vec<&str> = Vec::new();
    for item in study.items.iter().filter(|i| is_condition(&i.kind)) {
        let labels = surfaces.labels(&item.faces);
        if labels.is_empty() {
            return Err(format!(
                "{} is on no face of the part any more: select its faces again",
                item.name
            ));
        }
        let on_preserve = labels.iter().all(|&l| {
            kept.contains(&l)
                || surfaces
                    .face_body
                    .get(l as usize)
                    .is_some_and(|b| preserve.contains(&(*b as usize)))
        });
        if !on_preserve {
            return Err(format!(
                "{} is on a face that is not kept: put fixtures and loads on preserve bodies, \
                 or make a Preserve Region of their faces first",
                item.name
            ));
        }
        if matches!(item.kind, ItemKind::Fixed | ItemKind::Roller) {
            fixture_names.push(&item.name);
        } else {
            load_names.push(&item.name);
        }
        match item.kind {
            ItemKind::Fixed => fixtures.push(oxyde_sim::Fixture {
                kind: FixtureKind::Fixed,
                labels,
            }),
            ItemKind::Roller => fixtures.push(oxyde_sim::Fixture {
                kind: FixtureKind::Roller,
                labels,
            }),
            ItemKind::Force { newtons, direction } => loads.push(oxyde_sim::Load {
                kind: LoadKind::Force {
                    total: newtons,
                    direction: direction.vector(),
                },
                labels,
            }),
            ItemKind::Pressure { mpa } => loads.push(oxyde_sim::Load {
                kind: LoadKind::Pressure { value: mpa },
                labels,
            }),
            _ => {}
        }
    }
    if fixtures.is_empty() {
        return Err(
            "Nothing holds the design: select faces of a preserve body, then Fixed Geometry"
                .to_owned(),
        );
    }
    if loads.is_empty() {
        return Err(
            "Nothing loads the design: select faces of a preserve body, then Force or Pressure"
                .to_owned(),
        );
    }
    let index = |names: &[String], list: &[&str]| -> Vec<usize> {
        names
            .iter()
            .filter_map(|n| list.iter().position(|x| x == n))
            .collect()
    };
    let mut load_cases = Vec::with_capacity(g.load_cases.len());
    for c in &g.load_cases {
        let case = oxyde_sim::generative::LoadCase {
            fixtures: index(&c.fixtures, &fixture_names),
            loads: index(&c.loads, &load_names),
        };
        if case.fixtures.is_empty() || case.loads.is_empty() {
            return Err(format!(
                "{} carries nothing: tick a fixture and a load for it (Design conditions)",
                c.name
            ));
        }
        load_cases.push(case);
    }
    let objective = match g.objective {
        GenerativeObjective::MaxStiffness => {
            if !(g.mass_target.is_finite() && g.mass_target > 0.0) {
                return Err("Set the mass target (Objectives) first".to_owned());
            }
            Objective::MaxStiffness
        }
        GenerativeObjective::MinMass => Objective::MinMass {
            safety_factor: g.safety_factor,
            displacement_limit: g.displacement_limit,
        },
    };
    let symmetry = engine_symmetry(g.symmetry);
    let voxel_size = match g.resolution {
        MeshSize::ElementSize(h) => Some(h),
        MeshSize::Automatic { density } => voxel_size(&space, symmetry.as_ref(), density),
    }
    .filter(|h| h.is_finite() && *h > 0.0)
    .ok_or_else(|| "The resolution gives no usable voxel size".to_owned())?;
    Ok(GenerativeStudy {
        space,
        fixtures,
        loads,
        load_cases,
        objective,
        mass_target: g.mass_target,
        member_size: g.member_size,
        max_iterations: g.iterations.max(1) as usize,
        voxel_size,
        symmetry,
    })
}

/// Whether an item is a generative study's fixture or load (not a preserve region).
fn is_condition(kind: &ItemKind) -> bool {
    kind.fits(StudyType::Generative) && !matches!(kind, ItemKind::Preserve { .. })
}

fn engine_symmetry(s: Option<SymmetryPlanes>) -> Option<Symmetry> {
    s.filter(|s| s.planes.contains(&true)).map(|s| Symmetry {
        point: s.point,
        planes: s.planes,
    })
}

/// A new fixture or load joins the study's load cases, when it has some: a fixture every
/// case (most hold the part the same way), a load the last case (the one being set up).
pub(crate) fn join_load_cases(study: &mut Study, item: usize) {
    let Some(item) = study.items.get(item).filter(|i| is_condition(&i.kind)) else {
        return;
    };
    let name = item.name.clone();
    let fixture = matches!(item.kind, ItemKind::Fixed | ItemKind::Roller);
    let cases = &mut study.generative.load_cases;
    if fixture {
        for c in cases.iter_mut() {
            c.fixtures.push(name.clone());
        }
    } else if let Some(c) = cases.last_mut() {
        c.loads.push(name);
    }
}

/// A fixture or load deleted leaves the load cases.
pub(crate) fn leave_load_cases(study: &mut Study, name: &str) {
    for c in &mut study.generative.load_cases {
        c.fixtures.retain(|n| n != name);
        c.loads.retain(|n| n != name);
    }
}

/// mm: the voxels' edge the study's resolution gives its design space.
fn study_voxel_size(g: &GenerativeSettings, space: &DesignSpace) -> Option<f64> {
    let h = match g.resolution {
        MeshSize::ElementSize(h) => h,
        MeshSize::Automatic { density } => {
            voxel_size(space, engine_symmetry(g.symmetry).as_ref(), density)?
        }
    };
    (h.is_finite() && h > 0.0).then_some(h)
}

/// mm and voxels: the size the study's resolution gives the voxels, and about how many
/// the design space takes, from the box around its bodies.
pub(crate) fn voxel_estimate(study: &Study, surfaces: &Surfaces) -> Option<(f64, usize)> {
    let g = &study.generative;
    let space = design_space(study, surfaces).ok()?.space;
    let h = study_voxel_size(g, &space)?;
    let symmetry = engine_symmetry(g.symmetry);
    let (lo, hi) = oxyde_sim::generative::design_box(&space, symmetry.as_ref())?;
    let count = lo
        .iter()
        .zip(&hi)
        .map(|(l, u)| ((u - l) / h).ceil().max(1.0))
        .product::<f64>();
    count.is_finite().then_some((h, count as usize))
}

/// How a study's design space is shared out, laid on a grid coarse enough to take a
/// moment: for the panel's warnings, and the least an outcome weighs once regions keep
/// parts of bodies.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Census {
    /// mm: the voxels' edge, the study's or coarser.
    pub(crate) voxel: f64,
    /// Voxels always material.
    pub(crate) preserve: usize,
    /// Voxels material may go in.
    pub(crate) design: usize,
}

/// The most voxels a census lays: some hundredths of a second.
const CENSUS_VOXELS: f64 = 50_000.0;

/// The study's design space laid on its voxels, or coarser ones when those would be more
/// than [`CENSUS_VOXELS`]; `None` until it has something kept.
pub(crate) fn census(study: &Study, surfaces: &Surfaces) -> Option<Census> {
    let g = &study.generative;
    let Space { space, kept, .. } = design_space(study, surfaces).ok()?;
    let symmetry = engine_symmetry(g.symmetry);
    let (lo, hi) = oxyde_sim::generative::design_box(&space, symmetry.as_ref())?;
    let volume: f64 = lo.iter().zip(&hi).map(|(l, u)| (u - l).max(1e-9)).product();
    let h = study_voxel_size(g, &space)?.max((volume / CENSUS_VOXELS).cbrt());
    // The holes kept clear, as the generation keeps them: in the regions' faces, the
    // fixtures' and the loads'.
    let mut holes = kept;
    for item in study.items.iter().filter(|i| is_condition(&i.kind)) {
        holes.extend(surfaces.labels(&item.faces));
    }
    holes.sort_unstable();
    holes.dedup();
    let never = AtomicBool::new(false);
    let grid = Grid::lay(&space, symmetry.as_ref(), h, &holes, 0, &never).ok()?;
    let count = |k: Voxel| grid.kinds.iter().filter(|v| **v == k).count();
    Some(Census {
        voxel: grid.size,
        preserve: count(Voxel::Preserve),
        design: count(Voxel::Design),
    })
}

/// The kept material's mass in a material of `density` (kg/m³), kg: the least an outcome
/// can weigh. The preserve bodies' exactly; with preserve regions, the census's preserve
/// voxels, which measure them as the voxels do.
pub(crate) fn preserve_mass(
    study: &Study,
    surfaces: &Surfaces,
    census: Option<&Census>,
    density: f64,
) -> f64 {
    let volume: f64 = match census {
        Some(c) if preserve_items(study).next().is_some() => c.preserve as f64 * c.voxel.powi(3),
        _ => study
            .generative
            .preserve
            .iter()
            .filter_map(|id| solid_of(surfaces, id).map(|(_, s)| s))
            .map(|s| s.volume().abs())
            .sum(),
    };
    volume * 1e-9 * density
}

/// A surface's area, mm².
fn area_of(s: &SurfaceMesh) -> f64 {
    let p = |i: u32| s.positions.get(i as usize).copied();
    s.triangles
        .iter()
        .filter_map(|t| {
            let ([ax, ay, az], [bx, by, bz], [cx, cy, cz]) = (p(t[0])?, p(t[1])?, p(t[2])?);
            let u = [bx - ax, by - ay, bz - az];
            let v = [cx - ax, cy - ay, cz - az];
            let n = [
                u[1] * v[2] - u[2] * v[1],
                u[2] * v[0] - u[0] * v[2],
                u[0] * v[1] - u[1] * v[0],
            ];
            Some(0.5 * n.iter().map(|x| x * x).sum::<f64>().sqrt())
        })
        .sum()
}

/// What makes a study's outcomes disappoint, said before Generate, one line each: every
/// body kept (nothing to design), the kept material most of the design space, no material
/// to make them of, voxels too coarse for what is kept. `bodies` are the model's, with
/// their names; `materials` the study's ([`study_materials`]); `estimate` the voxels'
/// size and number ([`voxel_estimate`]).
pub(crate) fn setup_warnings(
    study: &Study,
    surfaces: &Surfaces,
    bodies: &[(StudyBody, String)],
    materials: &[(String, Result<Elastic, String>, [u8; 3])],
    estimate: Option<(f64, usize)>,
    census: Option<&Census>,
) -> Vec<String> {
    let g = &study.generative;
    let mut out = Vec::new();
    let all_kept = !bodies.is_empty()
        && g.starting_shape.is_none()
        && bodies.iter().all(|(b, _)| g.preserve.contains(b));
    if all_kept {
        out.push(
            "Every body is kept as it is: the outcomes can only add material around them. \
             To redesign a part, give it the Starting shape role (or none), and keep its holes \
             and mounting faces with Preserve Region instead"
                .to_owned(),
        );
    } else if let Some(c) = census {
        let share = c.preserve as f64 / (c.preserve + c.design).max(1) as f64;
        if share >= 0.5 {
            out.push(format!(
                "What is kept is {:.0} % of the design space: little is left to design. Keep \
                 less (shallower regions, fewer bodies), or give the material more room",
                100.0 * share
            ));
        }
    }
    if materials.is_empty() {
        out.push("No material: add one under Materials, or apply one to the part".to_owned());
    }
    for (_, elastic, _) in materials {
        if let Err(why) = elastic {
            out.push(format!("{why}: no outcome can be made of it"));
        }
    }
    if let Some((h, _)) = estimate {
        let coarse = |what: String| {
            format!(
                "The voxels ({} mm) are coarse for {what}: fewer than two across it. Raise \
                 the resolution (Resolution)",
                format_number_short(h)
            )
        };
        for (name, depth, _) in preserve_items(study) {
            if depth > 0.0 && depth < 2.0 * h {
                out.push(coarse(format!("{name} ({depth} mm deep)")));
            }
        }
        // A body's thickness, as a plate's: twice its volume over its area.
        for id in &g.preserve {
            let Some((_, s)) = solid_of(surfaces, id) else {
                continue;
            };
            let thickness = 2.0 * s.volume().abs() / area_of(s).max(1e-12);
            if thickness < h {
                let name = bodies
                    .iter()
                    .find(|(b, _)| b == id)
                    .map_or_else(|| id.to_string(), |(_, n)| n.clone());
                out.push(coarse(format!(
                    "{name} (about {} mm thick)",
                    format_number_short(thickness)
                )));
            }
        }
    }
    out
}

// ---- Outcomes ------------------------------------------------------------------------------

/// The outcome the engine gave, as the files keep it.
pub(crate) fn outcome_of(
    plan: &OutcomePlan,
    o: oxyde_sim::generative::Outcome,
    seconds: f64,
) -> GenerativeOutcome {
    GenerativeOutcome {
        id: plan.id,
        material: plan.material.clone(),
        method: plan.method,
        status: if o.stopped {
            OutcomeStatus::Stopped
        } else if o.converged {
            OutcomeStatus::Converged
        } else {
            OutcomeStatus::Limit
        },
        iterations: o.history.len() as u32,
        seconds,
        mass: o.mass,
        compliance: o.compliance,
        max_displacement: o.max_displacement,
        max_von_mises: o.max_von_mises,
        min_factor_of_safety: o.min_factor_of_safety,
        history: o
            .history
            .iter()
            .map(|s| GenerativeIteration {
                compliance: s.compliance,
                mass: s.mass,
                change: s.change,
            })
            .collect(),
        density: o.density,
        snapshots: o
            .snapshots
            .into_iter()
            .map(|(iteration, density)| DensitySnapshot {
                iteration: iteration as u32,
                density,
            })
            .collect(),
    }
}

/// An outcome that could not be generated, and why.
pub(crate) fn failed_outcome(plan: &OutcomePlan, why: String, seconds: f64) -> GenerativeOutcome {
    GenerativeOutcome {
        id: plan.id,
        material: plan.material.clone(),
        method: plan.method,
        status: OutcomeStatus::Failed(why),
        iterations: 0,
        seconds,
        mass: 0.0,
        compliance: 0.0,
        max_displacement: 0.0,
        max_von_mises: 0.0,
        min_factor_of_safety: None,
        history: Vec::new(),
        density: Vec::new(),
        snapshots: Vec::new(),
    }
}

/// A generative study's outcomes, with the grid they were grown on.
pub(crate) struct Generated {
    /// Tells one generation from the next, for the viewport.
    pub(crate) id: u64,
    /// The document's revision they were generated for (or found to hold for).
    pub(crate) revision: u64,
    pub(crate) grid: Grid,
    pub(crate) outcomes: Vec<GenerativeOutcome>,
    /// The model and the study as they were generated for.
    pub(crate) fingerprint: Option<Fingerprint>,
    /// s, every outcome's together.
    pub(crate) seconds: f64,
    /// The graphics card the solves ran on, when one did.
    pub(crate) card: Option<String>,
}

impl Generated {
    pub(crate) fn outcome(&self, id: u32) -> Option<&GenerativeOutcome> {
        self.outcomes.iter().find(|o| o.id == id)
    }
}

/// The grid as the files keep it.
fn voxel_grid(grid: &Grid) -> VoxelGrid {
    VoxelGrid {
        origin: grid.origin,
        size: grid.size,
        counts: grid.counts.map(|n| n as u32),
        voxels: grid
            .kinds
            .iter()
            .map(|k| match k {
                Voxel::Design => VoxelKind::Design,
                Voxel::Preserve => VoxelKind::Preserve,
                Voxel::Obstacle => VoxelKind::Obstacle,
                Voxel::Outside => VoxelKind::Outside,
            })
            .collect(),
    }
}

/// `generated` as a `.oxyres` holds it.
pub(crate) fn generative_results_file(
    generated: &Generated,
    simulation_id: &str,
    study: &str,
    fingerprint: Fingerprint,
) -> ResultsFile {
    let iterations: u64 = generated
        .outcomes
        .iter()
        .map(|o| u64::from(o.iterations))
        .sum();
    ResultsFile {
        document_id: oxyde_format::new_document_id(),
        simulation_id: simulation_id.to_owned(),
        study: study.to_owned(),
        study_type: StudyType::Generative,
        fingerprint,
        summary: Summary::default(),
        solver: SolverInfo {
            program: format!("oxyde {}", env!("CARGO_PKG_VERSION")),
            mesher: "voxels".to_owned(),
            method: match &generated.card {
                Some(card) => format!("simp+oc, multigrid pcg on {card}"),
                None => "simp+oc, multigrid pcg".to_owned(),
            },
            iterations,
            residual: 0.0,
            seconds: generated.seconds,
        },
        bodies: Vec::new(),
        labels: BTreeMap::new(),
        mesh: ResultMesh::default(),
        fields: NodalFields::default(),
        thermal: None,
        topology: None,
        generative: Some(GenerativeResults {
            grid: voxel_grid(&generated.grid),
            outcomes: generated.outcomes.clone(),
        }),
    }
}

/// The outcomes a `.oxyres` holds, for the document's `revision`; `None` when it holds
/// none or its grid is not one.
pub(crate) fn generated_from(file: &ResultsFile, revision: u64) -> Option<Generated> {
    let g = file.generative.as_ref()?;
    let kinds = g
        .grid
        .voxels
        .iter()
        .map(|k| match k {
            VoxelKind::Design => Voxel::Design,
            VoxelKind::Preserve => Voxel::Preserve,
            VoxelKind::Obstacle => Voxel::Obstacle,
            VoxelKind::Outside => Voxel::Outside,
        })
        .collect();
    let grid = Grid::from_parts(
        g.grid.origin,
        g.grid.size,
        g.grid.counts.map(|n| n as usize),
        kinds,
    )?;
    Some(Generated {
        id: crate::document::next_revision(),
        revision,
        grid,
        outcomes: g.outcomes.clone(),
        fingerprint: Some(file.fingerprint.clone()),
        seconds: file.solver.seconds,
        card: None,
    })
}

/// What the explore plot's vertical axis shows; the horizontal one is the mass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum ScatterAxis {
    #[default]
    MaxStress,
    Compliance,
    Displacement,
    Safety,
}

impl ScatterAxis {
    const ALL: [ScatterAxis; 4] = [
        ScatterAxis::MaxStress,
        ScatterAxis::Compliance,
        ScatterAxis::Displacement,
        ScatterAxis::Safety,
    ];

    fn label(self) -> &'static str {
        match self {
            ScatterAxis::MaxStress => "Max von Mises stress",
            ScatterAxis::Compliance => "Compliance (lower is stiffer)",
            ScatterAxis::Displacement => "Max displacement",
            ScatterAxis::Safety => "Min factor of safety",
        }
    }

    fn unit(self) -> &'static str {
        match self {
            ScatterAxis::MaxStress => "MPa",
            ScatterAxis::Compliance => "N·mm",
            ScatterAxis::Displacement => "mm",
            ScatterAxis::Safety => "",
        }
    }

    /// The outcome's value; `None` for a failed one or a safety factor unknown.
    pub(crate) fn value(self, o: &GenerativeOutcome) -> Option<f64> {
        if matches!(o.status, OutcomeStatus::Failed(_)) {
            return None;
        }
        match self {
            ScatterAxis::MaxStress => Some(o.max_von_mises),
            ScatterAxis::Compliance => Some(o.compliance),
            ScatterAxis::Displacement => Some(o.max_displacement),
            ScatterAxis::Safety => o.min_factor_of_safety,
        }
        .filter(|v| v.is_finite())
    }
}

/// `lo` to `hi` widened a little, never empty, for a plot's axis.
fn padded(lo: f64, hi: f64) -> (f64, f64) {
    let span = hi - lo;
    let pad = if span > 0.0 {
        0.08 * span
    } else {
        0.1 * lo.abs().max(1e-9)
    };
    (lo - pad, hi + pad)
}

// ---- The scene -----------------------------------------------------------------------------

/// The densities of outcome `o` at `snapshot` (an index into its snapshots), or at its
/// end.
pub(crate) fn densities(o: &GenerativeOutcome, snapshot: Option<usize>) -> &[u8] {
    match snapshot.and_then(|i| o.snapshots.get(i)) {
        Some(s) => &s.density,
        None => &o.density,
    }
}

fn face_mesh(positions: &[[f64; 3]], triangles: &[[u32; 3]]) -> FaceMesh {
    FaceMesh {
        positions: positions.iter().map(|p| p.map(|v| v as f32)).collect(),
        triangles: triangles.to_vec(),
    }
}

/// The scene of an outcome: its surface and the preserve bodies exact over it, in
/// `color`, and the obstacles faint around it.
pub(crate) fn outcome_scene(
    shape: &Shape,
    preserve: &[&SurfaceMesh],
    obstacles: &[&SurfaceMesh],
    color: [u8; 3],
) -> Scene {
    let mut faces = vec![face_mesh(&shape.positions, &shape.triangles)];
    faces.extend(
        preserve
            .iter()
            .map(|s| face_mesh(&s.positions, &s.triangles)),
    );
    let solid = faces.len();
    faces.extend(
        obstacles
            .iter()
            .map(|s| face_mesh(&s.positions, &s.triangles)),
    );
    let mut materials = vec![
        RenderMaterial {
            color: Some(color),
            ..RenderMaterial::default()
        };
        solid
    ];
    materials.extend(std::iter::repeat_n(
        RenderMaterial {
            color: Some(OBSTACLE_COLOR),
            opacity: GHOST_OPACITY,
            ..RenderMaterial::default()
        },
        faces.len() - solid,
    ));
    let mut scene = Scene::from_mesh(&Mesh {
        faces,
        edges: Vec::new(),
    });
    scene.set_materials(materials, Vec::new());
    scene
}

// ---- The job -------------------------------------------------------------------------------

/// What the generation thread takes.
struct GenInput {
    study: GenerativeStudy,
    plan: Vec<OutcomePlan>,
    gpu: bool,
}

/// What the generation thread has done so far, read by the tab every frame.
#[derive(Default)]
struct GenShared {
    stage: String,
    fraction: f64,
    grid: Option<Grid>,
    /// Outcomes finished and not yet taken by the tab.
    outcomes: Vec<GenerativeOutcome>,
    card: Option<String>,
    /// A note for the status bar: the graphics card unused, and why.
    note: Option<String>,
    /// The thread is done: with every outcome it could make, or why it made none.
    finished: Option<Result<(), String>>,
}

/// A generation running on its thread.
pub(crate) struct GenJob {
    cancel: Arc<AtomicBool>,
    shared: Arc<Mutex<GenShared>>,
    revision: u64,
    started: Instant,
    fingerprint: Option<Fingerprint>,
    total: usize,
}

impl Drop for GenJob {
    /// The thread notices at its next check and ends; nothing waits for it.
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

impl GenJob {
    /// Where it is: a line and the fraction done.
    pub(crate) fn progress(&self) -> (String, f64) {
        self.shared.lock().map_or(("Working".to_owned(), 0.0), |s| {
            (s.stage.clone(), s.fraction)
        })
    }
}

fn set_progress(shared: &Mutex<GenShared>, stage: Option<String>, fraction: f64) {
    if let Ok(mut s) = shared.lock() {
        if let Some(stage) = stage {
            s.stage = stage;
        }
        s.fraction = fraction.clamp(0.0, 1.0);
    }
}

/// Lays the design space on a grid, then grows each planned outcome in turn, on the
/// graphics card when asked and there is one (the CPU then for an outcome the card fails
/// on), handing each to `shared` as it is done.
fn generation_thread(input: GenInput, shared: &Mutex<GenShared>, cancel: &AtomicBool) {
    let finish = |result: Result<(), String>| {
        if let Ok(mut s) = shared.lock() {
            s.finished = Some(result);
        }
    };
    set_progress(shared, Some("Laying out the grid".to_owned()), 0.0);
    let grid = match Grid::build(&input.study, 0, cancel) {
        Ok(g) => g,
        Err(GenerativeError::Cancelled) => return finish(Ok(())),
        Err(e) => return finish(Err(capitalized(&e.to_string()))),
    };
    if let Ok(mut s) = shared.lock() {
        s.grid = Some(grid.clone());
    }
    let mut card: Option<oxyde_sim_gpu::GpuBackend> = None;
    if input.gpu {
        set_progress(shared, Some("Finding the graphics card".to_owned()), 0.0);
        match oxyde_sim_gpu::GpuBackend::new() {
            Ok(b) => {
                if let Ok(mut s) = shared.lock() {
                    s.card = Some(b.name().to_owned());
                }
                card = Some(b);
            }
            Err(e) => {
                if let Ok(mut s) = shared.lock() {
                    s.note = Some(format!("generated on the CPU: {e}"));
                }
            }
        }
    }
    let options = SolveOptions::default();
    let total = input.plan.len().max(1) as f64;
    for (k, plan) in input.plan.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let label = format!(
            "{}, {} ({} of {})",
            plan.material,
            method_label(&plan.method).to_lowercase(),
            k + 1,
            input.plan.len()
        );
        set_progress(shared, Some(label), k as f64 / total);
        let start = Instant::now();
        let progress = |f: f64| set_progress(shared, None, (k as f64 + f) / total);
        let method = engine_method(&plan.method);
        let result = match (&plan.elastic, &method) {
            (Err(why), _) | (_, Err(why)) => Err(why.clone()),
            (Ok(elastic), Ok(method)) => {
                let method = *method;
                let on_card = card.as_mut().map(|b| {
                    generate_on(
                        &grid,
                        &input.study,
                        method,
                        elastic,
                        &options,
                        Some(b as &mut dyn Backend),
                        cancel,
                        &progress,
                    )
                });
                match on_card {
                    // A card that fails is not used again: the CPU makes this outcome and
                    // the next ones.
                    Some(Err(GenerativeError::Solver(why))) => {
                        card = None;
                        if let Ok(mut s) = shared.lock() {
                            s.card = None;
                            s.note = Some(format!(
                                "the graphics card failed ({why}); generated on the CPU"
                            ));
                        }
                        generate_on(
                            &grid,
                            &input.study,
                            method,
                            elastic,
                            &options,
                            None,
                            cancel,
                            &progress,
                        )
                        .map_err(|e| e.to_string())
                    }
                    Some(r) => r.map_err(|e| e.to_string()),
                    None => generate_on(
                        &grid,
                        &input.study,
                        method,
                        elastic,
                        &options,
                        None,
                        cancel,
                        &progress,
                    )
                    .map_err(|e| e.to_string()),
                }
            }
        };
        let seconds = start.elapsed().as_secs_f64();
        let outcome = match result {
            Ok(o) => outcome_of(plan, o, seconds),
            // Stopped before its first iteration: nothing to keep.
            Err(_) if cancel.load(Ordering::Relaxed) => break,
            Err(why) => failed_outcome(plan, capitalized(&why), seconds),
        };
        if let Ok(mut s) = shared.lock() {
            s.outcomes.push(outcome);
        }
    }
    set_progress(shared, None, 1.0);
    finish(Ok(()));
}

fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

// ---- The tab's state -----------------------------------------------------------------------

/// What an outcome's surface is made from: the generation, the outcome, the iteration.
type SurfaceKey = (u64, u32, Option<usize>);

/// What the panel shows of a study's setup: the voxels' size and number
/// ([`voxel_estimate`]) and the [`census`].
type Setup = (Option<(f64, usize)>, Option<Census>);

/// What is shown in the viewport for an outcome: anything else, and it is drawn again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ShownKey {
    generated: u64,
    outcome: u32,
    snapshot: Option<usize>,
    obstacles: bool,
}

/// The Generative Design tab's state beside the open study.
#[derive(Default)]
pub(crate) struct GenView {
    /// The open study's outcomes.
    pub(crate) generated: Option<Generated>,
    pub(crate) job: Option<GenJob>,
    /// The outcome picked (its id).
    pub(crate) picked: Option<u32>,
    /// The iteration of it shown: an index into its snapshots, or `None` for its end.
    pub(crate) snapshot: Option<usize>,
    /// The picked outcome is drawn in the viewport in place of the model.
    pub(crate) show: bool,
    /// The obstacles drawn faintly around it.
    pub(crate) show_obstacles: bool,
    pub(crate) y_axis: ScatterAxis,
    pub(crate) error: Option<String>,
    shown: Option<ShownKey>,
    /// The roles the viewport's colours were made for.
    roles_shown: Option<u64>,
    /// The last outcome surface made, by what it was made from.
    surface: Option<(SurfaceKey, Shape)>,
    /// The study whose saved outcomes were looked for, so they are looked for once.
    looked_for: Option<String>,
    /// The voxels the resolution gives and the census, by what they were worked out from.
    setup: Option<(u64, Setup)>,
    /// The part file *Create Part* made last, to open.
    pub(crate) created: Option<PathBuf>,
}

impl GenView {
    /// Forgets the outcomes' view state: another study's, or none.
    pub(crate) fn clear_view(&mut self) {
        self.picked = None;
        self.snapshot = None;
        self.show = false;
        self.surface = None;
        self.error = None;
    }

    /// Whether something was shown in the viewport, which is taken back.
    pub(crate) fn take_shown(&mut self) -> bool {
        self.shown.take().is_some()
    }
}

/// What the panel asks for, done once it is drawn.
#[derive(Debug, Clone, Copy, PartialEq)]
enum GenAction {
    Generate,
    Stop,
    Pick(u32),
    Show(bool),
    Export,
    CreatePart,
    /// Open the part *Create Part* made.
    OpenCreated,
    OpenStudy,
    SaveStudy,
    OpenResults,
    SaveResults,
    /// A value of the study changed.
    Edited,
}

// ---- The application side ------------------------------------------------------------------

impl OxydeApp {
    /// Whether the Generative Design tab is the one open.
    pub(crate) fn generative_tab_open(&self) -> bool {
        match self.asm {
            Some(_) => self.asm_tab == crate::assembly_ui::AsmTab::Generative,
            None => self.tab == crate::app::CommandTab::Generative,
        }
    }

    /// Whether the open study is a generative study.
    pub(crate) fn generative_study_open(&self) -> bool {
        self.simulation.started && self.simulation.study.study_type == StudyType::Generative
    }

    /// Whether the tab works on the open study now: a part's generative study in its tab.
    fn generative_active(&self) -> bool {
        self.generative_tab_open() && self.generative_study_open()
    }

    /// Every frame: the generation's progress, the viewport's colours, the outcomes saved
    /// with the study read when it opens.
    pub(crate) fn generative_housekeeping(&mut self, ctx: &egui::Context) {
        self.poll_generation(ctx);
        if self.simulation.generative.shown.is_some() && !self.outcome_wanted() {
            self.restore_simulation_scene();
            ctx.request_repaint();
        }
        // The bodies coloured by role while the tab is set up, the model's own colours
        // otherwise. Not while an outcome is drawn: its scene would be replaced.
        let roles = self
            .generative_active()
            .then(|| colours_hash(&self.simulation.study));
        if self.simulation.generative.shown.is_none()
            && roles != self.simulation.generative.roles_shown
        {
            self.simulation.generative.roles_shown = roles;
            self.scene_revision = None;
            self.asm_scene = None;
            ctx.request_repaint();
        }
        self.load_saved_outcomes();
    }

    /// The bodies' faces in the colours of their roles, and the faces of the preserve
    /// regions in the preserve bodies' green, while the tab is set up: called with the
    /// materials of the model's faces (empty for the default everywhere).
    pub(crate) fn tint_generative_roles(&self, materials: &mut Vec<RenderMaterial>) {
        if !self.generative_active() {
            return;
        }
        let g = &self.simulation.study.generative;
        let paint = |m: &mut RenderMaterial, role: Role| {
            if let Some(color) = role.color() {
                *m = RenderMaterial {
                    color: Some(color),
                    ..RenderMaterial::default()
                };
            }
        };
        let kept = || preserve_items(&self.simulation.study).flat_map(|(_, _, faces)| faces);
        match &self.asm {
            None => {
                let Some(body) = self.shown_body() else {
                    return;
                };
                let faces = body.topology.faces.len();
                if materials.len() < faces {
                    materials.resize(faces, RenderMaterial::default());
                }
                for (i, m) in materials.iter_mut().enumerate().take(faces) {
                    let role = body
                        .solid_of_face(i as u32)
                        .map_or(Role::None, |s| role_of(g, &StudyBody::part(s.id)));
                    paint(m, role);
                }
                for f in kept().filter(|f| f.occurrence.is_none()) {
                    if let Some(m) = body
                        .naming
                        .find_face(&f.face)
                        .and_then(|i| materials.get_mut(i as usize))
                    {
                        paint(m, Role::Preserve);
                    }
                }
            }
            Some(doc) => {
                // Each part occurrence's faces, by the scene's ranges.
                for r in &doc.ranges {
                    let Some(body) = doc.part_at(&r.owner).and_then(|p| p.part.body()) else {
                        continue;
                    };
                    for (local, index) in r.faces.clone().enumerate() {
                        let role = body.solid_of_face(local as u32).map_or(Role::None, |s| {
                            let b = StudyBody::component(r.owner.top, r.owner.path.clone(), s.id);
                            role_of(g, &b)
                        });
                        if let Some(m) = materials.get_mut(index as usize) {
                            paint(m, role);
                        }
                    }
                    for f in kept().filter(|f| f.occurrence.as_ref() == Some(&r.owner)) {
                        if let Some(m) = body
                            .naming
                            .find_face(&f.face)
                            .and_then(|i| materials.get_mut((r.faces.start + i) as usize))
                        {
                            paint(m, Role::Preserve);
                        }
                    }
                }
            }
        }
    }

    /// What the panel and the messages call body `b`: `Body 2`, or in an assembly the
    /// occurrence's name and the body's number in its part.
    pub(crate) fn study_body_label(&self, b: &StudyBody) -> String {
        match (&self.asm, b.component) {
            (Some(doc), Some(top)) => {
                let o = crate::assembly::Occurrence {
                    top,
                    path: b.path.clone(),
                };
                let number = doc
                    .part_at(&o)
                    .and_then(|p| body_number(&p.part, b.body))
                    .map_or_else(String::new, |n| format!(" body {n}"));
                format!("{}{number}", doc.occurrence_name(&o))
            }
            _ => self.body_label(b.body),
        }
    }

    /// The bodies of the selected faces given `role`; taken from them when they all have
    /// it already.
    pub(crate) fn assign_role(&mut self, role: Role) {
        if !self.generative_study_open() || self.generative_running() {
            return;
        }
        let mut bodies: Vec<StudyBody> = self
            .selected_sim_faces()
            .iter()
            .filter_map(|f| match (&f.occurrence, &self.asm) {
                (None, None) => self.body_of_face(&f.face).map(StudyBody::part),
                (Some(o), Some(doc)) => {
                    let body = doc.part_at(o)?.part.body()?;
                    let id = body.solid_of_face(body.naming.find_face(&f.face)?)?.id;
                    Some(StudyBody::component(o.top, o.path.clone(), id))
                }
                _ => None,
            })
            .collect();
        bodies.sort();
        bodies.dedup();
        if bodies.is_empty() {
            self.status = format!(
                "{}: select a face of each body first, then click it",
                role.label()
            );
            return;
        }
        let g = &mut self.simulation.study.generative;
        let all = bodies.iter().all(|b| role_of(g, b) == role);
        let target = if all { Role::None } else { role };
        if target == Role::StartingShape && bodies.len() > 1 {
            self.status = "A study has one starting shape: select a face of one body".to_owned();
            return;
        }
        for b in &bodies {
            set_role(g, b.clone(), target);
        }
        let names: Vec<String> = bodies.iter().map(|b| self.study_body_label(b)).collect();
        self.status = if all {
            format!(
                "{} no longer {}",
                names.join(", "),
                role.label().to_lowercase()
            )
        } else {
            format!("{}: {}", names.join(", "), role.label().to_lowercase())
        };
        self.simulation.generative.show = false;
    }

    /// Whether a generation is running.
    pub(crate) fn generative_running(&self) -> bool {
        self.simulation.generative.job.is_some()
    }

    /// Generate: the outcomes of the open study, on a thread.
    pub(crate) fn start_generation(&mut self) {
        self.simulation.generative.error = None;
        if self.generative_running() || self.simulation.job.is_some() {
            return;
        }
        if let Err(e) = self.ensure_surfaces() {
            self.simulation.generative.error = Some(e);
            return;
        }
        let Some(surfaces) = self.simulation.surfaces.as_ref() else {
            return;
        };
        let study = &self.simulation.study;
        let engine = match engine_study(study, surfaces) {
            Ok(s) => s,
            Err(e) => {
                self.status = e.clone();
                self.simulation.generative.error = Some(e);
                return;
            }
        };
        let plan = outcome_plan(&study.generative, self.doc.part.material());
        if plan.is_empty() {
            let e = if study.generative.methods.is_empty() {
                "Pick a way of making it (Manufacturing) first"
            } else {
                "The part has no material: add one to the study (Materials) or apply one"
            };
            self.simulation.generative.error = Some(e.to_owned());
            self.status = e.to_owned();
            return;
        }
        let fingerprint = self.simulation_fingerprint().ok();
        let total = plan.len();
        let input = GenInput {
            study: engine,
            plan,
            gpu: self.prefs.simulation.generative_gpu,
        };
        let cancel = Arc::new(AtomicBool::new(false));
        let shared = Arc::new(Mutex::new(GenShared::default()));
        let (thread_cancel, thread_shared) = (cancel.clone(), shared.clone());
        let spawned = std::thread::Builder::new()
            .name("generative".to_owned())
            .spawn(move || {
                let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    generation_thread(input, &thread_shared, &thread_cancel);
                }));
                if caught.is_err()
                    && let Ok(mut s) = thread_shared.lock()
                {
                    s.finished =
                        Some(Err("The generation stopped on an internal error".to_owned()));
                }
            });
        if let Err(e) = spawned {
            self.simulation.generative.error = Some(format!("Could not start the generation: {e}"));
            return;
        }
        self.restore_simulation_scene();
        let revision = self.document_revision();
        let view = &mut self.simulation.generative;
        view.generated = None;
        view.clear_view();
        view.job = Some(GenJob {
            cancel,
            shared,
            revision,
            started: Instant::now(),
            fingerprint,
            total,
        });
        self.status = format!("Generating {total} outcome(s)…");
    }

    /// Stop: the outcome being grown kept as far as it got, the next ones not made.
    pub(crate) fn stop_generation(&mut self) {
        if let Some(job) = &self.simulation.generative.job {
            job.cancel.store(true, Ordering::Relaxed);
            self.status = "Stopping: the outcome being grown is kept as far as it got".to_owned();
        }
    }

    fn poll_generation(&mut self, ctx: &egui::Context) {
        let Some(job) = self.simulation.generative.job.as_ref() else {
            return;
        };
        let (grid, outcomes, card, note, finished) = match job.shared.lock() {
            Ok(mut s) => (
                s.grid.take(),
                std::mem::take(&mut s.outcomes),
                s.card.clone(),
                s.note.clone(),
                s.finished.take(),
            ),
            Err(_) => (
                None,
                Vec::new(),
                None,
                None,
                Some(Err("The generation thread stopped".to_owned())),
            ),
        };
        let (revision, fingerprint, started, total) = (
            job.revision,
            job.fingerprint.clone(),
            job.started,
            job.total,
        );
        let view = &mut self.simulation.generative;
        if let Some(grid) = grid {
            view.generated = Some(Generated {
                id: crate::document::next_revision(),
                revision,
                grid,
                outcomes: Vec::new(),
                fingerprint,
                seconds: 0.0,
                card: None,
            });
        }
        if let Some(g) = view.generated.as_mut() {
            let first = g.outcomes.is_empty() && !outcomes.is_empty();
            g.outcomes.extend(outcomes);
            g.card = card;
            g.seconds = started.elapsed().as_secs_f64();
            // The first outcome shows as soon as it is there.
            if first
                && let Some(o) = g
                    .outcomes
                    .iter()
                    .find(|o| !matches!(o.status, OutcomeStatus::Failed(_)))
            {
                view.picked = Some(o.id);
                view.show = true;
            }
        }
        let Some(finished) = finished else {
            ctx.request_repaint_after(Duration::from_millis(150));
            return;
        };
        self.simulation.generative.job = None;
        match finished {
            Err(why) => {
                self.status = why.clone();
                self.simulation.generative.error = Some(why);
                self.simulation.generative.generated = None;
            }
            Ok(()) => {
                let made = self
                    .simulation
                    .generative
                    .generated
                    .as_ref()
                    .map_or(0, |g| {
                        g.outcomes
                            .iter()
                            .filter(|o| !matches!(o.status, OutcomeStatus::Failed(_)))
                            .count()
                    });
                let seconds = started.elapsed().as_secs_f64();
                self.status = format!("{made} of {total} outcome(s) generated in {seconds:.0} s");
                if let Some(note) = note {
                    self.status = format!("{} ({note})", self.status);
                }
                // A saved study keeps its outcomes beside it, and is saved with them.
                if made > 0 && self.simulation.file.is_some() {
                    let status = self.status.clone();
                    if self.save_study(false) && self.save_results(false) {
                        self.status = format!("{status}; outcomes saved");
                    }
                }
            }
        }
        ctx.request_repaint();
    }

    /// The outcomes saved beside the open study, read once when it opens in the tab, if
    /// the model and the study are as they were generated for.
    fn load_saved_outcomes(&mut self) {
        if !self.generative_active()
            || self.simulation.generative.generated.is_some()
            || self.generative_running()
        {
            return;
        }
        let name = self.simulation.study.name.clone();
        if self.simulation.generative.looked_for.as_ref() == Some(&name) {
            return;
        }
        self.simulation.generative.looked_for = Some(name.clone());
        let Some(file) = &self.simulation.file else {
            return;
        };
        let path = crate::simulation_files::results_path(&file.path, &name);
        if !path.is_file() {
            return;
        }
        let Ok(results) = oxyde_format::results::load_results(&path) else {
            return;
        };
        if results.simulation_id != file.document_id || results.study != name {
            return;
        }
        let Ok(now) = self.simulation_fingerprint() else {
            return;
        };
        if !results.fingerprint.changes(&now).is_empty() {
            self.status = format!(
                "The outcomes saved for {name} are out of date: the model or the study changed. Generate again."
            );
            return;
        }
        if let Some(g) = generated_from(&results, self.document_revision()) {
            self.simulation.generative.generated = Some(g);
        }
    }

    /// Whether the picked outcome should be drawn now.
    fn outcome_wanted(&self) -> bool {
        let view = &self.simulation.generative;
        self.generative_active()
            && view.show
            && self.sketch.is_none()
            && view.generated.as_ref().is_some_and(|g| {
                g.revision == self.document_revision()
                    && view
                        .picked
                        .and_then(|id| g.outcome(id))
                        .is_some_and(|o| !o.density.is_empty())
            })
    }

    /// The picked outcome in the viewport, when it is wanted: its surface, the preserve
    /// bodies exact over it, the obstacles faint.
    pub(crate) fn generative_scene(&mut self) {
        if !self.outcome_wanted() {
            return;
        }
        let view = &self.simulation.generative;
        let (Some(g), Some(id)) = (view.generated.as_ref(), view.picked) else {
            return;
        };
        let key = ShownKey {
            generated: g.id,
            outcome: id,
            snapshot: view.snapshot,
            obstacles: view.show_obstacles,
        };
        if view.shown == Some(key) {
            return;
        }
        let Some(shape) = self.outcome_shape() else {
            return;
        };
        if self.ensure_surfaces().is_err() {
            return;
        }
        let Some(surfaces) = self.simulation.surfaces.as_ref() else {
            return;
        };
        let study = &self.simulation.study.generative;
        let solid = |id: &StudyBody| solid_of(surfaces, id).map(|(_, s)| s);
        let preserve: Vec<&SurfaceMesh> = study.preserve.iter().filter_map(solid).collect();
        let obstacles: Vec<&SurfaceMesh> = if key.obstacles {
            study
                .obstacles
                .iter()
                .filter_map(|o| solid(&o.body))
                .collect()
        } else {
            Vec::new()
        };
        let color = self
            .simulation
            .generative
            .generated
            .as_ref()
            .and_then(|g| g.outcome(id))
            .map_or(OUTCOME_COLOR, |o| self.material_color(&o.material));
        let scene = outcome_scene(&shape, &preserve, &obstacles, color);
        self.show_scene(scene);
        self.simulation.generative.shown = Some(key);
    }

    /// [`voxel_estimate`] and [`census`], worked out again only when what they depend on
    /// changed: the bodies, their roles, the regions, the faces held and loaded (whose
    /// holes are kept clear), the margin, the resolution and the symmetry.
    fn cached_setup(&mut self) -> Setup {
        let Some(surfaces) = self.simulation.surfaces.as_ref() else {
            return (None, None);
        };
        let study = &self.simulation.study;
        let g = &study.generative;
        let mut h = DefaultHasher::new();
        surfaces.revision.hash(&mut h);
        roles_hash(g).hash(&mut h);
        for item in study
            .items
            .iter()
            .filter(|i| i.kind.fits(StudyType::Generative))
        {
            let depth = match item.kind {
                ItemKind::Preserve { depth } => Some(depth.to_bits()),
                _ => None,
            };
            (depth, surfaces.labels(&item.faces)).hash(&mut h);
        }
        g.margin.map(|m| m.map(f64::to_bits)).hash(&mut h);
        match g.resolution {
            MeshSize::Automatic { density } => (0u8, density.to_bits()),
            MeshSize::ElementSize(size) => (1u8, size.to_bits()),
        }
        .hash(&mut h);
        g.symmetry
            .map(|s| (s.planes, s.point.map(f64::to_bits)))
            .hash(&mut h);
        let key = h.finish();
        if let Some((k, setup)) = self.simulation.generative.setup
            && k == key
        {
            return setup;
        }
        let setup = (voxel_estimate(study, surfaces), census(study, surfaces));
        self.simulation.generative.setup = Some((key, setup));
        setup
    }

    /// The colour of the study material `name`: its appearance's.
    fn material_color(&self, name: &str) -> [u8; 3] {
        study_materials(&self.simulation.study.generative, self.doc.part.material())
            .into_iter()
            .find(|(n, _, _)| n == name)
            .map_or(OUTCOME_COLOR, |(_, _, c)| c)
    }

    /// The picked outcome's surface at the iteration shown, made once.
    fn outcome_shape(&mut self) -> Option<Shape> {
        let view = &mut self.simulation.generative;
        let g = view.generated.as_ref()?;
        let id = view.picked?;
        let key = (g.id, id, view.snapshot);
        if let Some((k, shape)) = &view.surface
            && *k == key
        {
            return Some(shape.clone());
        }
        let o = g.outcome(id)?;
        let shape = surface(&g.grid, densities(o, view.snapshot), SMOOTHING);
        view.surface = Some((key, shape.clone()));
        Some(shape)
    }

    /// Export: the picked outcome's surface as STL, OBJ or 3MF, by the name chosen.
    pub(crate) fn export_outcome(&mut self) {
        let Some(shape) = self.outcome_shape() else {
            self.status = "Nothing to export: pick an outcome first".to_owned();
            return;
        };
        if shape.triangles.is_empty() {
            self.status = "The outcome has no material to export".to_owned();
            return;
        }
        let view = &self.simulation.generative;
        let name = view
            .generated
            .as_ref()
            .zip(view.picked)
            .and_then(|(g, id)| g.outcome(id))
            .map_or_else(
                || "Outcome".to_owned(),
                |o| {
                    format!(
                        "{} outcome {} {} {}",
                        self.simulation.study.name,
                        o.id,
                        o.material,
                        method_label(&o.method)
                    )
                },
            );
        let folder = self.doc.path.clone();
        let mut dialog = rfd::FileDialog::new()
            .add_filter("STL", &["stl"])
            .add_filter("OBJ", &["obj"])
            .add_filter("3MF", &["3mf"])
            .set_file_name(format!("{name}.stl"));
        if let Some(dir) = folder.as_deref().and_then(std::path::Path::parent) {
            dialog = dialog.set_directory(dir);
        }
        let Some(mut path) = dialog.save_file() else {
            return;
        };
        let extension = path
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        if !matches!(extension.as_str(), "stl" | "obj" | "3mf") {
            path.set_extension("stl");
        }
        let mesh = Mesh {
            faces: vec![face_mesh(&shape.positions, &shape.triangles)],
            edges: Vec::new(),
        };
        let written = match extension.as_str() {
            "obj" => oxyde_format::obj::save(
                &path,
                &[oxyde_format::obj::NamedMesh {
                    name: &name,
                    mesh: &mesh,
                }],
                1.0,
            ),
            "3mf" => oxyde_format::threemf::save(
                &path,
                &[oxyde_format::threemf::NamedMesh {
                    name: &name,
                    mesh: &mesh,
                    color: None,
                }],
                oxyde_format::stl::StlUnit::Millimetre,
            ),
            _ => {
                let triangles = oxyde_format::stl::triangles(&mesh, 1.0);
                oxyde_format::stl::save(&path, &name, &triangles, true)
            }
        };
        self.status = match written {
            Ok(()) => format!(
                "Exported the outcome ({} triangles) to {}",
                shape.triangles.len(),
                path.display()
            ),
            Err(e) => format!("Could not write {}: {e}", path.display()),
        };
    }

    /// Create Part: the picked outcome as a new part file, made off the UI thread (its
    /// surface cut to meet the preserve bodies, the starting shape and the obstacles on
    /// their faces, simplified, a solid made of each piece, written as STEP into the new
    /// part's Imported feature, in the outcome's material). The open part stays open; the
    /// panel offers the new one.
    pub(crate) fn create_part(&mut self) {
        // The bodies' surfaces as the outcome was grown on them, to meet them exactly; the
        // densities' own surface when they cannot be had.
        let surfaces = self
            .ensure_surfaces()
            .ok()
            .and(self.simulation.surfaces.as_ref());
        let space = surfaces.and_then(|s| design_space(&self.simulation.study, s).ok());
        let view = &self.simulation.generative;
        let Some((generated, o)) = view
            .generated
            .as_ref()
            .zip(view.picked)
            .and_then(|(g, id)| Some((g, g.outcome(id)?)))
        else {
            self.status = "Create Part: pick an outcome first".to_owned();
            return;
        };
        let (id, material_name) = (o.id, o.material.clone());
        // The densities shown: the iteration picked on the slider, else the last.
        let density = densities(o, view.snapshot);
        let (shape, held) = match &space {
            Some(s) => exact_surface(&generated.grid, density, SMOOTHING, &s.space),
            None => (surface(&generated.grid, density, SMOOTHING), Vec::new()),
        };
        if shape.triangles.is_empty() {
            self.status = "The outcome has no material to make a part of".to_owned();
            return;
        }
        let g = &self.simulation.study.generative;
        // The outcome's material: the study's, else the (first preserve body's) part's.
        let own_material = match (&self.asm, g.preserve.first()) {
            (Some(doc), Some(b)) => b
                .component
                .map(|top| crate::assembly::Occurrence {
                    top,
                    path: b.path.clone(),
                })
                .and_then(|o| doc.part_at(&o)?.part.material().cloned()),
            _ => self.doc.part.material().cloned(),
        };
        let material = g
            .materials
            .iter()
            .find(|m| m.name == material_name)
            .cloned()
            .or(own_material);
        let stem = match &self.asm {
            Some(doc) => doc.display_name(),
            None => self.doc.display_name(),
        };
        let name = format!("{stem} {} outcome {id}", self.simulation.study.name);
        let mut dialog = rfd::FileDialog::new()
            .add_filter("Oxyde part", &[oxyde_format::PART_EXTENSION])
            .set_file_name(format!("{name}.{}", oxyde_format::PART_EXTENSION));
        let folder = match &self.asm {
            Some(doc) => doc.path.clone(),
            None => self.doc.path.clone(),
        };
        if let Some(dir) = folder.as_deref().and_then(std::path::Path::parent) {
            dialog = dialog.set_directory(dir);
        }
        let Some(mut path) = dialog.save_file() else {
            return;
        };
        if !crate::assembly_ui::has_extension(&path, oxyde_format::PART_EXTENSION) {
            path.set_extension(oxyde_format::PART_EXTENSION);
        }
        let input = PartInput {
            shape,
            held,
            tolerance: PART_TOLERANCE * generated.grid.size,
            material,
            file_name: format!("{name}.step"),
        };
        self.simulation.generative.created = None;
        let target = path.clone();
        self.start_job(
            &format!("Making a part of outcome {id}"),
            move |kernel, progress| {
                let result = make_outcome_part(kernel, input, &|what| progress.say(what)).and_then(
                    |(part, note)| {
                        progress.say("Saving the part");
                        let file = oxyde_format::PartFile {
                            document_id: oxyde_format::new_document_id(),
                            definition: part.definition(),
                            properties: oxyde_format::Properties {
                                material: part
                                    .material()
                                    .map(|m| m.name.clone())
                                    .unwrap_or_default(),
                                ..oxyde_format::Properties::default()
                            },
                        };
                        let saved = oxyde_format::save_part(&target, &file)
                            .map(|()| note)
                            .map_err(|e| format!("could not save {}: {e}", target.display()));
                        let mut part = part;
                        part.release(kernel);
                        saved
                    },
                );
                crate::jobs::Outcome::PartCreated {
                    path: target,
                    result,
                }
            },
        );
    }

    /// The part *Create Part* made: saved, offered in the panel.
    pub(crate) fn part_created(&mut self, path: PathBuf, result: Result<String, String>) {
        match result {
            Ok(note) => {
                self.remember_recent(&path);
                // In an assembly, the part goes in where it was grown: at the origin, fixed.
                if self.asm.is_some() {
                    self.insert_part(&path, Some(oxyde_assembly::Placement::IDENTITY));
                }
                self.status = format!("Made {}: {note}", path.display());
                self.simulation.generative.created = Some(path);
            }
            Err(why) => self.status = format!("Could not make the part: {why}"),
        }
    }

    /// The Generative Design tab was chosen: when the open study is not a generative
    /// study, the model's first one opens, if it has one.
    pub(crate) fn enter_generative(&mut self) {
        if self.generative_study_open() || self.study_running() {
            return;
        }
        let first = self
            .simulation
            .study_list()
            .iter()
            .position(|(_, t)| *t == StudyType::Generative);
        if let Some(i) = first {
            self.switch_study(i);
        }
    }

    /// The Generative Design tab's buttons: New, the roles, fixtures and loads, Generate,
    /// Explore and Export.
    pub(crate) fn generative_tab(&mut self, ui: &mut egui::Ui) {
        use crate::icons::{Icon, command_button};
        let running = self.study_running();
        if command_button(
            ui,
            Icon::Generative,
            "New Generative Study",
            !running,
            false,
        )
        .on_hover_text(
            "A new generative study: shapes grown between bodies to keep, for several \
                 materials and ways of making them",
        )
        .clicked()
        {
            self.new_study(StudyType::Generative);
        }
        if !self.generative_study_open() {
            return;
        }
        let has_faces = !self.selected_sim_faces().is_empty();
        let enabled = !running
            && match &self.asm {
                Some(doc) => !doc.leaves(&doc.assembly).is_empty(),
                None => self.doc.part.body().is_some(),
            };
        ui.add_space(2.0);
        ui.separator();
        ui.add_space(2.0);
        let roles = [
            (
                Icon::PreserveBody,
                "Preserve",
                Role::Preserve,
                "Keep the bodies of the selected faces as they are: the outcomes join them",
            ),
            (
                Icon::Obstacle,
                "Obstacle",
                Role::Obstacle,
                "Keep material out of the bodies of the selected faces (and their offset)",
            ),
            (
                Icon::StartingShape,
                "Starting Shape",
                Role::StartingShape,
                "The material starts in the body of the selected faces and stays inside it",
            ),
        ];
        for (icon, label, role, hint) in roles {
            let response = command_button(ui, icon, label, enabled, false);
            let response = if has_faces {
                response.on_hover_text(hint)
            } else {
                response.on_hover_text(format!("{hint}\n(select a face of each body first)"))
            };
            if response.clicked() {
                self.assign_role(role);
            }
        }
        let hint = "Keep the material under the selected faces, as deep inside their body as set \
                    in the panel: around a hole (which stays clear), under a mounting face. The \
                    body needs no role: make it the starting shape to redesign it inside itself";
        let response = command_button(ui, Icon::Preserve, "Preserve Region", enabled, false);
        let response = if has_faces {
            response.on_hover_text(hint)
        } else {
            response.on_hover_text(format!("{hint}\n(select faces first)"))
        };
        if response.clicked() {
            self.add_study_item(ItemKind::Preserve { depth: 0.0 });
        }
        ui.add_space(2.0);
        ui.separator();
        ui.add_space(2.0);
        let items: [(Icon, &str, ItemKind, &str); 4] = [
            (
                Icon::Fixture,
                "Fixed Geometry",
                ItemKind::Fixed,
                "Hold the selected faces of a preserve body where they are",
            ),
            (
                Icon::Roller,
                "Roller/Slider",
                ItemKind::Roller,
                "Let the selected faces of a preserve body slide along themselves",
            ),
            (
                Icon::Force,
                "Force",
                ItemKind::Force {
                    newtons: 100.0,
                    direction: crate::simulation_ui::LoadDirection::Normal,
                },
                "A force on the selected faces of a preserve body, spread by area",
            ),
            (
                Icon::Pressure,
                "Pressure",
                ItemKind::Pressure { mpa: 1.0 },
                "A pressure on the selected faces of a preserve body",
            ),
        ];
        for (icon, label, kind, hint) in items {
            let response = command_button(ui, icon, label, enabled, false);
            let response = if has_faces {
                response.on_hover_text(hint)
            } else {
                response.on_hover_text(format!("{hint}\n(select faces first)"))
            };
            if response.clicked() {
                self.add_study_item(kind);
            }
        }
        ui.add_space(2.0);
        ui.separator();
        ui.add_space(2.0);
        if self.generative_running() {
            if command_button(ui, Icon::Cancel, "Stop", true, false)
                .on_hover_text("Stop: the outcome being grown is kept as far as it got")
                .clicked()
            {
                self.stop_generation();
            }
        } else if command_button(ui, Icon::RunStudy, "Generate", enabled, false)
            .on_hover_text("Grow an outcome for every material and every way of making it")
            .clicked()
        {
            self.start_generation();
        }
        let has_outcome = self
            .simulation
            .generative
            .generated
            .as_ref()
            .is_some_and(|g| g.outcomes.iter().any(|o| !o.density.is_empty()));
        let showing = self.simulation.generative.shown.is_some();
        if command_button(ui, Icon::Explore, "Explore", has_outcome, showing)
            .on_hover_text("Show the outcome picked in the panel in place of the model")
            .clicked()
        {
            let view = &mut self.simulation.generative;
            view.show = !showing;
            if view.picked.is_none() {
                view.picked = view.generated.as_ref().and_then(|g| {
                    g.outcomes
                        .iter()
                        .find(|o| !o.density.is_empty())
                        .map(|o| o.id)
                });
            }
        }
        if command_button(ui, Icon::Part, "Create Part", has_outcome, false)
            .on_hover_text(
                "A new part file of the outcome picked: its surface made a solid, meeting the \
                 bodies kept, the starting shape and the bodies to stay clear of on their faces",
            )
            .clicked()
        {
            self.create_part();
        }
        if command_button(ui, Icon::ExportMesh, "Export", has_outcome, false)
            .on_hover_text(
                "Write the outcome picked as STL, OBJ or 3MF: to print it, or to model over it",
            )
            .clicked()
        {
            self.export_outcome();
        }
    }

    /// The tab's panel when no generative study is open: what generative design does, New,
    /// the model's generative studies.
    fn generative_chooser(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            crate::icons::small(ui, crate::icons::Icon::Generative, 26.0);
            ui.strong("Generative design");
        });
        ui.label(
            egui::RichText::new(
                "Give the part's bodies their roles (bodies to keep, bodies to stay clear of), \
                 hold and load the bodies to keep, say how heavy the result may be and how it \
                 is to be made, and Oxyde grows shapes between them: one for every material \
                 and way of making it, to compare.",
            )
            .small()
            .weak(),
        );
        ui.add_space(4.0);
        let running = self.study_running();
        if ui
            .add_enabled(!running, egui::Button::new("New Generative Study"))
            .clicked()
        {
            self.new_study(StudyType::Generative);
        }
        let list = self.simulation.study_list();
        let mut open = None;
        for (i, (name, t)) in list.iter().enumerate() {
            if *t == StudyType::Generative && ui.link(name).clicked() {
                open = Some(i);
            }
        }
        ui.add_space(4.0);
        if ui
            .small_button("Open…")
            .on_hover_text("Open the studies saved for this model (.oxysim)")
            .clicked()
        {
            self.open_study();
        }
        if let Some(i) = open {
            self.switch_study(i);
        }
    }

    /// The tab's panel: the study, its roles, conditions, objectives, manufacturing,
    /// materials and resolution, and its outcomes.
    pub(crate) fn generative_panel(&mut self, ui: &mut egui::Ui) {
        if !self.generative_study_open() {
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.set_width(ui.available_width());
                self.generative_chooser(ui);
            });
            ui.add_space(4.0);
            return;
        }
        let surface_error = self.ensure_surfaces().err();
        // What the sections show, gathered before the study is borrowed to edit.
        let bodies: Vec<(StudyBody, String)> = self
            .simulation
            .surfaces
            .as_ref()
            .map(|s| {
                s.bodies
                    .iter()
                    .filter_map(|b| b.study_body())
                    .map(|b| {
                        let label = self.study_body_label(&b);
                        (b, label)
                    })
                    .collect()
            })
            .unwrap_or_default();
        let library: Vec<Material> = self.materials.materials().to_vec();
        let part_material = self.doc.part.material().cloned();
        let (estimate, census) = self.cached_setup();
        let materials = study_materials(&self.simulation.study.generative, part_material.as_ref());
        let regions = preserve_items(&self.simulation.study).next().is_some();
        let preserve_masses: Vec<(String, f64)> = match self.simulation.surfaces.as_ref() {
            Some(s) => materials
                .iter()
                .filter_map(|(name, e, _)| {
                    let e = e.as_ref().ok()?;
                    Some((
                        name.clone(),
                        preserve_mass(&self.simulation.study, s, census.as_ref(), e.density),
                    ))
                })
                .collect(),
            None => Vec::new(),
        };
        let warnings = match self.simulation.surfaces.as_ref() {
            Some(s) => setup_warnings(
                &self.simulation.study,
                s,
                &bodies,
                &materials,
                estimate,
                census.as_ref(),
            ),
            None => Vec::new(),
        };
        let running = self.generative_running();
        let progress = self
            .simulation
            .generative
            .job
            .as_ref()
            .map(GenJob::progress);
        let mut action: Option<GenAction> = None;
        let mut item_action: Option<PanelAction> = None;
        let max_height = (ui.available_height() * 0.75).max(180.0);

        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.set_width(ui.available_width());
            self.study_bar(ui);
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if running {
                        if ui.button("Stop").clicked() {
                            action = Some(GenAction::Stop);
                        }
                    } else if ui.button("Generate").clicked() {
                        action = Some(GenAction::Generate);
                    }
                });
            });
            ui.horizontal_wrapped(|ui| {
                let file_name = self.simulation.file.as_ref().map_or_else(
                    || "not saved".to_owned(),
                    |f| {
                        f.path
                            .file_name()
                            .map_or_else(String::new, |n| n.to_string_lossy().into_owned())
                    },
                );
                ui.label(egui::RichText::new(file_name).small().weak());
                if ui
                    .small_button("Open…")
                    .on_hover_text("Open a study (.oxysim) of this model")
                    .clicked()
                {
                    action = Some(GenAction::OpenStudy);
                }
                if ui
                    .small_button("Save")
                    .on_hover_text(
                        "Save the studies (.oxysim) next to the model; the outcomes go beside them",
                    )
                    .clicked()
                {
                    action = Some(GenAction::SaveStudy);
                }
                if ui
                    .small_button("Open outcomes…")
                    .on_hover_text(
                        "Open outcomes (.oxyres) of this study, if the model is unchanged",
                    )
                    .clicked()
                {
                    action = Some(GenAction::OpenResults);
                }
                let any = self
                    .simulation
                    .generative
                    .generated
                    .as_ref()
                    .is_some_and(|g| !g.outcomes.is_empty());
                if ui
                    .add_enabled(any, egui::Button::new("Save outcomes…").small())
                    .clicked()
                {
                    action = Some(GenAction::SaveResults);
                }
            });
            if let Some((stage, fraction)) = &progress {
                ui.add(
                    egui::ProgressBar::new(*fraction as f32)
                        .text(format!("{stage}… {:.0} %", fraction * 100.0)),
                );
            }
            if let Some(path) = &self.simulation.generative.created {
                ui.horizontal_wrapped(|ui| {
                    let name = path
                        .file_name()
                        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
                    ui.label(egui::RichText::new(format!("Made {name}")).small());
                    if ui
                        .small_button("Open it")
                        .on_hover_text("Open the part made from the outcome (this part closes)")
                        .clicked()
                    {
                        action = Some(GenAction::OpenCreated);
                    }
                });
            }
            for e in self
                .simulation
                .generative
                .error
                .iter()
                .chain(&surface_error)
            {
                ui.label(
                    egui::RichText::new(e)
                        .small()
                        .color(egui::Color32::from_rgb(210, 60, 60)),
                );
            }
            // Before Generate: what would make the outcomes disappoint.
            if !running {
                for w in &warnings {
                    ui.label(
                        egui::RichText::new(format!("⚠ {w}"))
                            .small()
                            .color(egui::Color32::from_rgb(205, 135, 30)),
                    );
                }
            }
            egui::ScrollArea::vertical()
                .id_salt("generative_panel")
                .max_height(max_height)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    let study = &mut self.simulation.study;
                    let selected = &mut self.simulation.selected;
                    ui.add_enabled_ui(!running, |ui| {
                        egui::CollapsingHeader::new("Design space")
                            .default_open(true)
                            .show(ui, |ui| {
                                let regions = preserve_items(study).count();
                                if design_space_rows(ui, &mut study.generative, &bodies, regions) {
                                    action.get_or_insert(GenAction::Edited);
                                }
                                for (i, item) in study.items.iter_mut().enumerate() {
                                    if matches!(item.kind, ItemKind::Preserve { .. }) {
                                        item_row(ui, i, item, selected, &mut item_action);
                                    }
                                }
                            });
                        egui::CollapsingHeader::new("Design conditions")
                            .default_open(true)
                            .show(ui, |ui| {
                                let mut any = false;
                                for (i, item) in study.items.iter_mut().enumerate() {
                                    if !is_condition(&item.kind) {
                                        continue;
                                    }
                                    any = true;
                                    item_row(ui, i, item, selected, &mut item_action);
                                }
                                if !any {
                                    ui.label(
                                        egui::RichText::new(
                                            "Select faces of a preserve body or region, then \
                                             Fixed Geometry or Roller/Slider to hold it, Force \
                                             or Pressure to load it",
                                        )
                                        .small()
                                        .weak(),
                                    );
                                }
                                ui.separator();
                                let (fixture_names, load_names) = item_names(&study.items);
                                if load_case_rows(
                                    ui,
                                    &mut study.generative,
                                    &fixture_names,
                                    &load_names,
                                ) {
                                    action.get_or_insert(GenAction::Edited);
                                }
                            });
                        egui::CollapsingHeader::new("Objectives and limits")
                            .default_open(true)
                            .show(ui, |ui| {
                                if objective_rows(
                                    ui,
                                    &mut study.generative,
                                    &preserve_masses,
                                    regions,
                                ) {
                                    action.get_or_insert(GenAction::Edited);
                                }
                            });
                        egui::CollapsingHeader::new("Manufacturing")
                            .default_open(true)
                            .show(ui, |ui| {
                                if manufacturing_rows(ui, &mut study.generative) {
                                    action.get_or_insert(GenAction::Edited);
                                }
                            });
                        egui::CollapsingHeader::new("Materials")
                            .default_open(true)
                            .show(ui, |ui| {
                                if material_rows(
                                    ui,
                                    &mut study.generative,
                                    &library,
                                    part_material.as_ref(),
                                ) {
                                    action.get_or_insert(GenAction::Edited);
                                }
                            });
                        egui::CollapsingHeader::new("Resolution")
                            .default_open(false)
                            .show(ui, |ui| {
                                if resolution_rows(ui, &mut study.generative, estimate) {
                                    action.get_or_insert(GenAction::Edited);
                                }
                            });
                    });
                    let p = &mut self.prefs.simulation;
                    if ui
                        .checkbox(&mut p.generative_gpu, "Use the graphics card")
                        .on_hover_text(
                            "Solve on the graphics card when there is one that can (several \
                             times faster); the processor otherwise",
                        )
                        .changed()
                    {
                        self.simulation.prefs_dirty = true;
                    }
                    let view = &mut self.simulation.generative;
                    if let Some(g) = view.generated.as_ref() {
                        egui::CollapsingHeader::new(format!("Outcomes ({})", g.outcomes.len()))
                            .default_open(true)
                            .show(ui, |ui| {
                                if let Some(a) = outcome_rows(
                                    ui,
                                    g,
                                    view.picked,
                                    &mut view.y_axis,
                                    &study.generative,
                                ) {
                                    action = Some(a);
                                }
                                let picked = view.picked.and_then(|id| g.outcome(id));
                                if let Some(o) = picked
                                    && let Some(a) = picked_rows(
                                        ui,
                                        o,
                                        &mut view.snapshot,
                                        &mut view.show,
                                        &mut view.show_obstacles,
                                        study.generative.safety_factor,
                                    )
                                {
                                    action = Some(a);
                                }
                            });
                    }
                });
        });
        if let Some(a) = item_action {
            self.study_item_action(a);
        }
        match action {
            Some(GenAction::Generate) => self.start_generation(),
            Some(GenAction::Stop) => self.stop_generation(),
            Some(GenAction::Pick(id)) => {
                let view = &mut self.simulation.generative;
                if view.picked != Some(id) {
                    view.snapshot = None;
                }
                view.picked = Some(id);
                view.show = true;
            }
            Some(GenAction::Show(on)) => self.simulation.generative.show = on,
            Some(GenAction::Export) => self.export_outcome(),
            Some(GenAction::CreatePart) => self.create_part(),
            Some(GenAction::OpenCreated) => {
                if let Some(path) = self.simulation.generative.created.clone() {
                    self.guard(crate::app::PendingAction::OpenRecent(path), ui.ctx());
                }
            }
            Some(GenAction::OpenStudy) => self.open_study(),
            Some(GenAction::SaveStudy) => {
                self.save_study(false);
            }
            Some(GenAction::OpenResults) => self.open_results(),
            Some(GenAction::SaveResults) => {
                self.save_results(true);
            }
            Some(GenAction::Edited) => self.simulation.generative.show = false,
            None => {}
        }
    }
}

// ---- Create Part ---------------------------------------------------------------------------

/// What *Create Part* works from: the outcome's surface (from [`exact_surface`], on the
/// bodies' faces where it meets them) and material.
struct PartInput {
    shape: Shape,
    /// Per point of `shape`, whether it is on a body's face, where simplifying leaves it.
    held: Vec<bool>,
    /// mm: the furthest simplifying may move the surface.
    tolerance: f64,
    material: Option<Material>,
    file_name: String,
}

/// The part of an outcome, rebuilt on `kernel`, and a note of what was done: the surface
/// simplified to [`PART_TRIANGLES`], a solid of each piece, written as STEP into an
/// Imported feature, in the outcome's material. No boolean joins the exact preserve bodies
/// or cuts the obstacles: the surface already meets them on their faces, and OpenCASCADE
/// takes minutes over a boolean between a fine outcome's thousands of faces and those, or
/// fails.
fn make_outcome_part<K: Kernel>(
    kernel: &mut K,
    input: PartInput,
    say: &dyn Fn(String),
) -> Result<(Part, String), String> {
    let PartInput {
        shape,
        held,
        tolerance,
        material,
        file_name,
    } = input;
    let mut made: Vec<ShapeHandle> = Vec::new();
    let release = |kernel: &mut K, shapes: &[ShapeHandle]| {
        for s in shapes {
            kernel.release(*s);
        }
    };
    let result = (|| {
        say("Simplifying the surface".to_owned());
        let fewer = decimate_holding(&shape, &held, PART_TRIANGLES, tolerance);
        let pieces = solid_pieces(&fewer);
        if pieces.is_empty() {
            return Err("the outcome has no closed surface".to_owned());
        }
        say(format!(
            "Making a solid of {} triangles",
            fewer.triangles.len()
        ));
        let mut solids = Vec::new();
        for p in &pieces {
            let s = kernel
                .solid_from_mesh(&p.positions, &p.triangles)
                .map_err(|e| format!("the surface makes no solid: {e}"))?
                .shape;
            made.push(s);
            solids.push(s);
        }
        let mut notes = vec![format!("{} triangles", fewer.triangles.len())];
        if solids.len() > 1 {
            notes.push(format!("{} bodies", solids.len()));
        }
        let bodies = solids;
        say("Writing the solid".to_owned());
        let step = std::env::temp_dir().join(format!(
            "oxyde-outcome-{}-{}.step",
            std::process::id(),
            crate::document::next_revision()
        ));
        let materials: Vec<Option<StepMaterial>> = bodies
            .iter()
            .map(|_| {
                material.as_ref().map(|m| StepMaterial {
                    name: m.name.clone(),
                    description: String::new(),
                    density: m.density,
                })
            })
            .collect();
        kernel
            .export_step(&bodies, &[], &materials, &step)
            .map_err(|e| format!("could not write the solid: {e}"))?;
        let bytes = std::fs::read(&step).map_err(|e| format!("could not read the solid: {e}"));
        let _ = std::fs::remove_file(&step);
        Ok((bytes?, notes.join(", ")))
    })();
    release(kernel, &made);
    let (bytes, note) = result?;
    say("Rebuilding the part".to_owned());
    let mut part = Part::new();
    part.set_material(material);
    part.add_feature(oxyde_model::FeatureKind::Import(Import::new(
        file_name,
        FileData::new(bytes),
    )));
    part.rebuild(kernel);
    let id = part.timeline().last().map(|e| e.id);
    if let Some(FeatureStatus::Error(why)) = id.and_then(|id| part.status(id)) {
        let why = why.clone();
        part.release(kernel);
        return Err(why);
    }
    Ok((part, note))
}

// ---- The panel's sections ------------------------------------------------------------------

fn weak(ui: &mut egui::Ui, text: &str) {
    ui.label(egui::RichText::new(text).small().weak());
}

/// The bodies with their roles, the obstacles' offsets and the margin; `regions` the
/// study's preserve regions, listed after.
fn design_space_rows(
    ui: &mut egui::Ui,
    g: &mut GenerativeSettings,
    bodies: &[(StudyBody, String)],
    regions: usize,
) -> bool {
    let mut changed = false;
    if bodies.is_empty() {
        weak(ui, "The part has no body yet");
        return false;
    }
    for (row, (id, name)) in bodies.iter().enumerate() {
        ui.horizontal(|ui| {
            let role = role_of(g, id);
            if let Some(c) = role.color() {
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
                ui.painter()
                    .rect_filled(rect, 2.0, egui::Color32::from_rgb(c[0], c[1], c[2]));
            } else {
                ui.add_space(14.0);
            }
            ui.label(name);
            let mut chosen = role;
            egui::ComboBox::from_id_salt(("generative_role", row))
                .selected_text(role.label())
                .width(110.0)
                .show_ui(ui, |ui| {
                    for r in Role::ALL {
                        ui.selectable_value(&mut chosen, r, r.label())
                            .on_hover_text(r.hint());
                    }
                });
            if chosen != role {
                set_role(g, id.clone(), chosen);
                changed = true;
            }
            if let Some(o) = g.obstacles.iter_mut().find(|o| o.body == *id) {
                changed |= ui
                    .add(
                        egui::DragValue::new(&mut o.offset)
                            .range(0.0..=1000.0)
                            .speed(0.05)
                            .prefix("+")
                            .suffix(" mm"),
                    )
                    .on_hover_text("Material keeps this far from the obstacle too")
                    .changed();
            }
        });
    }
    if g.preserve.is_empty() && regions == 0 {
        weak(
            ui,
            "Keep where the part is held and loaded: select a face of each body to keep, then \
             Preserve; or, to redesign a body, the faces to keep (its holes, its mounting \
             faces), then Preserve Region",
        );
    }
    if g.starting_shape.is_none() {
        // Each axis its own room: 0 along one keeps the outcome within the bodies there,
        // a part's own thickness.
        ui.horizontal_wrapped(|ui| {
            ui.label("Margin").on_hover_text(
                "How far the material may go beyond the bodies kept, both ways along each \
                 axis; a tenth of their size where off. 0 along an axis keeps the outcome \
                 within the bodies along it (a part's own thickness)",
            );
            for (margin, name) in g.margin.iter_mut().zip(["X", "Y", "Z"]) {
                let mut set = margin.is_some();
                if ui
                    .checkbox(&mut set, name)
                    .on_hover_text(format!(
                        "The room along {name}, both ways; a tenth of the bodies' size when off"
                    ))
                    .changed()
                {
                    *margin = set.then_some(0.0);
                    changed = true;
                }
                if let Some(m) = margin.as_mut() {
                    changed |= ui
                        .add(
                            egui::DragValue::new(m)
                                .range(0.0..=10_000.0)
                                .speed(0.5)
                                .suffix(" mm"),
                        )
                        .changed();
                }
            }
        });
    }
    changed
}

/// The names of the study's fixtures and of its loads, in order.
fn item_names(items: &[crate::simulation_ui::StudyItem]) -> (Vec<String>, Vec<String>) {
    let mut fixtures = Vec::new();
    let mut loads = Vec::new();
    for i in items.iter().filter(|i| is_condition(&i.kind)) {
        if matches!(i.kind, ItemKind::Fixed | ItemKind::Roller) {
            fixtures.push(i.name.clone());
        } else {
            loads.push(i.name.clone());
        }
    }
    (fixtures, loads)
}

/// The load cases: none (one of everything), or each with the fixtures and loads ticked.
fn load_case_rows(
    ui: &mut egui::Ui,
    g: &mut GenerativeSettings,
    fixtures: &[String],
    loads: &[String],
) -> bool {
    let mut changed = false;
    if g.load_cases.is_empty() {
        ui.horizontal(|ui| {
            weak(ui, "One load case: every fixture and load at once");
            if ui
                .small_button("Add a load case")
                .on_hover_text(
                    "Loads the part meets at different times, each carried on its own: the \
                     outcomes are as stiff as can be for them all",
                )
                .clicked()
            {
                g.load_cases = vec![
                    LoadCase {
                        name: "Load Case 1".to_owned(),
                        fixtures: fixtures.to_vec(),
                        loads: loads.to_vec(),
                    },
                    LoadCase {
                        name: "Load Case 2".to_owned(),
                        fixtures: fixtures.to_vec(),
                        loads: Vec::new(),
                    },
                ];
                changed = true;
            }
        });
        return changed;
    }
    let mut delete = None;
    for (i, c) in g.load_cases.iter_mut().enumerate() {
        ui.horizontal(|ui| {
            ui.strong(&c.name);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .small_button("×")
                    .on_hover_text("Delete this load case")
                    .clicked()
                {
                    delete = Some(i);
                }
            });
        });
        for (names, list, what) in [
            (fixtures, &mut c.fixtures, "Held by"),
            (loads, &mut c.loads, "Loaded by"),
        ] {
            ui.horizontal_wrapped(|ui| {
                ui.add_space(8.0);
                ui.label(egui::RichText::new(what).small());
                for n in names {
                    let mut on = list.contains(n);
                    if ui.toggle_value(&mut on, n.as_str()).changed() {
                        list.retain(|x| x != n);
                        if on {
                            list.push(n.clone());
                        }
                        changed = true;
                    }
                }
            });
        }
        if c.fixtures.is_empty() || c.loads.is_empty() {
            weak(ui, "Tick a fixture and a load for it");
        }
    }
    if let Some(i) = delete {
        g.load_cases.remove(i);
        changed = true;
    }
    let unused: Vec<&String> = loads
        .iter()
        .filter(|n| !g.load_cases.iter().any(|c| c.loads.contains(n)))
        .collect();
    if !g.load_cases.is_empty() && !unused.is_empty() {
        let names: Vec<&str> = unused.iter().map(|s| s.as_str()).collect();
        weak(
            ui,
            &format!("{} in no load case: not applied", names.join(", ")),
        );
    }
    if ui
        .small_button("Add a load case")
        .on_hover_text("Another load case, holding the part as the others do")
        .clicked()
    {
        let n = (1..)
            .map(|k| format!("Load Case {k}"))
            .find(|n| !g.load_cases.iter().any(|c| c.name == *n))
            .unwrap_or_default();
        g.load_cases.push(LoadCase {
            name: n,
            fixtures: fixtures.to_vec(),
            loads: Vec::new(),
        });
        changed = true;
    }
    changed
}

/// Maximize stiffness, the mass target and the safety factor; `preserve_masses` what the
/// kept material weighs in each material, measured by the voxels when `regions` keep some.
fn objective_rows(
    ui: &mut egui::Ui,
    g: &mut GenerativeSettings,
    preserve_masses: &[(String, f64)],
    regions: bool,
) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        changed |= ui
            .radio_value(
                &mut g.objective,
                GenerativeObjective::MaxStiffness,
                "Maximize stiffness",
            )
            .on_hover_text("The stiffest shape that weighs the mass target")
            .changed();
        changed |= ui
            .radio_value(
                &mut g.objective,
                GenerativeObjective::MinMass,
                "Minimize mass",
            )
            .on_hover_text(
                "The lightest shape that keeps its stress under the material's strength over \
                 the safety factor (and its displacement under the limit)",
            )
            .changed();
    });
    let stiffest = g.objective == GenerativeObjective::MaxStiffness;
    egui::Grid::new("generative_objectives")
        .num_columns(2)
        .spacing([10.0, 4.0])
        .show(ui, |ui| {
            if stiffest {
                ui.label("Mass target");
                changed |= ui
                    .add(
                        egui::DragValue::new(&mut g.mass_target)
                            .range(0.0..=100_000.0)
                            .speed(0.001)
                            .max_decimals(4)
                            .suffix(" kg"),
                    )
                    .on_hover_text(
                        "What every outcome weighs, the preserve bodies included, whatever its material",
                    )
                    .changed();
                ui.end_row();
            }
            ui.label("Safety factor");
            changed |= ui
                .add(
                    egui::DragValue::new(&mut g.safety_factor)
                        .range(0.1..=100.0)
                        .speed(0.05)
                        .max_decimals(2),
                )
                .on_hover_text(if stiffest {
                    "Outcomes whose estimated factor of safety is under it are marked in Explore"
                } else {
                    "The stress is kept under the material's strength divided by it"
                })
                .changed();
            ui.end_row();
            if !stiffest {
                ui.label("Displacement limit");
                ui.horizontal(|ui| {
                    let mut on = g.displacement_limit.is_some();
                    if ui
                        .checkbox(&mut on, "")
                        .on_hover_text("The most the outcome may move under its loads")
                        .changed()
                    {
                        g.displacement_limit = on.then_some(0.5);
                        changed = true;
                    }
                    if let Some(d) = g.displacement_limit.as_mut() {
                        changed |= ui
                            .add(
                                egui::DragValue::new(d)
                                    .range(0.0001..=1000.0)
                                    .speed(0.005)
                                    .max_decimals(4)
                                    .suffix(" mm"),
                            )
                            .changed();
                    }
                });
                ui.end_row();
            }
        });
    if stiffest {
        for (name, mass) in preserve_masses {
            let what = if regions {
                "What is kept weighs about"
            } else {
                "The preserve bodies weigh"
            };
            weak(
                ui,
                &format!(
                    "{what} {} in {name}: the target must be more",
                    format_value(*mass, "kg")
                ),
            );
        }
        if g.mass_target <= 0.0 {
            weak(ui, "Set the mass target to generate");
        }
    } else {
        weak(
            ui,
            "Each material's yield strength (else its tensile strength) over the safety \
             factor is the stress allowed",
        );
    }
    changed
}

/// The ways of making it, each with its directions.
fn manufacturing_rows(ui: &mut egui::Ui, g: &mut GenerativeSettings) -> bool {
    let mut changed = false;
    let has = |g: &GenerativeSettings, name: &str| g.methods.iter().any(|m| m.name() == name);
    let toggle = |g: &mut GenerativeSettings, name: &str, on: bool, make: GenerativeMethod| {
        if on {
            if !g.methods.iter().any(|m| m.name() == name) {
                g.methods.push(make);
            }
        } else {
            g.methods.retain(|m| m.name() != name);
        }
    };
    let mut unrestricted = has(g, "unrestricted");
    if ui
        .checkbox(&mut unrestricted, "Unrestricted")
        .on_hover_text("Any shape: the stiffest, whatever it takes to make")
        .changed()
    {
        toggle(
            g,
            "unrestricted",
            unrestricted,
            GenerativeMethod::Unrestricted,
        );
        changed = true;
    }
    let mut additive = has(g, "additive");
    if ui
        .checkbox(&mut additive, "Additive")
        .on_hover_text(
            "3D printed in layers along the build direction, from a plate at the design \
             space's end: nothing overhangs more than 45° from it, so it needs no supports",
        )
        .changed()
    {
        toggle(
            g,
            "additive",
            additive,
            GenerativeMethod::Additive {
                directions: vec![AxisDirection::PlusY],
            },
        );
        changed = true;
    }
    if let Some(GenerativeMethod::Additive { directions }) =
        g.methods.iter_mut().find(|m| m.name() == "additive")
    {
        ui.horizontal_wrapped(|ui| {
            ui.add_space(18.0);
            ui.label(egui::RichText::new("Built towards").small());
            for axis in 0..3 {
                for positive in [true, false] {
                    let d = AxisDirection::new(axis, positive);
                    let mut on = directions.contains(&d);
                    if ui.toggle_value(&mut on, direction_label(d)).changed() {
                        directions.retain(|x| *x != d);
                        if on {
                            directions.push(d);
                        }
                        changed = true;
                    }
                }
            }
            ui.label(egui::RichText::new("overhang 45°").small().weak());
        });
        if directions.is_empty() {
            weak(ui, "Pick a direction, or untick it");
        }
    }
    let mut cutting = has(g, "cutting");
    if ui
        .checkbox(&mut cutting, "2-axis cutting")
        .on_hover_text(
            "Cut from a plate by water jet, laser or wire: the shape the same all along the \
             direction",
        )
        .changed()
    {
        toggle(
            g,
            "cutting",
            cutting,
            GenerativeMethod::Cutting {
                directions: vec![AxisDirection::PlusZ],
            },
        );
        changed = true;
    }
    if let Some(GenerativeMethod::Cutting { directions }) =
        g.methods.iter_mut().find(|m| m.name() == "cutting")
    {
        ui.horizontal(|ui| {
            ui.add_space(18.0);
            ui.label(egui::RichText::new("Along").small());
            for axis in 0..3 {
                let d = AxisDirection::new(axis, true);
                let mut on = directions.iter().any(|x| x.axis() == axis);
                if ui.toggle_value(&mut on, axis_label(axis)).changed() {
                    directions.retain(|x| x.axis() != axis);
                    if on {
                        directions.push(d);
                    }
                    changed = true;
                }
            }
        });
        if directions.is_empty() {
            weak(ui, "Pick a direction, or untick it");
        }
    }
    let mut casting = has(g, "casting");
    if ui
        .checkbox(&mut casting, "Casting")
        .on_hover_text(
            "Cast in a mould pulled off along the direction: no undercut, nothing the mould \
             could not leave",
        )
        .changed()
    {
        toggle(
            g,
            "casting",
            casting,
            GenerativeMethod::Casting {
                directions: vec![AxisDirection::PlusY],
                parting: None,
            },
        );
        changed = true;
    }
    if let Some(GenerativeMethod::Casting {
        directions,
        parting,
    }) = g.methods.iter_mut().find(|m| m.name() == "casting")
    {
        ui.horizontal_wrapped(|ui| {
            ui.add_space(18.0);
            ui.label(egui::RichText::new("Pulled").small());
            for axis in 0..3 {
                for positive in [true, false] {
                    let d = AxisDirection::new(axis, positive);
                    let mut on = directions.contains(&d);
                    if ui.toggle_value(&mut on, direction_label(d)).changed() {
                        directions.retain(|x| *x != d);
                        if on {
                            directions.push(d);
                        }
                        changed = true;
                    }
                }
            }
        });
        ui.horizontal(|ui| {
            ui.add_space(18.0);
            let mut both = parting.is_some();
            if ui
                .checkbox(&mut both, "Both ways from a parting plane at")
                .on_hover_text(
                    "A two-part mould: each half pulled off away from the plane across the \
                     direction through this point",
                )
                .changed()
            {
                *parting = both.then_some([0.0; 3]);
                changed = true;
            }
            if let Some(p) = parting.as_mut() {
                for v in p.iter_mut() {
                    changed |= ui
                        .add(egui::DragValue::new(v).speed(0.5).suffix(" mm"))
                        .changed();
                }
            }
        });
        if directions.is_empty() {
            weak(ui, "Pick a direction, or untick it");
        }
    }
    let mut milling = has(g, "milling");
    if ui
        .checkbox(&mut milling, "Milling")
        .on_hover_text(
            "3-axis milled from the side picked: the tool reaches every face from there, and \
             no pocket is narrower than the tool",
        )
        .changed()
    {
        toggle(
            g,
            "milling",
            milling,
            GenerativeMethod::Milling {
                directions: vec![AxisDirection::PlusY],
                tool_diameter: Some(6.0),
            },
        );
        changed = true;
    }
    if let Some(GenerativeMethod::Milling {
        directions,
        tool_diameter,
    }) = g.methods.iter_mut().find(|m| m.name() == "milling")
    {
        ui.horizontal_wrapped(|ui| {
            ui.add_space(18.0);
            ui.label(egui::RichText::new("From").small());
            for axis in 0..3 {
                for positive in [true, false] {
                    let d = AxisDirection::new(axis, positive);
                    let mut on = directions.contains(&d);
                    if ui.toggle_value(&mut on, direction_label(d)).changed() {
                        directions.retain(|x| *x != d);
                        if on {
                            directions.push(d);
                        }
                        changed = true;
                    }
                }
            }
        });
        ui.horizontal(|ui| {
            ui.add_space(18.0);
            let mut set = tool_diameter.is_some();
            if ui
                .checkbox(&mut set, "Tool diameter")
                .on_hover_text("No pocket or gap narrower than the tool")
                .changed()
            {
                *tool_diameter = set.then_some(6.0);
                changed = true;
            }
            if let Some(d) = tool_diameter.as_mut() {
                changed |= ui
                    .add(
                        egui::DragValue::new(d)
                            .range(0.01..=1000.0)
                            .speed(0.1)
                            .suffix(" mm"),
                    )
                    .changed();
            }
        });
        if directions.is_empty() {
            weak(ui, "Pick a side, or untick it");
        }
    }
    let count: usize = g.methods.iter().map(|m| m.outcomes().len()).sum();
    if g.methods.is_empty() {
        weak(ui, "Pick at least one way of making it");
    } else {
        weak(
            ui,
            &format!("{count} way(s) of making it, each an outcome per material"),
        );
    }
    changed
}

/// The materials to generate for: the study's list, else the part's own.
fn material_rows(
    ui: &mut egui::Ui,
    g: &mut GenerativeSettings,
    library: &[Material],
    part: Option<&Material>,
) -> bool {
    let mut changed = false;
    let mut remove = None;
    for (i, m) in g.materials.iter().enumerate() {
        ui.horizontal(|ui| {
            let c = m.appearance.color;
            let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
            ui.painter()
                .rect_filled(rect, 2.0, egui::Color32::from_rgb(c[0], c[1], c[2]));
            ui.label(&m.name);
            if let Err(why) = elastic_of(m) {
                ui.label(egui::RichText::new("⚠").color(egui::Color32::from_rgb(210, 140, 30)))
                    .on_hover_text(why);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .small_button("×")
                    .on_hover_text("Take it off the study")
                    .clicked()
                {
                    remove = Some(i);
                }
            });
        });
    }
    if let Some(i) = remove {
        g.materials.remove(i);
        changed = true;
    }
    if g.materials.is_empty() {
        match part {
            Some(m) => weak(ui, &format!("The part's own material: {}", m.name)),
            None => weak(ui, "The part has no material: add one here"),
        }
    }
    let mut add = None;
    egui::ComboBox::from_id_salt("generative_add_material")
        .selected_text("Add a material…")
        .width(180.0)
        .show_ui(ui, |ui| {
            for (i, m) in library.iter().enumerate() {
                if g.materials.iter().any(|x| x.name == m.name) {
                    continue;
                }
                if ui.selectable_label(false, &m.name).clicked() {
                    add = Some(i);
                }
            }
        });
    if let Some(m) = add.and_then(|i| library.get(i)) {
        let mut m = m.clone();
        // A study document holds no images.
        m.appearance.texture = None;
        g.materials.push(m);
        changed = true;
    }
    changed
}

/// The voxels' size, the thinnest member, the iterations and the symmetry.
fn resolution_rows(
    ui: &mut egui::Ui,
    g: &mut GenerativeSettings,
    estimate: Option<(f64, usize)>,
) -> bool {
    let mut changed = false;
    egui::Grid::new("generative_resolution")
        .num_columns(2)
        .spacing([10.0, 4.0])
        .show(ui, |ui| {
            ui.label("Resolution");
            ui.horizontal(|ui| match &mut g.resolution {
                MeshSize::Automatic { density } => {
                    ui.label(egui::RichText::new("Coarse").small());
                    changed |= ui
                        .add(egui::Slider::new(density, 0.0..=1.0).show_value(false))
                        .changed();
                    ui.label(egui::RichText::new("Fine").small());
                }
                MeshSize::ElementSize(h) => {
                    changed |= ui
                        .add(
                            egui::DragValue::new(h)
                                .range(0.01..=1000.0)
                                .speed(0.01)
                                .max_decimals(3)
                                .suffix(" mm"),
                        )
                        .on_hover_text("The voxels' edge")
                        .changed();
                }
            });
            ui.end_row();
            ui.label("Voxel size");
            ui.horizontal(|ui| {
                let typed = matches!(g.resolution, MeshSize::ElementSize(_));
                match estimate {
                    Some((h, n)) => {
                        let text = format!("{} mm, about {n} voxels", format_number_short(h));
                        if n > MAX_VOXELS {
                            ui.label(
                                egui::RichText::new(format!("{text}: too many"))
                                    .small()
                                    .color(egui::Color32::from_rgb(210, 60, 60)),
                            );
                        } else {
                            ui.label(egui::RichText::new(text).small());
                        }
                    }
                    None => weak(ui, "give the preserve bodies first"),
                }
                let label = if typed { "Automatic" } else { "Type it" };
                if ui.small_button(label).clicked() {
                    g.resolution = if typed {
                        MeshSize::Automatic { density: 0.5 }
                    } else {
                        MeshSize::ElementSize(estimate.map_or(1.0, |(h, _)| h))
                    };
                    changed = true;
                }
            });
            ui.end_row();
            ui.label("Minimum thickness");
            ui.horizontal(|ui| {
                let mut set = g.member_size.is_some();
                if ui
                    .checkbox(&mut set, "")
                    .on_hover_text("The thinnest bar or wall wanted; about three voxels when off")
                    .changed()
                {
                    g.member_size = set.then(|| estimate.map_or(3.0, |(h, _)| 3.0 * h));
                    changed = true;
                }
                if let Some(t) = g.member_size.as_mut() {
                    changed |= ui
                        .add(
                            egui::DragValue::new(t)
                                .range(0.01..=1000.0)
                                .speed(0.05)
                                .suffix(" mm"),
                        )
                        .changed();
                }
            });
            ui.end_row();
            ui.label("Iterations at most");
            changed |= ui
                .add(egui::DragValue::new(&mut g.iterations).range(1..=10_000))
                .changed();
            ui.end_row();
            ui.label("Symmetry");
            ui.horizontal(|ui| {
                let mut s = g.symmetry.unwrap_or(SymmetryPlanes {
                    planes: [false; 3],
                    point: [0.0; 3],
                });
                let mut any = false;
                for (on, name) in s.planes.iter_mut().zip(["Right", "Top", "Front"]) {
                    if ui
                        .toggle_value(on, name)
                        .on_hover_text("Mirrored across a plane parallel to this standard plane")
                        .changed()
                    {
                        any = true;
                    }
                }
                if any {
                    g.symmetry = s.planes.contains(&true).then_some(s);
                    changed = true;
                }
            });
            ui.end_row();
            if let Some(s) = g.symmetry.as_mut() {
                ui.label("Through");
                ui.horizontal(|ui| {
                    for v in s.point.iter_mut() {
                        changed |= ui
                            .add(egui::DragValue::new(v).speed(0.5).suffix(" mm"))
                            .changed();
                    }
                });
                ui.end_row();
            }
        });
    changed
}

fn format_number_short(v: f64) -> String {
    if v >= 10.0 {
        format!("{v:.1}")
    } else if v >= 1.0 {
        format!("{v:.2}")
    } else {
        format!("{v:.3}")
    }
}

/// The outcomes: a plot of their mass against the axis picked, and a row each.
fn outcome_rows(
    ui: &mut egui::Ui,
    g: &Generated,
    picked: Option<u32>,
    y_axis: &mut ScatterAxis,
    settings: &GenerativeSettings,
) -> Option<GenAction> {
    let mut action = None;
    if g.outcomes.is_empty() {
        weak(ui, "Growing the first outcome…");
        return None;
    }
    let colors: BTreeMap<&str, [u8; 3]> = settings
        .materials
        .iter()
        .map(|m| (m.name.as_str(), m.appearance.color))
        .collect();
    let color = |name: &str| {
        let c = colors.get(name).copied().unwrap_or(OUTCOME_COLOR);
        egui::Color32::from_rgb(c[0], c[1], c[2])
    };
    egui::ComboBox::from_id_salt("generative_y_axis")
        .selected_text(y_axis.label())
        .width(200.0)
        .show_ui(ui, |ui| {
            for a in ScatterAxis::ALL {
                ui.selectable_value(y_axis, a, a.label());
            }
        });
    if let Some(id) = scatter(ui, g, picked, *y_axis, &color) {
        action = Some(GenAction::Pick(id));
    }
    for o in &g.outcomes {
        ui.horizontal(|ui| {
            let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
            ui.painter().rect_filled(rect, 2.0, color(&o.material));
            let failed = matches!(o.status, OutcomeStatus::Failed(_));
            let text = format!("{} · {} · {}", o.id, o.material, method_label(&o.method));
            let response = ui.add_enabled(
                !failed,
                egui::SelectableLabel::new(picked == Some(o.id), text),
            );
            if response.clicked() {
                action = Some(GenAction::Pick(o.id));
            }
            match &o.status {
                OutcomeStatus::Failed(why) => {
                    ui.label(
                        egui::RichText::new("failed")
                            .small()
                            .color(egui::Color32::from_rgb(210, 60, 60)),
                    )
                    .on_hover_text(why);
                }
                _ => {
                    ui.label(egui::RichText::new(format_value(o.mass, "kg")).small());
                    if let Some(f) = o.min_factor_of_safety {
                        let low = f < settings.safety_factor;
                        let text = egui::RichText::new(format!("FOS {f:.2}")).small();
                        ui.label(if low {
                            text.color(egui::Color32::from_rgb(210, 140, 30))
                        } else {
                            text.weak()
                        })
                        .on_hover_text(if low {
                            "Under the study's safety factor"
                        } else {
                            "The estimated factor of safety"
                        });
                    }
                }
            }
        });
    }
    action
}

/// The plot of the outcomes, mass across and `axis` up; the outcome clicked.
fn scatter(
    ui: &mut egui::Ui,
    g: &Generated,
    picked: Option<u32>,
    axis: ScatterAxis,
    color: &dyn Fn(&str) -> egui::Color32,
) -> Option<u32> {
    let points: Vec<(u32, f64, f64, &str)> = g
        .outcomes
        .iter()
        .filter_map(|o| Some((o.id, o.mass, axis.value(o)?, o.material.as_str())))
        .filter(|(_, m, _, _)| m.is_finite())
        .collect();
    if points.is_empty() {
        return None;
    }
    let (x_lo, x_hi) = padded(
        points.iter().map(|p| p.1).fold(f64::INFINITY, f64::min),
        points.iter().map(|p| p.1).fold(f64::NEG_INFINITY, f64::max),
    );
    let (y_lo, y_hi) = padded(
        points.iter().map(|p| p.2).fold(f64::INFINITY, f64::min),
        points.iter().map(|p| p.2).fold(f64::NEG_INFINITY, f64::max),
    );
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, 150.0), egui::Sense::click());
    let painter = ui.painter_at(rect);
    let visuals = ui.visuals();
    painter.rect_filled(rect, 3.0, visuals.extreme_bg_color);
    let plot = rect.shrink2(egui::vec2(8.0, 14.0));
    let at = |x: f64, y: f64| {
        egui::pos2(
            plot.left() + ((x - x_lo) / (x_hi - x_lo)) as f32 * plot.width(),
            plot.bottom() - ((y - y_lo) / (y_hi - y_lo)) as f32 * plot.height(),
        )
    };
    let ink = visuals.weak_text_color();
    let font = egui::FontId::proportional(10.0);
    painter.text(
        rect.left_bottom() + egui::vec2(4.0, -2.0),
        egui::Align2::LEFT_BOTTOM,
        format_value(x_lo.max(0.0), "kg"),
        font.clone(),
        ink,
    );
    painter.text(
        rect.right_bottom() + egui::vec2(-4.0, -2.0),
        egui::Align2::RIGHT_BOTTOM,
        format_value(x_hi, "kg"),
        font.clone(),
        ink,
    );
    painter.text(
        rect.left_top() + egui::vec2(4.0, 2.0),
        egui::Align2::LEFT_TOP,
        format_value(y_hi, axis.unit()),
        font,
        ink,
    );
    let mut nearest: Option<(u32, f32)> = None;
    let pointer = response.hover_pos();
    for (id, x, y, material) in &points {
        let p = at(*x, *y);
        let r = if picked == Some(*id) { 6.0 } else { 4.0 };
        painter.circle_filled(p, r, color(material));
        if picked == Some(*id) {
            painter.circle_stroke(
                p,
                r + 2.0,
                egui::Stroke::new(1.5_f32, visuals.strong_text_color()),
            );
        }
        if let Some(q) = pointer {
            let d = p.distance(q);
            if d < 10.0 && nearest.is_none_or(|(_, best)| d < best) {
                nearest = Some((*id, d));
            }
        }
    }
    let hovered = nearest.and_then(|(id, _)| g.outcome(id));
    let response = match hovered {
        Some(o) => response.on_hover_text(format!(
            "{} · {} · {}\n{}, {}",
            o.id,
            o.material,
            method_label(&o.method),
            format_value(o.mass, "kg"),
            axis.value(o)
                .map_or_else(String::new, |v| format_value(v, axis.unit()))
        )),
        None => response,
    };
    response
        .clicked()
        .then_some(nearest.map(|(id, _)| id))
        .flatten()
}

/// The picked outcome: its numbers, its iterations on a slider, show and export.
fn picked_rows(
    ui: &mut egui::Ui,
    o: &GenerativeOutcome,
    snapshot: &mut Option<usize>,
    show: &mut bool,
    show_obstacles: &mut bool,
    safety_factor: f64,
) -> Option<GenAction> {
    let mut action = None;
    ui.separator();
    ui.strong(format!(
        "Outcome {}: {}, {}",
        o.id,
        o.material,
        method_label(&o.method).to_lowercase()
    ));
    let status = match &o.status {
        OutcomeStatus::Converged => format!("settled after {} iterations", o.iterations),
        OutcomeStatus::Limit => format!("{} iterations, the limit: not settled", o.iterations),
        OutcomeStatus::Stopped => format!("stopped after {} iterations", o.iterations),
        OutcomeStatus::Failed(why) => why.clone(),
    };
    weak(ui, &format!("{status}, {:.0} s", o.seconds));
    egui::Grid::new("generative_picked")
        .num_columns(2)
        .spacing([10.0, 2.0])
        .show(ui, |ui| {
            ui.label("Mass");
            ui.label(format_value(o.mass, "kg"));
            ui.end_row();
            ui.label("Compliance");
            ui.label(format_value(o.compliance, "N·mm"))
                .on_hover_text("The work of the loads: the lower, the stiffer");
            ui.end_row();
            ui.label("Max displacement");
            ui.label(format_value(o.max_displacement, "mm"));
            ui.end_row();
            ui.label("Max von Mises");
            ui.label(format_value(o.max_von_mises, "MPa"))
                .on_hover_text("Estimated at the voxels' centres: a static study of the part made from it gives the exact one");
            ui.end_row();
            if let Some(f) = o.min_factor_of_safety {
                ui.label("Min factor of safety");
                let text = egui::RichText::new(format!("{f:.2}"));
                ui.label(if f < safety_factor {
                    text.color(egui::Color32::from_rgb(210, 140, 30))
                } else {
                    text
                });
                ui.end_row();
            }
        });
    if !o.snapshots.is_empty() {
        // The iterations kept, then the end.
        let last = o.snapshots.len();
        let mut at = snapshot.unwrap_or(last).min(last);
        let iteration = |i: usize| o.snapshots.get(i).map_or(o.iterations, |s| s.iteration);
        ui.horizontal(|ui| {
            ui.label("Iteration");
            let shown = iteration(at).to_string();
            if ui
                .add(
                    egui::Slider::new(&mut at, 0..=last)
                        .show_value(false)
                        .text(shown),
                )
                .changed()
            {
                *snapshot = (at < last).then_some(at);
                *show = true;
            }
        });
    }
    ui.horizontal(|ui| {
        if ui
            .checkbox(show, "Show it")
            .on_hover_text("In place of the model in the viewport")
            .changed()
        {
            action = Some(GenAction::Show(*show));
        }
        ui.checkbox(show_obstacles, "Obstacles")
            .on_hover_text("The obstacles drawn faintly around it");
        if ui.button("Export…").clicked() {
            action = Some(GenAction::Export);
        }
        if ui
            .button("Create Part…")
            .on_hover_text(
                "A new part file of the outcome: its surface made a solid, meeting the bodies \
                 kept, the starting shape and the bodies to stay clear of on their faces",
            )
            .clicked()
        {
            action = Some(GenAction::CreatePart);
        }
    });
    action
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxyde_model::FeatureId;

    fn body(feature: u32) -> StudyBody {
        StudyBody::part(BodyId::new(FeatureId::from_raw(feature), 0))
    }

    #[test]
    fn a_body_has_one_role_at_most() {
        let mut g = GenerativeSettings::default();
        set_role(&mut g, body(2), Role::Preserve);
        set_role(&mut g, body(3), Role::Obstacle);
        assert_eq!(role_of(&g, &body(2)), Role::Preserve);
        assert_eq!(role_of(&g, &body(3)), Role::Obstacle);
        g.obstacles[0].offset = 2.0;
        // The same role again keeps the offset.
        set_role(&mut g, body(3), Role::Obstacle);
        assert_eq!(g.obstacles[0].offset, 2.0);
        set_role(&mut g, body(2), Role::StartingShape);
        assert!(g.preserve.is_empty());
        assert_eq!(g.starting_shape, Some(body(2)));
        // A new starting shape replaces the one before.
        set_role(&mut g, body(4), Role::StartingShape);
        assert_eq!(role_of(&g, &body(2)), Role::None);
        set_role(&mut g, body(3), Role::None);
        assert!(g.obstacles.is_empty());
        let before = roles_hash(&g);
        set_role(&mut g, body(5), Role::Preserve);
        assert_ne!(roles_hash(&g), before);
    }

    #[test]
    fn methods_read_as_the_panel_says_them() {
        assert_eq!(method_label(&OutcomeMethod::Unrestricted), "Unrestricted");
        assert_eq!(
            method_label(&OutcomeMethod::Casting {
                direction: AxisDirection::MinusY,
                parting: None
            }),
            "Casting −Y"
        );
        assert_eq!(
            method_label(&OutcomeMethod::Casting {
                direction: AxisDirection::PlusZ,
                parting: Some([0.0; 3])
            }),
            "Casting ±Z"
        );
        assert_eq!(
            method_label(&OutcomeMethod::Cutting {
                direction: AxisDirection::PlusX
            }),
            "2-axis cutting along X"
        );
        assert_eq!(
            engine_method(&OutcomeMethod::Casting {
                direction: AxisDirection::MinusZ,
                parting: None
            }),
            Ok(Method::Casting {
                direction: Direction {
                    axis: 2,
                    positive: false
                },
                parting: None
            })
        );
    }

    fn steel() -> Material {
        let mut m = Material::new("Steel", "Steel", oxyde_model::Appearance::default(), 7850.0);
        m.mechanical = Some(Mechanical::new(200_000.0, 0.3, 250.0, 400.0));
        m
    }

    #[test]
    fn every_material_meets_every_method_direction() {
        let mut g = GenerativeSettings::default();
        g.methods.push(GenerativeMethod::Casting {
            directions: vec![AxisDirection::PlusY, AxisDirection::MinusY],
            parting: None,
        });
        let mut aluminium = steel();
        aluminium.name = "Aluminium".to_owned();
        aluminium.density = 2700.0;
        // Without study materials, the part's own.
        let plan = outcome_plan(&g, Some(&steel()));
        assert_eq!(plan.len(), 3);
        g.materials = vec![steel(), aluminium];
        let plan = outcome_plan(&g, None);
        let ids: Vec<u32> = plan.iter().map(|p| p.id).collect();
        assert_eq!(ids, [1, 2, 3, 4, 5, 6]);
        assert_eq!(plan[3].material, "Aluminium");
        assert_eq!(plan[3].method, OutcomeMethod::Unrestricted);
        assert_eq!(
            plan[5].method,
            OutcomeMethod::Casting {
                direction: AxisDirection::MinusY,
                parting: None
            }
        );
        let e = plan[3].elastic.as_ref().unwrap();
        assert_eq!((e.density, e.yield_strength), (2700.0, 250.0));
        // A material without a density is not one an outcome can weigh.
        let mut light = steel();
        light.density = 0.0;
        assert!(elastic_of(&light).is_err());
    }

    /// A box's surface, wound outwards, its sides labelled `first` to `first + 5`: −X, +X,
    /// −Y, +Y, −Z, +Z.
    fn cube(lo: [f64; 3], hi: [f64; 3], first: u32) -> SurfaceMesh {
        let mut s = SurfaceMesh::default();
        let corner = |i: usize| {
            [
                [lo, hi][i & 1][0],
                [lo, hi][(i >> 1) & 1][1],
                [lo, hi][i >> 2][2],
            ]
        };
        // Each side by its corners, counter-clockwise seen from outside.
        let sides: [[usize; 4]; 6] = [
            [0, 4, 6, 2],
            [1, 3, 7, 5],
            [0, 1, 5, 4],
            [2, 6, 7, 3],
            [0, 2, 3, 1],
            [4, 5, 7, 6],
        ];
        for (k, side) in sides.iter().enumerate() {
            let base = s.positions.len() as u32;
            s.positions.extend(side.iter().map(|&c| corner(c)));
            for t in [[0, 1, 2], [0, 2, 3]] {
                s.triangles.push(t.map(|i| base + i));
                s.labels.push(first + k as u32);
            }
        }
        s
    }

    fn pad_face(label: u32) -> crate::simulation_ui::SimFace {
        crate::simulation_ui::SimFace {
            occurrence: None,
            face: oxyde_model::FaceName::profile(FeatureId::from_raw(2 + label / 6), label % 6),
        }
    }

    /// A part of two pads 32 mm apart, both kept: the first held on its −X side, the
    /// second loaded 500 N down on its +X side.
    fn two_pads() -> (Surfaces, Study) {
        let pads = [
            cube([0.0, 0.0, 0.0], [4.0, 16.0, 8.0], 0),
            cube([36.0, 5.0, 2.0], [40.0, 11.0, 6.0], 6),
        ];
        let surfaces = Surfaces::for_test(
            pads.to_vec(),
            (0..12).map(pad_face).collect(),
            (0..12).map(|l| l / 6).collect(),
            vec![
                ("Body 1".to_owned(), Some(body(2).body), Some(steel())),
                ("Body 2".to_owned(), Some(body(3).body), Some(steel())),
            ],
        );
        let mut study = Study::of_type(StudyType::Generative, "Generative 1");
        study.add(ItemKind::Fixed, vec![pad_face(0)]);
        study.add(
            ItemKind::Force {
                newtons: -500.0,
                direction: crate::simulation_ui::LoadDirection::Y,
            },
            vec![pad_face(7)],
        );
        let g = &mut study.generative;
        set_role(g, body(2), Role::Preserve);
        set_role(g, body(3), Role::Preserve);
        // A fifth of the box in steel; voxels of 2 mm, a few hundred of them.
        g.mass_target = 0.2 * 40.0 * 16.0 * 8.0 * 7850.0 * 1e-9;
        g.margin = [Some(0.0); 3];
        g.resolution = MeshSize::ElementSize(2.0);
        g.iterations = 30;
        g.materials = vec![steel()];
        g.methods.push(GenerativeMethod::Cutting {
            directions: vec![AxisDirection::PlusZ],
        });
        g.methods.push(GenerativeMethod::Additive {
            directions: vec![AxisDirection::PlusY],
        });
        (surfaces, study)
    }

    #[test]
    fn a_study_is_refused_until_it_can_be_generated() {
        let (surfaces, study) = two_pads();
        assert!(engine_study(&study, &surfaces).is_ok());
        let mut loose = study.clone();
        set_role(&mut loose.generative, body(2), Role::Obstacle);
        let why = engine_study(&loose, &surfaces).unwrap_err();
        assert!(why.contains("Fixed-1") && why.contains("not kept"), "{why}");
        let mut light = study.clone();
        light.generative.mass_target = 0.0;
        assert!(
            engine_study(&light, &surfaces)
                .unwrap_err()
                .contains("mass target")
        );
        let mut empty = study.clone();
        empty.generative.preserve.clear();
        assert!(
            engine_study(&empty, &surfaces)
                .unwrap_err()
                .contains("Preserve")
        );
        let mut gone = study.clone();
        set_role(&mut gone.generative, body(9), Role::StartingShape);
        assert!(
            engine_study(&gone, &surfaces)
                .unwrap_err()
                .contains("no longer")
        );
        let estimate = voxel_estimate(&study, &surfaces).unwrap();
        assert_eq!(estimate, (2.0, 20 * 8 * 4));
        let mass = preserve_mass(&study, &surfaces, None, 7850.0);
        assert!((mass - (4.0 * 16.0 * 8.0 + 4.0 * 6.0 * 4.0) * 7850e-9).abs() < 1e-12);
    }

    /// A part of one body, a plate 40 × 10 × 4 mm, made the starting shape: kept 2 mm under
    /// its ends (Preserve Region), held at its −X end, loaded at its +X end.
    fn one_plate() -> (Surfaces, Study) {
        let surfaces = Surfaces::for_test(
            vec![cube([0.0, 0.0, 0.0], [40.0, 10.0, 4.0], 0)],
            (0..6).map(pad_face).collect(),
            vec![0; 6],
            vec![("Body 1".to_owned(), Some(body(2).body), Some(steel()))],
        );
        let mut study = Study::of_type(StudyType::Generative, "Generative 1");
        study.add(ItemKind::Preserve { depth: 2.0 }, vec![pad_face(0)]);
        study.add(ItemKind::Preserve { depth: 2.0 }, vec![pad_face(1)]);
        study.add(ItemKind::Fixed, vec![pad_face(0)]);
        study.add(
            ItemKind::Force {
                newtons: -500.0,
                direction: crate::simulation_ui::LoadDirection::Y,
            },
            vec![pad_face(1)],
        );
        let g = &mut study.generative;
        set_role(g, body(2), Role::StartingShape);
        g.mass_target = 0.4 * 40.0 * 10.0 * 4.0 * 7850.0 * 1e-9;
        g.resolution = MeshSize::ElementSize(1.0);
        g.materials = vec![steel()];
        (surfaces, study)
    }

    #[test]
    fn a_single_body_is_kept_by_its_regions() {
        let (surfaces, study) = one_plate();
        let engine = engine_study(&study, &surfaces).unwrap();
        // Two regions on the one body, which is the starting shape; no preserve body.
        assert!(engine.space.preserve.is_empty());
        assert_eq!(engine.space.region_bodies.len(), 1);
        let regions: Vec<(usize, &[u32], f64)> = engine
            .space
            .regions
            .iter()
            .map(|r| (r.body, r.labels.as_slice(), r.depth))
            .collect();
        assert_eq!(regions, [(0, &[0][..], 2.0), (0, &[1][..], 2.0)]);
        assert!(engine.space.starting_shape.is_some());
        // The regions are no fixtures or loads: one of each, in the one load case.
        assert_eq!((engine.fixtures.len(), engine.loads.len()), (1, 1));
        let (fixtures, loads) = item_names(&study.items);
        assert_eq!(
            (fixtures, loads),
            (vec!["Fixed-1".to_owned()], vec!["Force-1".to_owned()])
        );
        // Laid on voxels, the ends are kept and the middle is design.
        let grid = Grid::build(&engine, 0, &AtomicBool::new(false)).unwrap();
        let count = |k: Voxel| grid.kinds.iter().filter(|v| **v == k).count();
        assert_eq!(count(Voxel::Preserve), 2 * 2 * 10 * 4);
        assert_eq!(count(Voxel::Design), 36 * 10 * 4);

        // A fixture on a face nothing keeps, a region on an obstacle, nothing kept at all:
        // refused, each saying why.
        let mut loose = study.clone();
        loose.items[2].faces = vec![pad_face(2)];
        let why = engine_study(&loose, &surfaces).unwrap_err();
        assert!(why.contains("Fixed-1") && why.contains("not kept"), "{why}");
        let mut blocked = study.clone();
        set_role(&mut blocked.generative, body(2), Role::Obstacle);
        let why = engine_study(&blocked, &surfaces).unwrap_err();
        assert!(
            why.contains("Preserve-1") && why.contains("obstacle"),
            "{why}"
        );
        let mut none = study.clone();
        none.items
            .retain(|i| !matches!(i.kind, ItemKind::Preserve { .. }));
        let why = engine_study(&none, &surfaces).unwrap_err();
        assert!(why.contains("Preserve Region"), "{why}");

        // What is kept weighs what its voxels do.
        let census = census(&study, &surfaces).unwrap();
        assert_eq!((census.voxel, census.preserve), (1.0, 160));
        let mass = preserve_mass(&study, &surfaces, Some(&census), 7850.0);
        assert!((mass - 160.0 * 7850e-9).abs() < 1e-12);
    }

    #[test]
    fn a_study_set_up_to_disappoint_is_warned_about() {
        let warnings = |study: &Study, surfaces: &Surfaces| {
            let bodies: Vec<(StudyBody, String)> = surfaces
                .bodies
                .iter()
                .enumerate()
                .map(|(i, _)| (body(2 + i as u32), format!("Body {}", i + 1)))
                .collect();
            let materials = study_materials(&study.generative, None);
            setup_warnings(
                study,
                surfaces,
                &bodies,
                &materials,
                voxel_estimate(study, surfaces),
                census(study, surfaces).as_ref(),
            )
        };
        // Every body kept, no starting shape: the outcome can only add lumps; and the
        // second pad, 4 mm across, is about a voxel thick.
        let (surfaces, study) = two_pads();
        let said = warnings(&study, &surfaces);
        assert_eq!(said.len(), 2, "{said:?}");
        assert!(said[0].starts_with("Every body is kept"), "{said:?}");
        assert!(said[1].contains("Body 2"), "{said:?}");

        // The plate redesigned with its ends kept: nothing to say.
        let (surfaces, study) = one_plate();
        assert_eq!(warnings(&study, &surfaces), Vec::<String>::new());
        // Its ends kept 15 mm deep: three quarters of it kept.
        let mut deep = study.clone();
        for item in &mut deep.items {
            if let ItemKind::Preserve { depth } = &mut item.kind {
                *depth = 15.0;
            }
        }
        let said = warnings(&deep, &surfaces);
        assert!(
            said.iter().any(|w| w.starts_with("What is kept is 75 %")),
            "{said:?}"
        );
        // Voxels of 3 mm for regions 2 mm deep; and no material.
        let mut coarse = study.clone();
        coarse.generative.resolution = MeshSize::ElementSize(3.0);
        coarse.generative.materials.clear();
        let said = warnings(&coarse, &surfaces);
        assert!(
            said.iter().any(|w| w.starts_with("No material")),
            "{said:?}"
        );
        assert!(
            said.iter()
                .any(|w| w.contains("(3.00 mm)") && w.contains("Preserve-1 (2 mm deep)")),
            "{said:?}"
        );
    }

    #[test]
    fn outcomes_are_generated_one_by_one_and_saved_with_the_study() {
        let (surfaces, study) = two_pads();
        let engine = engine_study(&study, &surfaces).unwrap();
        let plan = outcome_plan(&study.generative, None);
        assert_eq!(plan.len(), 3);
        let shared = Mutex::new(GenShared::default());
        let cancel = AtomicBool::new(false);
        generation_thread(
            GenInput {
                study: engine,
                plan,
                gpu: false,
            },
            &shared,
            &cancel,
        );
        let mut shared = shared.into_inner().unwrap();
        assert_eq!(shared.finished, Some(Ok(())));
        let grid = shared.grid.take().unwrap();
        let outcomes = std::mem::take(&mut shared.outcomes);
        assert_eq!(outcomes.iter().map(|o| o.id).collect::<Vec<_>>(), [1, 2, 3]);
        for o in &outcomes {
            assert!(
                !matches!(o.status, OutcomeStatus::Failed(_)),
                "{:?}",
                o.status
            );
            assert_eq!(o.density.len(), grid.voxel_count());
            assert!(o.iterations > 0 && o.history.len() == o.iterations as usize);
            assert!(o.compliance > 0.0 && o.max_von_mises > 0.0);
            assert!((o.mass - study.generative.mass_target).abs() < 0.1 * o.mass);
            assert!(o.min_factor_of_safety.is_some());
        }
        // Printed up +Y: nothing above the first layer hangs over air.
        let printed = &outcomes[2].density;
        let n = grid.counts;
        for v in 0..grid.voxel_count() {
            let c = [v % n[0], (v / n[0]) % n[1], v / (n[0] * n[1])];
            if c[1] == 0 || printed[v] < 128 || grid.kinds[v] == Voxel::Preserve {
                continue;
            }
            let held = (c[0].saturating_sub(1)..=(c[0] + 1).min(n[0] - 1)).any(|x| {
                (c[2].saturating_sub(1)..=(c[2] + 1).min(n[2] - 1))
                    .any(|z| printed[grid.index([x, c[1] - 1, z])] >= 128)
            });
            assert!(held, "{c:?} hangs");
        }
        // 2-axis cutting along Z: every column along Z alike.
        let cut = &outcomes[1].density;
        let n = grid.counts;
        for j in 0..n[1] {
            for i in 0..n[0] {
                let first = cut[grid.index([i, j, 0])] >= 128;
                assert!((0..n[2]).all(|k| (cut[grid.index([i, j, k])] >= 128) == first));
            }
        }
        // The surface of an outcome, and its scene with the pads over it.
        let shape = surface(&grid, &outcomes[0].density, SMOOTHING);
        assert!(!shape.triangles.is_empty());
        let pads: Vec<&SurfaceMesh> = surfaces.solids.iter().collect();
        let scene = outcome_scene(&shape, &pads, &pads[..1], OUTCOME_COLOR);
        assert!(!scene.is_empty());

        // Saved and read back, the same.
        let generated = Generated {
            id: 1,
            revision: 0,
            grid,
            outcomes,
            fingerprint: None,
            seconds: 3.0,
            card: None,
        };
        let file =
            generative_results_file(&generated, "sim", "Generative 1", Fingerprint::default());
        let bytes = oxyde_format::results::write_results(std::io::Cursor::new(Vec::new()), &file)
            .unwrap()
            .into_inner();
        let read = oxyde_format::results::read_results(std::io::Cursor::new(bytes)).unwrap();
        let back = generated_from(&read, 7).unwrap();
        assert_eq!(back.outcomes, generated.outcomes);
        assert_eq!(back.grid.kinds, generated.grid.kinds);
        assert_eq!(back.grid.counts, generated.grid.counts);
        assert_eq!(back.revision, 7);
        // The plot's values: none for a failed outcome.
        assert!(ScatterAxis::MaxStress.value(&back.outcomes[2]).is_some());
        let plan = &outcome_plan(&study.generative, None)[0];
        let failed = failed_outcome(plan, "Why".to_owned(), 0.0);
        assert_eq!(ScatterAxis::MaxStress.value(&failed), None);
        assert!(ScatterAxis::Safety.value(&back.outcomes[0]).is_some());
    }

    #[test]
    fn load_cases_name_the_items_they_carry() {
        let (surfaces, mut study) = two_pads();
        // A second load on the loaded pad, and two cases.
        let push = study.add(ItemKind::Pressure { mpa: 2.0 }, vec![pad_face(8)]);
        assert_eq!(study.items[push].name, "Pressure-1");
        study.generative.load_cases = vec![
            LoadCase {
                name: "Load Case 1".to_owned(),
                fixtures: vec!["Fixed-1".to_owned()],
                loads: vec!["Force-1".to_owned()],
            },
            LoadCase {
                name: "Load Case 2".to_owned(),
                fixtures: vec!["Fixed-1".to_owned()],
                loads: vec!["Pressure-1".to_owned()],
            },
        ];
        let engine = engine_study(&study, &surfaces).unwrap();
        assert_eq!(engine.loads.len(), 2);
        assert_eq!(engine.load_cases.len(), 2);
        assert_eq!(engine.load_cases[1].loads, [1]);
        assert_eq!(engine.load_cases[1].fixtures, [0]);
        // A new fixture joins every case, a new load the last one.
        let roller = study.add(ItemKind::Roller, vec![pad_face(1)]);
        join_load_cases(&mut study, roller);
        let force = study.add(
            ItemKind::Force {
                newtons: 10.0,
                direction: crate::simulation_ui::LoadDirection::Normal,
            },
            vec![pad_face(9)],
        );
        join_load_cases(&mut study, force);
        let cases = &study.generative.load_cases;
        assert!(
            cases
                .iter()
                .all(|c| c.fixtures.contains(&"Roller/Slider-1".to_owned()))
        );
        assert_eq!(cases[1].loads, ["Pressure-1", "Force-2"]);
        // Deleted, it leaves them; a case left with no load is refused.
        leave_load_cases(&mut study, "Pressure-1");
        leave_load_cases(&mut study, "Force-2");
        let why = engine_study(&study, &surfaces).unwrap_err();
        assert!(why.contains("Load Case 2"), "{why}");
    }

    #[test]
    fn minimizing_mass_needs_no_target_and_keeps_the_stress_down() {
        let (surfaces, mut study) = two_pads();
        let g = &mut study.generative;
        g.objective = GenerativeObjective::MinMass;
        g.mass_target = 0.0;
        g.safety_factor = 2.0;
        g.methods = vec![GenerativeMethod::Milling {
            directions: vec![AxisDirection::PlusZ],
            tool_diameter: Some(4.0),
        }];
        let engine = engine_study(&study, &surfaces).unwrap();
        assert_eq!(
            engine.objective,
            Objective::MinMass {
                safety_factor: 2.0,
                displacement_limit: None
            }
        );
        let plan = outcome_plan(&study.generative, None);
        assert_eq!(method_label(&plan[0].method), "Milling from +Z");
        let shared = Mutex::new(GenShared::default());
        let cancel = AtomicBool::new(false);
        generation_thread(
            GenInput {
                study: engine,
                plan,
                gpu: false,
            },
            &shared,
            &cancel,
        );
        let shared = shared.into_inner().unwrap();
        let o = &shared.outcomes[0];
        assert!(
            !matches!(o.status, OutcomeStatus::Failed(_)),
            "{:?}",
            o.status
        );
        // Steel's 250 MPa over 2, with what a coarse grid's estimate allows.
        assert!(o.max_von_mises <= 1.1 * 125.0, "{} MPa", o.max_von_mises);
        let full = (40.0 * 16.0 * 8.0) * 7850e-9;
        assert!(o.mass < full, "{} kg", o.mass);
    }

    #[test]
    fn in_an_assembly_bodies_are_named_by_their_occurrence() {
        // The two pads as components 1 and 2 of an assembly (the second inside a
        // sub-assembly, component 4 of 2).
        let (mut surfaces, _) = two_pads();
        let occurrences = [
            crate::assembly::Occurrence::top(oxyde_assembly::ComponentId(1)),
            crate::assembly::Occurrence {
                top: oxyde_assembly::ComponentId(2),
                path: vec![oxyde_assembly::ComponentId(4)],
            },
        ];
        surfaces.assembly = true;
        for (b, o) in surfaces.bodies.iter_mut().zip(&occurrences) {
            b.occurrence = Some(o.clone());
        }
        for (f, b) in surfaces.faces.iter_mut().zip(&surfaces.face_body) {
            f.occurrence = Some(occurrences[*b as usize].clone());
        }
        let bodies: Vec<StudyBody> = surfaces
            .bodies
            .iter()
            .filter_map(|b| b.study_body())
            .collect();
        assert_eq!(
            bodies[1],
            StudyBody::component(
                oxyde_assembly::ComponentId(2),
                vec![oxyde_assembly::ComponentId(4)],
                body(3).body
            )
        );
        let mut study = Study::of_type(StudyType::Generative, "Generative 1");
        study.add(ItemKind::Fixed, vec![surfaces.faces[0].clone()]);
        study.add(
            ItemKind::Force {
                newtons: -500.0,
                direction: crate::simulation_ui::LoadDirection::Y,
            },
            vec![surfaces.faces[7].clone()],
        );
        let g = &mut study.generative;
        for b in &bodies {
            set_role(g, b.clone(), Role::Preserve);
        }
        g.mass_target = 0.03;
        g.resolution = MeshSize::ElementSize(2.0);
        // A part's body name finds nothing in the assembly.
        assert!(engine_study(&study, &surfaces).is_ok());
        study.generative.preserve[0] = body(2);
        let why = engine_study(&study, &surfaces).unwrap_err();
        assert!(why.contains("no longer in the model"), "{why}");
    }

    #[test]
    fn a_study_file_keeps_the_generative_settings() {
        let (_, study) = two_pads();
        let file = crate::simulation_files::file_study(&study);
        assert_eq!(file.generative, study.generative);
        assert_eq!((file.fixtures.len(), file.loads.len()), (1, 1));
        assert!(file.contacts.is_empty());
        let back = crate::simulation_files::study_of(&file);
        assert_eq!(back.generative, study.generative);
        assert_eq!(back.items, study.items);

        // One body redesigned: its preserved regions go in the generative block, its room
        // along each axis too, and come back as the study's items.
        let (_, mut study) = one_plate();
        study.generative.margin = [None, Some(0.0), None];
        let file = crate::simulation_files::file_study(&study);
        let names: Vec<(&str, f64)> = file
            .generative
            .preserved
            .iter()
            .map(|p| (p.name.as_str(), p.depth))
            .collect();
        assert_eq!(names, [("Preserve-1", 2.0), ("Preserve-2", 2.0)]);
        assert!(file.topology.preserved.is_empty());
        assert_eq!(file.generative.margin, [None, Some(0.0), None]);
        assert_eq!((file.fixtures.len(), file.loads.len()), (1, 1));
        let back = crate::simulation_files::study_of(&file);
        assert!(back.generative.preserved.is_empty());
        assert_eq!(back.generative, study.generative);
        // Read back, the regions come after the fixtures and loads.
        let by_name = |s: &Study| {
            let mut items = s.items.clone();
            items.sort_by(|a, b| a.name.cmp(&b.name));
            items
        };
        assert_eq!(by_name(&back), by_name(&study));
    }
}
