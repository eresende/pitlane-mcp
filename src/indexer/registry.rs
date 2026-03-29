use crate::indexer::c::CParser;
use crate::indexer::cpp::CppParser;
use crate::indexer::javascript::JavaScriptParser;
use crate::indexer::language::LanguageParser;
use crate::indexer::python::PythonParser;
use crate::indexer::rust::RustParser;
use crate::indexer::typescript::TypeScriptParser;

pub fn build_default_registry() -> Vec<Box<dyn LanguageParser>> {
    vec![
        Box::new(RustParser),
        Box::new(PythonParser),
        Box::new(JavaScriptParser),
        Box::new(TypeScriptParser),
        Box::new(CParser),
        Box::new(CppParser),
    ]
}
