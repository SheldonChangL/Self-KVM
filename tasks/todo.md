# Self-KVM — 自主復刻 Synergy/Barrier 軟體 KVM

> 目標：從零自製、不抄原版原始碼，打造一套「一組鍵鼠跨多台電腦共用」的軟體 KVM。
> 技術棧：Tauri 2（Rust 引擎 + Web 前端）／全平台 macOS·Win·Linux／對標 Barrier 完整功能。
> 協定：忠實復刻 Barrier 線上格式（4-byte 大端長度前綴 + 4-char opcode）。

## 架構（Cargo workspace）

```
crates/
  kvm-proto    純協定：訊息型別 + codec + 鍵碼表（無 I/O，完整單元測試）
  kvm-core     領域邏輯：螢幕佈局、邊緣偵測、座標映射、server/client 狀態機（純，可測）
  kvm-net      非同步傳輸：tokio framing + TLS(TOFU 指紋)
  kvm-input    平台輸入：trait + rdev 擷取 + enigo 注入 + mock backend
  kvm-daemon   無頭 CLI runner（server/client），--mock-input 可在 localhost 端到端實測
src-tauri/     Tauri 2 殼：包裝 daemon、commands/events、系統匣
ui/            Vite + Svelte 前端（frontend-design：佈局編輯器 / 狀態 / 設定 / 指紋信任）
```

**可驗證性策略**：proto/core/net 無需權限或顯示器，可完整測試。輸入抽象成 trait，
mock backend 記錄事件而非真注入 → server 透過 localhost 轉發合成輸入給 client，
client 注入 mock，斷言事件正確 = 真實的端到端證明。

## 任務清單

### Tier 1 — 核心（完整實作 + 測試 + 驗證）✅ 完成（36 測試全綠、0 警告）
- [x] T1.1 workspace 骨架 + kvm-proto codec 原語（BE int / 長度字串 / i32 向量）
- [x] T1.2 kvm-proto 全訊息型別 encode/decode + round-trip 測試
- [x] T1.3 kvm-proto 鍵碼表（KeyID / KeyButton / KeyModifierMask）
- [x] T1.4 kvm-net framing transport（4-byte 長度）+ localhost 整合測試
- [x] T1.5 kvm-core 螢幕佈局 + 鄰接 + 座標映射 + 邊緣偵測 + 測試
- [x] T1.6 kvm-core server 狀態機（握手/QINF/DINF/CIAK/enter-leave/輸入轉發/CALV）
- [x] T1.7 kvm-core client 狀態機
- [x] T1.8 kvm-input traits + mock backend
- [x] T1.9 kvm-daemon：server/client runtime + CLI
- [x] T1.10 端到端 localhost 測試：server→client 鍵鼠轉發（mock 注入斷言）✅ 通過

### Tier 2 — 真實輸入（實作 + 編譯通過）✅ 完成
- [x] T2.1 rdev 擷取 backend（grab，feature-gated）
- [x] T2.2 enigo 注入 backend（feature-gated）
- [x] T2.3 daemon real-input 接線（real binary 連線冒煙測試通過）

### Tier 3 — GUI（frontend-design）✅ 完成
- [x] T3.1 Tauri 2 app 骨架 + commands/events + state
- [x] T3.2 系統匣 + 背景執行緒接 daemon（capture 單例 + sink re-point）
- [x] T3.3 前端：control-deck 美學設定 UI（拖曳佈局編輯器、模式切換、即時狀態、訊號 log）✅ demo 模式視覺驗證
- [x] T3.4 整體 cargo build（tauri 2.11）+ 前端 build（tsc + vite）通過

### Tier 4 — 完整功能 ✅ 完成
- [x] T4.1 TLS 自簽憑證 + TOFU SHA-256 指紋（先握手驗指紋再收協定訊息）✅ localhost 實測 2 tests
- [x] T4.2 剪貼簿同步（CCLP/DCLP）✅ trait + mock 測試 + arboard backend + daemon bridge 接線
- [x] T4.3 設定持久化（save/load JSON）✅ 測試
- [x] T4.4 熱鍵 / 切換鎖定（screen-lock）✅ ServerMachine 測試

### Tier 5 — 快速檔案傳輸（FTP/Samba 式）✅ 完成
> 目標：在不影響鍵鼠延遲的前提下，做到像 FTP put 的快速檔案交換。
> 設計：**獨立 bulk 連線**（第二條 TCP/TLS），檔案位元組完全不碰單執行緒輸入 controller →
> 零 head-of-line blocking。協定 `FileOffer → FileAccept → N×FileChunk → FileEnd`，
> 真正實作了剪貼簿留作 TODO 的分塊 + 重組。
- [x] T5.1 kvm-proto：FOFR/FDAT/FEND/FACK 訊息 + u64 codec 原語 + round-trip（minor→9）
- [x] T5.2 kvm-core `file_transfer`：純切塊 `plan_messages` + `FileReassembler`（驗序/size/count）
      + `sanitize_filename`（防路徑穿越）✅ 10 純單元測試
- [x] T5.3 kvm-daemon `file_transfer`：獨立通道 `send_file`/`recv_file_to_dir`/`serve_recv`，
      stream 自磁碟、`tokio::fs` 落地、失敗刪殘檔、大小上限/接收確認 ✅ 3 localhost e2e
- [x] T5.4 CLI `send`/`recv` 子指令（= FTP put）✅ 真 process 8 MiB 傳輸 SHA-256 一致
- [x] T5.5 TLS 加密 bulk 通道（重用 TOFU 指紋）✅ 真 process 2 MiB 加密傳輸一致
- [x] T5.6 Tauri `send_file` command（GUI 觸發鉤子）✅ 編譯通過（GUI 行為需顯示器，未 headless 驗）

## 進度與驗證紀錄（Review）

**成果**：從零自製、不抄原版的軟體 KVM，Tauri（Rust 引擎 + Web 前端）/ 全平台架構 /
對標 Barrier 完整功能。**42 測試全綠、workspace 0 警告。**

**已驗證（真的能用）**
- 協定 codec、鍵碼、所有訊息 round-trip（12）
- 框架傳輸：localhost TCP round-trip、clean close、超大 frame 拒絕（3）
- 核心邏輯：佈局/邊緣偵測/座標映射、server 切換腦（grab/enter/leave/modifier/鎖定）、
  client 握手/注入閘控、設定持久化（21）
- **端到端**：真實 localhost TCP，server→client 鍵鼠轉發，mock 注入斷言完全一致；
  未知名稱 client 被拒（2）
- **TLS + TOFU 指紋**：真實 TLS 握手、加密 frame round-trip、指紋記錄與變更拒絕（+2）
- 真實 binary 連線冒煙測試（server/client 程序握手）
- GUI：control-deck 美學，demo 模式視覺驗證（截圖：佈局編輯器、即時狀態、模式切換）

**已實作但此環境無法 headless 驗證（誠實標示）**
- rdev 真實擷取 / enigo 真實注入：需 Accessibility/Input Monitoring 權限 → 僅編譯驗證
- arboard 剪貼簿：需系統剪貼簿 → mock 測試 + 編譯驗證
- Tauri GUI 視窗：需顯示器 → 完整編譯（tauri 2.11），UI 以瀏覽器 demo 模式視覺驗證

**界線**：純邏輯（proto/core/net）完整測試；OS/顯示器相關層編譯驗證 + 可抽換 mock。
