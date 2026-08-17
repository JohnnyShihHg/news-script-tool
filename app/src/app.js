const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
// Pure decision rules live in logic.js so they can be unit-tested without a DOM;
// this file keeps only wiring, rendering and IPC. See app/tests/logic.test.js.
const {
  sortByTime,
  isAlreadyInDoc,
  decideInclusion,
  summarizeMatches,
  selectKeywordTargets,
  splitKeywordRun,
  isRateLimitError,
  computeFunnel,
  buildOutputText,
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
const KEYWORD_COUNT = 4;
/** Max entries one keyword run may send; overwritten from config on startup.
 *  Matches the free tier's per-minute request cap. 0 = no cap. */
let keywordMaxPerRun = 15;

const el = (id) => document.getElementById(id);

const slugOf = (item) => (item.dto[item.kind] ?? item.dto).slug;

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

async function pickAndImport() {
  const folder = await invoke("pick_folder", { startDir: currentFolder || null });
  if (!folder) return;
  currentFolder = folder;
  el("folderLabel").textContent = folder;
  el("clearFolderBtn").disabled = false;
  const summary = await invoke("import_folder", { folder });
  loadSummary(summary);
  await runAutoPipeline();
}

/// Clears the on-screen cards only. Deliberately separate from 清空資料夾, which
/// deletes files on disk -- this one touches nothing outside the UI.
function clearCards() {
  items = [];
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
/// clicking. A folder that has since been deleted/renamed must not block startup, so
/// an import failure only surfaces on the label rather than throwing.
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
  const folder = config.import?.default_folder ?? "";
  if (!folder) return;

  currentFolder = folder;
  el("folderLabel").textContent = folder;
  el("clearFolderBtn").disabled = false;

  try {
    const summary = await invoke("import_folder", { folder });
    loadSummary(summary);
    await runAutoPipeline();
  } catch (err) {
    el("folderLabel").textContent = `${folder}（自動匯入失敗：${err}）`;
  }
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
  return selectKeywordTargets(items);
}

function loadSummary(summary) {
  // A fresh card set has not been compared against the doc yet, whatever the previous
  // set's state was -- otherwise the write-back guard would trust a stale comparison.
  hasCompared = false;
  items = summary.entries.map((dto, i) => {
    const kind = kindOf(dto);
    const bucket = bucketOf(kind);
    const fields = dto[kind] ?? dto; // enum payload
    return {
      dto,
      kind,
      bucket,
      id: `entry-${i}`,
      included: bucket === "passed" || bucket === "manual",
      /// The import-time default, kept so a re-compare can restore the tick without
      /// opting in buckets (unknown styles) that are deliberately off to begin with.
      defaultIncluded: bucket === "passed" || bucket === "manual",
      title: fields.title ?? "",
      body: fields.body ?? "",
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
  });
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

  const hasOutput = summary.passed + summary.needs_manual > 0;
  el("copyBtn").disabled = !hasOutput;
  el("saveBtn").disabled = !hasOutput;
  el("writeBackBtn").disabled = !hasOutput;
  el("clearCardsBtn").disabled = items.length === 0;

  renderFunnel();
  render();
}

/// One line answering "if I press 寫入 now, what goes in?" -- the stat pills above are
/// filters for the parse outcome, which is a different question and doesn't tell the
/// user what the write button will actually do.
function renderFunnel() {
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

  if (kind === "FilteredByStyle") {
    return `
      <div class="card filtered" data-bucket="${bucket}">
        <div class="card-head">
          <span class="slug">${fields.slug_marker ? `<span class="badge removed">${escapeHtml(fields.slug_marker)}</span> ` : ""}${escapeHtml(fields.slug)}</span>
          <span class="badge filtered">已濾除・樣式 ${escapeHtml(fields.style)}</span>
          <span class="file-name">${escapeHtml(fields.file_name)}</span>
        </div>
      </div>`;
  }

  if (kind === "ParseFailed") {
    return `
      <div class="card failed" data-bucket="${bucket}">
        <div class="card-head">
          <span class="badge failed">解析失敗</span>
          <span class="file-name">${escapeHtml(fields.file_name)}</span>
        </div>
        <div class="reason">${escapeHtml(fields.reason)}</div>
      </div>`;
  }

  const badgeLabel = { Passed: "通過", UnknownStyle: "未知樣式", NeedsManualContent: "待補稿" }[kind];
  const titleDiff = renderDiff(fields.raw_title, fields.raw_title !== undefined ? item.title : "");
  const bodyDiff = renderDiff(fields.raw_body, fields.raw_body !== undefined ? item.body : "");
  const hasDiff = !!(fields.raw_title || fields.raw_body);
  const matchLabel = item.alreadyInDoc
    ? "已在文件中"
    : { to_cut: "待處理", keep_refresh: "要重貼", removed: "已剔除" }[item.matchStatus];
  const matchClass = item.alreadyInDoc ? "removed" : item.matchStatus;

  return `
    <div class="card ${bucket} ${item.collapsed ? "collapsed" : ""} ${item.alreadyInDoc ? "done" : ""}" data-bucket="${bucket}" data-id="${id}">
      <div class="card-head">
        <button class="collapse-toggle" data-id="${id}" title="展開／收合">${item.collapsed ? "▶" : "▼"}</button>
        <label class="include-toggle">
          <input type="checkbox" class="include-cb" data-id="${id}" ${item.included ? "checked" : ""} />
          輸出
        </label>
        <span class="slug">${escapeHtml(fields.slug)}</span>
        <span class="badge ${bucket}">${badgeLabel}</span>
        ${matchLabel ? `<span class="badge ${matchClass}" title="${escapeHtml(item.matchedLine ?? "")}">${matchLabel}</span>` : ""}
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
        <label>關鍵字（空格分隔）</label>
        <div class="keywords-row">
          <input type="text" class="edit-keywords" data-id="${id}" value="${escapeHtml(item.keywords)}" placeholder="#關鍵字1 #關鍵字2 #關鍵字3 #關鍵字4" />
          <button class="gen-keywords-btn" data-id="${id}" ${!hasApiKey || item.keywordStatus === "loading" ? "disabled" : ""}>
            ${item.keywordStatus === "loading" ? "產生中…" : "產生關鍵字"}
          </button>
        </div>
        ${item.keywordStatus === "error" ? `<div class="w">⚠ 產生失敗：${escapeHtml(item.keywordError)}</div>` : ""}
      </div>

      ${fields.warnings && fields.warnings.length ? `
        <div class="warnings">
          ${fields.warnings.map((w) => `<div class="w">⚠ ${escapeHtml(w)}</div>`).join("")}
        </div>` : ""}
      </div>
    </div>`;
}

function render() {
  const visible = activeFilter === "all" ? items : items.filter((i) => i.bucket === activeFilter);
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
      if (item) item.included = e.target.checked;
      renderFunnel();
      updateKeywordButton();
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
  document.querySelectorAll(".collapse-toggle").forEach((elm) => {
    elm.addEventListener("click", (e) => {
      const item = items.find((i) => i.id === e.target.dataset.id);
      if (!item) return;
      item.collapsed = !item.collapsed;
      render();
    });
  });
  // The button carries a live count of what a run would send, so it has to be
  // recomputed by the one path every state change already goes through.
  updateKeywordButton();
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

async function generateKeywordsForAllIncluded() {
  const targets = keywordTargets();
  const { batch, remaining } = splitKeywordRun(targets, keywordMaxPerRun);
  await generateKeywordsFor(batch);
  if (remaining > 0) {
    const status = el("compareStatus");
    status.classList.remove("hidden");
    status.className = "compare-status";
    status.textContent =
      `本次已送出 ${batch.length} 則（避開 Gemini 每分鐘上限）。還有 ${remaining} 則未產生，` +
      `請等約一分鐘後再按一次「產生關鍵字」——已完成的不會重複消耗額度。`;
  }
  updateKeywordButton();
}

async function generateKeywordsFor(targets) {
  if (!targets || targets.length === 0) return;

  showProgress(0, targets.length, `產生關鍵字 0 / ${targets.length}`);
  targets.forEach((i) => { i.keywordStatus = "loading"; });
  render();

  const requests = targets.map((i) => ({ id: i.id, title: i.title, body: i.body }));
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
    reportRateLimit(results.filter((r) => !r.keywords).map((r) => r.error ?? ""), targets.length);
  } catch (err) {
    targets.forEach((i) => { i.keywordStatus = "error"; i.keywordError = String(err); });
    hideProgress();
    reportRateLimit([String(err)], targets.length);
  }
  render();
}

/// The free Gemini tier caps requests per minute, and a big batch walks straight into
/// it. Individual cards already show their own error, but a quota failure needs a
/// banner: the fix is "wait a minute and re-run", which is not obvious from a red
/// line buried on card 14 of 20.
function reportRateLimit(errors, attempted) {
  const hits = errors.filter(isRateLimitError);
  if (hits.length === 0) return;
  const status = el("compareStatus");
  status.classList.remove("hidden");
  status.className = "compare-status error";
  status.textContent =
    `⚠ 已達 Gemini 每分鐘請求上限（429）：${attempted} 則中有 ${hits.length} 則沒產生成功。` +
    `請等約一分鐘後，再按「全部產生關鍵字」補跑（已成功的不會重複消耗額度）。`;
}

/// Flattens each card's enum payload into the shape buildOutputText expects, so the
/// formatting rule itself stays a pure, tested function.
function buildOutput() {
  return buildOutputText(
    items.map((i) => {
      const fields = i.dto[i.kind] ?? i.dto;
      return {
        bucket: i.bucket,
        included: i.included,
        slug: fields.slug,
        slug_marker: fields.slug_marker ?? "",
        style: fields.style,
        time: fields.time,
        group: fields.group,
        title: i.title,
        body: i.body,
        keywords: i.keywords,
      };
    })
  );
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

el("importBtn").addEventListener("click", pickAndImport);
el("clearFolderBtn").addEventListener("click", clearFolder);
el("clearCardsBtn").addEventListener("click", clearCards);
el("copyBtn").addEventListener("click", copyAll);
el("saveBtn").addEventListener("click", saveAll);
el("genAllKeywordsBtn").addEventListener("click", generateKeywordsForAllIncluded);
el("settingsBtn").addEventListener("click", openSettings);
el("openCollabBtn").addEventListener("click", openCollab);
el("compareBtn").addEventListener("click", compareWithCollabDoc);
el("writeBackBtn").addEventListener("click", writeBackToCollab);

// Writing lands in a document colleagues are editing live, so this always confirms
// first and shows exactly how much is going in -- never a silent one-click send.
async function writeBackToCollab() {
  const outgoing = items.filter(
    (i) => (i.bucket === "passed" || i.bucket === "manual" || i.bucket === "unknown") && i.included
  );
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
    const msg = await invoke("append_to_collab_doc", { text });
    outgoing.forEach((i) => writtenSlugs.add(slugOf(i)));
    status.textContent = `✅ ${msg}`;
  } catch (err) {
    status.className = "compare-status error";
    status.textContent = `⚠ 寫入失敗：${err}`;
  } finally {
    btn.disabled = false;
  }
}
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

async function openCollab() {
  const status = el("compareStatus");
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

  const targets = items.filter((i) => i.bucket === "passed" || i.bucket === "unknown" || i.bucket === "manual");
  const slugs = targets.map((i) => (i.dto[i.kind] ?? i.dto).slug);

  try {
    const results = await invoke("compare_with_collab_doc", { slugs });
    const bySlug = new Map(results.map((r) => [r.slug, r]));
    const matched = [];
    for (const item of targets) {
      const slug = (item.dto[item.kind] ?? item.dto).slug;
      const r = bySlug.get(slug);
      if (!r) continue;
      item.matchStatus = r.status;
      item.matchedLine = r.matched_line;
      item.alreadyInDoc = isAlreadyInDoc(r);
      item.included = decideInclusion(r, item.defaultIncluded);
      matched.push(r);
    }
    const { toCut, keepRefresh, removed, alreadyIn } = summarizeMatches(matched);
    hasCompared = true;
    const autoUnticked = removed + alreadyIn;
    status.textContent =
      `比對完成：待處理 ${toCut}・要重貼 ${keepRefresh}・已剔除 ${removed}・已在文件中 ${alreadyIn}` +
      (autoUnticked > 0 ? `（已剔除／已在文件中的 ${autoUnticked} 則已自動取消勾選，可手動改回）` : "");
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

function settingsModalHtml(config, apiStatus) {
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

  return `
    <div class="modal-backdrop" id="settingsBackdrop">
      <div class="modal">
        <div class="modal-head">
          <h2>設定</h2>
          <button class="modal-close" id="settingsCloseBtn">✕</button>
        </div>
        <div class="modal-body">
          <section class="settings-section">
            <h3>Gemini API Key</h3>
            <div class="field">
              <label>
                目前狀態：
                ${apiStatus.has_key
                  ? `<span class="badge passed">已設定（來源：${apiStatus.source === "keyring" ? "系統憑證管理員" : "環境變數 GEMINI_API_KEY"}）</span>`
                  : `<span class="badge failed">尚未設定</span>`}
              </label>
              <div class="keywords-row">
                <input type="password" id="apiKeyInput" placeholder="貼上 Gemini API key" />
                <button id="saveApiKeyBtn">儲存</button>
                <button id="clearApiKeyBtn">清除</button>
              </div>
              <div class="muted">儲存後會存進系統憑證管理員（Windows Credential Manager），不會寫進明碼設定檔。</div>
              <div id="apiKeyError" class="w"></div>
            </div>
            <div class="field">
              <label>模型</label>
              <input type="text" data-cfg="gemini.model" value="${escapeHtml(config.gemini.model)}" />
            </div>
            <div class="field">
              <label>單次產生上限（則）</label>
              <input type="number" min="0" data-cfg-num="gemini.max_per_run" value="${config.gemini.max_per_run}" />
            </div>
            <div class="muted">免費方案每分鐘的請求數有上限，一次送太多會有一半以上失敗。按一次「產生關鍵字」最多送這麼多則，剩下的等約一分鐘再按一次即可接續（已完成的不會重複消耗額度）。填 0 代表不限制。</div>
          </section>

          <section class="settings-section">
            <h3>匯入設定</h3>
            <div class="field">
              <label>預設匯入資料夾</label>
              <div class="keywords-row">
                <input type="text" id="defaultFolderInput" data-cfg="import.default_folder" value="${escapeHtml(config.import.default_folder)}" placeholder="（未設定，每次需手動選擇）" />
                <button id="browseDefaultFolderBtn" type="button">瀏覽</button>
              </div>
              <div class="muted">設定後每次開啟工具會自動匯入這個資料夾的 txt，不必再手動點「匯入資料夾」。</div>
            </div>
          </section>

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

          <section class="settings-section">
            <h3>外觀主題</h3>
            <div class="field">
              <select id="themeSelect" data-cfg="ui.theme">
                ${THEMES.map(
                  (t) => `<option value="${t.id}" ${config.ui.theme === t.id ? "selected" : ""}>${t.name}</option>`
                ).join("")}
              </select>
              <div class="muted">選擇後會立即預覽；按下方「儲存設定」才會固定下來，之後每次開啟都維持這個主題，直到再次儲存別的。</div>
            </div>
          </section>

          <section class="settings-section">
            <h3>協作工具維護</h3>
            <div class="field">
              <div class="keywords-row">
                <button id="diagnoseCollabBtn" type="button">診斷協作連線</button>
                <button id="exportCollabBtn" type="button">匯出協作內容</button>
              </div>
              <div class="muted">比對或寫入出問題時用：「診斷」會回報與協作工具視窗的連線狀態；「匯出」把文件目前內容存成 txt，可在寫入前留一份備份。</div>
              <div id="collabToolStatus" class="muted"></div>
            </div>
          </section>

          <section class="settings-section">
            <h3>樣式篩選</h3>
            ${styleField("保留白名單 allowed_styles", "filter.allowed_styles", config.filter.allowed_styles)}
            ${styleField("排除黑名單 blocked_styles", "filter.blocked_styles", config.filter.blocked_styles)}
            ${styleField("排除的 slug 結尾 excluded_slug_suffixes", "filter.excluded_slug_suffixes", config.filter.excluded_slug_suffixes)}
            ${styleField("可回寫但要標記 flag_styles", "filter.flag_styles", config.filter.flag_styles)}
            <div class="field">
              <label>標題標記樣式（regex）</label>
              <input type="text" data-cfg="filter.title_tag_pattern" value="${escapeHtml(config.filter.title_tag_pattern)}" />
            </div>
            ${styleField("樣式空白時改看 slug slug_style_terms", "filter.slug_style_terms", config.filter.slug_style_terms)}
            <div class="muted">有些稿件不填「樣式」，直接把類型寫在新聞名稱裡（例：心喻14推播）。樣式欄真的空白時才會用這裡的詞去比對 slug，比中就當成該樣式處理並在該則標上提醒；樣式欄有填就一律以樣式欄為準。同樣只比對完整詞。</div>
          </section>

          <section class="settings-section">
            <h3>備註標記（編輯備註 → slug 前綴）</h3>
            ${styleField("勿上網 同義詞", "annotations.no_upload_terms", config.annotations.no_upload_terms)}
            <div class="field"><label>顯示標籤</label><input type="text" data-cfg="annotations.no_upload_label" value="${escapeHtml(config.annotations.no_upload_label)}" /></div>
            ${styleField("版權問題 同義詞", "annotations.copyright_terms", config.annotations.copyright_terms)}
            <div class="field"><label>顯示標籤</label><input type="text" data-cfg="annotations.copyright_label" value="${escapeHtml(config.annotations.copyright_label)}" /></div>
            ${styleField("可上網 同義詞", "annotations.allowed_upload_terms", config.annotations.allowed_upload_terms)}
            <div class="field"><label>顯示標籤</label><input type="text" data-cfg="annotations.allowed_upload_label" value="${escapeHtml(config.annotations.allowed_upload_label)}" /></div>
            <div class="muted">只比對完整詞、不看單一個字 —— 真實稿件有「切勿黑畫面」這種備註，用「勿」單字比對會誤判成不可上網。「註記」欄位不看（通常是攝影姓名）。</div>
          </section>

          <section class="settings-section">
            <h3>標題前綴</h3>
            ${styleField("判定獨家的備註關鍵字", "annotations.exclusive_terms", config.annotations.exclusive_terms)}
            <div class="field"><label>獨家前綴</label><input type="text" data-cfg="annotations.exclusive_prefix" value="${escapeHtml(config.annotations.exclusive_prefix)}" /></div>
            ${styleField("判定最新的樣式", "annotations.latest_styles", config.annotations.latest_styles)}
            <div class="field"><label>最新前綴</label><input type="text" data-cfg="annotations.latest_prefix" value="${escapeHtml(config.annotations.latest_prefix)}" /></div>
            <div class="muted">兩者同時成立時只加獨家。標題若已經有前綴就不會重複加。</div>
          </section>

          <section class="settings-section">
            <h3>內文雜訊清除</h3>
            <label class="checkbox-row"><input type="checkbox" data-cfg-bool="clean.strip_marker_symbols" ${config.clean.strip_marker_symbols ? "checked" : ""} /> 移除每行開頭的雜訊符號（例如 <code>..早安你好</code> → <code>早安你好</code>）</label>
            <label class="checkbox-row"><input type="checkbox" data-cfg-bool="clean.drop_non_cjk_lines" ${config.clean.drop_non_cjk_lines ? "checked" : ""} /> 刪除整行沒有中文的備註行（例如單獨一行的 <code>##</code>、<code>OK</code>）</label>
            <label class="checkbox-row"><input type="checkbox" data-cfg-bool="clean.strip_trailing_symbols" ${config.clean.strip_trailing_symbols ? "checked" : ""} /> 移除內文結尾的符號（例如 <code>。##</code> → <code>。</code>）</label>
            <div class="muted">句子中間的標點一律不動 —— 無法可靠分辨是誤打還是原文，改錯會動到要播出的稿件。含網址的行不受影響。</div>
          </section>

          <section class="settings-section">
            <h3>狀態比對標記</h3>
            ${styleField("代表「保留、要重貼」的關鍵字 refresh_keywords", "markers.refresh_keywords", config.markers.refresh_keywords)}
          </section>

          <section class="settings-section">
            <h3>標點轉換</h3>
            <label class="checkbox-row"><input type="checkbox" data-cfg-bool="punctuation.quotes_to_corner" ${config.punctuation.quotes_to_corner ? "checked" : ""} /> 直引號轉「」</label>
            <label class="checkbox-row"><input type="checkbox" data-cfg-bool="punctuation.dot_to_enumeration" ${config.punctuation.dot_to_enumeration ? "checked" : ""} /> . 轉頓號、（數字間除外）</label>
            <label class="checkbox-row"><input type="checkbox" data-cfg-bool="punctuation.protect_urls" ${config.punctuation.protect_urls ? "checked" : ""} /> 保護網址不轉換</label>
            <label class="checkbox-row"><input type="checkbox" data-cfg-bool="punctuation.preserve_halfwidth_space" ${config.punctuation.preserve_halfwidth_space ? "checked" : ""} /> 保留半形空格</label>
            <div class="field">
              <label>半形 → 全形對照</label>
              ${punctRows}
            </div>
          </section>

          <section class="settings-section">
            <h3>輸出格式</h3>
            <div class="field"><label>關鍵字數量</label><input type="number" min="1" data-cfg-num="output.keyword_count" value="${config.output.keyword_count}" /></div>
            <div class="field"><label>關鍵字分隔符號</label><input type="text" data-cfg="output.keyword_separator" value="${escapeHtml(config.output.keyword_separator)}" /></div>
            <div class="field"><label>則與則之間空白行數</label><input type="number" min="0" data-cfg-num="output.entry_blank_lines" value="${config.output.entry_blank_lines}" /></div>
          </section>
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
  "markers.refresh_keywords",
  "annotations.no_upload_terms",
  "annotations.copyright_terms",
  "annotations.allowed_upload_terms",
  "annotations.exclusive_terms",
  "annotations.latest_styles",
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
      ? elm.value.split(",").map((s) => s.trim()).filter((s) => s !== "")
      : elm.value;
    setPath(cfg, path, value);
  });
  document.querySelectorAll("[data-cfg-bool]").forEach((elm) => setPath(cfg, elm.dataset.cfgBool, elm.checked));
  document.querySelectorAll("[data-cfg-num]").forEach((elm) => setPath(cfg, elm.dataset.cfgNum, Number(elm.value)));
  document.querySelectorAll("[data-punct-key]").forEach((elm) => {
    cfg.punctuation.map[elm.dataset.punctKey] = elm.value;
  });
  return cfg;
}

async function openSettings() {
  const [config, apiStatus] = await Promise.all([invoke("get_config"), invoke("get_api_key_status")]);
  document.body.insertAdjacentHTML("beforeend", settingsModalHtml(config, apiStatus));

  // Dismissing without saving must not leave an un-persisted theme applied.
  const close = () => {
    applyTheme(config.ui.theme);
    el("settingsBackdrop")?.remove();
  };
  el("settingsCloseBtn").addEventListener("click", close);
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
      if (!currentFolder) await initFromConfig();
      status.textContent = "已儲存";
      status.style.color = "var(--ok)";
    } catch (err) {
      status.textContent = `儲存失敗：${err}`;
      status.style.color = "var(--danger)";
    }
  });
}
