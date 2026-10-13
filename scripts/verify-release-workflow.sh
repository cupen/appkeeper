#!/usr/bin/env bash
# Script-level verification for .github/workflows/release.yml.
#
# GitHub Actions cannot run locally, so this harness extracts the *shell* of
# each workflow step that carries release logic and exercises it against
# synthetic inputs: the version gate, the webui placeholder assertion, archive
# packaging, SHA256SUMS generation/format, the archive-completeness gate, the
# generated-notes assembly (byte-identity of the embedded checksum block) and
# the prerelease/draft flags passed to `gh release create`.
#
# It is deliberately NOT an end-to-end test: nothing here compiles Rust, builds
# the frontend, talks to GitHub or creates a release. Those paths are marked
# "needs CI" in the change tasks.
#
# Usage: scripts/verify-release-workflow.sh
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
workflow="$repo_root/.github/workflows/release.yml"

pass=0
fail=0
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

ok()   { printf '  \033[32mPASS\033[0m %s\n' "$1"; pass=$((pass + 1)); }
bad()  { printf '  \033[31mFAIL\033[0m %s\n' "$1"; fail=$((fail + 1)); }
head_() { printf '\n== %s ==\n' "$1"; }

# --- workflow step extraction ------------------------------------------------
# Pull the `run: |` body of the step whose `- name:` matches $1, dedented.
extract_step() {
  python3 - "$workflow" "$1" <<'PY'
import sys
text = open(sys.argv[1]).read()
name = sys.argv[2]
try:
    i = text.index("- name: " + name)
except ValueError:
    sys.exit(f"step not found: {name}")
j = text.index("run: |", i)
lines = text[j + len("run: |"):].split("\n")
out, indent = [], None
for ln in lines[1:]:
    if ln.strip() == "":
        out.append("")
        continue
    cur = len(ln) - len(ln.lstrip())
    if indent is None:
        indent = cur
    if cur < indent:
        break
    out.append(ln[indent:])
print("\n".join(out).rstrip())
PY
}

# --- fixtures ----------------------------------------------------------------
new_repo() {
  local d="$tmp/$1"
  rm -rf "$d"; mkdir -p "$d"
  printf '[package]\nname = "appkeeper"\nversion = "0.1.0"\nedition = "2024"\n' > "$d/Cargo.toml"
  printf 'MIT License\n' > "$d/LICENSE"
  printf '# appkeeper\n' > "$d/README.md"
  mkdir -p "$d/target/x86_64-unknown-linux-musl/release" \
           "$d/target/x86_64-unknown-linux-gnu/release" \
           "$d/target/x86_64-pc-windows-msvc/release"
  printf '#!/bin/sh\necho ak musl\n' > "$d/target/x86_64-unknown-linux-musl/release/ak"
  printf '#!/bin/sh\necho ak gnu\n'  > "$d/target/x86_64-unknown-linux-gnu/release/ak"
  printf 'MZfake\n' > "$d/target/x86_64-pc-windows-msvc/release/ak.exe"
  echo "$d"
}

# --- 1.1 trigger + permissions ----------------------------------------------
head_ "1.1 trigger and top-level permissions"
# Comments in the workflow mention the rejected alternatives by name, so strip
# comment lines before asserting the triggers/permissions that are *declared*.
decls="$(grep -v '^[[:space:]]*#' "$workflow")"
if grep -qE "^on:[[:space:]]*$" <<<"$decls" && grep -qF "tags: ['v*']" <<<"$decls"; then
  ok "trigger is push.tags ['v*']"
else
  bad "expected push.tags ['v*'] trigger"
fi
# The tag filter must be exactly ['v*']: any extra pattern (e.g. a nightly-*
# tag) would let non-version tags trigger a release, which the spec forbids.
if grep -qE "^    tags: \['v\*'\]$" <<<"$decls"; then
  ok "tag filter is exactly ['v*'] (non-version tags like nightly-* do not match)"
else
  bad "tag filter is not exactly ['v*'] — extra tag patterns would trigger releases"
fi
# Isolate the `on:` block (its keys are indented two spaces) and assert no
# other event is registered there.
on_block="$(awk '/^on:/{f=1; next} /^[^[:space:]]/{f=0} f' <<<"$decls")"
if grep -qE '^  (workflow_dispatch|release|schedule|pull_request|push):' <<<"$on_block" \
   && [ "$(grep -cE '^  [a-z_]+:' <<<"$on_block")" -eq 1 ]; then
  ok "only the push trigger is registered"
else
  bad "an extra trigger is registered under on:"
fi
if grep -qE '^permissions:' <<<"$decls" && grep -qE '^  contents: write$' <<<"$decls"; then
  ok "top-level permissions: contents: write"
else
  bad "top-level permissions: contents: write not declared"
fi
if grep -qF 'github.token' <<<"$decls" && ! grep -q 'secrets\.' <<<"$decls"; then
  ok "credentials via github.token, no personal PAT / secrets"
else
  bad "credentials must be github.token only"
fi
# The push trigger must be scoped to tags only: a `branches:`/`paths:` filter
# added under push would silently widen the release trigger to branch pushes.
push_block="$(awk '/^  push:/{f=1; next} /^  [a-z_]+:/{f=0} f' <<<"$on_block")"
if grep -qE '^    tags:' <<<"$push_block" \
   && ! grep -qE '^    (branches|branches-ignore|paths|paths-ignore):' <<<"$push_block"; then
  ok "push is scoped to tags only (no branches/paths filter)"
else
  bad "push carries a non-tag filter (branches/paths), widening the trigger"
fi
# Least privilege: `contents: write` and nothing else (spec "权限收敛到 contents 写").
perm_block="$(awk '/^permissions:/{f=1; next} /^[^[:space:]]/{f=0} f' <<<"$decls")"
if [ "$(grep -cE '^  [a-z-]+:' <<<"$perm_block")" -eq 1 ] && grep -qE '^  contents: write$' <<<"$perm_block"; then
  ok "permissions grant only contents: write (everything else none)"
else
  bad "permissions grant more than contents: write"
fi

# --- 1.2 version gate --------------------------------------------------------
head_ "1.2 version gate: tag vs Cargo.toml"
extract_step "Check tag matches Cargo.toml version" > "$tmp/version_gate.sh"
r="$(new_repo gate)"
if ( cd "$r" && GITHUB_REF_NAME=v0.1.0 bash "$tmp/version_gate.sh" >/dev/null 2>&1 ); then
  ok "matching tag v0.1.0 -> exit 0"
else
  bad "matching tag v0.1.0 should exit 0"
fi
if ( cd "$r" && GITHUB_REF_NAME=v9.9.9 bash "$tmp/version_gate.sh" >/dev/null 2>&1 ); then
  bad "drifting tag v9.9.9 should exit non-zero"
else
  ok "drifting tag v9.9.9 -> exit non-zero"
fi
if ( cd "$r" && GITHUB_REF_NAME=v0.1.0 bash "$tmp/version_gate.sh" 2>&1 | grep -q 'version OK' ); then
  ok "match prints confirmation"
else
  bad "match should print confirmation"
fi
# Cargo.toml must not be rewritten by the gate (design D2).
before="$(cat "$r/Cargo.toml")"
( cd "$r" && GITHUB_REF_NAME=v9.9.9 bash "$tmp/version_gate.sh" >/dev/null 2>&1 || true )
if [ "$before" = "$(cat "$r/Cargo.toml")" ]; then
  ok "Cargo.toml is untouched by the gate (read-only)"
else
  bad "Cargo.toml was modified by the gate"
fi
# The gate must actually be able to read a version: an unreadable/absent
# version line has to fail rather than pass vacuously.
r="$(new_repo gate_noversion)"
printf '[package]\nname = "appkeeper"\nedition = "2024"\n' > "$r/Cargo.toml"
if ( cd "$r" && GITHUB_REF_NAME=v0.1.0 bash "$tmp/version_gate.sh" >/dev/null 2>&1 ); then
  bad "a Cargo.toml with no version field should fail the gate"
else
  ok "missing Cargo.toml version -> fail (not a vacuous pass)"
fi
# The version must be read from [package], not any bare `version =` line: a
# future [workspace.package] version would otherwise be picked up silently.
r="$(new_repo gate_workspace_version)"
printf '[workspace.package]\nversion = "9.9.9"\n\n[package]\nname = "appkeeper"\nversion = "0.1.0"\n' > "$r/Cargo.toml"
if ( cd "$r" && GITHUB_REF_NAME=v0.1.0 bash "$tmp/version_gate.sh" 2>&1 | grep -q "version OK" ); then
  ok "gate reads [package].version, ignoring a [workspace.package] version"
else
  bad "gate picked up the wrong version (must scope to [package])"
fi
if ( cd "$r" && GITHUB_REF_NAME=v9.9.9 bash "$tmp/version_gate.sh" >/dev/null 2>&1 ); then
  bad "gate accepted the [workspace.package] version (must scope to [package])"
else
  ok "gate rejects a tag matching only the [workspace.package] version"
fi

# --- 1.3 verify job: quality gate on both platforms -------------------------
head_ "1.3 verify job: cargo test on ubuntu + windows before build"
# Isolate the verify job (two-space-indented job keys delimit it).
verify_block="$(awk '/^  verify:/{f=1} f && /^  [a-z_]+:/ && !/^  verify:/{f=0} f' "$workflow")"
if grep -qE '^        os: \[ubuntu-latest, windows-latest\]$' <<<"$verify_block"; then
  ok "verify matrix covers ubuntu-latest + windows-latest"
else
  bad "verify matrix must cover ubuntu-latest + windows-latest (both run tests)"
fi
if grep -qF 'run: cargo test --workspace' <<<"$verify_block"; then
  ok "verify runs cargo test --workspace"
else
  bad "verify job must run the full test suite (cargo test --workspace)"
fi
if grep -qF 'runs-on: ${{ matrix.os }}' <<<"$verify_block"; then
  ok "verify runs-on matrix.os (tests on each platform)"
else
  bad "verify runs-on must be matrix.os"
fi
# Each target must pair with the right archive extension and binary name
# (Windows -> .zip / ak.exe; Linux -> .tar.gz / ak). A swap here would produce
# an archive whose name lies about its platform.
assert_row() { # triple ext bin
  local row
  row="$(awk -v t="$1" '$0 ~ ("target: " t "$"){f=1} f&&/archive_ext:/{e=$2} f&&/^ *bin:/{b=$2; print e" "b; exit}' "$workflow")"
  if [ "$row" = "$2 $3" ]; then
    ok "matrix row $1 -> ext=$2 bin=$3"
  else
    bad "matrix row $1 pairs with '${row:-<none>}', expected '$2 $3'"
  fi
}
assert_row x86_64-unknown-linux-musl tar.gz ak
assert_row x86_64-unknown-linux-gnu  tar.gz ak
assert_row x86_64-pc-windows-msvc    zip    ak.exe

# spec: 「musl 产物不依赖 glibc」— the workflow must actually assert the musl
# binary is static, not merely build it. Without this step the scenario has no
# coverage at all.
musl_static_step="$(awk '/name: Assert musl artifact is statically linked/{f=1} f{print} f&&/^ *- name: /&&!/Assert musl/{exit}' "$workflow")"
if grep -qF "matrix.target == 'x86_64-unknown-linux-musl'" <<<"$musl_static_step"; then
  ok "musl static-link assertion is gated to the musl target only"
else
  bad "musl static-link assertion must run only for the musl target"
fi
if grep -qF 'ldd' <<<"$musl_static_step" && grep -qi 'statically linked' <<<"$musl_static_step"; then
  ok "musl artifact is asserted statically linked (ldd / libc check)"
else
  bad "workflow must assert the musl artifact is statically linked"
fi

# --- 2.1 build matrix --------------------------------------------------------
head_ "2.1 build matrix: three targets, Windows scheduled once"
if grep -qF 'needs: verify' "$workflow"; then
  ok "build job gates on verify"
else
  bad "build job must declare needs: verify"
fi
for t in x86_64-unknown-linux-musl x86_64-unknown-linux-gnu x86_64-pc-windows-msvc; do
  n="$(grep -cF "target: $t" "$workflow")"
  if [ "$n" -eq 1 ]; then ok "matrix entry for $t (once)"; else bad "$t appears $n times in the matrix"; fi
done
if [ "$(grep -cF 'os: windows-latest' "$workflow")" -eq 1 ]; then
  ok "windows-latest appears exactly once (Windows runs once)"
else
  bad "windows-latest should appear exactly once"
fi
if [ "$(grep -cF 'os: ubuntu-24.04' "$workflow")" -eq 2 ]; then
  ok "ubuntu-24.04 appears twice (musl + gnu)"
else
  bad "ubuntu-24.04 should appear twice"
fi

# --- 2.2 toolchain setup -----------------------------------------------------
head_ "2.2 toolchain: rust target + Node 22 + pnpm 11.24.0"
if grep -qF 'rustup target add ${{ matrix.target }}' "$workflow"; then
  ok "rustup target add per matrix target"
else
  bad "rustup target add step missing"
fi
if grep -qF "node-version: '22'" "$workflow"; then
  ok "actions/setup-node with node-version 22"
else
  bad "setup-node node-version 22 missing"
fi
if grep -qF 'pnpm/action-setup' "$workflow" && grep -qF 'version: 11.24.0' "$workflow"; then
  ok "pnpm/action-setup pinned to 11.24.0 (matches webui/package.json)"
else
  bad "pnpm/action-setup 11.24.0 missing"
fi
# The pinned pnpm version must agree with webui/package.json.
pkg_manager="$(sed -n 's/.*"packageManager"[[:space:]]*:[[:space:]]*"pnpm@\([^"]*\)".*/\1/p' "$repo_root/webui/package.json")"
if [ "$pkg_manager" = "11.24.0" ]; then
  ok "webui/package.json packageManager is pnpm@$pkg_manager (in sync)"
else
  bad "webui/package.json packageManager is $pkg_manager, workflow pins 11.24.0"
fi

# --- 2.4 build command -------------------------------------------------------
head_ "2.4 release build command"
if grep -qF 'cargo build --release --target ${{ matrix.target }} -p appkeeper --bin ak' "$workflow"; then
  ok "cargo build targets the matrix triple, package appkeeper, bin ak"
else
  bad "release build command mismatch"
fi

# --- 2.6 upload-artifact -----------------------------------------------------
head_ "2.6 upload-artifact"
if grep -qF 'uses: actions/upload-artifact@v4' "$workflow" \
   && grep -qF 'if-no-files-found: error' "$workflow" \
   && grep -qF 'name: ${{ matrix.target }}' "$workflow"; then
  ok "upload-artifact v4, name = target triple, if-no-files-found: error"
else
  bad "upload-artifact contract missing"
fi

# --- 2.3 webui placeholder assertion ----------------------------------------
head_ "2.3 webui placeholder assertion"
extract_step "Assert real webui bundle (not the placeholder)" > "$tmp/assert_webui.sh"
r="$(new_repo webui)"
mkdir -p "$r/webui"
if ( cd "$r" && bash "$tmp/assert_webui.sh" >/dev/null 2>&1 ); then
  bad "missing webui/dist/index.html should fail"
else
  ok "missing index.html -> fail"
fi
mkdir -p "$r/webui/dist"
printf '<html>placeholder</html>\n' > "$r/webui/dist/index.html"
printf 'placeholder\n' > "$r/webui/dist/.ak-placeholder"
if ( cd "$r" && bash "$tmp/assert_webui.sh" >/dev/null 2>&1 ); then
  bad "placeholder marker present should fail"
else
  ok "placeholder marker present -> fail"
fi
rm -f "$r/webui/dist/.ak-placeholder"
if ( cd "$r" && bash "$tmp/assert_webui.sh" >/dev/null 2>&1 ); then
  ok "real bundle (index.html, no marker) -> pass"
else
  bad "real bundle should pass"
fi

# --- 2.4b binary version self-check -----------------------------------------
head_ "2.4b binary --version self-check"
# The step interpolates `${{ matrix.target }}` / `${{ matrix.bin }}`; substitute
# the fixture's values since GitHub expressions are not expanded locally.
extract_step "Self-check binary version" \
  | sed -e 's/\${{ matrix\.target }}/x86_64-unknown-linux-gnu/g' \
        -e 's/\${{ matrix\.bin }}/ak/g' > "$tmp/selfcheck.sh"
r="$(new_repo selfcheck)"
mkdir -p "$r/target/x86_64-unknown-linux-gnu/release"
printf '#!/bin/sh\necho "ak 0.1.0"\n' > "$r/target/x86_64-unknown-linux-gnu/release/ak"
chmod +x "$r/target/x86_64-unknown-linux-gnu/release/ak"
if ( cd "$r" && GITHUB_REF_NAME=v0.1.0 bash "$tmp/selfcheck.sh" >/dev/null 2>&1 ); then
  ok "matching 'ak 0.1.0' passes"
else
  bad "matching binary version should pass"
fi
printf '#!/bin/sh\necho "ak 0.2.0"\n' > "$r/target/x86_64-unknown-linux-gnu/release/ak"
chmod +x "$r/target/x86_64-unknown-linux-gnu/release/ak"
if ( cd "$r" && GITHUB_REF_NAME=v0.1.0 bash "$tmp/selfcheck.sh" >/dev/null 2>&1 ); then
  bad "mismatched binary version should fail"
else
  ok "mismatched binary version -> fail"
fi
# A substring-only match must not slip through (e.g. "ak 0.1.0-extra").
printf '#!/bin/sh\necho "ak 0.1.0-extra"\n' > "$r/target/x86_64-unknown-linux-gnu/release/ak"
chmod +x "$r/target/x86_64-unknown-linux-gnu/release/ak"
if ( cd "$r" && GITHUB_REF_NAME=v0.1.0 bash "$tmp/selfcheck.sh" >/dev/null 2>&1 ); then
  bad "near-miss version should fail (exact match required)"
else
  ok "near-miss version -> fail (exact match required)"
fi

# --- 2.5 packaging -----------------------------------------------------------
head_ "2.5 packaging: naming + archive contents"
extract_step "Package archive (tar.gz)" > "$tmp/package_targz.sh"
r="$(new_repo pack)"
for target in x86_64-unknown-linux-musl x86_64-unknown-linux-gnu; do
  if ( cd "$r" && GITHUB_REF_NAME=v0.1.0 TARGET="$target" BIN=ak \
       GITHUB_ENV="$r/genv-$target" bash "$tmp/package_targz.sh" >/dev/null 2>&1 ); then
    ok "packaged $target"
  else
    bad "packaging failed for $target"
    continue
  fi
  name="ak-0.1.0-$target"
  listing="$(tar -tzf "$r/dist/$name.tar.gz" | grep -v '/$' | sed "s#^$name/##" | LC_ALL=C sort | tr '\n' ' ')"
  expected="$(printf '%s\n' LICENSE README.md ak | LC_ALL=C sort | tr '\n' ' ')"
  if [ "$listing" = "$expected" ]; then
    ok "$name.tar.gz contains exactly: $listing"
  else
    bad "$name.tar.gz unexpected contents: '$listing' (expected '$expected')"
  fi
  if grep -q "ARCHIVE_NAME=$name.tar.gz" "$r/genv-$target"; then
    ok "$name sets ARCHIVE_NAME"
  else
    bad "$name did not set ARCHIVE_NAME"
  fi
done
# The Windows .zip is produced by a pwsh step (Git Bash on the Windows runner
# has no `zip`). pwsh is unavailable here, so only the naming/contents contract
# is asserted structurally; executing Compress-Archive needs CI.
if grep -qF 'Compress-Archive -Path $staged -DestinationPath "dist/$name.zip"' "$workflow" \
   && grep -qF '$name = "ak-$version-$env:TARGET"' "$workflow" \
   && grep -qF '"ARCHIVE_NAME=$name.zip"' "$workflow"; then
  ok "Windows zip step: names ak-<version>-<triple>.zip and stages bin+LICENSE+README (execution needs CI)"
else
  bad "Windows zip step naming/contents contract not found"
fi

# --- 3.1 archive completeness gate ------------------------------------------
head_ "3.1 archive completeness gate"
extract_step "Require all three archives" > "$tmp/require.sh"
r="$(new_repo require)"
mkdir -p "$r/assets"
for f in ak-0.1.0-x86_64-unknown-linux-musl.tar.gz \
         ak-0.1.0-x86_64-unknown-linux-gnu.tar.gz \
         ak-0.1.0-x86_64-pc-windows-msvc.zip; do
  : > "$r/assets/$f"
done
if ( cd "$r" && GITHUB_REF_NAME=v0.1.0 bash "$tmp/require.sh" >/dev/null 2>&1 ); then
  ok "all three archives present -> pass"
else
  bad "complete set should pass"
fi
rm -f "$r/assets/ak-0.1.0-x86_64-unknown-linux-gnu.tar.gz"
if ( cd "$r" && GITHUB_REF_NAME=v0.1.0 bash "$tmp/require.sh" >/dev/null 2>&1 ); then
  bad "missing archive should fail"
else
  ok "one archive missing -> fail"
fi
# All three build jobs upload artifacts named by their target triple; the
# download step must merge them into one flat directory or the asset paths
# below (`assets/<archive>`) would not resolve.
if grep -qF 'uses: actions/download-artifact@v4' "$workflow" \
   && grep -qF 'merge-multiple: true' "$workflow"; then
  ok "download-artifact merges matrix artifacts into one directory"
else
  bad "download-artifact must set merge-multiple: true"
fi

# --- 3.2 SHA256SUMS format + verification -----------------------------------
head_ "3.2 SHA256SUMS: format and sha256sum -c"
extract_step "Compute SHA256SUMS" > "$tmp/sums.sh"
r="$(new_repo sums)"
mkdir -p "$r/assets"
printf 'one\n' > "$r/assets/ak-0.1.0-x86_64-unknown-linux-musl.tar.gz"
printf 'two\n' > "$r/assets/ak-0.1.0-x86_64-unknown-linux-gnu.tar.gz"
printf 'three\n' > "$r/assets/ak-0.1.0-x86_64-pc-windows-msvc.zip"
if ( cd "$r/assets" && bash "$tmp/sums.sh" >/dev/null 2>&1 ); then
  ok "SHA256SUMS generated and format check passed"
else
  bad "SHA256SUMS generation/format check failed"
fi
if [ "$(wc -l < "$r/assets/SHA256SUMS")" -eq 3 ]; then
  ok "SHA256SUMS has one line per archive"
else
  bad "SHA256SUMS line count != 3"
fi
if awk 'NF!=2 || $1 !~ /^[0-9a-f]{64}$/ || $2 ~ /\// { exit 1 }' "$r/assets/SHA256SUMS"; then
  ok "every line is '<64-hex>  <name>' with no path"
else
  bad "SHA256SUMS line format/path violation"
fi
if ( cd "$r/assets" && sha256sum -c SHA256SUMS >/dev/null 2>&1 ); then
  ok "sha256sum -c SHA256SUMS -> exit 0 (all OK)"
else
  bad "sha256sum -c should succeed"
fi
# The workflow must itself reject a malformed line (the format guard is the
# only thing standing between a path-prefixed hash and a broken SHA256SUMS).
sums_awk="$(grep -F "awk 'NF != 2" "$tmp/sums.sh" || true)"
if [ -n "$sums_awk" ]; then
  ok "workflow carries an explicit SHA256SUMS format guard"
  printf 'deadbeef  ok.tar.gz\n' > "$tmp/SHA256SUMS"
  if ( cd "$tmp" && eval "$sums_awk" >/dev/null 2>&1 ); then
    bad "format guard accepted a malformed line (short hash)"
  else
    ok "format guard rejects a malformed line (short hash)"
  fi
  printf '%064d  a.tar.gz  extra\n' 0 > "$tmp/SHA256SUMS"
  if ( cd "$tmp" && eval "$sums_awk" >/dev/null 2>&1 ); then
    bad "format guard accepted a 3-field line"
  else
    ok "format guard rejects a 3-field line"
  fi
  rm -f "$tmp/SHA256SUMS"
else
  bad "workflow has no SHA256SUMS format guard"
fi

# --- 3.3 release notes assembly + byte-identity -----------------------------
head_ "3.3 release notes: asset list + byte-identical checksum block"
extract_step "Assemble release notes" > "$tmp/notes.sh"
mkdir -p "$tmp/bin"
cat > "$tmp/bin/gh" <<'EOF'
#!/bin/sh
# stub: the generate-notes API is not reachable locally
echo "- feat: something (#1)"
EOF
chmod +x "$tmp/bin/gh"
r="$(new_repo notes)"
mkdir -p "$r/assets"
printf 'one\n' > "$r/assets/ak-0.1.0-x86_64-unknown-linux-musl.tar.gz"
printf 'two\n' > "$r/assets/ak-0.1.0-x86_64-unknown-linux-gnu.tar.gz"
printf 'three\n' > "$r/assets/ak-0.1.0-x86_64-pc-windows-msvc.zip"
( cd "$r/assets" && bash "$tmp/sums.sh" >/dev/null 2>&1 )
notes="$r/notes.md"
if ( cd "$r" && PATH="$tmp/bin:$PATH" GITHUB_REF_NAME=v0.1.0 GITHUB_REPOSITORY=cupen/xkeeper \
     bash "$tmp/notes.sh" > "$notes" 2>/dev/null ); then
  ok "notes assembled"
else
  bad "notes assembly failed"
fi
for needle in "## 资产" "## 校验和" "ak-0.1.0-x86_64-unknown-linux-musl.tar.gz" \
              "ak-0.1.0-x86_64-unknown-linux-gnu.tar.gz" "ak-0.1.0-x86_64-pc-windows-msvc.zip"; do
  if grep -qF "$needle" "$notes"; then
    ok "notes mention: $needle"
  else
    bad "notes missing: $needle"
  fi
done
# The auto-generated change list (spec "发布说明内容") must be present, fed by
# the generate-notes API output. Assert the section heading and that the stub's
# body landed inside the notes.
if grep -qF '## 变更' "$notes"; then
  ok "notes carry the auto-generated change-list heading"
else
  bad "notes missing the 变更 (auto-generated change list) section"
fi
if grep -qF 'feat: something' "$notes"; then
  ok "generated commit list is embedded in the notes"
else
  bad "generated commit list did not reach the notes"
fi
# The asset list must name platform and architecture for every archive, not
# just the filenames.
if grep -qF '| 平台 | 架构 | 文件 |' "$notes"; then
  ok "asset table declares 平台/架构/文件 columns"
else
  bad "asset table missing 平台/架构/文件 header"
fi
for row in 'Linux (musl, 静态) | x86_64 | `ak-0.1.0-x86_64-unknown-linux-musl.tar.gz`' \
           'Linux (glibc) | x86_64 | `ak-0.1.0-x86_64-unknown-linux-gnu.tar.gz`' \
           'Windows | x86_64 | `ak-0.1.0-x86_64-pc-windows-msvc.zip`'; do
  if grep -qF "$row" "$notes"; then
    ok "asset table row: $row"
  else
    bad "asset table missing/incorrect row: $row"
  fi
done
awk '/^```$/{n++; next} n==1{print}' "$notes" > "$tmp/embedded.txt"
if diff <(sed -e '$a\' "$tmp/embedded.txt") <(sed -e '$a\' "$r/assets/SHA256SUMS") >/dev/null; then
  ok "embedded code block is byte-identical to SHA256SUMS"
else
  bad "embedded code block differs from SHA256SUMS"
fi

# --- 3.4 prerelease detection + flags ---------------------------------------
head_ "3.4 gh release create: prerelease/draft/verify-tag flags"
extract_step "Create release" > "$tmp/create.sh"
cat > "$tmp/bin/gh" <<'EOF'
#!/bin/sh
echo "$@"
EOF
chmod +x "$tmp/bin/gh"
r="$(new_repo create)"
mkdir -p "$r/assets"
for f in ak-*-x86_64-unknown-linux-musl.tar.gz \
         ak-*-x86_64-unknown-linux-gnu.tar.gz \
         ak-*-x86_64-pc-windows-msvc.zip SHA256SUMS; do
  : > "$r/assets/$(basename "$f")"
done
run_create() {
  ( cd "$r" && PATH="$tmp/bin:$PATH" GITHUB_REF_NAME="$1" GITHUB_REPOSITORY=cupen/xkeeper \
    bash "$tmp/create.sh" 2>/dev/null )
}
# Formal, all three prerelease spellings, and a dash-bearing tag that is *not*
# one of them (must stay formal — only -rc/-beta/-alpha mark a prerelease).
for tag in v0.1.0 v0.2.0-rc.1 v0.2.0-beta.2 v0.3.0-alpha v0.1.0-preview.1 v1.0.0-nightly; do
  out="$(run_create "$tag")"
  if grep -q -- '--verify-tag' <<<"$out"; then ok "$tag: --verify-tag present"; else bad "$tag: --verify-tag missing"; fi
  if grep -q -- '--draft' <<<"$out"; then bad "$tag: must not pass --draft"; else ok "$tag: no --draft"; fi
  if [[ "$tag" =~ -(rc|beta|alpha) ]]; then
    if grep -q -- '--prerelease' <<<"$out"; then ok "$tag: --prerelease present"; else bad "$tag: --prerelease missing"; fi
  else
    if grep -q -- '--prerelease' <<<"$out"; then bad "$tag: unexpected --prerelease"; else ok "$tag: no --prerelease (formal release)"; fi
  fi
  if grep -qF "assets/ak-${tag#v}-x86_64-unknown-linux-musl.tar.gz" <<<"$out" \
     && grep -qF "assets/ak-${tag#v}-x86_64-unknown-linux-gnu.tar.gz" <<<"$out" \
     && grep -qF "assets/ak-${tag#v}-x86_64-pc-windows-msvc.zip" <<<"$out" \
     && grep -qF "assets/SHA256SUMS" <<<"$out"; then
    ok "$tag: all three archives + SHA256SUMS passed to gh"
  else
    bad "$tag: asset list incomplete"
  fi
done
# The release job has no checkout step, so `gh release create` must be told the
# repo explicitly; and the assembled notes file must be what gets published.
out="$(run_create v0.1.0)"
if grep -qF -- '--repo cupen/xkeeper' <<<"$out"; then
  ok "--repo <owner/name> passed (release job has no checkout)"
else
  bad "--repo missing: gh would run outside a repository in the release job"
fi
if grep -qF -- '--notes-file /tmp/release-notes.md' <<<"$out"; then
  ok "--notes-file points at the assembled release notes"
else
  bad "--notes-file /tmp/release-notes.md missing"
fi
if grep -qF -- '--title' <<<"$out"; then
  ok "release title is set explicitly"
else
  bad "release title not set"
fi

# --- 3.5 failure path --------------------------------------------------------
head_ "3.5 failure path: no continue-on-error, release gates on build"
if grep -qE '^[[:space:]]*continue-on-error:' <<<"$decls"; then
  bad "continue-on-error is set somewhere"
else
  ok "no continue-on-error anywhere"
fi
if grep -qF 'needs: build' "$workflow" \
   && [ "$(grep -cF 'needs: build' "$workflow")" -eq 1 ]; then
  ok "release job declares needs: build (skipped when any target fails)"
else
  bad "release job must declare needs: build"
fi

# --- 4.1 / 4.2 README --------------------------------------------------------
head_ "4.1/4.2 README: install & verify section + release process"
readme="$repo_root/README.md"
if grep -qF '## 安装与校验' "$readme"; then
  ok "README has 安装与校验 section"
else
  bad "README 安装与校验 section missing"
fi
for needle in 'sha256sum -c' 'ak-${version}-${target}/ak" --version' 'alpine' 'tar -xzf' \
              'x86_64-pc-windows-msvc.zip' 'Expand-Archive'; do
  if grep -qF "$needle" "$readme"; then ok "README mentions: $needle"; else bad "README missing: $needle"; fi
done
if grep -qF 'git tag v' "$readme" && grep -qF 'git push origin v' "$readme" \
   && grep -qF 'Cargo.toml' "$readme" && grep -qF '不一致会被流水线拒绝' "$readme"; then
  ok "README documents release order (Cargo.toml -> commit -> tag -> push) and the drift rejection"
else
  bad "README release-process ordering / drift note missing"
fi

# --- summary -----------------------------------------------------------------
printf '\n----------------------------------------\n'
printf 'passed: %d   failed: %d\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
