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

サキヨミには**メール+パスワードのログインが無い**。アプリの最初の画面にあるのは
「Appleで続ける」「Googleで続ける」の 2 つだけで、CLI もそれに合わせて Google でログインする。

```bash
sakiyomi auth login
```

URL が 1 行で表示され、ブラウザが開く。Google の画面でログインすると、このコマンドは
**自動で終了する**。code をコピーして貼る操作は無い。

```
https://accounts.google.com/o/oauth2/v2/auth?client_id=510209981354-...

この URL をブラウザで開いて Google にログインしてください。
ログインが終わると、このコマンドは自動で終了します。
logged in with Google (token stored in ...)
```

成功すると `session.json` にトークンが保存され、以降のコマンドが自動で使う。
トークンの値は表示されない。

```bash
sakiyomi auth status
```

### 戻ってくる仕組み

アプリのリダイレクト先は `app.sakiyomi.mobile:/oauthredirect` というカスタムスキーム
（バンドル 702228-702234）。Google は loopback も LAN IP も https も OOB も受け付けない
（すべて実測で拒否）。そのため、このスキームのハンドラを OS に登録する。

`auth login` が自動で登録し、ブラウザがリダイレクトすると OS が
`sakiyomi auth callback "<URI>"` を起動する。待機中のプロセスが結果を受け取って終了する。
既存の登録は上書きされ、元には戻らない。`auth unregister` で消せる。

```bash
sakiyomi auth register      # 明示的に登録
sakiyomi auth unregister    # 登録を消す
```

### サーバー・SSH 先でログインする（ヘッドレス）

ブラウザが無いマシンでも同じ流れで完結する。URL をブラウザのあるマシンで開き、
そのマシンから待機中のマシンへリダイレクトを転送する。

```bash
# サーバー側（待機するマシン）
sakiyomi auth login
# → URL が出て、127.0.0.1:8765 で待ち受ける
```

ブラウザのあるマシンで URL を開くと `app.sakiyomi.mobile:/oauthredirect?...` に
リダイレクトされる。そのマシンにも CLI を置き、転送先を指定して登録する。

```bash
# ブラウザ側のマシン（1 回だけ）
SAKIYOMI_RELAY=http://<サーバー>:8765/code sakiyomi auth register
```

以後はブラウザがリダイレクトした瞬間、OS がハンドラを起動し、その CLI が URI を
サーバーへ POST する。サーバー側の `auth login` は完了してトークンを保存する。
**人が code を打つ場面はどこにも無い。**

SSH 越しなら、ポート転送だけで足りる。

```bash
ssh -L 8765:127.0.0.1:8765 <server>
# サーバー側で: sakiyomi auth login
# ローカル側で: sakiyomi auth register --relay 127.0.0.1:8765 → URL を開く
```

待ち受けは環境変数で変えられる。

| 変数 | 用途 | 既定 |
|---|---|---|
| `SAKIYOMI_RELAY_BIND` | 待ち受けるアドレス | `127.0.0.1:8765` |
| `SAKIYOMI_RELAY` | 転送先（`auth register` に焼き込む） | なし |
| `SAKIYOMI_NO_RELAY` | `1` で待ち受けを止める | なし |
| `SAKIYOMI_BROWSER` | ブラウザ起動コマンド | OS 既定 |

### 登録先と差し替え

| 環境変数 | 既定 (Windows) | 既定 (Linux) |
|---|---|---|
| `SAKIYOMI_SCHEME_ROOT` | `HKCU\Software\Classes` | — |
| `SAKIYOMI_DESKTOP_DIR` | — | `~/.local/share/applications` |
| `SAKIYOMI_XDG_MIME` | — | `xdg-mime` |
| `SAKIYOMI_DESKTOP_DATABASE` | — | `update-desktop-database` |

### 確認だけしたいとき

```bash
sakiyomi auth login --dry-run    # 何もせず、何をするかの説明だけ
```

### 端末で確立済みのトークンを使う




アプリやエミュレーターですでにログイン済みなら、そのトークンを直接注入できる。こちらが主という運用でも構わない。

```bash
# 1 回だけ使う
sakiyomi --token "$TOKEN" positions list

# シェルの間ずっと使う
export SAKIYOMI_TOKEN="$TOKEN"     # PowerShell: $env:SAKIYOMI_TOKEN = "..."
sakiyomi positions list
```

優先順位は `--token` > `SAKIYOMI_TOKEN`（または設定ファイルの `token`）> 保存済みセッション。
`--token` は保存しないので、使い捨てにできる。

トークンの値はどこにも表示されない（`auth status` が出すのは有無と文字数だけ）。

### ログアウト

```bash
sakiyomi auth logout            # サーバーに POST /logout してからローカルも消す
sakiyomi auth logout --forget   # サーバー呼び出しが失敗してもローカルを消す
```

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
sakiyomi events list --ids 44909,6663              # 指定 ID だけ
sakiyomi events list --trial                      # trial_ui=1 を付ける
sakiyomi events show 44909                        # 1 件の詳細（{event, series}）
sakiyomi events history 44909                     # 確率の推移（既定 1w。--period 1d/1m/all）
sakiyomi events history 44909 --period 1d         # 直近 1 日
sakiyomi events live                              # ライブボード
sakiyomi events categories                        # カテゴリ 13 件
sakiyomi events subcategories soccer              # サブカテゴリ

sakiyomi positions list                           # 自分のポジション（要ログイン）
sakiyomi positions list --tab closed              # タブ指定
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

## 金銭が動く操作

**ここは読んでから使う。**

```bash
sakiyomi buy --event <event_id> --option <option_id> --stake <points>
sakiyomi sell <position_id>
sakiyomi claim <mission_id>
sakiyomi transfer --to <user> --amount <n> [--message "..."]
sakiyomi invite-claim <code>
sakiyomi gift-exchange --batch <id> --items '[{"id":"g1"}]'
```

### まず --dry-run

送信内容を表示して終わる。ネットワーク I/O はしない。セッションが無くても動く。

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

stdio の JSON-RPC。`initialize` / `tools/list` / `tools/call` を実装している。ツールは 14 個:

```
doctor, config, me, events_list, event_history, live_board,
positions, missions, ledger, rankings, notices,
buy, sell, claim
```

金銭系の 3 つは `confirm: true` が必須。付けなければエラーになり、`dry_run: true` なら送信内容だけ返す。

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
