# Lessons

> 修正與踩雷紀錄，避免重蹈覆轍。

- **手刻 PNG：座標一定要先 round/floor 再當 typed-array index**。JS 對 `Uint8Array`
  用非整數索引會「靜默寫入物件屬性」而非緩衝區，整段繪圖無效卻無錯誤。第一版圖示只畫出
  圓點（用了 `Math.round`），矩形/線（用 float 迴圈）全沒畫出來。修法：在 setter 內
  `x=Math.round(x); y=Math.round(y)`。
- **enigo `Key` 變體跨平台不一致**：macOS 沒有 `Key::Insert`。對應表要落到「不支援則
  略過」而非硬編所有變體，否則 `--features` 編譯炸。
- **enigo/arboard 的 handle 多半不是 `Send`**：要在 tokio 多執行緒 runtime 用，需用
  「專屬執行緒 + channel proxy」模式（`ThreadedInjector` / `ClipboardBridge`），
  讓跨 task 傳遞的是 `Sender`（Send）而非 handle 本身。
- **rustls 0.23 用 ring 後端**避免 aws-lc-rs 的 cmake/C 編譯依賴；
  `builder_with_provider(ring::default_provider())` 明確指定，免去 process-global
  install 競態。TOFU 驗證器仍要用 provider 的演算法真正驗簽，只是跳過 CA 鏈。
- **可測試性策略**：把 OS/顯示器 I/O 抽到 trait + mock，純狀態機與協定就能完整單元測試；
  端到端用 mock injector 在 localhost 實測，不需權限或顯示器。
- **大檔傳輸要走獨立連線，別擠控制連線**：單一 TCP 每方向是一條有序位元組流，4 MiB 的
  檔案 frame 會把排在後面的鍵鼠 frame 卡住（head-of-line blocking）。檔案傳輸開第二條
  TCP/TLS（bulk 通道）才不會讓游標卡頓。TLS-over-單一-TCP 沒用，因為還是一條有序流。
- **新增 `Message` variant 會打爆顯式列舉的 match**：`ClientMachine` 的「忽略」分支是
  逐一列出 variant（非萬用 `_`），加新訊息要記得補進去，否則 E0004 non-exhaustive。
  這是「不用萬用分支」換來的好處——編譯器強迫你決定每個新訊息怎麼處理。
- **`tokio::fs` 要 `features=["fs"]`**：workspace 預設只開 net/io-util/sync/macros/time，
  用到檔案 I/O 要在 workspace tokio features 補 `"fs"`，否則 `tokio::fs::File` 找不到。
- **檔名消毒只取最後一段 component**：`name.rsplit(['/','\\']).next()` 取 basename，
  再拒絕空 / `.` / `..` / 仍含分隔符或 NUL 的。這樣 `../../etc/passwd` 收斂成 `passwd`，
  不可能逃出下載目錄。純函式可完整單元測試，不必碰真檔案系統。
- **分塊重組要真的驗 seq/size/count**：抄剪貼簿的 `{ data, .. }` 會把 mark/seq 丟掉。
  收端必須照順序驗 seq、累計不可超過宣告 size、結尾 count 要對得上，任何缺塊/亂序/截斷
  都得拒絕並刪殘檔，否則靜默寫出壞檔。
- **獨立工具的 TOFU 自簽憑證每次啟動都換**：`server_acceptor()` 每跑一次產生新 cert/指紋，
  pin 過的 client 下次連會因「指紋變更」被拒。這是現有 TLS 設計的限制（憑證非持久化），
  測試時用臨時 `HOME` 避免污染真實 trust store。
