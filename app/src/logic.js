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
  /** Buckets whose entries are candidates for output at all. */
  const OUTPUT_BUCKETS = ["passed", "manual", "unknown"];

  function isOutputBucket(bucket) {
    return OUTPUT_BUCKETS.includes(bucket);
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
   * Entries worth spending Gemini quota on. Anything already handled in the shared
   * doc is excluded: re-generating keywords for it burns tokens for output that
   * would only be a duplicate.
   */
  function selectKeywordTargets(items) {
    return items.filter(
      (i) =>
        isOutputBucket(i.bucket) &&
        i.included &&
        (i.body ?? "").trim() !== "" &&
        (i.keywords ?? "").trim() === "" &&
        i.matchStatus !== "removed" &&
        !i.alreadyInDoc
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

  /** Numbers behind the "N 則待處理 → N 則略過 → N 則寫入" line. */
  function computeFunnel(items) {
    const relevant = items.filter((i) => isOutputBucket(i.bucket));
    const outgoing = relevant.filter((i) => i.included).length;
    return { pending: relevant.length, skipped: relevant.length - outgoing, outgoing };
  }

  /**
   * The four-line-per-entry output of spec §5, entries separated by a blank line.
   * Header fields are slug / style / time / group, with group omitted when blank.
   */
  function buildOutputText(items) {
    return items
      .filter((i) => isOutputBucket(i.bucket) && i.included)
      .map((i) => {
        // The 編輯備註 marker is prefixed here, at output time only. It is never part
        // of `slug` itself, because `slug` is what gets matched against the shared
        // doc -- a prefixed slug would fail every comparison.
        const head = [`${i.slug_marker ?? ""}${i.slug}`, i.style, i.time];
        if ((i.group ?? "").trim() !== "") head.push(i.group);
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

  return {
    sortByTime,
    OUTPUT_BUCKETS,
    isOutputBucket,
    isAlreadyInDoc,
    decideInclusion,
    summarizeMatches,
    selectKeywordTargets,
    splitKeywordRun,
    isRateLimitError,
    computeFunnel,
    buildOutputText,
  };
});
