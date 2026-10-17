/**
 * Pure decision logic for the UI, deliberately free of DOM and Tauri references so
 * it can run under `node --test`. Every bug found in review so far lived in these
 * rules rather than in rendering, so this is where the safety net is worth having.
 *
 * Loaded as a plain script in the app (exposing `window.AppLogic`) and required
 * directly by the tests -- no build step or module-type change to the app.
 */
(function (root, factory) {
  const api = factory();
  if (typeof module === "object" && module.exports) module.exports = api;
  else root.AppLogic = api;
})(typeof self !== "undefined" ? self : globalThis, function () {
  /** Buckets whose entries are part of the normal flow and counted in the funnel. */
  const OUTPUT_BUCKETS = ["passed", "manual", "unknown"];

  function isOutputBucket(bucket) {
    return OUTPUT_BUCKETS.includes(bucket);
  }

  /**
   * Buckets an entry can be written out from *if the user ticks it*.
   *
   * 已濾除 is included: a blocked style is a default, not a verdict, and a BS story
   * does occasionally turn out to be needed. Filtered entries start unticked, so this
   * changes nothing until someone deliberately ticks one — the alternative was
   * editing the blocklist in settings and re-importing just to rescue a single item.
   */
  function canOutputBucket(bucket) {
    return isOutputBucket(bucket) || bucket === "filtered";
  }

  /**
   * spec §6 collapses "not in the doc" and "in the doc but unmarked" into the same
   * ToCut status, but for deciding what to write back they are opposites. Only
   * `matched_line` distinguishes them.
   */
  function isAlreadyInDoc(result) {
    return result.status === "to_cut" && !!result.matched_line;
  }

  /**
   * Whether an entry should stay ticked for output after a comparison.
   *
   * Derived fresh from the result rather than only ever being cleared: if an entry
   * is later removed from the shared doc, comparing again must tick it back on.
   * `defaultIncluded` keeps buckets that start unticked (unknown styles, which need
   * a human call) from being silently opted in.
   */
  function decideInclusion(result, defaultIncluded) {
    const handled = isAlreadyInDoc(result) || result.status === "removed";
    return !handled && defaultIncluded;
  }

  /** Tally for the status line, keyed the same way the badges are labelled. */
  function summarizeMatches(results) {
    const counts = { toCut: 0, keepRefresh: 0, removed: 0, alreadyIn: 0 };
    for (const r of results) {
      if (isAlreadyInDoc(r)) counts.alreadyIn++;
      else if (r.status === "to_cut") counts.toCut++;
      else if (r.status === "keep_refresh") counts.keepRefresh++;
      else if (r.status === "removed") counts.removed++;
    }
    return counts;
  }

  /**
   * Whether an entry is marked do-not-publish, by comparing its composed marker with
   * the configured label. Both sides come from the same config, so a user who renames
   * the label keeps working.
   *
   * A blank label means "no marker configured" and must never match, or every entry
   * without a marker at all would read as do-not-publish.
   */
  function isNoUpload(item, noUploadLabel) {
    const label = (noUploadLabel ?? "").trim();
    if (label === "") return false;
    return (item.slug_marker ?? "").trim() === label;
  }

  /**
   * Entries worth spending Gemini quota on. Anything already handled in the shared
   * doc is excluded: re-generating keywords for it burns tokens for output that
   * would only be a duplicate.
   *
   * 勿上網 entries are excluded too. They still get written back normally -- they are
   * only skipped here, because a story that never goes online has no use for
   * keywords, and the run is capped per minute by the free tier: letting them in
   * pushes stories that *do* need keywords into the next batch and another minute of
   * waiting. This is the batch default only; the per-card 產生關鍵字 button still
   * works on them, so it stays a default rather than a verdict.
   */
  function selectKeywordTargets(items, noUploadLabel) {
    return items.filter(
      (i) =>
        canOutputBucket(i.bucket) &&
        i.included &&
        (i.body ?? "").trim() !== "" &&
        (i.keywords ?? "").trim() === "" &&
        i.matchStatus !== "removed" &&
        !i.alreadyInDoc &&
        !isNoUpload(i, noUploadLabel)
    );
  }

  /**
   * Split a keyword run into the slice to send now and the count left over.
   *
   * Producers tidy up roughly every half hour, so one import can hold dozens of
   * scripts — well past the free tier's per-minute request cap. Sending them all
   * would return a screen of 429 cards, so a run is capped and the remainder is
   * reported instead. Re-running picks up exactly the leftovers, because entries
   * that already have keywords are filtered out by selectKeywordTargets.
   *
   * A limit of 0 or less means "no cap" — someone on a paid key should not be
   * throttled by a free-tier number.
   */
  function splitKeywordRun(targets, limit) {
    const list = targets ?? [];
    if (!Number.isFinite(limit) || limit <= 0) return { batch: list, remaining: 0 };
    return { batch: list.slice(0, limit), remaining: Math.max(0, list.length - limit) };
  }

  /**
   * The free tier caps requests per minute; that failure is worth a banner because
   * the fix ("wait a minute, run again") differs from every other error.
   */
  function isRateLimitError(text) {
    const s = String(text ?? "");
    return s.includes("429") || s.includes("每分鐘請求上限");
  }

  /**
   * The entries an output action (複製 / 存檔 / 寫入) would send, right now.
   *
   * One rule, used by every button and by the enabled/disabled state behind them.
   * It used to be spelled out again inside the write-back handler as
   * passed/manual/unknown, which quietly dropped rescued 已濾除 entries: the funnel
   * counted them and buildOutputText wrote them, but 寫入 stayed greyed out and
   * reported "沒有勾選任何要輸出的稿件".
   */
  function outgoingItems(items) {
    return (items ?? []).filter((i) => canOutputBucket(i.bucket) && i.included);
  }

  /** Numbers behind the "N 則待處理 → N 則略過 → N 則寫入" line. */
  function computeFunnel(items) {
    // Filtered entries only enter the count once rescued, so the everyday numbers are
    // unchanged — but a rescued one must show up in 寫入, or the funnel would under-
    // report what actually goes into the shared doc.
    const relevant = items.filter(
      (i) => isOutputBucket(i.bucket) || (i.bucket === "filtered" && i.included)
    );
    const outgoing = relevant.filter((i) => i.included).length;
    return { pending: relevant.length, skipped: relevant.length - outgoing, outgoing };
  }

  /**
   * The four-line-per-entry output of spec §5, entries separated by a blank line.
   * Header fields are slug / style / time / group; blank fields are dropped.
   */
  function buildOutputText(items) {
    return items
      .filter((i) => canOutputBucket(i.bucket) && i.included)
      .map((i) => {
        // The 編輯備註 marker is prefixed here, at output time only. It is never part
        // of `slug` itself, because `slug` is what gets matched against the shared
        // doc -- a prefixed slug would fail every comparison.
        // 樣式 can legitimately be blank now: a row kept only because 編輯備註 named
        // a format carries no style until a human sets one, and a blank field would
        // otherwise leave a double space in the middle of the header line.
        const head = [`${i.slug_marker ?? ""}${i.slug}`, i.style, i.time, i.group]
          .filter((f) => (f ?? "").trim() !== "");
        return [head.join(" "), i.title, i.body, i.keywords].join("\n");
      })
      .join("\n\n");
  }

  /**
   * Order entries by 累積時間 ascending — earliest at the top — because that is the
   * order the tapes get cut in, and both the card list and the written-back output
   * have to follow it.
   *
   * Times are `HH:MM:SS` and compare correctly as plain strings once zero-padded,
   * which iNews already does. Anything without a usable time sorts last rather than
   * first, so a missing field can never silently push an entry to the top of a
   * running order. Ties keep their original order.
   */
  function sortByTime(items) {
    const key = (i) => {
      const t = (i.time ?? "").trim();
      return /^\d{1,2}:\d{2}:\d{2}$/.test(t) ? t.padStart(8, "0") : null;
    };
    return items
      .map((item, index) => ({ item, index, k: key(item) }))
      .sort((a, b) => {
        if (a.k === null && b.k === null) return a.index - b.index;
        if (a.k === null) return 1;
        if (b.k === null) return -1;
        if (a.k === b.k) return a.index - b.index;
        return a.k < b.k ? -1 : 1;
      })
      .map((w) => w.item);
  }

  /**
   * Card order on screen: ticked entries first, unticked after, each group still in
   * running order. Stable, so it never reshuffles entries within a group.
   */
  function sortForDisplay(items) {
    const sorted = sortByTime(items);
    return [...sorted.filter((i) => i.included), ...sorted.filter((i) => !i.included)];
  }

  /** The backend enum tag carried on every entry DTO. */
  function kindOf(dto) {
    return dto.kind; // "Passed" | "UnknownStyle" | "NeedsManualContent" | "FilteredByStyle" | "ParseFailed"
  }

  function bucketOf(kind) {
    switch (kind) {
      case "Passed": return "passed";
      case "UnknownStyle": return "unknown";
      case "NeedsManualContent": return "manual";
      case "FilteredByStyle": return "filtered";
      case "ParseFailed": return "failed";
      default: return "unknown";
    }
  }

  /**
   * Build one UI item from a backend entry DTO.
   *
   * Lives here, as a pure function, because of what happened in v0.1.9: this mapping
   * quietly dropped `slug_marker`, so every rule downstream that reads it — the
   * (勿上網) badge, keeping those entries out of the Gemini batch — saw `undefined`
   * and silently did nothing. The unit tests had built their items by hand, complete
   * with a `slug_marker`, so nothing caught it. Anything a rule reads off an item has
   * to be produced here, by code a test can call.
   */
  function itemFromDto(dto, index) {
    const kind = kindOf(dto);
    const bucket = bucketOf(kind);
    const fields = dto[kind] ?? dto; // enum payload
    // 待補稿 starts unticked: it has no body yet, so writing it out by default would
    // push empty entries into the doc.
    const included = bucket === "passed";
    return {
      dto,
      kind,
      bucket,
      id: `entry-${index}`,
      included,
      /** The import-time default, kept so a re-compare can restore the tick without
       *  opting in buckets (待補稿, unknown styles) that are deliberately off to begin
       *  with. */
      defaultIncluded: included,
      title: fields.title ?? "",
      body: fields.body ?? "",
      slug_marker: fields.slug_marker ?? "",
      keywords: "",
      keywordStatus: "idle", // idle | loading | error
      keywordError: "",
      matchStatus: null, // null | "to_cut" | "keep_refresh" | "removed"
      matchedLine: null,
      alreadyInDoc: false,
      // Collapsed by default: a full day is ~16 entries and reviewing means scanning
      // slugs and statuses, not reading every body. Editing is one click away.
      collapsed: true,
      time: fields.time ?? "",
    };
  }

  // --- 晚報 (evening bulletin) list -------------------------------------------

  /**
   * One evening card from a backend entry DTO, or null for a file that failed to
   * parse (it has no slug to match). Every bucket is kept, 已濾除 included: whether a
   * story counts is decided by the shared doc, not by its rundown style.
   */
  function eveningItemFromDto(dto, index) {
    const kind = kindOf(dto);
    if (kind === "ParseFailed") return null;
    const f = dto[kind] ?? dto;
    return {
      id: `ev-${index}`,
      slug: f.slug ?? "",
      fileName: f.file_name ?? "",
      style: f.style ?? "",
      time: f.time ?? "",
      group: f.group ?? "",
      marker: f.slug_marker ?? "",
      title: f.title ?? "",
      body: f.body ?? "",
      keywords: "",
      hashtag: "",
      /** Once someone types a hashtag it stops following the keywords. */
      hashtagManual: false,
      status: null, // null | "hit" | "no_online" | "not_found"
      matchedLine: null,
      included: false,
      keywordStatus: "idle",
      keywordError: "",
    };
  }

  /**
   * Whether a compare result puts a story on the list: in the day's block, online,
   * and not marked 勿上網 by its own 編輯備註.
   */
  function isEveningHit(status, item, noUploadLabel) {
    return status === "hit" && !isNoUpload({ slug_marker: item.marker }, noUploadLabel);
  }

  /**
   * Group sizes for `n` stories. Up to 8 stay as one group; from 9 the list is split
   * into at least two, at most 10 per group, as evenly as possible with any extra
   * one going to the later groups (13 -> 6/7).
   */
  function eveningGroupSizes(n) {
    if (n <= 0) return [];
    if (n <= 8) return [n];
    const groups = Math.max(2, Math.ceil(n / 10));
    const base = Math.floor(n / groups);
    const extra = n % groups;
    return Array.from({ length: groups }, (_, g) => base + (g >= groups - extra ? 1 : 0));
  }

  function splitIntoGroups(list) {
    const out = [];
    let at = 0;
    for (const size of eveningGroupSizes(list.length)) {
      out.push(list.slice(at, at + size));
      at += size;
    }
    return out;
  }

  /** 獨家 first, then 最新, then simply the first story in running order. */
  function pickHeadline(group, { exclusivePrefix = "獨家》", latestPrefix = "最新》" } = {}) {
    const starts = (p) => p && group.find((i) => (i.title ?? "").trim().startsWith(p));
    return starts(exclusivePrefix) || starts(latestPrefix) || group[0] || null;
  }

  function keywordWords(keywords) {
    return String(keywords ?? "")
      .split(/[\s#]+/)
      .map((w) => w.trim())
      .filter(Boolean);
  }

  /**
   * The one word that stands for a story: the first of its keywords that actually
   * appears in its title (烏龜翻 in 「…烏龜翻 駕駛爬出落跑…」), else the first keyword.
   */
  function autoHashtag(title, keywords) {
    const words = keywordWords(keywords);
    const t = title ?? "";
    return words.find((w) => t.includes(w)) ?? words[0] ?? "";
  }

  /** The hashtag that will be printed, without the leading `#`. */
  function hashtagOf(item) {
    const raw = item.hashtagManual ? item.hashtag : autoHashtag(item.title, item.keywords);
    return String(raw ?? "").replace(/^[\s#]+/, "").trim();
  }

  /** The stories that go out, in running order. */
  function eveningOutgoing(items) {
    return sortByTime((items ?? []).filter((i) => i.included));
  }

  /** `2026-10-17` -> `2026/10/17`, the form printed in the header line. */
  function slashDate(isoDate) {
    return String(isoDate ?? "").replace(/-/g, "/");
  }

  /** `2026-10-17` -> `1017`, the form of the doc's date divider. */
  function mmddOf(isoDate) {
    const m = /^\d{4}-(\d{2})-(\d{2})$/.exec(String(isoDate ?? ""));
    return m ? m[1] + m[2] : "";
  }

  /**
   * The list to copy: one complete block per group --
   *   {headline}  {yyyy/mm/dd}【{anchor}晚報】
   *   (blank)
   *   {title per story}
   *   #{word}#{word}...
   * -- with a blank line between groups. Stories without a hashtag are just left
   * out of the hashtag line rather than printing a bare `#`.
   */
  function buildEveningText(items, opts = {}) {
    return eveningGroups(items).map((group) => buildEveningGroupText(group, opts)).join("\n\n");
  }

  /** The stories that go out, already split into their groups. */
  function eveningGroups(items) {
    return splitIntoGroups(eveningOutgoing(items));
  }

  function buildEveningGroupText(group, { date, anchor, exclusivePrefix, latestPrefix } = {}) {
    const head = pickHeadline(group, { exclusivePrefix, latestPrefix });
    if (!head) return "";
    const lines = [`${head.title}  ${slashDate(date)}【${anchor ?? ""}晚報】`, ""];
    lines.push(...group.map((i) => i.title));
    const tags = group.map(hashtagOf).filter(Boolean);
    if (tags.length) lines.push(tags.map((t) => `#${t}`).join(""));
    return lines.join("\n");
  }

  /**
   * Stories in one group still waiting for keywords. Keyword runs are per group on
   * purpose: one press covers exactly the block about to be copied, instead of
   * spending quota on every hit of the night at once.
   */
  function groupKeywordTargets(group) {
    return group.filter((i) => !(i.keywords ?? "").trim() && ((i.title ?? "").trim() || (i.body ?? "").trim()));
  }

  /** Slugs in the same groups and order, for looking stories up afterwards. */
  function buildEveningSlugList(items) {
    return splitIntoGroups(eveningOutgoing(items))
      .map((group) => group.map((i) => i.slug).join("\n"))
      .join("\n\n");
  }

  /** The saved txt: the list as copied, then the slugs at the bottom. */
  function buildEveningExport(items, opts) {
    const text = buildEveningText(items, opts);
    if (!text) return "";
    return `${text}\n\n----- slug -----\n${buildEveningSlugList(items)}\n`;
  }

  /**
   * Re-importing the same day must not throw away what was already edited. Cards are
   * matched by slug and keep their title, keywords, hashtag and tick; everything
   * else (time, style, marker) comes fresh from the new import.
   */
  function mergeEveningEdits(fresh, previous) {
    const bySlug = new Map((previous ?? []).map((p) => [p.slug, p]));
    return fresh.map((item) => {
      const p = bySlug.get(item.slug);
      if (!p) return item;
      return {
        ...item,
        title: p.title,
        keywords: p.keywords,
        hashtag: p.hashtag,
        hashtagManual: p.hashtagManual,
        status: p.status,
        matchedLine: p.matchedLine,
        included: p.included,
      };
    });
  }

  return {
    eveningItemFromDto,
    isEveningHit,
    eveningGroupSizes,
    splitIntoGroups,
    pickHeadline,
    autoHashtag,
    hashtagOf,
    eveningOutgoing,
    slashDate,
    mmddOf,
    buildEveningText,
    eveningGroups,
    buildEveningGroupText,
    groupKeywordTargets,
    buildEveningSlugList,
    buildEveningExport,
    mergeEveningEdits,
    kindOf,
    bucketOf,
    itemFromDto,
    sortByTime,
    sortForDisplay,
    OUTPUT_BUCKETS,
    isOutputBucket,
    canOutputBucket,
    isAlreadyInDoc,
    isNoUpload,
    decideInclusion,
    summarizeMatches,
    selectKeywordTargets,
    splitKeywordRun,
    isRateLimitError,
    computeFunnel,
    outgoingItems,
    buildOutputText,
  };
});
