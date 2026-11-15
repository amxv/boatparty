const test = require("node:test");
const assert = require("node:assert");
const ms = require("ms");

test("parses a duration with the dependency", () => {
  assert.strictEqual(ms("2h"), 7200000);
});
