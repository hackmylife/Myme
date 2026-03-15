# myme — macOS向け自作日本語IME

ローカル動作・高速・安定を重視した macOS 向け日本語入力メソッド。
Rust で実装した変換エンジンを中心に、段階的に実用性を高めています。

## アーキテクチャ

```
┌─────────────────────────────────────────────┐
│  macOS IME フロントエンド (macos/MymeIM/)    │   ← InputMethodKit 統合
│    キー入力 → FFI → Rust コア → 候補表示     │
└────────────────┬────────────────────────────┘
                 │ C FFI (ffi.rs)
┌────────────────▼────────────────────────────┐
│  myme-core (crates/myme-core/)              │   ← 変換エンジン本体
│  ┌──────────┐ ┌────────────┐ ┌───────────┐  │
│  │ romaji   │ │ dictionary │ │ candidate │  │
│  │ ローマ字  │ │ 辞書検索   │ │ スコアリング│  │
│  └────┬─────┘ └─────┬──────┘ └─────┬─────┘  │
│  ┌────▼─────────────▼──────────────▼─────┐  │
│  │ session — セッション状態管理            │  │
│  │  Composing → Converting → Commit      │  │
│  └────────────────┬──────────────────────┘  │
│  ┌────────────────▼──────────────────────┐  │
│  │ segmenter — かな文字列の文節分割       │  │
│  │  greedy / viterbi                     │  │
│  └───────────────────────────────────────┘  │
│  ┌─────────────┐ ┌────────────────────┐     │
│  │ learning    │ │ user_dict          │     │
│  │ 選択履歴学習 │ │ ユーザー辞書        │     │
│  └─────────────┘ └────────────────────┘     │
└─────────────────────────────────────────────┘
┌─────────────────────────────────────────────┐
│  データ層 (data/)                            │
│  dict/system.dict  — SKK形式システム辞書      │
│  eval/basic.jsonl  — 単語評価 (50件)         │
│  eval/sentence.jsonl — 文評価 (30件)         │
└─────────────────────────────────────────────┘
```

## プロジェクト構成

```
myme/
├── crates/
│   ├── myme-core/      # 変換エンジン (Rust ライブラリ)
│   │   └── src/
│   │       ├── romaji.rs     # ローマ字→かな変換
│   │       ├── dictionary.rs # SKK辞書の読み込み・検索
│   │       ├── candidate.rs  # 候補のスコアリング・ソート
│   │       ├── segmenter.rs  # かな→文節分割 (greedy / viterbi)
│   │       ├── session.rs    # IMEセッション状態管理
│   │       ├── learning.rs   # 選択履歴の学習・永続化
│   │       ├── user_dict.rs  # ユーザー辞書
│   │       ├── ffi.rs        # C FFI (macOS統合用)
│   │       └── lib.rs
│   └── myme-cli/       # 開発用CLIハーネス
├── macos/MymeIM/       # macOS InputMethodKit プラグイン
├── tools/dict-builder/ # 辞書ビルドツール
├── data/
│   ├── dict/           # システム辞書 (SKK形式)
│   └── eval/           # 評価データ (JSONL)
└── docs/               # 設計ドキュメント
```

## ビルドと実行

```bash
# ビルド
cargo build --workspace

# 全テスト実行 (166テスト)
cargo test --workspace

# 評価実行
cargo run -p myme-cli -- --eval data/eval/basic.jsonl
cargo run -p myme-cli -- --eval data/eval/sentence.jsonl --verbose

# インタラクティブモード
cargo run -p myme-cli -- -i

# 辞書検索
echo "へんかん" | cargo run -p myme-cli -- --lookup
```

### CLI モード一覧

| フラグ | 説明 |
|--------|------|
| (なし) | stdin からローマ字を読み、かなに変換して出力 |
| `-i` / `--interactive` | raw-terminal でリアルタイム入力 |
| `-l` / `--lookup` | stdin からかなを読み、辞書候補を表示 |
| `-e` / `--eval <file>` | JSONL 評価ファイルを実行しメトリクス表示 |
| `-v` / `--verbose` | eval 時に失敗ケースのセグメント詳細を表示 |

## 開発フェーズ

### Phase 1: 最小のIME成立 ✅

macOS 上で動作する最小構成の日本語 IME を構築。

- ローマ字→かな変換 (romaji.rs)
- SKK形式辞書の読み込みと検索 (dictionary.rs)
- 候補スコアリングとソート (candidate.rs)
- IME セッション状態管理 — Idle → Composing → Converting → Commit (session.rs)
- macOS InputMethodKit 統合 (C FFI経由)
- 基本評価基盤 (JSONL eval)

### Phase 2: 実用的な変換 ✅

連文節変換、ユーザー辞書、学習機能を追加し実用性を向上。

- 連文節変換 — greedy longest-match セグメンテーション (segmenter.rs)
- 複数セグメントの候補ナビゲーション (←→キー)
- ユーザー辞書 (user_dict.rs) — TSV形式、システム辞書と合成
- 選択履歴学習 (learning.rs) — TSV永続化、90日GC、`min(count*10, 200)` ブースト
- 辞書の頻度アノテーション (`freq=N`) によるスコアブースト
- 評価データ 50 件、単語精度 98%

### Phase 3: 品質向上 🔧 ← 現在

学習統合の修正、評価の拡充、セグメンテーションの高度化。

#### Step 1: 学習ブーストの候補ランキング統合 ✅

**問題**: `LearningStore.boost()` は実装されていたが、変換時のランキングに反映されていなかった。

**変更内容**:
- `session.rs` に `apply_learning_boosts()` を追加 — 全セグメントの候補に学習ブーストを適用し、再ソート
- `handle_key()` で借用分割パターンを使用 — `learning.as_deref()` で読み取り専用参照を取得し、ブースト適用後にミュータブル参照で学習記録
- `handle_composing()` → `try_convert()` に `Option<&LearningStore>` をスレッド

**動作**: ユーザーが候補を選択するたびに学習記録され、次回変換時にその候補のスコアが加算される。5回選択で +50 ブースト (例: 「京」をデフォルト候補「今日」より上位に昇格)。

#### Step 2: 文レベル評価ケース ✅

**問題**: 50件の評価ケースがすべて単語検索で、セグメンテーション誤りや学習効果を検出できなかった。

**変更内容**:
- `data/eval/sentence.jsonl` — 30件の文レベル評価ケース (助詞、複合語、文節境界)
- CLI に `--verbose` フラグ — 失敗時にセグメントごとの `読み→表層形` を表示

**ベースライン**: 完全一致 3.3%、セグメント精度 34.7%。主な失敗原因は greedy が語境界をまたぐ辞書エントリを選択すること (例: 「きょうは」→「教派」) と、助詞の候補順位 (例: 「は」→「葉」、「を」→「小」)。

#### Step 3: Viterbi セグメンテーション 🔧

**問題**: greedy longest-match は下流の影響を考慮せず、語境界をまたぐ最長一致を選択してしまう。

**変更内容**:
- `segmenter.rs` に `segment_viterbi()` を実装 — DP テーブルで最小コスト経路を探索
- コスト関数: `SEGMENT_PENALTY - min(sqrt(top_score), MAX_SCORE_CONTRIBUTION)` + 未知文字ペナルティ
- 旧実装を `segment_greedy()` にリネーム、比較用に保持

**現状**: `segment()` は greedy をデフォルトとして使用。ポジションベースの辞書スコアリングでは、単一文字エントリのスコアが不釣り合いに高く (候補数が多い → スコア 200+)、Viterbi のコスト関数が複合語よりも短い分割を優先してしまう。頻度ベースのスコアリングが利用可能になれば、Viterbi がデフォルトとして greedy を上回る見込み。

## 現在の精度

| 評価セット | 完全一致 | セグメント精度 |
|-----------|---------|--------------|
| basic.jsonl (50件, 単語) | 98.0% (49/50) | 98.0% |
| sentence.jsonl (30件, 文) | 3.3% (1/30) | 34.7% |

文レベルの精度向上には、辞書の頻度データ改善と Viterbi コスト関数のチューニングが必要。

## テスト

```bash
# 全 166 テスト
cargo test --workspace
```

| モジュール | テスト数 | カバー範囲 |
|-----------|---------|-----------|
| romaji | 50+ | 母音行、子音行、拗音、促音、ん処理、バックスペース |
| dictionary | 20+ | SKKパース、検索、prefix search、頻度アノテーション |
| candidate | 4 | スコアソート、フィールド設定 |
| segmenter | 10 | 単語/複合語/未知文字分割、Viterbi vs greedy |
| session | 28 | 状態遷移、候補ナビ、セグメント操作、学習統合 |
| learning | 5 | 記録、ブースト、キャップ、永続化、GC |
| user_dict | 5 | 合成検索、スコア統合 |
| ffi | 12 | C FFI ラウンドトリップ |
| dict-builder | 10 | ひらがなバリデーション |

## 辞書形式

システム辞書は SKK 形式:

```
; コメント行
へんかん /変換/偏官/返還/
にほんご /日本語/
てすと /テスト;freq=1024/試験/
```

- 各行: `読み /候補1/候補2;freq=N/.../`
- スコア: `(候補数 - 位置) × 10 + log₂(freq) × 3`
- ユーザー辞書は同形式で `~/Library/Application Support/myme/user.dict` に配置

## 学習データ

選択履歴は TSV 形式で `~/Library/Application Support/myme/learning.tsv` に保存:

```
reading	surface	count	last_used
きょう	京	5	1710000000
```

- ブースト: `min(count × 10, 200)`
- 90日経過エントリは自動 GC
- 5回確定ごとにフラッシュ

## ライセンス

MIT
