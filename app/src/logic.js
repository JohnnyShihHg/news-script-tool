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
   *
   * 解析失敗 likewise: it also holds rows the rules judged not to be news (blank 樣式,
   * SOU, empty placeholders), which used to vanish silently and once took a real SL
   * story with them. They start unticked and can be rescued the same way.
   */
  const RESCUE_BUCKETS = ["filtered", "failed"];

  function canOutputBucket(bucket) {
    return isOutputBucket(bucket) || RESCUE_BUCKETS.includes(bucket);
  }

  /**
   * spec §6 collapses "not in the doc" and "in the doc but unmarked" into the same
   * ToCut status, but for deciding what to write back they are opposites. Only
   * `matched_line` distinguishes them.
   */
  function isAlreadyInDoc(result) {
    // "in_doc" since v0.3.2; a to_cut that still names a line is the older shape.
    return result.status === "in_doc" || (result.status === "to_cut" && !!result.matched_line);
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
      (i) => isOutputBucket(i.bucket) || (RESCUE_BUCKETS.includes(i.bucket) && i.included)
    );
    const outgoing = relevant.filter((i) => i.included).length;
    return { pending: relevant.length, skipped: relevant.length - outgoing, outgoing };
  }

  /**
   * The per-entry output of spec §5, entries separated by a blank line: header,
   * title, a blank line (v0.3.3), body, keywords. The blank line is left out when
   * there is no body yet. Header fields are slug / style / time / group; blank
   * fields are dropped.
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
        const body = (i.body ?? "").trim() !== "" ? ["", i.body] : [i.body];
        return [head.join(" "), i.title, ...body, i.keywords].join("\n");
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
   * One evening card from a backend entry DTO, or null for 解析失敗. Every other
   * bucket is kept, 已濾除 included: whether a story counts is decided by the shared
   * doc, not by its rundown style. 解析失敗 stays out because it now holds the rundown
   * structure rows that used to be dropped before they ever reached this list.
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
      // Per-card keywords are an aid only; the list's hashtag line comes from the
      // group (see groupTagLine), not from these.
      keywords: "",
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

  /** Fullwidth ASCII folded to halfwidth and case dropped, so `ＳＯＴ` and `sot` read as `SOT`. */
  function foldStyle(s) {
    return String(s ?? "")
      .replace(/[\uFF01-\uFF5E]/g, (c) => String.fromCharCode(c.charCodeAt(0) - 0xfee0))
      .trim()
      .toUpperCase();
  }

  /**
   * Whether a story's rundown style may run in the 晚報 at all. Online is not enough:
   * SL goes online but never runs in the 晚報. Evening rundowns are nearly all
   * SOT/短sot, so `blocked` only names the exceptions.
   */
  function isEveningStyle(style, blocked) {
    const s = foldStyle(style);
    return !(blocked ?? []).some((b) => foldStyle(b) === s);
  }

  /**
   * Whether a story is a 專題 package, which never runs in the 晚報: its slug starts
   * with one of `terms` (`專ＯＯＯ`) or its 組 contains one. Only the slug's start
   * counts, so an ordinary 專家/專櫃 story in the middle of a slug is not caught.
   */
  function isEveningExcluded(item, terms) {
    const slug = foldStyle(item?.slug);
    const group = foldStyle(item?.group);
    return (terms ?? [])
      .map(foldStyle)
      .filter(Boolean)
      .some((t) => slug.startsWith(t) || group.includes(t));
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

  function tagWords(text) {
    return String(text ?? "")
      .split(/[\s#]+/)
      .map((w) => w.trim())
      .filter(Boolean);
  }

  /**
   * Identity of a group for its stored hashtag line: the slugs it holds. Groups are
   * recomputed from the ticks every time, so an index would hand one group's tags to
   * another as soon as a story is ticked or unticked.
   */
  function groupKey(group) {
    return group.map((i) => i.slug).join("|");
  }

  /**
   * A group's hashtag line, `#a#b#c#d`. What was typed by hand wins; otherwise the
   * group's generated keywords. `entry` is `{ auto: string[], manual: string|null }`.
   */
  function groupTagLine(entry) {
    const manual = entry?.manual;
    const words = manual != null && manual.trim() !== "" ? tagWords(manual) : tagWords((entry?.auto ?? []).join(" "));
    return words.map((w) => `#${w}`).join("");
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
   *   #{word}#{word}...   (the group's own keywords, one request per group)
   * -- with a blank line between groups. A group with no keywords yet simply has
   * no hashtag line. `opts.groupTags` maps groupKey -> { auto, manual }.
   */
  function buildEveningText(items, opts = {}) {
    return eveningGroups(items).map((group) => buildEveningGroupText(group, opts)).join("\n\n");
  }

  /** The stories that go out, already split into their groups. */
  function eveningGroups(items) {
    return splitIntoGroups(eveningOutgoing(items));
  }

  function buildEveningGroupHead(group, { date, anchor, exclusivePrefix, latestPrefix } = {}) {
    const head = pickHeadline(group, { exclusivePrefix, latestPrefix });
    if (!head) return "";
    return [`${head.title}  ${slashDate(date)}【${anchor ?? ""}晚報】`, "", ...group.map((i) => i.title)].join("\n");
  }

  function buildEveningGroupText(group, opts = {}) {
    const head = buildEveningGroupHead(group, opts);
    if (!head) return "";
    const tags = groupTagLine(opts.groupTags?.[groupKey(group)]);
    return tags ? `${head}\n${tags}` : head;
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
   * matched by slug and keep their title, keywords and tick; everything
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
        status: p.status,
        matchedLine: p.matchedLine,
        included: p.included,
      };
    });
  }

  // --- 從 iNEWS 自動匯入 ---------------------------------------------------------

  /**
   * UI items for an `import_inews` result. Same mapping as a folder import, but the
   * id is the story's iNEWS key rather than its position, so the same story keeps
   * its card -- and everything edited on it -- from one poll to the next.
   */
  function inewsItemsFromSummary(summary) {
    const meta = summary.inews ?? { stories: [], show: "" };
    const byFile = new Map((meta.stories ?? []).map((s) => [s.file_name, s]));
    return (summary.entries ?? []).map((dto, index) => {
      const item = itemFromDto(dto, index);
      const fields = dto[item.kind] ?? dto;
      const story = byFile.get(fields.file_name);
      if (story) {
        item.id = `inews-${story.key}`;
        // 換節最後同步、延遲補掃的稿屬於舊節次，不是這一輪的目前節次。
        item.inews = { key: story.key, version: story.version, show: story.show ?? meta.show };
      }
      return item;
    });
  }

  /** What a poll changed against the versions seen last time: new stories and edited ones. */
  function diffInewsVersions(previous, stories) {
    const added = [];
    const changed = [];
    for (const s of stories ?? []) {
      const was = previous?.get(s.key);
      if (was === undefined) added.push(s.key);
      else if (was !== s.version) changed.push(s.key);
    }
    return { added, changed };
  }

  function writtenKey(item) {
    return item.inews ? `${item.inews.key}|${item.inews.version}` : null;
  }

  /**
   * Ticked, not handled in the doc, and this exact version not yet written. Covers
   * both "waiting for keywords" and "waiting to be written".
   */
  function isPendingWrite(item, writtenKeys) {
    if (!canOutputBucket(item.bucket) || !item.included) return false;
    if (item.alreadyInDoc || item.matchStatus === "removed") return false;
    const key = writtenKey(item);
    return !(key && writtenKeys?.has(key));
  }

  /**
   * Merge a fresh poll into the cards on screen.
   *
   * A story whose iNEWS version is unchanged keeps its card as it is -- keywords,
   * edits, ticks, compare result. An edited story (new version) gets a fresh card,
   * since its script changed under whatever was generated for it. A story gone from
   * the fresh set is dropped, except one from an earlier show still waiting to be
   * written: switching show must not silently lose it.
   */
  function mergeInewsItems(previous, fresh, writtenKeys) {
    const prevById = new Map((previous ?? []).filter((p) => p.inews).map((p) => [p.id, p]));
    const freshIds = new Set(fresh.map((f) => f.id));
    const freshShows = new Set(fresh.filter((f) => f.inews).map((f) => f.inews.show));
    const merged = fresh.map((f) => {
      const p = prevById.get(f.id);
      return p && f.inews && p.inews.version === f.inews.version ? p : f;
    });
    const carried = (previous ?? []).filter(
      (p) => p.inews && !freshIds.has(p.id) && !freshShows.has(p.inews.show) && isPendingWrite(p, writtenKeys)
    );
    return [...carried, ...merged];
  }

  /**
   * Entries the automatic flow may write now: pending, compared against the doc, and
   * done with keywords -- they have some, or need none (勿上網, no body).
   */
  function autoWriteCandidates(items, writtenKeys, noUploadLabel) {
    return (items ?? []).filter(
      (i) =>
        isPendingWrite(i, writtenKeys) &&
        i.matchStatus != null &&
        i.keywordStatus !== "loading" &&
        ((i.keywords ?? "").trim() !== "" || (i.body ?? "").trim() === "" || isNoUpload(i, noUploadLabel))
    );
  }

  /**
   * 跨節合併設定的文字 ↔ 物件：`1200=1400, 1800=2000` ↔ `{ "1200": "1400", "1800": "2000" }`.
   * Typed with a Chinese IME, so full-width ＝，、 and → are accepted too.
   */
  function parseSlotSpans(text) {
    const spans = {};
    for (const part of String(text ?? "").split(/[,，、;；\s]+/)) {
      const m = part.trim().match(/^(\d{4})\s*(?:=|＝|->|→)\s*(\d{4})$/);
      if (m) spans[m[1]] = m[2];
      else if (part.trim()) return null;
    }
    return spans;
  }

  function formatSlotSpans(spans) {
    return Object.entries(spans ?? {}).map(([a, b]) => `${a}=${b}`).join(", ");
  }

  /**
   * Keeps the 已補掃 set (and its 節目分隔線) in step with one auto round. A show that
   * is current again is a new airing (tomorrow's 2000), so it is no longer closed:
   * without this an App left running overnight saw yesterday's sweep and wrote
   * `==== 20 ====` right after the first 2000 story of the day (v0.3.4).
   */
  function noteInewsRound(closed, programs, meta) {
    if (meta?.show && meta.show !== meta.closed_show) {
      closed.delete(meta.show);
      programs.delete(meta.show);
    }
    if (meta?.closed_show) {
      closed.add(meta.closed_show);
      if (meta.program_divider) programs.set(meta.closed_show, meta.program_divider);
      else programs.delete(meta.closed_show);
    }
  }

  /** `關鍵KS=關鍵時刻, XX=某節目` -> { 關鍵KS: "關鍵時刻", ... }; null on a malformed part. */
  function parsePrograms(text) {
    const programs = {};
    for (const part of String(text ?? "").split(/[,，、;；\n]+/)) {
      if (!part.trim()) continue;
      const m = part.match(/^\s*([^=＝]+?)\s*[=＝]\s*(.+?)\s*$/);
      if (!m) return null;
      programs[m[1]] = m[2];
    }
    return programs;
  }

  /** `==== {hh} ====` with `{hh}` = the show's hour (`0800` -> `08`) and `{show}` = its folder name. */
  function formatDivider(format, show) {
    const s = String(show ?? "");
    return String(format ?? "").replaceAll("{hh}", s.slice(0, 2)).replaceAll("{show}", s);
  }

  /**
   * The text one automatic write sends, show by show, with the 節次分隔線 placed so
   * that a show's divider only goes in once its delayed sweep is done (v0.3.1).
   *
   * The shared doc can only grow at the bottom, so anything the sweep picks up has to
   * be written before the divider -- and the next show's stories after it. So while
   * the show last written to (`lastShow`) is still open, stories of a newer show are
   * held back (returned in `held`) rather than written above the divider.
   *
   *   closed   -- shows whose sweep has completed (`closed_show` from the backend)
   *   pending  -- shows that still have entries waiting (keywords, compare) outside
   *               `items`; their divider waits for those too
   *   after    -- once `lastShow` is closed and nothing of it is pending, a divider
   *               naming it goes in, even if no new story comes with it
   *   before   -- a divider naming the new show goes in before its first story
   *   programs -- show -> 節目分隔線 (`==== 22 關鍵時刻 ====`) that follows that
   *               show's own divider (after-position only, v0.3.3)
   *
   * Stories of a show already closed and left behind arrive late (edited after the
   * sweep); they are written where they are, with no divider of their own.
   */
  function composeInewsWrite(items, { lastShow = "", closed = new Set(), pending = new Set(), programs = new Map() } = {}, { format, position }, toText) {
    const groups = [];
    for (const i of items) {
      const show = i.inews?.show ?? "";
      const g = groups.find((x) => x.show === show);
      if (g) g.items.push(i);
      else groups.push({ show, items: [i] });
    }
    // The show being written goes first: its sweep results belong above its divider.
    groups.sort((a, b) => (b.show === lastShow) - (a.show === lastShow));

    const parts = [];
    const written = [];
    const held = [];
    const dividers = [];
    const divider = (show) => {
      const d = formatDivider(format, show);
      const program = position !== "before" ? programs.get(show) : undefined;
      parts.push(program ? `${d}\n\n${program}` : d);
      dividers.push(d);
      if (program) dividers.push(program);
    };
    let current = lastShow ?? "";
    const done = (show) => closed.has(show) && !pending.has(show);
    const closeIfDone = () => {
      if (position !== "before" && current && done(current)) {
        divider(current);
        current = "";
      }
    };
    for (const g of groups) {
      const late = g.show && g.show !== current && closed.has(g.show);
      if (g.show && g.show !== current && !late) {
        if (current && !done(current)) {
          held.push(...g.items);
          continue;
        }
        if (position === "before") divider(g.show);
        else if (current) divider(current);
        current = g.show;
      }
      parts.push(toText(g.items));
      written.push(...g.items);
    }
    closeIfDone();
    return { text: parts.filter((p) => p.trim() !== "").join("\n\n"), lastShow: current, written, held, dividers };
  }

  return {
    inewsItemsFromSummary,
    diffInewsVersions,
    writtenKey,
    isPendingWrite,
    mergeInewsItems,
    autoWriteCandidates,
    formatDivider,
    parseSlotSpans,
    formatSlotSpans,
    parsePrograms,
    noteInewsRound,
    composeInewsWrite,
    eveningItemFromDto,
    isEveningHit,
    isEveningStyle,
    isEveningExcluded,
    eveningGroupSizes,
    splitIntoGroups,
    pickHeadline,
    groupKey,
    groupTagLine,
    eveningOutgoing,
    slashDate,
    mmddOf,
    buildEveningText,
    eveningGroups,
    buildEveningGroupHead,
    buildEveningGroupText,
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
