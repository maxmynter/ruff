use std::iter::FusedIterator;

pub use module::{KnownModule, Module};
pub use path::SearchPathValidationError;
pub use resolver::SearchPaths;
pub(crate) use resolver::file_to_module;
pub use resolver::{resolve_module, resolve_real_module};
use ruff_db::system::SystemPath;

use crate::Db;
use crate::module_resolver::resolver::search_paths;
use resolver::SearchPathIterator;

mod module;
mod path;
mod resolver;
mod typeshed;

#[cfg(test)]
mod testing;

/// Returns an iterator over all search paths pointing to a system path
pub fn system_module_search_paths(db: &dyn Db) -> SystemModuleSearchPathsIter {
    SystemModuleSearchPathsIter {
        inner: search_paths(db),
    }
}

pub struct SystemModuleSearchPathsIter<'db> {
    inner: SearchPathIterator<'db>,
}

impl<'db> Iterator for SystemModuleSearchPathsIter<'db> {
    type Item = &'db SystemPath;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let next = self.inner.next()?;

            if let Some(system_path) = next.as_system_path() {
                return Some(system_path);
            }
        }
    }
}

impl FusedIterator for SystemModuleSearchPathsIter<'_> {}
