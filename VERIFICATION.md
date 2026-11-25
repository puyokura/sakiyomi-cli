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
- **`/minigame/wave/*` の `rules_version`** 実値。定数モジュールから来ており復元できない。
- **`/transfers`** は `transfer_enabled=false` のため実測不能。

## 5. 実装がバンドルと意図的に違う点

| 箇所 | バンドル | 実装 | 理由 |
|---|---|---|---|
| 素の通信断 | 生のエラーを上位へ流す | `TransportError::Network` に分類 | CLI では「送信されていない」と「結果不明」を区別する必要がある |
| タイムアウト | `ApiError(0, ...)` | phase 付き。connect/resolve は「未送信」、それ以外は「結果不明」 | 金銭操作の再試行可否が変わる |
| `?trial_ui=1` on `/events/{id}` | 付ける | 付けない | 実測で 404 |
| `/updown` | サブカテゴリ表引き | イベント ID を受ける | 引数が不明なため。404 時に「引数が未検証」と明示する |
