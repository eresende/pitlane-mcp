use crate::indexer::bash::BashParser;
use crate::indexer::c::CParser;
use crate::indexer::cpp::CppParser;
use crate::indexer::csharp::CSharpParser;
use crate::indexer::go::GoParser;
use crate::indexer::java::JavaParser;
use crate::indexer::javascript::JavaScriptParser;
use crate::indexer::language::LanguageParser;
use crate::indexer::python::PythonParser;
use crate::indexer::ruby::RubyParser;
use crate::indexer::rust::RustParser;
use crate::indexer::swift::SwiftParser;
use crate::indexer::typescript::TypeScriptParser;

pub fn build_default_registry() -> Vec<Box<dyn LanguageParser>> {
    vec![
        Box::new(RustParser),
        Box::new(PythonParser),
        Box::new(JavaScriptParser),
        Box::new(TypeScriptParser),
        Box::new(CParser),
        Box::new(CppParser),
        Box::new(GoParser),
        Box::new(JavaParser),
        Box::new(BashParser),
        Box::new(CSharpParser),
        Box::new(RubyParser),
        Box::new(SwiftParser),
    ]
}
