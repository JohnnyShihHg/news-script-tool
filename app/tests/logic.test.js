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

test("failed entries are never keyword targets", () => {
  const items = [entry({ bucket: "failed" }), entry({ bucket: "manual" })];
  assert.equal(L.selectKeywordTargets(items).length, 1);
});

// --- 勿上網: kept out of batch keyword runs, but still written back ---

const NO_UPLOAD = "(勿上網)";

test("a 勿上網 entry is not sent to the API in a batch run", () => {
  const items = [entry({ slug_marker: NO_UPLOAD }), entry()];
  assert.equal(L.selectKeywordTargets(items, NO_UPLOAD).length, 1);
});

test("a 勿上網 entry is still written back to the doc", () => {
  // Skipping the API must not turn into skipping the entry: it goes out as usual,
  // just with an empty keyword line.
  const item = {
    ...entry({ slug_marker: NO_UPLOAD }),
    slug: "合成焦點報導1800", title: "合成標題", style: "SOT", time: "07:49:58", group: "政",
  };
  assert.equal(L.buildOutputText([item]).split("\n")[0], "(勿上網)合成焦點報導1800 SOT 07:49:58 政");
});

test("other markers are unaffected", () => {
  const items = [entry({ slug_marker: "(可上網)" }), entry({ slug_marker: "(版權問題)" })];
  assert.equal(L.selectKeywordTargets(items, NO_UPLOAD).length, 2);
});

test("a renamed label still matches, because both sides come from the same config", () => {
  const items = [entry({ slug_marker: "★禁上網★" })];
  assert.equal(L.selectKeywordTargets(items, "★禁上網★").length, 0);
});

test("a blank label matches nothing rather than everything", () => {
  // Entries with no marker at all have slug_marker "", so a blank label comparing
  // equal would silently empty every keyword run.
  const items = [entry(), entry({ slug_marker: "" })];
  assert.equal(L.selectKeywordTargets(items, "").length, 2);
  assert.equal(L.selectKeywordTargets(items, undefined).length, 2);
});

test("isNoUpload ignores surrounding whitespace on both sides", () => {
  assert.equal(L.isNoUpload({ slug_marker: " (勿上網) " }, NO_UPLOAD), true);
  assert.equal(L.isNoUpload({}, NO_UPLOAD), false);
});

// --- 已濾除 rescue: a blocked style is a default, not a verdict ---

test("a filtered entry is not output or sent to the API while it stays unticked", () => {
  const items = [entry({ bucket: "filtered", included: false })];
  assert.equal(L.selectKeywordTargets(items).length, 0);
  assert.equal(L.buildOutputText(items.map((i) => ({ ...i, slug: "s", title: "t" }))), "");
});

test("ticking a filtered entry lets it be written out and get keywords", () => {
  const items = [entry({ bucket: "filtered", included: true })];
  assert.equal(L.selectKeywordTargets(items).length, 1);
  const out = L.buildOutputText([
    { bucket: "filtered", included: true, slug: "後送2天嬰11", style: "BS", time: "11:00:00", group: "生", title: "標題", body: "內文", keywords: "#合成關鍵字" },
  ]);
  assert.ok(out.includes("後送2天嬰11"), out);
  assert.ok(out.includes("內文"), out);
});

test("the funnel ignores filtered entries until one is rescued", () => {
  const base = [entry({ bucket: "passed", included: true }), entry({ bucket: "filtered", included: false })];
  assert.deepEqual(L.computeFunnel(base), { pending: 1, skipped: 0, outgoing: 1 });

  // Once ticked it has to appear in 寫入, or the funnel under-reports what goes in.
  const rescued = [entry({ bucket: "passed", included: true }), entry({ bucket: "filtered", included: true })];
  assert.deepEqual(L.computeFunnel(rescued), { pending: 2, skipped: 0, outgoing: 2 });
});

test("a rescued filtered entry is part of what 複製／存檔／寫入 send", () => {
  // The write-back handler used to re-spell this rule as passed/manual/unknown, so a
  // ticked 已濾除 story was written by buildOutputText but counted as nothing by the
  // button in front of it: 寫入 stayed disabled and reported 沒有勾選任何要輸出的稿件.
  const items = [
    entry({ bucket: "passed", included: true }),
    entry({ bucket: "filtered", included: true }),
    entry({ bucket: "filtered", included: false }),
    entry({ bucket: "unknown", included: true }),
    entry({ bucket: "manual", included: true }),
    entry({ bucket: "failed", included: true }),
  ];
  const out = L.outgoingItems(items);
  assert.equal(out.length, 4);
  assert.deepEqual(out.map((i) => i.bucket), ["passed", "filtered", "unknown", "manual"]);
});

test("outgoingItems agrees with what buildOutputText actually writes", () => {
  const items = [
    entry({ bucket: "filtered", included: true, slug: "後送2天嬰11", style: "BS", time: "11:00:00", title: "標題", body: "內文" }),
    entry({ bucket: "passed", included: false, slug: "略過", style: "SOT", time: "12:00:00", title: "t", body: "b" }),
  ];
  const written = L.buildOutputText(items).split("\n\n").filter((s) => s.trim() !== "");
  assert.equal(written.length, L.outgoingItems(items).length);
});

test("failed entries stay out of output even if somehow ticked", () => {
  const items = [{ bucket: "failed", included: true, slug: "x", title: "t", body: "b", keywords: "" }];
  assert.equal(L.buildOutputText(items), "");
});

// --- splitKeywordRun: keeps a half-hour batch from hitting the per-minute cap ---

test("a run is capped at the limit and reports what is left", () => {
  const targets = Array.from({ length: 23 }, () => entry());
  const { batch, remaining } = L.splitKeywordRun(targets, 15);
  assert.equal(batch.length, 15);
  assert.equal(remaining, 8);
});

test("a batch under the limit is sent whole with nothing left over", () => {
  const { batch, remaining } = L.splitKeywordRun([entry(), entry()], 15);
  assert.equal(batch.length, 2);
  assert.equal(remaining, 0);
});

test("a limit of zero means no cap, for anyone on a paid key", () => {
  const targets = Array.from({ length: 40 }, () => entry());
  const { batch, remaining } = L.splitKeywordRun(targets, 0);
  assert.equal(batch.length, 40);
  assert.equal(remaining, 0);
});

test("re-running picks up exactly the leftovers", () => {
  // The second press must cover the remainder and nothing else: selectKeywordTargets
  // drops whatever already has keywords, so the two steps compose.
  const targets = Array.from({ length: 23 }, () => entry());
  const first = L.splitKeywordRun(targets, 15);
  first.batch.forEach((i) => { i.keywords = "#合成關鍵字"; });
  const second = L.splitKeywordRun(L.selectKeywordTargets(targets), 15);
  assert.equal(second.batch.length, 8);
  assert.equal(second.remaining, 0);
});

test("an empty or missing target list is safe", () => {
  assert.deepEqual(L.splitKeywordRun([], 15), { batch: [], remaining: 0 });
  assert.deepEqual(L.splitKeywordRun(undefined, 15), { batch: [], remaining: 0 });
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
    // Filtered counts only once rescued (covered separately); failed never does.
    entry({ bucket: "filtered", included: false }),
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

// --- sortByTime: running order drives which tape gets cut when ---

test("entries are ordered by 累積時間 with the earliest first", () => {
  const items = [
    { time: "10:57:44", slug: "c" },
    { time: "06:48:32", slug: "a" },
    { time: "19:02:27", slug: "d" },
    { time: "07:01:27", slug: "b" },
  ];
  assert.deepEqual(L.sortByTime(items).map((i) => i.slug), ["a", "b", "c", "d"]);
});

test("single-digit hours still sort before later ones", () => {
  const items = [{ time: "12:00:00", slug: "noon" }, { time: "9:30:00", slug: "morning" }];
  assert.deepEqual(L.sortByTime(items).map((i) => i.slug), ["morning", "noon"]);
});

test("entries without a usable time sort last, never first", () => {
  // A missing field must not quietly promote an entry to the top of a running order.
  const items = [
    { time: "", slug: "blank" },
    { time: "08:00:00", slug: "real" },
    { time: "not a time", slug: "junk" },
  ];
  assert.deepEqual(L.sortByTime(items).map((i) => i.slug), ["real", "blank", "junk"]);
});

test("equal times keep their original order", () => {
  const items = [
    { time: "08:00:00", slug: "first" },
    { time: "08:00:00", slug: "second" },
  ];
  assert.deepEqual(L.sortByTime(items).map((i) => i.slug), ["first", "second"]);
});

test("sorting does not mutate the array it was given", () => {
  const items = [{ time: "10:00:00", slug: "b" }, { time: "09:00:00", slug: "a" }];
  L.sortByTime(items);
  assert.deepEqual(items.map((i) => i.slug), ["b", "a"]);
});

// --- slug_marker: composed at output only, never fused into the slug ---

test("the 編輯備註 marker is prefixed onto the slug line in the output", () => {
  const out = L.buildOutputText([
    {
      bucket: "passed", included: true,
      slug: "合成焦點報導1800", slug_marker: "(勿上網)",
      style: "SOT", time: "07:49:58", group: "政",
      title: "合成標題範例", body: "內文", keywords: "#關鍵字",
    },
  ]);
  assert.equal(out.split("\n")[0], "(勿上網)合成焦點報導1800 SOT 07:49:58 政");
});

test("an entry with no marker renders exactly as before", () => {
  const out = L.buildOutputText([
    {
      bucket: "passed", included: true,
      slug: "合成焦點報導1800", slug_marker: "",
      style: "SOT", time: "07:49:58", group: "政",
      title: "t", body: "b", keywords: "k",
    },
  ]);
  assert.equal(out.split("\n")[0], "合成焦點報導1800 SOT 07:49:58 政");
});

test("a missing slug_marker field does not print 'undefined'", () => {
  // Older cached entries, or any bucket that never carried the field.
  const out = L.buildOutputText([
    { bucket: "passed", included: true, slug: "s", style: "SOT", time: "01:00:00", group: "", title: "t", body: "b", keywords: "k" },
  ]);
  assert.equal(out.split("\n")[0], "s SOT 01:00:00");
});

test("the marker never becomes part of the slug used for doc matching", () => {
  // Compare sends `slug` to the shared doc; if the marker were fused in, every
  // comparison would miss and the tool would re-write entries already in the doc.
  const item = { slug: "合成焦點報導1800", slug_marker: "(勿上網)" };
  assert.equal(item.slug, "合成焦點報導1800");
});

// --- sortForDisplay: ticked cards on top, running order kept inside each group ---

test("sortForDisplay puts ticked entries first and keeps time order within each group", () => {
  const items = [
    { slug: "a", time: "10:00:00", included: false },
    { slug: "b", time: "09:00:00", included: true },
    { slug: "c", time: "08:00:00", included: false },
    { slug: "d", time: "11:00:00", included: true },
    { slug: "e", time: "", included: true },
  ];
  assert.deepEqual(L.sortForDisplay(items).map((i) => i.slug), ["b", "d", "e", "c", "a"]);
  // Display only: the input's own order is untouched.
  assert.deepEqual(items.map((i) => i.slug), ["a", "b", "c", "d", "e"]);
});

// --- itemFromDto: the layer v0.1.9's tests skipped over ---
// The (勿上網) rules below were all unit-tested and all correct; what broke was the
// step before them, which never copied slug_marker onto the item. These tests start
// from a backend DTO rather than a hand-built item, so that gap cannot reopen.

/** The shape core/src/model.rs serialises for a passed entry. */
function passedDto(fields = {}) {
  return {
    kind: "Passed",
    Passed: {
      slug: "合成焦點報導1200",
      slug_marker: "",
      style: "SOT",
      time: "11:32:45",
      group: "生",
      title: "合成標題",
      body: "合成內文",
      ...fields,
    },
  };
}

test("a (勿上網) marker on the backend DTO survives the mapping onto a UI item", () => {
  const item = L.itemFromDto(passedDto({ slug_marker: "(勿上網)" }), 0);
  assert.equal(item.slug_marker, "(勿上網)");
  assert.equal(item.bucket, "passed");
  assert.equal(item.title, "合成標題");
  assert.equal(item.time, "11:32:45");
});

test("an entry mapped from a (勿上網) DTO is kept out of the batch keyword run", () => {
  const items = [
    L.itemFromDto(passedDto({ slug_marker: "(勿上網)" }), 0),
    L.itemFromDto(passedDto({ slug: "合成生活消息0830" }), 1),
  ];
  const targets = L.selectKeywordTargets(items, "(勿上網)");
  assert.equal(targets.length, 1);
  assert.equal(targets[0].dto.Passed.slug, "合成生活消息0830");
});

test("a (勿上網) entry is still reported as such, so the card can badge it", () => {
  const item = L.itemFromDto(passedDto({ slug_marker: "(勿上網)" }), 0);
  assert.equal(L.isNoUpload(item, "(勿上網)"), true);
  // ...and it is still an ordinary ticked entry: the single-entry keyword button
  // stays available, this only opts it out of the batch.
  assert.equal(item.included, true);
});

test("the marker reaches the output line but never the slug itself", () => {
  const items = [L.itemFromDto(passedDto({ slug_marker: "(勿上網)" }), 0)];
  items[0].slug = items[0].dto.Passed.slug;
  items[0].style = "SOT";
  items[0].time = "11:32:45";
  items[0].group = "生";
  const out = L.buildOutputText(items);
  assert.match(out, /^\(勿上網\)合成焦點報導1200 SOT 11:32:45 生$/m);
  assert.equal(items[0].slug, "合成焦點報導1200");
});

test("other markers are mapped through untouched and stay in the batch", () => {
  for (const marker of ["(可上網)", "(版權問題)", ""]) {
    const item = L.itemFromDto(passedDto({ slug_marker: marker }), 0);
    assert.equal(item.slug_marker, marker);
    assert.equal(L.selectKeywordTargets([item], "(勿上網)").length, 1, `marker was ${marker}`);
  }
});

test("a DTO with no slug_marker at all maps to an empty string, not undefined", () => {
  const dto = { kind: "UnknownStyle", UnknownStyle: { slug: "合成未知樣式0700", title: "", body: "" } };
  const item = L.itemFromDto(dto, 3);
  assert.equal(item.slug_marker, "");
  assert.equal(item.bucket, "unknown");
  // Not part of the normal flow, so it starts unticked.
  assert.equal(item.included, false);
  assert.equal(item.id, "entry-3");
});
