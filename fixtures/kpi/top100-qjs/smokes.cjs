"use strict";
function run(pkg) {
  switch (pkg) {
    case 'path-parse': {
      const pathParse = require("path-parse");
      const r = pathParse("/a/b/c.js");
      return JSON.stringify({ pkg, ok: r.dir === "/a/b" && r.ext === ".js" });
      break;
    }
    case 'tslib': {
      const m = require(pkg);
      return JSON.stringify({ pkg, ok: true, t: typeof m });
      break;
    }
    case 'axios': {
      const ax = require("axios");
      return JSON.stringify({ pkg, ok: typeof ax.create === "function" });
      break;
    }
    case 'react': {
      const R = require("react");
      return JSON.stringify({ pkg, ok: typeof R.createElement === "function" });
      break;
    }
    case 'chalk': {
      const m = require(pkg);
      return JSON.stringify({ pkg, ok: true, t: typeof m });
      break;
    }
    case 'commander': {
      const m = require(pkg);
      return JSON.stringify({ pkg, ok: true, t: typeof m });
      break;
    }
    case 'semver': {
      const semver = require("semver");
      return JSON.stringify({ pkg, ok: semver.valid("1.0.0") === "1.0.0" });
      break;
    }
    case 'uuid': {
      const m = require(pkg);
      return JSON.stringify({ pkg, ok: true, t: typeof m });
      break;
    }
    case 'glob': {
      const m = require(pkg);
      return JSON.stringify({ pkg, ok: true, t: typeof m });
      break;
    }
    case 'debug': {
      const m = require(pkg);
      return JSON.stringify({ pkg, ok: true, t: typeof m });
      break;
    }
    case 'rxjs': {
      const m = require(pkg);
      return JSON.stringify({ pkg, ok: true, t: typeof m });
      break;
    }
    case 'typescript': {
      const ts = require("typescript");
      const o = ts.transpileModule("const x:number=1", { compilerOptions: { module: ts.ModuleKind.CommonJS } });
      return JSON.stringify({ pkg, ok: o.outputText.includes("x") });
      break;
    }
    case '@types/node': {
      return JSON.stringify({ pkg, ok: true, check: "types", has: require("fs").existsSync(require.resolve("@types/node/package.json")) });
      break;
    }
    case 'acorn': {
      const acorn = require("acorn");
      const ast = acorn.parse("1+1", { ecmaVersion: 2022 });
      return JSON.stringify({ pkg, ok: ast.type === "Program" });
      break;
    }
    case 'jsesc': {
      const j = require("jsesc");
      return JSON.stringify({ pkg, ok: j("a") === "a" });
      break;
    }
    case 'magic-string': {
      const MagicString = require("magic-string");
      const s = new MagicString("abc");
      s.overwrite(0,1,"z");
      return JSON.stringify({ pkg, ok: s.toString() === "zbc" });
      break;
    }
    case '@babel/core': {
      const babel = require("@babel/core");
      const out = babel.transformSync("1+1", { ast: false, code: true, configFile: false, babelrc: false, compact: false });
      return JSON.stringify({ pkg, ok: !!(out && out.code && !out.code.includes("1+1")) });
      break;
    }
    case 'core-js': {
      const m = require(pkg);
      return JSON.stringify({ pkg, ok: true, t: typeof m });
      break;
    }
    case 'react-dom': {
      const d = require("react-dom");
      return JSON.stringify({
        pkg,
        ok: typeof d.createPortal === "function" && typeof d.version === "string",
      });
      break;
    }
    case 'vue': {
      const vue = require("vue");
      return JSON.stringify({ pkg, ok: typeof vue === "object" || typeof vue === "function" });
      break;
    }
    case 'dlv': {
      const dlv = require("dlv");
      return JSON.stringify({ pkg, ok: dlv({ a: { b: 2 } }, "a.b") === 2 });
      break;
    }
    case 'express': {
      require("express");
      return JSON.stringify({ pkg, ok: true });
      break;
    }
    case 'deepmerge': {
      const m = require("deepmerge");
      const r = m({ a: 1 }, { b: 2 });
      return JSON.stringify({ pkg, ok: r.a === 1 && r.b === 2 });
      break;
    }
    case 'dequal': {
      const { dequal } = require("dequal");
      return JSON.stringify({ pkg, ok: dequal([1], [1]) });
      break;
    }
    case 'klona': {
      const { klona } = require("klona");
      const o = { a: 1 };
      const c = klona(o);
      c.a = 2;
      return JSON.stringify({ pkg, ok: o.a === 1 && c.a === 2 });
      break;
    }
    case 'rfdc': {
      const rfdc = require("rfdc");
      const clone = rfdc();
      const o = { a: 1 };
      const c = clone(o);
      c.a = 2;
      return JSON.stringify({ pkg, ok: o.a === 1 && c.a === 2 });
      break;
    }
    case 'tailwindcss': {
      const t = require("tailwindcss");
      return JSON.stringify({ pkg, ok: typeof t === "function" });
      break;
    }
    case 'postcss': {
      const p = require("postcss");
      const r = p.parse("a{color:red}");
      return JSON.stringify({ pkg, ok: r.type === "root" });
      break;
    }
    case 'zod': {
      const z = require("zod");
      return JSON.stringify({ pkg, ok: z.string().parse("a") === "a" });
      break;
    }
    case 'dotenv': {
      const m = require(pkg);
      return JSON.stringify({ pkg, ok: true, t: typeof m });
      break;
    }
    case 'fs-extra': {
      const m = require(pkg);
      return JSON.stringify({ pkg, ok: true, t: typeof m });
      break;
    }
    case 'rimraf': {
      const m = require(pkg);
      return JSON.stringify({ pkg, ok: true, t: typeof m });
      break;
    }
    case 'mkdirp': {
      const m = require(pkg);
      return JSON.stringify({ pkg, ok: true, t: typeof m });
      break;
    }
    case 'yargs': {
      const m = require(pkg);
      return JSON.stringify({ pkg, ok: true, t: typeof m });
      break;
    }
    case 'minimatch': {
      const m = require(pkg);
      return JSON.stringify({ pkg, ok: true, t: typeof m });
      break;
    }
    case 'ws': {
      const WebSocket = require("ws");
      return JSON.stringify({ pkg, ok: typeof WebSocket === "function" });
      break;
    }
    case 'cookie': {
      const m = require(pkg);
      return JSON.stringify({ pkg, ok: true, t: typeof m });
      break;
    }
    case 'qs': {
      const m = require(pkg);
      return JSON.stringify({ pkg, ok: true, t: typeof m });
      break;
    }
    case 'body-parser': {
      const m = require(pkg);
      return JSON.stringify({ pkg, ok: true, t: typeof m });
      break;
    }
    case 'cors': {
      const m = require(pkg);
      return JSON.stringify({ pkg, ok: true, t: typeof m });
      break;
    }
    case 'helmet': {
      const m = require(pkg);
      return JSON.stringify({ pkg, ok: true, t: typeof m });
      break;
    }
    case 'morgan': {
      const m = require(pkg);
      return JSON.stringify({ pkg, ok: true, t: typeof m });
      break;
    }
    case 'compression': {
      const m = require(pkg);
      return JSON.stringify({ pkg, ok: true, t: typeof m });
      break;
    }
    case 'http-errors': {
      const m = require(pkg);
      return JSON.stringify({ pkg, ok: true, t: typeof m });
      break;
    }
    case 'statuses': {
      const m = require(pkg);
      return JSON.stringify({ pkg, ok: true, t: typeof m });
      break;
    }
    case 'send': {
      const m = require(pkg);
      return JSON.stringify({ pkg, ok: true, t: typeof m });
      break;
    }
    case 'serve-static': {
      const m = require(pkg);
      return JSON.stringify({ pkg, ok: true, t: typeof m });
      break;
    }
    case 'finalhandler': {
      const m = require(pkg);
      return JSON.stringify({ pkg, ok: true, t: typeof m });
      break;
    }
    case 'on-finished': {
      const m = require(pkg);
      return JSON.stringify({ pkg, ok: true, t: typeof m });
      break;
    }
    case 'raw-body': {
      const m = require(pkg);
      return JSON.stringify({ pkg, ok: true, t: typeof m });
      break;
    }
    case 'bytes': {
      const m = require(pkg);
      return JSON.stringify({ pkg, ok: true, t: typeof m });
      break;
    }
    case 'negotiator': {
      const m = require(pkg);
      return JSON.stringify({ pkg, ok: true, t: typeof m });
      break;
    }
    case 'accepts': {
      const m = require(pkg);
      return JSON.stringify({ pkg, ok: true, t: typeof m });
      break;
    }
    case 'content-type': {
      const m = require(pkg);
      return JSON.stringify({ pkg, ok: true, t: typeof m });
      break;
    }
    case 'vary': {
      const m = require(pkg);
      return JSON.stringify({ pkg, ok: true, t: typeof m });
      break;
    }
    case 'cookie-signature': {
      const m = require(pkg);
      return JSON.stringify({ pkg, ok: true, t: typeof m });
      break;
    }
    case 'forwarded': {
      const m = require(pkg);
      return JSON.stringify({ pkg, ok: true, t: typeof m });
      break;
    }
    case 'proxy-addr': {
      const m = require(pkg);
      return JSON.stringify({ pkg, ok: true, t: typeof m });
      break;
    }
    case 'path-to-regexp': {
      const m = require(pkg);
      return JSON.stringify({ pkg, ok: true, t: typeof m });
      break;
    }
    case 'fresh': {
      const m = require(pkg);
      return JSON.stringify({ pkg, ok: true, t: typeof m });
      break;
    }
    case 'etag': {
      const m = require(pkg);
      return JSON.stringify({ pkg, ok: true, t: typeof m });
      break;
    }
    case 'depd': {
      const m = require(pkg);
      return JSON.stringify({ pkg, ok: true, t: typeof m });
      break;
    }
    case 'destroy': {
      const m = require(pkg);
      return JSON.stringify({ pkg, ok: true, t: typeof m });
      break;
    }
    case 'parseurl': {
      const m = require(pkg);
      return JSON.stringify({ pkg, ok: true, t: typeof m });
      break;
    }
    case 'range-parser': {
      const m = require(pkg);
      return JSON.stringify({ pkg, ok: true, t: typeof m });
      break;
    }
    case 'type-is': {
      const m = require(pkg);
      return JSON.stringify({ pkg, ok: true, t: typeof m });
      break;
    }
    case 'iconv-lite': {
      const m = require(pkg);
      return JSON.stringify({ pkg, ok: true, t: typeof m });
      break;
    }
    case 'safer-buffer': {
      const m = require(pkg);
      return JSON.stringify({ pkg, ok: true, t: typeof m });
      break;
    }
    case 'inherits': {
      const m = require(pkg);
      return JSON.stringify({ pkg, ok: true, t: typeof m });
      break;
    }
    case 'readable-stream': {
      const m = require(pkg);
      return JSON.stringify({ pkg, ok: true, t: typeof m });
      break;
    }
    case 'string_decoder': {
      const m = require(pkg);
      return JSON.stringify({ pkg, ok: true, t: typeof m });
      break;
    }
    case 'safe-buffer': {
      const m = require(pkg);
      return JSON.stringify({ pkg, ok: true, t: typeof m });
      break;
    }
    case 'once': {
      const m = require(pkg);
      return JSON.stringify({ pkg, ok: true, t: typeof m });
      break;
    }
    case 'wrappy': {
      const m = require(pkg);
      return JSON.stringify({ pkg, ok: true, t: typeof m });
      break;
    }
    case 'balanced-match': {
      const m = require(pkg);
      return JSON.stringify({ pkg, ok: true, t: typeof m });
      break;
    }
    case 'brace-expansion': {
      const m = require(pkg);
      return JSON.stringify({ pkg, ok: true, t: typeof m });
      break;
    }
    case 'concat-map': {
      const m = require(pkg);
      return JSON.stringify({ pkg, ok: true, t: typeof m });
      break;
    }
    case 'path-is-absolute': {
      const m = require(pkg);
      return JSON.stringify({ pkg, ok: true, t: typeof m });
      break;
    }
    case 'inflight': {
      const m = require(pkg);
      return JSON.stringify({ pkg, ok: true, t: typeof m });
      break;
    }
    case 'fs.realpath': {
      const m = require(pkg);
      return JSON.stringify({ pkg, ok: true, t: typeof m });
      break;
    }
    case 'graceful-fs': {
      const m = require(pkg);
      return JSON.stringify({ pkg, ok: true, t: typeof m });
      break;
    }
    case 'universalify': {
      const m = require(pkg);
      return JSON.stringify({ pkg, ok: true, t: typeof m });
      break;
    }
    case 'jsonfile': {
      const m = require(pkg);
      return JSON.stringify({ pkg, ok: true, t: typeof m });
      break;
    }
    case 'cross-spawn': {
      const m = require(pkg);
      return JSON.stringify({ pkg, ok: true, t: typeof m });
      break;
    }
    case 'which': {
      const m = require(pkg);
      return JSON.stringify({ pkg, ok: true, t: typeof m });
      break;
    }
    case 'isexe': {
      const m = require(pkg);
      return JSON.stringify({ pkg, ok: true, t: typeof m });
      break;
    }
    case 'shebang-command': {
      const m = require(pkg);
      return JSON.stringify({ pkg, ok: true, t: typeof m });
      break;
    }
    case 'path-key': {
      const m = require(pkg);
      return JSON.stringify({ pkg, ok: true, t: typeof m });
      break;
    }
    case '@reduxjs/toolkit': {
      const rt = require("@reduxjs/toolkit");
      return JSON.stringify({ pkg, ok: typeof rt.configureStore === "function" });
      break;
    }
    case 'redux': {
      const m = require(pkg);
      return JSON.stringify({ pkg, ok: true, t: typeof m });
      break;
    }
    case 'immer': {
      const m = require(pkg);
      return JSON.stringify({ pkg, ok: true, t: typeof m });
      break;
    }
    case 'nanoid': {
      const m = require(pkg);
      return JSON.stringify({ pkg, ok: true, t: typeof m });
      break;
    }
    case 'source-map-js': {
      const m = require(pkg);
      return JSON.stringify({ pkg, ok: true, t: typeof m });
      break;
    }
    case 'scheduler': {
      const m = require(pkg);
      return JSON.stringify({ pkg, ok: true, t: typeof m });
      break;
    }
    case 'prop-types': {
      const m = require(pkg);
      return JSON.stringify({ pkg, ok: true, t: typeof m });
      break;
    }
    case 'object-assign': {
      const m = require(pkg);
      return JSON.stringify({ pkg, ok: true, t: typeof m });
      break;
    }
    case 'loose-envify': {
      const m = require(pkg);
      return JSON.stringify({ pkg, ok: true, t: typeof m });
      break;
    }
    case 'js-tokens': {
      const m = require(pkg);
      return JSON.stringify({ pkg, ok: true, t: typeof m });
      break;
    }
    case 'hoist-non-react-statics': {
      const m = require(pkg);
      return JSON.stringify({ pkg, ok: true, t: typeof m });
      break;
    }
    case 'react-is': {
      const m = require(pkg);
      return JSON.stringify({ pkg, ok: true, t: typeof m });
      break;
    }
    default:
      throw new Error("unknown package: " + pkg);
  }
}
module.exports = { run };
