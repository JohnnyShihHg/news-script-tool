// ==UserScript==
// @name         協作工具守門員
// @namespace    news-script-tool
// @version      1.3.0
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
    highlightMinGapMs: 800,    // 重複行螢光最快多久重畫一次（被跳過的到時間一定補畫）
    disableSpellcheck: true,   // 關掉編輯區的拼字檢查紅色波浪線
    scrollGuardMs: 1200,       // 使用者停止捲動後多久內的程式捲動視為「被拉走」
    scrollGuardMinJumpPx: 80,  // 跳超過這個距離才攔
    coldStartReloadSeconds: 20, // 新開分頁超過這麼久還在「正在載入共同文章」就自動重整一次。0 = 關閉
    autoReloadAfterStuckSeconds: 0, // 0 = 關閉。設成例如 120 就會在卡住兩分鐘後自動重載
    reloadCooldownMinutes: 5,  // 兩次自動重載至少間隔這麼久，避免伺服器掛掉時無限重載
    staleSeconds: 90,          // 收到別人的更新、畫面卻一直沒變超過這麼久，就當作「接收停擺」
    staleMinUpdates: 3,        // 而且這段時間至少收到這麼多筆（避免一兩筆不顯示的小更新誤判）
    backfillCooldownMinutes: 2, // 收不到別人的內容時「背景補讀」整份，兩次至少間隔這麼久
    backfillMaxPerHour: 10,    // 背景補讀一小時最多幾次（一次約 10 MB，別拖垮協作工具）
    staleReload: true,         // 補讀後還是收不到：你 30 秒沒打字、沒有待上傳就自動重整
    staleReloadCooldownMinutes: 10, // 這種重整兩次至少間隔這麼久
    debugScroll: false,        // 開了會在 console 印出每一次程式捲動的來源堆疊

    showHud: false,            // 右下角那個狀態小框
    showAlertFrame: false,     // 卡住時整頁紅框。預設不顯示（使用者 11/11 決定）；偵測和自動重整照常
    compactLineButtons: true,  // 行動作按鈕縮小，比較容易塞進左邊空白不蓋到文字
    keepLineButtonsOffText: true, // 行動作列蓋到文字時往左挪開
    fixOutlineCollapse: true,  // 修他「文件分頁」漢堡收合後寬度沒縮的 bug
  };
  // 測試頁用：在載入腳本前設定 window.__collabGuardTestConfig 就能把秒數調短。
  if (window.__collabGuardTestConfig) Object.assign(CONFIG, window.__collabGuardTestConfig);

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

  // 接收停擺偵測用（第 7 節）。count／since：從畫面最後一次跟著更新變動之後，累積收到幾筆
  // 「套上去畫面卻沒變」的更新、第一筆是什麼時候。
  const STALE_RELOAD_KEY = "collabGuardStaleReloadAt";
  const STALE_LOG_KEY = "collabGuardStaleEvents";
  const stale = { count: 0, since: 0, lastDomAt: Date.now(), cursor: "", gap: null, gapConfirmed: false, reason: "" };

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

  // 「這個分頁剛自動重整過」的記號存兩份：sessionStorage，加上 window.name。網站儲存被
  // 封鎖時 sessionStorage 存不進也讀不到，只靠它的話每 20 秒就會重整一次、停不下來；
  // window.name 跟著分頁走、重新整理後還在，不受儲存封鎖影響。頁面原本的 window.name
  // 內容保留，只附加／替換我們這一段。
  // 接收停擺的重整（第 7 節）也用同一套記號，只是 key 不同。
  function readReloadMark(key) {
    let at = 0;
    try {
      at = Number(sessionStorage.getItem(key) || 0);
    } catch (e) {
      /* 儲存被封鎖：改看 window.name */
    }
    if (!at) {
      const m = String(window.name || "").match(new RegExp(`\\s*${key}=(\\d+)`));
      if (m) at = Number(m[1]);
    }
    return at;
  }
  function writeReloadMark(key, at) {
    try {
      sessionStorage.setItem(key, String(at));
    } catch (e) {
      /* 儲存被封鎖：還有 window.name 那份 */
    }
    try {
      const rest = String(window.name || "").replace(new RegExp(`\\s*${key}=(\\d+)`), "");
      window.name = `${rest ? `${rest} ` : ""}${key}=${at}`;
    } catch (e) {
      /* 兩份都存不進去的話，最壞是下一次開頁再重整一次 */
    }
  }
  const readColdMark = () => readReloadMark(COLD_RELOAD_KEY);
  const writeColdMark = (at) => writeReloadMark(COLD_RELOAD_KEY, at);
  {
    const at = readColdMark();
    cold.reloadedBefore = at > 0 && Date.now() - at < 10 * 60_000;
  }
  for (const type of ["keydown", "input", "paste", "drop", "compositionstart"]) {
    window.addEventListener(type, () => { cold.typed = true; }, true);
  }

  // 頁面的狀態文字沒有固定 id，用文字特徵找：那幾種寫法是 deriveSyncStatus 固定產出的。
  function findStatusText() {
    const texts = ["已同步", "同步中", "待同步", LOADING_TEXT, "連線中", "離線"];
    const nodes = document.querySelectorAll("span, div, small, p");
    for (const n of nodes) {
      // 狀態文字在編輯器外面；文件內容裡剛好有「離線」之類的字不算。
      if (n.children.length || (n.closest && n.closest(".ql-editor"))) continue;
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

  // ---- 0b. 快取診斷（v1.1.0，只讀）--------------------------------------------
  //
  // 冷啟動慢、要重整才讀得完，懷疑是協作工具的本地快取（IndexedDB）沒寫好，每次開頁都
  // 從頭讀。這裡只「數」這個網站有哪些 IndexedDB、每個物件庫幾筆、整個網站用了多少儲存
  // 空間，不讀任何內容（文件裡有未播出的稿）。讀完 30 秒後跑一次，記在 localStorage
  // collabGuardCacheInfo，HUD 顯示摘要；console 打 collabGuardDiag() 可以再跑一次。
  // 打開的連線數完馬上關；頁面要升級資料庫時也立刻讓開，不擋協作工具自己的存取。
  const CACHE_INFO_KEY = "collabGuardCacheInfo";
  const cacheInfo = { summary: "" };

  function countDatabase(name) {
    return new Promise((resolve) => {
      const out = { name, stores: [], records: 0 };
      let req;
      try {
        req = indexedDB.open(name);
      } catch (e) {
        out.error = String(e);
        resolve(out);
        return;
      }
      // 只有資料庫不存在才會走到這裡：不要替它建立，取消。
      req.onupgradeneeded = () => {
        try { req.transaction.abort(); } catch (e) { /* 已經結束 */ }
      };
      req.onerror = () => {
        out.error = String(req.error);
        resolve(out);
      };
      req.onsuccess = () => {
        const db = req.result;
        db.onversionchange = () => db.close();
        const names = Array.from(db.objectStoreNames);
        if (!names.length) {
          db.close();
          resolve(out);
          return;
        }
        let pending = names.length;
        const finish = () => {
          if (--pending === 0) {
            db.close();
            resolve(out);
          }
        };
        try {
          const tx = db.transaction(names, "readonly");
          for (const s of names) {
            const c = tx.objectStore(s).count();
            c.onsuccess = () => {
              out.stores.push({ name: s, count: c.result });
              out.records += c.result;
              finish();
            };
            c.onerror = () => {
              out.stores.push({ name: s, count: null });
              finish();
            };
          }
        } catch (e) {
          out.error = String(e);
          db.close();
          resolve(out);
        }
      };
    });
  }

  async function collectCacheInfo() {
    const info = { at: new Date().toISOString(), databases: [], usageMB: null };
    try {
      if (navigator.storage && navigator.storage.estimate) {
        const est = await navigator.storage.estimate();
        info.usageMB = Math.round(((est.usage || 0) / 1048576) * 10) / 10;
      }
      const dbs = indexedDB.databases ? await indexedDB.databases() : [];
      for (const d of dbs) {
        if (d.name) info.databases.push(await countDatabase(d.name));
      }
    } catch (e) {
      info.error = String(e);
    }
    const total = info.databases.reduce((n, d) => n + d.records, 0);
    cacheInfo.summary =
      `本地快取：${info.databases.length} 個資料庫、${total} 筆` +
      (info.usageMB !== null ? `、網站用了 ${info.usageMB} MB` : "");
    try {
      localStorage.setItem(CACHE_INFO_KEY, JSON.stringify(info));
    } catch (e) {
      /* 存不進去只少一份診斷 */
    }
    console.info("[守門員] 快取診斷", info);
    return info;
  }
  window.collabGuardDiag = collectCacheInfo;

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
        setTimeout(collectCacheInfo, 30_000);
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
        writeColdMark(Date.now());
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
    const receiveStalled = checkReceiveStall(pageStatus);
    const stuck = receiveStalled || /待同步|正在載入共同文章|連線中|離線/.test(pageStatus);
    if (stuck) {
      if (!state.stuckSince) state.stuckSince = Date.now();
    } else {
      state.stuckSince = 0;
    }
    // 紅框預設不顯示；唯一例外是「收不到別人的內容、你卻正在打字」—— 這時打的字可能被
    // 排到文件中間，要讓你知道（使用者 11/11 決定）。
    const typingWhileStalled = receiveStalled && Date.now() - state.lastUserInputAt < 30_000;
    alertFrame.style.display = (stuck && CONFIG.showAlertFrame) || typingWhileStalled ? "block" : "none";
    hud.innerHTML =
      `<b>守門員</b>　${pageStatus || "（讀不到狀態）"}<br>` +
      `上次推送 ${ago(state.lastPushAt)}・上次收取 ${ago(state.lastPullAt)}` +
      (cold.seconds !== null ? `<br>開頁讀取 ${cold.seconds} 秒${cold.reloadedBefore ? "（自動重整後）" : ""}` : "") +
      (cacheInfo.summary ? `<br>${cacheInfo.summary}` : "") +
      (state.blockedScrolls ? `<br>已擋下 ${state.blockedScrolls} 次捲動跳動` : "") +
      (state.lastError ? `<br><span style="color:#991b1b">${state.lastError}</span>` : "") +
      (receiveStalled ? `<br><span style="color:#991b1b">收不到別人的內容：${stale.reason}</span>` : "") +
      (backfill.lastDoneAt ? `<br>上次背景補讀 ${ago(backfill.lastDoneAt)}（共 ${backfill.count} 次）` : "") +
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
    // 背景補讀（第 7 節）：把頁面自己下一次的拉取改成「從頭拉」，伺服器會回整份快照，
    // 頁面照它平常的流程套上去。只改網址，不自己另外發請求。
    let callArgs = arguments;
    const isBackfill = isSync && !isWrite && typeof input === "string" && takeBackfillTurn();
    if (isBackfill) callArgs = [url.replace(/([?&]after=)\d+/, "$10"), init];
    return originalFetch.apply(this, callArgs).then(
      (res) => {
        if (isSync) {
          if (res.ok) {
            if (isWrite) state.lastPushAt = Date.now();
            else {
              state.lastPullAt = Date.now();
              // 另外讀一份副本看收到什麼；頁面自己讀的那份不受影響。
              res.clone().json().then((p) => noteIncoming(p, url, isBackfill)).catch(() => {
                if (isBackfill) finishBackfill(false);
              });
            }
            state.lastError = "";
          } else {
            state.lastError = `同步 HTTP ${res.status}`;
            if (isBackfill) finishBackfill(false);
          }
        }
        return res;
      },
      (err) => {
        if (isSync) state.lastError = `同步連線失敗：${err}`;
        if (isBackfill) finishBackfill(false);
        throw err;
      },
    );
  };

  // ---- 7. 收不到別人的內容：偵測＋背景補讀（v1.2.0）---------------------------
  //
  // 11/11 實際遇到：Chrome 分頁停在 11:05 的內容，App 的協作視窗已經到 15:15，兩邊都寫
  // 「已同步」。在那個分頁量了 60 秒：輪詢全部成功、收到 3 筆別人的更新，但編輯器畫面
  // 一次都沒變 —— **收得到、套不上去**。
  //
  // 原因（11/11 直接問伺服器確認）：伺服器每收到一份「整份快照」，就把快照之前的逐筆
  // 更新刪掉；之後用 after=N 來拉的人，只拿得到還沒被刪的。分頁如果落後（背景分頁被
  // Chrome 節流成一分鐘才拉一次），中間那段在它拉到之前就被刪了，伺服器也不會補快照給它。
  // 少了那一段，之後別人在那段之後打的字，Yjs 都會卡在「等前面那段」，永遠不顯示。頁面
  // 的狀態燈只看連線和待上傳，所以一直是綠的。
  //
  // 補救不需要重整：after=0 時伺服器會回整份快照，裡面就有漏掉的那段。所以把頁面自己
  // 的下一次拉取改成 after=0，頁面照平常流程套上去，卡住的內容一次全部出現。這跟開頁時
  // 頁面自己做的事一樣，只是不重建編輯器、不打斷打字。
  //
  // 兩種偵測：
  //  a. 漏號：拉回來的第一筆編號 > 自己的進度＋1（中間那段被刪了）。先記著，等下一筆別人
  //     的更新也沒顯示出來，就確定、馬上補讀。漏號但內容照樣顯示，表示漏的那段沒影響，不動。
  //  b. 保底：收到別人的更新 1.5 秒內畫面沒變，累積 staleMinUpdates 筆、超過 staleSeconds 秒。
  //
  // 不能把協作工具拖垮（使用者 11/11 交代）：
  //  - 只改頁面自己的拉取網址，不另外發請求；一次只補一份。
  //  - 背景分頁不補（等你切回來再補），補讀至少間隔 backfillCooldownMinutes，一小時最多
  //    backfillMaxPerHour 次。快照目前約 10 MB，跟重整一次讀的一樣。
  //  - 補讀失敗或補完還是卡住，才走自動重整（30 秒沒打字、沒有待上傳、10 分鐘一次）。
  const backfill = { wanted: false, inFlight: false, lastAt: 0, lastDoneAt: 0, count: 0, recent: [] };

  function logStaleEvent(kind, detail) {
    console.warn(`[守門員] ${kind}`, detail || "");
    try {
      const list = JSON.parse(localStorage.getItem(STALE_LOG_KEY) || "[]");
      list.push({ at: new Date().toISOString(), kind, ...(detail || {}) });
      localStorage.setItem(STALE_LOG_KEY, JSON.stringify(list.slice(-30)));
    } catch (e) {
      /* 存不進去只少一筆紀錄 */
    }
  }

  function sideNoteValue() {
    const box = document.getElementById("sideNoteInput");
    return box ? box.value : "";
  }

  function noteIncoming(payload, url, wasBackfill) {
    if (wasBackfill) {
      finishBackfill(Array.isArray(payload?.updates) && payload.updates.length > 0);
      return;
    }
    if (!payload || payload.hasMore) return;
    const ups = Array.isArray(payload.updates) ? payload.updates : [];
    if (!ups.length) return;

    const after = Number((url.match(/[?&]after=(\d+)/) || [])[1] || 0);
    const cursors = ups.map((e) => Number(e?.cursor)).filter((c) => Number.isFinite(c) && c > 0);
    if (after > 0 && cursors.length) {
      const first = Math.min(...cursors);
      if (first > after + 1 && !stale.gap) {
        stale.gap = { from: after + 1, to: first - 1 };
        // 只印 console 不存：漏號但內容照樣顯示時很常見，存了會把紀錄洗掉。確定有影響才存。
        console.info(`[守門員] 漏號 ${after + 1}～${first - 1}，看下一筆有沒有顯示`);
      }
    }

    // 自己剛上傳完收到的，多半是伺服器把自己的更新送回來，套上去畫面本來就不會變。
    if (Date.now() - state.lastPushAt < 3000) return;
    // 很大的一筆是整份快照，套上去也不會變。
    const n = ups.filter((e) => typeof e?.update === "string" && e.update.length < 20_000).length;
    if (!n) return;
    const arrivedAt = Date.now();
    const sideBefore = sideNoteValue();
    stale.cursor = String(payload.nextCursor ?? "");
    setTimeout(() => {
      // 畫面（或便條紙）有跟著變：正常。漏號也就沒有影響。
      if (stale.lastDomAt >= arrivedAt || sideNoteValue() !== sideBefore) {
        stale.gap = null;
        return;
      }
      if (!stale.count) stale.since = arrivedAt;
      stale.count += n;
      if (stale.gap && !stale.gapConfirmed) {
        stale.gapConfirmed = true;
        logStaleEvent("漏號而且收到的內容沒顯示", { missing: `${stale.gap.from}～${stale.gap.to}` });
      }
    }, 1500);
  }

  function watchEditorChanges() {
    const targets = [document.querySelector(".ql-editor"), document.getElementById("frozenTextPanel")];
    const observer = new MutationObserver(() => {
      // 自己打字造成的變動不算「別人的更新有顯示出來」。
      if (Date.now() - state.lastUserInputAt < 1000) return;
      stale.lastDomAt = Date.now();
      stale.count = 0;
      stale.since = 0;
    });
    for (const t of targets) {
      if (t) observer.observe(t, { childList: true, subtree: true, characterData: true, attributes: t.id === "frozenTextPanel" });
    }
  }
  watchEditorChanges();

  function requestBackfill(reason) {
    if (backfill.wanted || backfill.inFlight) return true;
    const now = Date.now();
    backfill.recent = backfill.recent.filter((t) => now - t < 3_600_000);
    if (now - backfill.lastAt < CONFIG.backfillCooldownMinutes * 60_000) return false;
    if (backfill.recent.length >= CONFIG.backfillMaxPerHour) return false;
    backfill.wanted = true;
    logStaleEvent("排定背景補讀", { reason });
    return true;
  }

  // fetch 攔截呼叫：這一次拉取要不要改成補讀。背景分頁不補，等切回來。
  function takeBackfillTurn() {
    if (!backfill.wanted || backfill.inFlight || document.hidden) return false;
    backfill.wanted = false;
    backfill.inFlight = true;
    backfill.lastAt = Date.now();
    backfill.recent.push(backfill.lastAt);
    backfill.count += 1;
    return true;
  }

  function finishBackfill(ok) {
    backfill.inFlight = false;
    if (!ok) {
      logStaleEvent("背景補讀失敗");
      return;
    }
    backfill.lastDoneAt = Date.now();
    // 頁面接著會把快照套上去，卡住的內容一次出現。重新開始觀察。
    stale.count = 0;
    stale.since = 0;
    stale.gap = null;
    stale.gapConfirmed = false;
    logStaleEvent("背景補讀完成", { cursor: stale.cursor });
  }

  // 每秒由 refreshHud 呼叫。回傳「現在是不是收不到別人的內容」。
  function checkReceiveStall(pageStatus) {
    const heuristic =
      stale.count >= CONFIG.staleMinUpdates &&
      stale.since > 0 &&
      Date.now() - stale.since >= CONFIG.staleSeconds * 1000;
    const stalled = heuristic || stale.gapConfirmed;
    if (!stalled) {
      stale.reason = "";
      return false;
    }
    stale.reason = stale.gapConfirmed
      ? `伺服器那段（${stale.gap ? `${stale.gap.from}～${stale.gap.to}` : "?"}）沒拿到`
      : `收到 ${stale.count} 筆，畫面都沒變（${Math.round((Date.now() - stale.since) / 1000)} 秒）`;
    if (document.hidden) return true; // 背景分頁：等切回來再處理
    if (backfill.wanted || backfill.inFlight) return true;
    if (requestBackfill(stale.reason)) return true;
    maybeReloadForStale(pageStatus); // 補讀用完次數或剛補過還是卡住
    return true;
  }

  let reloadBlockedLogged = false;
  function maybeReloadForStale(pageStatus) {
    const canReload =
      CONFIG.staleReload &&
      pageStatus.startsWith("已同步") && // 有待上傳的東西時不重整，免得掉資料
      Date.now() - state.lastUserInputAt > 30_000 &&
      Date.now() - readReloadMark(STALE_RELOAD_KEY) > CONFIG.staleReloadCooldownMinutes * 60_000;
    if (!canReload) {
      if (!reloadBlockedLogged) {
        reloadBlockedLogged = true;
        logStaleEvent("補讀後還是收不到，等時機重整", { reason: stale.reason });
      }
      return;
    }
    logStaleEvent("補讀後還是收不到，自動重整", { reason: stale.reason });
    writeReloadMark(STALE_RELOAD_KEY, Date.now());
    try {
      if (scroller) sessionStorage.setItem(SCROLL_MARK_KEY, String(scroller.scrollTop));
    } catch (e) {
      /* 存不進去只是重整後回到頁首 */
    }
    location.reload();
  }

  // ---- 4. 重複行螢光限流 --------------------------------------------------
  //
  // renderDuplicateLineHighlights 每次更新後 140ms 就跑一次，走過全文每一行，
  // 對每個要標記的重複項呼叫 getClientRects（每次都強迫瀏覽器重算版面），然後整層
  // 重建 —— 這就是螢光閃爍。限流成最多每 highlightMinGapMs 一次，標記還在，只是不再狂閃。
  //
  // 被跳過的那次一定補畫（1.3.0）：以前跳過就丟掉，等「下一次有變動」才重畫，你換完行
  // 如果沒人再動文件，螢光就一直停在舊的。現在間隔一到就補畫一次最新的狀態。
  //
  // 認得出它是因為它是具名函式而且頁面沒有壓縮。哪天作者改名或上壓縮，這段會自動
  // 失效（變成沒效果，不會壞）。
  const originalRaf = window.requestAnimationFrame;
  let highlightTrailing = 0;
  window.requestAnimationFrame = function (callback) {
    if (callback && callback.name === "renderDuplicateLineHighlights") {
      const now = Date.now();
      const since = now - state.lastHighlightAt;
      if (since < CONFIG.highlightMinGapMs) {
        if (!highlightTrailing) {
          highlightTrailing = setTimeout(() => {
            highlightTrailing = 0;
            window.requestAnimationFrame(callback);
          }, CONFIG.highlightMinGapMs - since);
        }
        return originalRaf.call(window, function skipped() {
          /* 這一輪跳過，間隔到了由上面的計時器補畫 */
        });
      }
      state.lastHighlightAt = now;
      return originalRaf.call(window, function renderDuplicateLineHighlights(ts) {
        callback(ts);
        anchorHighlightMarks();
      });
    }
    return originalRaf.apply(window, arguments);
  };

  // ---- 4b. 螢光黏住那一行（1.3.0）------------------------------------------
  //
  // 頁面的螢光不是畫在文字上，是另一層用「當下的像素座標」放的色條。你在上面換行、
  // 文字往下移，色條還停在原本的像素位置，要等下一次重畫才跟上 —— 看起來就是螢光
  // 「沒黏著那行」。
  //
  // 這裡在頁面每次畫完後，記下每個色條屬於編輯器的哪一行（.ql-editor 底下的哪個元素）、
  // 離那行頂端多遠；之後編輯器一有變動，就照那行現在的位置把色條挪過去。行被刪掉的
  // 色條先藏起來，等下一次重畫。只移動色條，不碰文件內容（不改編輯器裡的任何元素，
  // 改了 Quill 會當成編輯、同步給所有人）。
  let anchoredMarks = [];
  function anchorHighlightMarks() {
    anchoredMarks = [];
    try {
      const layer = document.getElementById("duplicateHighlightLayer");
      const editor = document.querySelector(".ql-editor");
      const page = document.querySelector(".doc-page");
      if (!layer || !editor || !page) return;
      const marks = Array.from(layer.children);
      if (!marks.length) return;
      const pageTop = page.getBoundingClientRect().top;
      const blocks = Array.from(editor.children);
      const tops = blocks.map((b) => {
        const r = b.getBoundingClientRect();
        return [r.top - pageTop, r.bottom - pageTop];
      });
      for (const mark of marks) {
        const top = parseFloat(mark.style.top);
        if (!Number.isFinite(top)) continue;
        // 色條畫在文字底部往上 10px，用它中間那點找所屬的行（二分搜尋，行是由上往下排的）。
        const y = top + 4;
        let lo = 0;
        let hi = tops.length - 1;
        let found = -1;
        while (lo <= hi) {
          const mid = (lo + hi) >> 1;
          if (y < tops[mid][0]) hi = mid - 1;
          else if (y >= tops[mid][1]) lo = mid + 1;
          else {
            found = mid;
            break;
          }
        }
        if (found >= 0) anchoredMarks.push({ mark, block: blocks[found], offset: top - tops[found][0] });
      }
    } catch (e) {
      anchoredMarks = []; // 出錯就退回原本的行為
    }
  }

  let followPending = false;
  function followHighlightMarks() {
    if (followPending || !anchoredMarks.length) return;
    followPending = true;
    originalRaf.call(window, () => {
      followPending = false;
      try {
        const page = document.querySelector(".doc-page");
        if (!page) return;
        const pageTop = page.getBoundingClientRect().top;
        for (const a of anchoredMarks) {
          if (!a.block.isConnected) {
            a.mark.style.visibility = "hidden";
            continue;
          }
          const want = Math.round(a.block.getBoundingClientRect().top - pageTop + a.offset);
          if (parseFloat(a.mark.style.top) !== want) a.mark.style.top = `${want}px`;
        }
      } catch (e) {
        /* 跟不上就等下一次重畫 */
      }
    });
  }
  {
    const editor = document.querySelector(".ql-editor");
    if (editor) {
      new MutationObserver(followHighlightMarks).observe(editor, { childList: true, subtree: true, characterData: true });
    }
  }

  // ---- 4c. 關掉拼字檢查的紅色波浪線（1.3.0）-----------------------------------
  //
  // 瀏覽器對可編輯區預設開拼字檢查，中文稿幾乎每行都被畫紅波浪。spellcheck 只是瀏覽器
  // 的顯示設定，不是文件內容，不會同步給別人。編輯器之外的便條紙也一起關。
  function disableSpellcheck() {
    if (!CONFIG.disableSpellcheck) return;
    for (const node of document.querySelectorAll(".ql-editor, #sideNoteInput")) {
      if (node.getAttribute("spellcheck") !== "false") node.setAttribute("spellcheck", "false");
    }
  }
  disableSpellcheck();
  setInterval(disableSpellcheck, 5000);

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
