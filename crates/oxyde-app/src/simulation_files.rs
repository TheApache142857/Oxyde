//! The Simulation tab's files: the model's studies saved together as a `.oxysim` document
//! and each study's results as a `.oxyres` beside it, `<study file>-<study>.oxyres`
//! (`oxyde_format::simulation`, `oxyde_format::results`, FORMAT.md §8–§9). The studies
//! saved next to a model as `<model>.oxysim` open with it.
//!
//! A study document names its part or assembly by relative path and document id, and holds
//! the fixtures, loads, contacts, mesh settings and material overrides by persistent face
//! name. A results document holds the mesh and the nodal fields, tied to the study by its
//! document id and to the model by a fingerprint (a hash of every part and assembly file's
//! definition and of the study itself), so results of an earlier model are told apart when
//! they are opened. After *Run*, the results of a saved study are written next to it.
//!
//! A flow study's results are a record of its OpenFOAM run instead (goals, residuals), and
//! its case folder is kept beside them, `<study file>-<study>.openfoam`, which holds the
//! mesh and the fields the plots are drawn from (`flow_ui.rs`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use oxyde_format::results::{
    DocumentHash, Elements, Extreme, Fingerprint, NodalFields, ResultBody, ResultFacet, ResultMesh,
    ResultsFile, SolverInfo, TemperatureFrame, ThermalFields,
};
use oxyde_format::simulation::{
    Contact as FileContact, ContactDetection, ContactKind as FileContactKind, Fixture,
    FixtureKind as FileFixtureKind, FlowCondition, FlowConditionKind, FlowGoal, ForceDirection,
    Load, LoadKind as FileLoadKind, MeshQuality, MeshSettings, MeshSize, Mesher, ModelKind,
    SimulationFile, StudiedModel, Study as FileStudy, StudyFace,
};
use oxyde_format::{AssemblyFile, PartFile};
use oxyde_sim::{ContactKind, Facet, Frame, Results, TetMesh, ThermalResults};

use crate::app::OxydeApp;
use crate::assembly::{Loaded, Occurrence};
use crate::simulation_ui::{
    Done, ElementQuality, ItemKind, LoadDirection, MesherChoice, SimFace, Study, StudyContact,
    StudyItem, StudyMesh,
};

/// Where the open study is saved, and its document id.
#[derive(Debug, Clone, PartialEq)]
pub struct StudyFile {
    pub path: PathBuf,
    pub document_id: String,
}

// ---- The study ------------------------------------------------------------------------------

fn study_face(f: &SimFace) -> StudyFace {
    match &f.occurrence {
        Some(o) => StudyFace::component(o.top, o.path.clone(), f.face.clone()),
        None => StudyFace::part(f.face.clone()),
    }
}

fn sim_face(f: &StudyFace) -> SimFace {
    SimFace {
        occurrence: f.component.map(|top| Occurrence {
            top,
            path: f.path.clone(),
        }),
        face: f.face.clone(),
    }
}

fn file_contact_kind(k: ContactKind) -> FileContactKind {
    match k {
        ContactKind::Bonded => FileContactKind::Bonded,
        ContactKind::Separation => FileContactKind::Separation,
        ContactKind::Sliding => FileContactKind::Sliding,
        ContactKind::Rough => FileContactKind::Rough,
        ContactKind::OffsetBonded => FileContactKind::OffsetBonded,
    }
}

fn contact_kind(k: FileContactKind) -> ContactKind {
    match k {
        FileContactKind::Bonded => ContactKind::Bonded,
        FileContactKind::Separation => ContactKind::Separation,
        FileContactKind::Sliding => ContactKind::Sliding,
        FileContactKind::Rough => ContactKind::Rough,
        FileContactKind::OffsetBonded => ContactKind::OffsetBonded,
    }
}

/// The study as a `.oxysim` holds it: the items its type takes.
pub(crate) fn file_study(study: &Study) -> FileStudy {
    let mut out = FileStudy::of_type(&study.name, study.study_type);
    let faces = |faces: &[SimFace]| faces.iter().map(study_face).collect::<Vec<_>>();
    let load = |name: &str, kind: FileLoadKind| Load {
        name: name.to_owned(),
        kind,
        suppressed: false,
    };
    for item in study.items.iter().filter(|i| i.kind.fits(study.study_type)) {
        let on = faces(&item.faces);
        match &item.kind {
            ItemKind::Fixed | ItemKind::Roller => out.fixtures.push(Fixture {
                name: item.name.clone(),
                kind: if item.kind == ItemKind::Fixed {
                    FileFixtureKind::Fixed
                } else {
                    FileFixtureKind::Roller
                },
                faces: faces(&item.faces),
                suppressed: false,
            }),
            ItemKind::Force { newtons, direction } => out.loads.push(Load {
                name: item.name.clone(),
                kind: FileLoadKind::Force {
                    faces: faces(&item.faces),
                    newtons: *newtons,
                    direction: match direction.vector() {
                        Some(v) => ForceDirection::Vector(v),
                        None => ForceDirection::Normal,
                    },
                },
                suppressed: false,
            }),
            ItemKind::Pressure { mpa } => out.loads.push(Load {
                name: item.name.clone(),
                kind: FileLoadKind::Pressure {
                    faces: faces(&item.faces),
                    mpa: *mpa,
                },
                suppressed: false,
            }),
            ItemKind::Temperature { celsius } => out.loads.push(load(
                &item.name,
                FileLoadKind::Temperature {
                    faces: on,
                    celsius: *celsius,
                },
            )),
            ItemKind::HeatPower { watts } => out.loads.push(load(
                &item.name,
                FileLoadKind::HeatPower {
                    faces: on,
                    watts: *watts,
                },
            )),
            ItemKind::HeatFlux { flux } => out.loads.push(load(
                &item.name,
                FileLoadKind::HeatFlux {
                    faces: on,
                    flux: *flux,
                },
            )),
            ItemKind::Convection {
                coefficient,
                ambient,
            } => out.loads.push(load(
                &item.name,
                FileLoadKind::Convection {
                    faces: on,
                    coefficient: *coefficient,
                    ambient: *ambient,
                },
            )),
            ItemKind::Radiation {
                emissivity,
                ambient,
            } => out.loads.push(load(
                &item.name,
                FileLoadKind::Radiation {
                    faces: on,
                    emissivity: *emissivity,
                    ambient: *ambient,
                },
            )),
            ItemKind::InternalHeat { watts } => out.loads.push(load(
                &item.name,
                FileLoadKind::InternalHeat {
                    faces: on,
                    watts: *watts,
                },
            )),
            ItemKind::InletVelocity { speed } => out.conditions.push(condition(
                &item.name,
                FlowConditionKind::InletVelocity { speed: *speed },
                on,
            )),
            ItemKind::InletVolumeFlow { rate } => out.conditions.push(condition(
                &item.name,
                FlowConditionKind::InletVolumeFlow { rate: *rate },
                on,
            )),
            ItemKind::InletMassFlow { rate } => out.conditions.push(condition(
                &item.name,
                FlowConditionKind::InletMassFlow { rate: *rate },
                on,
            )),
            ItemKind::StaticPressure { pascals } => out.conditions.push(condition(
                &item.name,
                FlowConditionKind::StaticPressure { pressure: *pascals },
                on,
            )),
            ItemKind::IdealWall => {
                out.conditions
                    .push(condition(&item.name, FlowConditionKind::IdealWall, on))
            }
            ItemKind::Goal(kind) => out.goals.push(FlowGoal {
                name: item.name.clone(),
                kind: *kind,
                faces: on,
            }),
        }
    }
    if study.gravity && study.study_type.is_structural() {
        out.loads.push(Load {
            name: "Gravity-1".to_owned(),
            kind: FileLoadKind::Gravity {
                acceleration: [0.0, -9.81, 0.0],
            },
            suppressed: false,
        });
    }
    for c in &study.contacts {
        out.contacts.push(FileContact {
            name: c.name.clone(),
            kind: file_contact_kind(c.kind),
            a: faces(&c.a),
            b: faces(&c.b),
            offset: c.offset,
            detection: if c.automatic {
                ContactDetection::Automatic {
                    tolerance: study.contact_tolerance,
                }
            } else {
                ContactDetection::Manual
            },
            suppressed: false,
        });
    }
    out.mesh = MeshSettings {
        mesher: match study.mesh.mesher {
            MesherChoice::Netgen => Mesher::Netgen,
            MesherChoice::Lattice => Mesher::Lattice,
        },
        quality: match study.mesh.quality {
            ElementQuality::High => MeshQuality::High,
            ElementQuality::Draft => MeshQuality::Draft,
        },
        size: if study.mesh.custom_size {
            MeshSize::ElementSize(study.mesh.element_size)
        } else {
            MeshSize::Automatic {
                density: f64::from(study.mesh.density),
            }
        },
    };
    out.materials = study.overrides.clone();
    out.thermal_materials = study.thermal_overrides.clone();
    out.transient = study.transient;
    out.reference_temperature = study.reference_temperature;
    out.flow = study.flow.clone();
    out.cooling = study.cooling;
    out
}

fn condition(name: &str, kind: FlowConditionKind, faces: Vec<StudyFace>) -> FlowCondition {
    FlowCondition {
        name: name.to_owned(),
        kind,
        faces,
        suppressed: false,
    }
}

/// The study a `.oxysim` holds, as the panel edits it. Suppressed items are left out (the
/// panel has no switch for them yet); a gravity load other than −Y becomes −Y.
pub(crate) fn study_of(file: &FileStudy) -> Study {
    let mut study = Study::of_type(file.study_type, &file.name);
    let faces = |faces: &[StudyFace]| faces.iter().map(sim_face).collect::<Vec<_>>();
    for f in file.fixtures.iter().filter(|f| !f.suppressed) {
        study.items.push(StudyItem {
            name: f.name.clone(),
            kind: match f.kind {
                FileFixtureKind::Fixed => ItemKind::Fixed,
                FileFixtureKind::Roller => ItemKind::Roller,
            },
            faces: faces(&f.faces),
        });
    }
    for l in file.loads.iter().filter(|l| !l.suppressed) {
        match &l.kind {
            FileLoadKind::Force {
                faces: f,
                newtons,
                direction,
            } => {
                let (direction, newtons) = match direction {
                    ForceDirection::Normal => (LoadDirection::Normal, *newtons),
                    ForceDirection::Vector(v) => LoadDirection::of_vector(*v, *newtons),
                };
                study.items.push(StudyItem {
                    name: l.name.clone(),
                    kind: ItemKind::Force { newtons, direction },
                    faces: faces(f),
                });
            }
            FileLoadKind::Pressure { faces: f, mpa } => study.items.push(StudyItem {
                name: l.name.clone(),
                kind: ItemKind::Pressure { mpa: *mpa },
                faces: faces(f),
            }),
            FileLoadKind::Gravity { .. } => study.gravity = true,
            FileLoadKind::Temperature { faces: f, celsius } => study.items.push(StudyItem {
                name: l.name.clone(),
                kind: ItemKind::Temperature { celsius: *celsius },
                faces: faces(f),
            }),
            FileLoadKind::HeatPower { faces: f, watts } => study.items.push(StudyItem {
                name: l.name.clone(),
                kind: ItemKind::HeatPower { watts: *watts },
                faces: faces(f),
            }),
            FileLoadKind::HeatFlux { faces: f, flux } => study.items.push(StudyItem {
                name: l.name.clone(),
                kind: ItemKind::HeatFlux { flux: *flux },
                faces: faces(f),
            }),
            FileLoadKind::Convection {
                faces: f,
                coefficient,
                ambient,
            } => study.items.push(StudyItem {
                name: l.name.clone(),
                kind: ItemKind::Convection {
                    coefficient: *coefficient,
                    ambient: *ambient,
                },
                faces: faces(f),
            }),
            FileLoadKind::Radiation {
                faces: f,
                emissivity,
                ambient,
            } => study.items.push(StudyItem {
                name: l.name.clone(),
                kind: ItemKind::Radiation {
                    emissivity: *emissivity,
                    ambient: *ambient,
                },
                faces: faces(f),
            }),
            FileLoadKind::InternalHeat { faces: f, watts } => study.items.push(StudyItem {
                name: l.name.clone(),
                kind: ItemKind::InternalHeat { watts: *watts },
                faces: faces(f),
            }),
        }
    }
    for c in file.contacts.iter().filter(|c| !c.suppressed) {
        if let ContactDetection::Automatic { tolerance } = c.detection {
            study.contact_tolerance = tolerance;
            study.contacts_found = true;
        }
        study.contacts.push(StudyContact {
            name: c.name.clone(),
            kind: contact_kind(c.kind),
            a: faces(&c.a),
            b: faces(&c.b),
            offset: c.offset,
            automatic: matches!(c.detection, ContactDetection::Automatic { .. }),
        });
    }
    let defaults = StudyMesh::default();
    study.mesh = StudyMesh {
        mesher: match file.mesh.mesher {
            Mesher::Netgen => MesherChoice::Netgen,
            Mesher::Lattice => MesherChoice::Lattice,
        },
        quality: match file.mesh.quality {
            MeshQuality::High => ElementQuality::High,
            MeshQuality::Draft => ElementQuality::Draft,
        },
        custom_size: matches!(file.mesh.size, MeshSize::ElementSize(_)),
        density: match file.mesh.size {
            MeshSize::Automatic { density } => density as f32,
            MeshSize::ElementSize(_) => defaults.density,
        },
        element_size: match file.mesh.size {
            MeshSize::ElementSize(h) => h,
            MeshSize::Automatic { .. } => defaults.element_size,
        },
    };
    study.overrides = file.materials.clone();
    study.thermal_overrides = file.thermal_materials.clone();
    study.transient = file.transient;
    study.reference_temperature = file.reference_temperature;
    for c in file.conditions.iter().filter(|c| !c.suppressed) {
        study.items.push(StudyItem {
            name: c.name.clone(),
            kind: match c.kind {
                FlowConditionKind::InletVelocity { speed } => ItemKind::InletVelocity { speed },
                FlowConditionKind::InletVolumeFlow { rate } => ItemKind::InletVolumeFlow { rate },
                FlowConditionKind::InletMassFlow { rate } => ItemKind::InletMassFlow { rate },
                FlowConditionKind::StaticPressure { pressure } => {
                    ItemKind::StaticPressure { pascals: pressure }
                }
                FlowConditionKind::IdealWall => ItemKind::IdealWall,
            },
            faces: faces(&c.faces),
        });
    }
    for g in &file.goals {
        study.items.push(StudyItem {
            name: g.name.clone(),
            kind: ItemKind::Goal(g.kind),
            faces: faces(&g.faces),
        });
    }
    study.flow = file.flow.clone();
    study.cooling = file.cooling;
    study.recount();
    study
}

/// `to` relative to the folder `from`, with `/` separators; its file name alone when the
/// two share no root (another drive), the document id finding it then.
pub(crate) fn relative_path(from: &Path, to: &Path) -> String {
    let from: Vec<_> = from.components().collect();
    let to_parts: Vec<_> = to.components().collect();
    let common = from
        .iter()
        .zip(&to_parts)
        .take_while(|(a, b)| a == b)
        .count();
    if common == 0 {
        return to
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    }
    let mut parts: Vec<String> = Vec::new();
    for _ in common..from.len() {
        parts.push("..".to_owned());
    }
    for c in to_parts.iter().skip(common) {
        parts.push(c.as_os_str().to_string_lossy().into_owned());
    }
    parts.join("/")
}

// ---- The results ----------------------------------------------------------------------------

/// The results of `done` as a `.oxyres` holds them.
pub(crate) fn results_file(
    done: &Done,
    simulation_id: &str,
    study: &str,
    fingerprint: Fingerprint,
) -> ResultsFile {
    let mesh = &done.mesh;
    let elements = match &mesh.edge_nodes {
        Some(edges) => Elements::Quadratic(
            mesh.tets
                .iter()
                .zip(edges)
                .map(|(c, e)| [c[0], c[1], c[2], c[3], e[0], e[1], e[2], e[3], e[4], e[5]])
                .collect(),
        ),
        None => Elements::Linear(mesh.tets.clone()),
    };
    let summary = done.results.as_ref().map(Results::summary);
    let thermal_summary = done.thermal.as_ref().map(ThermalResults::summary);
    let extreme = |(value, node): (f64, u32)| Extreme { value, node };
    let flow = done.flow.as_ref().map(|f| &f.run);
    let iterations = done.results.as_ref().map_or(0, |r| r.iterations)
        + done.thermal.as_ref().map_or(0, |t| t.iterations)
        + flow.map_or(0, |f| f.iterations as usize);
    let residual = done
        .results
        .as_ref()
        .map(|r| r.residual)
        .or_else(|| done.thermal.as_ref().map(|t| t.residual))
        .or_else(|| {
            // A flow's largest residual at its last iteration.
            flow.and_then(|f| f.residuals.last()).map(|row| {
                row.iter()
                    .copied()
                    .filter(|r| r.is_finite())
                    .fold(0.0, f64::max)
            })
        })
        .unwrap_or(0.0);
    ResultsFile {
        document_id: oxyde_format::new_document_id(),
        simulation_id: simulation_id.to_owned(),
        study: study.to_owned(),
        study_type: done.study_type,
        fingerprint,
        summary: oxyde_format::results::Summary {
            max_von_mises: summary.map(|s| extreme(s.max_von_mises)),
            max_displacement: summary.map(|s| extreme(s.max_displacement)),
            min_factor_of_safety: summary.and_then(|s| s.min_factor_of_safety.map(extreme)),
            max_temperature: thermal_summary.map(|t| extreme(t.max_temperature)),
            min_temperature: thermal_summary.map(|t| extreme(t.min_temperature)),
            max_heat_flux: thermal_summary.map(|t| extreme(t.max_heat_flux)),
        },
        solver: SolverInfo {
            program: format!("oxyde {}", env!("CARGO_PKG_VERSION")),
            mesher: done.mesher.to_owned(),
            method: match (&done.flow, done.study_type) {
                (Some(_), oxyde_format::simulation::StudyType::Cooling) => {
                    "chtMultiRegionSimpleFoam".to_owned()
                }
                (Some(_), _) => "simpleFoam".to_owned(),
                (None, _) => "pcg+amg".to_owned(),
            },
            iterations: iterations as u64,
            residual,
            seconds: done.mesh_seconds + done.solve_seconds,
        },
        bodies: done
            .bodies
            .iter()
            .map(|(name, material)| ResultBody {
                name: name.clone(),
                material: material.clone(),
            })
            .collect(),
        labels: done
            .faces
            .iter()
            .map(|(label, f)| (*label, study_face(f)))
            .collect(),
        mesh: ResultMesh {
            nodes: mesh.nodes.clone(),
            elements,
            element_bodies: mesh.bodies.clone(),
            facets: mesh
                .facets
                .iter()
                .map(|f| ResultFacet {
                    element: f.tet,
                    face: f.face,
                    label: f.label,
                })
                .collect(),
        },
        fields: match &done.results {
            Some(results) => NodalFields {
                displacement: results.displacement.clone(),
                stress: results.stress.clone(),
                von_mises: results.von_mises.clone(),
                yield_strength: results.yield_strength.clone(),
                contact_pressure: (!results.contact_pressure.is_empty())
                    .then(|| results.contact_pressure.clone()),
            },
            None => NodalFields::default(),
        },
        thermal: done.thermal.as_ref().map(|t| ThermalFields {
            temperature: t.temperature.clone(),
            heat_flux: t.heat_flux.clone(),
            history: t
                .frames
                .iter()
                .map(|f| TemperatureFrame {
                    time: f.time,
                    temperature: f.temperature.clone(),
                })
                .collect(),
        }),
        flow: flow.cloned(),
    }
}

/// What a `.oxyres` holds, as the tab shows it.
pub(crate) struct LoadedResults {
    pub mesh: TetMesh,
    pub results: Option<Results>,
    pub thermal: Option<ThermalResults>,
}

/// The mesh and results a `.oxyres` holds, or why they cannot be used.
pub(crate) fn mesh_and_results(file: &ResultsFile) -> Result<LoadedResults, String> {
    let m = &file.mesh;
    let (tets, edge_nodes) = match &m.elements {
        Elements::Linear(e) => (e.clone(), None),
        Elements::Quadratic(e) => (
            e.iter().map(|n| [n[0], n[1], n[2], n[3]]).collect(),
            Some(
                e.iter()
                    .map(|n| [n[4], n[5], n[6], n[7], n[8], n[9]])
                    .collect(),
            ),
        ),
    };
    let mesh = TetMesh {
        nodes: m.nodes.clone(),
        tets,
        edge_nodes,
        bodies: m.element_bodies.clone(),
        facets: m
            .facets
            .iter()
            .map(|f| Facet {
                tet: f.element,
                face: f.face,
                label: f.label,
            })
            .collect(),
    };
    mesh.validate()?;
    let f = &file.fields;
    let n = mesh.nodes.len();
    let results = if file.study_type.is_structural() {
        if [
            f.displacement.len(),
            f.stress.len(),
            f.von_mises.len(),
            f.yield_strength.len(),
        ]
        .iter()
        .any(|&len| len != n)
        {
            return Err("its fields do not match its mesh".to_owned());
        }
        Some(Results {
            displacement: f.displacement.clone(),
            stress: f.stress.clone(),
            von_mises: f.von_mises.clone(),
            yield_strength: f.yield_strength.clone(),
            iterations: file.solver.iterations as usize,
            residual: file.solver.residual,
            contact_pressure: f.contact_pressure.clone().unwrap_or_default(),
            contacts: Vec::new(),
            contact_solve: Default::default(),
        })
    } else {
        None
    };
    let thermal = match &file.thermal {
        Some(t) if t.temperature.len() != n || t.heat_flux.len() != n => {
            return Err("its temperatures do not match its mesh".to_owned());
        }
        Some(t) => Some(ThermalResults {
            temperature: t.temperature.clone(),
            heat_flux: t.heat_flux.clone(),
            frames: t
                .history
                .iter()
                .map(|f| Frame {
                    time: f.time,
                    temperature: f.temperature.clone(),
                })
                .collect(),
            iterations: file.solver.iterations as usize,
            residual: file.solver.residual,
            nonlinear_iterations: 0,
        }),
        None => None,
    };
    Ok(LoadedResults {
        mesh,
        results,
        thermal,
    })
}

/// Where a study's results are saved: beside the study file, `<study file>-<study>.oxyres`,
/// the study's name made safe for a file name.
pub(crate) fn results_path(study_file: &Path, study: &str) -> PathBuf {
    let stem = study_file
        .file_stem()
        .map_or_else(|| "Study".to_owned(), |s| s.to_string_lossy().into_owned());
    let safe: String = study
        .chars()
        .map(|c| {
            if c.is_control() || "/\\:*?\"<>|".contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    study_file.with_file_name(format!(
        "{stem}-{safe}.{}",
        oxyde_format::results::RESULTS_EXTENSION
    ))
}

// ---- The application side ------------------------------------------------------------------

impl OxydeApp {
    /// The model as it is now, and the study, for results: every part and assembly file's
    /// hash and the study's.
    pub(crate) fn simulation_fingerprint(&self) -> Result<Fingerprint, String> {
        let error = |e: oxyde_format::FormatError| e.to_string();
        let study = oxyde_format::results::study_hash(&file_study(&self.simulation.study))
            .map_err(error)?;
        let mut documents: Vec<DocumentHash> = Vec::new();
        match &self.asm {
            None => documents
                .push(oxyde_format::results::part_hash(&self.doc.to_file()).map_err(error)?),
            Some(asm) => {
                documents
                    .push(oxyde_format::results::assembly_hash(&asm.to_file()).map_err(error)?);
                for loaded in asm.docs.values().flatten() {
                    let hash = match loaded {
                        Loaded::Part(p) => oxyde_format::results::part_hash(&PartFile {
                            document_id: p.document_id.clone(),
                            definition: p.part.definition(),
                            properties: p.properties.clone(),
                        }),
                        Loaded::Assembly(a) => {
                            oxyde_format::results::assembly_hash(&AssemblyFile {
                                document_id: a.document_id.clone(),
                                assembly: a.assembly.clone(),
                                properties: a.properties.clone(),
                            })
                        }
                    };
                    documents.push(hash.map_err(error)?);
                }
            }
        }
        Ok(Fingerprint::new(study, documents))
    }

    /// The open model's path, document id and kind, when it has been saved.
    fn studied_model(&self) -> Option<(PathBuf, String, ModelKind)> {
        match &self.asm {
            Some(asm) => Some((
                asm.path.clone()?,
                asm.document_id.clone(),
                ModelKind::Assembly,
            )),
            None => Some((
                self.doc.path.clone()?,
                self.doc.document_id.clone(),
                ModelKind::Part,
            )),
        }
    }

    /// Save Study (`ask`: Save Study As): the study to its `.oxysim`, next to the model by
    /// default. Whether it was saved.
    pub(crate) fn save_study(&mut self, ask: bool) -> bool {
        let Some((model_path, model_id, kind)) = self.studied_model() else {
            self.status = "Save the model first: a study refers to its file".to_owned();
            return false;
        };
        let existing = self.simulation.file.clone();
        let path = match (&existing, ask) {
            (Some(f), false) => f.path.clone(),
            _ => {
                let stem = model_path
                    .file_stem()
                    .map_or_else(|| "Study".to_owned(), |s| s.to_string_lossy().into_owned());
                let mut dialog = rfd::FileDialog::new()
                    .add_filter(
                        "Oxyde study",
                        &[oxyde_format::simulation::SIMULATION_EXTENSION],
                    )
                    .set_file_name(format!("{stem}.oxysim"));
                if let Some(dir) = model_path.parent() {
                    dialog = dialog.set_directory(dir);
                }
                let Some(mut path) = dialog.save_file() else {
                    return false;
                };
                if !crate::assembly_ui::has_extension(&path, "oxysim") {
                    path.set_extension("oxysim");
                }
                path
            }
        };
        let folder = path.parent().map(Path::to_path_buf).unwrap_or_default();
        let model = StudiedModel {
            kind,
            path: relative_path(&folder, &model_path),
            document_id: model_id,
        };
        let mut file = SimulationFile::new(model);
        if let Some(f) = existing.filter(|f| f.path == path) {
            file.document_id = f.document_id;
        }
        file.studies = self
            .simulation
            .all_studies()
            .iter()
            .map(file_study)
            .collect();
        if file.studies.is_empty() {
            self.status = "No study to save: make one with New Study".to_owned();
            return false;
        }
        match oxyde_format::simulation::save_simulation(&path, &file) {
            Ok(()) => {
                self.status = format!("Saved the study to {}", path.display());
                self.simulation.file = Some(StudyFile {
                    path,
                    document_id: file.document_id,
                });
                true
            }
            Err(e) => {
                self.status = format!("Could not save the study: {e}");
                false
            }
        }
    }

    /// Open Study: a `.oxysim` made for the open model.
    pub(crate) fn open_study(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter(
                "Oxyde study",
                &[oxyde_format::simulation::SIMULATION_EXTENSION],
            )
            .pick_file()
        else {
            return;
        };
        let file = match oxyde_format::simulation::load_simulation(&path) {
            Ok(f) => f,
            Err(e) => {
                self.status = format!("Could not open {}: {e}", path.display());
                return;
            }
        };
        let current = match &self.asm {
            Some(asm) => asm.document_id.clone(),
            None => self.doc.document_id.clone(),
        };
        if file.model.document_id != current {
            self.status = format!(
                "{} is a study of {}, not of the open model: open that model first",
                path.display(),
                file.model.path
            );
            return;
        }
        if file.studies.is_empty() {
            self.status = format!("{} holds no study", path.display());
            return;
        }
        let count = file.studies.len();
        self.set_studies(file.studies.iter().map(study_of).collect());
        self.status = if count > 1 {
            format!("Opened {count} studies from {}", path.display())
        } else {
            format!("Opened the study {}", self.simulation.study.name)
        };
        self.simulation.file = Some(StudyFile {
            path,
            document_id: file.document_id,
        });
    }

    /// When the model changes: the studies saved next to it as `<model>.oxysim` (made
    /// for it, by document id), if there are any, as SolidWorks keeps a model's studies
    /// with it. Their results are not read; *Open results…* or *Run* shows them.
    pub(crate) fn open_saved_studies(&mut self) {
        let Some((model_path, model_id, _)) = self.studied_model() else {
            return;
        };
        let path = model_path.with_extension(oxyde_format::simulation::SIMULATION_EXTENSION);
        if !path.is_file() {
            return;
        }
        let Ok(file) = oxyde_format::simulation::load_simulation(&path) else {
            return;
        };
        if file.model.document_id != model_id || file.studies.is_empty() {
            return;
        }
        self.set_studies(file.studies.iter().map(study_of).collect());
        self.simulation.file = Some(StudyFile {
            path,
            document_id: file.document_id,
        });
    }

    /// Save Results (`ask`: choose where): the results of the last run to a `.oxyres`, by
    /// default next to the study, which is saved first if it has never been. Whether they
    /// were saved.
    pub(crate) fn save_results(&mut self, ask: bool) -> bool {
        if self.simulation.file.is_none() && !self.save_study(true) {
            return false;
        }
        let Some(study_file) = self.simulation.file.clone() else {
            return false;
        };
        // A flow study's case goes beside its study file, as its results do.
        if let Err(e) = self.keep_flow_case(&study_file.path) {
            self.status = e;
            return false;
        }
        let Some(done) = self.simulation.done.as_ref() else {
            self.status = "Nothing to save: run the study first".to_owned();
            return false;
        };
        let (true, Some(fingerprint)) = (done.solved(), &done.fingerprint) else {
            self.status = "Nothing to save: run the study first".to_owned();
            return false;
        };
        let mut path = results_path(&study_file.path, &self.simulation.study.name);
        if ask {
            let Some(chosen) = rfd::FileDialog::new()
                .add_filter("Oxyde results", &[oxyde_format::results::RESULTS_EXTENSION])
                .set_file_name(
                    path.file_name()
                        .map_or_else(String::new, |n| n.to_string_lossy().into_owned()),
                )
                .save_file()
            else {
                return false;
            };
            path = chosen;
        }
        let mut file = results_file(
            done,
            &study_file.document_id,
            &self.simulation.study.name,
            fingerprint.clone(),
        );
        if let (Some(run), Some(flow)) = (file.flow.as_mut(), done.flow.as_ref()) {
            let folder = path.parent().map(Path::to_path_buf).unwrap_or_default();
            run.case = relative_path(&folder, &flow.case_dir);
        }
        match oxyde_format::results::save_results(&path, &file) {
            Ok(()) => {
                self.status = format!("Saved the results to {}", path.display());
                true
            }
            Err(e) => {
                self.status = format!("Could not save the results: {e}");
                false
            }
        }
    }

    /// Open Results: a `.oxyres` of the open study and model, shown if the model and the
    /// study are still as they were solved.
    pub(crate) fn open_results(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("Oxyde results", &[oxyde_format::results::RESULTS_EXTENSION])
            .pick_file()
        else {
            return;
        };
        let file = match oxyde_format::results::load_results(&path) {
            Ok(f) => f,
            Err(e) => {
                self.status = format!("Could not open {}: {e}", path.display());
                return;
            }
        };
        if let Some(study) = &self.simulation.file
            && study.document_id != file.simulation_id
        {
            self.status = format!("{} are the results of another study file", path.display());
            return;
        }
        // The results of another of the model's studies: that one opens.
        if self.simulation.study.name != file.study {
            match self
                .simulation
                .study_list()
                .iter()
                .position(|(name, _)| *name == file.study)
            {
                Some(i) => self.switch_study(i),
                None => {
                    self.status = format!(
                        "{} are the results of {}, which is not one of this model's studies",
                        path.display(),
                        file.study
                    );
                    return;
                }
            }
        }
        let now = match self.simulation_fingerprint() {
            Ok(f) => f,
            Err(e) => {
                self.status = format!("Could not compare the results with the model: {e}");
                return;
            }
        };
        let changes = file.fingerprint.changes(&now);
        if !changes.is_empty() {
            let what = if changes.iter().any(|c| c == "study") {
                "the study (fixtures, loads, contacts, mesh or materials)"
            } else {
                "the model"
            };
            self.status = format!(
                "{} are out of date: {what} changed since they were computed. Run the study again.",
                path.display()
            );
            return;
        }
        let faces: BTreeMap<u32, SimFace> =
            file.labels.iter().map(|(k, f)| (*k, sim_face(f))).collect();
        if let Some(run) = file.flow.clone() {
            // A flow's fields are in its case folder, beside the results.
            let folder = path.parent().map(Path::to_path_buf).unwrap_or_default();
            let case_dir = folder.join(&run.case);
            self.show_loaded_flow(run, case_dir.clone(), faces, file.fingerprint, &file.solver);
            self.status = if self
                .simulation
                .done
                .as_ref()
                .and_then(|d| d.flow.as_ref())
                .is_some_and(|f| f.fields.is_some())
            {
                format!("Opened the results {}", path.display())
            } else {
                format!(
                    "Opened the results {}, without plots: their OpenFOAM case {} is not there",
                    path.display(),
                    case_dir.display()
                )
            };
            return;
        }
        let loaded = match mesh_and_results(&file) {
            Ok(r) => r,
            Err(e) => {
                self.status = format!("Could not use {}: {e}", path.display());
                return;
            }
        };
        let bodies = file
            .bodies
            .iter()
            .map(|b| (b.name.clone(), b.material.clone()))
            .collect();
        self.show_loaded_results(
            loaded.mesh,
            file.study_type,
            loaded.results,
            loaded.thermal,
            faces,
            bodies,
            file.fingerprint,
            &file.solver,
        );
        self.status = format!("Opened the results {}", path.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxyde_format::simulation::{StudyType, TransientSettings};
    use oxyde_model::{FaceName, FeatureId, Thermal};

    fn face(feature: u32) -> SimFace {
        SimFace {
            occurrence: None,
            face: FaceName::end_cap(FeatureId::from_raw(feature)),
        }
    }

    #[test]
    fn a_thermal_study_survives_its_file() {
        let mut study = Study::of_type(StudyType::TransientThermal, "Transient 1");
        for (k, kind) in [
            ItemKind::Temperature { celsius: 80.0 },
            ItemKind::HeatPower { watts: 12.5 },
            ItemKind::HeatFlux { flux: -1500.0 },
            ItemKind::Convection {
                coefficient: 25.0,
                ambient: 20.0,
            },
            ItemKind::Radiation {
                emissivity: 0.9,
                ambient: 15.0,
            },
            ItemKind::InternalHeat { watts: 5.0 },
        ]
        .into_iter()
        .enumerate()
        {
            study.items.push(StudyItem {
                name: format!("{}-1", kind.base_name()),
                kind,
                faces: vec![face(k as u32 + 2)],
            });
        }
        study.transient = TransientSettings {
            duration: 300.0,
            steps: 30,
            initial_temperature: 25.0,
        };
        study.thermal_overrides.insert(
            "Copper (C110)".to_owned(),
            Thermal::new(391.0, 385.0, 17e-6),
        );
        let mut back = study_of(&file_study(&study));
        assert_eq!(back.study_type, StudyType::TransientThermal);
        // The counters are rebuilt from the names read.
        back.made.clear();
        study.made.clear();
        assert_eq!(back, study);
        // A new item is numbered after the ones read.
        let mut read = study_of(&file_study(&study));
        read.add(ItemKind::Temperature { celsius: 0.0 }, vec![face(9)]);
        assert_eq!(
            read.items.last().map(|i| i.name.as_str()),
            Some("Temperature-2")
        );
    }

    #[test]
    fn a_study_keeps_only_the_items_its_type_takes() {
        // A static study's file never holds a thermal load, which readers would refuse.
        let mut study = Study::of_type(StudyType::Static, "Static 1");
        study.items.push(StudyItem {
            name: "Fixed-1".to_owned(),
            kind: ItemKind::Fixed,
            faces: vec![face(2)],
        });
        study.items.push(StudyItem {
            name: "Temperature-1".to_owned(),
            kind: ItemKind::Temperature { celsius: 50.0 },
            faces: vec![face(3)],
        });
        let file = file_study(&study);
        assert_eq!(file.fixtures.len(), 1);
        assert!(file.loads.is_empty());
        // A thermal stress study keeps both.
        study.study_type = StudyType::ThermalStress;
        let file = file_study(&study);
        assert_eq!((file.fixtures.len(), file.loads.len()), (1, 1));
    }

    #[test]
    fn a_flow_study_survives_its_file() {
        use oxyde_format::simulation::{FlowAnalysis, FlowGoalKind, FlowTurbulence, Fluid};
        let mut study = Study::of_type(StudyType::Flow, "Flow 1");
        for (k, kind) in [
            ItemKind::InletVelocity { speed: 2.0 },
            ItemKind::InletVolumeFlow { rate: 1e-4 },
            ItemKind::InletMassFlow { rate: 0.2 },
            ItemKind::StaticPressure { pascals: -50.0 },
            ItemKind::IdealWall,
            ItemKind::Goal(FlowGoalKind::Force),
            ItemKind::Goal(FlowGoalKind::MeanPressure),
        ]
        .into_iter()
        .enumerate()
        {
            study.items.push(StudyItem {
                name: format!("{}-1", kind.base_name()),
                kind,
                faces: vec![face(k as u32 + 2)],
            });
        }
        study.flow.analysis = FlowAnalysis::External {
            velocity: [0.0, 0.0, -3.0],
        };
        study.flow.fluid = Fluid::air();
        study.flow.turbulence = FlowTurbulence::Turbulent {
            intensity: 0.03,
            length: 2.0,
        };
        study.flow.mesh_level = 5;
        let file = file_study(&study);
        assert_eq!((file.conditions.len(), file.goals.len()), (5, 2));
        let mut back = study_of(&file);
        back.made.clear();
        study.made.clear();
        // Conditions come back first, then goals: the order the panel lists them in.
        assert_eq!(back, study);
        // A thermal study's file holds none of a flow study's items.
        let mut thermal = study.clone();
        thermal.study_type = StudyType::Thermal;
        let file = file_study(&thermal);
        assert!(file.conditions.is_empty() && file.goals.is_empty());
    }

    #[test]
    fn a_cooling_study_survives_its_file() {
        use oxyde_format::simulation::{CoolingSettings, FlowGoalKind};
        let mut study = Study::of_type(StudyType::Cooling, "Cooling 1");
        for (k, kind) in [
            ItemKind::InletVelocity { speed: 0.5 },
            ItemKind::StaticPressure { pascals: 0.0 },
            ItemKind::InternalHeat { watts: 12.0 },
            ItemKind::Convection {
                coefficient: 10.0,
                ambient: 25.0,
            },
            ItemKind::Goal(FlowGoalKind::MaxTemperature),
            ItemKind::Goal(FlowGoalKind::HeatFlow),
        ]
        .into_iter()
        .enumerate()
        {
            study.items.push(StudyItem {
                name: format!("{}-1", kind.base_name()),
                kind,
                faces: vec![face(k as u32 + 2)],
            });
        }
        study.cooling = CoolingSettings {
            temperature: 35.0,
            gravity: Some([0.0, -9.81, 0.0]),
            ..CoolingSettings::air()
        };
        let file = file_study(&study);
        assert_eq!(
            (file.conditions.len(), file.loads.len(), file.goals.len()),
            (2, 2, 2)
        );
        assert_eq!(file.cooling, study.cooling);
        let mut back = study_of(&file);
        back.made.clear();
        study.made.clear();
        // Conditions, then loads, then goals: their order in the file.
        let order = |s: &Study| -> Vec<String> { s.items.iter().map(|i| i.name.clone()).collect() };
        assert_eq!(order(&back).len(), 6);
        back.items.sort_by(|a, b| a.name.cmp(&b.name));
        study.items.sort_by(|a, b| a.name.cmp(&b.name));
        assert_eq!(back, study);
        // A flow study takes neither the heat nor the thermal goals.
        let mut flow = study.clone();
        flow.study_type = StudyType::Flow;
        let file = file_study(&flow);
        assert!(file.loads.is_empty());
        assert_eq!(file.goals.len(), 0);
    }

    #[test]
    fn each_study_has_its_results_file() {
        let study = Path::new("/models/bracket.oxysim");
        assert_eq!(
            results_path(study, "Thermal 1"),
            Path::new("/models/bracket-Thermal 1.oxyres")
        );
        assert_eq!(
            results_path(study, "A/B: hot?"),
            Path::new("/models/bracket-A_B_ hot_.oxyres")
        );
    }
}
