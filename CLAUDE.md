# myme — macOS 日本語 IME

Rust 製の macOS 向け日本語入力メソッド。変換エンジン (myme-core) と macOS 統合 (macos/MymeIM/) を分離した構成。

## ビルドとテスト

```bash
cargo test --workspace          # 全テスト (173件)
cargo build --workspace         # デバッグビルド
make dict                       # 辞書再ビルド (data/raw/ → data/dict/)
make install                    # macOS IME としてインストール
```

## 評価

```bash
cargo run -p myme-cli -- --eval data/eval/basic.jsonl          # 単語評価
cargo run -p myme-cli -- --eval data/eval/sentence.jsonl -v    # 文評価 (詳細)
cargo run -p myme-cli -- --eval data/eval/sentence.jsonl --report report.jsonl  # JSONL レポート
```

現在の精度: basic 100%, sentence 90% (Top-1)。変更後は必ず両方の eval を実行し、basic ≥98%、sentence が低下しないことを確認すること。

## コード規約

- 変換品質に関わるロジックは全て `crates/myme-core/` に置く。macOS 依存コードをコアに混ぜない
- 辞書スコアリング: `(候補数 - 位置) × 10 + log₂(freq) × 7`。freq は `data/raw/frequency.tsv` で管理
- 助詞ブースト: `dictionary.rs` の `PARTICLE_READINGS` リスト。助詞のひらがな形を +100 でトップに
- セグメンテーション: `segmenter.rs` で Viterbi がデフォルト。コスト = `SEGMENT_PENALTY(9) - min(sqrt(score), MAX(8))`。SP=MAX+1 が重要（セグメント数ペナルティを内蔵）
- テスト追加時: 既存テストの期待値を変えるのではなく、新しいテストを追加する

## ファイル構成

- `crates/myme-core/src/` — 変換エンジン。romaji→session→segmenter→dictionary→candidate の流れ
- `crates/myme-core/src/ffi.rs` — C FFI。macOS プラグインとの境界
- `crates/myme-cli/src/main.rs` — CLI (batch / interactive / lookup / eval)
- `tools/dict-builder/` — SKK辞書 (EUC-JP) → system.dict 変換。frequency.tsv と extra.dict もマージ
- `data/raw/frequency.tsv` — 頻度データ (読み\t表層形\t頻度)
- `data/raw/extra.dict` — 補助辞書 (動詞活用形など、SKK形式)
- `data/eval/` — 評価データ (JSONL: input, expected, tags?, note?)

## よくある作業

- **辞書に語を追加**: `data/raw/extra.dict` に SKK 形式で追加 → `make dict` → eval で確認
- **頻度調整**: `data/raw/frequency.tsv` に追加 → `make dict` → eval で確認
- **ローマ字マッピング追加**: `romaji.rs` の `ROMAJI_TABLE` に追加 → テスト追加
- **助詞追加**: `dictionary.rs` の `PARTICLE_READINGS` に追加

## 注意事項

- IMPORTANT: eval の basic.jsonl は 100% を維持すること。リグレッションは辞書/頻度/コスト関数のバランス崩れを意味する
- 辞書ビルド (`make dict`) は `data/raw/SKK-JISYO.L` が必要。なければ eval に影響はない
- `nn` のローマ字変換は `nn_pending` フラグで制御。変更時は `onna`/`sannpo`/`annna` のテストを確認
- Viterbi のコスト定数変更は eval の両方に影響するため慎重に。MAX_SCORE_CONTRIBUTION と SEGMENT_PENALTY のバランスが重要
- 学んだ知識は `docs/knowledge.md`に書き込むこと。ステップごと参照すること
- 毎日の開発で感じたことを `./docs/diary/yyyy-mm-dd-{agent_roll}`で書くこと。人間は見ないのでAIの素直な感想を書くこと
