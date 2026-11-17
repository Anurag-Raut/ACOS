//! Font discovery and Android `fonts.xml` resolution.
//!
//! Android resolves a `Typeface` through `/system/etc/fonts.xml`: an ordered
//! family list (default `sans-serif` first), per-script fallback families,
//! aliases, per-weight/style entries and variable-font axes. ACOS parses the
//! real file so text measurement uses the same Roboto variable font and the
//! same fallback order the framework does.
//!
//! Font files are memory-mapped once and shared (`Arc<FontFile>`) across every
//! `RenderContext`/`Surface`; nothing is copied per session.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::error::{RenderError, RenderResult};
use crate::handle::Handle;

/// One memory-mapped font file (a single face, or one face of a `.ttc`).
pub struct FontFile {
    pub path: PathBuf,
    pub data: memmap2::Mmap,
    pub face_index: u32,
    /// Stable identity used in shaping-cache keys.
    pub id: u64,
}

impl std::fmt::Debug for FontFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FontFile")
            .field("path", &self.path)
            .field("face_index", &self.face_index)
            .finish()
    }
}

/// A `<font>` entry inside a family.
#[derive(Clone)]
pub struct FontEntry {
    pub file: Arc<FontFile>,
    pub weight: u16,
    pub italic: bool,
    pub axes: Vec<(String, f32)>,
    pub post_script_name: Option<String>,
}

/// A `<family>`.
#[derive(Clone)]
pub struct Family {
    pub name: Option<String>,
    pub lang: Option<String>,
    pub fallback_for: Option<String>,
    pub fonts: Vec<FontEntry>,
    /// Loaded lazily for coverage checks.
    pub coverage: std::sync::OnceLock<CoverageSet>,
    /// Android's `fonts.xml` `ignore="true"`: the family is loaded (its files
    /// are available for direct use, e.g. a colour/bitmap strike) but is never
    /// selected by name or as a fallback.
    pub ignored: bool,
}

/// A resolved font reference (family entry + coordinates).
#[derive(Clone)]
pub struct FontRef {
    pub file: Arc<FontFile>,
    pub weight: u16,
    pub italic: bool,
    pub axes: Vec<(String, f32)>,
    pub family_key: usize,
}

impl std::fmt::Debug for FontRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FontRef")
            .field("path", &self.file.path)
            .field("weight", &self.weight)
            .field("italic", &self.italic)
            .finish()
    }
}

/// A simple coverage bitmap over the BMP for fast glyph-presence checks.
#[derive(Clone)]
pub struct CoverageSet {
    bmp: Box<[u64; 8192]>, // 65536 bits
}

impl CoverageSet {
    pub fn from_face(face: &ttf_parser::Face) -> Self {
        let mut bmp = Box::new([0u64; 8192]);
        // Only BMP codepoints can be bit-indexed cheaply; supplement is rare
        // for coverage decisions (emoji), handled by a conservative `true`.
        for cp in 0u32..=0xFFFF {
            if let Some(ch) = char::from_u32(cp) {
                if face.glyph_index(ch).is_some() {
                    bmp[(cp as usize) / 64] |= 1u64 << (cp % 64);
                }
            }
        }
        CoverageSet { bmp }
    }

    pub fn contains(&self, cp: u32) -> Option<bool> {
        if cp > 0xFFFF {
            // Report "unknown" so the caller can fall back to a real lookup.
            return None;
        }
        Some(self.bmp[(cp as usize) / 64] & (1u64 << (cp % 64)) != 0)
    }
}

/// Process-wide shared font collection. Android mmaps `/system/fonts` once;
/// ACOS does the same so every `RenderContext`/surface shares one physical
/// copy instead of re-mapping the whole font set per session.
static SHARED_FONTS: std::sync::OnceLock<Option<Arc<FontCollection>>> = std::sync::OnceLock::new();

/// The loaded font configuration.
pub struct FontCollection {
    pub families: Vec<Family>,
    pub aliases: HashMap<String, (String, Option<u16>)>,
    /// Index of the default family (`sans-serif`).
    pub default_family: usize,
    pub source: PathBuf,
    next_id: u64,
}

impl FontCollection {
    /// Return the process-wide shared collection (loading it on first use).
    pub fn shared() -> Option<Arc<FontCollection>> {
        SHARED_FONTS
            .get_or_init(|| FontCollection::load().map(Arc::new))
            .clone()
    }

    /// Locate and load fonts from the environment.
    ///
    /// Search order: `$ACOS_FONT_DIR`, the extracted Android 34 system image
    /// (`.cache/boot/android-34/fonts`), then host font directories.
    pub fn load() -> Option<FontCollection> {
        let dirs = candidate_font_dirs();
        for dir in dirs {
            if let Some(fc) = Self::load_from_dir(&dir) {
                return Some(fc);
            }
        }
        None
    }

    pub fn load_from_dir(dir: &Path) -> Option<FontCollection> {
        let xml = dir.join("fonts.xml");
        if xml.exists() {
            if let Ok(fc) = Self::load_android(dir, &xml) {
                return Some(fc);
            }
        }
        Self::load_scanned(dir)
    }

    fn load_android(dir: &Path, xml: &Path) -> RenderResult<FontCollection> {
        let text = std::fs::read_to_string(xml).map_err(|e| RenderError::Io {
            msg: format!("reading {}: {e}", xml.display()),
        })?;
        let index = build_file_index(dir);
        let doc = roxmltree::Document::parse(&text).map_err(|e| RenderError::Decode {
            msg: format!("fonts.xml parse: {e}"),
        })?;
        let root = doc.root_element();
        let mut families: Vec<Family> = Vec::new();
        let mut aliases: HashMap<String, (String, Option<u16>)> = HashMap::new();
        let mut next_id: u64 = 1;
        for node in root.children().filter(|n| n.is_element()) {
            match node.tag_name().name() {
                "family" => {
                    // Android's `ignore="true"` marks a family that the
                    // platform loads but never selects (e.g. the CBDT
                    // `NotoColorEmojiLegacy.ttf` when a COLR font is present).
                    let ignored = node.attribute("ignore") == Some("true");
                    let name = node.attribute("name").map(|s| s.to_string());
                    let lang = node.attribute("lang").map(|s| s.to_string());
                    let fallback_for = node.attribute("fallbackFor").map(|s| s.to_string());
                    let mut fonts = Vec::new();
                    for f in node
                        .children()
                        .filter(|n| n.is_element() && n.tag_name().name() == "font")
                    {
                        let weight = f
                            .attribute("weight")
                            .and_then(|s| s.parse::<u16>().ok())
                            .unwrap_or(400);
                        let style = f.attribute("style").unwrap_or("normal");
                        let italic = style == "italic";
                        let post_script_name = f.attribute("postScriptName").map(|s| s.to_string());
                        let filename = f
                            .children()
                            .find(|n| n.is_text())
                            .and_then(|t| t.text())
                            .map(|s| s.trim().to_string())
                            .filter(|s| !s.is_empty());
                        let Some(filename) = filename else { continue };
                        let Some(path) = index.get(&filename) else {
                            // Missing font file: skip rather than abort the
                            // whole config (the extracted image may be partial).
                            continue;
                        };
                        let mut axes = Vec::new();
                        for a in f
                            .children()
                            .filter(|n| n.is_element() && n.tag_name().name() == "axis")
                        {
                            if let (Some(tag), Some(v)) = (
                                a.attribute("tag"),
                                a.attribute("stylevalue")
                                    .and_then(|s| s.parse::<f32>().ok()),
                            ) {
                                axes.push((tag.to_string(), v));
                            }
                        }
                        if let Some(file) =
                            map_font(path, post_script_name.as_deref(), &mut next_id)
                        {
                            fonts.push(FontEntry {
                                file,
                                weight,
                                italic,
                                axes,
                                post_script_name,
                            });
                        }
                    }
                    if !fonts.is_empty() {
                        families.push(Family {
                            name,
                            lang,
                            fallback_for,
                            fonts,
                            coverage: std::sync::OnceLock::new(),
                            ignored,
                        });
                    }
                }
                "alias" => {
                    if let (Some(name), Some(to)) = (node.attribute("name"), node.attribute("to")) {
                        let weight = node.attribute("weight").and_then(|s| s.parse::<u16>().ok());
                        aliases.insert(name.to_string(), (to.to_string(), weight));
                    }
                }
                _ => {}
            }
        }
        let default_family = families
            .iter()
            .position(|f| !f.ignored && f.name.as_deref() == Some("sans-serif"))
            .or_else(|| families.iter().position(|f| !f.ignored && f.name.is_some()))
            .or_else(|| families.iter().position(|f| !f.ignored))
            .unwrap_or(0);
        Ok(FontCollection {
            families,
            aliases,
            default_family,
            source: xml.to_path_buf(),
            next_id,
        })
    }

    /// Fallback: scan a directory tree for font files and synthesise families
    /// from the font name tables (host fonts on a developer machine).
    fn load_scanned(dir: &Path) -> Option<FontCollection> {
        let mut files = Vec::new();
        collect_font_files(dir, &mut files, 0)?;
        if files.is_empty() {
            return None;
        }
        let mut by_family: HashMap<String, Vec<(PathBuf, u16, bool)>> = HashMap::new();
        for path in files {
            let Ok(file) = std::fs::File::open(&path) else {
                continue;
            };
            let Ok(data) = (unsafe { memmap2::Mmap::map(&file) }) else {
                continue;
            };
            let Ok(face) = ttf_parser::Face::parse(&data, 0) else {
                continue;
            };
            let family = face_family_name(&face).unwrap_or_else(|| {
                path.file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("unknown")
                    .to_string()
            });
            let weight = face.weight().to_number();
            let italic = face.is_italic() || face.is_oblique();
            by_family
                .entry(family)
                .or_default()
                .push((path, weight, italic));
        }
        if by_family.is_empty() {
            return None;
        }
        let mut next_id = 1u64;
        let mut families = Vec::new();
        // Stable order: the first family found becomes the default.
        let mut names: Vec<String> = by_family.keys().cloned().collect();
        names.sort();
        for name in names {
            let entries = by_family.remove(&name).unwrap();
            let mut fonts = Vec::new();
            for (path, weight, italic) in entries {
                if let Some(file) = map_font(&path, None, &mut next_id) {
                    fonts.push(FontEntry {
                        file,
                        weight,
                        italic,
                        axes: Vec::new(),
                        post_script_name: None,
                    });
                }
            }
            families.push(Family {
                name: Some(name),
                lang: None,
                fallback_for: None,
                fonts,
                coverage: std::sync::OnceLock::new(),
                ignored: false,
            });
        }
        Some(FontCollection {
            default_family: 0,
            families,
            aliases: HashMap::new(),
            source: dir.to_path_buf(),
            next_id,
        })
    }

    /// Resolve a typeface, Android-style: alias, then best weight/style entry.
    pub fn resolve_typeface(
        &self,
        family: Option<&str>,
        weight: u16,
        italic: bool,
    ) -> Option<FontRef> {
        let (family_name, alias_weight) = match family {
            Some(name) => {
                if let Some((to, w)) = self.aliases.get(name) {
                    (to.clone(), w.unwrap_or(weight))
                } else {
                    (name.to_string(), weight)
                }
            }
            None => ("sans-serif".to_string(), weight),
        };
        let idx = self.family_index(&family_name)?;
        let entry = pick_entry(&self.families[idx].fonts, alias_weight, italic)?;
        Some(FontRef {
            file: entry.file.clone(),
            weight: entry.weight,
            italic: entry.italic,
            axes: entry.axes.clone(),
            family_key: idx,
        })
    }

    pub fn resolve_weighted(
        &self,
        family: Option<&str>,
        weight: u16,
        italic: bool,
    ) -> Option<FontRef> {
        self.resolve_typeface(family, weight, italic)
    }

    fn family_index(&self, name: &str) -> Option<usize> {
        if let Some((to, _)) = self.aliases.get(name) {
            return self.family_index(to);
        }
        self.families
            .iter()
            .position(|f| !f.ignored && f.name.as_deref() == Some(name))
            .or(Some(self.default_family))
    }

    /// Ordered family indices to try for glyph fallback. The requested family
    /// comes first, then all remaining families in document order (Android's
    /// fallback list).
    pub fn fallback_chain(&self, family: Option<&str>) -> Vec<usize> {
        let mut chain = Vec::with_capacity(self.families.len());
        let primary = family.and_then(|f| self.family_index(f));
        if let Some(p) = primary {
            chain.push(p);
        } else {
            chain.push(self.default_family);
        }
        for i in 0..self.families.len() {
            if !chain.contains(&i) && !self.families[i].ignored {
                chain.push(i);
            }
        }
        chain
    }

    /// Find a font that has a glyph for `cp`, walking the fallback chain.
    pub fn font_for_char(
        &self,
        chain: &[usize],
        cp: u32,
        weight: u16,
        italic: bool,
    ) -> Option<FontRef> {
        let ch = char::from_u32(cp)?;
        for &idx in chain {
            let family = &self.families[idx];
            if let Some(entry) = find_entry_with_glyph(family, ch, weight, italic) {
                return Some(FontRef {
                    file: entry.file.clone(),
                    weight: entry.weight,
                    italic: entry.italic,
                    axes: entry.axes.clone(),
                    family_key: idx,
                });
            }
        }
        None
    }

    /// A stable key for a family+weight+style combination.
    pub fn family_has_glyph(&self, family: &Family, ch: char) -> bool {
        let cov = family
            .coverage
            .get_or_init(|| CoverageSet::from_face(&parse_face(&family.fonts[0].file, 0)));
        cov.contains(ch as u32).unwrap_or(false)
    }

    pub fn family_count(&self) -> usize {
        self.families.len()
    }

    pub fn next_font_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    /// Materialise a `Typeface` handle resolved through this collection. The
    /// caller owns the handle table.
    pub fn resolve_to_handle(
        &self,
        family: Option<&str>,
        weight: u16,
        italic: bool,
        table: &mut crate::handle::HandleTable<FontRef>,
    ) -> Option<Handle> {
        Some(table.insert(self.resolve_typeface(family, weight, italic)?))
    }
}

fn parse_face(file: &FontFile, index: u32) -> ttf_parser::Face<'_> {
    ttf_parser::Face::parse(&file.data, index).expect("font already validated")
}

fn pick_entry(fonts: &[FontEntry], weight: u16, italic: bool) -> Option<&FontEntry> {
    fonts
        .iter()
        .min_by_key(|e| entry_cost(e, weight, italic))
        .or_else(|| fonts.first())
}

fn entry_cost(e: &FontEntry, weight: u16, italic: bool) -> u32 {
    let w = (e.weight as i32 - weight as i32).unsigned_abs();
    let style_penalty = if e.italic == italic { 0 } else { 10_000 };
    w + style_penalty
}

fn find_entry_with_glyph(
    family: &Family,
    ch: char,
    weight: u16,
    italic: bool,
) -> Option<&FontEntry> {
    // Prefer the best weight/style entry that has the glyph.
    let mut candidates: Vec<&FontEntry> = family.fonts.iter().collect();
    candidates.sort_by_key(|e| entry_cost(e, weight, italic));
    for e in candidates {
        let face = parse_face(&e.file, e.file.face_index);
        if face.glyph_index(ch).is_some() {
            return Some(e);
        }
    }
    None
}

/// Build `filename -> path` for a font directory (recursive).
fn build_file_index(dir: &Path) -> HashMap<String, PathBuf> {
    let mut map = HashMap::new();
    index_dir(dir, &mut map, 0);
    map
}

fn index_dir(dir: &Path, map: &mut HashMap<String, PathBuf>, depth: usize) {
    if depth > 4 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            index_dir(&p, map, depth + 1);
        } else if let Some(name) = p.file_name().and_then(|s| s.to_str()) {
            map.entry(name.to_string()).or_insert_with(|| p.clone());
        }
    }
}

fn collect_font_files(dir: &Path, out: &mut Vec<PathBuf>, depth: usize) -> Option<()> {
    if depth > 5 {
        return Some(());
    }
    let entries = std::fs::read_dir(dir).ok()?;
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect_font_files(&p, out, depth + 1);
        } else if let Some(ext) = p.extension().and_then(|s| s.to_str()) {
            let ext = ext.to_ascii_lowercase();
            if matches!(ext.as_str(), "ttf" | "otf" | "ttc" | "otc") {
                out.push(p);
            }
        }
    }
    Some(())
}

fn map_font(
    path: &Path,
    post_script_name: Option<&str>,
    next_id: &mut u64,
) -> Option<Arc<FontFile>> {
    let file = std::fs::File::open(path).ok()?;
    // SAFETY: the file is opened read-only; the mapping is never written and
    // is only accessed through immutable slices (`&[u8]`). `Mmap` is `Sync`,
    // so the mapping is safely shared read-only across contexts/surfaces.
    // This is the one `unsafe` block in acos-render and it exists solely to
    // mmap fonts (Android mmaps /system/fonts for the same reason: share one
    // physical copy of a multi-hundred-MB font set across every session).
    let data = unsafe { memmap2::Mmap::map(&file) }.ok()?;
    let face_index = match post_script_name {
        Some(ps) => find_face_by_ps_name(&data, ps).unwrap_or(0),
        None => 0,
    };
    let id = *next_id;
    *next_id += 1;
    Some(Arc::new(FontFile {
        path: path.to_path_buf(),
        data,
        face_index,
        id,
    }))
}

fn find_face_by_ps_name(data: &[u8], ps: &str) -> Option<u32> {
    // TrueType collections expose a `ttcf` header with a face count.
    if data.len() >= 12 && &data[0..4] == b"ttcf" {
        let num = u32::from_be_bytes([data[8], data[9], data[10], data[11]]);
        for i in 0..num.min(64) {
            if let Ok(face) = ttf_parser::Face::parse(data, i) {
                if post_script_name_of(&face).as_deref() == Some(ps) {
                    return Some(i);
                }
            }
        }
    }
    if let Ok(face) = ttf_parser::Face::parse(data, 0) {
        if post_script_name_of(&face).as_deref() == Some(ps) {
            return Some(0);
        }
    }
    None
}

fn post_script_name_of(face: &ttf_parser::Face) -> Option<String> {
    let names = face.names();
    // Name ID 6 is the PostScript name.
    for i in 0..names.len() {
        if let Some(name) = names.get(i) {
            if name.name_id == 6 {
                if let Some(s) = name.to_string() {
                    return Some(s);
                }
            }
        }
    }
    None
}

fn face_family_name(face: &ttf_parser::Face) -> Option<String> {
    let names = face.names();
    // Prefer typographic family (16), then family (1).
    for want in [16u16, 1u16] {
        for i in 0..names.len() {
            if let Some(name) = names.get(i) {
                if name.name_id == want {
                    if let Some(s) = name.to_string() {
                        return Some(s);
                    }
                }
            }
        }
    }
    None
}

/// Candidate font directories, in priority order.
pub fn candidate_font_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(dir) = std::env::var("ACOS_FONT_DIR") {
        if !dir.is_empty() {
            dirs.push(PathBuf::from(dir));
        }
    }
    // `ACOS_BOOT_DIR` is the runtime's extracted Android system image; its
    // `fonts/` is the authoritative font set. Resolving it here (before the
    // host directories) keeps text metrics identical between a confined
    // `--native` launch and a normal one, and stops a host font set such as
    // `/usr/share/fonts` from silently standing in for the Android fonts — the
    // layout/bounds drift W191-A traced to exactly that substitution.
    if let Some(boot) = std::env::var_os("ACOS_BOOT_DIR").map(PathBuf::from) {
        let p = boot.join("fonts");
        if p.is_dir() {
            dirs.push(p);
        }
    }
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for rel in [
        "../../.cache/boot/android-34/fonts",
        ".cache/boot/android-34/fonts",
    ] {
        let p = manifest.join(rel);
        if p.exists() {
            dirs.push(p);
        }
    }
    for dir in [
        "/system/fonts",
        "/usr/share/fonts",
        "/usr/local/share/fonts",
        "/Library/Fonts",
        "/System/Library/Fonts",
    ] {
        let p = PathBuf::from(dir);
        if p.exists() {
            dirs.push(p);
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        for rel in [".fonts", ".local/share/fonts"] {
            let p = PathBuf::from(&home).join(rel);
            if p.exists() {
                dirs.push(p);
            }
        }
    }
    dirs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_real_fonts_xml_if_available() {
        let dirs = candidate_font_dirs();
        let Some(dir) = dirs.iter().find(|d| d.join("fonts.xml").exists()) else {
            eprintln!("SKIP: no Android fonts.xml found (set ACOS_FONT_DIR)");
            return;
        };
        let fc = FontCollection::load_from_dir(dir).expect("load fonts");
        assert!(
            fc.family_count() > 1,
            "family count = {}",
            fc.family_count()
        );
        let roboto = fc.resolve_typeface(None, 400, false).expect("roboto");
        assert!(
            roboto.file.path.to_string_lossy().contains("Roboto")
                || roboto.family_key == fc.default_family
        );
        // A CJK fallback should be found when the Noto CJK font is present.
        let chain = fc.fallback_chain(Some("sans-serif"));
        if let Some(f) = fc.font_for_char(&chain, '漢' as u32, 400, false) {
            assert!(
                f.file.path.to_string_lossy().to_lowercase().contains("cjk")
                    || f.file
                        .path
                        .to_string_lossy()
                        .to_lowercase()
                        .contains("noto")
            );
        } else {
            eprintln!("SKIP: no CJK fallback font available");
        }
    }

    #[test]
    fn resolve_weight_picks_bold_entry() {
        let dirs = candidate_font_dirs();
        let Some(dir) = dirs.iter().find(|d| d.join("fonts.xml").exists()) else {
            eprintln!("SKIP: no Android fonts.xml found");
            return;
        };
        let fc = FontCollection::load_from_dir(dir).unwrap();
        let bold = fc.resolve_typeface(None, 700, false).unwrap();
        assert_eq!(bold.weight, 700);
    }
}
