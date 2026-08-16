const test = require("node:test");
const assert = require("node:assert");
const L = require("../src/logic.js");

// --- isAlreadyInDoc: the distinction spec §6's status alone cannot express ---

test("an entry present in the doc but unmarked is not the same as one that is absent", () => {
  const absent = { status: "to_cut", matched_line: null };
  const present = { status: "to_cut", matched_line: "合成焦點報導1200 SOT 11:32:45" };

  assert.equal(L.isAlreadyInDoc(absent), false);
  assert.equal(L.isAlreadyInDoc(present), true);
});

test("a marked entry is not reported as already-in-doc, it is reported as removed", () => {
  const marked = { status: "removed", matched_line: "00 合成焦點報導1200 SOT 11:32:45" };
  assert.equal(L.isAlreadyInDoc(marked), false);
});

// --- decideInclusion: regression tests for the "tick only ever cleared" bug ---

test("comparing again re-ticks an entry that has since been removed from the doc", () => {
  // It was in the doc on the first compare, so it got unticked...
  const first = { status: "to_cut", matched_line: "合成焦點報導1200 SOT 11:32:45" };
  assert.equal(L.decideInclusion(first, true), false);

  // ...a colleague deletes it, and comparing again must bring the tick back rather
  // than leaving the entry silently skipped.
  const second = { status: "to_cut", matched_line: null };
  assert.equal(L.decideInclusion(second, true), true);
});

test("entries handled in the doc are unticked so they are not written twice", () => {
  assert.equal(L.decideInclusion({ status: "removed", matched_line: "00 x" }, true), false);
  assert.equal(
    L.decideInclusion({ status: "to_cut", matched_line: "x" }, true),
    false,
    "already in the doc"
  );
});

test("an entry flagged for re-paste stays ticked", () => {
  assert.equal(L.decideInclusion({ status: "keep_refresh", matched_line: "抓新 x" }, true), true);
});

test("comparing never opts in a bucket that starts unticked", () => {
  // Unknown-style entries need a human call; a comparison result must not tick them.
  assert.equal(L.decideInclusion({ status: "to_cut", matched_line: null }, false), false);
  assert.equal(L.decideInclusion({ status: "keep_refresh", matched_line: "抓新" }, false), false);
});

// --- summarizeMatches ---

test("counts split already-in-doc out of the to-cut total", () => {
  const counts = L.summarizeMatches([
    { status: "to_cut", matched_line: null },
    { status: "to_cut", matched_line: null },
    { status: "to_cut", matched_line: "already here" },
    { status: "keep_refresh", matched_line: "抓新 x" },
    { status: "removed", matched_line: "00 x" },
  ]);

  assert.deepEqual(counts, { toCut: 2, keepRefresh: 1, removed: 1, alreadyIn: 1 });
});

// --- selectKeywordTargets: this is what protects the API quota ---

function entry(over) {
  return {
    bucket: "passed",
    included: true,
    body: "內文",
    keywords: "",
    matchStatus: "to_cut",
    alreadyInDoc: false,
    ...over,
  };
}

test("entries already in the doc never reach the API", () => {
  const items = [entry({ alreadyInDoc: true }), entry({ matchStatus: "removed" }), entry()];
  assert.equal(L.selectKeywordTargets(items).length, 1);
});

test("entries that already have keywords are not regenerated", () => {
  // Re-running after a rate limit must only retry the ones that failed.
  const items = [entry({ keywords: "#合成關鍵字一 #合成關鍵字二" }), entry()];
  assert.equal(L.selectKeywordTargets(items).length, 1);
});

test("unticked entries and empty bodies are skipped", () => {
  const items = [entry({ included: false }), entry({ body: "   " }), entry()];
  assert.equal(L.selectKeywordTargets(items).length, 1);
});

test("filtered and failed buckets are never keyword targets", () => {
  const items = [entry({ bucket: "filtered" }), entry({ bucket: "failed" }), entry({ bucket: "manual" })];
  assert.equal(L.selectKeywordTargets(items).length, 1);
});

// --- isRateLimitError ---

test("recognises the quota failure from either the status code or the message", () => {
  assert.equal(L.isRateLimitError("Gemini API 錯誤（429）：RESOURCE_EXHAUSTED"), true);
  assert.equal(L.isRateLimitError("已達 Gemini 每分鐘請求上限（429）。"), true);
});

test("does not mistake other failures for a quota failure", () => {
  assert.equal(L.isRateLimitError("Gemini API 錯誤（404）：model not found"), false);
  assert.equal(L.isRateLimitError("連線失敗：timeout"), false);
  assert.equal(L.isRateLimitError(undefined), false);
});

// --- computeFunnel ---

test("the funnel counts only output-eligible buckets", () => {
  const items = [
    entry(),
    entry({ included: false }),
    entry({ bucket: "filtered" }),
    entry({ bucket: "failed" }),
  ];
  assert.deepEqual(L.computeFunnel(items), { pending: 2, skipped: 1, outgoing: 1 });
});

// --- buildOutputText: spec §5 ---

test("each entry renders as four lines with a blank line between entries", () => {
  const items = [
    {
      bucket: "passed",
      included: true,
      slug: "合成焦點報導1800",
      style: "SOT",
      time: "07:49:58",
      group: "政",
      title: "合成標題範例",
      body: "這是合成的內文範例",
      keywords: "#合成關鍵字一 #合成關鍵字二",
    },
    {
      bucket: "passed",
      included: true,
      slug: "合成車輛報導1800",
      style: "SOT",
      time: "10:57:44",
      group: "",
      title: "合成標題範例二",
      body: "這是合成的內文範例二",
      keywords: "#合成關鍵字三 #合成關鍵字四",
    },
  ];

  assert.equal(
    L.buildOutputText(items),
    "合成焦點報導1800 SOT 07:49:58 政\n合成標題範例\n這是合成的內文範例\n#合成關鍵字一 #合成關鍵字二\n\n" +
      "合成車輛報導1800 SOT 10:57:44\n合成標題範例二\n這是合成的內文範例二\n#合成關鍵字三 #合成關鍵字四"
  );
});

test("a blank group leaves no trailing space in the header line", () => {
  const out = L.buildOutputText([
    { bucket: "passed", included: true, slug: "s", style: "SOT", time: "01:00:00", group: "  ", title: "t", body: "b", keywords: "k" },
  ]);
  assert.equal(out.split("\n")[0], "s SOT 01:00:00");
});

test("unticked entries are left out of the output entirely", () => {
  const out = L.buildOutputText([
    { bucket: "passed", included: false, slug: "s", style: "SOT", time: "01:00:00", group: "", title: "t", body: "b", keywords: "k" },
  ]);
  assert.equal(out, "");
});
