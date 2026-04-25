const _ = require("lodash");
const semver = require("semver");

const chunks = _.chunk([1, 2, 3, 4], 2);
const valid = semver.valid("1.0.0");
if (!valid) {
  throw new Error("semver.valid failed");
}

console.log(JSON.stringify({ chunks, valid }));
