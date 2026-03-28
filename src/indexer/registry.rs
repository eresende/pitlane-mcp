use crate::indexer::language::LanguageParser;
use crate::indexer::python::PythonParser;
use crate::indexer::rust::RustParser;

pub fn build_default_registry() -> Vec<Box<dyn LanguageParser>> {
    vec![Box::new(RustParser), Box::new(PythonParser)]
}
