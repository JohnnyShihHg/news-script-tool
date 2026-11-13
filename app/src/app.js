const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
// Pure decision rules live in logic.js so they can be unit-tested without a DOM;
// this file keeps only wiring, rendering and IPC. See app/tests/logic.test.js.
const {
  itemFromDto,
  sortByTime,
  sortForDisplay,
  isAlreadyInDoc,
  isNoUpload,
  canOutputBucket,
  isOutputBucket,
  decideInclusion,
  summarizeMatches,
  selectKeywordTargets,
  splitKeywordRun,
  isRateLimitError,
  computeFunnel,
  outgoingItems: outgoingOf,
  buildOutputText,
  eveningItemFromDto,
  isEveningHit,
  isEveningStyle,
  isEveningExcluded,
  sortByTime: sortEvening,
  groupKey,
  groupTagLine,
  buildEveningGroupHead,
  mmddOf,
  buildEveningText,
  eveningGroups,
  buildEveningGroupText,
  buildEveningSlugList,
  buildEveningExport,
  mergeEveningEdits,
  inewsItemsFromSummary,
  holdLabel,
  findMisfires,
  writeParts,
  unknownReviewTargets,
  keepManualCards,
  diffInewsVersions,
  writtenKey,
  isPendingWrite,
  mergeInewsItems,
  autoWriteCandidates,
  composeInewsWrite,
  parseSlotSpans,
  formatSlotSpans,
  parsePrograms,
  noteInewsRound,
} = window.AppLogic;

/** @type {Array<{dto: any, id: string, included: boolean, title: string, body: string, keywords: string, keywordStatus: string, keywordError: string}>} */
let items = [];
let activeFilter = "all";
let currentFolder = "";
let hasApiKey = false;
/** True once a compare has run against the current card set; reset on every import. */
let hasCompared = false;
/** Slugs this session has already pushed into the shared doc, so a stale outline
 *  (it re-renders on a debounce) can never let the same entry be written twice. */
const writtenSlugs = new Set();
/** Today's date divider, owed before the next write because the last compare found
 *  no section for today yet (v0.3.2). Cleared once a write carries it. */
let dateHeaderOwed = "";
/** Day (MMDD) of the last compare, and the day whose divider this session already
 *  wrote: the scraped doc can lag behind a write, and a stale compare must not ask
 *  for the same divider twice. */
let compareDay = "";
/** The last compare could not find yesterday's section: auto-write waits for a click. */
let compareHoldsAutoWrite = false;
let dateHeaderWrittenFor = "";

function dateHeaderWritten() {
  if (dateHeaderOwed) dateHeaderWrittenFor = compareDay;
  dateHeaderOwed = "";
}
const KEYWORD_COUNT = 4;
/** Max entries one keyword run may send; overwritten from config on startup.
 *  Matches the free tier's per-minute request cap. 0 = no cap. */
let keywordMaxPerRun = 15;
/** The configured 勿上網 label, overwritten from config on startup. Entries carrying
 *  it are kept out of batch keyword runs -- see selectKeywordTargets in logic.js. */
let noUploadLabel = "(勿上網)";

const el = (id) => document.getElementById(id);

const slugOf = (item) => (item.dto[item.kind] ?? item.dto).slug;

function escapeHtml(s) {
  return (s ?? "").replace(/[&<>"']/g, (c) => ({
    "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;",
  }[c]));
}

// Minimal word-ish diff: highlight the whole old/new strings when they differ.
// Punctuation normalization changes individual characters throughout the string,
// so a char-level diff is noisier to read than just showing before/after in full.
function renderDiff(before, after) {
  if (before === after) return null;
  return `<div class="diff-box"><span class="diff-old">${escapeHtml(before)}</span>\n<span class="diff-new">${escapeHtml(after)}</span></div>`;
}

/// 匯入資料夾 always reads the folder bound in settings (預設匯入資料夾) -- there is
/// deliberately no second "pick a folder" path, so which folder gets imported is
/// decided in exactly one place. With nothing bound yet it opens settings to bind one.
async function importConfiguredFolder() {
  if (!currentFolder) {
    await openSettings();
    el("settingsStatus").textContent = "請先設定「預設匯入資料夾」並儲存，之後按「匯入資料夾」就會直接匯入。";
    el("defaultFolderInput")?.focus();
    return;
  }
  await importFolder(currentFolder);
}

/// The one import path shared by startup and 匯入資料夾. A folder that has since been
/// deleted/renamed only surfaces on the label, so it can be re-bound in settings.
async function importFolder(folder) {
  setCurrentFolder(folder);
  try {
    const summary = await invoke("import_folder", { folder });
    loadSummary(summary);
    await runAutoPipeline();
  } catch (err) {
    el("folderLabel").textContent = `${folder}（匯入失敗：${err}）`;
  }
}

function setCurrentFolder(folder) {
  currentFolder = folder;
  el("folderLabel").textContent = folder;
  el("clearFolderBtn").disabled = !folder;
  el("importBtn").title = folder ? `匯入：${folder}` : "請先到設定綁定預設匯入資料夾";
}

/// Clears the on-screen cards only. Deliberately separate from 清空資料夾, which
/// deletes files on disk -- this one touches nothing outside the UI.
function clearCards() {
  clearCooldown();
  items = [];
  // The next iNEWS poll then sees every story as new again; writtenKeys stays, so
  // nothing already written goes in twice.
  inewsVersions = new Map();
  hideInewsConfirm();
  hasCompared = false;
  el("summary").classList.add("hidden");
  el("emptyState").classList.remove("hidden");
  el("list").innerHTML = "";
  el("compareBtn").disabled = true;
  el("copyBtn").disabled = true;
  el("saveBtn").disabled = true;
  el("writeBackBtn").disabled = true;
  el("clearCardsBtn").disabled = true;
  el("compareStatus").classList.add("hidden");
  el("funnel").classList.add("hidden");
  hideProgress();
}

async function clearFolder() {
  if (!currentFolder) return;
  const ok = window.confirm(`確定要永久刪除「${currentFolder}」內所有 .txt 檔案嗎？此動作無法復原。`);
  if (!ok) return;
  const btn = el("clearFolderBtn");
  btn.disabled = true;
  try {
    const deleted = await invoke("clear_folder", { folder: currentFolder });
    window.alert(`已刪除 ${deleted} 個 .txt 檔案。`);
    // Reuse the single reset path so no piece of state (funnel, compare flag,
    // progress bar) can be forgotten here but remembered there.
    clearCards();
  } catch (err) {
    window.alert(`清空資料夾失敗：${err}`);
  } finally {
    btn.disabled = !currentFolder;
  }
}

const THEMES = [
  { id: "light", name: "預設（白底黑字）" },
  { id: "dark", name: "深色" },
  { id: "warm", name: "暖色護眼" },
  { id: "slate", name: "石墨藍" },
  { id: "contrast", name: "高對比" },
];

/// The theme id lives in config.toml, not browser storage, so it stays put across
/// restarts and only changes when someone saves a different one in settings.
function applyTheme(theme) {
  document.documentElement.dataset.theme = theme && theme !== "light" ? theme : "";
}

/// Applies the saved theme, then loads the configured default folder and imports it
/// straight away so the usual flow (open app -> cards are already there) needs no
/// clicking. A folder that has since been deleted/renamed must not block startup;
/// importFolder only surfaces the failure on the label.
async function initFromConfig() {
  let config;
  try {
    config = await invoke("get_config");
  } catch (err) {
    console.error("讀取設定失敗", err);
    return;
  }
  applyTheme(config.ui?.theme);
  if (Number.isFinite(config.gemini?.max_per_run)) keywordMaxPerRun = config.gemini.max_per_run;
  noUploadLabel = config.annotations?.no_upload_label ?? noUploadLabel;
  applyInewsConfig(config);
  const folder = config.import?.default_folder ?? "";
  if (!folder) return;

  await importFolder(folder);
}

// --- Progress bar (driven by the backend's per-item "keyword-progress" event; it
// counts loop iterations locally and costs no extra API calls) ---

function showProgress(done, total, label) {
  el("progressWrap").classList.remove("hidden");
  const pct = total > 0 ? Math.round((done / total) * 100) : 0;
  el("progressFill").style.width = `${pct}%`;
  el("progressLabel").textContent = label ?? `${done} / ${total}`;
}

function hideProgress() {
  el("progressWrap").classList.add("hidden");
  el("progressFill").style.width = "0";
  el("progressLabel").textContent = "";
}

listen("keyword-progress", (event) => {
  const { done, total, current } = event.payload;
  if (done >= total) {
    showProgress(total, total, `關鍵字完成 ${total} / ${total}`);
    setTimeout(hideProgress, 1200);
  } else {
    showProgress(done, total, `產生關鍵字 ${done + 1} / ${total}：${current ?? ""}`);
  }
});

/// After an import, compare against the shared doc — and stop there.
///
/// Keyword generation is deliberately NOT run automatically. Producers tidy up roughly
/// every half hour, so one import routinely holds dozens of scripts; firing them all at
/// Gemini on import walks straight into the free tier's per-minute cap and hands back a
/// screen of failed cards, having spent quota on entries the user may not even keep.
/// Compare still runs on its own because it costs nothing and it is what prunes the
/// keyword list, so by the time the user presses 產生關鍵字 the targets are already
/// narrowed to entries that actually need to go in.
async function runAutoPipeline() {
  if (items.length === 0) return;
  const status = el("compareStatus");

  let compared = false;
  try {
    await compareWithCollabDoc({ silent: true });
    compared = hasCompared;
  } catch {
    compared = false;
  }

  if (!compared) {
    status.classList.remove("hidden");
    status.className = "compare-status";
    status.textContent =
      "已匯入。尚未比對（協作工具視窗還沒開），請先點「開啟協作工具」再按「比對」，以免重複寫入。";
  }
  updateKeywordButton();
}

function keywordTargets() {
  // 每天開始寫入以前的節次（例：0600）永遠不會自動寫，也就不花額度產生關鍵字。
  return selectKeywordTargets(items, noUploadLabel).filter((i) => !beforeWriteStartShow(i));
}

// --- 每天開始寫入的時間（v0.3.26）---
// 設定 inews.write_start（預設 07:00）：播出日開始（02:00）到這個時間之前，自動流程照常讀 iNEWS、
// 比對、產生關鍵字，但不寫入協作文件；時間到了才開始寫，而且只寫這個時間之後的節次 ——
// 跟 07:00 才打開工具一樣。早上 06:00 到班就可以先開著。空白＝不限制。
function hhmmToMinutes(text) {
  const m = /^(\d{1,2}):(\d{2})$/.exec(String(text ?? "").trim());
  return m ? Number(m[1]) * 60 + Number(m[2]) : null;
}

/** 播出日裡的第幾分鐘（播出日從 broadcast_day_start 開始）。 */
function broadcastMinute(minutes) {
  const dayStart = hhmmToMinutes(inewsCfg.broadcast_day_start ?? "02:00") ?? 120;
  return (((minutes - dayStart) % 1440) + 1440) % 1440;
}

function writeStartMinute() {
  const start = hhmmToMinutes(inewsCfg.write_start);
  return start === null ? null : broadcastMinute(start);
}

/** 現在還沒到每天開始寫入的時間。 */
function beforeWriteStart(now = new Date()) {
  const start = writeStartMinute();
  return start !== null && broadcastMinute(now.getHours() * 60 + now.getMinutes()) < start;
}

/** 這張 iNEWS 卡片屬於開始寫入時間以前的節次（例：開始時間 07:00，0600 節的稿）。 */
function beforeWriteStartShow(item) {
  const start = writeStartMinute();
  const show = item.inews?.show ?? "";
  if (start === null || !/^\d{4}$/.test(show) || item.inews?.manual) return false;
  return broadcastMinute(Number(show.slice(0, 2)) * 60 + Number(show.slice(2))) < start;
}

function loadSummary(summary) {
  // A fresh card set has not been compared against the doc yet, whatever the previous
  // set's state was -- otherwise the write-back guard would trust a stale comparison.
  hasCompared = false;
  clearCooldown();
  items = summary.inews ? inewsItemsFromSummary(summary) : summary.entries.map(itemFromDto);
  rememberInewsVersions(summary);
  // Running order: earliest 累積時間 first, because that is the order tapes get cut
  // in. Sorting here means the card list and the written-back output share it.
  items = sortByTime(items);
  el("compareBtn").disabled = items.length === 0;

  el("statPassed").textContent = summary.passed;
  el("statUnknown").textContent = summary.unknown;
  el("statManual").textContent = summary.needs_manual;
  el("statFiltered").textContent = summary.filtered;
  el("statFailed").textContent = summary.failed;
  el("summary").classList.remove("hidden");
  el("emptyState").classList.toggle("hidden", items.length > 0);

  el("clearCardsBtn").disabled = items.length === 0;

  renderFunnel();
  render();
}

/// The single answer to "what would 複製／存檔／寫入 send right now?" -- every one of
/// those three has to agree, so none of them may re-derive the rule. The rule itself
/// lives in logic.js, where a test can reach it.
function outgoingItems() {
  return outgoingOf(items);
}

/// Output buttons follow the ticks, not the import-time parse tally. Deriving them
/// from the tally alone left a rescued 已濾除 (or 未知樣式) entry with everything on
/// screen ready to go and 寫入 still greyed out.
function updateOutputButtons() {
  const none = outgoingItems().length === 0;
  el("copyBtn").disabled = none;
  el("saveBtn").disabled = none;
  el("writeBackBtn").disabled = none;
}

/// One line answering "if I press 寫入 now, what goes in?" -- the stat pills above are
/// filters for the parse outcome, which is a different question and doesn't tell the
/// user what the write button will actually do.
function renderFunnel() {
  updateOutputButtons();
  const node = el("funnel");
  const { pending, skipped, outgoing } = computeFunnel(items);
  if (pending === 0) {
    node.classList.add("hidden");
    return;
  }

  node.classList.remove("hidden");
  node.innerHTML =
    `<span class="funnel-step"><span class="funnel-num">${pending}</span> 則待處理</span>` +
    (skipped > 0
      ? `<span class="funnel-arrow">→</span><span class="funnel-step skip"><span class="funnel-num">${skipped}</span> 則略過</span>`
      : "") +
    `<span class="funnel-arrow">→</span>` +
    `<span class="funnel-step out"><span class="funnel-num">${outgoing}</span> 則寫入</span>` +
    (hasCompared ? "" : `<span class="muted">（尚未比對，數字未扣除文件中已有的）</span>`);
}

function cardHtml(item) {
  const { dto, kind, bucket, id } = item;
  const fields = dto[kind] ?? dto;

  const badgeLabel = {
    Passed: "通過",
    UnknownStyle: "未知樣式",
    NeedsManualContent: "待補稿",
    // Renders as a full, editable card like any other: it starts unticked, but the
    // slug and body are there so a BS story that unexpectedly needs to go out can be
    // ticked on the spot rather than requiring a blocklist edit and a re-import.
    FilteredByStyle: "已濾除",
    // Also a full card, unticked: the reason is the first warning, and whatever could
    // be parsed is already in the fields so a wrongly judged row is one tick away.
    ParseFailed: "解析失敗",
  }[kind];
  const titleDiff = renderDiff(fields.raw_title, fields.raw_title !== undefined ? item.title : "");
  const bodyDiff = renderDiff(fields.raw_body, fields.raw_body !== undefined ? item.body : "");
  const hasDiff = !!(fields.raw_title || fields.raw_body);
  const matchLabel = item.alreadyInDoc
    ? "已在文件中"
    : item.prelisted
      ? "預列(X)"
      : { to_cut: "待處理", keep_refresh: "要重貼", removed: "已剔除" }[item.matchStatus];
  const matchClass = item.alreadyInDoc ? "removed" : item.matchStatus;
  // The 編輯備註 marker only ever showed up in the output text, so a 勿上網 story was
  // indistinguishable from any other card on screen. Show it where the call is made.
  const marker = (item.slug_marker ?? "").trim();
  const noUpload = isNoUpload(item, noUploadLabel);

  return `
    <div class="card ${bucket} ${item.collapsed ? "collapsed" : ""} ${item.alreadyInDoc ? "done" : ""} ${item.inews?.manual ? "manual-loaded" : ""}" data-bucket="${bucket}" data-id="${id}">
      <div class="card-head">
        <button class="collapse-toggle" data-id="${id}" title="展開／收合">${item.collapsed ? "▶" : "▼"}</button>
        <label class="include-toggle">
          <input type="checkbox" class="include-cb" data-id="${id}" ${item.included ? "checked" : ""} />
          輸出
        </label>
        <span class="slug">${escapeHtml(fields.slug)}</span>
        <span class="badge ${bucket}">${badgeLabel}</span>
        ${marker ? `<span class="badge marker">${escapeHtml(marker)}</span>` : ""}
        ${matchLabel ? `<span class="badge ${matchClass}" title="${escapeHtml(item.matchedLine ?? "")}">${matchLabel}</span>` : ""}
        ${item.crossDay ? `<span class="badge marker" title="${escapeHtml(item.matchedLine ?? "")}">前一天已上傳的重播</span>` : ""}
        ${holdLabel(item) ? `<span class="badge ${item.inews.hold === "suspect" ? "failed" : "marker"}">${holdLabel(item)}</span>` : ""}
        ${item.inews?.recheck ? `<span class="badge marker">複查補寫</span>` : ""}
        ${item.inews?.manual ? `<span class="badge manual-badge" title="從「搜尋 rundown」手動載入，換節次後清掉">手動載入</span>` : ""}
        <span>${escapeHtml(fields.style)}</span>
        <span>${escapeHtml(fields.time)}</span>
        <span>${escapeHtml(fields.group)}</span>
        ${item.collapsed ? `<span class="peek">${escapeHtml(item.title)}</span>` : ""}
        <span class="file-name">${escapeHtml(fields.file_name)}</span>
        ${hasDiff ? `<button class="diff-toggle" data-id="${id}">顯示轉換前後</button>` : ""}
      </div>

      <div class="card-body">
      <div class="field">
        <label>標題</label>
        <input type="text" class="edit-title" data-id="${id}" value="${escapeHtml(item.title)}" />
      </div>
      <div id="diff-title-${id}" class="diff-box hidden">${titleDiff ?? "（標點未變動）"}</div>

      <div class="field">
        <label>內文</label>
        <textarea class="edit-body" data-id="${id}">${escapeHtml(item.body)}</textarea>
      </div>
      <div id="diff-body-${id}" class="diff-box hidden">${bodyDiff ?? "（標點未變動）"}</div>

      <div class="field">
        <button class="normalize-btn" data-id="${id}" ${item.normalizing ? "disabled" : ""}>
          ${item.normalizing ? "校正中…" : "校正標點（標題＋內文）"}
        </button>
        <span class="muted">匯入時的標點正規化只跑一次；這裡手動打的或貼上的字要自己按這個才會套用同一套規則。</span>
        ${item.normalizeWarnings && item.normalizeWarnings.length ? `
          <div class="warnings">
            ${item.normalizeWarnings.map((w) => `<div class="w">⚠ ${escapeHtml(w)}</div>`).join("")}
          </div>` : ""}
      </div>

      <div class="field">
        <label>關鍵字（空格分隔）</label>
        <div class="keywords-row">
          <input type="text" class="edit-keywords" data-id="${id}" value="${escapeHtml(item.keywords)}" placeholder="#關鍵字1 #關鍵字2 #關鍵字3 #關鍵字4" />
          <button class="gen-keywords-btn" data-id="${id}" ${!hasApiKey || item.keywordStatus === "loading" ? "disabled" : ""}>
            ${item.keywordStatus === "loading" ? "產生中…" : "產生關鍵字"}
          </button>
        </div>
        ${item.keywordStatus === "error" ? `<div class="w">⚠ 產生失敗：${escapeHtml(item.keywordError)}</div>` : ""}
        ${noUpload ? `<div class="muted">${escapeHtml(marker)}，批次產生會略過（不上網的稿子用不到關鍵字，也不占每分鐘額度）。要的話按上面那顆按鈕單獨產生。</div>` : ""}
      </div>

      ${fields.warnings && fields.warnings.length ? `
        <div class="warnings">
          ${fields.warnings.map((w) => `<div class="w">⚠ ${escapeHtml(w)}</div>`).join("")}
        </div>` : ""}
      </div>
    </div>`;
}

/// True while an automatic iNEWS round is running. Re-rendering rebuilds every input,
/// so a round must not do it under someone typing into a card -- see render().
let inewsRoundActive = false;
let renderDeferred = false;

function isTypingInList() {
  const a = document.activeElement;
  return !!(a && el("list").contains(a) && (a.tagName === "TEXTAREA" || (a.tagName === "INPUT" && a.type === "text")));
}

el("list").addEventListener("focusout", () => {
  // Wait for focus to land: tabbing between two fields of one card is still typing.
  setTimeout(() => { if (renderDeferred && !isTypingInList()) render(); }, 0);
});

function render() {
  if (inewsRoundActive && isTypingInList()) {
    renderDeferred = true;
    return;
  }
  renderDeferred = false;
  const shown = activeFilter === "all" ? items : items.filter((i) => i.bucket === activeFilter);
  // Ticked cards first so they don't have to be scrolled for; display only -- `items`
  // itself stays in running order, which is what the output follows.
  const visible = sortForDisplay(shown);
  el("list").innerHTML = visible.map(cardHtml).join("");

  document.querySelectorAll(".edit-title").forEach((elm) => {
    elm.addEventListener("input", (e) => {
      const item = items.find((i) => i.id === e.target.dataset.id);
      if (item) item.title = e.target.value;
    });
  });
  document.querySelectorAll(".edit-body").forEach((elm) => {
    elm.addEventListener("input", (e) => {
      const item = items.find((i) => i.id === e.target.dataset.id);
      if (item) item.body = e.target.value;
    });
  });
  document.querySelectorAll(".edit-keywords").forEach((elm) => {
    elm.addEventListener("input", (e) => {
      const item = items.find((i) => i.id === e.target.dataset.id);
      if (item) item.keywords = e.target.value;
      // Typing keywords by hand takes that entry out of the run.
      updateKeywordButton();
    });
  });
  document.querySelectorAll(".include-cb").forEach((elm) => {
    elm.addEventListener("change", (e) => {
      const item = items.find((i) => i.id === e.target.dataset.id);
      if (item) {
        item.included = e.target.checked;
        // An explicit tick becomes the new baseline, or the next 比對 would silently
        // undo it: decideInclusion rebuilds `included` from `defaultIncluded`, which
        // is false for the buckets a user rescues from (已濾除, 未知樣式). Unticking
        // by hand sticks for the same reason.
        item.defaultIncluded = e.target.checked;
        // 已剔除／已在文件的卡片，比對每輪都會強制取消勾選（defaultIncluded 也蓋不過）。
        // 手動勾過就記下來，比對不再覆蓋（v0.3.15）。自動寫入本來就不寫這兩種，所以只
        // 影響手動寫入。
        item.userIncluded = e.target.checked;
      }
      renderFunnel();
      updateKeywordButton();
      // Move the card into the ticked/unticked group straight away.
      render();
    });
  });
  document.querySelectorAll(".diff-toggle").forEach((elm) => {
    elm.addEventListener("click", (e) => {
      const id = e.target.dataset.id;
      el(`diff-title-${id}`).classList.toggle("hidden");
      el(`diff-body-${id}`).classList.toggle("hidden");
    });
  });
  document.querySelectorAll(".gen-keywords-btn").forEach((elm) => {
    elm.addEventListener("click", (e) => generateKeywordsForOne(e.target.dataset.id));
  });
  document.querySelectorAll(".normalize-btn").forEach((elm) => {
    elm.addEventListener("click", (e) => normalizeOne(e.target.dataset.id));
  });
  document.querySelectorAll(".collapse-toggle").forEach((elm) => {
    elm.addEventListener("click", (e) => {
      const item = items.find((i) => i.id === e.target.dataset.id);
      if (!item) return;
      item.collapsed = !item.collapsed;
      render();
    });
  });
  // The button carries a live count of what a run would send, so it has to be
  // recomputed by the one path every state change already goes through -- and so do
  // the output buttons, for the same reason.
  updateKeywordButton();
  updateOutputButtons();
}

/// Applies the same punctuation pass import already runs, to a title/body the user
/// typed or pasted by hand. Import-time normalization only ever touches text that
/// existed at import time, so anything filled in afterwards (待補稿, a rescued 已濾除
/// entry) would otherwise stay unnormalized forever unless this is run explicitly.
async function normalizeOne(id) {
  const item = items.find((i) => i.id === id);
  if (!item) return;
  item.normalizing = true;
  render();
  try {
    const [titleResult, bodyResult] = await Promise.all([
      invoke("normalize_text", { text: item.title, isTitle: true }),
      invoke("normalize_text", { text: item.body }),
    ]);
    item.title = titleResult.text;
    item.body = bodyResult.text;
    item.normalizeWarnings = [...titleResult.warnings, ...bodyResult.warnings];
  } catch (err) {
    item.normalizeWarnings = [`校正失敗：${err}`];
  }
  item.normalizing = false;
  render();
}

async function generateKeywordsForOne(id) {
  const item = items.find((i) => i.id === id);
  if (!item) return;
  item.keywordStatus = "loading";
  render();
  try {
    const words = await invoke("generate_keywords", { title: item.title, body: item.body, count: KEYWORD_COUNT });
    item.keywords = words.join(" ");
    item.keywordStatus = "idle";
    item.keywordError = "";
  } catch (err) {
    item.keywordStatus = "error";
    item.keywordError = String(err);
  }
  render();
}

/// Keeps the button honest about what pressing it will do: how many entries are
/// waiting, and whether this run will only cover part of them. Called after anything
/// that can change the target set (import, compare, tick, manual keyword edit).
function updateKeywordButton() {
  const btn = el("genAllKeywordsBtn");
  if (!hasApiKey) {
    btn.disabled = true;
    btn.textContent = "產生關鍵字";
    btn.title = "尚未設定 Gemini API key，請到設定頁輸入";
    return;
  }
  const targets = keywordTargets();
  const { batch, remaining } = splitKeywordRun(targets, keywordMaxPerRun);
  btn.disabled = targets.length === 0;
  btn.textContent = targets.length === 0 ? "產生關鍵字" : `產生關鍵字（${batch.length}）`;
  btn.title =
    targets.length === 0
      ? "沒有需要產生關鍵字的稿件（已有關鍵字、未勾選，或已在文件中）"
      : remaining > 0
        ? `待產生 ${targets.length} 則，本次先送 ${batch.length} 則，剩下 ${remaining} 則等約一分鐘後再按一次`
        : `待產生 ${targets.length} 則`;
}

/// One-minute countdown under the status line, shown only when the user will have to
/// send again: a run left entries over, or the per-minute cap was hit. Counted from a
/// fixed end time so a throttled timer can't drift.
let cooldownTimer = null;
const COOLDOWN_MS = 60_000;

function clearCooldown() {
  if (cooldownTimer) clearInterval(cooldownTimer);
  cooldownTimer = null;
  el("cooldown")?.remove();
}

function startCooldown() {
  clearCooldown();
  const node = document.createElement("div");
  node.id = "cooldown";
  node.className = "cooldown";
  el("compareStatus").appendChild(node);
  const end = Date.now() + COOLDOWN_MS;
  const tick = () => {
    // Another action rewrote the status line; the countdown went with it.
    if (!node.isConnected) return clearCooldown();
    const left = Math.ceil((end - Date.now()) / 1000);
    if (left <= 0) {
      node.textContent = "✅ 可以再次送出";
      node.classList.add("done");
      clearInterval(cooldownTimer);
      cooldownTimer = null;
      return;
    }
    node.textContent = `⏳ 倒數 ${left} 秒後可再次送出`;
  };
  tick();
  cooldownTimer = setInterval(tick, 1000);
}

async function generateKeywordsForAllIncluded() {
  clearCooldown();
  const targets = keywordTargets();
  const { batch, remaining } = splitKeywordRun(targets, keywordMaxPerRun);
  const rateLimited = await generateKeywordsFor(batch);
  if (remaining > 0) {
    const status = el("compareStatus");
    status.classList.remove("hidden");
    status.className = "compare-status";
    status.textContent =
      `本次已送出 ${batch.length} 則（避開 Gemini 每分鐘上限）。還有 ${remaining} 則未產生，` +
      `請等約一分鐘後再按一次「產生關鍵字」——已完成的不會重複消耗額度。`;
  }
  if (remaining > 0 || rateLimited) startCooldown();
  updateKeywordButton();
}

/// Returns whether the run hit the per-minute cap.
async function generateKeywordsFor(targets) {
  if (!targets || targets.length === 0) return false;

  showProgress(0, targets.length, `產生關鍵字 0 / ${targets.length}`);
  targets.forEach((i) => { i.keywordStatus = "loading"; });
  render();

  const requests = targets.map((i) => ({ id: i.id, title: i.title, body: i.body }));
  let rateLimited = false;
  try {
    const results = await invoke("generate_keywords_batch", { items: requests, count: KEYWORD_COUNT });
    for (const r of results) {
      const item = items.find((i) => i.id === r.id);
      if (!item) continue;
      if (r.keywords) {
        item.keywords = r.keywords.join(" ");
        item.keywordStatus = "idle";
        item.keywordError = "";
      } else {
        item.keywordStatus = "error";
        item.keywordError = r.error ?? "未知錯誤";
      }
    }
    rateLimited = reportRateLimit(results.filter((r) => !r.keywords).map((r) => r.error ?? ""), targets.length);
  } catch (err) {
    targets.forEach((i) => { i.keywordStatus = "error"; i.keywordError = String(err); });
    hideProgress();
    rateLimited = reportRateLimit([String(err)], targets.length);
  }
  render();
  return rateLimited;
}

/// The free Gemini tier caps requests per minute, and a big batch walks straight into
/// it. Individual cards already show their own error, but a quota failure needs a
/// banner: the fix is "wait a minute and re-run", which is not obvious from a red
/// line buried on card 14 of 20.
function reportRateLimit(errors, attempted) {
  const hits = errors.filter(isRateLimitError);
  if (hits.length === 0) return false;
  const status = el("compareStatus");
  status.classList.remove("hidden");
  status.className = "compare-status error";
  status.textContent =
    `⚠ 已達 Gemini 每分鐘請求上限（429）：${attempted} 則中有 ${hits.length} 則沒產生成功。` +
    `請等約一分鐘後，再按「全部產生關鍵字」補跑（已成功的不會重複消耗額度）。`;
  return true;
}

/// Flattens each card's enum payload into the shape buildOutputText expects, so the
/// formatting rule itself stays a pure, tested function.
function outputFieldsOf(i) {
  const fields = i.dto[i.kind] ?? i.dto;
  return {
    bucket: i.bucket,
    included: i.included,
    slug: fields.slug,
    // 預列區的 (X) 稿（v0.3.23）：寫入時 slug 前面加 (X)，跟早班手打的一樣。
    slug_marker: (i.prelisted ? "(X)" : "") + (fields.slug_marker ?? ""),
    style: fields.style,
    time: fields.time,
    group: fields.group,
    title: i.title,
    body: i.body,
    keywords: i.keywords,
  };
}

// --- 預列清單（v0.3.23）---
// 今天的段落還沒有或是空的時候，第一次寫入前讀 iNEWS 的預列 queue（預設 STORIES.LOCAL.APPR）
// 最上面那一段，寫成 `(X)slug<Tab>樣式`，放在日期分隔線下面。同一批裡剛好是預列稿的，
// 寫入時也加 (X)。後端寫入當下會再確認一次段落是不是還空著，不是就不放。
let prelistWanted = false;
let prelistDoneFor = "";

async function prelistForWrite(outgoing) {
  if (!prelistWanted || !compareDay || prelistDoneFor === compareDay) return [];
  try {
    const list = await invoke("fetch_prelist");
    prelistDoneFor = compareDay;
    const slugs = new Set(list.map((p) => p.slug.trim()));
    for (const i of outgoing) if (slugs.has(slugOf(i).trim())) i.prelisted = true;
    return list.map((p) => `(X)${p.slug}${p.style ? `\t${p.style}` : ""}`);
  } catch (err) {
    logDecision(`[預列] 讀不到預列清單，這次先不補：${err}`);
    return [];
  }
}

function buildOutput() {
  return buildOutputText(items.map(outputFieldsOf));
}

async function copyAll() {
  const text = buildOutput();
  await navigator.clipboard.writeText(text);
  const btn = el("copyBtn");
  const original = btn.textContent;
  btn.textContent = "已複製！";
  setTimeout(() => { btn.textContent = original; }, 1200);
}

async function saveAll() {
  const text = buildOutput();
  await invoke("save_text_file", { defaultName: "新聞文稿.txt", content: text });
}

el("importBtn").addEventListener("click", importConfiguredFolder);
el("clearFolderBtn").addEventListener("click", clearFolder);
el("clearCardsBtn").addEventListener("click", clearCards);
el("copyBtn").addEventListener("click", copyAll);
el("saveBtn").addEventListener("click", saveAll);
el("genAllKeywordsBtn").addEventListener("click", generateKeywordsForAllIncluded);
el("settingsBtn").addEventListener("click", openSettings);
el("openCollabBtn").addEventListener("click", () => openCollab());
el("compareBtn").addEventListener("click", compareWithCollabDoc);
el("writeBackBtn").addEventListener("click", writeBackToCollab);

// Writing lands in a document colleagues are editing live, so this always confirms
// first and shows exactly how much is going in -- never a silent one-click send.
async function writeBackToCollab() {
  const outgoing = outgoingItems();
  if (outgoing.length === 0) {
    window.alert("目前沒有勾選任何要輸出的稿件。");
    return;
  }

  // Two independent duplicate guards, because each covers a hole the other leaves:
  // the doc comparison can be stale or skipped, while the session record cannot see
  // entries a colleague added from another machine.
  const already = outgoing.filter((i) => writtenSlugs.has(slugOf(i)) || i.alreadyInDoc);
  if (already.length > 0) {
    const ok = window.confirm(
      `⚠ 這 ${already.length} 則稿件已經在協作文件裡了：\n\n` +
        already.map((i) => `・${slugOf(i)}`).join("\n") +
        `\n\n再寫一次會在文件裡出現兩份。確定仍要寫入嗎？`
    );
    if (!ok) return;
  }

  if (!hasCompared) {
    const ok = window.confirm(
      "⚠ 還沒有跟協作工具文件比對過，無法得知這些稿件是不是已經在文件裡了。\n\n" +
        "建議先按「比對」再寫入，以免重複。仍要直接寫入嗎？"
    );
    if (!ok) return;
  }

  const text = buildOutput();
  if (!text.trim()) {
    window.alert("目前沒有勾選任何要輸出的稿件。");
    return;
  }
  const ok = window.confirm(
    `即將把 ${outgoing.length} 則稿件（共 ${text.split("\n").length} 行）附加到協作工具文件的「最下方」。\n\n` +
      (dateHeaderOwed ? `文件裡還沒有今天的日期分隔線，會先補上「${dateHeaderOwed}」。\n\n` : "") +
      `這會即時同步給所有正在編輯的同事，且不會刪除或覆蓋任何既有內容。\n\n確定要寫入嗎？`
  );
  if (!ok) return;

  const status = el("compareStatus");
  const btn = el("writeBackBtn");
  status.classList.remove("hidden");
  status.className = "compare-status";
  status.textContent = "寫入中…";
  btn.disabled = true;
  try {
    const prelist = await prelistForWrite(outgoing);
    const res = await invoke("append_to_collab_doc_checked", {
      parts: outgoing
        .map((i) => ({ slug: slugOf(i), text: buildOutputText([outputFieldsOf(i)]) }))
        .filter((p) => p.text.trim() !== ""),
      header: dateHeaderOwed || null,
      prelist,
      // 手動寫入也留紀錄：以前只有自動流程會寫「成功」，手動的在紀錄裡看不出來。
      logNote: `手動寫入 ${writeLogNote(outgoing, [])}`,
    });
    const msg = res.message ?? "整批都已經在文件裡了，這次沒有寫入。";
    dateHeaderWritten();
    outgoing.forEach((i) => {
      writtenSlugs.add(slugOf(i));
      const key = writtenKey(i);
      if (key) writtenKeys.add(key);
      // 寫過了：手動勾選的記憶清掉，下一次比對照文件狀態重算（不然會一直勾著）。
      delete i.userIncluded;
    });
    status.textContent = `✅ ${msg}`;
  } catch (err) {
    status.className = "compare-status error";
    status.textContent = `⚠ 寫入失敗：${err}`;
  } finally {
    btn.disabled = false;
  }
}
// --- 從 iNEWS 匯入 / 自動刷新 ---
//
// 匯入本身跟資料夾匯入走同一條路（後端 import_inews → import_files）。自動刷新
// 只是定時重跑，然後把現有的流程串起來：比對 → 產生關鍵字 → 寫入（或等確認）。
// 判斷規則全在 logic.js（mergeInewsItems / autoWriteCandidates / composeInewsWrite）。

let inewsCfg = {
  poll_seconds: 10,
  correct_time: true,
  auto_write: false,
  divider_format: "==== {hh} ====",
  divider_position: "after",
};
/** story key -> iNEWS version of the cards on screen, for change detection. */
let inewsVersions = new Map();
/** Show of the cards on screen. */
let inewsShow = "";
/** Show of the last automatic write this session, so the divider goes in once. */
let lastWrittenShow = "";
/** Shows whose delayed sweep has completed: their divider may now be written. */
const closedShows = new Set();
/** Show -> 節目分隔線 written right after that show's divider (關鍵時刻). */
const programDividers = new Map();
/** The next auto round is the first since 自動刷新 was switched on. */
let inewsSessionStart = false;
/** `${key}|${version}` written this session: one version is never written twice. */
const writtenKeys = new Set();
/** story key -> { slug, show } written this session, for the 疑似誤寫 check (v0.3.11). */
const writtenStories = new Map();
/** The last 疑似誤寫 report, shown under the round status until the next 定時複查. */
let misfires = [];
let inewsTimer = null;
let inewsBusy = false;
/** Keyword runs that hit the per-minute cap (or left entries over) wait this long. */
let keywordCooldownUntil = 0;

function applyInewsConfig(config) {
  inewsCfg = { ...inewsCfg, ...(config.inews ?? {}) };
}

function rememberInewsVersions(summary) {
  // Merged, not replaced: a sweep round also reports the previous show's stories, and
  // the round after it must not see them as new again.
  for (const s of summary.inews?.stories ?? []) inewsVersions.set(s.key, s.version);
  inewsShow = summary.inews?.show ?? "";
}

function inewsStatus(text, kind = "") {
  const node = el("inewsStatus");
  node.className = `compare-status ${kind}`;
  node.textContent = text;
}

function describeInewsRound(meta) {
  const aired = (meta.stories ?? []).filter((s) => (s.show ?? meta.show) === meta.show).length;
  const end = meta.end_show ? `${meta.range_end}（${meta.end_show} 開始，${meta.end_source}）` : `${meta.range_end}`;
  if (meta.off_air) {
    const lines = [
      `iNEWS ${meta.show} 停播${meta.program ? `（${meta.program}）` : ""}（現在 ${meta.now}）：` +
        `${meta.range_start}（${meta.start_source}）～ ${end} 不匯入。`,
    ];
    for (const e of meta.events ?? []) lines.push(`・${e}`);
    for (const w of meta.warnings ?? []) lines.push(`⚠ ${w}`);
    return lines.join("\n");
  }
  const lines = [
    `iNEWS ${meta.show} 節（現在 ${meta.now}${meta.switched_early ? "，已提前換到這一節" : ""}）：` +
      `有效範圍 ${meta.range_start}（${meta.start_source}）～ ${end}。` +
      `已播出 ${aired} 則、範圍內尚未播出 ${meta.pending} 則；本輪讀取 ${meta.retrieved} / ${meta.list_count} 則。`,
  ];
  if (meta.cutoff_title) {
    lines.push(`標記式截止：「${meta.cutoff_title}」以下不處理${meta.cutoff_rule === "last_prefixed" ? "（找不到預告後的廣告列，改用最後一個廣告列）" : ""}。`);
  }
  for (const e of meta.events ?? []) lines.push(`・${e}`);
  for (const w of meta.warnings ?? []) lines.push(`⚠ ${w}`);
  return lines.join("\n");
}

function updateStatsFromItems() {
  const count = (b) => items.filter((i) => i.bucket === b).length;
  el("statPassed").textContent = count("passed");
  el("statUnknown").textContent = count("unknown");
  el("statManual").textContent = count("manual");
  el("statFiltered").textContent = count("filtered");
  el("statFailed").textContent = count("failed");
  el("summary").classList.toggle("hidden", items.length === 0);
  el("emptyState").classList.toggle("hidden", items.length > 0);
  el("clearCardsBtn").disabled = items.length === 0;
  el("compareBtn").disabled = items.length === 0;
}

/// 「從 iNEWS 匯入」按鈕：跟「匯入資料夾」一樣整批換掉卡片，然後只比對。
async function importInewsOnce() {
  if (inewsBusy) return;
  inewsBusy = true;
  el("importInewsBtn").disabled = true;
  inewsStatus("讀取 iNEWS 中…");
  try {
    const summary = await invoke("import_inews");
    loadSummary(summary);
    inewsStatus(describeInewsRound(summary.inews), summary.inews.warnings?.length ? "warn" : "");
    await runAutoPipeline();
  } catch (err) {
    inewsStatus(`⚠ 從 iNEWS 匯入失敗：${err}`, "error");
  } finally {
    inewsBusy = false;
    el("importInewsBtn").disabled = false;
  }
}

/// 自動刷新的一輪：合併卡片，有新的已播出稿或內容變動（或上一輪留下沒做完的）
/// 才往下跑比對 → 關鍵字 → 寫入。
async function inewsRound() {
  if (inewsBusy) return;
  inewsBusy = true;
  inewsRoundActive = true;
  try {
    let summary;
    try {
      summary = await invoke("import_inews", { auto: true, sessionStart: inewsSessionStart });
      inewsSessionStart = false;
    } catch (err) {
      // 網路閃斷或伺服器重啟：下一輪後端會自動重連，所以不停下來。
      inewsStatus(`⚠ 讀取 iNEWS 失敗（下一輪會自動重試）：${err}`, "error");
      return;
    }
    const meta = summary.inews;
    noteInewsRound(closedShows, programDividers, meta);
    // Restarted mid-show: the backend picked up the show it was following, so the
    // divider for it is not written a second time (v0.3.5).
    //
    // 分隔線放在節次開始前（before）時不能這樣假設（v0.3.24）：11/13 07:48 換到 0800、
    // 還沒寫第一則（分隔線跟第一則一起寫）就更新重啟，接續時當成 0800 的分隔線已經寫過，
    // 結果 0800 的稿直接接在 0700 後面、沒有 ==== 08 ====。before 的分隔線照常跟第一則
    // 一起送，已經在文件裡的話寫入前比對會濾掉（v0.3.12 起），不會重複。
    if (meta.resumed_show && !lastWrittenShow && inewsCfg.divider_position !== "before") {
      lastWrittenShow = meta.resumed_show;
    }
    const { added, changed } = diffInewsVersions(inewsVersions, meta.stories);
    const hadCards = items.some((i) => i.inews);
    const before = items;
    // 手動載入的卡片（v0.3.15）只活在目前這一節：換節就不接回來。
    items = sortByTime(
      keepManualCards(mergeInewsItems(hadCards ? items : [], inewsItemsFromSummary(summary), writtenKeys), before, meta.show)
    );
    rememberInewsVersions(summary);
    if (added.length || changed.length || !hadCards) hasCompared = false;
    updateStatsFromItems();
    renderFunnel();
    render();
    queueUnknownReviews();
    inewsStatus(
      describeInewsRound(meta) +
        (added.length || changed.length ? `\n本輪新增 ${added.length} 則、內容變動 ${changed.length} 則。` : ""),
      meta.warnings?.length ? "warn" : ""
    );

    // 使用者正在操作（主視窗或協作視窗最近 30 秒有動作、或審查視窗開著）：這一輪不比對、
    // 不寫入、不校正，免得卡片在使用者面前變動（v0.3.15）。讀 rundown 照常。
    if (await operatingPause()) {
      el("inewsStatus").textContent += `\n⏸ 操作中，暫停自動比對與寫入（停止操作 ${PAUSE_IDLE_MS / 1000} 秒後恢復，最長暫停 ${PAUSE_MAX_MS / 60000} 分鐘）。`;
      return;
    }

    // 時間校正不等關鍵字、節次暫存或分隔線：已經在文件裡的稿每一輪都對一次。
    if (inewsCfg.correct_time !== false) await correctCollabTimes(meta.time_rows ?? []);

    if (meta.recheck) misfires = findMisfires(writtenStories, meta.recheck);
    if (misfires.length) {
      el("inewsStatus").textContent +=
        `\n⚠ 疑似誤寫 ${misfires.length} 則（寫進文件後，rundown 已經不是已播出；工具不會刪，請人工確認）：` +
        misfires.map((m) => `${m.slug}（${m.show}）`).join("、");
    }

    // Held cards (等確認 / 冷卻 / 可疑) cannot be written this round, so they alone do not start the pipeline.
    const pending = items.some((i) => isPendingWrite(i, writtenKeys) && !i.inews?.hold);
    if (added.length || changed.length || pending || meta.closed_show || dividerOwed()) await runInewsPipeline();
  } finally {
    inewsRoundActive = false;
    inewsBusy = false;
    if (renderDeferred && !isTypingInList()) render();
  }
}

// --- 操作中暫停（v0.3.15）---
const PAUSE_IDLE_MS = 30_000;
const PAUSE_MAX_MS = 180_000;
let lastMainActivityAt = 0;
let pausedSince = 0;
for (const type of ["mousedown", "keydown", "wheel", "input", "touchstart"]) {
  document.addEventListener(type, () => { lastMainActivityAt = Date.now(); }, true);
}

/** 這一輪要不要暫停自動比對與寫入。最長暫停 PAUSE_MAX_MS，之後強制跑一次。 */
async function operatingPause() {
  const now = Date.now();
  let busy = reviewing !== null || now - lastMainActivityAt < PAUSE_IDLE_MS;
  if (!busy) {
    try {
      const idle = await invoke("collab_idle_seconds");
      busy = idle != null && idle * 1000 < PAUSE_IDLE_MS;
    } catch {
      /* 問不到協作視窗就當作沒在操作 */
    }
  }
  if (!busy) {
    if (pausedSince) logDecision("[恢復] 使用者停止操作，恢復自動比對與寫入");
    pausedSince = 0;
    return false;
  }
  if (!pausedSince) {
    pausedSince = now;
    logDecision("[暫停] 使用者操作中，暫停自動比對與寫入");
    return true;
  }
  if (now - pausedSince >= PAUSE_MAX_MS) {
    logDecision(`[恢復] 已暫停 ${PAUSE_MAX_MS / 60000} 分鐘，強制跑一輪`);
    pausedSince = 0;
    return false;
  }
  return true;
}

/// 時間校正（v0.3.9）。失敗只顯示，不停自動刷新：寫入前的比對會自己擋。
async function correctCollabTimes(rows) {
  const node = el("inewsStatus");
  try {
    const r = await invoke("correct_collab_times", { rows });
    const lines = [];
    if (r.corrected.length) lines.push(`✏ 時間校正 ${r.corrected.length} 則：${r.corrected.join("、")}`);
    if (r.failed.length) lines.push(`⚠ 時間校正 ${r.failed.length} 則內容不符，下一輪重新判斷：${r.failed.join("、")}`);
    if (r.waiting) lines.push(`⏳ ${r.waiting} 則時間有變，等下一輪確認後再改`);
    if (lines.length) node.textContent += `\n${lines.join("\n")}`;
  } catch (err) {
    node.textContent += `\n⚠ 時間校正失敗（下一輪會再試）：${err}`;
  }
}

function stopInewsAuto(message) {
  el("inewsAutoToggle").checked = false;
  clearTimeout(inewsTimer);
  inewsTimer = null;
  if (message) inewsStatus(message, "error");
}

async function runInewsPipeline() {
  // 1. 比對協作文件。視窗沒開或沒登入就停下來，絕不在沒比對的情況下寫入。
  try {
    await compareWithCollabDoc({ silent: false });
  } catch (err) {
    // 協作工具還在載入或沒連上：這一輪跳過，下一輪再試，不停掉自動刷新（v0.3.15）。
    if (isCollabNotReady(err)) {
      inewsStatus(`⏳ ${String(err).replace(COLLAB_NOT_READY, "").trim()}`, "warn");
      return;
    }
    stopInewsAuto(`⚠ 無法比對協作文件，自動刷新已停止：${err}\n請先「開啟協作工具」並登入，再重新打開「自動刷新」。`);
    return;
  }

  // 2. 待上傳和要重貼的稿產生關鍵字，遵守單次上限；超過的留到下一輪。
  const targets = keywordTargets();
  if (targets.length > 0) {
    if (!hasApiKey) {
      inewsStatus("⚠ 尚未設定 Gemini API key，自動流程無法產生關鍵字，稿件不會自動寫入。", "error");
    } else if (Date.now() >= keywordCooldownUntil) {
      const { batch, remaining } = splitKeywordRun(targets, keywordMaxPerRun);
      const rateLimited = await generateKeywordsFor(batch);
      if (remaining > 0 || rateLimited) keywordCooldownUntil = Date.now() + COOLDOWN_MS;
    }
  }

  // 3. 寫入（或等確認）。分隔線要等上一節補掃完才寫，新一節的稿也先等著。
  if (beforeWriteStart()) {
    hideInewsConfirm();
    inewsStatus(`⏳ 每天 ${inewsCfg.write_start} 才開始寫入協作文件；在那之前照常讀 iNEWS、比對、產生關鍵字。`, "warn");
    return;
  }
  const ready = autoWriteCandidates(items, writtenKeys, noUploadLabel).filter((i) => !beforeWriteStartShow(i));
  const plan = inewsWriteText(ready);
  if (plan.written.length === 0 && !plan.text.trim()) {
    hideInewsConfirm();
    if (plan.held.length) showHeldNotice(plan.held);
    return;
  }
  if (inewsCfg.auto_write && !compareHoldsAutoWrite) await writeInewsItems(ready);
  else showInewsConfirm(plan);
}

/// Shows that still have entries waiting for keywords or a compare: their divider
/// waits for those, or a straggler would land under the next show's divider.
function pendingShowsOutside(ready) {
  const inReady = new Set(ready);
  return new Set(
    items
      .filter((i) => i.inews && !inReady.has(i) && isPendingWrite(i, writtenKeys) && i.inews.hold !== "suspect")
      .map((i) => i.inews.show)
  );
}

function inewsWriteText(ready) {
  return composeInewsWrite(
    ready,
    { lastShow: lastWrittenShow, closed: closedShows, pending: pendingShowsOutside(ready), programs: programDividers },
    { format: inewsCfg.divider_format, position: inewsCfg.divider_position },
    (group) => buildOutputText(group.map(outputFieldsOf))
  );
}

/// A closed show's divider is due even when no story comes with it.
function dividerOwed() {
  return !!lastWrittenShow && closedShows.has(lastWrittenShow) && inewsWriteText([]).text.trim() !== "";
}

function heldText(held) {
  const shows = [...new Set(held.map((i) => i.inews?.show))].join("、");
  return `${held.length} 則 ${shows} 的稿等 ${lastWrittenShow} 節延遲補掃完成、寫完分隔線後再寫入（換節次後 ${inewsCfg.sweep_delay_minutes ?? 10} 分鐘）。`;
}

/// 寫進決策紀錄的一行：幾則、哪些 slug、分隔線。
function writeLogNote(written, dividers) {
  const slugs = written.map(slugOf);
  return (
    `${written.length} 則` +
    (slugs.length ? `（${slugs.join("、")}）` : "") +
    (dividers.length ? `｜分隔線 ${dividers.join("、")}` : "") +
    (dateHeaderOwed ? `｜日期分隔線 ${dateHeaderOwed}` : "")
  );
}

function showHeldNotice(held) {
  const status = el("compareStatus");
  status.classList.remove("hidden");
  status.className = "compare-status";
  status.textContent = `⏸ ${heldText(held)}`;
}

function showInewsConfirm(plan) {
  const slugs = plan.written.map(slugOf);
  el("inewsConfirmText").textContent =
    (slugs.length
      ? `${slugs.length} 則已產生關鍵字，等待寫入協作文件：` + slugs.slice(0, 8).join("、") + (slugs.length > 8 ? ` 等 ${slugs.length} 則` : "")
      : `等待寫入節次分隔線 ${plan.dividers.join("、")}`) +
    (plan.dividers.length && slugs.length ? `（含分隔線 ${plan.dividers.join("、")}）` : "") +
    (plan.held.length ? `\n${heldText(plan.held)}` : "");
  el("inewsConfirm").classList.remove("hidden");
}

function hideInewsConfirm() {
  el("inewsConfirm").classList.add("hidden");
}

async function writeInewsItems(ready) {
  // 寫入前再擋一次：同一個版本（或這次 session 寫過的 slug）不重複寫入。
  const fresh = ready.filter((i) => isPendingWrite(i, writtenKeys) && !writtenSlugs.has(`${slugOf(i)}|${i.inews?.version ?? ""}`));
  const { text, pieces, lastShow, written, held, dividers } = inewsWriteText(fresh);
  if (!text.trim()) {
    if (held.length) showHeldNotice(held);
    return;
  }
  const status = el("compareStatus");
  status.classList.remove("hidden");
  status.className = "compare-status";
  status.textContent = written.length ? `寫入 ${written.length} 則到協作文件中…` : "寫入節次分隔線中…";
  try {
    // 寫入前在同一個協作操作裡重新讀一次文件，濾掉另一台電腦剛寫進去的那幾則。
    // 兩台同時開自動寫入時，比對跟寫入之間隔著產生關鍵字的那幾十秒，對方寫了什麼
    // 這邊看不到 —— 把確認挪到動手前一刻，重複寫入的窗口就從幾十秒縮到零點幾秒。
    // 每則稿分開傳（v0.3.14）：後端逐則判斷，不再從整串文字反推哪幾行是同一則。
    const prelist = await prelistForWrite(written);
    const res = await invoke("append_to_collab_doc_checked", {
      parts: writeParts(pieces, slugOf, (list) => buildOutputText(list.map(outputFieldsOf))),
      header: dateHeaderOwed || null,
      prelist,
      logNote: writeLogNote(written, dividers),
    });
    const msg = res.message ?? "整批都已經在文件裡了，這次沒有寫入。";
    if (res.dropped.length) {
      inewsStatus(`寫入前重新比對，已略過文件裡已有的 ${res.dropped.length} 則：${res.dropped.join("、")}`, "warn");
    }
    dateHeaderWritten();
    for (const i of written) {
      const key = writtenKey(i);
      if (key) writtenKeys.add(key);
      writtenSlugs.add(slugOf(i));
      writtenSlugs.add(`${slugOf(i)}|${i.inews?.version ?? ""}`);
      if (i.inews) writtenStories.set(i.inews.key, { slug: slugOf(i), show: i.inews.show });
      // 已經在文件裡了：取消勾選，下一輪比對也會得到同樣的結果。
      i.included = false;
      i.defaultIncluded = false;
      delete i.userIncluded;
      i.alreadyInDoc = true;
    }
    lastWrittenShow = lastShow;
    hideInewsConfirm();
    status.textContent = (written.length ? `✅ 自動寫入 ${written.length} 則：${msg}` : `✅ 已寫入節次分隔線：${msg}`) +
      (held.length ? `\n⏸ ${heldText(held)}` : "");
    renderFunnel();
    render();
  } catch (err) {
    if (isCollabNotReady(err)) {
      // 沒寫：稿子留著（還是待寫入），下一輪再試。
      status.textContent = `⏳ ${String(err).replace(COLLAB_NOT_READY, "").trim()}`;
      return;
    }
    stopInewsAuto(`⚠ 寫入協作文件失敗，自動刷新已停止：${err}\n請確認協作工具視窗已開啟並登入。`);
  }
}

// --- 未知樣式強制審查（v0.3.15）---
//
// 自動刷新時新出現的未知樣式（沒看過的 story key＋版本）排進佇列，一次跳一則擋住畫面
// 的審查視窗：寫入或略過，處理完才關。開窗時響一次、之後每 30 秒響一次，App 不在前景
// 時工作列閃爍。使用者處理的是播出過的稿，不是直播中，所以擋住畫面可以接受。

/** 已經審查過或排進佇列的 `${key}|${version}`。 */
const unknownSeen = new Set();
const unknownQueue = [];
/** 目前審查中的 { key, id }。 */
let reviewing = null;
let reviewBeepTimer = null;
let audioCtx = null;

function logDecision(line) {
  invoke("append_decision_log", { line }).catch(() => {});
}

function queueUnknownReviews() {
  if (inewsCfg.unknown_review === false) return;
  for (const t of unknownReviewTargets(items, unknownSeen)) {
    unknownSeen.add(t.key);
    unknownQueue.push(t);
    const item = items.find((x) => x.id === t.id);
    if (item) {
      const f = item.dto[item.kind] ?? item.dto;
      logDecision(`[未知樣式] ${f.slug} 樣式「${f.style || "空白"}」`);
    }
  }
  showNextUnknownReview();
}

// 審查提示音（v0.3.22）：用 Web Audio 合成，不用音檔。每個音是 [頻率 Hz, 開始秒, 長度秒]。
const REVIEW_SOUNDS = {
  double: { name: "雙響", wave: "sine", notes: [[880, 0, 0.2], [880, 0.28, 0.2]] },
  dingdong: { name: "叮咚", wave: "sine", notes: [[784, 0, 0.45], [523, 0.38, 0.7]] },
  rising: { name: "三連音", wave: "triangle", notes: [[523, 0, 0.16], [659, 0.17, 0.16], [784, 0.34, 0.3]] },
  alarm: { name: "警示", wave: "square", notes: [[880, 0, 0.12], [660, 0.15, 0.12], [880, 0.3, 0.12], [660, 0.45, 0.12]] },
  marimba: { name: "木琴", wave: "triangle", notes: [[1047, 0, 0.12], [1319, 0.13, 0.12], [1568, 0.26, 0.25]] },
};

function playReviewSound(kind, volume) {
  const sound = REVIEW_SOUNDS[kind] ?? REVIEW_SOUNDS.double;
  const peak = Math.min(1, Math.max(0, Number(volume ?? 60) / 100)) * 0.5;
  if (peak <= 0) return;
  try {
    audioCtx = audioCtx || new (window.AudioContext || window.webkitAudioContext)();
    audioCtx.resume?.();
    const t = audioCtx.currentTime;
    // 方波聽起來比正弦波大聲很多，壓低一點，各種音色同一個音量時差不多大聲。
    const level = sound.wave === "square" ? peak * 0.35 : peak;
    for (const [freq, start, len] of sound.notes) {
      const osc = audioCtx.createOscillator();
      const gain = audioCtx.createGain();
      osc.type = sound.wave;
      osc.frequency.value = freq;
      osc.connect(gain);
      gain.connect(audioCtx.destination);
      gain.gain.setValueAtTime(0.0001, t + start);
      gain.gain.exponentialRampToValueAtTime(level, t + start + 0.02);
      gain.gain.exponentialRampToValueAtTime(0.0001, t + start + len);
      osc.start(t + start);
      osc.stop(t + start + len + 0.02);
    }
  } catch {
    /* 沒有音效就只靠畫面 */
  }
}

function reviewBeep() {
  if (inewsCfg.unknown_review_sound === false) return;
  playReviewSound(inewsCfg.unknown_review_sound_kind, inewsCfg.unknown_review_volume);
}

function flashTaskbar() {
  try {
    const w = window.__TAURI__?.window;
    w?.getCurrentWindow?.().requestUserAttention(w.UserAttentionType?.Critical ?? 1);
  } catch {
    /* 閃不了也不影響審查 */
  }
}

function showNextUnknownReview() {
  if (reviewing) {
    renderUnknownReview();
    return;
  }
  while (unknownQueue.length && !reviewing) {
    const t = unknownQueue.shift();
    // 排隊期間稿子被抽掉或改成別的分類（例如比對後變成已在文件）就不用審了。
    if (items.some((x) => x.id === t.id && x.bucket === "unknown")) reviewing = t;
  }
  if (!reviewing) return;
  renderUnknownReview();
  reviewBeep();
  flashTaskbar();
  clearInterval(reviewBeepTimer);
  reviewBeepTimer = setInterval(reviewBeep, 30_000);
}

function renderUnknownReview() {
  const item = items.find((x) => x.id === reviewing?.id);
  if (!item) {
    // 審查中的卡片不見了（換節、稿子被抽掉）：換下一則。
    closeUnknownReview();
    showNextUnknownReview();
    return;
  }
  let box = el("unknownReview");
  if (!box) {
    box = document.createElement("div");
    box.id = "unknownReview";
    box.className = "modal-backdrop review-backdrop";
    document.body.appendChild(box);
  }
  const f = item.dto[item.kind] ?? item.dto;
  const reasons = (f.warnings ?? []).join("；");
  const body = (item.body ?? "").trim();
  box.innerHTML = `
    <div class="modal review-modal" role="alertdialog" aria-modal="true" aria-labelledby="reviewTitle">
      <div class="modal-head"><h2 id="reviewTitle">⚠ 未知樣式，需要人工審查</h2></div>
      <div class="modal-body">
        <p class="review-slug">${escapeHtml(f.slug)}</p>
        <p>樣式：${escapeHtml(f.style || "（空白）")}　累積時間：${escapeHtml(f.time || "—")}</p>
        ${reasons ? `<p class="review-reason">${escapeHtml(reasons)}</p>` : ""}
        ${item.title ? `<p><b>${escapeHtml(item.title)}</b></p>` : ""}
        <p class="review-body">${escapeHtml(body.length > 160 ? `${body.slice(0, 160)}…` : body || "（沒有內文）")}</p>
      </div>
      <div class="modal-foot">
        <button id="reviewWrite" class="primary">寫入</button>
        <button id="reviewSkip">略過</button>
        ${unknownQueue.length ? `<span class="review-more">還有 ${unknownQueue.length} 則待審查</span>` : ""}
      </div>
    </div>`;
  box.querySelector("#reviewWrite").addEventListener("click", () => resolveUnknownReview(true));
  box.querySelector("#reviewSkip").addEventListener("click", () => resolveUnknownReview(false));
  box.style.display = "flex";
}

function resolveUnknownReview(write) {
  const item = items.find((x) => x.id === reviewing?.id);
  if (item) {
    item.included = write;
    // 比對每輪都會重算勾選：照審查結果，不要被洗掉。
    item.defaultIncluded = write;
    item.userIncluded = write;
    logDecision(`[審查] ${slugOf(item)} → ${write ? "寫入" : "略過"}`);
  }
  closeUnknownReview();
  renderFunnel();
  render();
  showNextUnknownReview();
}

function closeUnknownReview() {
  reviewing = null;
  clearInterval(reviewBeepTimer);
  reviewBeepTimer = null;
  const box = el("unknownReview");
  if (box) box.style.display = "none";
}

// --- 當天 rundown 的 slug 搜尋（v0.3.15）---
//
// 讀 App 每一輪存在本地的 rundown 檔（剪輯工具用的同一份），不打 iNEWS。結果唯讀。

let searchTimer = null;
let searchSeq = 0;
const SEARCH_MAX_ROWS = 300;

function openRundownSearch() {
  let box = el("rundownSearch");
  if (!box) {
    box = document.createElement("div");
    box.id = "rundownSearch";
    box.className = "modal-backdrop";
    box.innerHTML = `
      <div class="modal search-modal" role="dialog" aria-modal="true" aria-labelledby="searchTitle">
        <div class="modal-head">
          <h2 id="searchTitle">搜尋今天的 rundown</h2>
          <button class="modal-close" id="searchClose" title="關閉">✕</button>
        </div>
        <div class="modal-body">
          <input id="searchInput" class="search-input" type="search" autocomplete="off"
            placeholder="輸入 slug 關鍵字；空白分開＝每個都要符合" />
          <p id="searchMeta" class="search-meta"></p>
          <div id="searchResults"></div>
        </div>
      </div>`;
    document.body.appendChild(box);
    const close = () => { box.style.display = "none"; };
    el("searchClose").addEventListener("click", close);
    box.addEventListener("click", (e) => { if (e.target === box) close(); });
    box.addEventListener("keydown", (e) => { if (e.key === "Escape") close(); });
    el("searchInput").addEventListener("input", () => {
      clearTimeout(searchTimer);
      searchTimer = setTimeout(runRundownSearch, 200);
    });
  }
  box.style.display = "flex";
  el("searchInput").focus();
  runRundownSearch();
}

async function runRundownSearch() {
  const query = el("searchInput").value;
  const meta = el("searchMeta");
  const out = el("searchResults");
  const seq = ++searchSeq;
  try {
    const r = await invoke("search_rundown_slugs", { query });
    if (seq !== searchSeq) return; // 打字很快時，舊的結果晚回來就丟掉
    meta.textContent = r.shows
      ? `播出日 ${r.date}：App 今天讀過 ${r.shows} 節、${r.stories} 則稿（App 沒開的時段不會有）。`
      : `播出日 ${r.date}：今天還沒有存下任何 rundown（App 開著自動刷新才會存）。`;
    if (!query.trim()) {
      out.innerHTML = "";
      return;
    }
    if (!r.hits.length) {
      out.innerHTML = `<p class="search-empty">找不到符合的 slug。</p>`;
      return;
    }
    searchHits = r.hits.slice(0, SEARCH_MAX_ROWS);
    const rows = searchHits.map((h, n) => `
      <tr>
        <td>${escapeHtml(h.show)}</td>
        <td>${escapeHtml(h.air_time || "—")}</td>
        <td>${escapeHtml(h.slug)}</td>
        <td>${escapeHtml(h.style || "—")}</td>
        <td>${escapeHtml((h.updated || "").slice(11) || "—")}</td>
        <td>${escapeHtml(cardStateOf(h.slug))}</td>
        <td><button class="search-load" data-hit="${n}">載入</button></td>
      </tr>`).join("");
    out.innerHTML =
      `<p class="search-meta">找到 ${r.hits.length} 則${r.hits.length > SEARCH_MAX_ROWS ? `，只顯示前 ${SEARCH_MAX_ROWS} 則` : ""}。</p>` +
      `<table class="search-table"><thead><tr><th>節次</th><th>播出時間</th><th>slug</th><th>樣式</th><th>iNEWS 修改</th><th>畫面上的卡片</th><th></th></tr></thead><tbody>${rows}</tbody></table>`;
    out.querySelectorAll(".search-load").forEach((btn) => {
      btn.addEventListener("click", () => loadSearchHit(searchHits[Number(btn.dataset.hit)], btn));
    });
  } catch (err) {
    if (seq !== searchSeq) return;
    meta.textContent = `⚠ 搜尋失敗：${err}`;
    out.innerHTML = "";
  }
}

let searchHits = [];

/**
 * 把一則搜尋結果載入成卡片（v0.3.15）：後端先讀搜尋紀錄存的原文，沒有才到 iNEWS 讀。
 * 卡片標成「手動載入」（虛線框），只活在目前這一節；勾選後照一般流程比對、產生關鍵字、
 * 寫在文件最下面（不寫分隔線）。
 */
async function loadSearchHit(hit, btn) {
  if (!hit) return;
  const meta = el("searchMeta");
  btn.disabled = true;
  btn.textContent = "載入中…";
  try {
    const res = await invoke("load_rundown_story", {
      show: hit.show,
      slug: hit.slug,
      id: hit.id,
      version: hit.version,
      airTime: hit.air_time,
    });
    const id = `inews-${res.key}`;
    if (items.some((i) => i.id === id)) {
      meta.textContent = `「${hit.slug}」已經在卡片裡了。`;
      btn.textContent = "已在卡片";
      return;
    }
    const entry = res.summary.entries?.[0];
    if (!entry) throw new Error("讀到的稿件沒有內容");
    const item = itemFromDto(entry, items.length);
    item.id = id;
    item.inews = { key: res.key, version: res.version, show: res.show, hold: null, recheck: false, manual: true, manualFor: inewsShow };
    // 手動載入就是要寫：可以輸出的分類直接勾上，而且比對不會再取消（未知樣式照樣會跳審查）。
    if (canOutputBucket(item.bucket)) {
      item.included = true;
      item.defaultIncluded = true;
      item.userIncluded = true;
    }
    items = sortByTime([...items, item]);
    hasCompared = false;
    updateStatsFromItems();
    renderFunnel();
    render();
    meta.textContent = `已載入「${hit.slug}」（${hit.show} ${hit.air_time || ""}，從${res.source}），下一輪會比對、寫入。`;
    btn.textContent = "已載入";
  } catch (err) {
    meta.textContent = `⚠ 載入失敗：${err}`;
    btn.disabled = false;
    btn.textContent = "載入";
  }
}

/** 這個 slug 在畫面上的卡片狀態；沒有卡片回「—」。 */
function cardStateOf(slug) {
  const card = items.find((i) => slugOf(i) === slug);
  if (!card) return "—";
  if (card.alreadyInDoc) return "已在文件中";
  if (card.prelisted) return "預列(X)";
  return { to_cut: "待處理", keep_refresh: "要重貼", removed: "已剔除" }[card.matchStatus] ?? "有卡片（還沒比對）";
}

el("rundownSearchBtn")?.addEventListener("click", openRundownSearch);

/** 協作工具還在載入或沒連上時，讀取文件的腳本回傳的錯誤帶這個記號（v0.3.15）。 */
const COLLAB_NOT_READY = "[未就緒]";
function isCollabNotReady(err) {
  return String(err).includes(COLLAB_NOT_READY);
}

async function confirmInewsWrite() {
  const btn = el("inewsConfirmBtn");
  btn.disabled = true;
  try {
    // 用按下去這一刻的狀態重算，不用顯示時那份：中間可能比對過或改過勾選。
    const ready = autoWriteCandidates(items, writtenKeys, noUploadLabel).filter((i) => !beforeWriteStartShow(i));
    if (ready.length === 0 && !dividerOwed()) {
      hideInewsConfirm();
      return;
    }
    await writeInewsItems(ready);
  } finally {
    btn.disabled = false;
  }
}

function scheduleInewsRound(delayMs) {
  clearTimeout(inewsTimer);
  if (!el("inewsAutoToggle").checked) return;
  inewsTimer = setTimeout(async () => {
    await inewsRound();
    const secs = Math.max(3, Number(inewsCfg.poll_seconds) || 10);
    scheduleInewsRound(secs * 1000);
  }, delayMs);
}

async function toggleInewsAuto() {
  if (!el("inewsAutoToggle").checked) {
    stopInewsAuto();
    inewsStatus("自動刷新已關閉。");
    return;
  }
  try {
    const cred = await invoke("get_inews_credential_status");
    if (!cred.has_credentials) {
      stopInewsAuto("⚠ 尚未設定 iNEWS 帳號密碼，請到「⚙ 設定 → iNEWS」輸入並按「測試連線」。");
      return;
    }
  } catch (err) {
    stopInewsAuto(`⚠ ${err}`);
    return;
  }
  inewsSessionStart = true;
  inewsStatus(`自動刷新已開啟，每 ${inewsCfg.poll_seconds} 秒讀一次 iNEWS。` +
    (inewsCfg.auto_write ? "（全自動寫入）" : "（產生關鍵字後等你按「確認寫入」）"));
  scheduleInewsRound(0);
}

el("importInewsBtn").addEventListener("click", importInewsOnce);
el("inewsAutoToggle").addEventListener("change", toggleInewsAuto);
el("inewsConfirmBtn").addEventListener("click", confirmInewsWrite);

/// These two live in the settings modal, so their output has to land inside it --
/// writing to the main compare banner would put the result behind the backdrop.
function collabToolStatusEl() {
  const inModal = el("collabToolStatus");
  if (inModal) return { node: inModal, inModal: true };
  const status = el("compareStatus");
  status.classList.remove("hidden");
  status.className = "compare-status";
  return { node: status, inModal: false };
}

function setCollabToolStatus(target, text, isError) {
  target.node.textContent = text;
  if (target.inModal) {
    target.node.style.color = isError ? "var(--danger)" : "var(--muted)";
  } else {
    target.node.className = isError ? "compare-status error" : "compare-status";
  }
}

async function exportCollabText() {
  const target = collabToolStatusEl();
  setCollabToolStatus(target, "抓取協作工具內容中…", false);
  try {
    const text = await invoke("export_collab_text");
    const lines = text.split("\n").length;
    await invoke("save_text_file", { defaultName: "協作工具內容.txt", content: text });
    setCollabToolStatus(target, `已抓到 ${lines} 行內容，請選擇存檔位置後開啟確認。`, false);
  } catch (err) {
    setCollabToolStatus(target, `⚠ 抓取失敗：${err}`, true);
  }
}

async function diagnoseCollab() {
  const target = collabToolStatusEl();
  setCollabToolStatus(target, "診斷中…", false);
  try {
    const probe = await invoke("diagnose_collab_bridge");
    setCollabToolStatus(target, `診斷結果：${probe}`, false);
  } catch (err) {
    setCollabToolStatus(target, `⚠ 診斷失敗：${err}`, true);
  }
}

async function openCollab(status = el("compareStatus")) {
  try {
    await invoke("open_collab_window");
  } catch (err) {
    status.textContent = `⚠ 開啟協作工具視窗失敗：${err}`;
    status.className = "compare-status error";
  }
}

async function compareWithCollabDoc({ silent = false } = {}) {
  const status = el("compareStatus");
  status.classList.remove("hidden");
  status.className = "compare-status";
  status.textContent = "比對中…";
  hasCompared = false;

  // 已濾除 is compared too: it can be rescued by a tick, and an entry that can be
  // written out has to be checked against the doc like any other, or the duplicate
  // guard would have nothing to go on for exactly the entries a human picked by hand.
  const targets = items.filter((i) => canOutputBucket(i.bucket));
  // 累積時間 goes along so a (抓新) line is only a re-grab for a *different* version;
  // the title so a slug reused from yesterday only counts as a re-run when the
  // headline matches too.
  const entries = targets.map((i) => {
    const f = i.dto[i.kind] ?? i.dto;
    // 節次：沒有累積時間的卡片（手動載入）判斷 (抓18) 用。搜尋載入的算在載入當時跟的
    // 那一節，其他手動卡片算現在這一節。
    const show = i.inews?.manualFor || i.inews?.show || inewsShow || "";
    return { slug: f.slug, time: f.time ?? "", title: i.title ?? "", show };
  });

  try {
    const { results, day } = await invoke("compare_with_collab_doc", { entries });
    compareDay = day.today;
    compareHoldsAutoWrite = !!day.hold_auto_write;
    dateHeaderOwed = day.header_to_add && dateHeaderWrittenFor !== day.today ? day.header_to_add : "";
    prelistWanted = !!day.prelist_wanted;
    const matched = [];
    // Paired by position, not slug: two versions of one slug can now differ.
    for (const [n, item] of targets.entries()) {
      const r = results[n];
      if (!r) continue;
      item.matchStatus = r.status;
      item.matchedLine = r.matched_line;
      item.crossDay = !!r.cross_day;
      item.prelisted = !!r.prelisted;
      item.alreadyInDoc = isAlreadyInDoc(r);
      item.included = decideInclusion(r, item.defaultIncluded, item.userIncluded);
      // 已濾除 entries are matched so their badge and the duplicate guard are right,
      // but they stay out of the tally: it reads as "how much of today's run is left
      // to do", and BS stories are not part of that unless someone rescues one.
      if (isOutputBucket(item.bucket) || item.included) matched.push(r);
    }
    const { toCut, keepRefresh, removed, alreadyIn } = summarizeMatches(matched);
    hasCompared = true;
    const autoUnticked = removed + alreadyIn;
    const crossDay = targets.filter((i) => i.crossDay).length;
    status.className = day.warning ? "compare-status warn" : "compare-status";
    status.textContent =
      `比對完成（今天 ${day.today} 的段落）：待處理 ${toCut}・要重貼 ${keepRefresh}・已剔除 ${removed}・已在文件中 ${alreadyIn}` +
      (crossDay > 0 ? `（其中 ${crossDay} 則是前一天已上傳的重播）` : "") +
      (autoUnticked > 0 ? `（已剔除／已在文件中的 ${autoUnticked} 則已自動取消勾選，可手動改回）` : "") +
      (dateHeaderOwed ? `\n今天的日期分隔線還不存在，第一次寫入時會先補上「${dateHeaderOwed}」。` : "") +
      (day.warning ? `\n⚠ ${day.warning}` : "");
    renderFunnel();
    render();
  } catch (err) {
    status.className = "compare-status error";
    status.textContent = `⚠ 比對失敗：${err}`;
    if (!silent) throw err;
  }
}
document.querySelectorAll(".stat").forEach((btn) => {
  btn.addEventListener("click", () => {
    activeFilter = btn.dataset.filter;
    document.querySelectorAll(".stat").forEach((b) => b.classList.toggle("active", b === btn));
    render();
  });
});

async function refreshApiKeyStatus() {
  const status = await invoke("get_api_key_status");
  hasApiKey = status.has_key;
  updateKeywordButton();
  renderEvening();
  return status;
}

// --- Updates ---
//
// Downloads are verified against the public key baked into tauri.conf.json before
// anything is installed, so a tampered or third-party build is rejected outright.

async function showCurrentVersion() {
  try {
    const version = await window.__TAURI__.app.getVersion();
    el("versionLabel").textContent = `目前版本 v${version}`;
  } catch {
    el("versionLabel").textContent = "";
  }
}

/// `silent` is for the automatic check on startup: it must never interrupt with a
/// dialog when there is nothing to report or when the network is simply unreachable.
async function checkForUpdate({ silent }) {
  const status = el("updateStatus");
  const setStatus = (t) => { if (status) status.textContent = t; };
  setStatus("檢查中…");

  let update;
  try {
    update = await window.__TAURI__.updater.check();
  } catch (err) {
    if (!silent) setStatus(`⚠ 檢查更新失敗：${err}`);
    return;
  }

  if (!update) {
    if (!silent) setStatus("已經是最新版本。");
    return;
  }

  const ok = window.confirm(
    `有新版本 v${update.version}（目前 v${update.currentVersion}）。\n\n` +
      (update.body ? `更新內容：\n${update.body}\n\n` : "") +
      "要現在下載並安裝嗎？安裝完成後工具會自動重新啟動。"
  );
  if (!ok) {
    setStatus(`有新版本 v${update.version}，可隨時回到這裡更新。`);
    return;
  }

  try {
    let downloaded = 0;
    let total = 0;
    await update.downloadAndInstall((event) => {
      if (event.event === "Started") {
        total = event.data.contentLength ?? 0;
        showProgress(0, total || 1, "下載更新中…");
      } else if (event.event === "Progress") {
        downloaded += event.data.chunkLength ?? 0;
        showProgress(downloaded, total || 1, total
          ? `下載更新 ${Math.round((downloaded / total) * 100)}%`
          : "下載更新中…");
      } else if (event.event === "Finished") {
        showProgress(1, 1, "安裝中…");
      }
    });
    hideProgress();
    setStatus("更新完成，即將重新啟動…");
    await window.__TAURI__.process.relaunch();
  } catch (err) {
    hideProgress();
    setStatus(`⚠ 更新失敗：${err}`);
  }
}

// Same two actions run many times a day; the guard keeps them from firing while the
// settings modal is open or mid-typing in a field.
document.addEventListener("keydown", (e) => {
  if (!e.ctrlKey || e.altKey || e.shiftKey) return;
  if (el("settingsBackdrop")) return;
  // Both shortcuts drive the 文稿 screen; firing them from 晚報 would write into the
  // shared doc from a screen that is meant to be read-only.
  if (viewMode !== "script") return;

  if (e.key === "Enter") {
    if (el("writeBackBtn").disabled) return;
    e.preventDefault();
    writeBackToCollab();
  } else if (e.key === "r" || e.key === "R") {
    if (el("compareBtn").disabled) return;
    e.preventDefault();
    compareWithCollabDoc();
  }
});

refreshApiKeyStatus();
initFromConfig();
// Silent so a missing network or an up-to-date install never nags on launch; it only
// speaks up when there is genuinely a new version to offer.
checkForUpdate({ silent: true });

// --- Settings modal ---

const PUNCT_LABELS = { ",": "， (逗號)", ":": "： (冒號)", ";": "； (分號)", "?": "？ (問號)", "!": "！ (驚嘆號)", "(": "（ (左括號)", ")": "） (右括號)" };

function settingsModalHtml(config, apiStatus, inewsCred) {
  const inewsSettings = config.inews ?? {};
  const styleField = (label, key, value) => `
    <div class="field">
      <label>${label}（逗號分隔）</label>
      <input type="text" data-cfg="${key}" value="${escapeHtml(value.join(", "))}" />
    </div>`;

  const punctRows = Object.entries(config.punctuation.map)
    .map(([half, full]) => `
      <div class="settings-punct-row">
        <span class="settings-punct-half">${escapeHtml(half)}</span>
        <span>→</span>
        <input type="text" data-punct-key="${escapeHtml(half)}" value="${escapeHtml(full)}" size="3" />
        <span class="muted">${PUNCT_LABELS[half] ?? ""}</span>
      </div>`)
    .join("");

  // 每一組可以收合，一次只展開一組（v0.3.22）；「版本與更新」固定展開。
  const sec = (id, title, body) => `
          <section class="settings-section collapsible" data-sec="${id}">
            <h3 class="settings-toggle" role="button" tabindex="0" aria-expanded="false"><span class="settings-caret">▸</span>${title}</h3>
            <div class="settings-body">${body}</div>
          </section>`;
  const check = (path, on, text) =>
    `<label class="checkbox-row"><input type="checkbox" data-cfg-bool="${path}" ${on ? "checked" : ""} /> ${text}</label>`;
  const num = (label, path, value, min = 0) =>
    `<div class="field"><label>${label}</label><input type="number" min="${min}" data-cfg-num="${path}" value="${value}" /></div>`;
  const text = (label, path, value) =>
    `<div class="field"><label>${label}</label><input type="text" data-cfg="${path}" value="${escapeHtml(value ?? "")}" /></div>`;
  const s = inewsSettings;
  const soundKind = s.unknown_review_sound_kind || "double";
  const volume = s.unknown_review_volume ?? 60;

  return `
    <div class="modal-backdrop" id="settingsBackdrop">
      <div class="modal">
        <div class="modal-head">
          <h2>設定</h2>
          <button class="modal-close" id="settingsCloseBtn">✕</button>
        </div>
        <div class="modal-body">
          <section class="settings-section">
            <h3>版本與更新</h3>
            <div class="field">
              <div class="keywords-row">
                <button id="checkUpdateBtn" type="button">檢查更新</button>
                <span id="versionLabel" class="muted"></span>
              </div>
              <div id="updateStatus" class="muted"></div>
            </div>
          </section>
${sec("gemini", "Gemini API Key", `
            <div class="field">
              <label>
                狀態：
                ${apiStatus.has_key
                  ? `<span class="badge passed">已設定（${apiStatus.source === "keyring" ? "系統憑證管理員" : "環境變數"}）</span>`
                  : `<span class="badge failed">尚未設定</span>`}
              </label>
              <div class="keywords-row">
                <input type="password" id="apiKeyInput" placeholder="貼上 Gemini API key" />
                <button id="saveApiKeyBtn">儲存</button>
                <button id="clearApiKeyBtn">清除</button>
              </div>
              <div class="muted">存在系統憑證管理員，不寫進設定檔。</div>
              <div id="apiKeyError" class="w"></div>
            </div>
            ${text("模型", "gemini.model", config.gemini.model)}
            ${num("單次產生上限（則，0 = 不限）", "gemini.max_per_run", config.gemini.max_per_run)}
            <div class="muted">免費方案每分鐘有上限，超過的等一分鐘再按一次接續。</div>`)}
${sec("import", "匯入資料夾", `
            <div class="field">
              <label>預設匯入資料夾</label>
              <div class="keywords-row">
                <input type="text" id="defaultFolderInput" data-cfg="import.default_folder" value="${escapeHtml(config.import.default_folder)}" placeholder="（未設定）" />
                <button id="browseDefaultFolderBtn" type="button">瀏覽</button>
              </div>
              <div class="muted">「匯入資料夾」和開啟時的自動匯入都用這裡。</div>
            </div>`)}
${sec("inews", "iNEWS 連線", `
            <div class="field">
              <label>
                帳號密碼：
                ${inewsCred.has_credentials
                  ? `<span class="badge passed">已設定（${escapeHtml(inewsCred.username)}）</span>`
                  : `<span class="badge failed">尚未設定</span>`}
              </label>
              <div class="keywords-row">
                <input type="text" id="inewsUserInput" placeholder="iNEWS 帳號" autocomplete="off" value="${escapeHtml(inewsCred.username)}" />
                <input type="password" id="inewsPassInput" placeholder="iNEWS 密碼" autocomplete="off" />
                <button id="saveInewsCredBtn" type="button">儲存</button>
                <button id="clearInewsCredBtn" type="button">清除</button>
                <button id="testInewsBtn" type="button">測試連線</button>
              </div>
              <div class="muted">存在系統憑證管理員。改了伺服器先按「儲存設定」再測試。</div>
              <div id="inewsCredStatus" class="muted"></div>
            </div>
            ${text("伺服器", "inews.server", s.server)}
            ${num("連接埠", "inews.port", s.port, 1)}
            ${text("節次所在路徑（點號串接）", "inews.root", s.root)}
            ${text("Queue", "inews.queue", s.queue)}
            ${num("自動刷新間隔（秒）", "inews.poll_seconds", s.poll_seconds, 3)}
            ${text("預列清單 queue（空白＝不補）", "inews.prelist_queue", s.prelist_queue ?? "51新聞台.STORIES.LOCAL.APPR")}
            <div class="muted">今天段落還空著時，第一次寫入前把它最上面那段寫成 <code>(X)slug</code>。</div>
            <div class="field">
              <label>決策紀錄（保留 14 天）</label>
              <div class="keywords-row"><button id="openInewsLogBtn" type="button">開啟紀錄資料夾</button><span id="inewsLogStatus" class="muted"></span></div>
            </div>`)}
${sec("slots", "節次判定", `
            ${num("沒有可信硬時間時，整點前幾分鐘換節", "inews.lead_minutes", s.lead_minutes)}
            <div class="muted">節次從第一個硬時間開始（整點前 30 分～後 15 分內才採用）。</div>
            <div class="field"><label>跨節合併（例：1200=1400, 1800=2000）</label><input type="text" data-cfg-spans="inews.slot_spans" value="${escapeHtml(formatSlotSpans(s.slot_spans))}" /></div>
            ${check("inews.off_air_on_empty", s.off_air_on_empty !== false, "空節次當停播（2200 關鍵時刻、0100 收播）")}
            <div class="field"><label>停播節目（slug 開頭=名稱，例：關鍵KS=關鍵時刻）</label><input type="text" data-cfg-programs="inews.programs" value="${escapeHtml(Object.entries(s.programs ?? {}).map(([a, b]) => `${a}=${b}`).join(", "))}" /></div>
            ${text("節目分隔線（{hh} 小時、{name} 名稱）", "inews.program_divider_format", s.program_divider_format ?? "==== {hh} {name} ====")}
            ${num("換節後幾分鐘補掃上一節", "inews.sweep_delay_minutes", s.sweep_delay_minutes ?? 10)}
            ${check("inews.hold_after_open_live", s.hold_after_open_live !== false, "0:00 的直進／LIVE 播出後，後面的稿等新的硬時間")}
            ${num("直進暫緩上限（分鐘）", "inews.open_live_max_minutes", s.open_live_max_minutes ?? 30, 1)}
            ${text("播出日分界（HH:MM）", "inews.broadcast_day_start", s.broadcast_day_start ?? "02:00")}
            ${text("每天開始寫入（HH:MM，空白＝不限制）", "inews.write_start", s.write_start ?? "07:00")}
            <div class="muted">播出日分界到這個時間之前只讀不寫；時間到了從當時的節次開始寫，跟那時才打開工具一樣。</div>
            ${text("節次分隔線（{hh} 小時、{show} 節次）", "inews.divider_format", s.divider_format)}
            <div class="field">
              <label>分隔線位置</label>
              <select data-cfg="inews.divider_position">
                <option value="after" ${s.divider_position === "after" ? "selected" : ""}>節次結束後</option>
                <option value="before" ${s.divider_position === "before" ? "selected" : ""}>節次開始前</option>
              </select>
            </div>
            ${check("inews.cutoff_by_marker", s.cutoff_by_marker, "標記式截止（舊規則，建議關閉）")}
            ${text("標記式截止：預告關鍵字", "inews.cutoff_preview_keyword", s.cutoff_preview_keyword)}
            ${text("標記式截止：廣告列序號開頭", "inews.cutoff_page_prefix", s.cutoff_page_prefix)}`)}
${sec("write", "寫入與比對", `
            ${check("inews.auto_write", s.auto_write, "全自動寫入協作文件（不勾＝等你按「確認寫入」）")}
            ${check("inews.auto_date_header", s.auto_date_header !== false, "沒有今天的日期分隔線時自動補上")}
            ${check("inews.cross_day_title_match", s.cross_day_title_match !== false, "前一天 slug＋標題相同、有處理標記才算重播")}
            ${check("inews.include_floated", s.include_floated, "也匯入 float 列")}
            ${check("inews.fill_computed_time", s.fill_computed_time, "累積時間空白時填入算出的時間")}
            ${num("新稿連續幾輪判定已播出才寫入（1 = 不等）", "inews.write_stable_rounds", s.write_stable_rounds ?? 2, 1)}
            ${num("rundown 異常門檻（秒）", "inews.anomaly_threshold_seconds", s.anomaly_threshold_seconds ?? 120, 1)}
            ${num("異常後連續幾輪穩定才恢復", "inews.anomaly_quiet_rounds", s.anomaly_quiet_rounds ?? 3, 1)}
            ${num("異常冷卻上限（分鐘）", "inews.anomaly_max_cooldown_minutes", s.anomaly_max_cooldown_minutes ?? 15, 1)}
            ${num("定時複查已結束節次（分鐘，0 = 關閉）", "inews.recheck_interval_minutes", s.recheck_interval_minutes ?? 30)}
            ${check("inews.correct_time", s.correct_time !== false, "時間校正：已寫入的稿時間跟著 rundown 改")}
            ${num("時間校正：連續幾輪一樣才改", "inews.correct_time_stable_rounds", s.correct_time_stable_rounds ?? 2, 1)}
            ${num("時間校正：差超過幾分鐘不改（疑似挪稿）", "inews.correct_time_max_delta_minutes", s.correct_time_max_delta_minutes ?? 10, 1)}`)}
${sec("review", "審查與提示音", `
            ${check("inews.unknown_review", s.unknown_review !== false, "未知樣式跳出審查視窗（寫入或略過）")}
            ${check("inews.unknown_review_sound", s.unknown_review_sound !== false, "審查視窗開著時每 30 秒響一次")}
            <div class="field">
              <label>提示音</label>
              <div class="keywords-row">
                <select id="soundKindSelect" data-cfg="inews.unknown_review_sound_kind">
                  ${Object.entries(REVIEW_SOUNDS).map(([id, snd]) => `<option value="${id}" ${soundKind === id ? "selected" : ""}>${snd.name}</option>`).join("")}
                </select>
                <button id="previewSoundBtn" type="button">試聽</button>
              </div>
            </div>
            <div class="field">
              <label>音量 <span id="volumeLabel">${volume}</span></label>
              <input type="range" id="soundVolumeInput" min="0" max="100" step="5" data-cfg-num="inews.unknown_review_volume" value="${volume}" />
            </div>`)}
${sec("theme", "外觀主題", `
            <div class="field">
              <select id="themeSelect" data-cfg="ui.theme">
                ${THEMES.map((t) => `<option value="${t.id}" ${config.ui.theme === t.id ? "selected" : ""}>${t.name}</option>`).join("")}
              </select>
              <div class="muted">立即預覽，按「儲存設定」才固定。</div>
            </div>`)}
${sec("collab", "協作工具維護", `
            <div class="field">
              <div class="keywords-row">
                <button id="diagnoseCollabBtn" type="button">診斷協作連線</button>
                <button id="exportCollabBtn" type="button">匯出協作內容</button>
              </div>
              <div class="muted">比對或寫入出問題時用；匯出可在寫入前留一份 txt 備份。</div>
              <div id="collabToolStatus" class="muted"></div>
            </div>`)}
${sec("filter", "樣式篩選", `
            ${styleField("保留白名單", "filter.allowed_styles", config.filter.allowed_styles)}
            ${styleField("排除黑名單", "filter.blocked_styles", config.filter.blocked_styles)}
            ${styleField("排除的 slug 結尾", "filter.excluded_slug_suffixes", config.filter.excluded_slug_suffixes)}
            ${styleField("可寫入但要標記", "filter.flag_styles", config.filter.flag_styles)}
            ${text("標題標記（regex）", "filter.title_tag_pattern", config.filter.title_tag_pattern)}
            ${text("標題標記備援（regex，上面找不到才用）", "filter.title_tag_fallback_pattern", config.filter.title_tag_fallback_pattern)}
            ${styleField("樣式空白時看 slug（可寫「詞=樣式」）", "filter.slug_style_terms", config.filter.slug_style_terms)}
            ${styleField("樣式空白時看影像編號尾碼（尾碼=樣式）", "filter.video_id_styles", config.filter.video_id_styles)}
            ${styleField("slug 以這些字開頭一律通過（例：專）", "filter.force_pass_slug_prefixes", config.filter.force_pass_slug_prefixes ?? ["專"])}
            ${check("filter.blank_style_timecode_pass", config.filter.blank_style_timecode_pass !== false, "樣式空白、slug 以時間標結尾的當新聞通過")}
            ${styleField("樣式空白、slug 含這些詞且有內文就通過", "filter.blank_style_body_pass_terms", config.filter.blank_style_body_pass_terms ?? ["氣象"])}
            ${styleField("slug 含這些詞沒有內文也寫入", "filter.no_body_pass_terms", config.filter.no_body_pass_terms ?? ["氣象"])}
            ${styleField("編輯備註含這些詞就濾除", "filter.blocked_note_terms", config.filter.blocked_note_terms)}`)}
${sec("annotations", "備註標記", `
            ${styleField("勿上網 同義詞", "annotations.no_upload_terms", config.annotations.no_upload_terms)}
            ${text("顯示標籤", "annotations.no_upload_label", config.annotations.no_upload_label)}
            ${styleField("版權問題 同義詞", "annotations.copyright_terms", config.annotations.copyright_terms)}
            ${text("顯示標籤", "annotations.copyright_label", config.annotations.copyright_label)}
            ${styleField("可上網 同義詞", "annotations.allowed_upload_terms", config.annotations.allowed_upload_terms)}
            ${text("顯示標籤", "annotations.allowed_upload_label", config.annotations.allowed_upload_label)}
            <div class="muted">只比對完整詞，不看單一個字。</div>`)}
${sec("prefix", "標題前綴", `
            ${styleField("判定獨家的備註關鍵字", "annotations.exclusive_terms", config.annotations.exclusive_terms)}
            ${text("獨家前綴", "annotations.exclusive_prefix", config.annotations.exclusive_prefix)}
            ${styleField("判定最新的樣式", "annotations.latest_styles", config.annotations.latest_styles)}
            ${text("最新前綴", "annotations.latest_prefix", config.annotations.latest_prefix)}
            <div class="muted">兩者都成立時只加獨家。</div>`)}
${sec("evening", "晚報", `
            ${styleField("主播名單", "evening.anchors", config.evening.anchors)}
            ${styleField("代表「不上網」的標記", "evening.no_online_markers", config.evening.no_online_markers)}
            ${styleField("不上晚報的樣式", "evening.blocked_styles", config.evening.blocked_styles ?? ["SL"])}
            ${styleField("不上晚報的專題標記", "evening.excluded_terms", config.evening.excluded_terms ?? ["專"])}
            <div class="muted">只收今天段落裡有上網的稿；只影響晚報。</div>`)}
${sec("clean", "內文雜訊清除", `
            ${check("clean.strip_marker_symbols", config.clean.strip_marker_symbols, "移除行首雜訊符號（<code>..早安</code> → <code>早安</code>）")}
            ${check("clean.drop_non_cjk_lines", config.clean.drop_non_cjk_lines, "刪除整行沒有中文的備註行")}
            ${check("clean.strip_trailing_symbols", config.clean.strip_trailing_symbols, "移除內文結尾符號（<code>。##</code> → <code>。</code>）")}`)}
${sec("markers", "抓新標記", `
            ${styleField("代表「要重抓」的關鍵字", "markers.refresh_keywords", config.markers.refresh_keywords)}
            <div class="muted">帶時間的（抓18、(19)）要等播出時間到了才抓。</div>`)}
${sec("punct", "標點轉換", `
            ${check("punctuation.quotes_to_corner", config.punctuation.quotes_to_corner, "直引號轉「」")}
            ${check("punctuation.title_dot_to_enumeration", config.punctuation.title_dot_to_enumeration, "標題 . 轉頓號")}
            ${check("punctuation.dot_to_enumeration", config.punctuation.dot_to_enumeration, "內文 . 轉頓號（數字間除外）")}
            ${check("punctuation.protect_urls", config.punctuation.protect_urls, "網址不轉換")}
            ${check("punctuation.preserve_halfwidth_space", config.punctuation.preserve_halfwidth_space, "保留半形空格")}
            <div class="field">
              <label>半形 → 全形</label>
              ${punctRows}
            </div>`)}
${sec("output", "輸出格式", `
            ${num("關鍵字數量", "output.keyword_count", config.output.keyword_count, 1)}
            ${text("關鍵字分隔符號", "output.keyword_separator", config.output.keyword_separator)}
            ${num("則與則之間空白行數", "output.entry_blank_lines", config.output.entry_blank_lines)}`)}
        </div>
        <div class="modal-foot">
          <button id="resetConfigBtn">還原預設值</button>
          <span class="spacer"></span>
          <span id="settingsStatus" class="muted"></span>
          <button id="saveConfigBtn">儲存設定</button>
        </div>
      </div>
    </div>`;
}

const LIST_FIELDS = new Set([
  "filter.allowed_styles",
  "filter.blocked_styles",
  "filter.excluded_slug_suffixes",
  "filter.flag_styles",
  "filter.slug_style_terms",
  "filter.force_pass_slug_prefixes",
  "filter.video_id_styles",
  "filter.blank_style_body_pass_terms",
  "filter.no_body_pass_terms",
  "filter.blocked_note_terms",
  "markers.refresh_keywords",
  "annotations.no_upload_terms",
  "annotations.copyright_terms",
  "annotations.allowed_upload_terms",
  "annotations.exclusive_terms",
  "annotations.latest_styles",
  "evening.anchors",
  "evening.no_online_markers",
  "evening.blocked_styles",
  "evening.excluded_terms",
]);

function readConfigFromForm(base) {
  const cfg = JSON.parse(JSON.stringify(base));
  const setPath = (obj, path, value) => {
    const parts = path.split(".");
    let target = obj;
    for (let i = 0; i < parts.length - 1; i++) target = target[parts[i]];
    target[parts[parts.length - 1]] = value;
  };
  document.querySelectorAll("[data-cfg]").forEach((elm) => {
    const path = elm.dataset.cfg;
    const isListField = LIST_FIELDS.has(path);
    const value = isListField
      // Settings are typed with a Chinese IME, so accept full-width commas and 、 too;
      // otherwise `發動畫，TIRO` is saved as one entry that never matches anything.
      ? elm.value.split(/[,，、;；\s]+/).map((s) => s.trim()).filter((s) => s !== "")
      : elm.value;
    setPath(cfg, path, value);
  });
  document.querySelectorAll("[data-cfg-spans]").forEach((elm) => {
    const spans = parseSlotSpans(elm.value);
    if (spans === null) throw new Error(`跨節合併格式錯誤：「${elm.value}」，請寫成 1200=1400, 1800=2000`);
    setPath(cfg, elm.dataset.cfgSpans, spans);
  });
  document.querySelectorAll("[data-cfg-programs]").forEach((elm) => {
    const programs = parsePrograms(elm.value);
    if (programs === null) throw new Error(`停播節目格式錯誤：「${elm.value}」，請寫成 關鍵KS=關鍵時刻`);
    setPath(cfg, elm.dataset.cfgPrograms, programs);
  });
  document.querySelectorAll("[data-cfg-bool]").forEach((elm) => setPath(cfg, elm.dataset.cfgBool, elm.checked));
  document.querySelectorAll("[data-cfg-num]").forEach((elm) => setPath(cfg, elm.dataset.cfgNum, Number(elm.value)));
  document.querySelectorAll("[data-punct-key]").forEach((elm) => {
    cfg.punctuation.map[elm.dataset.punctKey] = elm.value;
  });
  return cfg;
}

// 設定分組一次只展開一組（v0.3.22）。上次展開的那組記在本機，下次打開設定還是它。
const SETTINGS_OPEN_KEY = "settingsOpenSection";
function wireSettingsAccordion() {
  const sections = [...document.querySelectorAll("#settingsBackdrop .settings-section.collapsible")];
  const setOpen = (target) => {
    for (const sec of sections) {
      const open = sec === target;
      sec.classList.toggle("open", open);
      sec.querySelector(".settings-toggle").setAttribute("aria-expanded", String(open));
    }
    try {
      localStorage.setItem(SETTINGS_OPEN_KEY, target?.dataset.sec ?? "");
    } catch {
      /* 存不進去只是下次不會記得 */
    }
  };
  for (const sec of sections) {
    const head = sec.querySelector(".settings-toggle");
    const toggle = () => setOpen(sec.classList.contains("open") ? null : sec);
    head.addEventListener("click", toggle);
    head.addEventListener("keydown", (e) => {
      if (e.key === "Enter" || e.key === " ") {
        e.preventDefault();
        toggle();
      }
    });
  }
  let remembered = "";
  try {
    remembered = localStorage.getItem(SETTINGS_OPEN_KEY) ?? "";
  } catch {
    /* 讀不到就全部收合 */
  }
  const first = sections.find((sec) => sec.dataset.sec === remembered);
  if (first) setOpen(first);
}

async function openSettings() {
  const [config, apiStatus, inewsCred] = await Promise.all([
    invoke("get_config"),
    invoke("get_api_key_status"),
    invoke("get_inews_credential_status").catch(() => ({ has_credentials: false, username: "" })),
  ]);
  document.body.insertAdjacentHTML("beforeend", settingsModalHtml(config, apiStatus, inewsCred));

  // Dismissing without saving must not leave an un-persisted theme applied.
  const close = () => {
    applyTheme(config.ui.theme);
    el("settingsBackdrop")?.remove();
  };
  el("settingsCloseBtn").addEventListener("click", close);
  wireSettingsAccordion();
  el("previewSoundBtn")?.addEventListener("click", () =>
    playReviewSound(el("soundKindSelect").value, Number(el("soundVolumeInput").value)));
  el("soundVolumeInput")?.addEventListener("input", (e) => { el("volumeLabel").textContent = e.target.value; });
  el("settingsBackdrop").addEventListener("click", (e) => { if (e.target.id === "settingsBackdrop") close(); });

  el("saveApiKeyBtn").addEventListener("click", async () => {
    const key = el("apiKeyInput").value;
    if (!key.trim()) return;
    try {
      await invoke("set_api_key", { key });
      await refreshApiKeyStatus();
      close();
      openSettings();
    } catch (err) {
      el("apiKeyError").textContent = `⚠ 儲存失敗：${err}`;
    }
  });
  el("clearApiKeyBtn").addEventListener("click", async () => {
    await invoke("clear_api_key");
    await refreshApiKeyStatus();
    close();
    openSettings();
  });

  const inewsCredStatus = (text, isError) => {
    el("inewsCredStatus").textContent = text;
    el("inewsCredStatus").style.color = isError ? "var(--danger)" : "var(--muted)";
  };
  el("saveInewsCredBtn").addEventListener("click", async () => {
    try {
      await invoke("set_inews_credentials", {
        username: el("inewsUserInput").value,
        password: el("inewsPassInput").value,
      });
      close();
      await openSettings();
      el("inewsCredStatus").textContent = "已儲存。可以按「測試連線」確認。";
    } catch (err) {
      inewsCredStatus(`⚠ 儲存失敗：${err}`, true);
    }
  });
  el("clearInewsCredBtn").addEventListener("click", async () => {
    try {
      await invoke("clear_inews_credentials");
      stopInewsAuto();
      close();
      openSettings();
    } catch (err) {
      inewsCredStatus(`⚠ 清除失敗：${err}`, true);
    }
  });
  el("openInewsLogBtn").addEventListener("click", async () => {
    try {
      el("inewsLogStatus").textContent = await invoke("open_inews_log_folder");
    } catch (err) {
      el("inewsLogStatus").textContent = `⚠ ${err}`;
    }
  });
  el("testInewsBtn").addEventListener("click", async () => {
    inewsCredStatus("連線中…", false);
    try {
      inewsCredStatus(`✅ ${await invoke("test_inews_connection")}`, false);
    } catch (err) {
      inewsCredStatus(`⚠ ${err}`, true);
    }
  });

  el("diagnoseCollabBtn").addEventListener("click", diagnoseCollab);
  el("exportCollabBtn").addEventListener("click", exportCollabText);
  el("checkUpdateBtn").addEventListener("click", () => checkForUpdate({ silent: false }));
  showCurrentVersion();

  // Preview immediately so the choice can be judged; closing without saving reverts
  // to whatever is on disk, matching the "stays until someone saves" promise.
  el("themeSelect").addEventListener("change", (e) => applyTheme(e.target.value));

  el("browseDefaultFolderBtn").addEventListener("click", async () => {
    const folder = await invoke("pick_folder", { startDir: el("defaultFolderInput").value || null });
    if (folder) el("defaultFolderInput").value = folder;
  });

  el("resetConfigBtn").addEventListener("click", async () => {
    await invoke("reset_config");
    close();
    openSettings();
  });

  el("saveConfigBtn").addEventListener("click", async () => {
    const status = el("settingsStatus");
    try {
      const updated = readConfigFromForm(config);
      await invoke("save_config", { config: updated });
      // Keep the in-memory baseline in step so a later close() doesn't revert the
      // theme the user just committed.
      config.ui.theme = updated.ui.theme;
      applyTheme(updated.ui.theme);
      if (Number.isFinite(updated.gemini?.max_per_run)) {
        keywordMaxPerRun = updated.gemini.max_per_run;
        updateKeywordButton();
      }
      // The label is what decides which entries a batch run skips, so an edited one
      // has to reach the running card set without a restart.
      if (updated.annotations?.no_upload_label !== undefined) {
        noUploadLabel = updated.annotations.no_upload_label;
        updateKeywordButton();
      }
      // First binding imports straight away, like a normal startup. Re-binding only
      // switches the target: importing here would silently replace cards the user may
      // have been editing, so the next 匯入資料夾 picks up the new folder instead.
      const newFolder = updated.import?.default_folder ?? "";
      applyEveningConfig(updated);
      // 新的間隔、寫入模式、分隔線馬上生效，不用重開自動刷新。
      applyInewsConfig(updated);
      if (!currentFolder) await initFromConfig();
      else if (newFolder !== currentFolder) setCurrentFolder(newFolder);
      status.textContent = "已儲存";
      status.style.color = "var(--ok)";
    } catch (err) {
      status.textContent = `儲存失敗：${err}`;
      status.style.color = "var(--danger)";
    }
  });
}

// --- 晚報 (evening bulletin) list ---
//
// A second, read-only screen: import the evening anchor's rundown from the same
// folder, compare it against the chosen day's block of the shared doc, and turn the
// stories that went online into the title list. Nothing here writes to the doc; the
// cards and edits are kept on disk (evening.json) so a restart loses nothing.

let viewMode = "script";
/** @type {Array<any>} */
let evItems = [];
/** Each group's hashtag line, keyed by groupKey: { auto: ["#a", ...], manual: string|null }. */
let evGroupTags = {};
/** Date the current cards were imported/compared for, so edits only carry over
 *  between imports of the same day. */
let evItemsDate = "";
let eveningOpts = { exclusivePrefix: "獨家》", latestPrefix: "最新》" };
let eveningBlockedStyles = ["SL"];
let eveningExcluded = ["專"];
let evSaveTimer = null;
let evStateLoaded = false;
/** True while a 產生關鍵字 run over the groups is in flight. */
let evGroupRun = false;

function todayIso() {
  const d = new Date();
  const pad = (n) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;
}

function setMode(mode) {
  viewMode = mode;
  const evening = mode === "evening";
  el("scriptToolbar").classList.toggle("hidden", evening);
  el("eveningToolbar").classList.toggle("hidden", !evening);
  el("scriptView").classList.toggle("hidden", evening);
  el("eveningView").classList.toggle("hidden", !evening);
  el("modeScriptBtn").classList.toggle("active", !evening);
  el("modeEveningBtn").classList.toggle("active", evening);
}

function evStatus(text, isError = false) {
  const node = el("evStatus");
  node.classList.remove("hidden");
  node.className = isError ? "compare-status error" : "compare-status";
  node.textContent = text;
}

function applyEveningConfig(config) {
  eveningOpts = {
    exclusivePrefix: config.annotations?.exclusive_prefix ?? "獨家》",
    latestPrefix: config.annotations?.latest_prefix ?? "最新》",
  };
  eveningBlockedStyles = config.evening?.blocked_styles ?? eveningBlockedStyles;
  eveningExcluded = config.evening?.excluded_terms ?? eveningExcluded;
  const select = el("evAnchor");
  const current = select.value;
  const anchors = config.evening?.anchors ?? [];
  select.innerHTML = anchors.map((a) => `<option value="${escapeHtml(a)}">${escapeHtml(a)}</option>`).join("");
  if (anchors.includes(current)) select.value = current;
  renderEveningOutput();
}

function evOutputOpts() {
  return { date: el("evDate").value, anchor: el("evAnchor").value, groupTags: evGroupTags, ...eveningOpts };
}

// Debounced so typing in a title does not write the file on every keystroke.
function saveEveningSoon() {
  if (!evStateLoaded) return;
  clearTimeout(evSaveTimer);
  evSaveTimer = setTimeout(saveEveningNow, 400);
}

async function saveEveningNow() {
  clearTimeout(evSaveTimer);
  const state = {
    version: 1,
    date: el("evDate").value,
    anchor: el("evAnchor").value,
    itemsDate: evItemsDate,
    items: evItems.map(({ keywordStatus, keywordError, ...rest }) => rest),
    groupTags: evGroupTags,
  };
  try {
    await invoke("save_evening_state", { json: JSON.stringify(state) });
  } catch (err) {
    evStatus(`⚠ 晚報內容儲存失敗：${err}`, true);
  }
}

/// Restores the saved cards only when they are for today: the date defaults to today,
/// and yesterday's list sitting under today's date would be compared against the
/// wrong block of the doc. The anchor is always restored.
async function loadEvening() {
  el("evDate").value = todayIso();
  try {
    const raw = await invoke("load_evening_state");
    if (raw) {
      const state = JSON.parse(raw);
      if (state.anchor && [...el("evAnchor").options].some((o) => o.value === state.anchor)) {
        el("evAnchor").value = state.anchor;
      }
      if (state.date === todayIso() && Array.isArray(state.items)) {
        evItems = state.items.map((i) => ({ ...i, keywordStatus: "idle", keywordError: "" }));
        evItemsDate = state.itemsDate ?? state.date;
        evGroupTags = state.groupTags && typeof state.groupTags === "object" ? state.groupTags : {};
      }
    }
  } catch (err) {
    console.error("讀取晚報內容失敗", err);
  }
  evStateLoaded = true;
  renderEvening();
}

async function importEvening() {
  if (!currentFolder) {
    await importConfiguredFolder(); // opens settings with the same hint as 文稿
    return;
  }
  const date = el("evDate").value;
  evStatus("匯入中…");
  let summary;
  try {
    summary = await invoke("import_folder", { folder: currentFolder });
  } catch (err) {
    evStatus(`⚠ 匯入失敗：${err}`, true);
    return;
  }
  const fresh = sortEvening(summary.entries.map(eveningItemFromDto).filter(Boolean));
  // Same day: keep titles and hashtag lines already edited. Another day: start clean.
  const sameDay = evItemsDate === date;
  evItems = sameDay ? mergeEveningEdits(fresh, evItems) : fresh;
  if (!sameDay) evGroupTags = {};
  evItemsDate = date;
  renderEvening();
  saveEveningSoon();
  evStatus(`已匯入 ${evItems.length} 則。比對中…`);
  await compareEvening({ silent: true });
}

async function compareEvening({ silent = false } = {}) {
  const date = el("evDate").value;
  const mmdd = mmddOf(date);
  if (!mmdd) {
    evStatus("⚠ 請先選擇日期。", true);
    return;
  }
  if (evItems.length === 0) return;
  evStatus(`比對協作文件 ${mmdd} 的區塊中…`);
  try {
    const results = await invoke("compare_evening", { slugs: evItems.map((i) => i.slug), mmdd });
    const bySlug = new Map(results.map((r) => [r.slug, r]));
    for (const item of evItems) {
      const r = bySlug.get(item.slug);
      if (!r) continue;
      const online = isEveningHit(r.status, item, noUploadLabel);
      const styleOk = isEveningStyle(item.style, eveningBlockedStyles);
      const special = isEveningExcluded(item, eveningExcluded);
      const hit = online && styleOk && !special;
      // A doc hit vetoed by the story's own 勿上網 note or by its style gets its own
      // label, or it would read as a hit that mysteriously stayed unticked.
      item.status = r.status !== "hit" ? r.status : !online ? "note_no_upload" : special ? "special_excluded" : !styleOk ? "style_excluded" : "hit";
      item.matchedLine = r.matched_line;
      item.included = hit;
    }
    evItemsDate = date;
    const hits = evItems.filter((i) => i.included).length;
    evStatus(
      `比對完成（${mmdd}）：命中 ${hits} 則，未命中 ${evItems.length - hits} 則。` +
        (hits > 0 && hasApiKey
          ? "按「產生關鍵字」替清單每一組產生 hashtag（一組送一次）。"
          : "")
    );
    renderEvening();
    saveEveningSoon();
  } catch (err) {
    const msg = String(err);
    evStatus(
      msg.includes("找不到") && msg.includes("日期分隔線")
        ? `⚠ ${msg}`
        : `⚠ 比對失敗：${msg}${silent ? "（請先「開啟協作工具」再按「比對當天」）" : ""}`,
      true
    );
  }
}

async function generateEveningKeywords(targets) {
  if (targets.length === 0) return;
  const { batch, remaining } = splitKeywordRun(targets, keywordMaxPerRun);
  batch.forEach((i) => { i.keywordStatus = "loading"; });
  renderEvening();
  showProgress(0, batch.length, `產生關鍵字 0 / ${batch.length}`);
  try {
    const results = await invoke("generate_keywords_batch", {
      items: batch.map((i) => ({ id: i.id, title: i.title, body: i.body })),
      count: KEYWORD_COUNT,
    });
    for (const r of results) {
      const item = evItems.find((i) => i.id === r.id);
      if (!item) continue;
      if (r.keywords) {
        item.keywords = r.keywords.join(" ");
        item.keywordStatus = "idle";
        item.keywordError = "";
      } else {
        item.keywordStatus = "error";
        item.keywordError = r.error ?? "未知錯誤";
      }
    }
    const failed = results.filter((r) => !r.keywords);
    if (failed.some((r) => isRateLimitError(r.error))) {
      evStatus(`⚠ 已達 Gemini 每分鐘請求上限：${failed.length} 則沒產生成功，請等約一分鐘後再按一次。`, true);
    } else if (remaining > 0) {
      evStatus(`本次已送出 ${batch.length} 則，還有 ${remaining} 則，請等約一分鐘後再按一次「產生關鍵字」。`);
    }
  } catch (err) {
    batch.forEach((i) => { i.keywordStatus = "error"; i.keywordError = String(err); });
    hideProgress();
    evStatus(`⚠ 產生關鍵字失敗：${err}`, true);
  }
  renderEvening();
  saveEveningSoon();
}

const EV_STATUS_LABEL = {
  hit: "命中",
  no_online: "標記不上網",
  not_found: "當天區塊沒有",
  note_no_upload: "備註勿上網",
  style_excluded: "樣式不上晚報",
  special_excluded: "專題不上晚報",
};

/// 晚報 產生關鍵字: one Gemini request per group, sending the group exactly as it
/// will be copied (header line + titles) and getting back 4 keywords for the group
/// as a whole -- its hashtag line. 9 stories in 2 groups cost 2 requests, not 9.
/// Every press regenerates every group; a hand-edited line is kept, since that is
/// the one thing a person deliberately decided.
async function generateEveningGroupKeywords() {
  const opts = evOutputOpts();
  const groups = eveningGroups(evItems);
  if (groups.length === 0 || evGroupRun) return;
  evGroupRun = true;
  renderEveningOutput();
  showProgress(0, groups.length, `產生關鍵字 第 1 / ${groups.length} 組`);
  const requests = groups.map((group, g) => ({
    id: `g${g}`,
    header: buildEveningGroupHead(group, opts).split("\n")[0],
    titles: group.map((i) => i.title),
  }));
  try {
    const results = await invoke("generate_group_keywords", { groups: requests, count: KEYWORD_COUNT });
    const failed = [];
    results.forEach((r, g) => {
      const group = groups[g];
      if (!group) return;
      if (r.keywords) {
        const key = groupKey(group);
        evGroupTags[key] = { auto: r.keywords, manual: evGroupTags[key]?.manual ?? null };
      } else {
        failed.push(`第 ${g + 1} 組：${r.error ?? "未知錯誤"}`);
      }
    });
    evStatus(
      failed.length
        ? `⚠ ${groups.length} 組中有 ${failed.length} 組沒產生成功，請再按一次「產生關鍵字」。\n${failed.join("\n")}`
        : `✅ 已替 ${groups.length} 組產生關鍵字（共送出 ${groups.length} 次請求）。`,
      failed.length > 0
    );
  } catch (err) {
    hideProgress();
    evStatus(`⚠ 產生關鍵字失敗：${err}`, true);
  }
  evGroupRun = false;
  renderEveningOutput();
  saveEveningSoon();
}

function eveningCardHtml(item) {
  const statusLabel = item.status ? EV_STATUS_LABEL[item.status] : "尚未比對";
  const statusClass =
    ["note_no_upload", "style_excluded", "special_excluded"].includes(item.status) ? "no_online" : item.status ?? "pending";
  const marker = (item.marker ?? "").trim();
  return `
    <div class="card ${item.included ? "ev-hit" : "ev-miss"} ${item.collapsed === false ? "" : "collapsed"}" data-id="${item.id}">
      <div class="card-head">
        <button class="collapse-toggle ev-collapse" data-id="${item.id}" title="展開／收合">${item.collapsed === false ? "▼" : "▶"}</button>
        <label class="include-toggle">
          <input type="checkbox" class="ev-include" data-id="${item.id}" ${item.included ? "checked" : ""} />
          列入
        </label>
        <span class="slug">${escapeHtml(item.slug)}</span>
        <span class="badge ${statusClass}" title="${escapeHtml(item.matchedLine ?? "")}">${statusLabel}</span>
        ${marker ? `<span class="badge marker">${escapeHtml(marker)}</span>` : ""}
        <span>${escapeHtml(item.style)}</span>
        <span>${escapeHtml(item.time)}</span>
        ${item.collapsed === false ? "" : `<span class="peek">${escapeHtml(item.title)}</span>`}
        <span class="file-name">${escapeHtml(item.fileName)}</span>
      </div>
      <div class="card-body">
        <div class="field">
          <label>標題</label>
          <input type="text" class="ev-title" data-id="${item.id}" value="${escapeHtml(item.title)}" />
        </div>
        <div class="field">
          <label>關鍵字（輔助用，不會進清單；清單的 hashtag 行由「產生關鍵字」逐組產生）</label>
          <div class="keywords-row">
            <input type="text" class="ev-keywords" data-id="${item.id}" value="${escapeHtml(item.keywords)}" placeholder="#關鍵字1 #關鍵字2 #關鍵字3 #關鍵字4" />
            <button class="ev-gen-one" data-id="${item.id}" ${!hasApiKey || item.keywordStatus === "loading" ? "disabled" : ""}>
              ${item.keywordStatus === "loading" ? "產生中…" : "產生關鍵字"}
            </button>
          </div>
          ${item.keywordStatus === "error" ? `<div class="w">⚠ 產生失敗：${escapeHtml(item.keywordError)}</div>` : ""}
        </div>
        ${item.matchedLine ? `<div class="muted">協作文件：${escapeHtml(item.matchedLine)}</div>` : ""}
      </div>
    </div>`;
}

function groupTagInputHtml(group) {
  const key = groupKey(group);
  const entry = evGroupTags[key];
  const value = entry?.manual ?? groupTagLine(entry);
  return `<input type="text" class="ev-tagline" data-key="${escapeHtml(key)}" value="${escapeHtml(value)}"
    placeholder="按「產生關鍵字」產生這組的 hashtag，或直接輸入（例：#烏龜翻#牛肉麵）" />`;
}

function renderEveningOutput() {
  const opts = evOutputOpts();
  const text = buildEveningText(evItems, opts);
  const groups = eveningGroups(evItems);
  el("evPreview").innerHTML = groups
    .map((group, g) => `
        <div class="ev-group">
          <div class="ev-group-head">
            <span>${groups.length > 1 ? `第 ${g + 1} 組・` : ""}${group.length} 則</span>
          </div>
          <pre class="ev-pre">${escapeHtml(buildEveningGroupHead(group, opts))}</pre>
          ${groupTagInputHtml(group)}
        </div>`)
    .join("");
  // Typing in a hashtag line only stores it -- re-rendering here would drop the caret.
  document.querySelectorAll(".ev-tagline").forEach((input) =>
    input.addEventListener("input", (e) => {
      const key = e.target.dataset.key;
      const entry = evGroupTags[key] ?? { auto: [], manual: null };
      // Emptying the line hands it back to the generated keywords.
      entry.manual = e.target.value.trim() === "" ? null : e.target.value;
      evGroupTags[key] = entry;
      const copyable = buildEveningText(evItems, evOutputOpts());
      el("evCopyBtn").disabled = !copyable;
      el("evSaveBtn").disabled = !copyable;
      saveEveningSoon();
    })
  );
  const btn = el("evGenKeywordsBtn");
  btn.disabled = !hasApiKey || evGroupRun || groups.length === 0;
  btn.textContent = evGroupRun ? "產生中…" : groups.length > 1 ? `產生關鍵字（${groups.length} 組）` : "產生關鍵字";
  btn.title = hasApiKey
    ? "替清單每一組產生關鍵字：一組送一次請求，整組的標題一起送"
    : "尚未設定 Gemini API key，請到設定頁輸入";
  el("evSlugs").textContent = buildEveningSlugList(evItems);
  el("evOutput").classList.toggle("hidden", !text);
  el("evCopyBtn").disabled = !text;
  el("evSaveBtn").disabled = !text;
}

function renderEvening() {
  const ordered = sortEvening(evItems);
  const visible = [...ordered.filter((i) => i.included), ...ordered.filter((i) => !i.included)];
  el("evList").innerHTML = visible.map(eveningCardHtml).join("");
  el("evEmptyState").classList.toggle("hidden", evItems.length > 0);
  el("evClearBtn").disabled = evItems.length === 0;
  el("evCompareBtn").disabled = evItems.length === 0;

  const find = (e) => evItems.find((i) => i.id === e.target.dataset.id);
  // Text edits refresh only the preview, not the cards, so the caret stays put.
  const onEdit = (cls, apply) =>
    document.querySelectorAll(cls).forEach((elm) =>
      elm.addEventListener("input", (e) => {
        const item = find(e);
        if (!item) return;
        apply(item, e.target.value);
        renderEveningOutput();
        saveEveningSoon();
      })
    );
  onEdit(".ev-title", (item, v) => { item.title = v; });
  onEdit(".ev-keywords", (item, v) => { item.keywords = v; });
  document.querySelectorAll(".ev-include").forEach((elm) =>
    elm.addEventListener("change", (e) => {
      const item = find(e);
      if (item) item.included = e.target.checked;
      renderEvening();
      saveEveningSoon();
    })
  );
  document.querySelectorAll(".ev-collapse").forEach((elm) =>
    elm.addEventListener("click", (e) => {
      const item = find(e);
      if (!item) return;
      item.collapsed = item.collapsed === false;
      renderEvening();
    })
  );
  document.querySelectorAll(".ev-gen-one").forEach((elm) =>
    elm.addEventListener("click", (e) => {
      const item = find(e);
      if (item) generateEveningKeywords([item]);
    })
  );
  renderEveningOutput();
}

el("modeScriptBtn").addEventListener("click", () => setMode("script"));
el("modeEveningBtn").addEventListener("click", () => setMode("evening"));
el("evImportBtn").addEventListener("click", importEvening);
el("evOpenCollabBtn").addEventListener("click", () => openCollab(el("evStatus")));
el("evCompareBtn").addEventListener("click", () => compareEvening());
el("evGenKeywordsBtn").addEventListener("click", generateEveningGroupKeywords);
el("evClearBtn").addEventListener("click", () => {
  evItems = [];
  evGroupTags = {};
  el("evStatus").classList.add("hidden");
  renderEvening();
  saveEveningSoon();
});
el("evAnchor").addEventListener("change", () => { renderEveningOutput(); saveEveningSoon(); });
el("evDate").addEventListener("change", () => {
  // The old hits belong to another day's block; keep the cards but make the user
  // compare again rather than print them under a date they were never checked for.
  if (evItems.length > 0 && el("evDate").value !== evItemsDate) {
    evItems.forEach((i) => { i.status = null; i.matchedLine = null; i.included = false; });
    evStatus(`日期已改為 ${el("evDate").value}，請按「比對當天」重新比對。`);
    renderEvening();
  } else {
    renderEveningOutput();
  }
  saveEveningSoon();
});
el("evCopyBtn").addEventListener("click", async () => {
  await navigator.clipboard.writeText(buildEveningText(evItems, evOutputOpts()));
  const btn = el("evCopyBtn");
  btn.textContent = "已複製！";
  setTimeout(() => { btn.textContent = "複製清單"; }, 1200);
});
el("evSaveBtn").addEventListener("click", async () => {
  const opts = evOutputOpts();
  await invoke("save_text_file", {
    defaultName: `${opts.anchor}晚報_${mmddOf(opts.date)}.txt`,
    content: buildEveningExport(evItems, opts),
  });
});
// Last write before the window goes away, so a debounce in flight is not lost.
window.addEventListener("beforeunload", () => { if (evSaveTimer) saveEveningNow(); });

(async () => {
  try {
    applyEveningConfig(await invoke("get_config"));
  } catch (err) {
    console.error("讀取晚報設定失敗", err);
  }
  await loadEvening();
})();
