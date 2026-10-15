//! Assemblies: parts placed in space and held together by mates.
//!
//! - [`Assembly`] lists [`Component`]s (instances of part files, each with a placement)
//!   and [`Mate`]s between their faces, edges, planes and axes. Bottom-up only: a part is
//!   edited in its own document, the assembly just places it.
//! - [`solve`] moves the components that are not fixed until every mate holds, and tells
//!   which mates cannot be met and which components can still move.
//! - [`bom`] counts the parts for a bill of materials.
//!
//! Pure Rust: mate references are persistent face and edge names from `oxyde-model`, and
//! [`resolve`] turns them into geometry using a part's rebuilt body.
//! See `docs/ARCHITECTURE.md` §8.

pub mod bom;
mod mate;
mod placement;
mod solve;

pub use mate::{Mate, MateKind, MateRef, MateTarget, Primitive, PrimitiveKind, resolve};
pub use placement::Placement;
pub use solve::{ComponentFreedom, MateStatus, Solution, default_alignment, solve};

/// Identifies a component within its assembly. Never reused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ComponentId(pub u32);

/// Identifies a mate within its assembly. Never reused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MateId(pub u32);

/// One placed instance of a part file.
#[derive(Debug, Clone, PartialEq)]
pub struct Component {
    pub id: ComponentId,
    /// Instance name, unique in the assembly: `Bracket<1>`, `Bracket<2>`…
    pub name: String,
    /// The part file, relative to the assembly's folder, with `/` separators.
    pub path: String,
    /// The part's document id, to find it again after it was moved or renamed.
    pub document_id: String,
    pub placement: Placement,
    /// Fixed components never move; the first one inserted is fixed, as in SolidWorks.
    pub fixed: bool,
    pub hidden: bool,
}

/// An assembly document's content.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Assembly {
    pub components: Vec<Component>,
    pub mates: Vec<Mate>,
    pub next_component_id: u32,
    pub next_mate_id: u32,
}

impl Assembly {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn component(&self, id: ComponentId) -> Option<&Component> {
        self.components.iter().find(|c| c.id == id)
    }

    pub fn component_mut(&mut self, id: ComponentId) -> Option<&mut Component> {
        self.components.iter_mut().find(|c| c.id == id)
    }

    pub fn mate(&self, id: MateId) -> Option<&Mate> {
        self.mates.iter().find(|m| m.id == id)
    }

    pub fn mate_mut(&mut self, id: MateId) -> Option<&mut Mate> {
        self.mates.iter_mut().find(|m| m.id == id)
    }

    /// Adds an instance of a part. `stem` is the file name without extension; the
    /// instance is named `stem<n>` with the first free `n`. The first component is fixed.
    pub fn add_component(
        &mut self,
        stem: &str,
        path: String,
        document_id: String,
        placement: Placement,
    ) -> ComponentId {
        self.next_component_id = self
            .next_component_id
            .max(
                self.components
                    .iter()
                    .map(|c| c.id.0 + 1)
                    .max()
                    .unwrap_or(1),
            )
            .max(1);
        let id = ComponentId(self.next_component_id);
        self.next_component_id += 1;
        let name = (1..)
            .map(|n| format!("{stem}<{n}>"))
            .find(|name| self.components.iter().all(|c| &c.name != name))
            .expect("some instance number is free");
        let fixed = self.components.is_empty();
        self.components.push(Component {
            id,
            name,
            path,
            document_id,
            placement,
            fixed,
            hidden: false,
        });
        id
    }

    /// Adds a mate, named after its kind (`Coincident1`, `Distance2`…).
    pub fn add_mate(&mut self, kind: MateKind, a: MateRef, b: MateRef, aligned: bool) -> MateId {
        self.next_mate_id = self
            .next_mate_id
            .max(self.mates.iter().map(|m| m.id.0 + 1).max().unwrap_or(1))
            .max(1);
        let id = MateId(self.next_mate_id);
        self.next_mate_id += 1;
        let name = (1..)
            .map(|n| format!("{}{n}", kind.label()))
            .find(|name| self.mates.iter().all(|m| &m.name != name))
            .expect("some mate number is free");
        self.mates.push(Mate {
            id,
            name,
            kind,
            a,
            b,
            aligned,
            suppressed: false,
        });
        id
    }

    /// Removes a component and the mates that use it. Returns the removed mates' names.
    pub fn remove_component(&mut self, id: ComponentId) -> Vec<String> {
        self.components.retain(|c| c.id != id);
        let mut removed = Vec::new();
        self.mates.retain(|m| {
            let uses = m.a.component == id || m.b.component == id;
            if uses {
                removed.push(m.name.clone());
            }
            !uses
        });
        removed
    }

    pub fn remove_mate(&mut self, id: MateId) {
        self.mates.retain(|m| m.id != id);
    }

    /// Mates that use a component.
    pub fn mates_of(&self, id: ComponentId) -> impl Iterator<Item = &Mate> {
        self.mates
            .iter()
            .filter(move |m| m.a.component == id || m.b.component == id)
    }
}

#[cfg(test)]
mod tests;
