//! Heat conduction in solids: the steady and transient thermal studies of the Simulation
//! tab, as SolidWorks Simulation and Fusion 360 run them, and the temperatures a thermal
//! stress study expands the model by ([`crate::solve::ThermalStrain`]).
//!
//! - **The equation.** `ρc ∂T/∂t = ∇·(k ∇T) + Q` in each body, with `k` its conductivity,
//!   `ρc` its heat capacity per volume and `Q` heat made inside it; at the boundary, faces
//!   held at a temperature, heat flowing in (a total power spread by area, or a flux),
//!   convection to a fluid (`h (T − T∞)` leaving) and radiation to the surroundings
//!   (`εσ (T⁴ − T∞⁴)`, in kelvin). Faces with none of them are insulated.
//! - **Elements.** The same four- and ten-node tetrahedra as the static solve, straight
//!   sided: conduction `∫ k ∇Nₐ·∇N_b` with its integration points, heat capacity
//!   `∫ ρc Nₐ N_b` exactly (the products of barycentric monomials integrate in closed
//!   form), boundary terms on the facets' three- or six-node triangles with a seven-point
//!   rule exact to the fifth degree.
//! - **Contacts** conduct perfectly, whatever their mechanical kind: a contact's slave nodes
//!   are tied to the master side's facets as in [`crate::contact`], the gap in temperature
//!   held to zero by an augmented Lagrangian (a conductance of `30 k / h` per area and a
//!   multiplier updated after each solve), as SolidWorks' bonded thermal contact does.
//! - **Fixed temperatures** are eliminated: their rows and columns leave the system, their
//!   values move to the right-hand side.
//! - **Radiation** is non-linear: Newton's method on `εσ T⁴`, whose Jacobian `∫ 4εσ T³ Nₐ N_b`
//!   is symmetric and positive, so each step is a conduction problem like the others.
//! - **Transient studies** march by backward Euler (`(C/Δt + K) Tⁿ⁺¹ = C/Δt Tⁿ + f`), which
//!   is stable at any step: the model starts at a uniform temperature, the boundary
//!   conditions apply from the first instant, and the temperatures are kept at evenly
//!   spaced times for playback.
//! - **The solve** is conjugate gradients preconditioned by the static solve's multigrid
//!   ([`crate::amg`]) with one mode (a uniform temperature), Jacobi when it cannot be
//!   built; the matrix is the same at every step unless radiation changes it.
//!
//! Units: millimetres, seconds, watts and degrees Celsius; conductivity in W/(m·K), heat
//! capacity in J/(kg·K), density in kg/m³, film coefficients and fluxes per square metre,
//! as material libraries list them. Heat flux comes out in W/m².

// Indices into the mesh (checked by `TetMesh::validate` first) and into arrays sized here.
#![allow(clippy::indexing_slicing)]

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::Facet;
use crate::amg::{BsrRef, Multigrid};
use crate::contact::{self, Contact, Tie};
use crate::material::Elastic;
use crate::math::{V3, add, norm, scale};
use crate::mesh::{EDGES, TetMesh};
use crate::solve::{
    Geometry, SolveOptions, facet_nodes, geometry, integration_points, node_points, pieces,
    shape_gradients, thread_count,
};

/// The Stefan–Boltzmann constant, W/(mm²·K⁴).
const STEFAN_BOLTZMANN: f64 = 5.670_374_419e-14;
/// 0 °C in kelvin.
pub const KELVIN: f64 = 273.15;
/// Radiation and contact iterations stop when no temperature moves by more than this
/// fraction of the temperature range (and a millikelvin).
const CHANGE_TOLERANCE: f64 = 1e-6;
/// At most this many Newton (and contact) solves per steady state or time step.
const MAX_NONLINEAR: usize = 60;

/// How a body's material conducts and stores heat.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ThermalMaterial {
    /// W/(m·K).
    pub conductivity: f64,
    /// J/(kg·K); only transient studies need it.
    pub specific_heat: f64,
    /// kg/m³; only transient studies need it.
    pub density: f64,
}

impl ThermalMaterial {
    pub const fn new(conductivity: f64, specific_heat: f64, density: f64) -> Self {
        Self {
            conductivity,
            specific_heat,
            density,
        }
    }

    /// Whether a steady study can use it: a positive conductivity, and a heat capacity
    /// and density that are not negative.
    pub fn is_valid(&self) -> bool {
        self.conductivity.is_finite()
            && self.conductivity > 0.0
            && self.specific_heat.is_finite()
            && self.specific_heat >= 0.0
            && self.density.is_finite()
            && self.density >= 0.0
    }

    /// Whether a transient study can use it too: it stores heat.
    pub fn stores_heat(&self) -> bool {
        self.is_valid() && self.specific_heat > 0.0 && self.density > 0.0
    }

    /// W/(mm·K).
    fn conductivity_mm(&self) -> f64 {
        self.conductivity * 1e-3
    }

    /// J/(mm³·K).
    fn capacity_mm(&self) -> f64 {
        self.density * 1e-9 * self.specific_heat
    }
}

/// What a thermal load does to its faces.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ThermalLoadKind {
    /// Held at a temperature, °C.
    Temperature { celsius: f64 },
    /// A total heat flow into the faces, W, shared by area; negative takes heat out.
    HeatPower { watts: f64 },
    /// A heat flux into the faces, W/m²; negative takes heat out.
    HeatFlux { flux: f64 },
    /// Convection to a fluid at `ambient` °C, with a film coefficient in W/(m²·K).
    Convection { coefficient: f64, ambient: f64 },
    /// Radiation to surroundings at `ambient` °C, with an emissivity from 0 to 1.
    Radiation { emissivity: f64, ambient: f64 },
}

impl ThermalLoadKind {
    /// Whether it sets the temperature level (a steady study needs one per piece).
    fn anchors(&self) -> bool {
        match *self {
            ThermalLoadKind::Temperature { .. } => true,
            ThermalLoadKind::Convection { coefficient, .. } => coefficient > 0.0,
            ThermalLoadKind::Radiation { emissivity, .. } => emissivity > 0.0,
            ThermalLoadKind::HeatPower { .. } | ThermalLoadKind::HeatFlux { .. } => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ThermalLoad {
    pub kind: ThermalLoadKind,
    /// The face labels it acts on.
    pub labels: Vec<u32>,
}

/// Heat made inside bodies, W in all, spread evenly through their volume.
#[derive(Debug, Clone, PartialEq)]
pub struct InternalHeat {
    pub bodies: Vec<u32>,
    pub watts: f64,
}

/// How a transient study marches in time.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Transient {
    /// s.
    pub duration: f64,
    /// Time steps over the duration.
    pub steps: usize,
    /// The uniform temperature the model starts at, °C.
    pub initial: f64,
    /// At most this many sets of temperatures kept, the start and the end included.
    pub frames: usize,
}

/// What a thermal study holds.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ThermalStudy {
    pub loads: Vec<ThermalLoad>,
    pub internal: Vec<InternalHeat>,
    /// Contacts between bodies meshed apart; every kind conducts perfectly.
    pub contacts: Vec<Contact>,
    /// `None` for the steady state.
    pub transient: Option<Transient>,
}

/// The temperatures at one time of a transient study.
#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    /// s from the start.
    pub time: f64,
    /// °C per node.
    pub temperature: Vec<f64>,
}

/// The largest and smallest values of a thermal study's results and where they are.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ThermalSummary {
    /// °C, and the node.
    pub max_temperature: (f64, u32),
    pub min_temperature: (f64, u32),
    /// W/m², and the node.
    pub max_heat_flux: (f64, u32),
}

/// The results of a thermal study, per node of the mesh.
#[derive(Debug, Clone, PartialEq)]
pub struct ThermalResults {
    /// °C: the steady state, or the end of a transient study.
    pub temperature: Vec<f64>,
    /// W/m², `−k ∇T` averaged over the elements around each node, at the same time.
    pub heat_flux: Vec<V3>,
    /// A transient study's temperatures at the times kept, from the start; empty for a
    /// steady one.
    pub frames: Vec<Frame>,
    /// Conjugate gradient iterations, over every solve.
    pub iterations: usize,
    /// The residual reached by the last solve, as a fraction of its right-hand side.
    pub residual: f64,
    /// Linear solves for radiation and contacts, over every step (one per step without
    /// them).
    pub nonlinear_iterations: usize,
}

impl ThermalResults {
    /// The length of the heat flux at each node, W/m².
    pub fn heat_flux_magnitude(&self) -> Vec<f64> {
        self.heat_flux.iter().map(|q| norm(*q)).collect()
    }

    pub fn summary(&self) -> ThermalSummary {
        let pick = |values: &mut dyn Iterator<Item = f64>, larger: bool| {
            let mut best: Option<(f64, u32)> = None;
            for (i, v) in values.enumerate() {
                if !v.is_finite() {
                    continue;
                }
                let better = best.is_none_or(|(b, _)| if larger { v > b } else { v < b });
                if better {
                    best = Some((v, i as u32));
                }
            }
            best.unwrap_or((0.0, 0))
        };
        ThermalSummary {
            max_temperature: pick(&mut self.temperature.iter().copied(), true),
            min_temperature: pick(&mut self.temperature.iter().copied(), false),
            max_heat_flux: pick(&mut self.heat_flux.iter().map(|q| norm(*q)), true),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ThermalError {
    /// The mesh is not usable: why.
    Invalid(String),
    /// The material of this body is missing or not usable (no conductivity).
    BadMaterial {
        body: u32,
    },
    /// A transient study needs this body's specific heat and density.
    NoHeatCapacity {
        body: u32,
    },
    /// Nothing sets the temperature of the piece of the mesh holding this body: a steady
    /// study needs a fixed temperature, convection or radiation on it (or on a part it
    /// touches).
    NotAnchored {
        body: u32,
    },
    /// A setting cannot be used: why.
    BadStudy(String),
    /// The solve did not reach its tolerance.
    NotConverged {
        iterations: usize,
        residual: f64,
    },
    Cancelled,
}

impl std::fmt::Display for ThermalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ThermalError::Invalid(why) => write!(f, "the mesh cannot be used: {why}"),
            ThermalError::BadMaterial { body } => {
                write!(f, "body {body} has no usable thermal conductivity")
            }
            ThermalError::NoHeatCapacity { body } => write!(
                f,
                "body {body} has no specific heat or density, which a transient study needs"
            ),
            ThermalError::NotAnchored { body } => write!(
                f,
                "nothing sets the temperature of body {body}: add a Temperature, a Convection or a Radiation on it (or on a part it touches)"
            ),
            ThermalError::BadStudy(why) => write!(f, "{why}"),
            ThermalError::NotConverged {
                iterations,
                residual,
            } => write!(
                f,
                "the thermal solve did not converge ({iterations} iterations, residual {residual:.1e})"
            ),
            ThermalError::Cancelled => write!(f, "cancelled"),
        }
    }
}

// ---- Shape functions ------------------------------------------------------------------------

/// The element's shape functions at barycentric coordinates `l`: the corners, then the
/// edge nodes in [`EDGES`] order for ten-node elements.
pub(crate) fn shape_values(l: [f64; 4], quadratic: bool, out: &mut [f64; 10]) -> usize {
    if !quadratic {
        out[..4].copy_from_slice(&l);
        return 4;
    }
    for i in 0..4 {
        out[i] = l[i] * (2.0 * l[i] - 1.0);
    }
    for (k, &(i, j)) in EDGES.iter().enumerate() {
        out[4 + k] = 4.0 * l[i] * l[j];
    }
    10
}

/// A polynomial in barycentric coordinates: coefficients of monomials `Π lᵢ^aᵢ`.
type Polynomial = Vec<(f64, [u8; 4])>;

fn element_polynomials(quadratic: bool) -> Vec<Polynomial> {
    let unit = |i: usize, power: u8| {
        let mut a = [0u8; 4];
        a[i] = power;
        a
    };
    let mut out: Vec<Polynomial> = Vec::new();
    for i in 0..4 {
        if quadratic {
            out.push(vec![(2.0, unit(i, 2)), (-1.0, unit(i, 1))]);
        } else {
            out.push(vec![(1.0, unit(i, 1))]);
        }
    }
    if quadratic {
        for &(i, j) in &EDGES {
            let mut a = [0u8; 4];
            a[i] = 1;
            a[j] = 1;
            out.push(vec![(4.0, a)]);
        }
    }
    out
}

fn factorial(n: u32) -> f64 {
    (1..=n).map(f64::from).product()
}

/// `∫ Nₐ N_b dV` over an element of unit volume, row-major `n × n`: exact, from
/// `∫ Π lᵢ^aᵢ dV = 6 V Π aᵢ! / (Σ aᵢ + 3)!`.
fn reference_mass(quadratic: bool) -> Vec<f64> {
    let shapes = element_polynomials(quadratic);
    let n = shapes.len();
    let mut out = vec![0.0; n * n];
    for a in 0..n {
        for b in 0..n {
            let mut sum = 0.0;
            for &(ca, pa) in &shapes[a] {
                for &(cb, pb) in &shapes[b] {
                    let powers: [u32; 4] = std::array::from_fn(|k| u32::from(pa[k] + pb[k]));
                    let numerator: f64 = powers.iter().map(|&p| factorial(p)).product();
                    let total: u32 = powers.iter().sum();
                    sum += ca * cb * 6.0 * numerator / factorial(total + 3);
                }
            }
            out[a * n + b] = sum;
        }
    }
    out
}

/// A seven-point rule on the triangle, exact for polynomials of degree 5 (Dunavant):
/// barycentric points and weights as fractions of the area.
fn triangle_points() -> [([f64; 3], f64); 7] {
    let (a1, b1, w1) = (
        0.059_715_871_789_770,
        0.470_142_064_105_115,
        0.132_394_152_788_506,
    );
    let (a2, b2, w2) = (
        0.797_426_985_353_087,
        0.101_286_507_323_456,
        0.125_939_180_544_827,
    );
    let third = 1.0 / 3.0;
    [
        ([third, third, third], 0.225),
        ([a1, b1, b1], w1),
        ([b1, a1, b1], w1),
        ([b1, b1, a1], w1),
        ([a2, b2, b2], w2),
        ([b2, a2, b2], w2),
        ([b2, b2, a2], w2),
    ]
}

/// A facet's shape functions at barycentric `l`: its corners, then (six-node) the middles
/// of its edges 0–1, 1–2 and 2–0, in the order [`facet_nodes`] lists its nodes.
fn triangle_shapes(l: [f64; 3], quadratic: bool) -> ([f64; 6], usize) {
    let mut out = [0.0; 6];
    if !quadratic {
        out[..3].copy_from_slice(&l);
        return (out, 3);
    }
    for i in 0..3 {
        out[i] = l[i] * (2.0 * l[i] - 1.0);
    }
    out[3] = 4.0 * l[0] * l[1];
    out[4] = 4.0 * l[1] * l[2];
    out[5] = 4.0 * l[2] * l[0];
    (out, 6)
}

// ---- The matrix -------------------------------------------------------------------------------

/// A symmetric sparse matrix, row by row, columns sorted.
#[derive(Clone)]
struct Csr {
    row_start: Vec<usize>,
    columns: Vec<u32>,
    values: Vec<f64>,
}

impl Csr {
    /// The pattern: an entry for every pair of nodes sharing an element or a contact tie.
    fn pattern(mesh: &TetMesh, ties: &[Tie]) -> Csr {
        let n = mesh.nodes.len();
        let mut pairs: Vec<u64> = Vec::new();
        for e in 0..mesh.tets.len() {
            let (nodes, count) = mesh.element_nodes(e);
            for &a in &nodes[..count] {
                for &b in &nodes[..count] {
                    pairs.push((u64::from(a) << 32) | u64::from(b));
                }
            }
        }
        for tie in ties {
            for (a, _) in tie.terms() {
                for (b, _) in tie.terms() {
                    pairs.push((u64::from(a) << 32) | u64::from(b));
                }
            }
        }
        // Every row has its diagonal, even a node in no element.
        pairs.extend((0..n as u64).map(|i| (i << 32) | i));
        pairs.sort_unstable();
        pairs.dedup();
        let mut row_start = vec![0usize; n + 1];
        for &p in &pairs {
            row_start[(p >> 32) as usize + 1] += 1;
        }
        for i in 0..n {
            row_start[i + 1] += row_start[i];
        }
        let columns = pairs.iter().map(|&p| p as u32).collect();
        Csr {
            row_start,
            columns,
            values: vec![0.0; pairs.len()],
        }
    }

    fn rows(&self) -> usize {
        self.row_start.len() - 1
    }

    fn slot(&self, row: usize, col: u32) -> Option<usize> {
        let span = self.row_start[row]..self.row_start[row + 1];
        let k = self.columns[span.clone()].binary_search(&col).ok()?;
        Some(span.start + k)
    }

    fn add(&mut self, row: usize, col: u32, value: f64) {
        if let Some(k) = self.slot(row, col) {
            self.values[k] += value;
        }
    }

    fn diagonal(&self, row: usize) -> f64 {
        self.slot(row, row as u32).map_or(0.0, |k| self.values[k])
    }

    fn view(&self) -> BsrRef<'_> {
        BsrRef {
            r: 1,
            c: 1,
            row_start: &self.row_start,
            columns: &self.columns,
            values: &self.values,
        }
    }
}

/// Row ranges of about equal numbers of entries, one per thread.
fn row_ranges(m: &Csr, threads: usize) -> Vec<(usize, usize)> {
    let rows = m.rows();
    let total = m.values.len().max(1);
    let mut out = Vec::with_capacity(threads);
    let mut start = 0;
    for t in 1..=threads {
        let target = total * t / threads;
        let mut end = start;
        while end < rows && (m.row_start[end] < target || t == threads) {
            end += 1;
        }
        out.push((start, end));
        start = end;
    }
    out.retain(|&(a, b)| b > a);
    out
}

/// `y = A x`, on several threads when it pays.
fn multiply(m: &Csr, ranges: &[(usize, usize)], x: &[f64], y: &mut [f64]) {
    let rows_of = |(r0, r1): (usize, usize), out: &mut [f64]| {
        for (row, o) in (r0..r1).zip(out.iter_mut()) {
            let mut sum = 0.0;
            for k in m.row_start[row]..m.row_start[row + 1] {
                sum += m.values[k] * x[m.columns[k] as usize];
            }
            *o = sum;
        }
    };
    if ranges.len() <= 1 || m.rows() < 20_000 {
        rows_of((0, m.rows()), y);
        return;
    }
    std::thread::scope(|s| {
        let mut rest = y;
        let mut taken = 0;
        for &(r0, r1) in ranges {
            let (mine, tail) = std::mem::take(&mut rest).split_at_mut(r1 - taken);
            let mine = &mut mine[r0 - taken..];
            taken = r1;
            rest = tail;
            s.spawn(move || rows_of((r0, r1), mine));
        }
    });
}

// ---- The study --------------------------------------------------------------------------------

/// Everything assembled once: the conduction matrix with convection and the ties'
/// conductances, the heat capacity, the fixed loads, the fixed temperatures.
struct Assembled {
    /// Conduction, convection and contact conductances, W/K.
    conduction: Csr,
    /// Heat capacity, J/K (zero for a steady study).
    capacity: Option<Csr>,
    /// Heat flowing in, W: fluxes, powers, internal heat, convection's ambient share.
    heat: Vec<f64>,
    /// The temperature a node is held at, °C.
    fixed: Vec<Option<f64>>,
    /// Facets that radiate, with their emissivity and surroundings' temperature (K).
    radiating: Vec<(Facet, f64, f64)>,
}

/// Solves the thermal study on the mesh, `materials[b]` being body `b`'s. `progress` hears
/// the fraction done, from 0 to 1.
pub fn solve_thermal(
    mesh: &TetMesh,
    materials: &[ThermalMaterial],
    study: &ThermalStudy,
    options: &SolveOptions,
    cancel: &AtomicBool,
    progress: &dyn Fn(f64),
) -> Result<ThermalResults, ThermalError> {
    mesh.validate().map_err(ThermalError::Invalid)?;
    if mesh.tets.is_empty() {
        return Err(ThermalError::Invalid("it has no elements".to_owned()));
    }
    for &b in &mesh.bodies {
        let Some(m) = materials.get(b as usize).filter(|m| m.is_valid()) else {
            return Err(ThermalError::BadMaterial { body: b });
        };
        if study.transient.is_some() && !m.stores_heat() {
            return Err(ThermalError::NoHeatCapacity { body: b });
        }
    }
    if let Some(t) = &study.transient
        && !(t.duration.is_finite() && t.duration > 0.0 && t.steps > 0)
    {
        return Err(ThermalError::BadStudy(
            "a transient study needs a positive duration and at least one time step".to_owned(),
        ));
    }
    let geometries: Vec<Geometry> = (0..mesh.tets.len())
        .map(|e| geometry(mesh, e))
        .collect::<Option<_>>()
        .ok_or_else(|| ThermalError::Invalid("an element is flat".to_owned()))?;
    // The ties of the contacts, with conductances from the conductivities (in place of the
    // moduli the static solve pairs with).
    let as_elastic: Vec<Elastic> = materials
        .iter()
        .map(|m| Elastic::new(m.conductivity_mm(), 0.3, 0.0, 0.0))
        .collect();
    let pairing = contact::pair(mesh, &study.contacts, &as_elastic);
    let ties = &pairing.ties;
    let assembled = assemble(mesh, materials, study, &geometries, ties)?;
    if study.transient.is_none()
        && let Some(body) = unanchored(mesh, study, &assembled, ties)
    {
        return Err(ThermalError::NotAnchored { body });
    }
    if cancel.load(Ordering::Relaxed) {
        return Err(ThermalError::Cancelled);
    }
    progress(0.05);

    let threads = thread_count(options.threads);
    let dt = study.transient.map(|t| t.duration / t.steps as f64);
    // Without radiation the matrix is the same for every solve: made, and its multigrid
    // built, once.
    let constant = assembled
        .radiating
        .is_empty()
        .then(|| prepare(&assembled, dt, &[], threads));
    let mut multigrid = constant
        .as_ref()
        .and_then(|(_, system)| system.multigrid(threads));
    let mut stepper = Stepper {
        mesh,
        assembled: &assembled,
        ties,
        options,
        threads,
        multipliers: vec![0.0; ties.len()],
        iterations: 0,
        residual: 0.0,
        nonlinear: 0,
    };
    let mut shared = constant.as_ref().map(|(base, system)| Shared {
        base,
        system,
        multigrid: &mut multigrid,
    });
    let n = mesh.nodes.len();
    let (temperature, frames) = match &study.transient {
        None => {
            let start = initial_guess(study, n);
            let t = stepper.advance(&start, None, shared.as_mut(), cancel, &|f| {
                progress(0.05 + 0.9 * f)
            })?;
            (t, Vec::new())
        }
        Some(transient) => {
            let dt = dt.unwrap_or(transient.duration);
            let mut t = vec![transient.initial; n];
            let stride = transient
                .steps
                .div_ceil(transient.frames.saturating_sub(1).max(1))
                .max(1);
            let mut frames = vec![Frame {
                time: 0.0,
                temperature: t.clone(),
            }];
            for step in 1..=transient.steps {
                let previous = t.clone();
                let span = |f: f64| {
                    progress(0.05 + 0.9 * ((step - 1) as f64 + f) / transient.steps as f64);
                };
                t = stepper.advance(
                    &previous,
                    Some((&previous, dt)),
                    shared.as_mut(),
                    cancel,
                    &span,
                )?;
                if step % stride == 0 || step == transient.steps {
                    frames.push(Frame {
                        time: dt * step as f64,
                        temperature: t.clone(),
                    });
                }
            }
            (t, frames)
        }
    };
    let heat_flux = recover_flux(mesh, materials, &geometries, &temperature);
    progress(1.0);
    Ok(ThermalResults {
        temperature,
        heat_flux,
        frames,
        iterations: stepper.iterations,
        residual: stepper.residual,
        nonlinear_iterations: stepper.nonlinear,
    })
}

/// A first guess for a steady study's Newton iterations: the mean of the temperatures
/// the loads name (held faces, fluids, surroundings), else 20 °C, and never under 20 °C:
/// Newton's method on the convex `T⁴` converges steadily from above, while from near
/// 0 K its first step is out of all proportion.
fn initial_guess(study: &ThermalStudy, n: usize) -> Vec<f64> {
    let named: Vec<f64> = study
        .loads
        .iter()
        .filter_map(|l| match l.kind {
            ThermalLoadKind::Temperature { celsius } => Some(celsius),
            ThermalLoadKind::Convection { ambient, .. }
            | ThermalLoadKind::Radiation { ambient, .. } => Some(ambient),
            _ => None,
        })
        .filter(|t| t.is_finite())
        .collect();
    let guess = if named.is_empty() {
        20.0
    } else {
        named.iter().sum::<f64>() / named.len() as f64
    };
    vec![guess.max(20.0); n]
}

fn assemble(
    mesh: &TetMesh,
    materials: &[ThermalMaterial],
    study: &ThermalStudy,
    geometries: &[Geometry],
    ties: &[Tie],
) -> Result<Assembled, ThermalError> {
    let n = mesh.nodes.len();
    let quadratic = mesh.is_quadratic();
    let mut conduction = Csr::pattern(mesh, ties);
    let mut capacity = study.transient.map(|_| conduction.clone());
    let mut heat = vec![0.0; n];
    let mass = reference_mass(quadratic);
    let per = if quadratic { 10 } else { 4 };
    // ∫ Nₐ dV per unit volume: the rows of the mass matrix, as the shape functions sum
    // to one.
    let share: Vec<f64> = (0..per)
        .map(|a| mass[a * per..(a + 1) * per].iter().sum())
        .collect();

    // Bodies' volumes, then each body's heat per volume (W/mm³), for internal heat.
    let mut body_volume: BTreeMap<u32, f64> = BTreeMap::new();
    for (e, g) in geometries.iter().enumerate() {
        *body_volume.entry(mesh.bodies[e]).or_insert(0.0) += g.volume;
    }
    let mut body_heat: BTreeMap<u32, f64> = BTreeMap::new();
    for h in &study.internal {
        if !h.watts.is_finite() {
            return Err(ThermalError::BadStudy(
                "an internal heat is not a finite power".to_owned(),
            ));
        }
        let mut bodies = h.bodies.clone();
        bodies.sort_unstable();
        bodies.dedup();
        let volume: f64 = bodies.iter().filter_map(|b| body_volume.get(b)).sum();
        if volume > 0.0 {
            for b in bodies {
                *body_heat.entry(b).or_insert(0.0) += h.watts / volume;
            }
        }
    }

    let mut grads = [[0.0; 3]; 10];
    let points = integration_points(quadratic);
    let mut local = [0.0f64; 100];
    for (e, g) in geometries.iter().enumerate() {
        let material = &materials[mesh.bodies[e] as usize];
        let k = material.conductivity_mm();
        let (nodes, count) = mesh.element_nodes(e);
        let nodes = &nodes[..count];
        local.fill(0.0);
        for &(l, w) in &points {
            shape_gradients(g, l, quadratic, &mut grads);
            let weight = w * g.volume * k;
            for a in 0..count {
                for b in 0..count {
                    local[a * count + b] += weight
                        * (grads[a][0] * grads[b][0]
                            + grads[a][1] * grads[b][1]
                            + grads[a][2] * grads[b][2]);
                }
            }
        }
        let rho_c = material.capacity_mm() * g.volume;
        for a in 0..count {
            for b in 0..count {
                // Capacity shares conduction's pattern: one search for both.
                let Some(slot) = conduction.slot(nodes[a] as usize, nodes[b]) else {
                    continue;
                };
                conduction.values[slot] += local[a * count + b];
                if let Some(c) = capacity.as_mut() {
                    c.values[slot] += rho_c * mass[a * count + b];
                }
            }
        }
        if let Some(&q) = body_heat.get(&mesh.bodies[e]) {
            for a in 0..count {
                heat[nodes[a] as usize] += q * g.volume * share[a];
            }
        }
    }

    // Boundary loads.
    let mut fixed: Vec<Option<f64>> = vec![None; n];
    let mut radiating = Vec::new();
    let points = triangle_points();
    for load in &study.loads {
        let facets: Vec<&Facet> = mesh
            .facets
            .iter()
            .filter(|f| f.label != crate::NO_LABEL && load.labels.contains(&f.label))
            .collect();
        let area: f64 = facets.iter().map(|f| norm(mesh.facet_area_normal(f))).sum();
        if facets.is_empty() || !area.is_finite() || area <= 0.0 {
            continue;
        }
        let finite = |values: &[f64]| values.iter().all(|v| v.is_finite());
        match load.kind {
            ThermalLoadKind::Temperature { celsius } => {
                if !finite(&[celsius]) || celsius < -KELVIN {
                    return Err(ThermalError::BadStudy(format!(
                        "{celsius} °C is not a temperature"
                    )));
                }
                for f in &facets {
                    for i in facet_nodes(mesh, f) {
                        fixed[i as usize] = Some(celsius);
                    }
                }
            }
            ThermalLoadKind::HeatPower { .. } | ThermalLoadKind::HeatFlux { .. } => {
                // W/mm².
                let flux = match load.kind {
                    ThermalLoadKind::HeatPower { watts } => watts / area,
                    ThermalLoadKind::HeatFlux { flux } => flux * 1e-6,
                    _ => 0.0,
                };
                if !flux.is_finite() {
                    return Err(ThermalError::BadStudy(
                        "a heat load is not a finite number".to_owned(),
                    ));
                }
                for f in &facets {
                    let a = norm(mesh.facet_area_normal(f));
                    let nodes = facet_nodes(mesh, f);
                    for (l, w) in points {
                        let (shape, count) = triangle_shapes(l, quadratic);
                        for k in 0..count {
                            heat[nodes[k] as usize] += w * a * flux * shape[k];
                        }
                    }
                }
            }
            ThermalLoadKind::Convection {
                coefficient,
                ambient,
            } => {
                if !finite(&[coefficient, ambient]) || coefficient < 0.0 {
                    return Err(ThermalError::BadStudy(
                        "a convection needs a film coefficient that is not negative".to_owned(),
                    ));
                }
                // W/(mm²·K).
                let h = coefficient * 1e-6;
                for f in &facets {
                    let a = norm(mesh.facet_area_normal(f));
                    let nodes = facet_nodes(mesh, f);
                    for (l, w) in points {
                        let (shape, count) = triangle_shapes(l, quadratic);
                        for p in 0..count {
                            heat[nodes[p] as usize] += w * a * h * ambient * shape[p];
                            for q in 0..count {
                                conduction.add(
                                    nodes[p] as usize,
                                    nodes[q],
                                    w * a * h * shape[p] * shape[q],
                                );
                            }
                        }
                    }
                }
            }
            ThermalLoadKind::Radiation {
                emissivity,
                ambient,
            } => {
                if !finite(&[emissivity, ambient]) || !(0.0..=1.0).contains(&emissivity) {
                    return Err(ThermalError::BadStudy(
                        "a radiation needs an emissivity from 0 to 1".to_owned(),
                    ));
                }
                if ambient < -KELVIN {
                    return Err(ThermalError::BadStudy(format!(
                        "{ambient} °C is not a temperature"
                    )));
                }
                for f in &facets {
                    radiating.push((**f, emissivity, ambient + KELVIN));
                }
            }
        }
    }
    // The ties' conductances.
    for tie in ties {
        let k = tie.penalty * tie.area;
        for (a, ca) in tie.terms() {
            for (b, cb) in tie.terms() {
                conduction.add(a as usize, b, k * ca * cb);
            }
        }
    }
    Ok(Assembled {
        conduction,
        capacity,
        heat,
        fixed,
        radiating,
    })
}

/// The first body whose piece of the mesh (pieces tied by contacts counted together) has
/// nothing that sets its temperature, if any.
fn unanchored(
    mesh: &TetMesh,
    study: &ThermalStudy,
    assembled: &Assembled,
    ties: &[Tie],
) -> Option<u32> {
    let piece = pieces(mesh);
    let n = mesh.nodes.len();
    let mut parent: Vec<u32> = (0..n as u32).collect();
    fn find(parent: &mut [u32], mut i: u32) -> u32 {
        while parent[i as usize] != i {
            let up = parent[parent[i as usize] as usize];
            parent[i as usize] = up;
            i = up;
        }
        i
    }
    let union = |parent: &mut [u32], a: u32, b: u32| {
        let (a, b) = (find(parent, a), find(parent, b));
        if a != b {
            parent[a.max(b) as usize] = a.min(b);
        }
    };
    for (i, &p) in piece.iter().enumerate() {
        union(&mut parent, i as u32, p);
    }
    for tie in ties {
        for (m, _) in tie.terms() {
            union(&mut parent, tie.slave, m);
        }
    }
    let mut anchored = vec![false; n];
    for (i, f) in assembled.fixed.iter().enumerate() {
        if f.is_some() {
            let r = find(&mut parent, i as u32);
            anchored[r as usize] = true;
        }
    }
    for load in study.loads.iter().filter(|l| l.kind.anchors()) {
        for f in mesh
            .facets
            .iter()
            .filter(|f| f.label != crate::NO_LABEL && load.labels.contains(&f.label))
        {
            let corner = mesh.facet_corners(f)[0];
            let r = find(&mut parent, corner);
            anchored[r as usize] = true;
        }
    }
    for (e, t) in mesh.tets.iter().enumerate() {
        let r = find(&mut parent, t[0]);
        if !anchored[r as usize] {
            return Some(mesh.bodies[e]);
        }
    }
    None
}

/// One linear system ready to solve: its matrix with the fixed temperatures taken out,
/// the multigrid's mode (a uniform temperature, zero where it is fixed) and the diagonal's
/// inverse for Jacobi.
struct System {
    matrix: Csr,
    ranges: Vec<(usize, usize)>,
    modes: Vec<f64>,
    jacobi: Vec<f64>,
}

impl System {
    fn multigrid(&self, threads: usize) -> Option<Multigrid<'_>> {
        Multigrid::new(self.matrix.view(), &self.modes, 1, threads).ok()
    }
}

/// The matrix for time step `dt` (`None`: steady) with `radiation`'s terms, before the
/// fixed temperatures are taken out (their columns move to the right-hand side), and the
/// system made of it.
fn prepare(
    a: &Assembled,
    dt: Option<f64>,
    radiation: &[(usize, u32, f64)],
    threads: usize,
) -> (Csr, System) {
    let base = base_matrix(a, dt, radiation);
    let mut matrix = base.clone();
    eliminate_fixed(&mut matrix, &a.fixed);
    let ranges = row_ranges(&matrix, threads);
    let modes = a
        .fixed
        .iter()
        .map(|f| if f.is_some() { 0.0 } else { 1.0 })
        .collect();
    let jacobi = (0..matrix.rows())
        .map(|i| {
            let d = matrix.diagonal(i);
            if d > 0.0 && d.is_finite() {
                1.0 / d
            } else {
                1.0
            }
        })
        .collect();
    (
        base,
        System {
            matrix,
            ranges,
            modes,
            jacobi,
        },
    )
}

/// The system every solve shares when nothing radiates, and its multigrid (`None` once it
/// could not be built or stalled: Jacobi carries on).
struct Shared<'s, 'm> {
    base: &'s Csr,
    system: &'s System,
    multigrid: &'m mut Option<Multigrid<'s>>,
}

/// Solves the study's systems step after step, keeping the solves' counts and the
/// contacts' multipliers.
struct Stepper<'a> {
    mesh: &'a TetMesh,
    assembled: &'a Assembled,
    ties: &'a [Tie],
    options: &'a SolveOptions,
    threads: usize,
    /// One per tie: the heat flux (W/mm²) through it.
    multipliers: Vec<f64>,
    iterations: usize,
    residual: f64,
    nonlinear: usize,
}

impl Stepper<'_> {
    /// The temperatures at the next time (`step`: the previous temperatures and the time
    /// step) or the steady state, from `start`: Newton's iterations for radiation and the
    /// contacts' multiplier updates until both settle. `shared` is the system when it does
    /// not change.
    fn advance(
        &mut self,
        start: &[f64],
        step: Option<(&[f64], f64)>,
        mut shared: Option<&mut Shared>,
        cancel: &AtomicBool,
        progress: &dyn Fn(f64),
    ) -> Result<Vec<f64>, ThermalError> {
        let a = self.assembled;
        let n = self.mesh.nodes.len();
        let quadratic = self.mesh.is_quadratic();
        let radiates = !a.radiating.is_empty();
        let nonlinear = radiates || !self.ties.is_empty();
        let mut t = start.to_vec();
        for (i, f) in a.fixed.iter().enumerate() {
            if let Some(v) = f {
                t[i] = *v;
            }
        }
        let dt = step.map(|(_, dt)| dt);
        let points = triangle_points();
        for round in 0..MAX_NONLINEAR {
            if cancel.load(Ordering::Relaxed) {
                return Err(ThermalError::Cancelled);
            }
            // Right-hand side: the fixed heat, the stored heat, the contacts' multipliers.
            let mut rhs = a.heat.clone();
            if let (Some((previous, dt)), Some(c)) = (step, &a.capacity) {
                let mut stored = vec![0.0; n];
                multiply(c, &[(0, n)], previous, &mut stored);
                for (r, s) in rhs.iter_mut().zip(&stored) {
                    *r += s / dt;
                }
            }
            for (tie, &lambda) in self.ties.iter().zip(&self.multipliers) {
                for (i, c) in tie.terms() {
                    rhs[i as usize] -= tie.area * lambda * c;
                }
            }
            // Radiation linearized at the current temperatures.
            let mut radiation: Vec<(usize, u32, f64)> = Vec::new();
            for &(facet, emissivity, ambient) in &a.radiating {
                let area = norm(self.mesh.facet_area_normal(&facet));
                let nodes = facet_nodes(self.mesh, &facet);
                let es = emissivity * STEFAN_BOLTZMANN;
                for (l, w) in points {
                    let (shape, count) = triangle_shapes(l, quadratic);
                    let tq: f64 = (0..count).map(|k| shape[k] * t[nodes[k] as usize]).sum();
                    let tk = (tq + KELVIN).max(1.0);
                    let leaving = es * (tk.powi(4) - ambient.powi(4));
                    let slope = 4.0 * es * tk.powi(3);
                    for p in 0..count {
                        // −R(Tₖ) + J Tₖ: the linearized radiation's right-hand side.
                        rhs[nodes[p] as usize] +=
                            w * area * shape[p] * (slope * (tk - KELVIN) - leaving);
                        for q in 0..count {
                            radiation.push((
                                nodes[p] as usize,
                                nodes[q],
                                w * area * slope * shape[p] * shape[q],
                            ));
                        }
                    }
                }
            }
            let stage = |f: f64| {
                let done = if nonlinear {
                    (round as f64 + f) / (round as f64 + 2.0)
                } else {
                    f
                };
                progress(done.clamp(0.0, 1.0));
            };
            let goal = self.options.tolerance.max(1e-15);
            let mut x = t.clone();
            let (spent, reached) = match shared.as_deref_mut() {
                Some(shared) => {
                    apply_fixed(shared.base, &a.fixed, &mut rhs);
                    solve_system(
                        shared.system,
                        shared.multigrid,
                        &rhs,
                        &mut x,
                        goal,
                        self.options.max_iterations,
                        cancel,
                        &stage,
                    )?
                }
                None => {
                    let (base, system) = prepare(a, dt, &radiation, self.threads);
                    apply_fixed(&base, &a.fixed, &mut rhs);
                    let mut multigrid = system.multigrid(self.threads);
                    solve_system(
                        &system,
                        &mut multigrid,
                        &rhs,
                        &mut x,
                        goal,
                        self.options.max_iterations,
                        cancel,
                        &stage,
                    )?
                }
            };
            self.iterations += spent;
            self.residual = reached;
            self.nonlinear += 1;
            // How far the temperatures moved, and the ties' gaps.
            let (lo, hi) = x
                .iter()
                .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), v| {
                    (lo.min(*v), hi.max(*v))
                });
            let range = (hi - lo).max(0.0);
            let tolerance = CHANGE_TOLERANCE * range.max(1.0);
            let moved = x
                .iter()
                .zip(&t)
                .map(|(a, b)| (a - b).abs())
                .fold(0.0, f64::max);
            let mut gap = 0.0f64;
            for (tie, lambda) in self.ties.iter().zip(self.multipliers.iter_mut()) {
                let g: f64 = tie.terms().map(|(i, c)| c * x[i as usize]).sum();
                *lambda += tie.penalty * g;
                gap = gap.max(g.abs());
            }
            t = x;
            if !nonlinear || (gap <= tolerance && (!radiates || moved <= tolerance)) {
                return Ok(t);
            }
        }
        Err(ThermalError::NotConverged {
            iterations: self.iterations,
            residual: self.residual,
        })
    }
}

/// Conduction (with convection and ties), plus the capacity over `dt`, plus radiation's
/// Jacobian.
fn base_matrix(a: &Assembled, dt: Option<f64>, radiation: &[(usize, u32, f64)]) -> Csr {
    let mut m = a.conduction.clone();
    if let (Some(dt), Some(c)) = (dt, &a.capacity) {
        for (v, cv) in m.values.iter_mut().zip(&c.values) {
            *v += cv / dt;
        }
    }
    for &(row, col, v) in radiation {
        m.add(row, col, v);
    }
    m
}

/// Moves the fixed temperatures' columns of `matrix` to the right-hand side and sets their
/// rows to the value: `rhs` before [`eliminate_fixed`] takes them out of the matrix.
fn apply_fixed(matrix: &Csr, fixed: &[Option<f64>], rhs: &mut [f64]) {
    for (d, f) in fixed.iter().enumerate() {
        let Some(value) = f else { continue };
        // The matrix is symmetric: column d's entries are row d's.
        for k in matrix.row_start[d]..matrix.row_start[d + 1] {
            let j = matrix.columns[k] as usize;
            if fixed[j].is_none() {
                rhs[j] -= matrix.values[k] * value;
            }
        }
    }
    for (d, f) in fixed.iter().enumerate() {
        if let Some(value) = f {
            rhs[d] = *value;
        }
    }
}

/// The fixed temperatures' rows and columns replaced by the identity.
fn eliminate_fixed(matrix: &mut Csr, fixed: &[Option<f64>]) {
    for d in 0..matrix.rows() {
        if fixed[d].is_none() {
            for k in matrix.row_start[d]..matrix.row_start[d + 1] {
                if fixed[matrix.columns[k] as usize].is_some() {
                    matrix.values[k] = 0.0;
                }
            }
            continue;
        }
        for k in matrix.row_start[d]..matrix.row_start[d + 1] {
            matrix.values[k] = if matrix.columns[k] as usize == d {
                1.0
            } else {
                0.0
            };
        }
    }
}

/// Conjugate gradients on `system`, preconditioned by `multigrid`, else Jacobi (from
/// then on, once multigrid has stalled): the iterations spent and the residual reached.
#[allow(clippy::too_many_arguments)]
fn solve_system(
    system: &System,
    multigrid: &mut Option<Multigrid>,
    rhs: &[f64],
    x: &mut [f64],
    goal: f64,
    max_iterations: usize,
    cancel: &AtomicBool,
    progress: &dyn Fn(f64),
) -> Result<(usize, f64), ThermalError> {
    let mut spent = 0;
    if let Some(m) = multigrid.as_mut() {
        let mut precondition = |r: &[f64], z: &mut [f64]| m.apply(r, z);
        match conjugate_gradients(
            system,
            rhs,
            x,
            goal,
            MULTIGRID_ITERATIONS.min(max_iterations),
            cancel,
            progress,
            &mut precondition,
        ) {
            Ok(done) => return Ok(done),
            Err(CgStop::Cancelled) => return Err(ThermalError::Cancelled),
            Err(CgStop::Stalled { iterations, .. }) => {
                spent = iterations;
                if !x.iter().all(|v| v.is_finite()) {
                    x.fill(0.0);
                }
                *multigrid = None;
            }
        }
    }
    let mut precondition = |r: &[f64], z: &mut [f64]| {
        for ((z, r), d) in z.iter_mut().zip(r).zip(&system.jacobi) {
            *z = r * d;
        }
    };
    match conjugate_gradients(
        system,
        rhs,
        x,
        goal,
        max_iterations.saturating_sub(spent),
        cancel,
        progress,
        &mut precondition,
    ) {
        Ok((done, residual)) => Ok((spent + done, residual)),
        Err(CgStop::Cancelled) => Err(ThermalError::Cancelled),
        Err(CgStop::Stalled {
            iterations,
            residual,
        }) => Err(ThermalError::NotConverged {
            iterations: spent + iterations,
            residual,
        }),
    }
}

/// Iterations multigrid gets before Jacobi takes over.
const MULTIGRID_ITERATIONS: usize = 500;

enum CgStop {
    Cancelled,
    Stalled { iterations: usize, residual: f64 },
}

#[allow(clippy::too_many_arguments)]
fn conjugate_gradients(
    system: &System,
    rhs: &[f64],
    x: &mut [f64],
    goal: f64,
    limit: usize,
    cancel: &AtomicBool,
    progress: &dyn Fn(f64),
    precondition: &mut dyn FnMut(&[f64], &mut [f64]),
) -> Result<(usize, f64), CgStop> {
    let n = x.len();
    let m = &system.matrix;
    let dot = |a: &[f64], b: &[f64]| a.iter().zip(b).map(|(x, y)| x * y).sum::<f64>();
    let rhs_size = dot(rhs, rhs).sqrt();
    if rhs_size <= 0.0 {
        x.fill(0.0);
        return Ok((0, 0.0));
    }
    let mut r = vec![0.0; n];
    multiply(m, &system.ranges, x, &mut r);
    for (r, b) in r.iter_mut().zip(rhs) {
        *r = b - *r;
    }
    let mut residual = dot(&r, &r).sqrt() / rhs_size;
    let mut z = vec![0.0; n];
    precondition(&r, &mut z);
    let mut p = z.clone();
    let mut q = vec![0.0; n];
    let mut rz = dot(&r, &z);
    let log_goal = (1.0 / goal).ln();
    let mut iterations = 0;
    while residual > goal {
        let stalled = CgStop::Stalled {
            iterations,
            residual,
        };
        if iterations >= limit || !(rz > 0.0 && rz.is_finite()) {
            return Err(stalled);
        }
        multiply(m, &system.ranges, &p, &mut q);
        let pq = dot(&p, &q);
        if !(pq > 0.0 && pq.is_finite()) {
            return Err(stalled);
        }
        let alpha = rz / pq;
        for i in 0..n {
            x[i] += alpha * p[i];
            r[i] -= alpha * q[i];
        }
        residual = dot(&r, &r).sqrt() / rhs_size;
        iterations += 1;
        precondition(&r, &mut z);
        let rz_next = dot(&r, &z);
        let beta = rz_next / rz;
        rz = rz_next;
        for i in 0..n {
            p[i] = z[i] + beta * p[i];
        }
        if iterations % 4 == 0 {
            if cancel.load(Ordering::Relaxed) {
                return Err(CgStop::Cancelled);
            }
            progress(((1.0 / residual.max(goal)).ln() / log_goal).clamp(0.0, 1.0));
        }
    }
    Ok((iterations, residual))
}

/// The heat flux `−k ∇T` at the nodes, W/m², averaged over the elements around each.
fn recover_flux(
    mesh: &TetMesh,
    materials: &[ThermalMaterial],
    geometries: &[Geometry],
    temperature: &[f64],
) -> Vec<V3> {
    let n = mesh.nodes.len();
    let quadratic = mesh.is_quadratic();
    let mut sum = vec![[0.0f64; 3]; n];
    let mut count = vec![0u32; n];
    let points = node_points(quadratic);
    let mut grads = [[0.0; 3]; 10];
    for (e, g) in geometries.iter().enumerate() {
        let k = materials[mesh.bodies[e] as usize].conductivity_mm();
        let (nodes, n_nodes) = mesh.element_nodes(e);
        for (local, l) in points.iter().enumerate() {
            shape_gradients(g, *l, quadratic, &mut grads);
            let mut gradient = [0.0; 3];
            for a in 0..n_nodes {
                gradient = add(gradient, scale(grads[a], temperature[nodes[a] as usize]));
            }
            // W/mm² to W/m².
            let q = scale(gradient, -k * 1e6);
            let node = nodes[local] as usize;
            sum[node] = add(sum[node], q);
            count[node] += 1;
        }
    }
    sum.iter()
        .zip(&count)
        .map(|(s, &c)| {
            if c == 0 {
                [0.0; 3]
            } else {
                scale(*s, 1.0 / f64::from(c))
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ContactKind, mesh_solids, mesher::box_surface};

    const STEEL: ThermalMaterial = ThermalMaterial::new(50.0, 500.0, 7850.0);

    fn run(mesh: &TetMesh, materials: &[ThermalMaterial], study: &ThermalStudy) -> ThermalResults {
        let cancel = AtomicBool::new(false);
        solve_thermal(
            mesh,
            materials,
            study,
            &SolveOptions::default(),
            &cancel,
            &|_| {},
        )
        .unwrap()
    }

    fn at(mesh: &TetMesh, r: &ThermalResults, p: V3) -> f64 {
        let i = (0..mesh.nodes.len())
            .min_by(|&a, &b| {
                let d = |i: usize| norm(crate::math::sub(mesh.nodes[i], p));
                d(a).total_cmp(&d(b))
            })
            .unwrap();
        r.temperature[i]
    }

    fn bar(quadratic: bool) -> TetMesh {
        let mut mesh = TetMesh::structured_box([0.0; 3], [100.0, 10.0, 10.0], [20, 2, 2]);
        if quadratic {
            mesh.make_quadratic();
        }
        mesh
    }

    fn held(label: u32, celsius: f64) -> ThermalLoad {
        ThermalLoad {
            kind: ThermalLoadKind::Temperature { celsius },
            labels: vec![label],
        }
    }

    #[test]
    fn the_reference_mass_integrates_one() {
        for quadratic in [false, true] {
            let m = reference_mass(quadratic);
            // The shape functions sum to one: their mass sums to the volume.
            assert!((m.iter().sum::<f64>() - 1.0).abs() < 1e-12);
            let n = if quadratic { 10 } else { 4 };
            let rows: Vec<f64> = (0..n).map(|a| m[a * n..(a + 1) * n].iter().sum()).collect();
            if quadratic {
                // ∫ N dV: −1/20 at the corners, 1/5 at the edges.
                assert!((rows[0] + 0.05).abs() < 1e-12 && (rows[4] - 0.2).abs() < 1e-12);
            } else {
                assert!((m[0] - 0.1).abs() < 1e-12 && (m[1] - 0.05).abs() < 1e-12);
            }
        }
        let w: f64 = triangle_points().iter().map(|(_, w)| w).sum();
        assert!((w - 1.0).abs() < 1e-12);
    }

    #[test]
    fn a_bar_between_two_temperatures_is_linear() {
        for quadratic in [false, true] {
            let mesh = bar(quadratic);
            let study = ThermalStudy {
                loads: vec![held(0, 100.0), held(1, 20.0)],
                ..ThermalStudy::default()
            };
            let r = run(&mesh, &[STEEL], &study);
            for (p, t) in mesh.nodes.iter().zip(&r.temperature) {
                let expected = 100.0 - 80.0 * p[0] / 100.0;
                assert!((t - expected).abs() < 1e-5, "{t} at {p:?}");
            }
            // q = k ΔT / L = 50 W/(m·K) × 80 K / 0.1 m = 40 kW/m², along +X.
            for q in &r.heat_flux {
                assert!((q[0] - 40_000.0).abs() < 1e-3 * 40_000.0, "{q:?}");
                assert!(q[1].abs() < 1.0 && q[2].abs() < 1.0);
            }
            let s = r.summary();
            assert!((s.max_temperature.0 - 100.0).abs() < 1e-9);
            assert!((s.min_temperature.0 - 20.0).abs() < 1e-9);
        }
    }

    #[test]
    fn heat_in_at_one_end_climbs_the_bar() {
        // 10 W into the +X end, the −X end held at 20 °C: ΔT = P L / (k A)
        // = 10 × 0.1 / (50 × 1e-4) = 200 K.
        let mesh = bar(true);
        let study = ThermalStudy {
            loads: vec![
                held(0, 20.0),
                ThermalLoad {
                    kind: ThermalLoadKind::HeatPower { watts: 10.0 },
                    labels: vec![1],
                },
            ],
            ..ThermalStudy::default()
        };
        let r = run(&mesh, &[STEEL], &study);
        let hot = at(&mesh, &r, [100.0, 5.0, 5.0]);
        assert!((hot - 220.0).abs() < 1e-6, "{hot}");
        // The same as a flux: 10 W over 100 mm² is 100 kW/m².
        let study = ThermalStudy {
            loads: vec![
                held(0, 20.0),
                ThermalLoad {
                    kind: ThermalLoadKind::HeatFlux { flux: 100_000.0 },
                    labels: vec![1],
                },
            ],
            ..ThermalStudy::default()
        };
        let r = run(&mesh, &[STEEL], &study);
        assert!((at(&mesh, &r, [100.0, 0.0, 0.0]) - 220.0).abs() < 1e-6);
    }

    #[test]
    fn internal_heat_bends_the_profile_into_a_parabola() {
        // Q W/m³ in a bar held at 0 °C at both ends: T(x) = Q x (L − x) / 2k,
        // largest at mid-length, Q L² / 8k.
        let mesh = bar(true);
        let watts = 5.0;
        let q = watts / (100.0 * 10.0 * 10.0 * 1e-9);
        let study = ThermalStudy {
            loads: vec![held(0, 0.0), held(1, 0.0)],
            internal: vec![InternalHeat {
                bodies: vec![0],
                watts,
            }],
            ..ThermalStudy::default()
        };
        let r = run(&mesh, &[STEEL], &study);
        for x in [25.0, 50.0, 80.0] {
            let expected = q * (x * 1e-3) * ((100.0 - x) * 1e-3) / (2.0 * 50.0);
            let t = at(&mesh, &r, [x, 5.0, 5.0]);
            assert!(
                (t - expected).abs() < 1e-6 * expected.max(1.0),
                "{t} vs {expected}"
            );
        }
    }

    #[test]
    fn convection_loses_heat_as_a_resistance_in_series() {
        // 100 °C at one end, convection to 20 °C at the other: the heat crosses the bar
        // (L / kA) and the film (1 / hA) in series.
        let mesh = bar(true);
        let h = 500.0;
        let study = ThermalStudy {
            loads: vec![
                held(0, 100.0),
                ThermalLoad {
                    kind: ThermalLoadKind::Convection {
                        coefficient: h,
                        ambient: 20.0,
                    },
                    labels: vec![1],
                },
            ],
            ..ThermalStudy::default()
        };
        let r = run(&mesh, &[STEEL], &study);
        let area = 1e-4;
        let resistance = 0.1 / (50.0 * area) + 1.0 / (h * area);
        let power = 80.0 / resistance;
        let end = 20.0 + power / (h * area);
        let t = at(&mesh, &r, [100.0, 5.0, 5.0]);
        assert!((t - end).abs() < 1e-6, "{t} vs {end}");
    }

    #[test]
    fn radiation_settles_where_heat_in_equals_heat_out() {
        // 1 W into one end, radiated from the other to 0 K surroundings with ε = 1 from
        // 100 mm²: T⁴ = P / (σ A) at that end.
        let mesh = bar(false);
        let study = ThermalStudy {
            loads: vec![
                ThermalLoad {
                    kind: ThermalLoadKind::HeatPower { watts: 1.0 },
                    labels: vec![0],
                },
                ThermalLoad {
                    kind: ThermalLoadKind::Radiation {
                        emissivity: 1.0,
                        ambient: -KELVIN,
                    },
                    labels: vec![1],
                },
            ],
            ..ThermalStudy::default()
        };
        let r = run(&mesh, &[STEEL], &study);
        let radiating = (1.0f64 / (5.670_374_419e-8 * 1e-4)).powf(0.25) - KELVIN;
        let t = at(&mesh, &r, [100.0, 5.0, 5.0]);
        assert!((t - radiating).abs() < 1e-3, "{t} vs {radiating}");
        assert!(r.nonlinear_iterations > 2);
        // The heat conducted along the bar: ΔT = P L / (k A) = 20 K.
        let cold_end = at(&mesh, &r, [0.0, 5.0, 5.0]);
        assert!((cold_end - t - 20.0).abs() < 1e-3);
    }

    #[test]
    fn multigrid_keeps_the_iterations_few() {
        // 60 000 nodes of ten-node elements: Jacobi would need hundreds of iterations.
        let mut mesh = TetMesh::structured_box([0.0; 3], [200.0, 40.0, 40.0], [40, 8, 8]);
        mesh.make_quadratic();
        let study = ThermalStudy {
            loads: vec![
                held(0, 0.0),
                ThermalLoad {
                    kind: ThermalLoadKind::Convection {
                        coefficient: 25.0,
                        ambient: 80.0,
                    },
                    labels: vec![1, 2, 3, 4, 5],
                },
            ],
            ..ThermalStudy::default()
        };
        let r = run(&mesh, &[STEEL], &study);
        assert!(r.iterations < 60, "{} iterations", r.iterations);
        let s = r.summary();
        assert!(s.max_temperature.0 < 80.0 && s.min_temperature.0.abs() < 1e-9);
    }

    #[test]
    fn a_floating_steady_study_is_refused() {
        let mesh = bar(false);
        let study = ThermalStudy {
            loads: vec![ThermalLoad {
                kind: ThermalLoadKind::HeatPower { watts: 1.0 },
                labels: vec![0],
            }],
            ..ThermalStudy::default()
        };
        let cancel = AtomicBool::new(false);
        let e = solve_thermal(
            &mesh,
            &[STEEL],
            &study,
            &SolveOptions::default(),
            &cancel,
            &|_| {},
        );
        assert_eq!(e, Err(ThermalError::NotAnchored { body: 0 }));
    }

    #[test]
    fn a_cooling_bar_follows_its_first_mode() {
        // A bar at 100 °C, both ends held at 0 °C from t = 0: the slowest mode decays as
        // exp(−α π² t / L²) and carries almost everything after a while. Its peak at
        // mid-length is (4/π) 100 e^(−α π² t / L²) (the next mode, 3π, is gone by then).
        let mesh = bar(true);
        let alpha = STEEL.conductivity / (STEEL.density * STEEL.specific_heat);
        let length: f64 = 0.1;
        let tau = length * length / (alpha * std::f64::consts::PI.powi(2));
        let duration = 0.5 * tau;
        let study = ThermalStudy {
            loads: vec![held(0, 0.0), held(1, 0.0)],
            transient: Some(Transient {
                duration,
                steps: 200,
                initial: 100.0,
                frames: 11,
            }),
            ..ThermalStudy::default()
        };
        let r = run(&mesh, &[STEEL], &study);
        assert_eq!(r.frames.len(), 11);
        assert_eq!(r.frames[0].time, 0.0);
        assert!((r.frames[10].time - duration).abs() < 1e-9 * duration);
        let t = at(&mesh, &r, [50.0, 5.0, 5.0]);
        let first = 4.0 / std::f64::consts::PI * 100.0 * (-duration / tau).exp();
        let third = 4.0 / (3.0 * std::f64::consts::PI) * 100.0 * (-9.0 * duration / tau).exp();
        let expected = first - third;
        // Backward Euler with 200 steps: a fraction of a per cent.
        assert!((t - expected).abs() < 0.01 * expected, "{t} vs {expected}");
        // Cooling all the way: each frame is colder at mid-length.
        let mid = |f: &Frame| {
            let i = (0..mesh.nodes.len())
                .min_by(|&a, &b| {
                    let d = |i: usize| norm(crate::math::sub(mesh.nodes[i], [50.0, 5.0, 5.0]));
                    d(a).total_cmp(&d(b))
                })
                .unwrap();
            f.temperature[i]
        };
        assert!(r.frames.windows(2).all(|w| mid(&w[1]) <= mid(&w[0]) + 1e-9));
    }

    #[test]
    fn a_transient_study_needs_a_heat_capacity() {
        let mesh = bar(false);
        let study = ThermalStudy {
            loads: vec![held(0, 0.0)],
            transient: Some(Transient {
                duration: 1.0,
                steps: 2,
                initial: 20.0,
                frames: 3,
            }),
            ..ThermalStudy::default()
        };
        let cancel = AtomicBool::new(false);
        let e = solve_thermal(
            &mesh,
            &[ThermalMaterial::new(50.0, 0.0, 7850.0)],
            &study,
            &SolveOptions::default(),
            &cancel,
            &|_| {},
        );
        assert_eq!(e, Err(ThermalError::NoHeatCapacity { body: 0 }));
    }

    #[test]
    fn heat_crosses_a_contact_between_bodies_meshed_apart() {
        // Two blocks end to end, meshed apart and tied by a contact: the same linear
        // profile as one bar.
        let left = box_surface([0.0; 3], [50.0, 10.0, 10.0]);
        let mut right = box_surface([50.0, 0.0, 0.0], [100.0, 10.0, 10.0]);
        for l in &mut right.labels {
            *l += 6;
        }
        let options = crate::MeshOptions {
            element_size: 2.5,
            quadratic: true,
            ..crate::MeshOptions::default()
        };
        let cancel = AtomicBool::new(false);
        let (mesh, _) = mesh_solids(&[left, right], &options, &cancel, &|_| {}).unwrap();
        // Left's −X (label 0) hot, right's +X (label 7) cold; left's +X (1) meets
        // right's −X (6).
        let study = ThermalStudy {
            loads: vec![held(0, 100.0), held(7, 0.0)],
            contacts: vec![Contact {
                kind: ContactKind::Separation,
                a_labels: vec![1],
                b_labels: vec![6],
                offset: 0.0,
            }],
            ..ThermalStudy::default()
        };
        let aluminium = ThermalMaterial::new(167.0, 896.0, 2700.0);
        let r = run(&mesh, &[aluminium, aluminium], &study);
        for x in [25.0, 75.0] {
            let t = at(&mesh, &r, [x, 5.0, 5.0]);
            let expected = 100.0 * (1.0 - x / 100.0);
            // The lattice mesher bevels the edges where the blocks meet.
            assert!((t - expected).abs() < 2.0, "{t} vs {expected} at {x}");
        }
    }
}
