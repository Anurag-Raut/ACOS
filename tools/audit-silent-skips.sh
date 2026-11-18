#!/usr/bin/env bash
# audit-silent-skips.sh -- list every test site that reports a missing
# prerequisite and then returns (HYG3). A silent skip makes `cargo test` print
# `test result: ok` while the test asserted nothing.
#
#   tools/audit-silent-skips.sh          # list the sites, exit 1 if any
#
# Reviewed non-test sites are listed in tools/silent-skip-allowlist.txt.
# A site is an `eprintln!`/`println!`/`print!` whose text says skip/skipping/
# SKIP (not a comment), with a `return`, `continue` or `Ok(())` within the next
# three lines, or any `SKIP` literal that is printed. Sites converted to
# `acos_test_support::require_prereq` do not match: they panic by default.
set -uo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root" || exit 2
hits=0
while IFS= read -r file; do
  awk -v F="$file" '
    { line[NR] = $0 }
    END {
      for (i = 1; i <= NR; i++) {
        l = line[i]
        if (l ~ /^[[:space:]]*\/\//) continue
        # A print macro whose text (on this line or in the next three, for a
        # multi-line `eprintln!(\n "SKIP ..."\n)`) says skip, followed by a
        # return/continue/Ok(()) within six lines (HYG4: the one-line-only rule
        # missed multi-line prints, e.g. corpus.rs, text_coverage.rs).
        if (l ~ /(e?print(ln)?!)[[:space:]]*\(/) {
          said = 0
          for (j = i; j <= i + 3 && j <= NR; j++) {
            if (tolower(line[j]) ~ /skip/) { said = 1; break }
          }
          if (!said) continue
          silent = 0
          for (j = i; j <= i + 6 && j <= NR; j++) {
            if (line[j] ~ /(return|continue|Ok\(\(\)\))/) { silent = 1; break }
          }
          if (silent) printf "%s:%d: %s\n", F, i, l
        }
      }
    }' "$file"
done < <(find crates -name '*.rs' -not -path '*/target/*' | grep -v '/acos-test-support/' | sort) > .cache/hyg3-audit.txt 2>/dev/null
# Drop the reviewed non-test sites listed in silent-skip-allowlist.txt.
allow="tools/silent-skip-allowlist.txt"
if [ -f "$allow" ]; then
  while IFS=$'\t' read -r apath needle; do
    case "$apath" in ''|'#'*) continue ;; esac
    grep -v -F "$apath:" .cache/hyg3-audit.txt | cat > .cache/hyg3-audit.tmp
    grep -F "$apath:" .cache/hyg3-audit.txt | grep -v -F "$needle" >> .cache/hyg3-audit.tmp || true
    mv .cache/hyg3-audit.tmp .cache/hyg3-audit.txt
  done < "$allow"
fi
hits=$(wc -l < .cache/hyg3-audit.txt | tr -d ' ')
cat .cache/hyg3-audit.txt
echo "SILENT-SKIP SITES: $hits"
[ "$hits" -eq 0 ]
