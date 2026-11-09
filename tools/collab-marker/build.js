// node tools/collab-marker/build.js
//
// 把 lookup.js + panel.js 串成一行 `javascript:` 書籤，輸出兩個檔：
//   bookmarklet.txt  貼到書籤網址欄的純文字（給同事就是傳這個內容）
//   install.html     拖進書籤列用的安裝頁，附複製按鈕
//
// 刻意不用任何打包工具或壓縮器 —— 這支程式要能在只有 node 的機器上跑，跟 repo 其他
// 部分一樣不需要 npm install。只做兩件保守的事：拿掉整行註解、去掉行首縮排。
const fs = require("fs");
const path = require("path");

const dir = __dirname;

function stripComments(src) {
  return src
    .split(/\r?\n/)
    .filter((line) => {
      const t = line.trim();
      // 只拿掉「整行都是註解」的行。行尾註解、字串裡的 // 都不動，免得切壞程式。
      return t !== "" && !t.startsWith("//") && !t.startsWith("/*") && !t.startsWith("*");
    })
    .map((line) => line.trim())
    .join("\n");
}

const lookup = stripComments(fs.readFileSync(path.join(dir, "lookup.js"), "utf8"));
const panel = stripComments(fs.readFileSync(path.join(dir, "panel.js"), "utf8"));

// 包一層 `var module = {exports:{}}`，lookup.js 就會把自己掛到這個區域變數上，
// 而不是掛到 window —— 書籤不該在別人的頁面留下全域變數。
const bundle = `(function(){var module={exports:{}};\n${lookup}\n${panel}\n})();`;

const bookmarklet = "javascript:" + encodeURIComponent(bundle);

fs.writeFileSync(path.join(dir, "bookmarklet.txt"), bookmarklet + "\n", "utf8");

const html = `<!doctype html>
<html lang="zh-Hant">
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>安裝「查上網標記」書籤</title>
<style>
  :root{color-scheme:light dark}
  body{font-family:system-ui,-apple-system,"Microsoft JhengHei",sans-serif;line-height:1.7;
       max-width:640px;margin:40px auto;padding:0 20px}
  h1{font-size:20px}
  ol{padding-left:22px}
  .drag{display:inline-block;margin:14px 0;padding:10px 18px;background:#2563eb;color:#fff;
        border-radius:8px;text-decoration:none;font-weight:600;cursor:grab}
  .note{font-size:13px;color:#666}
  button{font-size:14px;padding:6px 14px;border-radius:6px;border:1px solid #bbb;cursor:pointer}
  code{background:rgba(127,127,127,.15);padding:1px 5px;border-radius:4px}
</style>
<h1>查上網標記</h1>
<p>在協作工具那一頁點一下，貼上 slug，就會告訴你文件裡那一則前面有沒有 <code>(勿上網)</code>。只讀不寫，不會改到文件。</p>
<ol>
  <li>先讓書籤列顯示出來（Chrome：<code>⌘⇧B</code> / <code>Ctrl+Shift+B</code>）</li>
  <li>把下面這顆按鈕<strong>拖</strong>到書籤列</li>
  <li>開著協作文件那一頁，點它</li>
</ol>
<p><a class="drag" href="${bookmarklet.replace(/"/g, "&quot;")}">查上網標記</a></p>
<p class="note">拖不動的話：<button id="copy">複製程式碼</button> 然後在書籤列按右鍵 →「新增頁面」，名稱隨意，網址貼上剛複製的內容。</p>
<script>
document.getElementById("copy").addEventListener("click", async function () {
  var a = document.querySelector("a.drag");
  try {
    await navigator.clipboard.writeText(a.href);
    this.textContent = "已複製";
  } catch (e) {
    // 剪貼簿被擋（例如用 file:// 開啟）時，退回選取讓使用者自己按 ⌘C。
    var r = document.createRange();
    r.selectNode(a);
    getSelection().removeAllRanges();
    getSelection().addRange(r);
    this.textContent = "請按 ⌘C / Ctrl+C";
  }
});
</script>
</html>
`;

fs.writeFileSync(path.join(dir, "install.html"), html, "utf8");

console.log(`bookmarklet.txt  ${bookmarklet.length} 字元`);
console.log("install.html     已更新");
