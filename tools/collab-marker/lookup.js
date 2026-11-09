// 查標記的純邏輯：沒有 DOM、沒有瀏覽器 API，所以可以用 node --test 直接測。
// panel.js 負責介面，build.js 把這兩支串成一行 javascript: 書籤。
//
// 折疊規則刻意跟 core/src/clean.rs 的 `fold()` 一致：全形 ASCII 轉半形、全形空白
// 轉半形空白、去頭尾空白、轉小寫。協作文件是人手打的，全形括號、全形英數、大小寫
// 混用都會出現，不折疊就會漏判。
(function (root) {
  "use strict";

  function fold(s) {
    return String(s == null ? "" : s)
      .replace(/[！-～]/g, function (c) {
        return String.fromCharCode(c.charCodeAt(0) - 0xfee0);
      })
      .replace(/　/g, " ")
      .trim()
      .toLowerCase();
  }

  // 一行開頭連續的括號標記全部剝掉，回傳標記內容與剩下的部分。
  // 折疊之後全形括號已經變成半形，所以這裡只需要認 ( )。
  function splitLeadingMarkers(folded) {
    var rest = folded;
    var markers = [];
    for (;;) {
      var m = /^\(([^()]*)\)\s*/.exec(rest);
      if (!m) break;
      markers.push(m[1].trim());
      rest = rest.slice(m[0].length);
    }
    return { markers: markers, rest: rest };
  }

  // 上傳者簽名：每個人上傳完會在 slug 前面留自己的記號（UP、kk…），人人不同、
  // 跟上網與否無關。短的純英數字一律當成簽名忽略掉 —— 既不影響判斷，也不印出來，
  // 更不能讓它參與分組，否則同一則稿兩天由不同人上傳會被拆成兩筆而不是 ×2。
  function isUploaderTag(token) {
    return /^[a-z0-9]{1,4}$/.test(token);
  }

  // (抓新)、(抓)、(19)、(x/19) 這類是「要重抓」的提示，不是上網標記 —— 把它們當成
  // 透明的，不然 slug 前面掛了 (19) 的稿會被報成「其他標記」。規則對齊 App v0.3.7。
  function isRefreshMarker(marker) {
    return marker.indexOf("抓") >= 0 || /^[a-z]*\/?\d{1,2}$/.test(marker);
  }

  // 「不要上網」的各種寫法。對齊 App 的 no_upload_terms（core/src/clean.rs）：
  // 勿上網、勿上YT、勿YT、勿網、網勿 都含「勿」；但還有一整族不含「勿」的寫法
  // —— 不上網、不上YT、不po網、不要上網 —— 只認「勿」會整組漏掉。
  //
  // 這個判斷寧可寬一點：漏報一則該擋的稿，比多報一則要你自己確認的稿嚴重得多。
  function isNoUploadTerm(text) {
    if (text.indexOf("勿") >= 0) return true;
    if (text.indexOf("不") < 0) return false;
    return text.indexOf("上") >= 0 || text.indexOf("po") >= 0 || text.indexOf("yt") >= 0;
  }

  // 標記是短詞。括號裡的不限長度（`(版權問題 勿上網)` 這種合寫的要認得），但沒有括號
  // 的裸字限 6 個字以內 —— 不然一句內文「民眾不滿上街抗議」含「不」又含「上」，
  // 整行會被誤判成勿上網。
  var MAX_BARE_MARKER_LEN = 6;

  function hasNoUploadMarker(markers, bare) {
    var i;
    for (i = 0; i < markers.length; i++) {
      if (isNoUploadTerm(markers[i])) return true;
    }
    for (i = 0; bare && i < bare.length; i++) {
      if (bare[i].length <= MAX_BARE_MARKER_LEN && isNoUploadTerm(bare[i])) return true;
    }
    return false;
  }

  function otherMarkers(markers) {
    var out = [];
    for (var i = 0; i < markers.length; i++) {
      if (markers[i] !== "" && !isRefreshMarker(markers[i])) out.push(markers[i]);
    }
    return out;
  }

  // 拆解 slug 前面的那一段：括號標記（`(勿上網)`）與裸標記（`UP`）可能同時出現，
  // 順序也不固定，所以括號不限定在行首。
  function parsePrefix(prefix) {
    var markers = [];
    var plain = "";
    var re = /\(([^()]*)\)/g;
    var last = 0;
    var m;
    while ((m = re.exec(prefix)) !== null) {
      markers.push(m[1].trim());
      plain += " " + prefix.slice(last, m.index);
      last = re.lastIndex;
    }
    plain += " " + prefix.slice(last);
    var bare = plain.split(/\s+/).filter(function (t) { return t !== ""; });
    return { markers: markers, bare: bare };
  }

  // 把一行的空白全部抽掉，同時記住每個字原本在第幾個位置。
  //
  // 為什麼要這樣：slug 本身就含空白（`某某某 2100`），而且同一則稿在文件裡跟在你手上
  // 空白可能不一致 —— 你打 `某某某2100`、文件寫 `某某某 2100`，或反過來。拿抽掉空白的
  // 字串比對才找得到。但邊界判斷（完全命中）還是要回到原始字串上看，不然
  // 「蛋價」會命中「蛋價續漲」。index map 就是為了這件事。
  function compact(line) {
    var text = "";
    var map = [];
    for (var i = 0; i < line.length; i++) {
      var c = line.charAt(i);
      if (/\s/.test(c)) continue;
      text += c;
      map.push(i);
    }
    return { text: text, map: map };
  }

  // 命中的左右邊界檢查，一律回到原始（還有空白的）那一行上判斷。
  // 左邊界：行首、空白，或緊鄰括號標記的 `)`（`UP (勿上網)蛋價續漲` 中間沒有空白）。
  // 右邊界：行尾或空白。
  function isFieldBounded(line, startOrig, endOrig) {
    var prev = startOrig === 0 ? "" : line.charAt(startOrig - 1);
    var next = endOrig + 1 >= line.length ? "" : line.charAt(endOrig + 1);
    var startsField = startOrig === 0 || /\s/.test(prev) || prev === ")";
    var endsField = next === "" || /\s/.test(next);
    return startsField && endsField;
  }

  /**
   * 在一行裡找 slug，忽略雙方的空白差異，但維持完全命中。
   * @returns {number} 命中處在「原始行」裡的起點，找不到回 -1
   */
  function findSlugRange(rest, slug) {
    var q = compact(slug).text;
    if (q === "") return null;
    var c = compact(rest);
    var from = 0;
    for (;;) {
      var idx = c.text.indexOf(q, from);
      if (idx < 0) return null;
      var startOrig = c.map[idx];
      var endOrig = c.map[idx + q.length - 1];
      if (isFieldBounded(rest, startOrig, endOrig)) return { start: startOrig, end: endOrig };
      from = idx + 1;
    }
  }

  function findSlugInLine(rest, slug) {
    var r = findSlugRange(rest, slug);
    return r ? r.start : -1;
  }

  // 把一個命中範圍向左右撐到欄位邊界，得到它所在的完整欄位。
  // 邊界跟 isFieldBounded 同一套：空白，以及括號標記的 `)` / `(` —— 標記跟 slug 之間
  // 常常沒有空白（`(勿上網)男主播221900`），不把 `)` 當邊界的話標記會被算進欄位長度，
  // 覆蓋率被稀釋掉，該認列的部分命中就漏了。
  function expandToFields(rest, start, end) {
    var s = start;
    var e = end;
    while (s > 0 && !/\s/.test(rest.charAt(s - 1)) && rest.charAt(s - 1) !== ")") s -= 1;
    while (e + 1 < rest.length && !/\s/.test(rest.charAt(e + 1)) && rest.charAt(e + 1) !== "(") e += 1;
    return { start: s, end: e };
  }

  // 部分命中：同事報 slug 時常常只給一段（完整是 `女飛官211800`，他給 `飛官21 1800`），
  // 開頭少一兩個字，完全命中就對不上。這種不能當成命中 —— 一旦放寬左邊界，
  // 「蛋價」就會命中「每日蛋價」，有機會把某則乾淨稿的 ✅ 安到另一則身上。
  //
  // 折衷：命中的那一段必須佔整個欄位的八成以上，才認列為「部分命中」，而且介面上
  // 會把它在文件裡的完整寫法印出來讓你核對。
  //
  // 八成是試出來的：`飛官211800 / 女飛官211800` 是 89%（要放過），而
  // `合成蛋價 / 合成蛋價續漲`、`合成蛋價 / 每日合成蛋價` 都是 67%（要擋掉）。
  // 一開始抓六成，上面那兩個危險案例就全漏進來了。
  var PARTIAL_MIN_RATIO = 0.8;

  function findPartialRange(rest, slug) {
    var q = compact(slug).text;
    if (q.length < 3) return null;
    var c = compact(rest);
    var from = 0;
    for (;;) {
      var idx = c.text.indexOf(q, from);
      if (idx < 0) return null;
      var span = expandToFields(rest, c.map[idx], c.map[idx + q.length - 1]);
      var field = compact(rest.slice(span.start, span.end + 1)).text;
      if (field.length > 0 && q.length / field.length >= PARTIAL_MIN_RATIO) return span;
      from = idx + 1;
    }
  }

  function lineMatchesSlug(rest, slug) {
    return findSlugInLine(rest, slug) >= 0;
  }

  // 這一行在 slug 命中位置前後有什麼標記。
  //
  // 前面是標記的正常位置，但不是所有人都寫在前面 —— `蛋價續漲 (勿上網) SOT` 這種
  // 寫在後面的也看得到。後面那一段只拿來判斷「要不要擋」，不拿來判斷其他標記，
  // 因為那一段正常情況下是樣式、時間、組別，不是標記區。
  function classifyAt(rest, start, end) {
    var pre = parsePrefix(rest.slice(0, start));
    var bare = pre.bare.filter(function (t) {
      return !isRefreshMarker(t) && !isUploaderTag(t) && !isNoUploadTerm(t);
    });
    var others = otherMarkers(pre.markers);
    var frontNoUpload = hasNoUploadMarker(pre.markers, pre.bare);

    var tailNoUpload = false;
    if (typeof end === "number") {
      var tail = parsePrefix(rest.slice(end + 1));
      tailNoUpload = hasNoUploadMarker(tail.markers, tail.bare);
    }

    var status = frontNoUpload || tailNoUpload ? "no-upload" : others.length > 0 ? "other" : "none";
    return {
      status: status,
      markers: others,
      prefix: bare,
      // 標記寫在後面時介面要講一聲，不然你會對著「前面沒東西」的那一行懷疑是不是誤判。
      behind: !frontNoUpload && tailNoUpload,
    };
  }

  // 使用者貼進來的 slug 可能連標記一起複製（`UP 蛋價續漲`、`勿上網 蛋價續漲`）。
  // 把開頭那些一看就是標記的詞剝掉：含「勿」的、重抓標記、以及 1～4 個英數字的短詞
  // （UP、kk 這類簽名）。中文詞不剝 —— 中文 slug 本來就可能以短詞開頭，剝了會誤傷。
  function stripLeadingNoise(rest) {
    var out = rest;
    for (;;) {
      var m = /^(\S+)\s+(?=\S)/.exec(out);
      if (!m) break;
      var head = m[1];
      var markerish = head.indexOf("勿") >= 0 || isRefreshMarker(head) || /^[a-z0-9]{1,4}$/.test(head);
      if (!markerish) break;
      out = out.slice(m[0].length);
    }
    return out;
  }

  // 逐字比對兩段等長文字，差幾個字就回幾（超過上限就提早放棄）。
  // 只算「換字」不算增刪 —— 打錯字（同音字、形近字）就是換字，這樣夠用而且很快。
  function substitutions(a, b, limit) {
    var diff = 0;
    for (var i = 0; i < a.length; i++) {
      if (a.charAt(i) !== b.charAt(i)) {
        diff += 1;
        if (diff > limit) return diff;
      }
    }
    return diff;
  }

  // 長一點的 slug 容許差兩個字，短的只容許一個 —— 短字串差兩個字就幾乎是另一則稿了。
  function allowedTypos(len) {
    return len >= 9 ? 2 : 1;
  }

  /**
   * 完全命中失敗時，找出「只差一兩個字」的行，當成提示回報（不是命中）。
   * 打錯字、同音字、形近字都是換掉某幾個字，所以逐字比對就抓得到，不需要注音字典。
   */
  function nearMatches(docLines, wanted, cap) {
    var q = compact(wanted).text;
    if (q.length < 3) return []; // 太短的字串「差一個字」毫無意義
    var limit = allowedTypos(q.length);
    var found = [];
    for (var d = 0; d < docLines.length; d++) {
      var rest = docLines[d].rest;
      var c = compact(rest);
      for (var i = 0; i + q.length <= c.text.length; i++) {
        var startOrig = c.map[i];
        var endOrig = c.map[i + q.length - 1];
        if (!isFieldBounded(rest, startOrig, endOrig)) continue;
        var window = c.text.slice(i, i + q.length);
        var diff = substitutions(q, window, limit);
        if (diff === 0 || diff > limit) continue;
        var info = classifyAt(rest, startOrig, endOrig);
        found.push({
          text: rest.slice(startOrig, endOrig + 1),
          status: info.status,
          markers: info.markers,
          prefix: info.prefix,
          distance: diff,
          line: docLines[d].no,
        });
      }
    }
    // 差越少越前面；同樣的建議只留一筆。
    found.sort(function (a, b) { return a.distance - b.distance || a.line - b.line; });
    var out = [];
    var seen = {};
    for (var k = 0; k < found.length && out.length < (cap || 3); k++) {
      var key = found[k].text + "|" + found[k].status;
      if (seen[key]) continue;
      seen[key] = true;
      out.push(found[k]);
    }
    return out;
  }

  // ---- 勿上網的最後一道網 ----
  //
  // 前提（使用者確認過）：協作文件裡的標記是對的，會出錯的是輸入端 —— 同事口頭報的
  // slug、打錯的字、只給一段。所以這一關反過來做：不管你輸入成什麼樣子，只要文件裡
  // 存在一則「跟你查的很像」而且是勿上網的稿，就一定要講出來。
  //
  // 這裡刻意容易觸發。多報一則，你花三秒看一眼；漏報一則，後果是上了不該上的。

  // 把括號標記換成等長空白：位置不變，後面切欄位時才對得回原文。
  function maskBrackets(rest) {
    return rest.replace(/\([^()]*\)/g, function (m) {
      return new Array(m.length + 1).join(" ");
    });
  }

  // 一行裡可以拿來跟 slug 比對的候選字串：每個欄位，以及相鄰兩欄位合起來
  // （slug 本身含空白時會被拆成兩欄，例如 `女飛官21 1800`）。
  function slugCandidates(rest) {
    var masked = maskBrackets(rest);
    var ranges = [];
    var re = /\S+/g;
    var m;
    while ((m = re.exec(masked)) !== null) {
      ranges.push({ start: m.index, end: m.index + m[0].length - 1, text: m[0] });
    }
    var out = [];
    function usable(t) {
      return (
        t.length >= 3 &&
        !isUploaderTag(t) &&
        !isRefreshMarker(t) &&
        !isNoUploadTerm(t) &&
        !/^\d{1,2}:\d{2}/.test(t)
      );
    }
    for (var i = 0; i < ranges.length; i++) {
      if (usable(ranges[i].text)) out.push(rest.slice(ranges[i].start, ranges[i].end + 1));
      if (i + 1 < ranges.length) {
        var pair = rest.slice(ranges[i].start, ranges[i + 1].end + 1);
        if (usable(compact(pair).text)) out.push(pair);
      }
    }
    return out;
  }

  // 編輯距離（增、刪、改各算一步）。
  //
  // 這裡不能用「最長共同連續子字串」—— 實際案例 `飛官21 1800` 對上文件裡的
  // `女飛官211900`，開頭少一個字、中間還錯一位數字，最長連續只剩「飛官211」五個字，
  // 相似度被低估成 0.55 而漏掉警示。編輯距離算出來是差 2 步，相似度 0.78，正確地
  // 判定為相關。
  function editDistance(a, b) {
    var prev = [];
    var i, j;
    for (j = 0; j <= b.length; j++) prev[j] = j;
    for (i = 1; i <= a.length; i++) {
      var cur = [i];
      for (j = 1; j <= b.length; j++) {
        var cost = a.charAt(i - 1) === b.charAt(j - 1) ? 0 : 1;
        cur[j] = Math.min(cur[j - 1] + 1, prev[j] + 1, prev[j - 1] + cost);
      }
      prev = cur;
    }
    return prev[b.length];
  }

  var RELATED_MIN_RATIO = 0.6;

  function isRelated(queryCompact, candidateCompact) {
    var longer = Math.max(queryCompact.length, candidateCompact.length);
    if (longer === 0) return false;
    return 1 - editDistance(queryCompact, candidateCompact) / longer >= RELATED_MIN_RATIO;
  }

  /**
   * 文件裡所有「跟這個查詢很像」的勿上網稿。
   * 不管查詢本身命中與否都會跑；命中的結果本身就是勿上網時才不需要。
   */
  // 太短的字串別進這一關：三個字的 slug 差一個字就有 0.67 的相似度，等於任兩則短稿
  // 都「很像」，紅字會滿天飛，然後沒人再把紅字當一回事。真實 slug 都在五個字以上。
  var MIN_RELATED_LEN = 5;

  function relatedNoUpload(docLines, wanted, cap) {
    var q = compact(wanted).text;
    if (q.length < MIN_RELATED_LEN) return [];
    var out = [];
    var seen = {};
    for (var d = 0; d < docLines.length; d++) {
      var rest = docLines[d].rest;
      var whole = parsePrefix(rest);
      if (!hasNoUploadMarker(whole.markers, whole.bare)) continue;
      var cands = slugCandidates(rest);
      for (var c = 0; c < cands.length; c++) {
        var cc = compact(cands[c]).text;
        if (cc.length < MIN_RELATED_LEN) continue;
        if (cc === q) continue; // 完全一樣的話，它已經是命中結果了
        if (!isRelated(q, cc)) continue;
        if (seen[cands[c]]) continue;
        seen[cands[c]] = true;
        out.push({ text: cands[c], line: docLines[d].no });
        break; // 同一行報一次就夠
      }
      if (out.length >= (cap || 3)) break;
    }
    return out;
  }

  /**
   * 比對一份文件。
   * @param {string[]} lines 文件的每一行（原始字串）
   * @param {string[]} slugs 要查的 slug（原始字串，可以帶標記，會被剝掉）
   */
  function lookup(lines, slugs) {
    var docLines = [];
    for (var i = 0; i < lines.length; i++) {
      var folded = fold(lines[i]);
      if (folded === "" || isSeparatorLine(folded)) continue;
      docLines.push({ no: i + 1, rest: folded });
    }

    var results = [];
    for (var s = 0; s < slugs.length; s++) {
      var raw = slugs[s];
      var wanted = stripLeadingNoise(splitLeadingMarkers(fold(raw)).rest);
      var groups = [];
      for (var d = 0; d < docLines.length; d++) {
        var range = findSlugRange(docLines[d].rest, wanted);
        if (!range) continue;
        var info = classifyAt(docLines[d].rest, range.start, range.end);
        var key = info.status + "|" + info.markers.join(",") + "|" + info.prefix.join(",") + "|" + info.behind;
        var hit = null;
        for (var g = 0; g < groups.length; g++) {
          if (groups[g].key === key) { hit = groups[g]; break; }
        }
        if (hit) {
          hit.count += 1;
          hit.lines.push(docLines[d].no);
        } else {
          groups.push({
            key: key,
            status: info.status,
            markers: info.markers,
            prefix: info.prefix,
            behind: info.behind,
            count: 1,
            lines: [docLines[d].no],
          });
        }
      }
      // 完全命中掛零時，退一步找部分命中 —— 只給一段 slug 的情況比打錯字常見得多。
      var partial = groups.length === 0;
      if (partial) {
        for (var pd = 0; pd < docLines.length; pd++) {
          var prange = findPartialRange(docLines[pd].rest, wanted);
          if (!prange) continue;
          var pinfo = classifyAt(docLines[pd].rest, prange.start, prange.end);
          var ptext = docLines[pd].rest.slice(prange.start, prange.end + 1);
          var pkey = pinfo.status + "|" + ptext + "|" + pinfo.behind;
          var phit = null;
          for (var pg = 0; pg < groups.length; pg++) {
            if (groups[pg].key === pkey) { phit = groups[pg]; break; }
          }
          if (phit) {
            phit.count += 1;
            phit.lines.push(docLines[pd].no);
          } else {
            groups.push({
              key: pkey,
              status: pinfo.status,
              markers: pinfo.markers,
              prefix: pinfo.prefix,
              behind: pinfo.behind,
              // 部分命中一定要把文件裡的完整寫法帶出來，不然你無從核對它指的是哪一則。
              partial: ptext,
              count: 1,
              lines: [docLines[pd].no],
            });
          }
        }
      }

      // 找不到時列出所有相近的；找得到時原則上不干擾 —— 唯一的例外是「命中的那幾行
      // 都沒有勿上網，但隔壁有一則只差一兩個字的卻是勿上網」。那正是打錯字最危險的
      // 情況：你會拿到一個 ✅，而真正該擋的那則就在旁邊。這種一定要講。
      var suggestions = groups.length === 0 ? nearMatches(docLines, wanted, 3) : [];

      // 結果本身已經是勿上網就不必再警示；否則一律掃一遍文件裡相關的勿上網稿。
      var alreadyBlocked =
        groups.length > 0 && resultRank({ found: true, groups: groups }) === RANK["no-upload"];
      var warnings = alreadyBlocked ? [] : relatedNoUpload(docLines, wanted, 3);
      // 已經在錯字建議裡出現過的就不重複講。
      warnings = warnings.filter(function (w) {
        for (var i = 0; i < suggestions.length; i++) {
          if (suggestions[i].text === w.text) return false;
        }
        return true;
      });

      results.push({
        slug: String(raw).trim(),
        found: groups.length > 0,
        groups: groups,
        suggestions: suggestions,
        warnings: warnings,
      });
    }
    return results;
  }

  function isSeparatorLine(folded) {
    return /^=+[^=]*=+$/.test(folded) || /^=+$/.test(folded);
  }

  // 結果排序：要處理的排前面。勿上網 → 其他標記 → 找不到 → 無標記。
  // 「無標記」是唯一不需要你做任何事的結果，所以沉到最底下；一則 slug 跨天出現多次
  // 而標記不一致時，照它最嚴重的那一種排，不然一筆 (勿上網) 會被同名的無標記蓋掉。
  // 部分命中又沒有勿上網的，排在「找不到」後面、「無標記」前面：它需要你核對一眼，
  // 不像完全命中的無標記那樣可以直接放過。
  var RANK = { "no-upload": 0, other: 1, missing: 2, partial: 2.5, none: 3 };

  // 這一筆有沒有任何「⛔」要讓人看到：命中的、警示的、或錯字建議裡的。
  function hasRedFlag(result) {
    var i;
    for (i = 0; i < (result.warnings || []).length; i++) return true;
    for (i = 0; i < (result.suggestions || []).length; i++) {
      if (result.suggestions[i].status === "no-upload") return true;
    }
    for (i = 0; i < (result.groups || []).length; i++) {
      if (result.groups[i].status === "no-upload") return true;
    }
    return false;
  }

  function resultRank(result) {
    // 紅字優先於一切：查不到、但相關的勿上網稿被掃出來的那一筆，跟直接命中勿上網
    // 一樣危險（多半就是同事報錯了），所以也排在最前面，不要夾在中間讓人滑過去。
    if (hasRedFlag(result)) return RANK["no-upload"];
    if (!result.found) return RANK.missing;
    var best = RANK.none;
    for (var i = 0; i < result.groups.length; i++) {
      var g = result.groups[i];
      var r = RANK[g.status];
      if (g.partial && r > RANK.partial) r = RANK.partial;
      if (r < best) best = r;
    }
    return best;
  }

  // 同一級之內保持輸入順序（Array.prototype.sort 的穩定性在舊瀏覽器沒有保證，
  // 所以自己帶 index 進去排）。
  function sortResults(results) {
    return results
      .map(function (r, i) { return { r: r, i: i, rank: resultRank(r) }; })
      .sort(function (a, b) { return a.rank - b.rank || a.i - b.i; })
      .map(function (x) { return x.r; });
  }

  function parseSlugInput(text) {
    return String(text == null ? "" : text)
      .split(/\r?\n/)
      .map(function (l) { return l.trim(); })
      .filter(function (l) { return l !== ""; });
  }

  root.CollabMarkerLookup = {
    fold: fold,
    splitLeadingMarkers: splitLeadingMarkers,
    hasNoUploadMarker: hasNoUploadMarker,
    lineMatchesSlug: lineMatchesSlug,
    findSlugInLine: findSlugInLine,
    findSlugRange: findSlugRange,
    findPartialRange: findPartialRange,
    isNoUploadTerm: isNoUploadTerm,
    parsePrefix: parsePrefix,
    isUploaderTag: isUploaderTag,
    stripLeadingNoise: stripLeadingNoise,
    nearMatches: nearMatches,
    relatedNoUpload: relatedNoUpload,
    editDistance: editDistance,
    slugCandidates: slugCandidates,
    lookup: lookup,
    resultRank: resultRank,
    hasRedFlag: hasRedFlag,
    sortResults: sortResults,
    parseSlugInput: parseSlugInput,
  };
})(typeof module !== "undefined" && module.exports ? module.exports : (typeof globalThis !== "undefined" ? globalThis : this));
