// node --test tools/collab-marker/tests/lookup.test.js
const test = require("node:test");
const assert = require("node:assert");
const { lookup, parseSlugInput, fold, lineMatchesSlug } = require("../lookup.js").CollabMarkerLookup;

// 一份縮小版的協作文件，欄位排列跟真的一樣（slug 樣式 時間 組），內容全部虛構。
const DOC = [
  "==========1109==========",
  "(勿上網)合成蛋價續漲 SOT 18:00:00 生活",
  "合成長照補助上路 SOT 18:02:30 生活",
  "(版權問題)合成國際快訊 SOT 18:05:00",
  "(19)合成颱風動態 LIVE 18:07:00",
  "（勿上）合成全形括號稿 SOT 18:09:00",
  "(勿)合成單字稿 SOT 18:11:00",
  "合成蛋價 SOT 18:13:00",
  "==========1108==========",
  "(勿上網)合成蛋價續漲 SOT 18:00:00 生活",
  "合成重播無標記 SOT 18:20:00",
].join("\n").split("\n");

function one(slug) {
  return lookup(DOC, [slug])[0];
}

test("勿上網 的三種寫法都認得，全形括號也吃", () => {
  assert.equal(one("合成全形括號稿").groups[0].status, "no-upload");
  assert.equal(one("合成單字稿").groups[0].status, "no-upload");
  assert.equal(one("合成蛋價續漲").groups[0].status, "no-upload");
});

test("沒有標記的稿報無標記", () => {
  const r = one("合成長照補助上路");
  assert.equal(r.found, true);
  assert.equal(r.groups[0].status, "none");
});

test("完全命中：前綴不算命中", () => {
  // 「合成蛋價」在文件裡自己有一行，不能因為「合成蛋價續漲」存在就混在一起。
  const r = one("合成蛋價");
  assert.equal(r.groups.length, 1);
  assert.equal(r.groups[0].status, "none");
  assert.equal(r.groups[0].count, 1);
});

test("文件裡沒有就是找不到，不會硬湊一個最接近的", () => {
  const r = one("合成風災重建");
  assert.equal(r.found, false);
  assert.deepEqual(r.groups, []);
});

test("不管日期：同一則跨天出現就累計次數", () => {
  const r = one("合成蛋價續漲");
  assert.equal(r.groups.length, 1, "兩天都是同一種標記，應該併成一組");
  assert.equal(r.groups[0].count, 2);
});

test("(19)、(抓新) 這類重抓標記不算上網標記", () => {
  assert.equal(one("合成颱風動態").groups[0].status, "none");
});

test("其他標記單獨列出來，不會被誤報成無標記", () => {
  const r = one("合成國際快訊");
  assert.equal(r.groups[0].status, "other");
  assert.deepEqual(r.groups[0].markers, ["版權問題"]);
});

test("貼進來的 slug 自己帶標記也能查", () => {
  assert.equal(one("(勿上網)合成蛋價續漲").groups[0].status, "no-upload");
  assert.equal(one("(19)合成颱風動態").groups[0].status, "none");
});

test("大小寫與全形英數折疊後比對", () => {
  const doc = ["合成ＳＯＴ測試ＡＢ SOT 18:00:00"];
  assert.equal(lookup(doc, ["合成SOT測試ab"])[0].found, true);
});

test("日期分隔線與空行不參與比對", () => {
  assert.equal(lookup(DOC, ["==========1109=========="])[0].found, false);
  assert.equal(lookup(DOC, [""])[0].found, false);
});

test("一次查多筆，順序跟輸入一致", () => {
  const r = lookup(DOC, ["合成長照補助上路", "合成蛋價續漲", "合成風災重建"]);
  assert.deepEqual(r.map((x) => (x.found ? x.groups[0].status : "missing")), ["none", "no-upload", "missing"]);
});

test("輸入解析：空白行與前後空白都清掉", () => {
  assert.deepEqual(parseSlugInput("  甲  \n\n乙\n   \n丙"), ["甲", "乙", "丙"]);
});

test("fold 跟 Rust 側同規則：全形轉半形、去空白、轉小寫", () => {
  assert.equal(fold("　ＳＯＴ　"), "sot");
});

test("命中要求 slug 後面緊接空白或行尾", () => {
  assert.equal(lineMatchesSlug("甲乙 sot 18:00", "甲乙"), true);
  assert.equal(lineMatchesSlug("甲乙丙 sot", "甲乙"), false);
  assert.equal(lineMatchesSlug("甲乙", "甲乙"), true);
});

const { sortResults } = require("../lookup.js").CollabMarkerLookup;

test("排序：勿上網 → 其他標記 → 找不到 → 無標記", () => {
  const input = ["合成長照補助上路", "合成風災重建", "合成國際快訊", "合成蛋價續漲"];
  const sorted = sortResults(lookup(DOC, input));
  assert.deepEqual(sorted.map((r) => r.slug), [
    "合成蛋價續漲",       // 勿上網
    "合成國際快訊",       // 其他標記
    "合成風災重建",       // 找不到
    "合成長照補助上路",   // 無標記
  ]);
});

test("排序：同一級之內保持輸入順序", () => {
  const sorted = sortResults(lookup(DOC, ["合成蛋價", "合成長照補助上路", "合成重播無標記"]));
  assert.deepEqual(sorted.map((r) => r.slug), ["合成蛋價", "合成長照補助上路", "合成重播無標記"]);
});

test("排序：跨天標記不一致時照最嚴重的那一種排", () => {
  // 同一個 slug 一天有 (勿上網)、另一天沒有 —— 必須當成勿上網排在最前面，
  // 不然「有一天忘了標」這種最該看到的情況反而沉到底下。
  const doc = ["(勿上網)合成甲 SOT 18:00:00", "合成甲 SOT 19:00:00", "合成乙 SOT 20:00:00"];
  const sorted = sortResults(lookup(doc, ["合成乙", "合成甲"]));
  assert.deepEqual(sorted.map((r) => r.slug), ["合成甲", "合成乙"]);
  assert.equal(sorted[0].groups.length, 2, "兩種標記要各自列出來");
});

const { stripLeadingNoise } = require("../lookup.js").CollabMarkerLookup;

// 回歸：實務上的標記不一定寫在括號裡，會直接寫成 `UP 蛋價續漲 SOT 18:00:00`。
// 舊版只認行首的 slug，這種行全部報成「文件裡找不到」。
const DOC2 = [
  "UP 合成蛋價續漲 SOT 18:00:00 生活",
  "勿上網 合成長照補助上路 SOT 18:02:30",
  "UP (勿上網)合成國際快訊 SOT 18:05:00",
  "合成乾淨稿 SOT 18:07:00",
  "每日合成蛋價 SOT 18:09:00",
];

test("裸標記在前面也找得到，不再報找不到", () => {
  const r = lookup(DOC2, ["合成蛋價續漲"])[0];
  assert.equal(r.found, true);
  assert.equal(r.groups[0].status, "none");
  assert.deepEqual(r.groups[0].prefix, [], "UP 是上傳者簽名，不該印出來");
});

test("上傳者簽名不影響判斷，也不拆散分組", () => {
  // 同一則稿兩天由不同人上傳（UP / kk），標記狀態一樣就該併成 ×2。
  const doc = ["UP 合成甲 SOT 18:00:00", "kk 合成甲 SOT 19:00:00"];
  const r = lookup(doc, ["合成甲"])[0];
  assert.equal(r.groups.length, 1);
  assert.equal(r.groups[0].count, 2);
  assert.equal(r.groups[0].status, "none");
});

test("不認得的中文裸字仍然印出來，不會被當成簽名吃掉", () => {
  const r = lookup(["暫停 合成乙 SOT 18:00:00"], ["合成乙"])[0];
  assert.deepEqual(r.groups[0].prefix, ["暫停"]);
});

test("簽名跟勿上網並存時照樣判勿上網", () => {
  const r = lookup(["kk 勿上網 合成丙 SOT 18:00:00"], ["合成丙"])[0];
  assert.equal(r.groups[0].status, "no-upload");
  assert.deepEqual(r.groups[0].prefix, [], "勿上網 由標籤代表，簽名忽略，兩個都不必再印");
});

test("沒有括號的「勿上網」一樣算勿上網", () => {
  assert.equal(lookup(DOC2, ["合成長照補助上路"])[0].groups[0].status, "no-upload");
});

test("裸標記加括號標記並存時，兩邊都看", () => {
  const r = lookup(DOC2, ["合成國際快訊"])[0];
  assert.equal(r.groups[0].status, "no-upload");
});

test("完全命中沒有被放寬：slug 仍然要獨立成欄位", () => {
  // 「合成蛋價」不能命中「UP 合成蛋價續漲」，也不能命中「每日合成蛋價」的後半段。
  const r = lookup(DOC2, ["合成蛋價"])[0];
  assert.equal(r.found, false);
});

test("slug 出現在欄位中間但邊界對不上就不算", () => {
  assert.equal(lookup(["甲 乙丙丁 SOT"], ["乙丙"])[0].found, false);
  assert.equal(lookup(["甲 乙丙 SOT"], ["乙丙"])[0].found, true);
});

test("貼進來的 slug 帶裸標記也查得到", () => {
  assert.equal(lookup(DOC2, ["UP 合成蛋價續漲"])[0].found, true);
  assert.equal(lookup(DOC2, ["勿上網 合成長照補助上路"])[0].found, true);
  // 連括號標記的那則也一樣
  assert.equal(lookup(DOC, ["(勿上網) 合成蛋價續漲"])[0].found, true);
});

test("中文開頭的詞不會被當成標記剝掉", () => {
  // 「每日 合成蛋價」這種 slug 本身就以中文短詞開頭，剝掉會查錯對象。
  assert.equal(stripLeadingNoise("每日 合成蛋價"), "每日 合成蛋價");
  assert.equal(stripLeadingNoise("up 合成蛋價"), "合成蛋價");
  assert.equal(stripLeadingNoise("勿上網 合成蛋價"), "合成蛋價");
});

// ---- 空白差異與錯字建議 ----

const DOC3 = [
  "UP 合成蛋價續漲 2100 SOT 18:00:00 生活",
  "(勿上網)合成長照補助 2100 SOT 18:02:30",
  "合成乾淨稿 1900 SOT 18:05:00",
];

test("slug 含空白：你打的跟文件寫的空白不一致也找得到", () => {
  assert.equal(lookup(DOC3, ["合成蛋價續漲 2100"])[0].found, true, "一樣有空白");
  assert.equal(lookup(DOC3, ["合成蛋價續漲2100"])[0].found, true, "你沒打空白");
  assert.equal(lookup(DOC3, ["合成蛋價續漲  2100"])[0].found, true, "多打了空白");
  assert.equal(lookup(["合成甲2100 SOT 18:00"], ["合成甲 2100"])[0].found, true, "文件沒空白、你打了");
});

test("忽略空白之後，欄位中間仍然不算命中", () => {
  // 「合成蛋價」結束在「續」字中間，不是欄位邊界，所以不算命中 —— 抽掉空白之後
  // 這條底線還在。
  assert.equal(lookup(DOC3, ["合成蛋價"])[0].found, false);
  assert.equal(lookup(DOC3, ["價續漲 2100"])[0].found, false, "從欄位中間開頭的也不算");
});

test("只打到時段前面（在空白處斷開）算命中，而且指到同一則", () => {
  // slug 含空白時，「哪裡是 slug 結尾」文件本身沒有標示，所以斷在空白處的寫法
  // 一律視為命中。少打時段不會害你查不到，而且回報的是正確那一行的標記。
  const r = lookup(DOC3, ["合成蛋價續漲"])[0];
  assert.equal(r.found, true);
  assert.equal(r.groups[0].status, "none");
  const r2 = lookup(DOC3, ["合成長照補助"])[0];
  assert.equal(r2.groups[0].status, "no-upload");
});

test("含空白的 slug 一樣讀得到前面的標記", () => {
  const r = lookup(DOC3, ["合成長照補助 2100"])[0];
  assert.equal(r.groups[0].status, "no-upload");
});

test("錯字：差一個字時給建議，但不算命中", () => {
  const r = lookup(DOC3, ["合成蛋假續漲 2100"])[0];
  assert.equal(r.found, false, "錯字不能算命中");
  assert.equal(r.suggestions.length, 1);
  assert.equal(r.suggestions[0].text, "合成蛋價續漲 2100");
  assert.equal(r.suggestions[0].distance, 1);
  assert.equal(r.suggestions[0].status, "none", "建議也要帶著它自己的標記狀態");
});

test("錯字建議會把標記一起帶出來", () => {
  const r = lookup(DOC3, ["合成長造補助 2100"])[0];
  assert.equal(r.suggestions[0].status, "no-upload");
});

test("找得到就不給建議，不要拿疑似的東西干擾", () => {
  const r = lookup(DOC3, ["合成乾淨稿 1900"])[0];
  assert.equal(r.found, true);
  assert.deepEqual(r.suggestions, []);
});

test("差太多就不給建議", () => {
  const r = lookup(DOC3, ["完全不相干的稿 2100"])[0];
  assert.deepEqual(r.suggestions, []);
});

test("短字串不給錯字建議", () => {
  // 「甲乙」跟「甲丙」差一個字就是 50%，這種提示只會誤導。
  assert.deepEqual(lookup(["甲乙 SOT 18:00"], ["甲丙"])[0].suggestions, []);
});

test("長 slug 容許差兩個字", () => {
  const doc = ["合成十二個字的長標題 2100 SOT 18:00"];
  const r = lookup(doc, ["合成十二個子的長鏢題 2100"])[0];
  assert.equal(r.found, false);
  assert.equal(r.suggestions.length, 1);
  assert.equal(r.suggestions[0].distance, 2);
});

// ---- 勿上網一定要被抓出來 ----

test("不含「勿」的禁上網寫法也要擋：不上網、不上YT、不po網、不要上網", () => {
  for (const marker of ["不上網", "不上YT", "不po網", "不要上網", "勿上網", "勿", "網勿"]) {
    const doc = [`(${marker})合成甲 SOT 18:00:00`];
    assert.equal(lookup(doc, ["合成甲"])[0].groups[0].status, "no-upload", `漏掉 ${marker}`);
  }
});

test("沒有括號的禁上網寫法一樣擋", () => {
  assert.equal(lookup(["不上網 合成甲 SOT 18:00"], ["合成甲"])[0].groups[0].status, "no-upload");
});

test("標記寫在 slug 後面也抓得到，並標示位置", () => {
  const r = lookup(["合成甲 (勿上網) SOT 18:00:00"], ["合成甲"])[0];
  assert.equal(r.groups[0].status, "no-upload");
  assert.equal(r.groups[0].behind, true, "要讓介面能講「標記寫在後面」");
});

test("標記寫在前面時不會被標成寫在後面", () => {
  const r = lookup(["(勿上網)合成甲 SOT 18:00:00"], ["合成甲"])[0];
  assert.equal(r.groups[0].behind, false);
});

test("其他標記不會因為放寬而誤判成勿上網", () => {
  const r = lookup(["(版權問題)合成甲 SOT 18:00:00"], ["合成甲"])[0];
  assert.equal(r.groups[0].status, "other");
  const r2 = lookup(["(可上網)合成甲 SOT 18:00:00"], ["合成甲"])[0];
  assert.equal(r2.groups[0].status, "other", "「可上網」含「上網」但不含「不」「勿」，不能擋");
});

test("命中無標記、但旁邊有一則只差一字是勿上網時，一定要提醒", () => {
  // 打錯字最危險的情況：你拿到 ✅，而真正該擋的那則就在隔壁。
  const doc = ["合成蛋價續漲 2100 SOT 18:00:00", "(勿上網)合成蛋假續漲 2100 SOT 18:02:00"];
  const r = lookup(doc, ["合成蛋價續漲 2100"])[0];
  assert.equal(r.found, true);
  assert.equal(r.groups[0].status, "none");
  assert.equal(r.warnings.length, 1);
  assert.match(r.warnings[0].text, /合成蛋假續漲/);
});

test("命中就是勿上網時，不再追加相近提醒", () => {
  const doc = ["(勿上網)合成蛋價續漲 2100 SOT 18:00:00", "(勿上網)合成蛋假續漲 2100 SOT 18:02:00"];
  const r = lookup(doc, ["合成蛋價續漲 2100"])[0];
  assert.deepEqual(r.suggestions, []);
  assert.deepEqual(r.warnings, []);
});

test("命中無標記、相近的也無標記時不囉嗦", () => {
  const doc = ["合成蛋價續漲 2100 SOT 18:00:00", "合成蛋假續漲 2100 SOT 18:02:00"];
  const r = lookup(doc, ["合成蛋價續漲 2100"])[0];
  assert.deepEqual(r.suggestions, []);
  assert.deepEqual(r.warnings, []);
});

// ---- 部分命中：同事只給一段 slug ----

const DOC4 = [
  "UP 女飛官211800 SOT 18:00:00",
  "(勿上網)男主播221900 SOT 18:02:00",
  "合成蛋價續漲 SOT 18:04:00",
  "每日合成蛋價 SOT 18:06:00",
];

test("只給一段 slug（開頭少字、中間多空白）算部分命中", () => {
  // 完整是「女飛官211800」，同事給「飛官21 1800」。
  const r = lookup(DOC4, ["飛官21 1800"])[0];
  assert.equal(r.found, true);
  assert.equal(r.groups[0].partial, "女飛官211800", "要把文件裡的完整寫法帶出來核對");
  assert.equal(r.groups[0].status, "none");
});

test("部分命中照樣看得到勿上網", () => {
  const r = lookup(DOC4, ["主播22 1900"])[0];
  assert.equal(r.groups[0].status, "no-upload");
  assert.equal(r.groups[0].partial, "男主播221900");
});

test("蓋得不夠多就不算部分命中，避免安錯稿", () => {
  // 「合成蛋價」只佔「合成蛋價續漲」「每日合成蛋價」的 67%，兩邊都不能算命中，
  // 否則某一則的標記會被安到另一則身上。
  const r = lookup(DOC4, ["合成蛋價"])[0];
  assert.equal(r.found, false);
});

test("有完全命中時不會再去找部分命中", () => {
  const r = lookup(DOC4, ["合成蛋價續漲"])[0];
  assert.equal(r.found, true);
  assert.equal(r.groups.length, 1);
  assert.equal(r.groups[0].partial, undefined);
});

test("部分命中又沒標記的，排在找不到後面、完全命中的無標記前面", () => {
  const { sortResults } = require("../lookup.js").CollabMarkerLookup;
  const sorted = sortResults(lookup(DOC4, ["合成蛋價續漲", "合成蛋價", "飛官21 1800"]));
  assert.deepEqual(sorted.map((r) => r.slug), [
    "合成蛋價",       // 找不到
    "飛官21 1800",    // 部分命中、無標記 —— 要核對一眼
    "合成蛋價續漲",   // 完全命中、無標記 —— 可以直接放過
  ]);
});

// ---- 最後一道網：文件是對的，錯的是輸入端 ----

test("輸入歪掉到完全查不到，只要文件裡有相關的勿上網就一定跳出來", () => {
  // 同事報「飛官21 1800」，文件裡其實是「女飛官211900」而且標了勿上網。
  // 時段記錯 + 開頭少字，完全命中、部分命中、錯字建議全都搆不到。
  const doc = ["(勿上網)女飛官211900 SOT 18:00:00"];
  const r = lookup(doc, ["飛官21 1800"])[0];
  assert.equal(r.found, false);
  assert.equal(r.warnings.length, 1, "這種才是最危險的，一定要講");
  assert.match(r.warnings[0].text, /女飛官211900/);
});

test("部分命中到乾淨的那則、但另一則相關的是勿上網時也要警示", () => {
  const doc = ["女飛官211800 SOT 18:00:00", "(勿上網)女飛官211900 SOT 18:02:00"];
  const r = lookup(doc, ["飛官21 1800"])[0];
  assert.equal(r.groups[0].status, "none");
  assert.equal(r.warnings.length, 1);
});

test("不相干的稿不會被硬扯進來警示", () => {
  const doc = ["(勿上網)男主播221900 SOT 18:00:00", "合成蛋價續漲 SOT 18:02:00"];
  assert.deepEqual(lookup(doc, ["合成蛋價續漲"])[0].warnings, []);
});

test("slug 被拆成兩欄也比得到（女飛官21 1800）", () => {
  const doc = ["(勿上網)女飛官21 1800 SOT 18:00:00"];
  const r = lookup(doc, ["飛官211900"])[0];
  assert.equal(r.warnings.length, 1);
  assert.match(r.warnings[0].text, /女飛官21 1800/);
});

test("警示不會跟錯字建議講同一則兩次", () => {
  const doc = ["(勿上網)合成蛋假續漲 2100 SOT 18:00:00"];
  const r = lookup(doc, ["合成蛋價續漲 2100"])[0];
  assert.equal(r.found, false);
  const texts = r.suggestions.map((x) => x.text).concat(r.warnings.map((x) => x.text));
  assert.equal(new Set(texts).size, texts.length, "同一則不該出現兩次");
});

// ---- 邊界與誤判 ----

test("內文句子含「不」又含「上」不會被當成勿上網標記", () => {
  // 「民眾不滿上街抗議」整句是一個詞，含不也含上，但它是內文不是標記。
  const doc = ["合成甲 SOT 18:00:00 民眾不滿上街抗議"];
  assert.equal(lookup(doc, ["合成甲"])[0].groups[0].status, "none");
});

test("括號裡的合寫標記仍然認得，不受長度限制", () => {
  const doc = ["(版權問題 勿上網)合成甲 SOT 18:00:00"];
  assert.equal(lookup(doc, ["合成甲"])[0].groups[0].status, "no-upload");
});

test("空文件、空輸入都不會爆", () => {
  assert.deepEqual(lookup([], ["合成甲"])[0].found, false);
  assert.deepEqual(lookup(["合成甲 SOT"], [])  , []);
  assert.equal(lookup(["合成甲 SOT"], ["   "])[0].found, false);
});

test("超長一行不會讓比對失控", () => {
  const long = "合成甲 " + "文".repeat(5000);
  const r = lookup([long], ["合成甲"])[0];
  assert.equal(r.found, true);
});

// ---- 紅字一律排最前面 ----

test("查不到但掃出相關勿上網的，排在最前面而不是夾在中間", () => {
  const doc = [
    "合成乾淨稿甲 SOT 18:00:00",
    "(勿上網)女飛官211900 SOT 18:02:00",
    "合成乾淨稿乙 SOT 18:04:00",
  ];
  const { sortResults } = require("../lookup.js").CollabMarkerLookup;
  const sorted = sortResults(lookup(doc, ["合成乾淨稿甲", "飛官21 1800", "合成乾淨稿乙"]));
  assert.equal(sorted[0].slug, "飛官21 1800", "有紅字的要在最前面");
  assert.equal(sorted[0].found, false);
  assert.equal(sorted[0].warnings.length, 1);
});

test("錯字建議裡是勿上網的也排最前面", () => {
  const doc = ["(勿上網)合成蛋價續漲 2100 SOT 18:00:00", "合成乾淨稿 SOT 18:02:00"];
  const { sortResults } = require("../lookup.js").CollabMarkerLookup;
  const sorted = sortResults(lookup(doc, ["合成乾淨稿", "合成蛋假續漲 2100"]));
  assert.equal(sorted[0].slug, "合成蛋假續漲 2100");
});

test("沒有紅字的找不到，維持原本的位置（在無標記之前）", () => {
  const doc = ["合成乾淨稿 SOT 18:00:00"];
  const { sortResults } = require("../lookup.js").CollabMarkerLookup;
  const sorted = sortResults(lookup(doc, ["合成乾淨稿", "完全不相干的稿"]));
  assert.deepEqual(sorted.map((r) => r.slug), ["完全不相干的稿", "合成乾淨稿"]);
});

test("太短的 slug 不進相關警示，避免紅字滿天飛", () => {
  // 「合成乙」對「合成甲」差一個字就是 0.67 相似度，這種程度的「像」沒有意義。
  const doc = ["(勿上網)合成甲 SOT 18:00:00", "合成乙 SOT 19:00:00"];
  assert.deepEqual(lookup(doc, ["合成乙"])[0].warnings, []);
  // 長度夠的照樣要有紅字 —— 這一筆是錯字建議接住的（同長度差一字），
  // 警示那條路因為去重所以是空的，兩條路加起來才是「有沒有紅字」。
  const { hasRedFlag } = require("../lookup.js").CollabMarkerLookup;
  const doc2 = ["(勿上網)女飛官211900 SOT 18:00:00"];
  const r = lookup(doc2, ["女飛官211800"])[0];
  assert.equal(hasRedFlag(r), true);
  assert.equal(r.suggestions[0].status, "no-upload");
});
