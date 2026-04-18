use swc_core::common::{sync::Lrc, FileName, Globals, Mark, SourceMap, GLOBALS};
use swc_core::ecma::codegen::to_code_default;
use swc_core::ecma::parser::{lexer::Lexer, EsSyntax, Parser, StringInput, Syntax, TsSyntax};
use swc_core::ecma::transforms::module::common_js::common_js;
use swc_core::ecma::transforms::module::util::Config;
use swc_core::ecma::transforms::typescript::strip;

fn main() {
    let code = "import { x } from './y'; export const z = x;";
    let cm: Lrc<SourceMap> = Default::default();
    let fm = cm.new_source_file(FileName::Custom("test.ts".into()).into(), code.to_string());
    let syntax = Syntax::Typescript(TsSyntax::default());
    let lexer = Lexer::new(syntax, Default::default(), StringInput::from(&*fm), None);
    let mut parser = Parser::new_from(lexer);
    let mut program = parser.parse_program().unwrap();

    let globals = Globals::default();
    GLOBALS.set(&globals, || {
        let unresolved_mark = Mark::new();
        let top_level_mark = Mark::new();
        program = program.apply(strip(unresolved_mark, top_level_mark));
        program = program.apply(common_js(unresolved_mark, Config::default(), Default::default(), None));
        let code = to_code_default(cm.clone(), None, &program);
        println!("Output:\n{}", code);
    });
}
