// 書籤小工具的介面：在協作工具那一頁疊一個浮動面板。
// 只讀不寫 —— 這支程式裡沒有任何寫回文件的路徑，拿到的 DOM 只用來讀文字。
(function () {
  "use strict";

  var L =
    (typeof module !== "undefined" && module.exports && module.exports.CollabMarkerLookup) ||
    (typeof globalThis !== "undefined" ? globalThis.CollabMarkerLookup : null);

  var HOST_ID = "collab-marker-panel-host";

  // 再點一次書籤就收起來。舊的那一份有掛在 document 上的監聽器，一定要讓它自己收尾，
  // 直接 remove() 會把監聽器留在頁面上，開開關關幾次就疊一堆。
  var existing = document.getElementById(HOST_ID);
  if (existing) {
    if (typeof existing.__collabMarkerCleanup === "function") existing.__collabMarkerCleanup();
    else existing.remove();
    return;
  }

  // 同源的 iframe 也找一遍：協作文件有可能被包在 iframe 裡，而書籤只跑在最外層。
  // 跨網域的 iframe 讀不到（瀏覽器擋），那就讀不到，不會假裝有。
  function allEditors() {
    var found = [];
    function collect(doc) {
      try {
        var list = doc.querySelectorAll(".ql-editor");
        for (var i = 0; i < list.length; i++) found.push(list[i]);
        var frames = doc.querySelectorAll("iframe");
        for (var f = 0; f < frames.length; f++) {
          var inner = null;
          try {
            inner = frames[f].contentDocument;
          } catch (e) {
            inner = null; // 跨網域，跳過
          }
          if (inner) collect(inner);
        }
      } catch (e) {
        /* 讀不到就算了 */
      }
    }
    collect(document);
    return found;
  }

  // 一個區塊元素就是文件的一行。
  //
  // 這裡刻意用 textContent 而不是 innerText：innerText 只拿得到「看得見」的文字，
  // 區塊被收合、被 CSS 藏起來、或捲動區外被瀏覽器省略時就讀不到 —— 那等於一則
  // 勿上網的稿對這個工具隱形。寧可多讀到看不見的行，也不能漏。
  function linesOf(el) {
    var out = [];
    var kids = el.children;
    if (!kids || kids.length === 0) {
      return String(el.textContent || "").split(/\r?\n/);
    }
    for (var i = 0; i < kids.length; i++) {
      var k = kids[i];
      // 清單（ol/ul）的每個 li 各自是一行。
      if (k.children && k.children.length && /^(ol|ul)$/i.test(k.tagName)) {
        for (var j = 0; j < k.children.length; j++) {
          out.push(String(k.children[j].textContent || ""));
        }
      } else {
        out.push(String(k.textContent || ""));
      }
    }
    return out;
  }

  function readDocLines() {
    var editors = allEditors();
    var best = null;
    var bestLen = -1;
    for (var i = 0; i < editors.length; i++) {
      var len = String(editors[i].textContent || "").length;
      if (len > bestLen) {
        bestLen = len;
        best = editors[i];
      }
    }
    if (best) return { lines: linesOf(best), fromEditor: true };

    // 退路：整頁的文字。讀之前先把面板藏起來 —— 它也在這一頁上，不藏的話你剛打進
    // 輸入框的 slug 會被當成文件內容讀回來，然後自己命中自己。
    var prevDisplay = host.style.display;
    host.style.display = "none";
    var text = document.body ? String(document.body.innerText || "") : "";
    host.style.display = prevDisplay;
    return { lines: text.split(/\r?\n/), fromEditor: false };
  }

  var host = document.createElement("div");
  host.id = HOST_ID;
  host.style.cssText = "position:fixed;top:16px;right:16px;z-index:2147483647;";
  var root = host.attachShadow ? host.attachShadow({ mode: "open" }) : host;

  var style = document.createElement("style");
  // 用 shadow DOM 隔開，協作工具自己的 CSS 不會影響面板，面板也不會影響它。
  style.textContent = [
    ":host{all:initial}",
    "*{box-sizing:border-box;font-family:system-ui,-apple-system,'Microsoft JhengHei',sans-serif}",
    ".box{width:320px;background:#fff;color:#1a1a1a;border:1px solid #d0d0d0;border-radius:10px;",
    "box-shadow:0 8px 28px rgba(0,0,0,.18);overflow:hidden}",
    ".hd{display:flex;align-items:center;justify-content:space-between;padding:8px 10px;",
    "background:#f3f4f6;border-bottom:1px solid #e0e0e0;cursor:move;user-select:none}",
    ".hd b{font-size:13px;font-weight:600}",
    ".x{border:0;background:transparent;font-size:15px;line-height:1;cursor:pointer;color:#666;padding:2px 4px}",
    ".bd{padding:10px}",
    "textarea{width:100%;height:92px;resize:vertical;font-size:13px;line-height:1.5;padding:6px 8px;",
    "border:1px solid #ccc;border-radius:6px;font-family:inherit}",
    ".row{display:flex;align-items:center;justify-content:space-between;margin-top:8px;gap:8px}",
    ".hint{font-size:11px;color:#777}",
    "button.go{border:0;background:#2563eb;color:#fff;font-size:13px;padding:6px 14px;border-radius:6px;cursor:pointer}",
    "button.go:hover{background:#1d4ed8}",
    ".out{margin-top:10px;max-height:300px;overflow:auto;font-size:13px}",
    ".item{padding:6px 0;border-top:1px solid #eee}",
    ".item:first-child{border-top:0}",
    ".slug{font-weight:600;word-break:break-all}",
    ".tag{display:inline-block;margin-top:3px;padding:1px 7px;border-radius:999px;font-size:12px}",
    ".no{background:#fee2e2;color:#991b1b}",
    ".ok{background:#dcfce7;color:#166534}",
    ".ot{background:#fef3c7;color:#92400e}",
    ".ms{background:#eee;color:#555}",
    ".n{color:#777;font-size:11px;margin-left:4px}",
    ".sug{margin-top:4px;padding-left:10px;border-left:2px solid #ddd;font-size:12px;color:#444;line-height:1.9}",
    ".sug b{font-weight:600;word-break:break-all}",
    ".sug.warn{border-left-color:#dc2626;color:#991b1b}",
    ".err{color:#991b1b;font-size:12px;margin-top:8px;line-height:1.5}",
  ].join("");

  var box = document.createElement("div");
  box.className = "box";
  box.innerHTML = [
    '<div class="hd"><b>查上網標記</b><button class="x" title="關閉（Esc）">✕</button></div>',
    '<div class="bd">',
    '<textarea placeholder="一行一個 slug，貼上後按查詢"></textarea>',
    '<div class="row"><span class="hint">一行一個 · ⌘/Ctrl+Enter</span><button class="go">查詢</button></div>',
    '<div class="out"></div>',
    "</div>",
  ].join("");

  root.appendChild(style);
  root.appendChild(box);
  document.body.appendChild(host);

  var ta = box.querySelector("textarea");
  var out = box.querySelector(".out");

  function esc(s) {
    return String(s).replace(/[&<>"']/g, function (c) {
      return { "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c];
    });
  }

  function render(results, info) {
    if (!results.length) {
      out.innerHTML = '<div class="err">沒有輸入任何 slug。</div>';
      return;
    }
    var html = "";
    for (var i = 0; i < results.length; i++) {
      var r = results[i];
      html += '<div class="item"><div class="slug">' + esc(r.slug) + "</div>";
      if (!r.found) {
        html += '<span class="tag ms">⚠ 文件裡找不到</span>';
        // 只差一兩個字的行當成提示列出來 —— 多半是打錯字或同音字，但判斷權留給人。
        for (var k = 0; k < r.suggestions.length; k++) {
          var sg = r.suggestions[k];
          var scls = sg.status === "no-upload" ? "no" : sg.status === "other" ? "ot" : "ok";
          var slabel =
            sg.status === "no-upload"
              ? "⛔ (勿上網)"
              : sg.status === "other"
                ? "⚠ " + esc(sg.markers.join("、"))
                : "✅ 無標記";
          html +=
            '<div class="sug">是不是這則：<b>' + esc(sg.text) + "</b> " +
            '<span class="tag ' + scls + '">' + slabel + "</span>" +
            '<span class="n">差 ' + sg.distance + " 字</span></div>";
        }
      } else {
        for (var g = 0; g < r.groups.length; g++) {
          var grp = r.groups[g];
          var cls = grp.status === "no-upload" ? "no" : grp.status === "other" ? "ot" : "ok";
          var label =
            grp.status === "no-upload"
              ? "⛔ (勿上網)"
              : grp.status === "other"
                ? "⚠ 其他標記：" + esc(grp.markers.join("、"))
                : "✅ 無標記";
          // slug 前面沒有括號的裸字（UP、暫停…）照原樣附在後面：我們不知道它的語意，
          // 不敢替它下判斷，但也不能當成沒看見。
          // prefix 到這裡已經濾掉上傳者簽名與含「勿」的字（後者由 ⛔ 標籤代表）。
          var extra = grp.prefix || [];
          var pre = extra.length ? '<span class="n">· 前面有「' + esc(extra.join(" ")) + "」</span>" : "";
          if (grp.behind) pre += '<span class="n">· 標記寫在後面</span>';
          // 部分命中務必把文件裡的完整寫法印出來，否則無從確認指的是不是同一則。
          if (grp.partial) pre += '<span class="n">· 部分命中：' + esc(grp.partial) + "</span>";
          html +=
            '<div><span class="tag ' + cls + '">' + label + "</span>" +
            (grp.count > 1 ? '<span class="n">×' + grp.count + "</span>" : "") +
            pre +
            "</div>";
        }
      }
      // 文件是對的、錯的是輸入端，所以不管上面查成什麼樣子，只要文件裡有相關的
      // 勿上網稿就一定要紅字講出來。
      for (var w = 0; w < (r.warnings || []).length; w++) {
        html +=
          '<div class="sug warn">⛔ 文件裡有一則勿上網的很像：<b>' +
          esc(r.warnings[w].text) + "</b></div>";
      }
      html += "</div>";
    }
    if (info) html += '<div class="err">' + esc(info) + "</div>";
    out.innerHTML = html;
  }

  function run() {
    if (!L) {
      out.innerHTML = '<div class="err">判斷邏輯沒有載入成功，請重新建立書籤。</div>';
      return;
    }
    var doc = readDocLines();
    var slugs = L.parseSlugInput(ta.value);
    var info = doc.fromEditor
      ? ""
      : "注意：這一頁找不到協作工具的編輯器（.ql-editor），目前是拿整頁文字在比對，結果可能不準。請確認已經開在協作文件頁、且文件已載入完成。";
    render(L.sortResults(L.lookup(doc.lines, slugs)), info);
  }

  function close() {
    document.removeEventListener("keydown", onKey, true);
    document.removeEventListener("mousemove", onMove);
    document.removeEventListener("mouseup", onUp);
    host.remove();
  }

  function onKey(e) {
    if (e.key === "Escape") close();
  }

  box.querySelector(".x").addEventListener("click", close);
  box.querySelector(".go").addEventListener("click", run);
  ta.addEventListener("keydown", function (e) {
    if ((e.metaKey || e.ctrlKey) && e.key === "Enter") run();
  });
  document.addEventListener("keydown", onKey, true);

  // 面板可以拖，免得擋到正在編輯的那一段。
  var hd = box.querySelector(".hd");
  var drag = null;
  hd.addEventListener("mousedown", function (e) {
    if (e.target.className === "x") return;
    var rect = host.getBoundingClientRect();
    drag = { dx: e.clientX - rect.left, dy: e.clientY - rect.top };
    e.preventDefault();
  });
  function onMove(e) {
    if (!drag) return;
    host.style.left = e.clientX - drag.dx + "px";
    host.style.top = e.clientY - drag.dy + "px";
    host.style.right = "auto";
  }
  function onUp() {
    drag = null;
  }
  document.addEventListener("mousemove", onMove);
  document.addEventListener("mouseup", onUp);
  host.__collabMarkerCleanup = close;

  ta.focus();
})();
