use swc_core::common::{sync::Lrc, FileName, Globals, Mark, SourceMap, GLOBALS};
use swc_core::ecma::codegen::to_code_default;
use swc_core::ecma::parser::{lexer::Lexer, EsSyntax, Parser, StringInput, Syntax, TsSyntax};
use swc_core::ecma::transforms::react::{react, Options as ReactOptions, Runtime as ReactRuntime};
use swc_core::ecma::transforms::typescript::strip;

fn parse_program(
    code: &str,
    filename: &str,
) -> anyhow::Result<(
    Lrc<SourceMap>,
    swc_core::ecma::ast::Program,
    bool, // is_ts
    bool, // is_tsx
    bool, // is_jsx
)> {
    let cm: Lrc<SourceMap> = Default::default();
    let fm = cm.new_source_file(
        FileName::Custom(filename.to_string()).into(),
        code.to_string(),
    );

    let is_tsx = filename.ends_with(".tsx");
    let is_ts = filename.ends_with(".ts") || is_tsx;
    let is_jsx = filename.ends_with(".jsx");

    let syntax = if is_ts {
        Syntax::Typescript(TsSyntax {
            tsx: is_tsx,
            decorators: true,
            ..Default::default()
        })
    } else {
        Syntax::Es(EsSyntax {
            jsx: is_jsx,
            ..Default::default()
        })
    };

    let lexer = Lexer::new(syntax, Default::default(), StringInput::from(&*fm), None);
    let mut parser = Parser::new_from(lexer);
    let program = parser
        .parse_program()
        .map_err(|e| anyhow::anyhow!("Parse error in {filename}: {e:?}"))?;

    if let Some(e) = parser.take_errors().into_iter().next() {
        return Err(anyhow::anyhow!("Parse error in {filename}: {e:?}"));
    }

    Ok((cm, program, is_ts, is_tsx, is_jsx))
}

/// Strip TypeScript/JSX syntax only. **Preserves ESM `import`/`export`.**
///
/// Use this for native ESM files. The output is valid ESM JavaScript that
/// QuickJS can evaluate with `JS_EVAL_TYPE_MODULE`.
pub fn strip_types_only(code: &str, filename: &str) -> anyhow::Result<String> {
    let (cm, program, is_ts, is_tsx, is_jsx) = parse_program(code, filename)?;

    let globals = Globals::default();
    GLOBALS.set(&globals, || -> anyhow::Result<String> {
        let unresolved_mark = Mark::new();
        let top_level_mark = Mark::new();
        let mut program = program;

        if is_ts {
            program = program.apply(strip(unresolved_mark, top_level_mark));
        }
        if is_tsx || is_jsx {
            let react_opts = ReactOptions {
                runtime: Some(ReactRuntime::Classic),
                development: Some(false),
                ..Default::default()
            };
            program = program.apply(react(
                cm.clone(),
                None::<swc_core::common::comments::SingleThreadedComments>,
                react_opts,
                top_level_mark,
                unresolved_mark,
            ));
        }

        Ok(to_code_default(cm.clone(), None, &program))
    })
}

/// Full transpile: strip TypeScript/JSX **and** convert ESM `import`/`export` → CommonJS.
///
/// Use this for CJS files (`.cjs`, `"type": "commonjs"`) that use TS/JSX syntax.
pub fn transpile_ts(code: &str, filename: &str) -> anyhow::Result<String> {
    let (cm, program, is_ts, is_tsx, is_jsx) = parse_program(code, filename)?;

    let globals = Globals::default();
    GLOBALS.set(&globals, || -> anyhow::Result<String> {
        let unresolved_mark = Mark::new();
        let top_level_mark = Mark::new();
        let mut program = program;

        if is_ts {
            program = program.apply(strip(unresolved_mark, top_level_mark));
        }
        if is_tsx || is_jsx {
            let react_opts = ReactOptions {
                runtime: Some(ReactRuntime::Classic),
                development: Some(false),
                ..Default::default()
            };
            program = program.apply(react(
                cm.clone(),
                None::<swc_core::common::comments::SingleThreadedComments>,
                react_opts,
                top_level_mark,
                unresolved_mark,
            ));
        }

        use swc_core::ecma::transforms::base::fixer::fixer;
        use swc_core::ecma::transforms::module::common_js::common_js;
        use swc_core::ecma::transforms::module::common_js::FeatureFlag;
        use swc_core::ecma::transforms::module::path::Resolver;
        use swc_core::ecma::transforms::module::util::Config;

        program = program.apply(common_js(
            Resolver::Default,
            unresolved_mark,
            Config::default(),
            FeatureFlag::default(),
        ));

        program = program.apply(fixer(
            None as Option<&dyn swc_core::common::comments::Comments>,
        ));

        Ok(to_code_default(cm.clone(), None, &program))
    })
}
