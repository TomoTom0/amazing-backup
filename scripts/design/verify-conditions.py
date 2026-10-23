#!/usr/bin/env python3
"""amazing-backup テスト設計条件書の機械検証 (test-structure skill準拠)。

条件書: tests/design/*.toml(エリア別分割)
テスト: [covers:<id>] タグ付きのテストファイル。走査対象:
  - tests/**/test_*.py  (Pythonテスト。混在余地のため残す。現状はRustテストのみ)
  - tests/**/*.rs       (cargo結合テスト tests/ 直下。tests/design/ は除外)
  - src/**/*.rs         (cargo単体テスト src 内 #[cfg(test)]。設計8章)
  Rustテストではテスト関数のdoc commentとしてタグを付ける(設計8章):
      /// [covers:<stable-id>]
      #[test]
      fn ...() { ... }

検査:
  (a) coversタグが実在する条件idを指すこと(dangling tag検出)
  (b) 全条件が1つ以上のタグでcoverされていること
      テストファイルが1つも無い段階は警告表示のみで fail しない
      (テストコード作成後は fail する)
  (c) 条件書tomlのschema検査(id形式・必須フィールド・expect_*存在・given実質性)
  (d) source_lines をタグ走査結果(正本)へ自動同期する(手書き不要)
  (e) [[excluded]] の構造検査(item/source/reason 空欄をfail)

機械検証のpassは網羅・内容の正当性の証明ではない。出典×条件の内容照合は
レビュー・検収時の人手確認で行う(test-structure §5)。

実行:
  uv run python scripts/design/verify-conditions.py [--strict-source]

戻り値: 全検査pass時 0 / errorあり時 1
"""

from __future__ import annotations

import argparse
import re
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
DESIGN_DIR = ROOT / "tests" / "design"
TESTS_DIR = ROOT / "tests"
SRC_DIR = ROOT / "src"

COVER_TAG_RE = re.compile(r"\[covers:([A-Za-z0-9._-]+)\]")
CONDITION_ID_RE = re.compile(r"^[a-z0-9]+(-[a-z0-9]+)*\.[a-z0-9]+(-[a-z0-9]+)*$")
ID_FIELD_RE = re.compile(r'^\s*id\s*=\s*"([^"]+)"\s*$')
SOURCE_LINES_FIELD_RE = re.compile(r'^\s*source_lines\s*=\s*"([^"]*)"\s*$')
TABLE_HEADER_RE = re.compile(r"^\s*\[")  # [meta] / [[condition]] / [[excluded]] 等

EXPECT_KEYS = ("expect_return", "expect_return_shape", "expect_throw", "expect_no_throw")
REQUIRED_CONDITION_FIELDS = ("id", "target_function", "source", "description", "given")
REQUIRED_EXCLUDED_FIELDS = ("item", "source", "reason")
GIVEN_PLACEHOLDERS = {"", "-", "なし", "none", "n/a", "todo", "tbd", "x", "?"}
STRICT_SOURCE_MARKERS = ("tmp/tasks/", "docs/", "TASK-")


def collect_test_tags(errors: list[str]) -> tuple[list[Path], dict[str, list[str]]]:
    """テストファイルから [covers:<id>] タグを走査する(同期の正本)。

    走査対象(設計8章):
      - tests/**/test_*.py  (Python混在余地。現状はRustテストのみ)
      - tests/**/*.rs       (cargo結合テスト。tests/design/ は対象外)
      - src/**/*.rs         (cargo単体テスト #[cfg(test)]。doc commentのタグを含む)
    """
    found: set[Path] = set()
    if TESTS_DIR.is_dir():
        for pat in ("test_*.py", "*.rs"):
            for p in TESTS_DIR.rglob(pat):
                if DESIGN_DIR not in p.parents:
                    found.add(p)
    if SRC_DIR.is_dir():
        found.update(SRC_DIR.rglob("*.rs"))
    test_files = sorted(found)
    covers: dict[str, list[str]] = {}
    for tf in test_files:
        try:
            text = tf.read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError) as e:
            errors.append(f"テストファイル読込失敗 {tf}: {e}")
            continue
        rel = tf.relative_to(ROOT).as_posix()
        for lineno, line in enumerate(text.splitlines(), start=1):
            for m in COVER_TAG_RE.finditer(line):
                covers.setdefault(m.group(1), []).append(f"{rel}:{lineno}")
    return test_files, covers


def check_meta(path: Path, data: dict, errors: list[str]) -> None:
    meta = data.get("meta")
    if not isinstance(meta, dict):
        errors.append(f"{path.name}: [meta] セクションが無い")
        return
    feature = meta.get("feature")
    if not isinstance(feature, str) or not feature.strip():
        errors.append(f"{path.name}: [meta].feature が空欄")
    elif feature != path.stem:
        errors.append(
            f"{path.name}: [meta].feature ({feature!r}) がファイルのstem ({path.stem!r}) と不一致"
        )
    source_file = meta.get("source_file")
    if not isinstance(source_file, str) or not source_file.strip():
        errors.append(f"{path.name}: [meta].source_file が空欄")
    verified = meta.get("verified")
    if not isinstance(verified, bool):
        errors.append(f"{path.name}: [meta].verified がboolでない")
    elif verified:
        for key in ("verified_at", "verified_by", "test_command", "source_hash"):
            v = meta.get(key)
            if not isinstance(v, str) or not v.strip():
                errors.append(
                    f"{path.name}: verified=true のため [meta].{key} が必要(検証記録で裏付ける)"
                )


def check_conditions(
    path: Path,
    data: dict,
    seen_ids: dict[str, str],
    errors: list[str],
    warnings: list[str],
    strict_source: bool,
) -> int:
    conds = data.get("condition", [])
    if not conds:
        warnings.append(f"{path.name}: [[condition]] が0件")
        return 0
    count = 0
    for idx, cond in enumerate(conds):
        label = f"{path.name} [[condition]][{idx}]"
        if not isinstance(cond, dict):
            errors.append(f"{label}: テーブル形式でない")
            continue
        count += 1
        cid = cond.get("id")
        if isinstance(cid, str) and cid.strip():
            if not CONDITION_ID_RE.match(cid):
                errors.append(
                    f"{label}: id 形式違反(stable idは <area>.<kebab-topic>、"
                    f"小文字ASCII英数字と-のみ、区切りの . は1つ): {cid!r}"
                )
            if cid in seen_ids:
                errors.append(
                    f"{label}: id 重複({cid} は {seen_ids[cid]} でも定義済み)"
                )
            else:
                seen_ids[cid] = path.name
        else:
            cid = None
        for field in REQUIRED_CONDITION_FIELDS:
            v = cond.get(field)
            if not isinstance(v, str) or not v.strip():
                errors.append(f"{label}: 必須フィールド {field} が空欄")
        given = cond.get("given")
        if isinstance(given, str) and given.strip().lower() in GIVEN_PLACEHOLDERS:
            errors.append(f"{label}: given が実質を持たない({given!r})")
        if not any(
            isinstance(cond.get(k), str) and cond.get(k).strip() for k in EXPECT_KEYS
        ):
            errors.append(
                f"{label}: expect_return / expect_return_shape / expect_throw / "
                "expect_no_throw のいずれか1つ以上が必要"
            )
        if cid is not None:
            label = f"{path.name} {cid}"
        if strict_source:
            src = cond.get("source")
            if (
                isinstance(src, str)
                and src.strip()
                and not any(marker in src for marker in STRICT_SOURCE_MARKERS)
            ):
                errors.append(
                    f"{label}: --strict-source: source に出典参照"
                    f"(tmp/tasks/・docs/・TASK-)が含まれない: {src!r}"
                )
        if cond.get("verified") is False:
            reason = cond.get("unverifiable_reason")
            if not isinstance(reason, str) or not reason.strip():
                errors.append(
                    f"{label}: verified=false の条件には unverifiable_reason が必要"
                )
    return count


def check_excluded(path: Path, data: dict, errors: list[str]) -> int:
    entries = data.get("excluded", [])
    for idx, entry in enumerate(entries):
        label = f"{path.name} [[excluded]][{idx}]"
        if not isinstance(entry, dict):
            errors.append(f"{label}: テーブル形式でない")
            continue
        for field in REQUIRED_EXCLUDED_FIELDS:
            v = entry.get(field)
            if not isinstance(v, str) or not v.strip():
                errors.append(f"{label}: 必須フィールド {field} が空欄")
    return len(entries)


def sync_source_lines(path: Path, covers: dict[str, list[str]]) -> bool:
    """各 [[condition]] の source_lines をタグ走査結果へ同期する。

    - 既存の source_lines 行はその場で値を置換する
    - 行が無く新値が非空なら、ブロック内の最後の非空行の直後に挿入する
    - 新値が空(タグなし)なら行を挿入せず、既存行のみ空値へ更新する
    """
    try:
        text = path.read_text(encoding="utf-8")
    except (OSError, UnicodeDecodeError):
        return False
    lines = text.splitlines(keepends=True)
    blocks: list[tuple[int, int]] = []  # (start, end_exclusive)
    for i, line in enumerate(lines):
        if line.strip() == "[[condition]]":
            end = len(lines)
            for j in range(i + 1, len(lines)):
                if TABLE_HEADER_RE.match(lines[j]):
                    end = j
                    break
            blocks.append((i, end))
    if not blocks:
        return False
    changed = False
    for start, end in reversed(blocks):
        block = lines[start:end]
        cid = None
        sl_idx = None
        for k, line in enumerate(block):
            m = ID_FIELD_RE.match(line)
            if m is not None and cid is None:
                cid = m.group(1)
                continue
            m = SOURCE_LINES_FIELD_RE.match(line)
            if m is not None and sl_idx is None:
                sl_idx = k
        if cid is None:
            continue
        new_val = "; ".join(sorted(covers.get(cid, [])))
        if sl_idx is not None:
            old_val = SOURCE_LINES_FIELD_RE.match(block[sl_idx]).group(1)  # type: ignore[union-attr]
            if old_val != new_val:
                block[sl_idx] = f'source_lines = "{new_val}"\n'
                changed = True
        elif new_val:
            insert_at = len(block)
            for k in range(len(block) - 1, -1, -1):
                if block[k].strip():
                    insert_at = k + 1
                    break
            block.insert(insert_at, f'source_lines = "{new_val}"\n')
            changed = True
        lines[start:end] = block
    if changed:
        path.write_text("".join(lines), encoding="utf-8")
    return changed


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="テスト設計条件書(tests/design/*.toml)と[covers:<id>]タグの機械検証"
    )
    parser.add_argument(
        "--strict-source",
        action="store_true",
        help="source欄へ出典参照(tmp/tasks/・docs/・TASK-)が含まれることを検査する(将来用)",
    )
    args = parser.parse_args(argv)

    errors: list[str] = []
    warnings: list[str] = []

    if not DESIGN_DIR.is_dir():
        print(f"error: 条件書ディレクトリが無い: {DESIGN_DIR}", file=sys.stderr)
        return 1
    paths = sorted(DESIGN_DIR.glob("*.toml"))
    if not paths:
        print(f"error: 条件書tomlが1つも無い: {DESIGN_DIR}", file=sys.stderr)
        return 1

    # テスト側タグ走査(同期の正本)
    test_files, covers = collect_test_tags(errors)

    # 条件書parse
    parsed: list[tuple[Path, dict]] = []
    for path in paths:
        try:
            with path.open("rb") as f:
                parsed.append((path, tomllib.load(f)))
        except tomllib.TOMLDecodeError as e:
            errors.append(f"{path.name}: TOML parse失敗: {e}")
        except OSError as e:
            errors.append(f"{path.name}: 読込失敗: {e}")

    # (c)(e) schema検査
    seen_ids: dict[str, str] = {}
    n_conditions = 0
    n_excluded = 0
    for path, data in parsed:
        check_meta(path, data, errors)
        n_conditions += check_conditions(
            path, data, seen_ids, errors, warnings, args.strict_source
        )
        n_excluded += check_excluded(path, data, errors)

    # (a) dangling covers tag
    for tag_id in sorted(covers):
        if tag_id not in seen_ids:
            for loc in covers[tag_id]:
                errors.append(
                    f"(a) dangling covers tag: [{tag_id}] at {loc} "
                    "(条件書に存在しないidを参照している)"
                )

    # (b) 網羅検査
    uncovered = [cid for cid in seen_ids if cid not in covers]
    if not test_files:
        if uncovered:
            warnings.append(
                "(b) テストファイル(tests/**/test_*.py・tests/**/*.rs・src/**/*.rs)が"
                "1つも無いため網羅検査は警告扱い: "
                f"{len(uncovered)}/{len(seen_ids)} 条件が未cover(テストコード作成後はfailする)"
            )
    else:
        for cid in sorted(uncovered):
            errors.append(
                f"(b) 未coverの条件: {cid} ([covers:{cid}] タグがテスト走査対象に無い)"
            )

    # (d) source_lines自動同期
    synced = [path.name for path, _ in parsed if sync_source_lines(path, covers)]

    # レポート
    print("== verify-conditions ==")
    print(
        f"design: {len(paths)} files / {n_conditions} conditions / "
        f"{n_excluded} excluded entries"
    )
    print(
        f"tests: {len(test_files)} files / "
        f"{sum(len(v) for v in covers.values())} covers tags / "
        f"{len(seen_ids) - len(uncovered)} covered conditions"
    )
    for w in warnings:
        print(f"WARNING: {w}")
    if synced:
        print(f"(d) source_lines同期: {', '.join(synced)}")
    else:
        print("(d) source_lines同期: 変更なし")
    for e in errors:
        print(f"ERROR: {e}")
    if errors:
        print(f"result: FAIL ({len(errors)} errors)")
        return 1
    print("result: PASS")
    return 0


if __name__ == "__main__":
    sys.exit(main())
