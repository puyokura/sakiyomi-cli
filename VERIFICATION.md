# 実測記録

このファイルは、実装が**実際に動かした結果**だけを残す。主張と実測を分ける。

- 検証環境: Windows 11 (build 26300)、rustc/cargo 1.96.0 (x86_64-pc-windows-gnu)、
  `sakiyomi 0.1.0`、対象 `https://sakiyomi.app/api`
- 実 API に対して実行した。モックではない。
- 未認証で実行した。認証情報は提供されていないため、**認証後の経路は未検証**。

## 1. 未認証で実測できたもの

| コマンド | 結果 | 備考 |
|---|---|---|
| `doctor` | exit 0, verdict ready | `/config` 200、`/me` 401 を確認 |
| `config` | 200 | `auth_open` `social_open` `google_login_ready` `transfer_enabled` `claim_auto_days` `quiz_adopt_bonus` |
| `events list` | 200, 30 件 | `events` + `next_ids`（295 件） |
| `events list --table` | 24 列 30 行の格子 | 値の切り詰めなし |
| `events show 44775` | 200 | 締切済みイベントでも `{event, series}` を返す |
| `events show 99999999` | 404 → exit 1 | `{"error":"イベントが見つかりません"}` |
| `events history 44775` | 200, 360 点 | `{as_of, history:[{option_id,prob,created_at}]}` |
| `events history 44775 --period 7d` | 200, 93 点 | period が効いている |
| `events live` | 200 | `{"at":...,"events":[]}` |
| `events categories` | 200, 13 件 | `{key,label}` |
| `events subcategories soccer` | 200, 12 件 | |
| `missions list` | 200, 6 件 | **未認証で読める** |
| `notices list` | 200, 6 件 | 本文に改行を含む |
| `rankings list` | 200, 50 件 | `period=weekly` が既定で返る |
| `positions list` | 401 → exit 6 | 認証必須 |
| `ledger list` | 401 → exit 6 | 認証必須 |
| `blocks list` | 401 → exit 6 | 認証必須 |
| `me` | 401 → exit 6 | 認証必須 |
| `users show <unknown>` | 404 → exit 1 | `{"error":"ユーザーが見つかりません"}` |
| `events updown <id>` | 404 → exit 1 | **引数が未検証**（下記 §4） |

## 2. 実測で確定したサーバー挙動

- `POST /login` に `content-length: 0` + `content-type: application/json` → **400**
  「うまく送信できませんでした」。`{}` を送れば 401「メールアドレスまたはパスワードが違います」。
  空 body に JSON の content-type を付けてはいけない（実装は `send_empty()` で分岐）。
- `GET /events?ids=44775` → 200 で `{"events":[]}`。締切済みのイベントは一覧系から消える。
  同じ ID でも `GET /events/44775` は 200 で返る。**単体取得と一覧取得で意味が違う。**
- `GET /events/{id}?trial_ui=1` → **404**。バンドルは trial 時にこのクエリを付けるが
  （497957-497966）、実サーバーは単体取得では拒否する。実装は付けない。
- `GET /events?trial_ui=1` → 200、応答 24,831 → 23,923 バイト。差はあるが内容の意味は未確定。
- `?ids=` はカンマ区切りで複数指定でき、存在するものだけ返る
  （`ids=6663,44775` → `[6663]`）。

- **トークン抽出（実測で成功）**。root の取れた MuMu エミュレーター（Android 14, x86_64）で
  frida-server 16.7.19 と frida クライアント 16.7.19 を揃え、`spawn` 後に
  `expo.modules.securestore.encryptors.AESEncryptor.decryptItem` の戻り値をフックして
  取得した。戻り値は 64 文字で、2 回の実行で同一。得た値を `auth import-token` に渡すと
  `verified: true`、`GET /me` 200、`positions` と `doctor` も通る。
  バージョン不一致（サーバ 16 / クライアント 17）では `typeof Java` が `undefined` になり
  フックが刺さらないこと、起動後に attach すると復号が済んでいて取れないことも実測した。
- **TLS 傍受は成立しない（実測）**。エミュレーターからホストへの TCP は通るのに、アプリは
  プロキシ設定を無視して 443 へ直接接続し、mitmproxy には 1 件も来なかった。むしろ
  プロキシを設定するとアプリの通信が壊れた（`通信できません`）。

- **デイリーボーナス / 広告 / ミニゲーム / クイズ（実測）**。
  `POST /missions/{id}/claim` はヘッドレスで通り、`claim 1` 相当が
  `{"ok":true,"reward_coins":100,...}` を返すことを確認（受取済みなら 409）。
  `GET /ads/reward` は状態を返すだけで、付与は AdMob の SSV が行う
  （バンドル 456403-456413 で `serverSideVerificationOptions` を渡している）。
  `nonce` / `win_event` / `mg_play` を付けても `used_today` は 0 のまま残高も動かず、
  `POST /ads/reward` は 404。**広告コインはヘッドレスで取れない。**
  `POST /minigame/wave/start` は `rules_version` を数値で送る必要があり（文字列は 409）、
  レベルから計算したタップ列で `cleared: true` と `reward: {kind: free, coins: 20}`
  を得た。台帳にも `reason=minigame / detail=波よみジャンプ (クリア) / delta=20` が記録された。
  同日 2 回目のクリアは `reward: null`（`free_coins` は 1 日 1 回）。
  `POST /quizzes` は 2 つの経路（生 HTTP と CLI）で 200
  `{"ok":true,"quiz_adopt_bonus":1000}`。**削除・編集 API は無い**（`DELETE /quizzes/{id}` と
  その POST はいずれも 404）。

## 3. 金銭操作の安全弁（実測）

`--dry-run` はネットワーク I/O をしない。閉じたポートを base にしても exit 0 で送信内容を表示する。

```
$ sakiyomi buy --event 44775 --option 59383 --stake 100 --dry-run
POST https://sakiyomi.app/api/events/44775/buy
headers:
  content-type: application/json
  x-option-colors: 8
  x-prob-display: 100
body:
{
  "option_id": 59383,
  "stake": 100
}
note: no session token; this request would be rejected with 401
dry-run: nothing was sent
exit=0
```

- トークンは `Bearer ***` に置換される。値は出ない。
- `option_id` は JSON 数値。バンドルが数値 ID をそのまま入れているため（665770-665775）。
- `--yes` 無しでは stdin から `yes` の完全一致が要る。`y` / `no` / `yes please` / EOF は中断。
- `MoneyGuard::run(self)` が所有権を消費するので、二重送信はコンパイルエラーになる。
- 転送層にリトライは無い。

## 4. 未検証（明示）

- **認証後の全経路**。実アカウントでの確認は行った（`GET /me` 200、`coins`/`points`、
  `positions`、`ledger`、`missions`、`notices`、`blocks`、`rankings`、`claim`、`buy`、`sell`
  の実測を含む）。ただし網羅ではない。`transfer` / `gift-exchange` / `invite-claim` は
  ポイントが動くため `--dry-run` のみで実行していない。
- **トークン抽出は、root の無い端末・実機・iOS では未検証。** 検証したのは root の
  取れた MuMu エミュレーター（Android 14, x86_64）だけ。
- **OAuth はこの CLI には無い。** Google の認可コードはカスタムスキーム
  `app.sakiyomi.mobile:/oauthredirect` に返り、Google はこのクライアントについて
  loopback / LAN IP / https / OOB のいずれも拒否する（4 種とも実測）。したがって
  CLI 単体で認可コードを受け取る経路が存在しない。トークンはアプリ側で確立したものを
  `auth import-token` で取り込む。取り込み時に `GET /me` で検証し、401 なら保存しない
  （偽トークンで exit 6・ファイル未生成を実測）。
- **`/updown/{X}` の引数**。バンドルの唯一の呼び出しは `useUpDownChart(r15, ...)` で、
  `r15` は `source === 'updown' ? SLOT18[subcategory] : null`（749599-749622）から来る。
  SLOT18 は期間セレクタの配列（744613-744622）であり、イベント ID ではない。
  実測では 44775 / 44909 / 6663 / 25142 / 1 のすべてが 404。**引数の正体は不明。**
- **サーバー側の認可**。他人の position を sell できるか等は未検証。
- **レート制限**。
- **`/minigame/wave/*` の `rules_version`** の実値は定数モジュールから復元できなかった。実測では 1 と 2 が通り、3 は 409 で拒否される。既定は 2。
- **`/transfers`** は `transfer_enabled=false` のため実測不能。

## 5. 実装がバンドルと意図的に違う点

| 箇所 | バンドル | 実装 | 理由 |
|---|---|---|---|
| 素の通信断 | 生のエラーを上位へ流す | `TransportError::Network` に分類 | CLI では「送信されていない」と「結果不明」を区別する必要がある |
| タイムアウト | `ApiError(0, ...)` | phase 付き。connect/resolve は「未送信」、それ以外は「結果不明」 | 金銭操作の再試行可否が変わる |
| `?trial_ui=1` on `/events/{id}` | 付ける | 付けない | 実測で 404 |
| `/updown` | サブカテゴリ表引き | イベント ID を受ける | 引数が不明なため。404 時に「引数が未検証」と明示する |
