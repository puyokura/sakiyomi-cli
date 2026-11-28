# サキヨミ CLI

`sakiyomi.app` の予想イベントを、ターミナルから読む・買う・売るための非公式クライアント。
リリース済み Android アプリ（`app.sakiyomi.mobile`）の静的解析から通信仕様を復元して作った。
バイナリは 1 つで、対話用の CLI と MCP サーバーの両方になる。

```bash
sakiyomi <コマンド>       # 通常の CLI
sakiyomi mcp             # MCP over stdio（AI エージェントから呼ぶ用）
```

## 最初の 3 分

```bash
cargo build --release

./target/release/sakiyomi doctor        # 接続と認証状態を実測する
./target/release/sakiyomi events list   # 予想一覧を JSON で読む
```

`doctor` が出すもの:

```
base       : https://sakiyomi.app/api
config     : C:\Users\<user>\AppData\Roaming\sakiyomi\config.toml
state      : C:\Users\<user>\AppData\Roaming\sakiyomi\session.json
token      : (none)

checks:
  [ok  ] config_file                  ... (absent, using defaults)
  [ok  ] state_dir                    ...
  [ok  ] session_token                absent (only unauthenticated checks will run)
  [ok  ] GET /config                  200, keys: auth_open, social_open, ...
  [ok  ] GET /me (no token)           401 as expected
  [skip] GET /me (token)              no token available

verdict: ready
```

`GET /me (no token)` が **401 で ok** なのが正しい状態。トークン無しで 200 が返ったら
サーバーの認証モデルが変わったという意味で、ここが `FAIL` になる。

## ログイン

**この CLI は Google のログインを自分では行わない。** アプリ側で確立したトークンを取り込む。

理由は仕様上の制約で、こちらの都合ではない。アプリの Google ログインは、認可コードを
`app.sakiyomi.mobile:/oauthredirect` というカスタムスキームへ返す。Google はこの
クライアントに対して、loopback も LAN IP も https も OOB も受け付けない（実測で 4 種とも拒否）。
つまりデスクトップやサーバーのプロセスが認可コードを受け取る道が無い。だから試さない。

トークンさえあれば後は同じ。API は `Authorization: Bearer <token>` を受けるだけなので、
取り込み経路を 3 つ用意している。

### 取り込む

```bash
sakiyomi auth import-token --token "$TOKEN"
```

標準入力からも渡せる。貼り付けでもパイプでもよい。

```bash
echo "$TOKEN" | sakiyomi auth import-token --stdin
```

取り込み時に `GET /me` で検証する。通らなければ保存しない。

```
$ sakiyomi auth import-token --token "<token>"
{
  "ok": true,
  "verified": true,
  "token_length": 64,
  "state_path": "..."
}
```

`--dry-run` を付けると、検証も保存もしない。

```bash
sakiyomi auth import-token --token "$TOKEN" --dry-run
```

### 保存せずに 1 回だけ使う

```bash
sakiyomi --token "$TOKEN" positions list          # 1 回だけ
export SAKIYOMI_TOKEN="$TOKEN"                    # シェルの間ずっと
sakiyomi positions list
```

優先順位は `--token` > 保存済みセッション。設定ファイルの `token` にも書ける。

トークンの値はどこにも表示されない。`auth status` が出すのは有無と文字数だけ。

```bash
$ sakiyomi auth status
{
  "token_present": true,
  "token_length": 64,
  "auth_provider": null,
  "acquired_via": "import"
}
```

### 終了コード

トークンが拒否されたときは 6。入力形式がおかしいときは 2。

| 状況 | 終了コード |
|---|---|
| 取り込み成功 | 0 |
| トークンが 401 で拒否された | 6 |
| 空白入り・空文字 | 2 |
| 検証は通ったが保存できない | 1 |

### ログアウト

```bash
sakiyomi auth logout              # POST /logout してローカルのセッションも消す
sakiyomi auth logout --forget     # サーバー呼び出しが失敗してもローカルを消す
```

### トークンをどこから取るか

アプリでログイン済みなら、端末側にトークンが保存されている。保存値は Expo の
SecureStore に AES 暗号文で入っており、鍵は AndroidKeyStore にある。暗号文を直接
復号するのは現実的でない（実測で `key_v1` は keystore の永続ストアに見つからなかった）。

代わりに、復号結果が JavaScript へ返る瞬間を捕まえる。`tools/token-extract/extract_token.py`
がそれを行う。実測で動く。

**前提**: root の取れた Android（エミュレーター可）と、frida。バージョンを揃える。

```bash
# 端末側
adb shell "/data/local/tmp/frida-server &"
adb forward tcp:27042 tcp:27042
adb shell /data/local/tmp/frida-server --version      # 例: 16.7.19

# 手元側（端末と同じ版を入れる）
pip install "frida==16.7.19"

# 抽出（アプリは起動しない状態にしておく）
python tools/token-extract/extract_token.py --out token.txt

# 取り込む
sakiyomi auth import-token --stdin < token.txt
```

捕まえる対象は `expo.modules.securestore.encryptors.AESEncryptor.decryptItem` の
戻り値だけ。鍵を扱う必要はない。

**実測で分かった落とし穴**（両方とも実際に踏んだ）:

- **バージョンを合わせないと動かない。** 端末の frida-server が 16.7.19 でクライアントが
  17.24.2 だと、`typeof Java` が `undefined` になり、フックが一切刺さらない。
- **アプリを起動した後では取れない。** トークンは起動時に復号されるので、後から
  attach しても復号経路を通らない。`spawn` で止めた状態でフックを刺してから再開する。
- **TLS 傍受では取れない。** このアプリはプロキシ設定を無視して 443 に直接つなぐ。
  実測では mitmproxy に接続が 1 件も来なかった。むしろプロキシ設定を入れると
  アプリの通信が壊れた（`通信できません`）。この経路は使わない。

実機では root か debuggable が必要で難易度は上がる。iOS は別の手法になる。この
スクリプトは Android の、しかも root が取れる環境だけを対象にしている。

## 読み取り

既定の出力は **JSON**。`--table` を付けると人間向けの表になる。

```bash
sakiyomi events list                     # JSON（プログラム向け）
sakiyomi events list --table             # 表（目で見る用）
```

表の例（列は実データから決まる。値は切り詰めない）:

```
id     category  subcategory           title                                    status  closes_at
-----  --------  --------------------  ---------------------------------------  ------  ------------------------
 6663  soccer    チャンピオンズリーグ  アーセナル vs ドルトムント、勝つのは?  open    2026-11-24T20:00:00.000Z
58936  sports    競馬                  競馬 ジャパンカップ(11/29)、日本馬が勝つ? open    2026-11-29T06:30:00.000Z
```

CJK は表示幅 2 として桁を合わせる。`{"events": [...]}` のように配列を 1 つだけ含む応答は、
表表示のときその配列に展開する。

### 使える読み取り

```bash
sakiyomi config                                   # 機能フラグ（認証不要）
sakiyomi me                                       # 自分のアカウント（要ログイン）

sakiyomi events list                              # 一覧（30 件 + next_ids）
sakiyomi events list --ids 44909,6663              # 指定 ID だけ（50 件超は自動で分割）
sakiyomi events list --stake 100                   # 各選択肢に buy_prob / shares を併記
sakiyomi events list --trial                      # trial_ui=1 を付ける
sakiyomi events show 44909                        # 1 件の詳細（{event, series}）
sakiyomi events history 44909                     # 確率の推移（既定 1w。--period 1d/1m/all）
sakiyomi events history 44909 --period 1d         # 直近 1 日
sakiyomi events live                              # ライブボード
sakiyomi events categories                        # カテゴリ 13 件
sakiyomi events subcategories soccer              # サブカテゴリ
sakiyomi events updown btc --live                 # updown（BTC 5分）のラウンドと現在価格

sakiyomi positions list                           # 自分のポジション（要ログイン）
sakiyomi positions list --tab result              # タブ指定（active / result）
sakiyomi quiz list                                # 自分のクイズと審査状況
sakiyomi quiz list --status approved              # 採用済みだけ
sakiyomi positions claim <position_id>            # 的中分の払い戻しを受け取る
sakiyomi positions claim all                      # 未受取の won をまとめて受け取る
sakiyomi missions list                            # ミッション（未認証でも読める）
sakiyomi ledger list                              # ポイント履歴（既定 coin、--currency point）
sakiyomi notices list                             # お知らせ
sakiyomi rankings list                            # 利益ランキング 50 件（既定 weekly）
sakiyomi users show <handle>                      # 他人のプロフィール
sakiyomi users positions <handle>                 # 他人のポジション
sakiyomi blocks list                              # ブロック一覧（要ログイン）
```

### フォロー・ブロック

```bash
sakiyomi follow <user_id>          # フォローする（POST）
sakiyomi follow <user_id> --off    # フォローを解除する（DELETE）
sakiyomi block <user_id>           # ブロックする（POST）
sakiyomi block <user_id> --off     # ブロックを解除する（DELETE）
```

`--dry-run` で送信メソッドを確認できる。既定の確認プロンプトは金銭操作と同じ。

## コインを増やす

デイリーボーナス、広告のコイン、ミニゲームのコイン、クイズの作成。どれも
ヘッドレスで動くかどうかを実測で分けた上で、動くものだけを実装している。

```bash
sakiyomi bonus status                    # 今日なにが受け取れるか（読み取り）
sakiyomi wave status                     # 同じ内容をゲーム側から
sakiyomi wave play --yes                 # 1 回プレイして、クリア分を受け取る
sakiyomi quiz --title "..." --option A --option B \
              --prob 60 --prob 40 --closes-at 2026-11-27T00:00:00.000Z --yes
sakiyomi missions list                   # 未受取のミッションを探す
sakiyomi claim <mission_id>              # ミッション報酬を受け取る
```

`bonus status` が出すもの（実測値）:

```
{
  "login_bonus_coins": 30,
  "login_bonus": { "day": "2026-11-27", "available": true, "received": false }
}
```

広告の本数・広告ユニット・視聴の成否は出さない。CLI は広告を出せないので、
出しても読めない値になる。`wave status` は同じ内容に、ミニゲームの当日分を
台帳から集計した `minigame` を足して返す。`wave play` の応答からも動画ボーナスの
項目（`video` / `video_coins` / `video_max` / `daily_cap` / `today.video_done`）は
落としている。

### デイリーボーナス

`login_bonus.available` が `true` の日は、missions の id 1
（ログインボーナス）を `claim` で受け取れる。受け取り済みなら
`409 このミッションは達成済みです` が返る。

```bash
sakiyomi missions list | grep -A1 '"kind": "login"'   # claimable を見る
sakiyomi claim 1 --yes
```

### updown（ビットコイン5分）ゲーム

`events updown` の引数は**イベント ID ではなく市場シンボル**です（実測で確定）。
バンドルはサブカテゴリから `{'ビットコイン': 'btc'}` で引きます（753033）。
イベント ID を渡すと、単体取得で subcategory を調べて同じ表で変換します。

```bash
sakiyomi events updown btc --live    # ラウンド一覧・現在価格・server_now
sakiyomi events updown btc           # 価格履歴（約30分、5秒間隔）
```

`--live` を付けると `rounds` が返ります（最大 12 件、`id` `status` `closes_at`
`participants` `baseline` を含む）。付けないと価格履歴だけです。`history` は
`[ms, price]` の配列で、`--live` では約 20 秒分しか返らないので、長い期間の
判定をしたいときは自分で蓄積してください。

ラウンドの板は `events show <round_id>` で読めます（`category=updown`、options は
「上がる」「下がる」）。ただし**一括取得 `GET /events?ids=` はラウンドを返さない**
（空配列）ので、`events list --ids` では取れません。

```bash
sakiyomi events updown btc --live | jq '.rounds[] | select(.status=="open") | .id'
# 出た ID で板を見て、そのまま買える
sakiyomi events show 97770
sakiyomi buy --event 97770 --option 119377 --stake 10 --dry-run
```
### 広告のコイン

**CLI は広告を出さないし、出せない。** コマンドからも広告の状態を消してある
（広告の本数・広告ユニット・視聴の成否は返さない）。理由は次のとおり。

理由は `GET /ads/reward` の形にある。この経路は状態を返すだけで、付与は
AdMob のサーバー側検証（SSV）コールバックが行う。アプリは
`RewardedAd.createForAdRequest` に `serverSideVerificationOptions` を渡して
`customData` を載せ、広告網からサーバーへ直接通知させる（バンドル 456403-456413）。
CLI から出せるのは nonce を付けた GET だけで、それでは付与されない。

実測で確かめた。`nonce` / `win_event` / `mg_play` を付けて何度叩いても
`used_today` は 0 のまま、コイン残高も動かない。`POST`・`PUT`・`PATCH` は
`/ads/reward` に対してすべて 404 で、付与の窓口は API 空間に存在しない。

SSV の受け口は `https://sakiyomi.app/ads/ssv` にあり、クエリ無しなら 200 `ok`、
パラメータを 1 つでも付けると 403 `forbidden` になる。認証ヘッダを付けても 403 で、
なりすませない。アプリが載せる `customData` は `wb` / `lb` / `mg` + `x` +
`randomUUID()` で、**署名もトークンも含まない**。視聴の証明は Google が署名で行い、
その検証鍵はサーバー側にしかない。

一方、アプリで実際に広告を最後まで再生すると付与は起きる（実測）:

```
前: used_today=0 remaining=5 coins=30
後: used_today=1 remaining=4 coins=40
台帳: video delta=10
```

したがって `bonus status` は「今いくらもらえるか」を読むだけのコマンドで、
受け取りはしない。API だけで付与を作る経路は存在しない。

**エミュレータ（または実機）の上でアプリを動かす必要はある。** ただし画面を
見ずに操作は自動化できる。`adb input tap` と `sleep` だけで、人手も画面も
使わずに 4 本連続で受け取れた（実測）。

```bash
adb shell am start -n app.sakiyomi.mobile/.MainActivity; sleep 14
adb shell input tap 270 1559   # ためる
sleep 7
adb shell input tap 754 1238   # 見る（動画視聴）
sleep 150                      # 再生完了を待つ
adb shell input tap 852 48     # Close（右上）
```

座標は 900x1600 の画面で測った値。`uiautomator dump` から
`bounds="[726,1217][782,1259]"`（見る）と `bounds="[828,24][876,72]"`（Close）を
動的に取れるので、固定値に頼らず実装できる。実測の推移:

```
開始: coins=0   used_today=1  remaining=4
 4本: coins=40  used_today=5  remaining=0   台帳に video +10 が 4 件
```

エミュレータを一切使わずに広告を再生する方法は無い（実測）。広告ユニットを Web
形式で叩くと空の HTML が返り配信されない。付与は Google の SSV 署名をサーバーが
検証して行うが、`/ads/ssv` はクエリ無しなら 200、何か付けると 403 `forbidden` で、
偽の署名では通らない。そして Android アプリのプロセスを動かす手段がこの環境には
他に無い（`adb devices` は MuMu のみ、WSA・WSL のディストロ・Android SDK の
いずれも未導入、USB 実機も無し）。

代わりに **オファーウォール**（`GET /api/offerwall`）という Web で完結する獲得経路が
ある。4 社（zucks / smaad / skyflag / ratel）の URL を返し、いずれもブラウザで
開ける。skyflag には実案件が並ぶ（例: アプリを入れてレベルを上げると最大
118,035 コイン）。ただし案件は人間の操作が前提で、ヘッドレスで自動化できるのは
閲覧まで。この経路での付与実績はまだ無い（台帳に該当する reason が無い）。

早送りはできない。再生中に `KEYCODE_BACK` を送っても広告は閉じず、完了時に
付与された。在庫切れのときは `Ad failed to load : 3`（`ERROR_CODE_NO_FILL`）が
返り、`見る` を押しても広告が開かない（待つと回復する）。再生には 90〜150 秒
かかるので、30 秒で判定すると取り違える。

### ミニゲームのコイン

**ヘッドレスで取れる。** 実装した。

サーバーはタップ時刻の列を受け取り、自分の持つレベルで再生して結果を決める。
だから画面を作る必要はなく、レベルからタップ列を計算すれば足りる。
`wave play` はそれをやる。

```bash
$ sakiyomi wave play --yes
round 11364: 8 taps, waiting 7s so the replay sees them in the past
ok: {
  "cleared": true,
  "best_row": 8,
  "falls": 0,
  "reward": { "kind": "free", "coins": 20 },
  ...
}
```

3 つの実測済みの落とし穴がある。

- **`rules_version` は数値で送る。** 文字列にすると
  `409 アプリを最新の版にしてから遊んでください` になる。1 と 2 が通り、3 は未公開として拒否される。
- **タップ時刻が未来だとクリアにならない。** 再生は「まだ来ていない時刻のタップ」を
  ミスとして扱う。`start` の直後に `finish` を送ると `best_row: 8` なのに
  `cleared: false` になる。最後のタップが過ぎるまで待つ必要があり、`wave play` は
  自動で待つ（`--wait` で上書きできる）。
- **同じ日に 2 回クリアしても 2 回目は無報酬。** `free_coins` は 1 日 1 回。
  `today.free_done` が `true` なら、もう受け取り済み。

タップ列の計算は `crates/sakiyomi-cli/src/wave.rs`。レベルは
`island x0 / width / period / speed / dir` だけなので、位置は
`((x0+PAD+dir·speed·t/1000) mod period) − PAD` で決まる。定数はすべてバンドルから
取り出してあり、モジュールの表に根拠の行番号を書いてある。

### クイズの作成

**ヘッドレスで作れる。** 実装した。ただし採択されるとポイントが動く。

```bash
sakiyomi quiz --title "波よみクイズ" \
              --option "上がる" --option "下がる" \
              --prob 60 --prob 40 \
              --closes-at 2026-11-27T00:00:00.000Z \
              --dry-run     # まず確認
```

`--prob` は省略できる（その場合 `prob` は null で送られ、確率未設定として扱われる）。
`--option` は 2 つ以上。採択時に `quiz_adopt_bonus`（実測 1000）が入る。

**取り消しはできない。** `DELETE /quizzes/{id}` も編集 API も無い（実測で 404）。
作ったものは審査に回り、却下されるまで残る。`--dry-run` で中身を確認してから送ること。

## 決めた時刻に自動で買う

`sakiyomi schedule` は買付を予約しておく機能です。予約はファイルに残り、
`sakiyomi schedule run` が常駐して時刻が来たら買います。CLI のプロセスを
起動し続ける必要があるのは `run` だけで、それ以外は 1 回で終わります。

```bash
# 予約する（JST 12:00 に、event 90241 の option 110224 へ 100 コイン）
sakiyomi schedule add --at 2026-12-01T12:00+09:00 \
                      --event 90241 --option 110224 --stake 100

sakiyomi schedule list         # 予約の一覧
sakiyomi schedule remove <id>  # 予約を取り消す（送信はしない）

# 常駐して待つ。Ctrl-C で止めてよい
sakiyomi schedule run

# 1 件だけ処理して終わる（cron から呼ぶ用）
sakiyomi schedule run --once
```

### 時刻の書き方

`--at` は `2026-12-01T12:00+09:00` のようにタイムゾーンを付けて書きます。
`Z` を付けると UTC です。ファイルには UTC で保存されるので、実行環境の
タイムゾーンが変わっても予約の時刻は動きません。

タイムゾーンを付けない書き方（`2026-12-01 12:00`）は、そのマシンのローカル時刻として
解釈します。ただし Windows ではローカルのタイムゾーンを取得する手段が無いため、
その場合は明示を促すエラー（exit 2）にしています。

### 予約と買付の通貨

`--stake` は**コイン**です。サーバーは `coins` を引きます（ポイントではありません）。
この点は以前「ポイント」と書いていましたが、実測で誤りが分かったので直しました。

### 落ちたときの扱い

`run` は送信の**前**に「送信中」の印をファイルへ書きます。途中で落ちた場合、
その予約は `in_flight` のまま残り、次回の `run` でも自動では再送しません。
サーバーが受け取っている可能性があるためです。状態を自分で確認したあと、
次のどちらかで確定させてください。

```bash
sakiyomi schedule reconcile <id> --done    # サーバーが受け取っていた
sakiyomi schedule reconcile <id> --retry   # 送られていなかった（再送する）
```

送信が「確実に届かなかった」場合は自動で `not_sent` になり、次回 `run` で再試行します。

### 予約できる範囲

`--stake` は 10〜100,000 コインです。サーバーがこの範囲外を
`コインは10〜100,000の範囲で入力してください` で拒否するため、`schedule add` の
時点で同じ境界を検査します。範囲外を受け付けて後で失敗するより、
オペレーターがその場にいるときに止めるほうが安全です。
## 金銭が動く操作

**ここは読んでから使う。**

```bash
sakiyomi buy --event <event_id> --option <option_id> --stake <points>
sakiyomi sell <position_id>
sakiyomi claim <mission_id>
sakiyomi positions claim <position_id> [--currency point|coin]
sakiyomi positions claim all [--keep-going]
sakiyomi schedule add --at <time> --event <event> --option <option> --stake <coins>
sakiyomi schedule run
sakiyomi transfer --to <user> --amount <n> [--message "..."]
sakiyomi invite-claim <code>
sakiyomi gift-exchange --batch <id> --items '[{"id":"g1"}]'
```

### まず --dry-run

送信内容を表示して終わる。**買付系の `--dry-run` は送信内容に加えて、買付後の
`buy_prob` と `shares` の推定も表示する**（一覧の `prob` は買付価格ではない）。
推定のため対象イベントを 1 回 GET するが、買付そのものは送らない。
セッションが無くても動く。

```bash
$ sakiyomi buy --event 44909 --option 111516 --stake 100 --dry-run
POST https://sakiyomi.app/api/events/44909/buy
headers:
  content-type: application/json
  x-option-colors: 8
  x-prob-display: 100
body:
{
  "option_id": 111516,
  "stake": 100
}
note: no session token; this request would be rejected with 401
dry-run: nothing was sent
```

送信前にこれを必ず一度見る。`option_id` が数値になっていること、`stake` が意図した値であることを確認できる。

### 実行時は確認プロンプト

`--yes` を付けない限り、`yes` と打たないと実行されない。`y` でも `YES please` でも通らない。

```
$ sakiyomi buy --event 44909 --option 111516 --stake 10
About to buy into 44909:
  111516 x 10 points on event 44909
Type 'yes' to continue: yes
ok: {...}
```

パイプや cron から呼ぶときは端末が無いので、`--yes` を明示する必要がある。

### 失敗したときの終了コード

| コード | 意味 | どうする |
|---|---|---|
| 0 | 成功 | — |
| 1 | サーバーが拒否した（残高不足、締切済みなど） | 本文を読んで判断 |
| 2 | 引数の誤り | 直す |
| 3 | 確認プロンプトで中断した | — |
| 4 | **送信されていない** | 原因を直して再試行してよい |
| 5 | **結果不明**（応答が来ないまま切れた） | 再試行するな。残高とポジションを先に確認 |
| 6 | 認証が無い／切れた | 再ログイン |

4 と 5 の区別が要点。5 のときは**サーバー側で処理済みかもしれない**ので、
同じコマンドを再実行すると二重購入になり得る。まず `positions list` と `ledger list` を見る。

```
$ sakiyomi buy --event 1 --option 2 --stake 10
OUTCOME UNKNOWN — the request may have been processed (...).
Do NOT retry blindly: check the position/balance first.
```

リクエストは 1 回しか送らない。転送層にリトライは無い。

## 設定ファイル

置き場所（OS 標準）:

| 項目 | Windows | Linux / macOS |
|---|---|---|
| 設定 | `%APPDATA%\sakiyomi\config.toml` | `$XDG_CONFIG_HOME/sakiyomi/config.toml`（未設定なら `~/.config/sakiyomi/`） |
| セッション | 同ディレクトリの `session.json` | 同左 |

中身は必要な行だけ書けばよい。

```toml
base = "https://sakiyomi.app/api"   # 接続先を変えたいときだけ
timeout_secs = 30
token = "..."                       # 端末で確立済みのトークンを固定したいとき
app_version = "1.0.42"              # サーバーが版を見る場合だけ
```

別の場所を使うときは `--config` / `--state`、または `SAKIYOMI_CONFIG` / `SAKIYOMI_STATE`。

どの層から値が来たかは `--verbose` で分かる:

```bash
$ sakiyomi --verbose config
base           <- default
token          <- default
state          <- env
```

## MCP（AI から使う）

```bash
sakiyomi mcp
```

stdio の JSON-RPC。`initialize` / `tools/list` / `tools/call` を実装している。ツールは 22 個:

```
doctor, config, me, events_list, event_history, live_board,
positions, position_claim, missions, ledger, rankings, notices,
quizzes_mine, bonus_status, wave_play, quiz_create,
schedule_list, schedule_add, schedule_remove,
buy, sell, claim
```

`schedule run` は常駐するため MCP には出していません。予約の追加と確認だけを
MCP から行い、実行は CLI 側で `sakiyomi schedule run` を起動します。

状態やポイントが動く 6 つ（`buy` / `sell` / `claim` / `position_claim` / `wave_play` /
`quiz_create`）は `confirm: true` が必須。付けなければエラーになり、`dry_run: true` なら
送信内容だけ返す。`position_claim` は `{"position":"all"}` で未受取をまとめて処理する。

```json
{"name": "buy", "arguments": {"event": "44909", "option": "111516", "stake": 100, "confirm": true, "dry_run": true}}
```

失敗の返し方は 2 種類。AI 側の扱いが変わるので分けている。

- ツールが走って失敗（401、5xx、ネットワーク断）→ `isError: true` と本文
- 引数不正・ツール不在 → JSON-RPC エラー `-32602`

ツール呼び出しは別スレッドで走るので、遅い HTTP が `ping` や `tools/list` を止めない。

## 出力を絞り込む

JSON のまま `jq` に渡すのが基本。

```bash
sakiyomi events list | jq '.events[] | {id, title, closes_at}'
sakiyomi rankings list | jq '.rankings[0:3]'
sakiyomi events show 44909 | jq '.event.options'
```

## 知っておくべき制限

- **入金（チャージ）は実装していない。** アプリの `/charge` は画面遷移でしかなく、対応する HTTP 経路が
  バンドルに存在しないため。存在しない API を推測で叩くことはしない。
- **`events updown` は実質使えない。** 引数がイベント ID かどうかが解析から確定できず、
  実測ではどの ID でも 404 になる。コマンドは残しているが 404 時は「引数が未検証」と表示する。
- **認証後の経路は実測できていない。** 検証時に実アカウントを用意していないため、
  Google ログイン後の挙動は解析（バンドル）に基づく。実際の口座で試すときは `--dry-run` から始める。
- **Google ログインの最後の一歩は未検証。** code の受け取り（OS 経由の callback 起動）と
  `/auth/social` への `id_token` 送信までは実測したが、本物の Google アカウントで最後まで
  通してはいない。手元で試すときは `--dry-run` から始める。
- サーバー側の認可・レート制限は未検証。他人のポジションを売れるか等は調べていない。

## 困ったとき

| 症状 | 見るところ |
|---|---|
| `verdict: problems found` | `doctor` の `FAIL` 行。`--base` が正しいか、ネットワークか |
| 全部 401（exit 6） | `auth status` でトークンが入っているか。期限切れなら再ログイン |
| `state error: ...` | `session.json` が壊れている。消すか `--state` で別パスを指定 |
| 表が横に長すぎる | 既定の JSON に戻して `jq` で絞る |
| 何が送られるか分からない | すべてのコマンドに `--dry-run` が付く |

## 免責

非公式ツール。サキヨミ運営とは無関係。解析はリリース済み APK の静的解析のみ。
ポイントが動く操作は自分の責任で。
