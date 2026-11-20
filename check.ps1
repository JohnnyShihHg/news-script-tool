# 一個指令跑完所有檢查（AUDIT_REPORT 5.1）。完成任何修改前都要跑，而且要全部通過。
#
#   powershell -ExecutionPolicy Bypass -File check.ps1
#   powershell -ExecutionPolicy Bypass -File check.ps1 -UpdateClippyBaseline   # 清掉舊警告後更新待清清單
#
# 跑的東西（全部是本機、假資料，不連 iNEWS、不碰協作文件、不呼叫 Gemini、不抓氣象署）：
#   1. cargo test --workspace（同時產生 app/tests/generated/collab-cases.js）
#   2. cargo clippy：不能比 tools/check/clippy-baseline.txt 多（先止血；清單裡的是待清）
#   3. cargo fmt --check：只報數量、不擋（既有 600 多處差異，待清，見 AUDIT_REPORT #14）
#   4. 空的 catch：JS 檔和 main.rs 裡注入的腳本一律不准有
#   5. JS 語法：app.js、logic.js、守門員、collab-marker（只解析不執行）
#   6. 瀏覽器測試頁（Edge／Chrome headless）：logic.test.js、lookup.test.js、
#      fake-quill.html（寫入／校正腳本 golden test）、守門員 fake-stale.html 每個 mode
#
# 這台電腦預設的 gnu 工具鏈編不過，有裝 stable-x86_64-pc-windows-msvc 就用它。

param([switch]$UpdateClippyBaseline)

$ErrorActionPreference = "Continue"
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
$OutputEncoding = [System.Text.Encoding]::UTF8
$root = $PSScriptRoot
Set-Location $root
$env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"

$failures = New-Object System.Collections.Generic.List[string]
$notes = New-Object System.Collections.Generic.List[string]
function Step($name) { Write-Host ""; Write-Host "== $name ==" -ForegroundColor Cyan }
function Fail($msg) { $failures.Add($msg); Write-Host "FAIL $msg" -ForegroundColor Red }
function Pass($msg) { Write-Host "ok   $msg" -ForegroundColor Green }

$toolchain = @()
$list = & rustup toolchain list 2>$null
if ($list -match "stable-x86_64-pc-windows-msvc") { $toolchain = @("+stable-x86_64-pc-windows-msvc") }

# ---------------------------------------------------------------- 1. cargo test
Step "cargo test --workspace"
& cargo @toolchain test --workspace --quiet
if ($LASTEXITCODE -ne 0) { Fail "cargo test 失敗（看上面的輸出）" } else { Pass "cargo test" }

# ---------------------------------------------------------------- 2. clippy
Step "cargo clippy（不能多過待清清單）"
$baselinePath = Join-Path $root "tools\check\clippy-baseline.txt"
$raw = & cargo @toolchain clippy --workspace --all-targets --message-format=json 2>$null
$clippyExit = $LASTEXITCODE
$seen = @{}
$current = @{}
foreach ($line in $raw) {
  if (-not "$line".StartsWith("{")) { continue }
  $msg = $null
  try { $msg = "$line" | ConvertFrom-Json } catch { Write-Host "（略過一行無法解析的 cargo 輸出）"; continue }
  if ($msg.reason -ne "compiler-message" -or $msg.message.level -ne "warning" -or -not $msg.message.code) { continue }
  $span = $msg.message.spans | Where-Object { $_.is_primary } | Select-Object -First 1
  if (-not $span) { continue }
  $file = ($span.file_name -replace "\\", "/")
  $code = $msg.message.code.code
  $at = "$file|$($span.line_start)|$code"
  if ($seen.ContainsKey($at)) { continue } # lib 和 lib test 會各報一次
  $seen[$at] = $true
  $key = "$file|$code"
  if ($current.ContainsKey($key)) { $current[$key] += 1 } else { $current[$key] = 1 }
}
if ($clippyExit -ne 0) {
  Fail "cargo clippy 跑不起來（exit $clippyExit）。沒裝的話：rustup component add --toolchain stable-x86_64-pc-windows-msvc clippy"
} elseif ($UpdateClippyBaseline) {
  $header = @(
    "# cargo clippy 既有警告（待清，見 AUDIT_REPORT #14）。格式：檔案|lint|數量。",
    "# check.ps1 只擋「比這份多」的警告。清掉幾個之後跑 check.ps1 -UpdateClippyBaseline 更新；不要為了過關把新警告加進來。"
  )
  $body = $current.Keys | Sort-Object | ForEach-Object { "$_|$($current[$_])" }
  [System.IO.File]::WriteAllLines($baselinePath, [string[]]($header + $body), (New-Object System.Text.UTF8Encoding($false)))
  Pass "已更新 tools/check/clippy-baseline.txt（$($current.Count) 組）"
} else {
  $baseline = @{}
  if (Test-Path $baselinePath) {
    foreach ($l in [System.IO.File]::ReadAllLines($baselinePath)) {
      if ($l.StartsWith("#") -or -not $l.Trim()) { continue }
      $parts = $l.Split("|")
      $baseline["$($parts[0])|$($parts[1])"] = [int]$parts[2]
    }
  }
  $new = @()
  foreach ($k in $current.Keys) {
    $allowed = 0
    if ($baseline.ContainsKey($k)) { $allowed = $baseline[$k] }
    if ($current[$k] -gt $allowed) { $new += "$k（現在 $($current[$k])，待清清單 $allowed）" }
  }
  $total = ($current.Values | Measure-Object -Sum).Sum
  if ($new.Count) {
    Fail ("clippy 有新的警告：`n  " + ($new -join "`n  ") + "`n  請修掉這個警告本身（照 clippy 的 help 改），不要加 #[allow]、也不要更新待清清單來過關。看細節：cargo clippy --workspace --all-targets")
  } else {
    Pass "clippy 沒有新警告（既有待清 $total 個，見 tools/check/clippy-baseline.txt）"
  }
}

# ---------------------------------------------------------------- 3. rustfmt（只報數量）
Step "cargo fmt --check（只報數量，不擋）"
$fmt = & cargo @toolchain fmt --check 2>$null
if ($LASTEXITCODE -ne 0 -and -not ($fmt -match "^Diff in")) {
  $notes.Add("cargo fmt 跑不起來。沒裝的話：rustup component add --toolchain stable-x86_64-pc-windows-msvc rustfmt")
} else {
  $diffs = @($fmt | Where-Object { $_ -match "^Diff in" }).Count
  $notes.Add("rustfmt 待清差異 $diffs 處（既有，見 AUDIT_REPORT #14；要整個專案一次格式化請另開一個只做格式的 commit）")
  Write-Host "rustfmt 差異 $diffs 處（不擋）"
}

# ---------------------------------------------------------------- 4a. 前端寫死的設定預設值
Step "前端不准自己寫設定預設值（?? 數字）"
$appJs = [System.IO.File]::ReadAllText((Join-Path $root "app\src\app.js"))
$cfgDefault = '\b(inewsCfg|inewsSettings|s|config\.[a-z_]+)\.[a-z_]+\s*\?\?\s*\d'
$cfgHits = @()
foreach ($m in [regex]::Matches($appJs, $cfgDefault)) {
  $lineNo = ($appJs.Substring(0, $m.Index) -split "`n").Count
  $cfgHits += "app\src\app.js:$lineNo  $($m.Value)"
}
if ($cfgHits.Count) {
  Fail ("前端寫了設定的預設值：`n  " + ($cfgHits -join "`n  ") +
    "`n  正確做法：預設值只寫在 core/src/config.rs（serde 預設），get_config 會回傳完整設定，前端直接用；" +
    "新欄位記得在 Rust 加預設值，core/tests/frontend_config_fields.rs 會檢查。")
} else {
  Pass "沒有前端寫死的設定預設值"
}

# ---------------------------------------------------------------- 4b. 空的 catch
Step "不准有空的 catch"
$jsFiles = @(Get-ChildItem -Path "app\src", "app\tests", "tools" -Recurse -Include *.js, *.html -File |
  Where-Object { $_.FullName -notmatch "\\generated\\" })
$emptyJs = '\.catch\(\s*(\(\s*\w*\s*\)|\w+)\s*=>\s*(\{\s*\}|null|undefined)\s*\)|catch\s*(\([^)]*\))?\s*\{\s*\}'
$emptyRs = 'catch\s*(\([^)]*\))?\s*\{\{\s*\}\}|\.catch\(\s*(\(\s*\w*\s*\)|\w+)\s*=>\s*\{\{\s*\}\}\s*\)'
$hits = @()
foreach ($f in $jsFiles) {
  $text = [System.IO.File]::ReadAllText($f.FullName)
  foreach ($m in [regex]::Matches($text, $emptyJs)) {
    $lineNo = ($text.Substring(0, $m.Index) -split "`n").Count
    $hits += "$($f.FullName.Substring($root.Length + 1)):$lineNo  $($m.Value -replace '\s+', ' ')"
  }
}
foreach ($f in @(Get-ChildItem -Path "app\src-tauri\src", "core\src" -Recurse -Include *.rs -File)) {
  $text = [System.IO.File]::ReadAllText($f.FullName)
  foreach ($m in [regex]::Matches($text, $emptyRs)) {
    $lineNo = ($text.Substring(0, $m.Index) -split "`n").Count
    $hits += "$($f.FullName.Substring($root.Length + 1)):$lineNo  $($m.Value -replace '\s+', ' ')"
  }
}
if ($hits.Count) {
  Fail ("空的 catch（錯誤被吞掉、事後查不到原因）：`n  " + ($hits -join "`n  ") +
    "`n  正確做法：自動流程的判斷用 logDecision(...) 記進決策紀錄；寫紀錄本身失敗就在狀態列顯示（inewsStatus(..., `"warn`")）；" +
    "真的可以忽略的，在 catch 裡寫一行註解說明為什麼可以忽略，或至少 console.warn。")
} else {
  Pass "沒有空的 catch"
}

# ---------------------------------------------------------------- 5/6. 瀏覽器
$browser = $env:CHECK_BROWSER
if (-not $browser) {
  foreach ($p in @("${env:ProgramFiles(x86)}\Microsoft\Edge\Application\msedge.exe", "$env:ProgramFiles\Microsoft\Edge\Application\msedge.exe",
                   "$env:ProgramFiles\Google\Chrome\Application\chrome.exe", "${env:ProgramFiles(x86)}\Google\Chrome\Application\chrome.exe")) {
    if ($p -and (Test-Path $p)) { $browser = $p; break }
  }
}

function Open-Page($relPath, $query, $budgetMs) {
  $profileDir = Join-Path ([System.IO.Path]::GetTempPath()) ("nst-check-" + [guid]::NewGuid().ToString("N"))
  New-Item -ItemType Directory -Path $profileDir | Out-Null
  $url = "file:///" + ((Join-Path $root $relPath) -replace "\\", "/") + $query
  $outFile = "$profileDir.out.html"
  # msedge.exe 是 GUI 程式：用 & 呼叫的話 PowerShell 不等它、也拿不到輸出，所以用 Start-Process 等它跑完。
  # --virtual-time-budget 讓頁面的計時器快轉，守門員 40 秒的劇本幾秒就跑完。
  $browserArgs = @("--headless=new", "--disable-gpu", "--no-first-run", "--no-default-browser-check", "--allow-file-access-from-files",
            "`"--user-data-dir=$profileDir`"", "--virtual-time-budget=$budgetMs", "--dump-dom", "`"$url`"")
  Start-Process -FilePath $browser -ArgumentList $browserArgs -NoNewWindow -Wait `
    -RedirectStandardOutput $outFile -RedirectStandardError "$profileDir.err.txt" | Out-Null
  $out = ""
  if (Test-Path $outFile) { $out = [System.IO.File]::ReadAllText($outFile, [System.Text.Encoding]::UTF8) }
  foreach ($p in @($profileDir, $outFile, "$profileDir.err.txt")) {
    try { Remove-Item -Recurse -Force $p -ErrorAction Stop } catch { Write-Host "（暫存檔刪不掉，略過：$p）" }
  }
  return $out
}

function Check-Harness($name, $relPath) {
  $dom = Open-Page $relPath "" 5000
  $m = [regex]::Match($dom, '<pre id="out">(\d+) passed, (\d+) failed \(of (\d+)\)')
  if (-not $m.Success) { Fail "${name}：讀不到結果（頁面沒跑完或載入失敗）"; return }
  $passed = [int]$m.Groups[1].Value; $failed = [int]$m.Groups[2].Value
  # 只看結果框：頁面原始碼裡本來就有 "load errors:" 這個字串。
  $detail = [regex]::Match($dom, '<pre id="out">([\s\S]*?)</pre>').Groups[1].Value
  if ($failed -gt 0 -or $passed -eq 0 -or $detail -match "load errors:") {
    Fail "${name}：$passed 通過、$failed 失敗`n$([System.Net.WebUtility]::HtmlDecode($detail))"
  } else {
    Pass "${name}：$passed 通過"
  }
}

if (-not $browser) {
  Fail "找不到 Edge 或 Chrome，瀏覽器測試沒跑。設定環境變數 CHECK_BROWSER 指到瀏覽器執行檔。"
} else {
  Step "JS 語法（只解析不執行）"
  $dom = Open-Page "tools\check\syntax-check.html" "" 3000
  $m = [regex]::Match($dom, '<pre id="out">([\s\S]*?)</pre>')
  $text = [System.Net.WebUtility]::HtmlDecode($m.Groups[1].Value)
  if (-not $m.Success -or $text -notmatch "^ALL OK") { Fail "JS 語法檢查：`n$text" } else { Pass ($text -split "`n")[0] }

  Step "瀏覽器測試頁"
  Check-Harness "logic.test.js" "app\tests\browser-harness.html"
  Check-Harness "lookup.test.js（collab-marker）" "tools\collab-marker\tests\browser-harness.html"
  Check-Harness "寫入／校正腳本 golden test（fake-quill.html）" "app\tests\fake-quill.html"

  Step "守門員 fake-stale.html 每個 mode"
  foreach ($mode in @("ok", "stale", "gap", "gapok", "hard", "typing", "hidden", "echo", "cold", "coldhidden", "toolwrite", "coldhang")) {
    $dom = Open-Page "tools\collab-guard\tests\fake-stale.html" "?mode=$mode" 40000
    $v = [System.Net.WebUtility]::HtmlDecode([regex]::Match($dom, '<pre id="verdict">([^<]*)</pre>').Groups[1].Value)
    if ($v -like "PASS*") { Pass "守門員 $v" } else { Fail "守門員 mode=${mode}：$v" }
  }
}

# ---------------------------------------------------------------- 結果
Write-Host ""
foreach ($n in $notes) { Write-Host "註：$n" -ForegroundColor Yellow }
if ($failures.Count) {
  Write-Host ""
  Write-Host "check 失敗：$($failures.Count) 項" -ForegroundColor Red
  exit 1
}
Write-Host "check 全部通過" -ForegroundColor Green
exit 0
