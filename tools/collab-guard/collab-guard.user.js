// ==UserScript==
// @name         協作工具守門員
// @namespace    news-script-tool
// @version      1.1.0
// @description  擋住協作工具的毛病：新分頁讀不完要手動重整、改動卡在本機沒上傳、重複行螢光閃爍、捲動被拉回去。只讀不改文件內容。
// @match        https://*.chatgpt.site/*
// @match        http://*.chatgpt.site/*
// @run-at       document-idle
// @grant        none
// @updateURL    https://raw.githubusercontent.com/JohnnyShihHg/news-script-tool/main/tools/collab-guard/collab-guard.user.js
// @downloadURL  https://raw.githubusercontent.com/JohnnyShihHg/news-script-tool/main/tools/collab-guard/collab-guard.user.js
// ==/UserScript==

// 自動更新：竄改猴會定期抓上面那個網址比對 @version，比較新才提示／安裝。
// 所以**改完程式一定要把 @version 往上加**，否則大家手上永遠是舊的 —— 竄改猴只看
// 版本號，不看檔案內容有沒有變。

// 為什麼需要這支：協作工具的同步只在它內部的 bootstrapFinalized 亮起時才排程上傳，
// 沒亮就把改動堆在記憶體裡而且不再重試 —— 那一頁變成只收不發，外觀完全正常，別台
// 電腦怎麼重新整理都看不到。它另有一條不看那道閘門的「離開前補送」掛在 window 的
// pagehide 上，所以我們定期補送一個 pagehide 就能把內容推上去。
//
// 這支腳本完全不碰文件內容：不輸入、不刪除、不格式化。它只做三件事 —— 戳同步、
// 限流一個會閃的渲染函式、把被程式拉走的捲動位置拉回來。
(function () {
  "use strict";

  const CONFIG = {
    pushEverySeconds: 10,      // 多久補送一次
    highlightMinGapMs: 1500,   // 重複行螢光最快多久重畫一次
    scrollGuardMs: 1200,       // 使用者停止捲動後多久內的程式捲動視為「被拉走」
    scrollGuardMinJumpPx: 80,  // 跳超過這個距離才攔
    coldStartReloadSeconds: 20, // 新開分頁超過這麼久還在「正在載入共同文章」就自動重整一次。0 = 關閉
    autoReloadAfterStuckSeconds: 0, // 0 = 關閉。設成例如 120 就會在卡住兩分鐘後自動重載
    reloadCooldownMinutes: 5,  // 兩次自動重載至少間隔這麼久，避免伺服器掛掉時無限重載
    debugScroll: false,        // 開了會在 console 印出每一次程式捲動的來源堆疊

    showHud: false,            // 右下角那個狀態小框。關掉仍然會紅框示警
    compactLineButtons: true,  // 行動作按鈕縮小，比較容易塞進左邊空白不蓋到文字
    keepLineButtonsOffText: true, // 行動作列蓋到文字時往左挪開
    fixOutlineCollapse: true,  // 修他「文件分頁」漢堡收合後寬度沒縮的 bug
  };

  const RELOAD_MARK_KEY = "collabGuardReloadedAt";
  const SCROLL_MARK_KEY = "collabGuardScrollTop";

  // 比對刻意放寬到整個 chatgpt.site：原本綁死 /helper4 這個路徑，只要網址帶了不同的
  // 路徑或子頁就整支不執行，而且在竄改猴裡只會顯示「尚未執行過」，看不出原因。
  //
  // 放寬之後改由「這一頁有沒有 Quill 編輯器」決定要不要啟動 —— 以頁面內容判斷比以
  // 網址判斷可靠。Quill 是動態載入的，所以最多等 20 秒。
  function waitForEditor(onReady) {
    let waited = 0;
    const tick = () => {
      if (document.querySelector(".ql-editor")) {
        onReady();
        return;
      }
      waited += 500;
      if (waited > 20_000) {
        console.log("[守門員] 這一頁沒有協作編輯器，不啟動");
        return;
      }
      setTimeout(tick, 500);
    };
    tick();
  }

  const state = {
    lastPushAt: 0,
    lastPullAt: 0,
    lastError: "",
    lastUserScrollAt: 0,
    lastUserInputAt: 0,
    guardedScrollTop: null,
    stuckSince: 0,
    lastHighlightAt: 0,
    blockedScrolls: 0,
    allowScrollUntil: 0,
  };

  // ---- 1. 推送看門狗 ----------------------------------------------------
  //
  // 補送的是「伺服器還缺的部分」，沒缺就什麼都不送，所以多戳沒有副作用。
  // 注意它有 48,000 字元上限、超過會靜默放棄，所以是「不讓它累積」而不是根治。
  function nudgePush() {
    try {
      window.dispatchEvent(new Event("pagehide"));
    } catch (e) {
      /* 忽略：戳失敗不影響頁面 */
    }
  }

  // ---- 2. 看得見的同步狀態 ----------------------------------------------
  //
  // 頁面本身就有狀態文字（已同步／待同步 N 筆／正在載入共同文章／連線中），
  // 但它很小、不會叫。卡住時整頁加紅框，讓靜默失敗變成躲不掉的警告。
  // ---- 0. 冷啟動自救（v1.1.0）----------------------------------------------
  //
  // 新開的分頁要把整份共同文章讀下來，狀態停在「正在載入共同文章」。常常就這樣卡住、
  // 永遠讀不完，要手動重新整理一次才會好（重整會重跑一次載入流程）。這裡在開頁後
  // coldStartReloadSeconds 秒還卡在「正在載入共同文章」、而且這段時間沒有打字時，自動
  // 重整一次。冷啟動時還沒有任何編輯，重整不會掉資料。
  //
  // 一個分頁只自動重整一次：重整後還是卡住就不再重整（紅框照樣提示），避免本來就讀
  // 很久的環境被一直重整。每次讀完花幾秒記在 localStorage（collabGuardColdLoads，
  // 最近 20 筆），之後照實際數字調整秒數。
  //
  // 這一段不等編輯器出現就開始看：卡住的時候編輯器可能根本還沒出來。只認「正在載入
  // 共同文章」這段協作工具才有的文字，30 秒內都沒看到狀態文字就當作不是協作工具，停止。
  const COLD_RELOAD_KEY = "collabGuardColdReloadAt";
  const COLD_LOG_KEY = "collabGuardColdLoads";
  const LOADING_TEXT = "正在載入共同文章";
  const cold = { seconds: null, reloadedBefore: false, typed: false, sawLoading: false };
  try {
    const at = Number(sessionStorage.getItem(COLD_RELOAD_KEY) || 0);
    cold.reloadedBefore = at > 0 && Date.now() - at < 10 * 60_000;
  } catch (e) {
    /* 無痕模式之類的讀不到：當作沒重整過 */
  }
  for (const type of ["keydown", "input", "paste", "drop", "compositionstart"]) {
    window.addEventListener(type, () => { cold.typed = true; }, true);
  }

  // 頁面的狀態文字沒有固定 id，用文字特徵找：那幾種寫法是 deriveSyncStatus 固定產出的。
  function findStatusText() {
    const texts = ["已同步", "同步中", "待同步", LOADING_TEXT, "連線中", "離線"];
    const nodes = document.querySelectorAll("span, div, small, p");
    for (const n of nodes) {
      if (n.children.length) continue;
      const t = (n.textContent || "").trim();
      if (texts.some((x) => t.startsWith(x))) return t;
    }
    return "";
  }

  function recordColdLoad(seconds, reloaded) {
    try {
      const list = JSON.parse(localStorage.getItem(COLD_LOG_KEY) || "[]");
      list.push({ at: new Date().toISOString(), seconds, reloaded, afterAutoReload: cold.reloadedBefore });
      localStorage.setItem(COLD_LOG_KEY, JSON.stringify(list.slice(-20)));
    } catch (e) {
      /* 存不進去只少一筆量測 */
    }
  }

  function watchColdStart() {
    const navStart = Date.now() - performance.now();
    const timer = setInterval(() => {
      const elapsed = (Date.now() - navStart) / 1000;
      const status = findStatusText();
      const loading = status.startsWith(LOADING_TEXT);
      if (loading) cold.sawLoading = true;
      if (/^(已同步|同步中|待同步)/.test(status)) {
        clearInterval(timer);
        cold.seconds = Math.round(elapsed * 10) / 10;
        console.info(`[守門員] 冷啟動讀取 ${cold.seconds} 秒${cold.reloadedBefore ? "（自動重整之後）" : ""}`);
        recordColdLoad(cold.seconds, false);
        return;
      }
      if (
        loading &&
        CONFIG.coldStartReloadSeconds > 0 &&
        elapsed > CONFIG.coldStartReloadSeconds &&
        !cold.typed &&
        !cold.reloadedBefore
      ) {
        clearInterval(timer);
        console.warn(`[守門員] 開頁 ${Math.round(elapsed)} 秒還在「${LOADING_TEXT}」，自動重整一次`);
        recordColdLoad(Math.round(elapsed), true);
        try {
          sessionStorage.setItem(COLD_RELOAD_KEY, String(Date.now()));
        } catch (e) {
          /* 存不進去也照樣重整；下次可能會再重整一次 */
        }
        location.reload();
        return;
      }
      if ((elapsed > 30 && !status && !cold.sawLoading) || elapsed > 600) clearInterval(timer);
    }, 1000);
  }
  watchColdStart();

  // 到這裡只定義了函式與狀態，還沒動到頁面。真正啟動要等編輯器出現。
  waitForEditor(startGuard);

  function startGuard() {
  setInterval(nudgePush, CONFIG.pushEverySeconds * 1000);

  applyLayoutFixes();

  const hud = document.createElement("div");
  hud.style.cssText =
    "position:fixed;right:12px;bottom:12px;z-index:2147483647;font:12px/1.6 system-ui,sans-serif;" +
    "background:rgba(255,255,255,.95);color:#1a1a1a;border:1px solid #d0d0d0;border-radius:8px;" +
    "padding:6px 10px;box-shadow:0 4px 16px rgba(0,0,0,.15);max-width:260px;pointer-events:none";
  hud.style.display = CONFIG.showHud ? "block" : "none";
  document.body.appendChild(hud);

  const alertFrame = document.createElement("div");
  alertFrame.style.cssText =
    "position:fixed;inset:0;z-index:2147483646;pointer-events:none;border:4px solid #dc2626;display:none";
  document.body.appendChild(alertFrame);

  const readPageStatus = findStatusText;

  function ago(ts) {
    if (!ts) return "—";
    const s = Math.round((Date.now() - ts) / 1000);
    return s < 60 ? `${s} 秒前` : `${Math.round(s / 60)} 分前`;
  }

  function refreshHud() {
    const pageStatus = readPageStatus();
    const stuck = /待同步|正在載入共同文章|連線中|離線/.test(pageStatus);
    if (stuck) {
      if (!state.stuckSince) state.stuckSince = Date.now();
    } else {
      state.stuckSince = 0;
    }
    alertFrame.style.display = stuck ? "block" : "none";
    hud.innerHTML =
      `<b>守門員</b>　${pageStatus || "（讀不到狀態）"}<br>` +
      `上次推送 ${ago(state.lastPushAt)}・上次收取 ${ago(state.lastPullAt)}` +
      (cold.seconds !== null ? `<br>開頁讀取 ${cold.seconds} 秒${cold.reloadedBefore ? "（自動重整後）" : ""}` : "") +
      (state.blockedScrolls ? `<br>已擋下 ${state.blockedScrolls} 次捲動跳動` : "") +
      (state.lastError ? `<br><span style="color:#991b1b">${state.lastError}</span>` : "") +
      (stuck && state.stuckSince ? `<br><span style="color:#991b1b">已卡住 ${ago(state.stuckSince)}</span>` : "");

    if (
      CONFIG.autoReloadAfterStuckSeconds > 0 &&
      state.stuckSince &&
      Date.now() - state.stuckSince > CONFIG.autoReloadAfterStuckSeconds * 1000 &&
      Date.now() - state.lastUserInputAt > 10_000
    ) {
      // 重載會走兩條上傳路徑：離開前補送，以及重載後 bootstrap 的分塊推送。
      // 後者沒有 48KB 上限，所以積壓太多時只有這條救得回來。
      //
      // 冷卻是必要的：伺服器真的掛掉時狀態會一直是「連線中」，沒有冷卻就會每兩分鐘
      // 重載一次、無限輪迴，而且每次都把文件重新載一遍。
      const lastReload = Number(sessionStorage.getItem(RELOAD_MARK_KEY) || 0);
      if (Date.now() - lastReload > CONFIG.reloadCooldownMinutes * 60_000) {
        try {
          sessionStorage.setItem(RELOAD_MARK_KEY, String(Date.now()));
          if (scroller) sessionStorage.setItem(SCROLL_MARK_KEY, String(scroller.scrollTop));
        } catch (e) {
          /* 無痕模式之類的存不進去，不影響重載 */
        }
        location.reload();
      }
    }
  }
  setInterval(refreshHud, 1000);

  // ---- 3. 攔 fetch，知道同步到底有沒有在動 --------------------------------
  const originalFetch = window.fetch;
  window.fetch = function (input, init) {
    const url = typeof input === "string" ? input : input?.url || "";
    const isSync = url.includes("/api/sync");
    const isWrite = isSync && (init?.method || "GET").toUpperCase() === "POST";
    return originalFetch.apply(this, arguments).then(
      (res) => {
        if (isSync) {
          if (res.ok) {
            if (isWrite) state.lastPushAt = Date.now();
            else state.lastPullAt = Date.now();
            state.lastError = "";
          } else {
            state.lastError = `同步 HTTP ${res.status}`;
          }
        }
        return res;
      },
      (err) => {
        if (isSync) state.lastError = `同步連線失敗：${err}`;
        throw err;
      },
    );
  };

  // ---- 4. 重複行螢光限流 --------------------------------------------------
  //
  // renderDuplicateLineHighlights 每次更新後 140ms 就跑一次，走過全文每一行，
  // 對每個要標記的重複項呼叫 getClientRects（每次都強迫瀏覽器重算版面），然後整層
  // 重建 —— 這就是螢光閃爍。限流成最多每 1.5 秒一次，標記還在，只是不再狂閃。
  //
  // 認得出它是因為它是具名函式而且頁面沒有壓縮。哪天作者改名或上壓縮，這段會自動
  // 失效（變成沒效果，不會壞）。
  const originalRaf = window.requestAnimationFrame;
  window.requestAnimationFrame = function (callback) {
    if (callback && callback.name === "renderDuplicateLineHighlights") {
      const now = Date.now();
      const since = now - state.lastHighlightAt;
      if (since < CONFIG.highlightMinGapMs) {
        return originalRaf.call(window, function skipped() {
          /* 這一輪跳過，下一次更新會再排一次 */
        });
      }
      state.lastHighlightAt = now;
    }
    return originalRaf.apply(window, arguments);
  };

  // ---- 5. 捲動防跳 --------------------------------------------------------
  //
  // 頁面裡有十幾處會改 scrollTop，Quill 在遠端更新進來時也會動選取範圍，所以與其猜
  // 是誰幹的，不如攔住動作本身：**直接接管捲動容器的 scrollTop 設定子**。
  //
  // 這樣分得乾乾淨淨 —— 使用者用滾輪、觸控、拖捲軸捲動「不會」經過這個設定子，只有
  // 程式碼指派 scrollTop 才會。第一版用「距離上次使用者捲動幾毫秒」來猜，結果實測
  // 直接失敗：程式的跳動正好就發生在使用者剛捲完那一瞬間，被誤判成使用者自己捲的。
  function findScroller() {
    let node = document.querySelector(".ql-editor");
    while (node && node !== document.body) {
      const style = getComputedStyle(node);
      if (/(auto|scroll)/.test(style.overflowY) && node.scrollHeight > node.clientHeight) return node;
      node = node.parentElement;
    }
    return document.scrollingElement || document.documentElement;
  }

  // 哪些動作算「使用者自己在捲」：滾輪、觸控、拖捲軸、方向鍵。
  //
  // 這裡要很小心地跟「使用者按了一個會捲動的按鈕」分開。第一版把所有 mousedown 和
  // 所有 keydown 都當成使用者在捲，結果點「找下一個重複」那種按鈕時，mousedown 先
  // 讓防跳進入警戒，接著程式要捲到目標行就被擋掉 —— 按鈕看起來整個壞掉。左邊分頁
  // 點標題跳行、Home/End 也一起遭殃。
  //
  // 所以分成兩種：點在控制項上（按鈕、連結、輸入框、分頁清單）是「我要它捲」，
  // 反而要替接下來的程式捲動開綠燈；點在別處才是「我自己在捲」。
  const CONTROL_SELECTOR =
    'button, a, [role="button"], input, select, label, .outline-list, .line-action-bar, .quick-panel';
  const SCROLL_INTENT_KEYS = ["ArrowUp", "ArrowDown", "PageUp", "PageDown", " ", "Spacebar"];

  function markUserScroll() {
    state.lastUserScrollAt = Date.now();
    // 使用者一旦自己捲動，先前因為點按鈕／敲鍵開的綠燈立刻取消。
    // 沒有這一行，點完按鈕或打完字的 1.5 秒內，遠端更新造成的跳動會整個放行 ——
    // 而「打完字往下捲就被拉回去」正好發生在那個空窗期。
    state.allowScrollUntil = 0;
  }
  function allowProgrammaticScrollBriefly() {
    state.allowScrollUntil = Date.now() + 1200;
  }

  ["wheel", "touchmove"].forEach((type) =>
    window.addEventListener(type, markUserScroll, { passive: true, capture: true }),
  );
  window.addEventListener(
    "mousedown",
    (e) => {
      const onControl = e.target instanceof Element && e.target.closest(CONTROL_SELECTOR);
      if (onControl) allowProgrammaticScrollBriefly();
      else markUserScroll(); // 拖捲軸、點編輯區都算自己捲
    },
    { passive: true, capture: true },
  );
  window.addEventListener(
    "keydown",
    (e) => {
      if (SCROLL_INTENT_KEYS.includes(e.key)) {
        // 方向鍵、PageUp/Down、空白鍵是「我自己在捲」。這裡不能同時記成「正在打字」，
        // 否則打字豁免（400ms 內放行程式捲動）會把方向鍵捲動之後的跳動一起放過去。
        markUserScroll();
        return;
      }
      state.lastUserInputAt = Date.now();
      allowProgrammaticScrollBriefly(); // Home/End、快捷鍵、搜尋 Enter 都要讓它捲
    },
    { capture: true },
  );
  window.addEventListener("input", () => { state.lastUserInputAt = Date.now(); }, { capture: true });

  // 現在該不該擋：使用者剛剛自己捲過、而且不是正在打字（打字時跳到游標是正常行為）。
  function shouldBlockProgrammaticScroll(from, to) {
    // 使用者剛按了按鈕或敲了導覽鍵：那是「請你捲過去」，不是要擋的跳動。
    if (Date.now() < state.allowScrollUntil) return false;
    if (Math.abs(to - from) < CONFIG.scrollGuardMinJumpPx) return false;
    if (Date.now() - state.lastUserInputAt < 400) return false;
    return Date.now() - state.lastUserScrollAt < CONFIG.scrollGuardMs;
  }

  const scrollTopDescriptor =
    Object.getOwnPropertyDescriptor(Element.prototype, "scrollTop") ||
    Object.getOwnPropertyDescriptor(HTMLElement.prototype, "scrollTop");

  function guardScroller(node) {
    if (!node || node.__collabGuardPatched || !scrollTopDescriptor) return;
    node.__collabGuardPatched = true;
    Object.defineProperty(node, "scrollTop", {
      configurable: true,
      get() {
        return scrollTopDescriptor.get.call(this);
      },
      set(value) {
        const from = scrollTopDescriptor.get.call(this);
        if (shouldBlockProgrammaticScroll(from, Number(value) || 0)) {
          state.blockedScrolls += 1;
          if (CONFIG.debugScroll) {
            console.warn("[守門員] 擋下程式捲動", { from, to: value }, new Error("來源").stack);
          }
          return; // 不動，使用者停在原地
        }
        scrollTopDescriptor.set.call(this, value);
      },
    });
  }

  let scroller = findScroller();
  guardScroller(scroller);
  setInterval(() => {
    const next = findScroller();
    if (next !== scroller) {
      scroller = next;
      guardScroller(scroller);
    }
  }, 5000);

  // scrollIntoView 不經過 scrollTop，單獨擋一次：遠端更新後把游標捲回視野就是走這條。
  const originalScrollIntoView = Element.prototype.scrollIntoView;
  Element.prototype.scrollIntoView = function (...args) {
    if (
      scroller &&
      scroller.contains(this) &&
      shouldBlockProgrammaticScroll(0, CONFIG.scrollGuardMinJumpPx) // 只看「使用者剛捲過且沒在打字」
    ) {
      state.blockedScrolls += 1;
      if (CONFIG.debugScroll) console.warn("[守門員] 擋下 scrollIntoView", new Error("來源").stack);
      return;
    }
    return originalScrollIntoView.apply(this, args);
  };

  // ---- 6. 版面修正 --------------------------------------------------------
  function applyLayoutFixes() {
    const rules = [];

    if (CONFIG.fixOutlineCollapse) {
      // 他的 CSS 把收合後的欄寬寫成跟展開時一模一樣：
      //   .main                   { grid-template-columns: 220px minmax(0,1fr) clamp(235px,20vw,250px) }
      //   .main.outline-collapsed { grid-template-columns: 220px minmax(0,1fr) clamp(235px,20vw,250px) }
      // 所以按漢堡只會把標題和清單 display:none，那 220px 的空欄還杵在那裡 ——
      // 看起來就像「按了沒反應」。右邊快捷面板的收合有把欄寬改成 42px，所以是好的。
      //
      // 兩邊可以各自收合，所以**四種組合都要寫**。第一版只寫了左邊收合那一種，
      // 結果把第三欄寫死成展開寬度：兩邊同時收合時，右邊會被這條規則蓋回去、縮不了。
      // 三個 class 的選擇器比兩個 class 的更具體，所以組合那條會贏。
      rules.push(
        ".main.outline-collapsed{grid-template-columns:42px minmax(0,1fr) clamp(235px,20vw,250px) !important}",
        ".main.outline-collapsed.quick-panel-collapsed{grid-template-columns:42px minmax(0,1fr) 42px !important}",
        "@media (max-width:860px){" +
          ".main.outline-collapsed{grid-template-columns:42px minmax(0,1fr) 220px !important}" +
          ".main.outline-collapsed.quick-panel-collapsed{grid-template-columns:42px minmax(0,1fr) 42px !important}" +
        "}",
        // 欄寬縮到 42px 之後，他原本的內距（左右各 10px + 標題列 4px）只留 14px，
        // 塞不下 30px 的漢堡按鈕。照他右邊面板收合時的做法：內距縮小、按鈕置中。
        ".main.outline-collapsed .outline-panel{padding:16px 5px !important}",
        ".main.outline-collapsed .outline-header{padding:0 !important;justify-content:center !important}",
      );
    }

    if (CONFIG.compactLineButtons) {
      // 行動作按鈕變窄，左邊空白才塞得下整排 —— 塞得下，他自己的程式就會把它放在
      // 文字左邊，不會退而求其次壓在文字上。治本的一半。
      rules.push(
        ".line-copy-btn{min-width:34px !important;padding:0 6px !important;font-size:12px !important}",
        ".line-duplicate-nav-btn{min-width:28px !important;padding:0 4px !important}",
        ".line-search-btn,.line-heading-btn{min-width:28px !important;padding:0 4px !important}",
      );
    }

    if (!rules.length) return;
    const style = document.createElement("style");
    style.textContent = rules.join("\n");
    document.head.appendChild(style);

    if (CONFIG.keepLineButtonsOffText) keepLineButtonsOffText();
  }

  // 治本的另一半：縮小之後還是塞不下時（放大檢視時左邊空白很窄），他的程式會把動作列
  // 直接貼在可視區左緣，壓在文字上。這裡在他定位完之後再把它往左挪到文字左邊；真的
  // 沒有空間就壓低透明度，至少看得到底下的字，滑鼠移上去才變清楚。
  function keepLineButtonsOffText() {
    const bar = document.getElementById("lineActionBar");
    const editor = document.querySelector(".ql-editor");
    if (!bar || !editor) return;

    let adjusting = false;
    const reposition = () => {
      if (adjusting) return;

      // 他有一條分支：「文件分頁收合時，就把動作列停進那一欄裡」
      // （canUseCollapsedOutline，可用寬度 = 欄寬 - 8*2）。以前那一欄收合後還是
      // 220px，塞得下；我們把它修成真的縮到 42px 之後，可用寬度只剩 26px，於是
      // 動作列被壓成 26px 寬又加上 wrap 變多行，整個咬在最左邊。
      //
      // 把那個擠壓解掉：拿掉 wrap、清掉他寫死的寬度，讓它回到自然寬度，再由下面的
      // 邏輯決定位置。收合後的那一欄本來就是空的（只有頂端一個漢堡），動作列蓋在
      // 上面沒有任何損失。
      if (bar.classList.contains("wrap")) bar.classList.remove("wrap");
      if (bar.style.width) bar.style.width = "";

      const barRect = bar.getBoundingClientRect();
      if (!barRect.width || !barRect.height) return;
      const textLeft = editor.getBoundingClientRect().left;

      // 一律貼齊文字左緣：不只是「不要蓋到字」，還要離你正在看的那一行夠近。
      // 他原本可能把它丟在左邊很遠的地方（收合分頁欄那條分支就是），眼睛要跑很遠。
      const wanted = Math.round(textLeft - barRect.width - 6);
      if (Math.abs(barRect.left - wanted) <= 1 && !bar.style.opacity) return;

      adjusting = true;
      if (wanted >= 2) {
        bar.style.left = `${wanted}px`;
        bar.style.opacity = "";
      } else {
        // 真的沒空間（放大到左邊幾乎沒有留白）：留在原處但淡化，讓底下的字看得見。
        bar.style.opacity = "0.35";
      }
      requestAnimationFrame(() => { adjusting = false; });
    };

    bar.addEventListener("mouseenter", () => { bar.style.opacity = "1"; });
    bar.addEventListener("mouseleave", () => { reposition(); });
    new MutationObserver(reposition).observe(bar, { attributes: true, attributeFilter: ["style", "class"] });
  }

  function restoreScrollAfterReload() {
    let saved;
    try {
      saved = sessionStorage.getItem(SCROLL_MARK_KEY);
      sessionStorage.removeItem(SCROLL_MARK_KEY);
    } catch (e) {
      return;
    }
    if (!saved) return;
    // 自動重載把你的捲動位置弄丟了，補回去。等文件載完才有高度可捲。
    let tries = 0;
    const tick = () => {
      const target = Number(saved);
      if (scroller && scroller.scrollHeight > target) {
        state.lastUserScrollAt = Date.now(); // 這次捲動是「還原」，不要被防跳攔下來
        scroller.scrollTop = target;
        return;
      }
      if (++tries < 40) setTimeout(tick, 250);
    };
    tick();
  }

  // 放在最後：它要用到第 5 節才建立的 scroller。
  restoreScrollAfterReload();

  console.log("[守門員] 已啟動：推送看門狗、同步狀態、螢光限流、捲動防跳、版面修正");
  }
})();
