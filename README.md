# colabo

Rust 製の共同編集ワークスペースです。ワークスペース、ファイル、チャット、招待、共同編集スナップショットはファイルシステムに保存し、ファイル履歴には各ワークスペース専用の bare Git リポジトリを使います。

## 起動

必要なものは Rust 1.95 以降、Node.js、npm です。Typst はブラウザー内の WebAssembly で組版するため、Typst CLI は不要です。初回プレビューでは Typst のフォントやパッケージ取得元へブラウザーから接続できる必要があります。

```powershell
Copy-Item .env.example .env
cd crates/colabo-server/web
npm ci
npm run build
cd ../../..
cargo run -p colabo-server
```

初期設定は `http://127.0.0.1:85` で待ち受けます。OIDC クライアントが未設定の場合もサーバーは起動しますが、ログインは無効になり、ワークスペース API は未認証アクセスを拒否します。

## OIDC

アプリは単一 OIDC Provider の Authorization Code Flow と PKCE を使います。サーバーは `iss` と `sub` を内部ユーザー ID に対応させ、ID Token の署名、発行者、対象、期限、nonce を検証します。メールアドレスは識別子として保存しません。

OIDC Provider に以下を登録して `.env` に設定してください。

- Issuer: `https://auth.371tti.net`
- Redirect URI: `https://dev.371tti.net/auth/callback`（`.env.example` の既定値）
- Response type: `code`
- PKCE: `S256`
- Scopes: `openid profile`
- Token endpoint 認証: `client_secret_basic`

`.env.example` には秘密情報を含めていません。Client ID と Client Secret は環境ごとに設定してください。片方だけを設定するとサーバーは起動を拒否します。

HTTPS の公開環境では `COLABO_PUBLIC_URL` と `OIDC_REDIRECT_URI` を同じ origin に設定し、`SESSION_COOKIE_SECURE=true` にしてください。リバースプロキシからの外部 URL は `COLABO_PUBLIC_URL` で指定します。

## Crate 構成

```text
crates/
├── collab-core/          # collab-system からコピーした CRDT とプロトコル
├── colabo-domain/        # User、Workspace、Role、権限、パス規則
├── colabo-storage/       # FS、セッション、チャット、Git 履歴
├── colabo-collaboration/ # コピーした共同編集シーケンサーと認証済み WebSocket 接続
└── colabo-server/        # OIDC、HTTP API、権限判定、Web UI
    ├── src/api/          # account、files、sharing、chat、history、workspaces
    └── web/              # Vite、Monaco、HTML/CSS/JS
```

共同編集エンジンの Rust ソースは `collab-system` からこの workspace 内にコピーしており、隣接リポジトリへの path dependency はありません。エンジンが持つ未認証 HTTP ルーターは公開せず、アプリ側でログイン、workspace 所属、Reader / Writer 権限を確認してから WebSocket を接続します。

## 保存先

`COLABO_DATA_DIR` はアプリケーション内部データのルートです。既定では `./data` を使います。

```text
data/
├── users/                 # 内部 UserId ごとのユーザー情報
├── identities/            # iss + sub から UserId への対応
├── sessions/              # ハッシュ化したアプリセッション
└── workspaces/<id>/
    ├── metadata.json
    ├── members.json
    ├── invitations.json   # 招待トークン自体は保存しない
    ├── chat.jsonl
    ├── files/              # ユーザーファイルのみ
    ├── repo.git/           # ファイルツリーだけを記録
    └── collaboration/      # CRDT Snapshot とファイル/actor ID
```

フォルダー取り込みは1リクエストあたりファイル本体 190 MB までです。Multipart の境界情報を含むHTTPリクエストは約198 MBで制限します。受信データは一時ファイルへ順次書き出し、パスを検証してからワークスペースへ保存します。保存時にはシンボリックリンクも検査します。公開環境では手前のCDNやリバースプロキシの上限も合わせてください。

Typst プレビューは Web Worker 内の WebAssembly で SVG を生成し、ワークスペースにある `.ttf` / `.otf` / `.ttc` / `.otc` フォントと `@preview` 公開パッケージを読み込みます。たとえば次の形式でパッケージを利用できます。

```typst
#import "@preview/cetz:0.5.3"
#cetz.canvas({
  import cetz.draw
  draw.circle((0, 0), radius: 1)
})
```

プレビューにはブラウザーからフォント配信元と `packages.typst.org` へ接続できる必要があります。Typst.ts 0.8.0-rc3 を使い、Typst 0.15 系の公開パッケージに対応します。プレビュー用に同期するワークスペースの上限は5,000ファイル / 256 MiBです。

## UI

UI は Rust サーバー内から配信します。Monaco Editor をテキスト共同編集に使い、Reader は読み取り専用です。Markdown は DOMPurify でサニタイズし、HTML は sandbox iframe に隔離します。画像、音声、動画、PDF をブラウザー内で表示し、CSV は表として描画します。Liner CSS の配布 CSS、フォント、デザイントークンを使用します。
