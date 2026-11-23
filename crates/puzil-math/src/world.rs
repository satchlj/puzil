//! A minimal in-memory typst World: the NewCM faces, the vendored mitex specs, one main file.

use std::collections::HashMap;

use typst::{
    Library, LibraryExt, World,
    diag::{FileError, FileResult},
    foundations::{Bytes, Datetime},
    syntax::{FileId, RootedPath, Source, VirtualPath, VirtualRoot},
    text::{Font, FontBook},
    utils::LazyHash,
};

/// mitex's Typst scope (vendored, patched for typst 0.15).
const SPECS: &[(&str, &str)] = &[
    ("/specs/mod.typ", include_str!("../vendor/mitex/specs/mod.typ")),
    ("/specs/prelude.typ", include_str!("../vendor/mitex/specs/prelude.typ")),
    ("/specs/latex/standard.typ", include_str!("../vendor/mitex/specs/latex/standard.typ")),
];

pub struct Base {
    library: LazyHash<Library>,
    book: LazyHash<FontBook>,
    fonts: Vec<Font>,
    files: HashMap<FileId, Source>,
}

pub fn file_id(path: &str) -> FileId {
    RootedPath::new(VirtualRoot::Project, VirtualPath::new(path).unwrap()).intern()
}

impl Base {
    pub fn new() -> Self {
        // Only New Computer Modern (math and text) is needed; the rest of typst-assets
        // stays unloaded.
        let fonts: Vec<Font> = typst_assets::fonts()
            .flat_map(|data| Font::iter(Bytes::new(data)))
            .filter(|f| f.info().family.starts_with("New Computer Modern"))
            .collect();
        let book = FontBook::from_fonts(&fonts);
        let files = SPECS
            .iter()
            .map(|(p, text)| {
                let id = file_id(p);
                (id, Source::new(id, text.to_string()))
            })
            .collect();
        Self { library: LazyHash::new(Library::default()), book: LazyHash::new(book), fonts, files }
    }
}

pub struct MathWorld<'a> {
    pub base: &'a Base,
    pub main: Source,
}

impl World for MathWorld<'_> {
    fn library(&self) -> &LazyHash<Library> {
        &self.base.library
    }
    fn book(&self) -> &LazyHash<FontBook> {
        &self.base.book
    }
    fn main(&self) -> FileId {
        self.main.id()
    }
    fn source(&self, id: FileId) -> FileResult<Source> {
        if id == self.main.id() {
            return Ok(self.main.clone());
        }
        self.base.files.get(&id).cloned().ok_or_else(|| FileError::NotFound(id.vpath().get_without_slash().into()))
    }
    fn file(&self, id: FileId) -> FileResult<Bytes> {
        Ok(Bytes::new(self.source(id)?.text().as_bytes().to_vec()))
    }
    fn font(&self, index: usize) -> Option<Font> {
        self.base.fonts.get(index).cloned()
    }
    fn today(&self, _: Option<typst::foundations::Duration>) -> Option<Datetime> {
        None
    }
}
