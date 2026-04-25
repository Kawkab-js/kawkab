"use strict";
function run(pkg) {
  switch (pkg) {
    case 'path-parse': {
      const pathParse = require("path-parse");
      const r = pathParse("/a/b/c.js");
      return JSON.stringify({ pkg, ok: r.dir === "/a/b" && r.ext === ".js" });
      break;
    }
    default:
      return JSON.stringify({ pkg, ok: false });
  }
}
module.exports = { run };
