//! Annotation structures and `annotations_directory_item` parsing.
//!
//! Parsing is complete enough that a well-formed (or even mildly malformed)
//! annotations section never causes a top-level parse failure by itself; the
//! directory and sets are decoded eagerly and stored.

use crate::encoded::{decode_annotation, AnnotationElement, EncodedAnnotation};
use crate::error::DexResult;
use crate::reader::Reader;

/// A single decoded annotation: visibility plus the encoded annotation body.
#[derive(Clone, Debug, PartialEq)]
pub struct Annotation {
    pub visibility: u8,
    pub annotation: EncodedAnnotation,
}

impl Annotation {
    pub fn type_idx(&self) -> u32 {
        self.annotation.type_idx
    }

    pub fn elements(&self) -> &[AnnotationElement] {
        &self.annotation.elements
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct AnnotationsDirectory {
    pub class_annotations: Vec<Annotation>,
    pub field_annotations: Vec<(u32, Vec<Annotation>)>,
    pub method_annotations: Vec<(u32, Vec<Annotation>)>,
    /// `(method_idx, per-parameter annotation sets)`
    pub parameter_annotations: Vec<(u32, Vec<Vec<Annotation>>)>,
}

impl AnnotationsDirectory {
    /// Parse an `annotation_set_item` at `off` into a list of annotations.
    pub fn read_set(r: &Reader, off: u32) -> DexResult<Vec<Annotation>> {
        // A zero offset means "no annotations" (e.g. an unannotated parameter
        // in an `annotation_set_ref_list`).
        if off == 0 {
            return Ok(Vec::new());
        }
        let off = off as usize;
        let size = r.u32(off)? as usize;
        let mut out = Vec::with_capacity(size.min(4096));
        for i in 0..size {
            let ann_off = r.u32(off + 4 + i * 4)? as usize;
            let visibility = r.u8(ann_off)?;
            let (annotation, _) = decode_annotation(r, ann_off + 1, 0)?;
            out.push(Annotation {
                visibility,
                annotation,
            });
        }
        Ok(out)
    }

    /// Parse an `annotation_set_ref_list` at `off` into per-parameter sets.
    pub fn read_set_ref_list(r: &Reader, off: u32) -> DexResult<Vec<Vec<Annotation>>> {
        let off = off as usize;
        let size = r.u32(off)? as usize;
        let mut out = Vec::with_capacity(size.min(4096));
        for i in 0..size {
            let set_off = r.u32(off + 4 + i * 4)?;
            out.push(Self::read_set(r, set_off)?);
        }
        Ok(out)
    }
}

/// Parse an `annotations_directory_item` at `off`.
pub fn parse_directory(r: &Reader, off: u32) -> DexResult<AnnotationsDirectory> {
    let off = off as usize;
    let class_annotations_off = r.u32(off)?;
    let fields_size = r.u32(off + 4)? as usize;
    let methods_size = r.u32(off + 8)? as usize;
    let parameters_size = r.u32(off + 12)? as usize;

    let mut cursor = off + 16;
    let mut dir = AnnotationsDirectory::default();

    if class_annotations_off != 0 {
        dir.class_annotations = AnnotationsDirectory::read_set(r, class_annotations_off)?;
    }

    for _ in 0..fields_size {
        let field_idx = r.u32(cursor)?;
        let set_off = r.u32(cursor + 4)?;
        cursor += 8;
        dir.field_annotations
            .push((field_idx, AnnotationsDirectory::read_set(r, set_off)?));
    }
    for _ in 0..methods_size {
        let method_idx = r.u32(cursor)?;
        let set_off = r.u32(cursor + 4)?;
        cursor += 8;
        dir.method_annotations
            .push((method_idx, AnnotationsDirectory::read_set(r, set_off)?));
    }
    for _ in 0..parameters_size {
        let method_idx = r.u32(cursor)?;
        let list_off = r.u32(cursor + 4)?;
        cursor += 8;
        dir.parameter_annotations
            .push((method_idx, AnnotationsDirectory::read_set_ref_list(r, list_off)?));
    }

    Ok(dir)
}
