# 📊 AI Quota Deck

集中查看 Claude Code、Codex、Antigravity、Gemini 與 Grok 使用額度的 Windows 系統匣工具。

📘 [English](./README.md)

---

## 畫面預覽

### Dashboard
<p align="center">
  <img src="./assets/screenshot.png" alt="AI Quota Deck 深色 Dashboard" width="500">
</p>

### Widget
<p align="center">
  <img src="./assets/screenshot2.png" alt="AI Quota Deck Widget" width="320">
</p>

### Strip
<p align="center">
  <img src="./assets/screenshot3.png" alt="AI Quota Deck Strip" width="500">
</p>

Dashboard 會顯示重置時間、使用速度、方案與快取狀態；Widget 與 Strip 則保留最重要的百分比，並依用量顯示綠、黃、紅色。

---

## 顯示內容

| 服務 | 額度週期 | 設定方式 |
|---|---|---|
| **Claude** | 5 小時、每週，以及可用的模型專屬額度 | 登入 Claude Code |
| **Codex** | Free 顯示每月；Plus 顯示每週 | 登入 Codex Desktop |
| **Antigravity** | Gemini 池與 Claude + GPT 池各自的每週與 5 小時額度 | 保持 Antigravity IDE 開啟 |
| **Gemini** | 5 小時、每週 | [進階 Browser Bridge 設定](#進階gemini-與-grok) |
| **Grok** | 每週與各產品細目；免費帳號則顯示查詢額度 | Browser Bridge，或 Grok Build 備援 |

未設定的服務會自動隱藏；各家獨立更新，單一服務發生問題不會拖累其他卡片。

其他功能：

- Dashboard、Widget 與 Strip 三種顯示方式
- 可勾選要顯示哪些服務；隱藏的服務完全不會被查詢
- 可置頂、記住位置並鎖定移動的 Widget
- 可放在螢幕任意位置，或釘在 Windows 工作列上方且不占用工作區的精簡橫向 Strip
- 淺色、深色與跟隨系統主題
- 系統匣操作與選用的開機啟動
- 服務或瀏覽器暫時無法使用時保留快取資料
- Windows 閒置／鎖定時暫停 Claude Code 查詢，並透過官方 CLI 恢復過期 OAuth 連線；遇到 rate limit 時自動退避

在 Strip 中點擊圖釘形狀的工作列按鈕，再將 Strip 拖到工作列的空白處即可。工作列自動隱藏或其他 App 進入全螢幕時，Strip 會暫時讓位。

---

## 安裝方式

**系統需求：Windows 10 或 11。**

1. [點此下載最新安裝檔](https://github.com/JoshuaWang2211/ai-quota-deck/releases/latest/download/ai-quota-deck_0.3.0_x64-setup.exe)。
2. 執行安裝檔，再啟動 **AI Quota Deck**。
3. 關閉視窗會回到系統匣；左鍵點擊系統匣圖示即可重新開啟。

**Codex：** 只需在 Codex Desktop 登入即可。

**Claude：** 若沒有偵測到 Claude Code，請先安裝它，在終端機執行一次 `claude` 並完成登入；之後不必讓 CLI 持續執行。

**Antigravity：** 在 Antigravity IDE 登入一次，想看即時數字時保持 IDE 開啟。App 只讀取 IDE 在 `127.0.0.1` 的本機 language server；IDE 關閉時會保留最後一次結果並標示 `cached`，最多 24 小時，之後卡片會提示開啟 IDE。

---

## Widget 與 Strip

主面板上方有兩個按鈕：

- **Widget**：開啟置頂小工具。拖曳標題列調整位置；鎖頭按鈕可鎖定或解鎖位置。
- **Strip**：把相同資料排成精簡橫條；按鈕以外的區域都能拖曳，而且不會保留或縮小 Windows 工作區。

Widget 與 Strip 會各自記住位置。兩種畫面都可從自身按鈕或系統匣開啟主面板、隱藏，並共用主面板資料，不會增加查詢次數。

---

## 進階：Gemini 與 Grok

> 這段設定比較麻煩，需要開啟 Chromium 開發人員模式、手動載入未封裝擴充功能，並保留已登入的 Gemini 或 Grok 分頁。

Gemini 與 Grok 必須透過隨附的 **AI Quota Deck Browser Bridge** 從瀏覽器讀取用量。若只使用 Claude Code、Codex 與 Antigravity，可以跳過本節。

- Gemini 必須安裝 Bridge。
- Grok 優先使用 Bridge，也可退回讀取已登入的 Grok Build CLI。
- 已驗證 Chrome、Comet、Edge 與 Brave；Vivaldi、Opera、Chromium 已支援註冊，但尚未實測。

### 設定步驟

1. 先啟動一次 AI Quota Deck，再點擊 **Providers**（在還沒偵測到任何服務前，按鈕會顯示 **Set up providers**）。
2. 複製或開啟 App 顯示的 Bridge 資料夾：

   ```text
   %LOCALAPPDATA%\ai-quota-deck\browser-bridge
   ```

3. 開啟 `chrome://extensions`，啟用**開發人員模式**，再按**載入未封裝項目**。
4. 選擇 Bridge 資料夾。
5. 點擊一次 Bridge 工具列圖示，授權它與桌面 App 通訊。
6. 保留已登入的 [Gemini](https://gemini.google.com) 或 [Grok](https://grok.com) 分頁。

卡片通常會在約三分鐘內出現；重新打開面板時也會主動檢查。

### 必須保留 Bridge

要持續更新，Bridge 與對應的登入分頁都必須保留；背景分頁即可。瀏覽器／分頁關閉、帳號登出或電腦睡眠期間無法產生新資料，面板會保留最後一次結果並標示 `cached`，喚醒或解鎖後再補抓。

更新 AI Quota Deck 後，請重開瀏覽器，讓它載入新版 Bridge。

---

## 獨立瀏覽器擴充功能

若您只需要查看 Gemini 或 Grok 的用量，不需要執行桌面 App，可以使用我製作的 Chrome 擴充功能：

- **[Gemini Usage Monitor](https://chromewebstore.google.com/detail/cdepnbhodggenlkdeelnocfckdendhad?utm_source=item-share-cb)**
- **[Grok Usage Watch](https://chromewebstore.google.com/detail/bmpboaihdkpkjehbceegdmndkonlpdge?utm_source=item-share-cb)**

---

## 疑難排解

**看不到 Gemini 或 Grok：** 確認 Bridge 已啟用、曾點擊一次工具列圖示，且已開啟登入中的服務分頁。

**瀏覽器資料過期：** 打開對應分頁並等待最多約三分鐘；瀏覽器、Bridge、分頁與系統匣 App 都必須正在執行。

**Claude 顯示 rate limit：** cached 卡片會顯示倒數。App 會完整遵守伺服器的冷卻時間；重開 App 不會提早重試，而 Claude Code 換新登入憑證後也不會沿用舊憑證的期限。

**Claude 在閒置或鎖定時沒有更新：** 這是刻意的節流。回來操作後會先等待約兩分鐘再查詢，避免與剛恢復的 Claude Desktop 或 Claude Code 搶在同一時間連線；既有的 rate-limit 冷卻仍優先。

**Claude 顯示 token rejected：** App 會在背景請已安裝的 Claude Code CLI 更新 token，然後自動重試。若仍失敗，再手動開啟一次 Claude Code。

**Antigravity 提示開啟 IDE：** App 只能在 Antigravity IDE 執行中讀取額度。開啟 IDE 後等待一個更新週期即可。

**Windows 顯示未知發行者：** 目前版本尚未加入程式碼簽章，請只從本專案的 GitHub Releases 頁面下載。

---

## 隱私

AI Quota Deck 沒有遙測，也不會上傳資料。

- 使用支援的桌面 App 或 CLI 已儲存的登入資料。
- 不會讀取或使用各服務的 refresh token。Claude 回傳 `401` 時，只會在背景執行官方 `claude update` 並重讀 access token；此指令也可能同時更新 Claude Code。
- Antigravity 只從 IDE 在 `127.0.0.1` 的本機 language server 讀取；不會讀取、儲存或更新任何 Google 登入。
- Bridge 只能讀取 `gemini.google.com` 與 `grok.com`。
- 只有額度、重置時間、服務／帳號代號與觀測時間會傳到 App；Cookie 與頁面 token 留在瀏覽器中。

---

## 已知限制

各家用量來自未公開的內部端點；若供應商改變格式，單一卡片可能暫時失效，但不會影響其他服務。

同一瀏覽器服務若同時登入多個帳號，會顯示最後回報的帳號；目前尚未支援固定帳號。

---

## 專案資訊

- [更新紀錄](./docs/CHANGELOG.md)
- [架構說明](./docs/ARCHITECTURE.md)
- [回報問題](https://github.com/JoshuaWang2211/ai-quota-deck/issues)
- 開發者：[Joshua Wang](https://www.threads.com/@joshuawang2211)

## 授權

本專案採用 [MIT License](./LICENSE)。
