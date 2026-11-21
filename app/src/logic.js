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
  function decideInclusion(result, defaultIncluded, userIncluded) {
    // 使用者手動勾過（或取消過）就照使用者的（v0.3.15）：以前已剔除／已在文件的卡片
    // 每次比對都被強制取消勾選，手動勾的還沒按寫入就被洗掉。自動寫入本來就不寫這兩種
    // （isPendingWrite），所以這只影響手動寫入。
    if (typeof userIncluded === "boolean") return userIncluded;
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
        !isNoUpload(i, noUploadLabel) &&
        // TEL 稿不產生關鍵字（v0.3.15，使用者決定）。
        !isTelStyle(i) &&
        // 可疑稿（v0.3.11）不會自動寫，先不花額度產生關鍵字；放行後下一輪再產生。
        i.inews?.hold !== "suspect"
    );
  }

  /** 樣式是 TEL（大小寫、全形半形不分）。TEL 稿不產生關鍵字，也不用等關鍵字就能寫入。 */
  function isTelStyle(item) {
    const fields = item?.dto ? item.dto[item.kind] ?? item.dto : item;
    const style = String(fields?.style ?? item?.style ?? "")
      .replace(/[！-～]/g, (c) => String.fromCharCode(c.charCodeAt(0) - 0xfee0))
      .trim()
      .toUpperCase();
    return style === "TEL";
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
        // 逗號分隔的關鍵字行（v0.3.34）：空一行接在 # 那行下面，給公司另一套上傳系統用。
        const csv = i.keywords_csv ? keywordCsv(i.keywords) : "";
        return [head.join(" "), i.title, ...body, i.keywords, ...(csv ? ["", csv] : [])].join("\n");
      })
      .join("\n\n");
  }

  /** `#中和 #忠孝街 #瓦斯槍` → `中和,忠孝街,瓦斯槍`（v0.3.34）。不管原本用什麼分隔。 */
  function keywordCsv(keywords) {
    return String(keywords ?? "")
      .split(/[\s,，、]+/)
      .map((k) => k.replace(/^[#＃]+/, "").trim())
      .filter((k) => k !== "")
      .join(",");
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
      // 匯入時拿到的標題：同一天再匯入時，用來分辨標題是不是使用者改過的（v0.3.36）。
      importedTitle: f.title ?? "",
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
   *
   * 標題只留「使用者改過的」（v0.3.36）：11/19 18:07 先抓一次，晚報時段的稿還沒寫好、
   * 標題是空的；20:49 再抓拿到完整版，舊的空標題卻被當成編輯留下來。所以空標題、或跟
   * 上次匯入時一模一樣（沒改過）的標題，一律換成這次匯入的。
   */
  function mergeEveningEdits(fresh, previous) {
    const bySlug = new Map((previous ?? []).map((p) => [p.slug, p]));
    return fresh.map((item) => {
      const p = bySlug.get(item.slug);
      if (!p) return item;
      const prevTitle = String(p.title ?? "");
      const edited = prevTitle.trim() !== "" && (p.importedTitle === undefined || prevTitle !== p.importedTitle);
      return {
        ...item,
        title: edited ? prevTitle : item.title,
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
        // hold（v0.3.11）：這一輪不能自動寫入的原因；recheck：定時複查找到的漏寫稿。
        item.inews = {
          key: story.key,
          version: story.version,
          show: story.show ?? meta.show,
          hold: story.hold ?? null,
          recheck: !!story.recheck,
        };
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
   * 工具寫過、文件裡卻找不到的稿（v0.3.32）。11/17 淑麗報氣象0822 08:22 寫入，08:32 前後
   * 連同下一則一起從文件消失；工具記得「這一版寫過」就再也不寫。比對結果是待處理、而且
   * 沒有對到任何一行（真的不在今天的段落裡），連續 `rounds` 次比對都這樣才放行重寫 ——
   * 剛寫完文件還沒同步過來的那一兩輪不算。
   *
   *   streaks -- 上一次的 Map key -> 連續找不到的次數
   * 回傳 { release: 要重寫的 key, streaks: 新的 Map }。中間有一次找到就重新算。
   */
  function vanishedWrites(items, writtenKeys, streaks, rounds) {
    const next = new Map();
    const release = [];
    for (const i of items ?? []) {
      const key = writtenKey(i);
      if (!key || !writtenKeys?.has(key) || next.has(key) || release.includes(key)) continue;
      if (i.matchStatus !== "to_cut" || i.matchedLine) continue;
      const n = (streaks?.get(key) ?? 0) + 1;
      if (n >= rounds) release.push(key);
      else next.set(key, n);
    }
    return { release, streaks: next };
  }

  /**
   * 放行重寫（v0.3.39）：寫入時卡片被設成「不勾選、已在文件」（defaultIncluded 也是 false），
   * 只刪 writtenKeys 的話，下一次比對還是算成不勾選，永遠不會自動重寫 —— 11/20 龜殼花咬人1800、
   * 顯卡記憶漲1800 08:39 記了 [重寫]，一直到 09:36 有人手動寫入才補上。
   */
  function reopenForRewrite(item) {
    if (!item) return;
    item.defaultIncluded = true;
    item.included = true;
    item.alreadyInDoc = false;
    delete item.userIncluded;
  }

  /**
   * 放行重寫時，把「這次 session 寫過」的記錄全部拿掉（v0.3.45）：版本 key、內容指紋，
   * 還有 writtenSlugs 的「slug｜版本」。以前漏了 writtenSlugs：寫入前那道 freshForWrite 會把
   * 這則默默濾掉、不寫也不記；它又還算「待寫」，上一節永遠收不了尾，下一節整批卡住，
   * 直到有人手動寫入或重開 App（11/21 07:58～08:14 的 0800、14:05～14:18 的 1400；
   * 11/20 09 卡 30 分鐘推測也是）。rec：{ writtenKeys, writtenSlugs, writtenPrints }。
   */
  function releaseForRewrite(key, item, rec, slugOf) {
    rec.writtenKeys.delete(key);
    if (item) {
      rec.writtenPrints.delete(contentPrint(item));
      rec.writtenSlugs.delete(slugOf(item));
      rec.writtenSlugs.delete(`${slugOf(item)}|${item.inews?.version ?? ""}`);
    }
    reopenForRewrite(item);
  }

  /**
   * 這一輪「有稿在等、卻什麼都沒寫」時，逐則說明在等什麼（v0.3.39）。11/20 09:06～09:36 0900
   * 整節沒寫，紀錄只有比對，查不到卡在哪。回傳 [{ slug, why }]，只列還沒寫、這一輪也不能寫的。
   */
  function stuckReasons(items, ready, writtenKeys, opts = {}) {
    const inReady = new Set(ready ?? []);
    const why = (i) =>
      i.inews?.hold === "suspect" ? "可疑稿，等人工確認"
      : i.inews?.hold === "cooldown" ? "rundown 時間異常，暫停"
      : i.inews?.hold ? "等下一輪確認"
      : i.matchStatus == null ? "等比對"
      : i.keywordStatus === "loading" ? "關鍵字產生中"
      : i.keywordStatus === "error" ? `關鍵字失敗：${String(i.keywordError ?? "").slice(0, 80)}`
      : (i.keywords ?? "").trim() !== "" ? "等寫入"
      : !opts.hasApiKey ? "沒有 Gemini API key"
      : opts.cooldownSeconds > 0 ? "等關鍵字（額度冷卻中）"
      : "等關鍵字";
    return (items ?? [])
      .filter((i) => isPendingWrite(i, writtenKeys) && !inReady.has(i) && !(opts.skip && opts.skip(i)))
      .map((i) => ({ slug: String((i.dto?.[i.kind] ?? i.dto ?? {}).slug ?? i.slug ?? ""), why: why(i) }));
  }

  /**
   * 下一節的稿被擋住、在等上一節（`ctx.lastShow`）時寫進決策紀錄的那一行（v0.3.41）。
   * 以前 app.js 的 heldReason 自己判斷每則在等什麼，跟 stuckReasons 的用字已經分歧
   * （冷卻中記成「等確認」、關鍵字產生中記成「等關鍵字」）；現在原因一律由 stuckReasons 決定，
   * 這裡只決定開頭：上一節還沒補掃完是「延遲補掃完成」，補掃完了是「收尾：」逐則列原因。
   * ctx：{ lastShow, closed: Set, hasApiKey, cooldownSeconds }。
   */
  function heldReasonLine(held, ready, items, writtenKeys, ctx) {
    const show = ctx.lastShow;
    const heldShows = [...new Set((held ?? []).map((i) => i.inews?.show))].join("、");
    const n = (held ?? []).length;
    if (!ctx.closed?.has(show)) return `[等待寫入] ${n} 則 ${heldShows} 的稿等 ${show} 節延遲補掃完成`;
    const sameShow = (items ?? []).filter(
      (i) => i.inews?.show === show && i.inews.hold !== "suspect" && !i.inews.recheck && !i.inews.manual
    );
    const list = stuckReasons(sameShow, ready, writtenKeys, { hasApiKey: ctx.hasApiKey, cooldownSeconds: ctx.cooldownSeconds });
    return (
      `[等待寫入] ${n} 則 ${heldShows} 的稿等 ${show} 節收尾：` +
      (list.length ? list.map((r) => `${r.slug}（${r.why}）`).join("、") : "沒有找到在等的稿")
    );
  }

  // --- 從 app.js 搬過來的寫入判斷（v0.3.44）。app.js 只帶入狀態、呼叫這些。---

  /** `07:00` → 420；格式不對（含空白）→ null。 */
  function hhmmToMinutes(text) {
    const m = /^(\d{1,2}):(\d{2})$/.exec(String(text ?? "").trim());
    return m ? Number(m[1]) * 60 + Number(m[2]) : null;
  }

  /**
   * 播出日裡的第幾分鐘（播出日從 `dayStartText` 開始，例 02:00）。設定格式錯時退回 02:00
   * （設定頁會擋，這裡只防壞掉的 config.toml）；預設值本身在後端。
   */
  function broadcastMinute(minutes, dayStartText) {
    const dayStart = hhmmToMinutes(dayStartText) ?? 120;
    return (((minutes - dayStart) % 1440) + 1440) % 1440;
  }

  /** 每天開始寫入的時間在播出日裡的第幾分鐘；沒設（空白）→ null（不限制）。cfg：{ broadcast_day_start, write_start }。 */
  function writeStartMinute(cfg) {
    const start = hhmmToMinutes(cfg?.write_start);
    return start === null ? null : broadcastMinute(start, cfg?.broadcast_day_start);
  }

  /** 現在（當天第幾分鐘）還沒到每天開始寫入的時間（v0.3.26）。 */
  function isBeforeWriteStart(nowMinutes, cfg) {
    const start = writeStartMinute(cfg);
    return start !== null && broadcastMinute(nowMinutes, cfg?.broadcast_day_start) < start;
  }

  /** 這張 iNEWS 卡片屬於開始寫入時間以前的節次（例：開始時間 07:00，0600 節的稿）。手動載入的不算。 */
  function isBeforeWriteStartShow(item, cfg) {
    const start = writeStartMinute(cfg);
    const show = item?.inews?.show ?? "";
    if (start === null || !/^\d{4}$/.test(show) || item.inews?.manual) return false;
    return broadcastMinute(Number(show.slice(0, 2)) * 60 + Number(show.slice(2)), cfg?.broadcast_day_start) < start;
  }

  /**
   * 已收尾的節次欠著分隔線、即使沒有稿要一起寫（state：{ lastShow, closed, programs }）。
   * 跟寫入時用同一個 composeInewsWrite 算，空的一批算得出文字就是欠著。
   */
  function isDividerOwed(items, writtenKeys, state, divider, toText) {
    if (!state?.lastShow || !state.closed?.has(state.lastShow)) return false;
    const pending = pendingShows(items, [], writtenKeys);
    return composeInewsWrite([], { lastShow: state.lastShow, closed: state.closed, pending, programs: state.programs }, divider, toText).text.trim() !== "";
  }

  /** 這次寫入前要不要讀預列清單（v0.3.23）：後端說今天段落還空著，而且這個播出日還沒補過。 */
  function needsPrelist(wanted, day, doneFor) {
    return !!wanted && !!day && doneFor !== day;
  }

  /**
   * 寫入結束後「哪個播出日已經補過預列」（v0.3.44）。以前一讀到預列清單就記成補過，那次寫入
   * 若沒寫成（未就緒、逾時、失敗），當天就再也不補。改成寫入成功才記；沒寫成維持原樣，
   * 下一輪再讀一次。文件裡已經有今天的內容（含別人先補的預列）時，比對和後端寫入前都會判斷
   * 「不需要補」，不會重複。attemptedFor：這次寫入有讀預列清單的播出日（沒讀就是空字串）。
   */
  function prelistDoneAfterWrite(doneFor, attemptedFor, wrote) {
    return wrote && attemptedFor ? attemptedFor : doneFor;
  }

  /**
   * 讀到的預列清單 → 要寫進文件的行（`(X)slug<Tab>樣式`），並標出這一批裡剛好是預列稿的
   * （寫入時加 (X)，會設 `prelisted = true`）。
   */
  function applyPrelist(list, outgoing, slugOf) {
    const slugs = new Set((list ?? []).map((p) => String(p.slug ?? "").trim()));
    for (const i of outgoing ?? []) if (slugs.has(String(slugOf(i) ?? "").trim())) i.prelisted = true;
    return (list ?? []).map((p) => `(X)${p.slug}${p.style ? `\t${p.style}` : ""}`);
  }

  /** 自動寫入前再擋一次：同一個版本（或這次 session 寫過的 slug｜版本）不重複寫。 */
  function freshForWrite(ready, writtenKeys, writtenSlugs, slugOf) {
    return (ready ?? []).filter(
      (i) => isPendingWrite(i, writtenKeys) && !writtenSlugs.has(`${slugOf(i)}|${i.inews?.version ?? ""}`)
    );
  }

  /**
   * 寫入成功後的狀態更新（自動、手動共用；v0.3.44 起手動寫入也走這裡）：記下寫過的版本、slug、稿（疑似誤寫用）、內容指紋，並把卡片
   * 改成「已在文件、不勾選」—— 下一輪比對也會得到同樣的結果。rec：{ writtenKeys, writtenSlugs,
   * writtenStories, writtenPrints }（都會被改）。
   */
  function recordAutoWrite(written, rec, slugOf) {
    for (const i of written ?? []) {
      const key = writtenKey(i);
      if (key) rec.writtenKeys.add(key);
      rec.writtenSlugs.add(slugOf(i));
      rec.writtenSlugs.add(`${slugOf(i)}|${i.inews?.version ?? ""}`);
      if (i.inews) rec.writtenStories.set(i.inews.key, { slug: slugOf(i), show: i.inews.show });
      const print = contentPrint(i);
      if (print) rec.writtenPrints.add(print);
      i.included = false;
      i.defaultIncluded = false;
      delete i.userIncluded;
      i.alreadyInDoc = true;
    }
  }

  /** 寫進決策紀錄的一行：幾則、哪些 slug、分隔線、日期分隔線。 */
  function writeLogNote(written, dividers, dateHeader, slugOf) {
    const slugs = (written ?? []).map(slugOf);
    return (
      `${slugs.length} 則` +
      (slugs.length ? `（${slugs.join("、")}）` : "") +
      ((dividers ?? []).length ? `｜分隔線 ${dividers.join("、")}` : "") +
      (dateHeader ? `｜日期分隔線 ${dateHeader}` : "")
    );
  }

  /** 畫面上「等上一節補掃完」的說明。 */
  function heldText(held, lastShow, sweepDelayMinutes) {
    const shows = [...new Set((held ?? []).map((i) => i.inews?.show))].join("、");
    return `${(held ?? []).length} 則 ${shows} 的稿等 ${lastShow} 節延遲補掃完成、寫完分隔線後再寫入（換節次後 ${sweepDelayMinutes} 分鐘）。`;
  }

  /**
   * 同一種狀況只記一次（v0.3.40）：跟上次記的一樣就回 ""（不記）；`line` 傳空字串表示狀況解除，
   * 下次再發生會再記。`last` 是 Map（kind → 上次的內容），會被改。回傳要記的那一行。
   */
  function noteChange(last, kind, line) {
    if ((last.get(kind) ?? "") === line) return "";
    last.set(kind, line);
    return line;
  }

  /** 協作工具還在載入或沒連上時，讀取文件的腳本回傳的錯誤帶這個記號（v0.3.15）。 */
  const COLLAB_NOT_READY = "[未就緒]";
  function isCollabNotReady(err) {
    return String(err).includes(COLLAB_NOT_READY);
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
   *
   * The 累積時間 moves without the version changing when the rundown is retimed. A
   * kept card not yet written takes the fresh time, so it is written with the latest
   * one; a written one is left alone -- the doc line is fixed by 時間校正 (v0.3.9).
   *
   * The hold (等確認 / 冷卻 / 可疑, v0.3.11) always comes from the fresh poll, and a
   * held card is never carried: it was held because this round could not be trusted,
   * so once it is gone from the rundown it is gone.
   */
  function mergeInewsItems(previous, fresh, writtenKeys) {
    const prevById = new Map((previous ?? []).filter((p) => p.inews).map((p) => [p.id, p]));
    const freshIds = new Set(fresh.map((f) => f.id));
    const freshShows = new Set(fresh.filter((f) => f.inews).map((f) => f.inews.show));
    const merged = fresh.map((f) => {
      const p = prevById.get(f.id);
      if (!(p && f.inews && p.inews.version === f.inews.version)) return f;
      p.inews.hold = f.inews.hold ?? null;
      p.inews.recheck = !!f.inews.recheck;
      if (p.time !== f.time && !writtenKeys?.has(writtenKey(p))) {
        p.time = f.time;
        p.dto = f.dto;
      }
      return p;
    });
    const carried = (previous ?? []).filter(
      (p) =>
        p.inews && !p.inews.hold && !freshIds.has(p.id) && !freshShows.has(p.inews.show) && isPendingWrite(p, writtenKeys)
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
        !i.inews?.hold &&
        i.matchStatus != null &&
        i.keywordStatus !== "loading" &&
        ((i.keywords ?? "").trim() !== "" ||
          (i.body ?? "").trim() === "" ||
          isNoUpload(i, noUploadLabel) ||
          isTelStyle(i))
    );
  }

  /**
   * 手動載入的卡片（v0.3.15，搜尋 rundown 點選載入）：只活在載入時的那一節。每一輪合併
   * 完，把還屬於目前節次、這一輪沒出現的手動卡片接回來；換節就不再接，自然清掉。
   * 這一輪的 rundown 裡也有同一則（id 相同）時，以合併的結果為準。
   */
  function keepManualCards(merged, previous, currentShow) {
    const ids = new Set((merged ?? []).map((i) => i.id));
    const kept = (previous ?? []).filter(
      (p) => p.inews?.manual && p.inews.manualFor === currentShow && !ids.has(p.id)
    );
    return [...(merged ?? []), ...kept];
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

  /**
   * 重開後還沒寫過任何一則（lastShow 是空的）時，補掃帶節目分隔線的那一節要當成「寫到
   * 這一節」，不然節目分隔線永遠不會寫（v0.3.37）。11/19 22:03 重開，22:10 補掃 2000、
   * 寫了 出國更划算2100，但 `==== 22 關鍵時刻 ====` 沒寫：2000 被當成「晚到的稿」寫在
   * 最下面，收尾時沒有「目前節次」可以收。已經在文件裡的分隔線，寫入前比對會濾掉。
   */
  function resumeShowForProgram(lastShow, meta) {
    if (lastShow) return lastShow;
    return meta?.closed_show && meta.program_divider ? meta.closed_show : "";
  }

  /**
   * 寫入進度存檔（v0.3.38）：上一則寫到哪一節、哪些節次已收尾、收尾後要接的節目分隔線。
   * 原本只在記憶體，重開 App 就沒了（11/19 22:03 重開，`==== 22 關鍵時刻 ====` 沒寫）。
   */
  function writeSessionSnapshot(day, lastShow, closed, programs, prints) {
    return {
      version: 1,
      day: String(day ?? ""),
      lastShow: String(lastShow ?? ""),
      closed: [...(closed ?? [])],
      programs: Object.fromEntries(programs ?? []),
      // 寫過的稿的內容指紋（v0.3.41，見 contentPrint）：重開後也認得出已經寫過的稿。
      prints: [...(prints ?? [])],
    };
  }

  /** FNV-1a 32 位元，十六進位。只拿來比「內容一不一樣」，不是安全用途。 */
  function fnv1a(text) {
    let h = 0x811c9dc5;
    for (let k = 0; k < text.length; k++) {
      h ^= text.charCodeAt(k);
      h = Math.imul(h, 0x01000193) >>> 0;
    }
    return h.toString(16).padStart(8, "0");
  }

  /**
   * 一則稿的內容指紋（v0.3.41）：`節次|slug|標題＋內文的雜湊`。iNEWS 的 story key（連版本）
   * 每次存檔、搬稿都會換，「key|版本」記得的「寫過了」會失效（AUDIT_REPORT #5）；指紋只看
   * 寫出去的東西，內容沒變就認得出來，內容改了（記者改稿）就是新的指紋、照常寫。
   * 沒有 iNEWS 資訊（手動匯入的資料夾卡片）回空字串，不參與。
   */
  function contentPrint(item) {
    if (!item?.inews) return "";
    const slug = String((item.dto?.[item.kind] ?? item.dto ?? {}).slug ?? item.slug ?? "").trim();
    const text = `${String(item.title ?? "").trim()}\n${String(item.body ?? "").trim()}`;
    return `${item.inews.show ?? ""}|${slug}|${fnv1a(text)}`;
  }

  /**
   * 還在等寫入、但同一節同一個 slug、內容一模一樣的稿今天已經寫過（只是 iNEWS 換了 key）：
   * 當作已寫過，把它的 key 記進 writtenKeys（v0.3.41）。回傳被認回的卡片，給決策紀錄用。
   * 要重貼（keep_refresh，文件裡是抓新行）的不認：那是製作人要求重寫。
   * 一定要在產生關鍵字之前跑，不然會白花額度。
   */
  function adoptWrittenPrints(items, writtenKeys, prints) {
    const adopted = [];
    if (!prints?.size) return adopted;
    for (const i of items ?? []) {
      if (!isPendingWrite(i, writtenKeys) || i.matchStatus === "keep_refresh") continue;
      const p = contentPrint(i);
      const key = writtenKey(i);
      if (!p || !key || !prints.has(p)) continue;
      writtenKeys.add(key);
      adopted.push(i);
    }
    return adopted;
  }

  /**
   * 重開後第一輪把存檔接回來；不是同一個播出日、格式不對就回 null（照舊從頭）。
   *
   * 「上一則寫到的節次」要還會收尾才接：已收尾、是目前節次、是上一個有效節次，或後端接續的
   * 就是它。App 關了好幾節才重開，那一節不會再補掃，接回來的話它永遠收不了尾，後面每一節
   * 的稿都會被擋住，所以這種情況照舊當成沒寫過。
   */
  function restoreWriteSession(saved, meta) {
    if (!saved || saved.version !== 1 || !meta?.day || saved.day !== meta.day) return null;
    const closed = Array.isArray(saved.closed) ? saved.closed.filter((s) => typeof s === "string" && s) : [];
    const programs = Object.entries(saved.programs && typeof saved.programs === "object" ? saved.programs : {}).filter(
      ([show, text]) => closed.includes(show) && typeof text === "string" && text
    );
    let lastShow = typeof saved.lastShow === "string" ? saved.lastShow : "";
    const willClose =
      closed.includes(lastShow) || lastShow === meta.show || lastShow === meta.previous_show || lastShow === meta.resumed_show;
    if (!willClose) lastShow = "";
    const prints = Array.isArray(saved.prints) ? saved.prints.filter((s) => typeof s === "string" && s) : [];
    return { lastShow, closed, programs, prints };
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
   *   programs -- show -> 節目分隔線 (`==== 22 關鍵時刻 ====`) for the off-air
   *               program that follows that show. After-position: right after the
   *               show's own divider (v0.3.3). Before-position: on its own once the
   *               show is closed, since it names what airs next (v0.3.5)
   *
   * Stories of a show already closed and left behind arrive late (edited after the
   * sweep); they are written where they are, with no divider of their own.
   */
  /**
   * 還有稿在等（關鍵字、比對）的節次：它們的分隔線要等，不然漏網的稿會落到下一節的分隔線
   * 下面。定時複查撿回來的、手動載入的卡片本來就寫在最下面，不擋（v0.3.35：11/19 複查撿回
   * 的 1200 卡片寫不出去，1200 一直收不了尾，1400 整節等到 15:05 才寫）。可疑稿也不擋。
   */
  function pendingShows(items, ready, writtenKeys) {
    const inReady = new Set(ready ?? []);
    return new Set(
      (items ?? [])
        .filter(
          (i) =>
            i.inews && !inReady.has(i) && isPendingWrite(i, writtenKeys) &&
            i.inews.hold !== "suspect" && !i.inews.recheck && !i.inews.manual
        )
        .map((i) => i.inews.show)
    );
  }

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
    // The same pieces as `parts`, keeping which stories each one holds (v0.3.14): the
    // write-time re-check judges story by story and cannot rebuild that from the joined
    // text -- one story is "head+title", a blank line, then "body+keywords".
    const pieces = [];
    const written = [];
    const held = [];
    const dividers = [];
    const push = (text, stories) => {
      parts.push(text);
      pieces.push(stories ? { text, items: stories } : { text });
    };
    const divider = (show) => {
      const d = formatDivider(format, show);
      const program = position !== "before" ? programs.get(show) : undefined;
      push(program ? `${d}\n\n${program}` : d);
      dividers.push(d);
      if (program) dividers.push(program);
    };
    let current = lastShow ?? "";
    const done = (show) => closed.has(show) && !pending.has(show);
    const closeIfDone = () => {
      if (!current || !done(current)) return;
      if (position !== "before") {
        divider(current);
        current = "";
      } else if (programs.has(current)) {
        // An off-air program has no story of its own to put a before-divider over.
        const program = programs.get(current);
        push(program);
        dividers.push(program);
        current = "";
      }
    };
    for (const g of groups) {
      // 定時複查找到的漏寫稿（v0.3.11）也一樣：那一節早就結束了，直接寫在最下面。
      // 手動載入的卡片（v0.3.15）也一樣寫在最下面、不寫分隔線。
      const late =
        g.show && g.show !== current && (closed.has(g.show) || g.items.every((i) => i.inews?.recheck || i.inews?.manual));
      if (g.show && g.show !== current && !late) {
        if (current && !done(current)) {
          held.push(...g.items);
          continue;
        }
        if (position === "before") divider(g.show);
        else if (current) divider(current);
        current = g.show;
      }
      push(toText(g.items), g.items);
      written.push(...g.items);
    }
    closeIfDone();
    return {
      text: parts.filter((p) => p.trim() !== "").join("\n\n"),
      pieces: pieces.filter((p) => p.text.trim() !== ""),
      lastShow: current,
      written,
      held,
      dividers,
    };
  }

  /**
   * What the write-time re-check receives (v0.3.14): one part per story (with its slug)
   * and one per divider. Their texts joined by a blank line are exactly the text that
   * would have been written. `toText` renders a list of stories, as for
   * `composeInewsWrite`; a story it renders as nothing (unticked) is left out.
   */
  function writeParts(pieces, slugOf, toText) {
    const parts = [];
    for (const p of pieces ?? []) {
      if (!p.items) {
        if ((p.text ?? "").trim() !== "") parts.push({ text: p.text });
        continue;
      }
      for (const item of p.items) {
        const text = toText([item]);
        if ((text ?? "").trim() !== "") parts.push({ slug: slugOf(item), text });
      }
    }
    return parts;
  }

  /** 重播樣式（v0.3.31）：比對後今天文件已有同 slug 就略過，不跳審查。 */
  const RERUN_STYLES = ["重播"];

  function isRerunStyle(item) {
    const fields = item?.dto ? item.dto[item.kind] ?? item.dto : item;
    return RERUN_STYLES.includes(String(fields?.style ?? item?.style ?? "").trim());
  }

  /**
   * 未知樣式強制審查（v0.3.15）：還沒審查、也還沒排進佇列的未知樣式 iNEWS 卡片，依畫面
   * 順序。key 是 story key＋版本：稿子內容改過（新版本）要重新審查。
   *
   * 重播樣式（v0.3.31）要等比對完才決定，邏輯同一般 SOT：今天的段落有同 slug，是抓新行
   * 而且要重抓（不同節）→ `skip: true, write: true`，照抓新寫入；其他（乾淨一行、有標記、
   * 抓新但同一節）→ `skip: true`，略過不寫。都不審查。今天找不到（包括只有前一天有）→
   * 照舊審查；還沒比對 → 先不排。
   */
  function unknownReviewTargets(items, seen) {
    const out = [];
    for (const i of items ?? []) {
      if (i.bucket !== "unknown" || !i.inews) continue;
      const key = `${i.inews.key}|${i.inews.version}`;
      if (seen?.has(key) || out.some((o) => o.key === key)) continue;
      if (isRerunStyle(i)) {
        if (i.matchStatus == null) continue;
        if (i.matchedLine && !i.crossDay) {
          out.push(i.matchStatus === "keep_refresh" ? { key, id: i.id, skip: true, write: true } : { key, id: i.id, skip: true });
          continue;
        }
      }
      out.push({ key, id: i.id });
    }
    return out;
  }

  /** A card held back this round (等確認 / 冷卻 / 可疑) -> its label, or "" (v0.3.11). */
  function holdLabel(item) {
    return { confirm: "⏳ 等下一輪確認", cooldown: "⏸ rundown 時間異常，暫停寫入", suspect: "⚠ 可疑：時間異常，不寫入" }[
      item?.inews?.hold ?? ""
    ] ?? "";
  }

  /**
   * 疑似誤寫 (v0.3.11): stories this session wrote to the doc whose show the 定時複查
   * just read again, and which are no longer aired there -- what a rundown read in the
   * middle of a move looks like afterwards. Only reported; the doc is never edited.
   *
   *   written -- Map story key -> { slug, show } written this session
   *   recheck -- `meta.recheck`: { shows, aired_keys, aired_slugs }
   *
   * v0.3.35：iNEWS 的 story key 每次存檔、搬稿都會換，只比 key 會把播完又被製作人存檔
   * 或搬到後面節次重播的稿全報成疑似誤寫（11/19 一次報 20 則）。key 還在、或同一節還有
   * 同 slug 的已播出列，都不算。
   */
  function findMisfires(written, recheck) {
    if (!recheck) return [];
    const shows = new Set(recheck.shows ?? []);
    const aired = new Set(recheck.aired_keys ?? []);
    const airedSlugs = new Set(recheck.aired_slugs ?? []);
    return [...(written ?? new Map()).entries()]
      .filter(([key, w]) => shows.has(w.show) && !aired.has(key) && !airedSlugs.has(`${w.show}|${String(w.slug ?? "").trim()}`))
      .map(([key, w]) => ({ key, ...w }));
  }

  return {
    writeParts,
    unknownReviewTargets,
    vanishedWrites,
    reopenForRewrite,
    stuckReasons,
    heldReasonLine,
    hhmmToMinutes,
    broadcastMinute,
    writeStartMinute,
    isBeforeWriteStart,
    isBeforeWriteStartShow,
    isDividerOwed,
    needsPrelist,
    applyPrelist,
    freshForWrite,
    recordAutoWrite,
    prelistDoneAfterWrite,
    releaseForRewrite,
    writeLogNote,
    heldText,
    noteChange,
    COLLAB_NOT_READY,
    isCollabNotReady,
    keywordCsv,
    pendingShows,
    isRerunStyle,
    isTelStyle,
    keepManualCards,
    holdLabel,
    findMisfires,
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
    resumeShowForProgram,
    writeSessionSnapshot,
    restoreWriteSession,
    contentPrint,
    adoptWrittenPrints,
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
