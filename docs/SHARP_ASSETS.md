# Sharp公式配布資産

`web/public/sharp/`には、Sharp Xシリーズまたはそのエミュレータで使用するために
公式公開された次の未改変ファイルだけを収録する。これらはwasm-68kのGPLライセンス
ではなく、同ディレクトリの`SHARP_LICENSE_CP932.txt`に従う。許諾条件の要点は
**Xシリーズ／エミュレータ用途限定、非商用、許諾文書の再配布物への添付、
著作権表示の維持**である。正確な条件は必ず添付原文を参照すること。

| ファイル | 公式アーカイブ | byte数 | SHA-256 |
| --- | --- | ---: | --- |
| `IPLROM.DAT` | `X68BIOSE.LZH` | 131072 | `8ead1d0f4ebb9c59a7fa118596f819e191c310442a00c56ab5ec5e9e7a189677` |
| `IPLROMXV.DAT` | `X68BIOSX.LZH` | 131072 | `743436ba571b73ba7d9e12cde2767d05f2885e1ec275fbc3cd0904994675b79a` |
| `IPLROM30.DAT` | `X68BIOS3.LZH` | 131072 | `bdba942ab9c633a3172fbf1a8579849df52c0eeb0da8a3411402f4564d035a27` |
| `HUMAN302.XDF` | `HUMN302I.LZH` | 1261568 | `bc814dab949f517ec3fb5b5b0e71f2adb468107ae0c431ee92ec38b30b031833` |
| `SHARP_LICENSE_CP932.txt` | 各アーカイブ共通 | 4353 | `6d8e254081388c32d748757327313aa831efd7fc785b24678d770be9efbaf00f` |

取得元:

- 許諾条件: <http://retropc.net/x68000/software/sharp/license.htm>
- IPL-ROM: <http://retropc.net/x68000/software/sharp/x68bios/index.htm>
- Human68k 3.02: <http://retropc.net/x68000/software/sharp/human302/index.htm>

`SHARP_LICENSE_CP932.txt`は公式アーカイブ内の原文をbyte単位で保持する。
`SHARP_LICENSE_UTF8.txt`はWebブラウザで読めるよう文字コードだけをUTF-8へ変換した
便宜コピーであり、内容は変更していない。CGROM、ゲーム、その他のROM／媒体は
この許諾対象として収録しない。

Pages UIはダウンロード後にサイズとSHA-256を照合し、Human68k XDFを必ず書込保護で
mountする。利用者の変更は元ファイルへ反映せず、コアのcopy-on-write overlayだけに
保持される。CGROM未読込時は著作物を合成せず、CGROMアドレス範囲を空フォントで
mapするため、IPL/FDCは例外停止せず進むが文字は表示されない。正しい文字表示には
利用者が権利を持つ768KiBの`CGROM.DAT`をWeb UIから読み込む必要がある。

XM6等に付属するCGROM、SCSI ROM、DLLはSharp公式配布資産の許諾対象として扱わない。
ローカル検証用に置く場合は、公開ディレクトリ`web/public/`ではなくGit除外済みの
`local-assets/xm6/`を使用する。Viteは`web/public/`をそのままPages成果物へコピー
するため、そこへ置いてはならない。

SCSI ROMの種類にも注意する。`SCSIINROM.DAT`はX68030内蔵SCSI用、
`SCSIEXROM.DAT`はX68000/XVIの拡張SCSI用であり、機種をまたいで読み込まない。
コアは8KiB ROM先頭ベクタの`$FCxxxx`／`$EAxxxx`を検査して誤組合せを拒否する。

## CGROM未読込時の表示について

Human68kの起動処理はCGROMを参照して文字ビットマップをTVRAMへ展開する。CGROMを
読み込まずに公式IPLとHUMAN302.XDFだけを起動した場合、文字の代わりに黒画面や起動中の
一時的な横帯（TVRAMの未初期化／ラスタ更新）が見えることがある。これはPagesへ
CGROMを同梱しないための仕様であり、CPU/FDCの実行停止を意味しない。文字を表示する
には、権利を持つ768KiBの`CGROM.DAT`をWeb UIの「CG」欄から読み込んでからリセット
すること。CGROMを含む画面や媒体をIssueへ添付してはならない。
