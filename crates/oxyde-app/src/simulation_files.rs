//! The Simulation tab's files: the study saved as a `.oxysim` document and its results as
//! a `.oxyres` (`oxyde_format::simulation`, `oxyde_format::results`, FORMAT.md §8–§9).
//!
//! A study document names its part or assembly by relative path and document id, and holds
//! the fixtures, loads, contacts, mesh settings and material overrides by persistent face
//! name. A results document holds the mesh and the nodal fields, tied to the study by its
//! document id and to the model by a fingerprint (a hash of every part and assembly file's
//! definition and of the study itself), so results of an earlier model are told apart when
//! they are opened. After *Run*, the results of a saved study are written next to it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use oxyde_format::results::{
    DocumentHash, Elements, Extreme, Fingerprint, NodalFields, ResultBody, ResultFacet, ResultMesh,
    ResultsFile, SolverInfo,
};
use oxyde_format::simulation::{
    Contact as FileContact, ContactDetection, ContactKind as FileContactKind, Fixture,
    FixtureKind as FileFixtureKind, ForceDirection, Load, LoadKind as FileLoadKind, MeshQuality,
    MeshSettings, MeshSize, Mesher, ModelKind, SimulationFile, StudiedModel, Study as FileStudy,
    StudyFace,
};
use oxyde_format::{AssemblyFile, PartFile};
use oxyde_sim::{ContactKind, Facet, Results, TetMesh};

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

/// The study as a `.oxysim` holds it.
pub(crate) fn file_study(study: &Study) -> FileStudy {
    let mut out = FileStudy::new(&study.name);
    let faces = |faces: &[SimFace]| faces.iter().map(study_face).collect::<Vec<_>>();
    for item in &study.items {
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
        }
    }
    if study.gravity {
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
    out
}

/// The study a `.oxysim` holds, as the panel edits it. Suppressed items are left out (the
/// panel has no switch for them yet); a gravity load other than −Y becomes −Y.
pub(crate) fn study_of(file: &FileStudy) -> Study {
    let mut study = Study {
        name: file.name.clone(),
        ..Study::default()
    };
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
    results: &Results,
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
    let summary = results.summary();
    let extreme = |(value, node): (f64, u32)| Extreme { value, node };
    ResultsFile {
        document_id: oxyde_format::new_document_id(),
        simulation_id: simulation_id.to_owned(),
        study: study.to_owned(),
        fingerprint,
        summary: oxyde_format::results::Summary {
            max_von_mises: extreme(summary.max_von_mises),
            max_displacement: extreme(summary.max_displacement),
            min_factor_of_safety: summary.min_factor_of_safety.map(extreme),
        },
        solver: SolverInfo {
            program: format!("oxyde {}", env!("CARGO_PKG_VERSION")),
            mesher: done.mesher.to_owned(),
            method: "pcg+amg".to_owned(),
            iterations: results.iterations as u64,
            residual: results.residual,
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
        fields: NodalFields {
            displacement: results.displacement.clone(),
            stress: results.stress.clone(),
            von_mises: results.von_mises.clone(),
            yield_strength: results.yield_strength.clone(),
            contact_pressure: (!results.contact_pressure.is_empty())
                .then(|| results.contact_pressure.clone()),
        },
    }
}

/// The mesh and results a `.oxyres` holds, or why they cannot be used.
pub(crate) fn mesh_and_results(file: &ResultsFile) -> Result<(TetMesh, Results), String> {
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
    let results = Results {
        displacement: f.displacement.clone(),
        stress: f.stress.clone(),
        von_mises: f.von_mises.clone(),
        yield_strength: f.yield_strength.clone(),
        iterations: file.solver.iterations as usize,
        residual: file.solver.residual,
        contact_pressure: f.contact_pressure.clone().unwrap_or_default(),
        contacts: Vec::new(),
        contact_solve: Default::default(),
    };
    Ok((mesh, results))
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
        file.studies = vec![file_study(&self.simulation.study)];
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
        let Some(first) = file.studies.first() else {
            self.status = format!("{} holds no study", path.display());
            return;
        };
        let more = file.studies.len() - 1;
        self.set_study(study_of(first));
        self.status = if more > 0 {
            format!(
                "Opened {} (its first study; {more} more not shown)",
                first.name
            )
        } else {
            format!("Opened the study {}", first.name)
        };
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
        let Some(done) = self.simulation.done.as_ref() else {
            self.status = "Nothing to save: run the study first".to_owned();
            return false;
        };
        let (Some(results), Some(fingerprint)) = (&done.results, &done.fingerprint) else {
            self.status = "Nothing to save: run the study first".to_owned();
            return false;
        };
        let mut path = study_file
            .path
            .with_extension(oxyde_format::results::RESULTS_EXTENSION);
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
        let file = results_file(
            done,
            results,
            &study_file.document_id,
            &self.simulation.study.name,
            fingerprint.clone(),
        );
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
        let (mesh, results) = match mesh_and_results(&file) {
            Ok(r) => r,
            Err(e) => {
                self.status = format!("Could not use {}: {e}", path.display());
                return;
            }
        };
        let faces: BTreeMap<u32, SimFace> =
            file.labels.iter().map(|(k, f)| (*k, sim_face(f))).collect();
        let bodies = file
            .bodies
            .iter()
            .map(|b| (b.name.clone(), b.material.clone()))
            .collect();
        self.show_loaded_results(mesh, results, faces, bodies, file.fingerprint, &file.solver);
        self.status = format!("Opened the results {}", path.display());
    }
}
