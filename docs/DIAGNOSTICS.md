# 診断レポートの読み方

公式 `IPLROM.DAT` と `HUMAN302.XDF` を X68000 で起動したとき、`PC=$00ff9006 /
SR=$2000` はIPLのFDCステータス／結果フェーズ待ちを示す。一度だけ観測することは
正常だが、同じ位置に数フレーム以上留まり `$0018` のエラー画面へ進むのは正常ではない。

旧実装ではFDC結果割り込みをデータ転送中に早く通知し、IOC割り込み要求をレベル扱い
していた。またHD63450のTerminal CountをFDCへ通知せず、IPLが要求した384 bytesの
DMA転送後にも1024 bytesセクタの残りを待ち続けていた。さらにDMACがFDCのDREQを
待たず、IPLがFDC commandを送る前に空FIFOを転送して完了する経路があった。この
組合せで上記の停止が発生した。現在は次を実装し、公式Human68kを`A>`プロンプト相当
の起動処理まで実行する700フレーム回帰試験で固定している。

- FDCのexecution phaseと7 bytesのresult phaseを分離し、結果フェーズ開始時だけIRQを発生
- IOCでFDC信号の立上りを要求としてラッチし、CPU acknowledgeで要求だけを解除
- FDC execution phaseのDREQでDMAをゲートし、DMACの先行start時はcommand到着まで待機
- DMA Terminal Countで未転送のセクタ末尾を破棄し、FDCをresult phaseへ移行
- CPUアダプタを命令境界ごとの実行経路に固定し、自己再配置コードとdecode cacheの不整合を回避
- Area Setを8KiB単位で実装し、CPUの例外entry中もSビットをBusへ即時同期
- M68000のbus/address error 14-byte frameを`SR, PC, IR, address, status`順に修正

画面に `$0018` だけが表示された場合は、画面の文字列だけではCPU例外、Human68kの
媒体エラー、またはFDCの結果コードを区別できません。UIの「診断情報出力」で得た
JSONを確認してください。

- `cpu_pc` / `cpu_sr`: 現在のCPU位置（`00ff9006`が継続する場合は異常）
- `first_bus_fault` / `last_bus_fault` / `bus_fault_count`: 実際のバスエラー
- `fdc_command`, `fdc_status`, `fdc_output`: FDCの現在フェーズ
- `fdc_st0`, `fdc_st1`, `fdc_st2`: 結果フェーズのST0/ST1/ST2（データ転送中は次の結果待ち）

起動中に未搭載SCSI/MIDI等の検出用バスエラーが少数記録されることはあります。低位RAM
のArea Set違反が増え続ける、`PC=$00000004`のillegal instructionになる、またはFDCの
`output`が残ったまま`$00ff9006`へ留まる場合は異常です。

修正版でも同じ停止になる場合は、ブラウザを強制再読込してWasmを更新してから再試行する。
それでも再現する場合は、ROM／媒体自体ではなく上記の診断JSONをIssueへ添付する。

SCSI ROMを使う場合は、X68000/XVIには`SCSIEXROM.DAT`、X68030には
`SCSIINROM.DAT`を選びます。8KiB ROMのリセットベクタも検査するため、機種違いの
ROMは明示的なエラーになり、エミュレーション自体は継続します。ROM、ゲーム、
CGROM本体や媒体イメージは診断JSONへ添付しないでください。
