"use strict";

const fs = require("fs");
const path = require("path");
const { run } = require("./smokes.cjs");
const marker = path.join(__dirname, ".current-pkg");
const pkg =
  process.env.KAWKAB_PKG && String(process.env.KAWKAB_PKG).trim()
    ? String(process.env.KAWKAB_PKG).trim()
    : fs.readFileSync(marker, "utf8").trim();
if (!pkg) {
  console.error("KAWKAB_PKG or .current-pkg missing");
  process.exit(2);
}
try {
  console.log(run(pkg));
} catch (e) {
  console.error(e && e.stack ? e.stack : e);
  process.exit(1);
}
