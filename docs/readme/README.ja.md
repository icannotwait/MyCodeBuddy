<h1 align="center">
  <a href="https://docs.codeg.app"><img src="../../public/icon.svg" alt="Codeg logo" width="64" align="absmiddle" /></a> Codeg
</h1>

<p align="center">
  <a href="https://github.com/icannotwait/MyCodeBuddy"><img src="https://img.shields.io/github/stars/icannotwait/MyCodeBuddy?style=flat&color=e91e63" alt="GitHub stars" /></a>
  <a href="https://github.com/icannotwait/MyCodeBuddy/releases/latest"><img src="https://img.shields.io/github/v/release/icannotwait/MyCodeBuddy?style=flat&color=e91e63" alt="Latest release" /></a>
  <a href="https://github.com/icannotwait/MyCodeBuddy/releases"><img src="https://img.shields.io/github/downloads/icannotwait/MyCodeBuddy/total?style=flat&color=e91e63" alt="Total downloads" /></a>
  <a href="../../LICENSE"><img src="https://img.shields.io/github/license/icannotwait/MyCodeBuddy?style=flat&color=e91e63" alt="License" /></a>
  <a href="https://docs.codeg.app"><img src="https://img.shields.io/badge/docs-docs.codeg.app-3451b2?style=flat" alt="Documentation" /></a>
  <a href="https://docs.codeg.app/getting-started/installation"><img src="https://img.shields.io/badge/macOS%20%7C%20Windows%20%7C%20Linux%20%7C%20Docker%20%7C%20iOS%20%7C%20Android-03a9f4?style=flat" alt="Platforms: macOS, Windows, Linux, Docker, iOS, Android" /></a>
</p>

<p align="center">
  <sub><a href="../../README.md">English</a> · <a href="./README.zh-CN.md">简体中文</a> · <a href="./README.zh-TW.md">繁體中文</a> · <strong>日本語</strong> · <a href="./README.ko.md">한국어</a> · <a href="./README.es.md">Español</a> · <a href="./README.de.md">Deutsch</a> · <a href="./README.fr.md">Français</a> · <a href="./README.pt.md">Português</a> · <a href="./README.ar.md">العربية</a></sub>
</p>

<p align="center">
  <strong>マルチエージェント・コーディングワークスペース。</strong><br/>
  あらゆる AI コーディングエージェントをひとつの場所で動かし、そして協働させます。
</p>

<p align="center"><sub>Windows 向け DrawCode は <a href="https://github.com/icannotwait/MyCodeBuddy">MyCodeBuddy</a> から公開しています。</sub></p>

<h3 align="center"><a href="https://github.com/icannotwait/MyCodeBuddy/releases/latest"><ins>DrawCode をダウンロード</ins></a> · <a href="https://docs.codeg.app"><ins>ドキュメント</ins></a></h3>

<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="../images/workspace-dark.png" />
    <img src="../images/workspace-light.png" alt="Codeg のワークスペース：エージェントとの会話と、その隣に並ぶライブ diff とプロジェクトのファイル" width="960" />
  </picture>
</p>

<table>
  <tr>
    <td width="50%" valign="top">🧩 <strong>すべてのエージェントを、ひとつのインターフェースで</strong><br/>20 種のエージェントを内蔵し、ACP エージェントならどれでも加えられます — いずれもターミナルではなく、同じ構造化された会話として表示されます。</td>
    <td width="50%" valign="top">🔎 <strong>引き継げるセッション</strong><br/>各エージェントがディスクに残す履歴をインポート・検索・再開でき、さらに別のエージェントへ引き継げます。</td>
  </tr>
  <tr>
    <td width="50%" valign="top">🤝 <strong>協働するエージェント</strong><br/><code>@</code> ひとつでエージェント間の委譲ができ、ToDo タスクをキューに積めば、それぞれ専用のワークツリーで無人のまま進みます。</td>
    <td width="50%" valign="top">🌍 <strong>どこで作業していても</strong><br/>デスクトップアプリ、セルフホストのサーバーまたは Docker、iPhone・iPad・Android — さらに Telegram、Lark、WeChat からも。</td>
  </tr>
</table>

## ✨ 機能

<table>
<tr>
<td width="50%" valign="middle">

### すべてのエージェントを、ひとつのインターフェースで

20 種のエージェントを内蔵し、ほかの ACP エージェントも公開レジストリまたはその distribution JSON から加えられます。Codeg はそのすべてと Agent Client Protocol で通信するので、どれも目を凝らして読むターミナルではなく、同じリッチな会話 — ツールカード、ライブ diff、計画、権限の確認 — として表示されます。

[ドキュメント →](https://docs.codeg.app/guide/supported-agents) · [カスタムエージェント →](https://docs.codeg.app/guide/custom-agents) · [UI レンダリング →](https://docs.codeg.app/guide/skills#cards-tables-and-charts-in-replies)

</td>
<td width="50%">
<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../images/features/agents-dark.gif" />
  <img src="../images/features/agents-light.gif" alt="すべてのエージェントを、ひとつのインターフェースで" width="100%" />
</picture>
</td>
</tr>
<tr>
<td width="50%" valign="middle">

### すべてのセッションを、検索可能に

履歴を取り込み、検索して再開。`@` で別のエージェントにセッションを共有し、タグで整理できます。

[ドキュメント →](https://docs.codeg.app/guide/aggregation) · [タグ →](https://docs.codeg.app/guide/workspace#tag-conversations)

</td>
<td width="50%">
<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../images/features/sessions-dark.gif" />
  <img src="../images/features/sessions-light.gif" alt="すべてのセッションを、検索可能に" width="100%" />
</picture>
</td>
</tr>
<tr>
<td width="50%" valign="middle">

### マルチエージェント協調

`@` でエージェントに依頼。複数のエージェントが並行して働き、進捗が会話に届きます。

[ドキュメント →](https://docs.codeg.app/guide/multi-agent)

</td>
<td width="50%">
<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../images/features/multi-agent-dark.gif" />
  <img src="../images/features/multi-agent-light.gif" alt="マルチエージェント協調" width="100%" />
</picture>
</td>
</tr>
<tr>
<td width="50%" valign="middle">

### 無人で進む ToDo タスク

タスク専用のワークツリーで、今すぐ、または指定時刻に実行。変更はあなたが確認して承認します。

[ドキュメント →](https://docs.codeg.app/guide/tasks) · [自動処理 →](https://docs.codeg.app/guide/repository#let-it-pick-items-up-by-itself)

</td>
<td width="50%">
<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../images/features/todos-dark.gif" />
  <img src="../images/features/todos-light.gif" alt="無人で進む ToDo タスク" width="100%" />
</picture>
</td>
</tr>
<tr>
<td width="50%" valign="middle">

### 無限キャンバス

会話、ファイル、ターミナル、メモを無限のボードに配置。各エージェントの作業をひと目で把握できます。

[ドキュメント →](https://docs.codeg.app/guide/canvas)

</td>
<td width="50%">
<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../images/features/canvas-dark.gif" />
  <img src="../images/features/canvas-light.gif" alt="無限キャンバス" width="100%" />
</picture>
</td>
</tr>
<tr>
<td width="50%" valign="middle">

### デスクトップ・サーバー・モバイル・チャット

デスクトップ、サーバー、Docker、モバイルに対応。Telegram、Lark、WeChat からもエージェントと会話できます。

[ドキュメント →](https://docs.codeg.app/getting-started/deployment) · [リモートワークスペース →](https://docs.codeg.app/guide/workspace#desktop-and-browser) · [モバイルアプリ →](https://docs.codeg.app/getting-started/installation#mobile-apps) · [チャットチャンネル →](https://docs.codeg.app/guide/chat-channels)

</td>
<td width="50%">
<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../images/features/anywhere-dark.gif" />
  <img src="../images/features/anywhere-light.gif" alt="デスクトップ・サーバー・モバイル・チャット" width="100%" />
</picture>
</td>
</tr>
<tr>
<td width="50%" valign="middle">

### Office ドキュメントと科学研究

Office ファイルを作成してライブプレビュー、PDF も閲覧。研究スキルはどのエージェントでも使えます。

[ドキュメント →](https://docs.codeg.app/guide/office) · [科学研究 →](https://docs.codeg.app/guide/research)

</td>
<td width="50%">
<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../images/features/office-dark.gif" />
  <img src="../images/features/office-light.gif" alt="Office ドキュメントと科学研究" width="100%" />
</picture>
</td>
</tr>
<tr>
<td width="50%" valign="middle">

### ブラウザとコンピューター操作

デスクトップの内蔵ブラウザからページやエラーを共有。コンピューター操作（プレビュー）で共有ウィンドウも操作できます。

[ドキュメント →](https://docs.codeg.app/guide/browser) · [コンピューター操作 →](https://docs.codeg.app/guide/computer-use)

</td>
<td width="50%">
<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../images/features/browser-dark.gif" />
  <img src="../images/features/browser-light.gif" alt="ブラウザとコンピューター操作" width="100%" />
</picture>
</td>
</tr>
<tr>
<td width="50%" valign="middle">

### コマンドラインと CI

`codeg-cli` でスクリプトや CI から起動中の Codeg に依頼。返答や JSON イベントを出力します。

[ドキュメント →](https://docs.codeg.app/guide/command-line)

</td>
<td width="50%" valign="middle">

```bash
codeg-cli -a codex \
  -p "Review the latest commit"

git diff | codeg-cli -a codex \
  -p "Review this diff"
```

</td>
</tr>
</table>

**このほかにも：**

- **[開発の一連の流れをまるごと](https://docs.codeg.app/guide/workspace)** — コードエディター、ライブ差分、[Git とワークツリー](https://docs.codeg.app/guide/git)、3 ペインのマージエディター、ターミナル、分割表示
- **[リポジトリパネル](https://docs.codeg.app/guide/repository)** — GitHub、GitLab、Gitea、Forgejo のイシュー、プルリクエスト、チェック、マージ。どのイシューや PR も ToDo タスクにできます
- **[オートメーション](https://docs.codeg.app/guide/automations)** — 設定済みの入力欄を保存し、cron スケジュールまたは任意のタイミングでヘッドレス実行
- **[トークン使用量](https://docs.codeg.app/guide/token-usage)** — 推移、キャッシュヒット率、アクティビティのヒートマップ、フォルダー・エージェント・モデル・セッション別の使用量、API の定価で見積もったコスト
- **[分岐とステアリング](https://docs.codeg.app/guide/workspace#follow-along-%E2%80%94-the-conversation)** — 対応しているエージェントでは、完了した返信から会話を分岐させたり、まだ実行中のターンにメッセージを送り込んだりできます
- **[スキル](https://docs.codeg.app/guide/skills) と [MCP](https://docs.codeg.app/guide/mcp)** — エージェントごとのスキルパック、ローカルの MCP スキャン、レジストリの検索とインストール
- **[プロジェクトブート](https://docs.codeg.app/guide/project-boot)** — ライブプレビュー付きで新規プロジェクトを視覚的に構築
- **[通知](https://docs.codeg.app/reference/settings/general#desktop-notifications)** — ターンが終わったときやエージェントに呼ばれたときに、デスクトップ通知と通知音、タスクバーボタンの点滅や Dock アイコンのバウンドで知らせます
- **[自分好みに](https://docs.codeg.app/reference/settings/appearance)** — カラートークン単位で塗り替えられる 12 のテーマ、壁紙、角丸、チャット幅、カスタム CSS、デスクトップペット — インターフェースは 10 言語に対応
- **[バックアップと同期](https://docs.codeg.app/reference/settings/system#backup-restore)** — 暗号化バックアップと、ファイルまたは自分の WebDAV サーバーを介したマシン間の設定同期
- **[URL スキーム](../../docs/url-scheme.md)** — `codeg://session/<id>` で別のアプリから会話を開けます（デスクトップ）
- **まだまだあります** — ほぼすべてのリリースで何かが加わります。全リストは [リリースノート](https://github.com/icannotwait/MyCodeBuddy/releases) で確認できます

## 🤖 対応エージェント

Codeg はすべてのエージェントと [Agent Client Protocol](https://agentclientprotocol.com) で通信するため、どのエージェントにも同じ構造化されたインターフェースが用意されます。20 種を内蔵し、その多くは Codeg がインストール・バージョン固定・更新まで面倒を見ます：

<p>
  <a href="https://www.anthropic.com/claude-code"><kbd><img src="../images/agents/claude-code.svg" alt="" width="16" valign="middle" /> Claude Code</kbd></a> &nbsp;
  <a href="https://github.com/openai/codex"><kbd><img src="../images/agents/codex.svg" alt="" width="16" valign="middle" /> Codex</kbd></a> &nbsp;
  <a href="https://github.com/google-gemini/gemini-cli"><kbd><img src="../images/agents/gemini-cli.svg" alt="" width="16" valign="middle" /> Gemini CLI</kbd></a> &nbsp;
  <a href="https://opencode.ai"><kbd><picture><source media="(prefers-color-scheme: dark)" srcset="../images/agents/opencode-dark.svg" /><img src="../images/agents/opencode.svg" alt="" width="16" valign="middle" /></picture> OpenCode</kbd></a> &nbsp;
  <a href="https://cline.bot"><kbd><picture><source media="(prefers-color-scheme: dark)" srcset="../images/agents/cline-dark.svg" /><img src="../images/agents/cline.svg" alt="" width="16" valign="middle" /></picture> Cline</kbd></a> &nbsp;
  <a href="https://hermes-agent.nousresearch.com"><kbd><picture><source media="(prefers-color-scheme: dark)" srcset="../images/agents/hermes-agent-dark.svg" /><img src="../images/agents/hermes-agent.svg" alt="" width="16" valign="middle" /></picture> Hermes Agent</kbd></a> &nbsp;
  <a href="https://www.codebuddy.ai"><kbd><picture><source media="(prefers-color-scheme: dark)" srcset="../images/agents/codebuddy-dark.svg" /><img src="../images/agents/codebuddy.svg" alt="" width="16" valign="middle" /></picture> CodeBuddy</kbd></a> &nbsp;
  <a href="https://www.kimi.com/code"><kbd><img src="../images/agents/kimi-code.svg" alt="" width="16" valign="middle" /> Kimi Code</kbd></a> &nbsp;
  <a href="../agents/minimax-code.md"><kbd><img src="../images/agents/minimax-code.svg" alt="" width="16" valign="middle" /> MiniMax Code</kbd></a> &nbsp;
  <a href="https://pi.dev"><kbd><img src="../images/agents/pi.svg" alt="" width="16" valign="middle" /> Pi</kbd></a> &nbsp;
  <a href="https://x.ai/cli"><kbd><picture><source media="(prefers-color-scheme: dark)" srcset="../images/agents/grok-dark.svg" /><img src="../images/agents/grok.svg" alt="" width="16" valign="middle" /></picture> Grok</kbd></a> &nbsp;
  <a href="https://cursor.com/cli"><kbd><picture><source media="(prefers-color-scheme: dark)" srcset="../images/agents/cursor-dark.svg" /><img src="../images/agents/cursor.svg" alt="" width="16" valign="middle" /></picture> Cursor</kbd></a> &nbsp;
  <a href="https://deepseek-harness.github.io/deepseek-harness/"><kbd><img src="../images/agents/deepseek-harness.svg" alt="" width="16" valign="middle" /> DeepSeek Harness</kbd></a> &nbsp;
  <a href="https://qoder.com"><kbd><picture><source media="(prefers-color-scheme: dark)" srcset="../images/agents/qoder-dark.svg" /><img src="../images/agents/qoder.svg" alt="" width="16" valign="middle" /></picture> Qoder</kbd></a> &nbsp;
  <a href="https://antigravity.google"><kbd><picture><source media="(prefers-color-scheme: dark)" srcset="../images/agents/google-antigravity-dark.svg" /><img src="../images/agents/google-antigravity.svg" alt="" width="16" valign="middle" /></picture> Google Antigravity</kbd></a> &nbsp;
  <a href="https://kiro.dev/cli/"><kbd><img src="../images/agents/kiro.svg" alt="" width="16" valign="middle" /> Kiro CLI</kbd></a> &nbsp;
  <a href="../agents/zcode.md"><kbd><img src="../images/agents/zcode.svg" alt="" width="16" valign="middle" /> ZCode</kbd></a> &nbsp;
  <a href="https://docs.trae.cn/cli_about-trae-code-cli-2"><kbd><img src="../images/agents/trae.svg" alt="" width="16" valign="middle" /> TRAE CLI</kbd></a> &nbsp;
  <a href="https://commandcode.ai"><kbd><img src="../images/agents/command-code.svg" alt="" width="16" valign="middle" /> Command Code</kbd></a> &nbsp;
  <a href="https://docs.codeg.app/guide/custom-agents"><kbd>+ 任意の ACP エージェント</kbd></a>
</p>

リストにない？公開されている ACP レジストリから好きなエージェントを選ぶか、その distribution JSON を貼り付ければ、Codeg がインストールし、起動できるかを確かめ、内蔵エージェントと同じように扱います。各エージェントの実行環境要件とセッションの保存場所は [対応エージェント](https://docs.codeg.app/guide/supported-agents) を参照してください。

## 🪟 画面分割

タブ列がひとつでは足りないときもあります。会話タブを右クリックすれば、ビューを**右**または**下**へ、何度でも分割できます — 左右に 2 つ、縦に 3 つ、あるいは格子状に。どのグループもそれ自体がひとつのワークスペースで、独自のタブ、独自のヘッダー、独自の新規会話ボタンを持ちます。片方のペインで Claude Code にリファクタリングさせ、隣のペインで Codex に diff をレビューさせる、という具合です。

タブをグループ間でドラッグしても、そのセッションは移動中もストリーミングを続けます。グループの境界をドラッグすれば、スペースの分け方が変わります。レイアウトはワークスペースごとに、下書きも含めて記憶されます — Codeg を開き直せば分割はそのまま戻り、送らなかった文字も入力欄に残っています。

![会話エリアをタブグループの格子に分割する](../images/split-light.gif#gh-light-mode-only)
![会話エリアをタブグループの格子に分割する](../images/split-dark.gif#gh-dark-mode-only)

## 📦 インストール

**デスクトップ** — このリポジトリの GitHub Releases が配布するのは Windows 向け **DrawCode**（NSIS）です。日常使いにはデスクトップ版を入れてください。

**サーバー** — `codeg-server` は自己ホスト専用です（Cargo feature `server`）。デスクトップ利用者は DrawCode をインストールします。Windows（PowerShell）：

```powershell
.\uninstall-server.ps1
# or:
irm https://raw.githubusercontent.com/icannotwait/MyCodeBuddy/main/uninstall-server.ps1 | iex

.\install.ps1 -Version v0.45.0-mycodebuddy.1
# or:
irm https://raw.githubusercontent.com/icannotwait/MyCodeBuddy/main/install.ps1 | iex
$env:CODEG_STATIC_DIR="$env:LOCALAPPDATA\codeg-server\web"; codeg-server
```

インストーラーの既定ディレクトリは `%LOCALAPPDATA%\codeg-server` なので、DrawCode のデスクトップ版を上書きしません。古いインストールがデスクトップのフォルダーにある場合、スクリプトはサーバーをそこから移します。

[GitHub Releases](https://github.com/icannotwait/MyCodeBuddy/releases) では、セルフホスト用の署名付きサーバーアーカイブも配布しています：`codeg-server-linux-x64.tar.gz`、`codeg-server-linux-arm64.tar.gz`、`codeg-server-darwin-x64.tar.gz`、`codeg-server-darwin-arm64.tar.gz`、`codeg-server-windows-x64.zip`。各アーカイブにはサーバー、MCP コンパニオン、コンピューター操作ヘルパー、`web/` アセット、ライセンスファイルが含まれます。

**Docker** — 任意のサーバーをこのリポジトリからビルドします：

```bash
docker compose up -d
```

Docker Compose はイメージをローカルでビルドし、Cargo feature `server` を有効にします。データは `/data` ボリュームに残り、トークン・ポート・プロジェクトのマウントは `docker-compose.yml` で設定します。

**ソースからビルド** — `codeg-server` には `--features server`、`codeg-mcp` には `--features mcp-bin` が必要です：

```bash
pnpm install && pnpm build
cd src-tauri
cargo build --release --bin codeg-server --no-default-features --features server
cargo build --release --bin codeg-mcp --no-default-features --features mcp-bin
# コンピューター操作を有効にする場合のみ：
cargo build --release --bin codeg-computer-helper --no-default-features --features computer-helper
CODEG_STATIC_DIR=../out ./target/release/codeg-server
```

**Linux/macOS のソースビルド更新** — リポジトリのルートで実行し、ソースを取得して再ビルドします：

```bash
git pull
pnpm install && pnpm build
cd src-tauri
cargo build --release --bin codeg-server --no-default-features --features server
cargo build --release --bin codeg-mcp --no-default-features --features mcp-bin
# コンピューター操作を有効にする場合のみ：
cargo build --release --bin codeg-computer-helper --no-default-features --features computer-helper
# サービスを停止し、サーバー、MCP コンパニオン、任意の操作ヘルパー、Web アセットを再配置して再起動します。
```

Docker の更新は、リポジトリのルートで `git pull && docker compose up --build -d --force-recreate` を実行します。

**モバイル** — [iOS アプリ](https://apps.apple.com/app/codeg-client/id6785199071) または [Android APK](https://github.com/xintaofei/codeg-android/releases/latest) をインストールし、デスクトップアプリの **Webサービス**か自分の `codeg-server` を指定するだけ：アドレスとトークンを入れれば完了です。接続手順は [モバイルアプリ](https://docs.codeg.app/getting-started/installation#mobile-apps)。

Compose、ビルド済みバイナリ、ソースからのビルド、その場での更新は [デプロイ](https://docs.codeg.app/getting-started/deployment) に、環境変数は [設定](https://docs.codeg.app/getting-started/configuration) にあります。

## 👥 コミュニティとサポート

- **WeChat** — QRコードをスキャンして、ディスカッション、フィードバック、アップデートのためのグループに参加してください：

  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="../images/weixin-dark.jpg" />
    <img src="../images/weixin-light.jpg" alt="WeChat" width="200" />
  </picture>

- **イシュー** — バグを見つけた？欲しい機能がない？[イシューを作成してください](https://github.com/icannotwait/MyCodeBuddy/issues)。
- **プライバシー** — ローカル優先：解析・保存・プロジェクト操作はあなたのマシン上にとどまり、Web モードとサーバーモードはトークン認証で保護されます。詳細は [プライバシーとセキュリティ](https://docs.codeg.app/reference/privacy) を参照してください。
- **LinuxDO** — [LinuxDO](https://linux.do) コミュニティのサポートに感謝します。
- **応援する** — [リポジトリにスターを付けて](https://github.com/icannotwait/MyCodeBuddy)、今後の進展をフォローしてください。

## 🤝 コントリビューション

イシューもプルリクエストも歓迎します。Codeg を自分でビルドするなら、まず [開発](https://docs.codeg.app/reference/development) と [アーキテクチャ](https://docs.codeg.app/reference/architecture) から読んでください。

<a href="https://github.com/icannotwait/MyCodeBuddy/graphs/contributors">
  <img src="https://contrib.rocks/image?repo=icannotwait/MyCodeBuddy" alt="Codeg のコントリビューター" />
</a>

## ⭐ スター履歴

<a href="https://www.star-history.com/?repos=icannotwait%2FMyCodeBuddy&type=date&releases=&legend=bottom-right">
 <picture>
   <source media="(prefers-color-scheme: dark)" srcset="https://api.star-history.com/chart?repos=icannotwait/MyCodeBuddy&type=date&theme=dark&legend=bottom-right" />
   <source media="(prefers-color-scheme: light)" srcset="https://api.star-history.com/chart?repos=icannotwait/MyCodeBuddy&type=date&legend=bottom-right" />
   <img alt="Star History Chart" src="https://api.star-history.com/chart?repos=icannotwait/MyCodeBuddy&type=date&legend=bottom-right" />
 </picture>
</a>

## 🙏 謝辞

- MyCodeBuddy は DrawCode を公開する、元の Codeg プロジェクトのフォークです。リリースとインストーラーはこのリポジトリから提供します。

- [Agent Client Protocol](https://agentclientprotocol.com) — Codeg が対応するすべてのエージェントへ接続できる土台
- [Superpowers](https://github.com/obra/superpowers) — Codeg のエキスパートスキルモジュールを支えるプロジェクト
- [OfficeCLI](https://github.com/iOfficeAI/OfficeCLI) — Codeg の Office ドキュメントワークフローを支えるプロジェクト
- [scientific-agent-skills](https://github.com/K-Dense-AI/scientific-agent-skills) — Codeg の科学研究スキルを支えるプロジェクト（MIT ライセンスのサブセット）
- [json-render](https://github.com/vercel-labs/json-render) — Codeg の UI レンダリング（カード・表・グラフ）を支えるプロジェクト
- [cua-driver](https://github.com/trycua/cua) — Codeg のコンピューター操作を支えるプロジェクト

## 📜 ライセンス

Apache-2.0。[LICENSE](../../LICENSE) を参照してください。
