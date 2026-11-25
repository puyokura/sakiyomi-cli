# サキヨミ (app.sakiyomi.mobile) APK 解析レポート

対象: `app.sakiyomi.mobile` versionName 1.0.42 / versionCode 111
解析日: 2026-11-24
取得元: Google Play（MuMu Player 上の Play ストア経由で `/data/app` から吸い出し）

このレポートは、実際に取り出した APK 4 本を apktool・jadx・Hermes 逆コンパイラ・ELF パーサに通して
得た結果だけを書いている。確認できた事実と、確認できていない推測は章を分けてある。
数値はすべて実行ログから取っており、推定値は使っていない。

---

## 1. 成果物の全体像

アプリは Expo SDK 57 / React Native（New Architecture, Fabric）製で、画面とロジックの大半は
JavaScript（Hermes バイトコード）に入っている。Java/Kotlin 側は事実上グルーコードで、
`MainActivity` と `MainApplication` の 2 クラスだけが自前実装だった。

| 項目 | 値 |
|---|---|
| パッケージ | app.sakiyomi.mobile |
| 表示名 | サキヨミ |
| versionName / versionCode | 1.0.42 / 111 |
| minSdk / targetSdk / compileSdk | 24 / 36 / 36 |
| APK 構成 | base + split_config.ja + split_config.x86_64 + split_config.xhdpi |
| 合計サイズ | 95,990,187 バイト（約 91.5 MB） |
| DEX | 7 本（classes.dex 12.90 MB ほか、計約 57.4 MB） |
| smali クラス数 | 51,948 |
| jadx 復元 Java クラス数 | 27,018 |
| ネイティブ .so | 26 本（すべて x86_64 split 内） |
| JS バンドル | index.android.bundle 5,063,716 バイト（Hermes バイトコード v98） |
| JS 逆コンパイル結果 | 32,460,520 バイト / 834,313 行 |
| JS 文字列テーブル | ユニーク 14,199 件（うち日本語 667 件） |

技術スタックの内訳は、Expo SDK 57（`app.config` の `sdkVersion`）+ React Native（New Architecture, Fabric）+
Expo Modules + expo-router（ファイルベースルーティング）+ React Query + Reanimated/Worklets +
react-native-webview + react-native-svg + Glide + Firebase Messaging + Google Mobile Ads（AdMob）+
Play Core。React Native の正確なバージョン番号はバンドルからは特定できなかった（`reactNativeVersion`
は他モジュール経由の getter で、値を直接持っていない）。

加えて **Google PairIP**（`com.pairip.application.Application`）が入っており、起動時に
`LicenseClient.checkLicense()` を `attachBaseContext` で呼ぶ。Play 配布アプリのライセンス改ざん
検知としては標準的な構成。

---

## 2. 署名と完全性

4 本すべてが同一証明書で署名されており、v2 と v3 の両スキームを持つ。v1（JAR 署名）は無い。
split 間で署名者が食い違うと `install-multiple` が失敗するので、これは実務上重要な確認点だった。

```
署名証明書 SHA-256: 4B 29 0F CD 5C 63 54 D8 6C F7 7E 76 8F B2 7D E5
                    2A E0 D5 A9 A4 30 E5 02 D7 27 61 4B 94 7D 8F F2
subject: C=US, ST=California, L=Mountain View, O=Google Inc., OU=Android, CN=Android
署名方式: v2 = true, v3 = true, v1 = false
```

subject が Google の Android 汎用証明書になっているのは、Play App Signing で再署名された
配布 APK だからで、開発者固有の情報は含まれない。吸い出した 4 本の SHA-256 は端末上の実ファイルと
一致し、`adb install-multiple` で再インストールして `Success` を確認済み。

各 APK のハッシュとサイズは `analysis/signatures.json` に全件記録してある。

---

## 3. MANIFEST の精査

### 3.1 権限（38 件）

危険権限として目立つのは次の 5 つ。いずれも宣言されているだけで、実際に要求されるのは
利用機能に触れたときだが、宣言自体は監査対象になる。

- `SYSTEM_ALERT_WINDOW` — 他アプリの上に重ねて描画できる。Expo の開発者メニュー由来で
  混入することもある権限で、製品ビルドに残っているのは望ましくない。
- `RECORD_AUDIO` / `CAMERA` — 音声・カメラ。`app.config` の permissions にも明示されている。
- `READ_EXTERNAL_STORAGE` / `WRITE_EXTERNAL_STORAGE` — いずれも `maxSdkVersion=32` が付いており、
  Android 13 以降では要求されない。適切な絞り込み。
- `AD_ID` / `ACCESS_ADSERVICES_AD_ID` / `ACCESS_ADSERVICES_ATTRIBUTION` / `ACCESS_ADSERVICES_TOPICS`
  — 広告 ID と Privacy Sandbox 系。AdMob 導入に伴うもの。
- `RECEIVE_BOOT_COMPLETED` + `POST_NOTIFICATIONS` + `FOREGROUND_SERVICE` — 通知・再起動後の復帰。
  Expo Notifications の標準セット。

ランチャーバッジ用の権限（Samsung/HTC/Sony/Huawei/OPPO など 16 件）は
`me.leolin.shortcutbadger` 系ライブラリが一括宣言するもので、実害はないが権限一覧を膨らませる。

### 3.2 エクスポートされたコンポーネント

12 activity / 11 service / 12 receiver / 8 provider のうち、`exported=true` は 5 件。
権限で保護されていないのは `MainActivity` 1 件だけで、これはランチャー起動に必須なので妥当。

| 種別 | 名前 | 保護 |
|---|---|---|
| activity | app.sakiyomi.mobile.MainActivity | なし（ランチャー、deep link 受け口） |
| service | androidx.work.impl.background.systemjob.SystemJobService | BIND_JOB_SERVICE |
| receiver | androidx.work.impl.diagnostics.DiagnosticsReceiver | DUMP |
| receiver | com.google.firebase.iid.FirebaseInstanceIdReceiver | c2dm.permission.SEND |
| receiver | androidx.profileinstaller.ProfileInstallReceiver | DUMP |

provider 8 件はすべて `exported=false`。うち 4 件は `grantUriPermissions=true` の FileProvider で、
`RNCWebViewFileProvider` / `FileSystemFileProvider` / `ClipboardFileProvider` / `ImagePickerFileProvider` /
`CropFileProvider` が該当する。パス定義は `res/xml/*_provider_paths.xml` に分離されており、
外部ストレージ全域を晒すような設定にはなっていない。

`debridable` 属性や `debuggable` は付いていない。`extractNativeLibs=false`（so を APK 内から直接 mmap）
も modern な設定。

### 3.3 バックアップ

`allowBackup=true` だが、SecureStore だけはバックアップから除外されている。これは適切。

```xml
<!-- secure_store_backup_rules.xml -->
<full-backup-content>
    <include domain="sharedpref" path="." />
    <exclude domain="sharedpref" path="SecureStore" />
</full-backup-content>
```

`dataExtractionRules`（Android 12+）でも同じ除外が書かれている。ただし sharedpref 全体は
バックアップ対象なので、AsyncStorage に置いたデータ（後述の `session` 相当のキャッシュなど）は
クラウドバックアップに乗り得る。トークン本体は SecureStore 側にあるので致命ではない。

### 3.4 ディープリンク

3 系統のスキームを受け付ける。

- `sakiyomi://` （カスタムスキーム）
- `app.sakiyomi.mobile://`
- `https://sakiyomi.app/` （`autoVerify=true`、`/event/` の pathPrefix、`/`、`/register`）

App Links の検証を通す構成なので、`https://sakiyomi.app/event/<id>` を踏むとアプリが直接開く。
JS 側では `useLocalSearchParams` / `useGlobalSearchParams` が 33 箇所で使われており、
`/event/[id]`、`/user/[handle]`、`/legal/[doc]`、`/notices/[id]` がパラメータを受ける。
パラメータの検証はサーバー側の責務で、クライアント側では型変換以上のことはしていない。

---

## 4. OTA アップデート機構

Expo Updates が有効で、起動のたびに更新を確認する設定になっている。ここは解析上いちばん重要な箇所の
ひとつで、**アプリ本体を再インストールせずに JS を差し替えられる**経路だからだ。

```
EXPO_UPDATES_ENABLED                = true
EXPO_UPDATE_URL                     = https://ota.sakiyomi.app/manifest
EXPO_UPDATES_CHECK_ON_LAUNCH        = ALWAYS
EXPO_UPDATES_LAUNCH_WAIT_MS         = 0
EXPO_RUNTIME_VERSION                = 1.0.42
request headers                     = {"expo-channel-name":"production"}
ENABLE_BSDIFF_PATCH_SUPPORT         = true
CODE_SIGNING_METADATA               = {"keyid":"main","alg":"rsa-v1_5-sha256"}
CODE_SIGNING_CERTIFICATE            = (RSA 証明書, DER 735 バイト, SHA-256 99f30134...)
```

code signing が有効なので、`ota.sakiyomi.app` が侵害されても署名鍵なしでは任意コードを配れない。
これは正しい設計判断で、評価できる点。逆に言えば、**この RSA 秘密鍵の管理がアプリ全体の
完全性の単一障害点**になる。鍵が漏れれば、Play を通さずに任意の JS を全端末へ配布できる。

manifest エンドポイントは外部から 3 通りのヘッダ構成（ヘッダ無し / `expo-channel-name` +
`expo-platform` + `expo-runtime-version` + `expo-protocol-version` / 加えて
`Accept: application/expo+json`）で叩いたが、いずれも 406 で内容は取得できなかった。
したがって「未認証では manifest を返さない」ところまでは再現性をもって確認できたが、
それ以上の挙動は未検証。

なお `assets/expo-root.pem` は Expo のルート証明書（有効期限 2042-03-01）で、これは
Expo の標準同梱物。

---

## 5. 通信と API 面

### 5.1 ベース URL とヘッダ

クライアントは `https://sakiyomi.app/api` を基点にしている。リクエスト組み立ては
1 箇所の `request` 関数に集約されていて、ヘッダは次の形で生成される（逆コンパイル結果から復元）。

```js
headers = {
  'content-type': 'application/json',
  'x-option-colors': String(OPTION_COLORS_KNOWN),
  'x-prob-display': '100',
  ...(token ? { authorization: 'Bearer ' + token } : {})
}
```

ヘッダ名は小文字の `authorization`、値は `'Bearer '` とトークンの連結。認証は Bearer 方式で、
Cookie は使っていない。`x-prob-display: '100'` はクライアントが固定値を送っているだけで、
サーバーがこれをどう扱うかは未検証。

### 5.2 エンドポイント一覧

バンドルから復元したパスは 87 件。うち HTTP メソッドまで特定できたものが 24 件。
以下は金銭・認証・課金に関わる重要度の高いもの。

| メソッド | パス | 用途（クライアント側の文脈から） |
|---|---|---|
| POST | /login | メール+パスワードログイン |
| POST | /auth/social | ソーシャルログイン（Apple / Google） |
| POST | /sms/send | SMS 認証コード送信 |
| POST | /sms/verify | SMS コード検証 |
| POST | /buy | イベントのポジション購入 |
| POST | /sell | ポジション売却 |
| POST | /claim | ミッション報酬受け取り |
| POST | /gifts/exchange | ギフト交換（Amazon ギフト等） |
| POST | /transfers | 送金・移転 |
| POST | /invite/claim | 招待コード適用 |
| POST | /minigame/wave/start | ミニゲーム開始（play_id 発行） |
| POST | /minigame/wave/finish | ミニゲーム結果送信（taps 数） |
| POST | /result-request | 結果訂正申請 |
| POST | /ads/reward | 広告視聴リワード |
| POST | /profile/privacy | 公開範囲設定 |
| GET? | /account/delete | アカウント削除 |
| GET? | /api/track | 行動トラッキング |
| GET? | /api/client-log | クライアントログ送信 |

`/buy` と `/sell` は本文から ID を組み立てる形（`/events/{id}/buy`、`/positions/{id}/sell`）で、
`/claim` は `/missions/{id}/claim`。REST らしい設計になっている。

### 5.3 実際に叩いて確認したこと

未認証で 2 本だけ実行した。認証が要るものは触っていない。

```
GET /api/config  -> 200
{"auth_open":false,"social_open":true,"google_login_ready":true,
 "transfer_enabled":false,"claim_auto_days":14,"quiz_adopt_bonus":1000}

GET /api/me      -> 401
```

`/api/config` が機能フラグを未認証で返すのは意図的な設計で、`transfer_enabled=false` から
送金機能が現在オフであることがわかる。`claim_auto_days=14`、`quiz_adopt_bonus=1000` といった
パラメータもサーバー側で制御されている。つまり**金銭に関わるパラメータはクライアントに
ハードコードされていない**。これは良い設計で、クライアントを書き換えても報酬額は変えられない。

### 5.4 秘密情報の不在

バンドルと Java ソースの両方をパターン走査した結果、次のものは **0 件**だった。

- Google API キー（`AIza...`）
- AWS アクセスキー（`AKIA...`）
- Stripe 本番キー（`sk_live_...` / `pk_live_...`）
- PEM 秘密鍵
- 20 文字以上の Bearer トークン直書き

クライアントに埋まっているのは Google OAuth のクライアント ID 2 件
（`510209981354-9v76...` と `510209981354-85oj...`）と AdMob のアプリ ID
（`ca-app-pub-5666429946941403~1387485234`）だけで、これらは公開前提の識別子。

---

## 6. 認証とトークン管理

トークンは `expo-secure-store` に保存される。キーは 2 つ。

```
'session_token'   … セッショントークン本体
'auth_provider'   … どの認証手段で入ったか（apple / google）
```

SecureStore は Android では Keystore 由来の鍵で暗号化して SharedPreferences に置く実装で、
上記のとおりバックアップからは除外されている。アクセシビリティ定数
（`AFTER_FIRST_UNLOCK` / `AFTER_FIRST_UNLOCK_THIS_DEVICE_ONLY` / `WHEN_UNLOCKED` など）は
SecureStore モジュール内に定義されているが、`keychainAccessible` を指定して呼んでいる箇所は
バンドル内に無かった。つまり既定値で保存しており、どの定数がそれに当たるかは
Expo 側の実装依存で、このバンドルからは決められない（`keychainAccessible` の出現 0 件）。

ログアウト時は `deleteItemAsync` で両キーを消す実装になっている。トークンの取得は
`getToken` / `peekToken` / `setToken` / `clearToken` の 4 関数に集約されており、
画面コンポーネントが直接 SecureStore を触る箇所は無い。凝集度は高い。

`/auth/social` に送る payload は provider ごとに形が違う。実装が確認できたのは 2 種類。

```js
{ provider: 'apple',  idToken, nickname }   // expo-apple-authentication の signInAsync
{ provider: 'google', idToken }             // OIDC（authorization code + PKCE）
```

Google 側は OIDC で `code_verifier` を伴う認可コードフローを使い、得た idToken をサーバーに渡して
自前セッションに交換する二段構え。`device_key` は端末識別子で、`device_key_sent_user` という
フラグで送信済み管理をしている（1 ユーザー 1 端末の紐付けと推測）。
招待コードは `{code, invite_v2:true, promo_v1:true, via}` を `/invite/claim` に送る。

なお UI には LINE のロゴと共有リンク（`https://line.me/R/share?text=`）があるが、
LINE の OAuth 実装（`access.line.me` 等）はバンドル内に存在しなかった。LINE はログイン手段ではなく
共有先としてだけ使われている可能性が高い（未確定）。

---

## 7. ネイティブ層

26 本の .so はすべて x86_64 で、ELF ヘッダを直接読んだ結果 **26/26 が PIE + NX スタック +
Full RELRO** だった。コンパイラ既定の hardened 設定がそのまま効いている。

サイズ上位は `libreactnative.so` 6.77 MB、`libhermesvm.so` 2.47 MB、`libappmodules.so` 2.06 MB、
`libavif_android.so` 1.81 MB、`libreanimated.so` 1.47 MB。自前のネイティブコードは無く、
すべて RN / Expo / 画像デコーダの依存ライブラリ。

注意点として、**base.apk には .so が 1 本も入っていない**。ネイティブライブラリは
ABI split（`split_config.x86_64.apk`）側にだけ入っている。つまり base.apk 単体では
起動できず、実機に入れるときは必ず split を同梱する必要がある。

もうひとつ、この APK 一式は x86_64 向けなので **ARM 実機には入らない**。Play がエミュレーターの
ABI を返したためで、arm64-v8a 版が必要なら別経路（apkeep の `device=px_9a` 等）で取り直す必要がある。

---

## 8. 広告・収益化まわり

AdMob（Google Mobile Ads）が組み込まれ、リワード広告とオファーウォールの 2 経路がある。
`/ads/reward` に POST して報酬を受け取る流れで、クライアント側に 15,000 ms の待機と
404/500 のステータス分岐が入っている。

`/api/config` の `transfer_enabled=false` から、ポイント送金機能は現在サーバー側で無効。
ギフト交換は `/gifts/exchange` に `{batch_id, items}` を送る形で、Amazon ギフト相当の
交換プログラムは MJC GROUP 株式会社 が提供している旨の文言がバンドル内にあった
（サポート窓口は support@sakiyomi.app）。

---

## 9. 確認できた事実と、できていないこと

### 確認した（実測）

- APK 4 本の署名・ハッシュ・構成、単一署名者であること
- manifest の権限 38 件、コンポーネント 43 件の exported 状態
- バックアップ除外設定、OTA 設定、code signing 証明書の存在
- JS バンドルの Hermes 逆コンパイル（83 万行）と文字列 14,199 件の抽出
- エンドポイント 87 件、メソッド付き 24 件の復元
- 秘密情報パターン 5 種の不在
- .so 26 本の ELF ハードニング属性
- `/api/config` 200 と `/api/me` 401 の実応答

### 確認できていない（推測を含む）

- **サーバー側の認可**。`/buy` `/sell` `/claim` などが「本人のリソースか」を検証しているかは、
  認証済みリクエストを送らないと判定できない。今回は未認証の 2 本しか叩いていない。
- **レート制限**。`/api/track` や `/ads/reward` の連打耐性は未検証。
- **OTA manifest の内容**。406 で拒否され、ペイロードを取得できなかった。
- **SecureStore のアクセシビリティ実値**。`keychainAccessible` を指定している箇所が無いため、
  Expo 既定の値で保存されている。その既定が何かは Expo 側の実装依存で、このバンドルからは決められない。
- **jadx の取りこぼし**。359 クラスでデコンパイルエラーが出ており、その部分は smali のまま
  `decode/base/smali*` に残っている。Java ソースだけを見ると見落とす箇所がある。
- **arm64-v8a 版**。今回の一式は x86_64 のみで、ARM 実機向けの split は含まれていない。

### 攻撃面の評価（所見）

壊れている箇所は見つからなかった。むしろ、設計上の判断が妥当な点が目立つ。

- 金銭パラメータを `/api/config` 経由でサーバーが配っており、クライアント改変で報酬を増やせない
- OTA に code signing を入れており、配信基盤が落ちても任意コード実行には至らない
- トークンを SecureStore + バックアップ除外で扱っている
- バンドルにも Java にも秘密情報が無い
- exported コンポーネントはランチャー 1 件のみで、残りは権限で保護

注意を要向けるべきは次の 3 点で、いずれも「実装の誤り」ではなく「運用・設計の選択」。

1. **OTA 署名鍵の管理**。ここが漏れると全端末に任意 JS を配れる。鍵の保管場所とローテーション手順を
   文書化しておくべき。
2. **`SYSTEM_ALERT_WINDOW` の残留**。他アプリに重ねて描ける権限で、UI リダイレクト攻撃の
   足場になり得る。Expo の開発者メニュー由来なら製品ビルドから外せる。ただし本アプリが実際に
   オーバーレイを表示しているかは未確認で、「権限が宣言されている」以上のことは言えない。
3. **`allowBackup=true` での sharedpref 一括バックアップ**。SecureStore は除外されているが、
   それ以外の設定データはクラウドに乗る。機微な設定を置いていないなら問題ない。

---

## 10. ディレクトリ構成

```
rea-sakiyomi/
├── REPORT.md                  … 本レポート
├── apk/
│   ├── base.apk               70,704,845 B
│   ├── split_config.ja.apk        70,041 B
│   ├── split_config.x86_64.apk 25,110,823 B
│   ├── split_config.xhdpi.apk    104,478 B
│   └── meta.json              … versionCode / minSdk / ABI など
├── decode/base/               … apktool 3.0.3 の展開結果
│   ├── AndroidManifest.xml    … デコード済み（人間可読）
│   ├── smali, smali_classes2..7
│   ├── res/                   … リソース 899 ファイル
│   ├── assets/                … index.android.bundle, app.config, expo-root.pem
│   └── apktool.yml
├── jadx/base/sources/         … jadx 1.5.6 の Java ソース 27,018 クラス
└── analysis/
    ├── bundle/
    │   ├── index.hasm         … Hermes 逆アセンブル（56.5 MB）
    │   ├── index.js           … Hermes 逆コンパイル（32.5 MB / 834,313 行）
    │   ├── strings.json       … 文字列 14,199 件 + パス/URL/ホスト抽出
    │   ├── contexts.json      … API 基底 URL・Bearer・各エンドポイントの前後文脈
    │   ├── functions_index.json … 元関数名 6,021 件
    │   └── decompile.log
    ├── inventory.json         … 権限・コンポーネント・SDK 構成・.so 一覧
    ├── signatures.json        … 各 APK の署名・ハッシュ・DEX/so 内訳
    ├── endpoints.json         … エンドポイント 87 件と実測結果
    ├── ota.json               … OTA 設定・署名証明書・exported 一覧
    ├── native_libs.json       … .so 26 本の ELF 属性
    ├── findings_js.json       … ストレージキー・秘密情報走査・クライアント側フラグ
    └── libs/                  … 抽出した .so 26 本
```

合計 888.8 MB（decode 428.6 MB、jadx 256.6 MB、analysis 112.1 MB、apk 91.5 MB）。

### 再現用スクリプト（`analysis/` 直下）

すべて Python 3.13 で実行確認済み。パスはこのディレクトリ構成を前提にしている。

| スクリプト | 役割 |
|---|---|
| `extract_strings.py` | hasm から文字列・パス・URL・ホストを抽出 |
| `build_inventory.py` | manifest から権限・コンポーネント・SDK 構成を JSON 化 |
| `verify_signatures.py` | 4 APK の署名・ハッシュ・署名者一致を検証 |
| `build_endpoints.py` | エンドポイント表と重要度・実測結果を統合 |
| `analyze_ota.py` | OTA 設定・code signing 証明書・exported 一覧 |
| `analyze_native.py` | .so の ELF ヘッダから PIE/NX/RELRO を判定 |
| `extract_findings.py` | ストレージキー・秘密情報パターン・フラグ語の走査 |
| `extract_context.py` | 特定パターン前後のコード文脈を切り出し |
| `extract_entitlement.py` | 課金判定・リクエスト組み立て箇所の文脈切り出し |
| `extract_functions.py` | 逆コンパイル結果から元関数名を復元 |

外部ツール: apktool 3.0.3 / jadx 1.5.6 / hermes-dec 0.1.8 / androguard 4.1.4 / capstone 5.0.9。

### 検証環境

Windows 11 (build 26300)、Python 3.13.14、JDK 21.0.13、MuMu Player の Android 15 インスタンス。
APK は Play ストアから実際にインストールしたものを `/data/app` から吸い出した実物で、
改変は加えていない。ハッシュは端末側の `sha256sum` と一致することを確認済み。
