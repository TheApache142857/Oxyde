//! Appearances: how a part, a body or a face looks in the viewport — a colour, how
//! see-through it is, and optionally a colour texture (an image repeated over the
//! surface).
//!
//! As in SolidWorks, an appearance can be put on the whole part, on one body, or on single
//! faces, and the most specific one wins: face over body over part. A part with no
//! appearance of its own is drawn in the viewport's part colour. Appearances are display
//! properties, not features: they do not take part in the rebuild and are not in the
//! timeline, but they are part of the document and of its undo history.

use crate::feature::{BodyId, FileData};
use crate::naming::FaceName;

/// How something looks.
#[derive(Debug, Clone, PartialEq)]
pub struct Appearance {
    /// sRGB, 0–255 per channel.
    pub color: [u8; 3],
    /// 0 = opaque, 1 = invisible.
    pub transparency: f64,
    /// A colour texture repeated over the surface; it replaces the colour where it is.
    pub texture: Option<Texture>,
}

impl Appearance {
    /// A plain, opaque colour.
    pub fn colored(color: [u8; 3]) -> Self {
        Self {
            color,
            transparency: 0.0,
            texture: None,
        }
    }

    /// The transparency, kept in 0..1.
    pub fn clamped(mut self) -> Self {
        if !self.transparency.is_finite() {
            self.transparency = 0.0;
        }
        self.transparency = self.transparency.clamp(0.0, 1.0);
        if let Some(t) = &mut self.texture {
            if !t.scale.is_finite() || t.scale <= 0.0 {
                t.scale = Texture::DEFAULT_SCALE;
            }
        }
        self
    }
}

impl Default for Appearance {
    /// The default colour, before the viewport's own: SolidWorks' grey-blue.
    fn default() -> Self {
        Self::colored([182, 190, 202])
    }
}

/// A colour texture: an image (PNG or JPEG) repeated over the surface. It is mapped
/// automatically along the model's axes, so no texture coordinates are needed: one repeat
/// covers `scale` millimetres.
#[derive(Debug, Clone, PartialEq)]
pub struct Texture {
    /// Name of the image file, for display.
    pub file_name: String,
    /// The image file's contents.
    pub data: FileData,
    /// Size of one repeat of the image on the part, in mm (along the image's width).
    pub scale: f64,
}

impl Texture {
    pub const DEFAULT_SCALE: f64 = 50.0;
}

/// The appearances of a part: its own, and those of single bodies and faces, which win
/// over it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Appearances {
    pub part: Option<Appearance>,
    pub bodies: Vec<(BodyId, Appearance)>,
    pub faces: Vec<(FaceName, Appearance)>,
}

impl Appearances {
    pub fn is_empty(&self) -> bool {
        self.part.is_none() && self.bodies.is_empty() && self.faces.is_empty()
    }

    pub fn body(&self, id: BodyId) -> Option<&Appearance> {
        self.bodies.iter().find(|(b, _)| *b == id).map(|(_, a)| a)
    }

    pub fn face(&self, name: &FaceName) -> Option<&Appearance> {
        self.faces.iter().find(|(f, _)| f == name).map(|(_, a)| a)
    }

    /// Puts `appearance` on a body (replacing the body's previous one), or takes it off
    /// with `None`.
    pub fn set_body(&mut self, id: BodyId, appearance: Option<Appearance>) {
        self.bodies.retain(|(b, _)| *b != id);
        if let Some(a) = appearance {
            self.bodies.push((id, a.clamped()));
        }
    }

    /// Same for a face.
    pub fn set_face(&mut self, name: &FaceName, appearance: Option<Appearance>) {
        self.faces.retain(|(f, _)| f != name);
        if let Some(a) = appearance {
            self.faces.push((name.clone(), a.clamped()));
        }
    }

    pub fn set_part(&mut self, appearance: Option<Appearance>) {
        self.part = appearance.map(Appearance::clamped);
    }

    /// The appearance a face is drawn with: its own, else its body's, else the part's.
    /// `None` means the viewport's default.
    pub fn of_face(&self, name: Option<&FaceName>, body: Option<BodyId>) -> Option<&Appearance> {
        name.and_then(|n| self.face(n))
            .or_else(|| body.and_then(|b| self.body(b)))
            .or(self.part.as_ref())
    }

    /// Forgets the appearances of faces and bodies made by `feature` (deleted).
    pub fn remove_feature(&mut self, feature: crate::feature::FeatureId) {
        self.bodies.retain(|(b, _)| b.feature != feature);
        self.faces.retain(|(f, _)| f.key.feature() != feature);
    }

    /// Every texture used, each once (by its bytes).
    pub fn textures(&self) -> Vec<&Texture> {
        let mut out: Vec<&Texture> = Vec::new();
        let all = self
            .part
            .iter()
            .chain(self.bodies.iter().map(|(_, a)| a))
            .chain(self.faces.iter().map(|(_, a)| a));
        for appearance in all {
            if let Some(t) = &appearance.texture {
                if !out.iter().any(|o| o.data == t.data) {
                    out.push(t);
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feature::FeatureId;

    #[test]
    fn the_most_specific_appearance_wins() {
        let mut a = Appearances::default();
        let f1 = FeatureId::from_raw(1);
        let body = BodyId::new(f1, 0);
        let face = FaceName::start_cap(f1);
        assert!(a.of_face(Some(&face), Some(body)).is_none());
        a.set_part(Some(Appearance::colored([1, 1, 1])));
        assert_eq!(a.of_face(Some(&face), Some(body)).unwrap().color, [1, 1, 1]);
        a.set_body(body, Some(Appearance::colored([2, 2, 2])));
        assert_eq!(a.of_face(Some(&face), Some(body)).unwrap().color, [2, 2, 2]);
        a.set_face(&face, Some(Appearance::colored([3, 3, 3])));
        assert_eq!(a.of_face(Some(&face), Some(body)).unwrap().color, [3, 3, 3]);
        // Another face of the same body keeps the body's.
        let other = FaceName::end_cap(f1);
        assert_eq!(a.of_face(Some(&other), Some(body)).unwrap().color, [2, 2, 2]);
        a.set_face(&face, None);
        assert_eq!(a.of_face(Some(&face), Some(body)).unwrap().color, [2, 2, 2]);
        a.remove_feature(f1);
        assert_eq!(a.of_face(Some(&face), Some(body)).unwrap().color, [1, 1, 1]);
    }

    #[test]
    fn values_are_kept_in_range_and_textures_listed_once() {
        let mut a = Appearances::default();
        let data = FileData::new(vec![1, 2, 3]);
        let texture = Texture {
            file_name: "wood.png".into(),
            data: data.clone(),
            scale: -4.0,
        };
        a.set_part(Some(Appearance {
            color: [0; 3],
            transparency: 7.0,
            texture: Some(texture.clone()),
        }));
        a.set_body(
            BodyId::new(FeatureId::from_raw(2), 0),
            Some(Appearance {
                color: [0; 3],
                transparency: -1.0,
                texture: Some(Texture {
                    file_name: "copy.png".into(),
                    data,
                    scale: 10.0,
                }),
            }),
        );
        let part = a.part.as_ref().unwrap();
        assert_eq!(part.transparency, 1.0);
        assert_eq!(part.texture.as_ref().unwrap().scale, Texture::DEFAULT_SCALE);
        assert_eq!(a.bodies[0].1.transparency, 0.0);
        assert_eq!(a.textures().len(), 1);
    }
}
