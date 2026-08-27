#!/usr/bin/env bash
# Release one tested commit, then make the Homebrew cask point at its macOS artifact.
set -euo pipefail

readonly repo="TakumiHendricksDev/worktreemanager"
readonly tap_repo="TakumiHendricksDev/homebrew-tap"
readonly tap_cask="Casks/wtm.rb"

step() { printf '\033[1;34m▸\033[0m %s\n' "$1"; }
ok() { printf '\033[1;32m✓\033[0m %s\n' "$1"; }
die() {
    printf '\033[1;31m✗\033[0m %s\n' "$1" >&2
    exit 1
}

require() {
    command -v "$1" >/dev/null 2>&1 || die "missing required command: $1"
}

metadata_version() {
    case "$1" in
        Cargo.toml)
            sed -n '/^\[workspace.package\]/,/^\[/ s/^version[[:space:]]*=[[:space:]]*"\([^"]*\)"/\1/p' "$1" | head -n 1
            ;;
        package.json | src-tauri/tauri.conf.json)
            sed -n 's/^[[:space:]]*"version":[[:space:]]*"\([^"]*\)",/\1/p' "$1" | head -n 1
            ;;
    esac
}

wait_for_run() {
    local workflow="$1"
    local sha="$2"
    local ref="$3"
    local run_id=""
    local attempts=0

    step "waiting for the $workflow workflow to appear"
    while [ -z "$run_id" ] && [ "$attempts" -lt 60 ]; do
        run_id="$(gh run list \
            --repo "$repo" \
            --workflow "$workflow" \
            --commit "$sha" \
            --limit 20 \
            --json databaseId,headBranch \
            --jq ".[] | select(.headBranch == \"$ref\") | .databaseId" | sed -n '1p')"
        if [ -z "$run_id" ]; then
            attempts=$((attempts + 1))
            sleep 5
        fi
    done
    [ -n "$run_id" ] || die "$workflow did not appear for $ref at $sha"

    gh run watch "$run_id" --repo "$repo" --compact --exit-status
}

version="${1:-}"
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "usage: just release <major.minor.patch>"
readonly version
readonly tag="v$version"

[ "$(uname -s)" = "Darwin" ] || die "release verification is macOS-only"
for command in cargo git gh just ruby shasum unzip ditto plutil lipo; do
    require "$command"
done

root="$(git rev-parse --show-toplevel)"
cd "$root"
[ "$(git branch --show-current)" = "main" ] || die "releases must run from main"
[ -z "$(git status --porcelain)" ] || die "commit or stash every change before releasing"
gh auth status >/dev/null

step "checking main and release state"
git fetch origin main --tags
[ "$(git rev-parse HEAD)" = "$(git rev-parse origin/main)" ] || \
    die "local main must exactly match origin/main"

cargo_version="$(metadata_version Cargo.toml)"
package_version="$(metadata_version package.json)"
tauri_version="$(metadata_version src-tauri/tauri.conf.json)"
[ "$cargo_version" = "$package_version" ] && [ "$cargo_version" = "$tauri_version" ] || \
    die "Cargo.toml, package.json, and tauri.conf.json disagree on the current version"

if [ "$cargo_version" != "$version" ]; then
    if git rev-parse --verify --quiet "refs/tags/$tag" >/dev/null; then
        die "$tag already exists but the source metadata is still $cargo_version"
    fi

    step "updating source metadata to $version"
    ruby - "$version" <<'RUBY'
version = ARGV.fetch(0)

def replace_once(path, pattern, replacement)
  source = File.read(path)
  changed = source.sub(pattern, replacement)
  raise "expected one version field in #{path}" if changed == source
  File.write(path, changed)
end

replace_once(
  "Cargo.toml",
  /(\[workspace\.package\]\nversion\s*=\s*")[^"]+(\")/,
  "\\1#{version}\\2",
)
replace_once("package.json", /(^\s*"version":\s*")[^"]+(\",)/, "\\1#{version}\\2")
replace_once(
  "src-tauri/tauri.conf.json",
  /(^\s*"version":\s*")[^"]+(\",)/,
  "\\1#{version}\\2",
)
RUBY

    step "running the local release gates"
    just check
    just audit

    # Cargo updates the lockfile's workspace package versions during the compile gate above. Check
    # afterwards so the safety assertion covers Cargo's generated edit as well as the three source
    # fields, without hand-editing dependency-shaped records in Cargo.lock.
    expected_changes=$'Cargo.lock\nCargo.toml\npackage.json\nsrc-tauri/tauri.conf.json'
    actual_changes="$(git diff --name-only | LC_ALL=C sort)"
    [ "$actual_changes" = "$expected_changes" ] || {
        printf 'Expected version changes:\n%s\nActual changes:\n%s\n' \
            "$expected_changes" "$actual_changes" >&2
        die "versioning changed an unexpected set of files"
    }

    git add Cargo.toml Cargo.lock package.json src-tauri/tauri.conf.json
    git commit \
        -m "Release $tag" \
        -m "Set every application and workspace package version to $version." \
        -m "Verified with just check and just audit."
    git push origin main
else
    ok "source metadata is already $version; resuming the release"
fi

sha="$(git rev-parse HEAD)"
[ "$sha" = "$(git rev-parse origin/main)" ] || die "release commit is not on origin/main"
wait_for_run "CI" "$sha" "main"

if git rev-parse --verify --quiet "refs/tags/$tag" >/dev/null; then
    [ "$(git rev-list -n 1 "$tag")" = "$sha" ] || die "$tag points at a different commit"
    ok "$tag already points at the release commit"
else
    step "tagging the tested commit"
    [ -z "$(git ls-remote --tags origin "refs/tags/$tag")" ] || \
        die "$tag exists on origin but was not fetched locally"
    git tag -a "$tag" -m "Release $tag"
    git push origin "$tag"
fi

wait_for_run "Release" "$sha" "$tag"

release_dir="$(mktemp -d "${TMPDIR:-/tmp}/wtm-release.XXXXXX")"
trap 'rm -rf "$release_dir"' EXIT
dist="$release_dir/dist"
mkdir -p "$dist"

step "downloading and verifying release artifacts"
gh release download "$tag" --repo "$repo" --dir "$dist"

mac_zip="$dist/wtm-$version-macos-arm64.zip"
linux_image="$dist/wtm-$version-linux-x86_64.AppImage"
mac_checksums="$dist/checksums-macos-arm64.txt"
linux_checksums="$dist/checksums-linux-x86_64.txt"
for asset in "$mac_zip" "$linux_image" "$mac_checksums" "$linux_checksums"; do
    [ -s "$asset" ] || die "release asset missing or empty: $(basename "$asset")"
done
(
    cd "$dist"
    shasum -a 256 -c "$(basename "$mac_checksums")"
    shasum -a 256 -c "$(basename "$linux_checksums")"
)

unzip -Z1 "$mac_zip" | grep '^Worktree Manager\.app/' >/dev/null || \
    die "the macOS zip does not contain Worktree Manager.app at its root"
app_dir="$release_dir/app"
mkdir -p "$app_dir"
ditto -x -k "$mac_zip" "$app_dir"
plist="$app_dir/Worktree Manager.app/Contents/Info.plist"
binary="$app_dir/Worktree Manager.app/Contents/MacOS/wtm"
[ "$(plutil -extract CFBundleShortVersionString raw -o - "$plist")" = "$version" ] || \
    die "the macOS bundle reports the wrong version"
[ "$(plutil -extract LSMinimumSystemVersion raw -o - "$plist")" = "13.0" ] || \
    die "the macOS bundle no longer targets macOS 13"
lipo -archs "$binary" | tr ' ' '\n' | grep -x arm64 >/dev/null || \
    die "the macOS bundle does not contain an arm64 binary"
mac_sha="$(shasum -a 256 "$mac_zip" | awk '{print $1}')"

step "preparing the Homebrew tap"
tap_dir="$release_dir/homebrew-tap"
gh repo clone "$tap_repo" "$tap_dir" -- --depth 1
cask="$tap_dir/$tap_cask"
current_tap_version="$(sed -n 's/^[[:space:]]*version "\([^"]*\)"/\1/p' "$cask")"
current_tap_sha="$(sed -n 's/^[[:space:]]*sha256 "\([^"]*\)"/\1/p' "$cask")"

tap_commit=""
if [ "$current_tap_version" = "$version" ]; then
    [ "$current_tap_sha" = "$mac_sha" ] || \
        die "the tap already names $version with a different checksum"
    ok "the Homebrew tap already contains $version"
else
    ruby -e 'require "rubygems"; exit(Gem::Version.new(ARGV[0]) < Gem::Version.new(ARGV[1]) ? 0 : 1)' \
        "$current_tap_version" "$version" || \
        die "refusing to move the tap backward from $current_tap_version to $version"
    ruby - "$cask" "$version" "$mac_sha" <<'RUBY'
path, version, sha = ARGV
source = File.read(path)
versioned = source.sub(/^  version "[^"]+"$/, "  version \"#{version}\"")
raise "cask version was not found" if versioned == source
checksummed = versioned.sub(/^  sha256 "[^"]+"$/, "  sha256 \"#{sha}\"")
raise "cask checksum was not found" if checksummed == versioned
File.write(path, checksummed)
RUBY
    ruby -c "$cask"
    git -C "$tap_dir" diff --check
    git -C "$tap_dir" add "$tap_cask"
    git -C "$tap_dir" commit \
        -m "Update wtm to $version" \
        -m "Point the cask at the verified macOS arm64 artifact from $tag." \
        -m "SHA-256: $mac_sha"
    tap_commit="$(git -C "$tap_dir" rev-parse HEAD)"
fi

is_draft="$(gh release view "$tag" --repo "$repo" --json isDraft --jq .isDraft)"
if [ "$is_draft" = "true" ]; then
    step "publishing $tag"
    gh release edit "$tag" --repo "$repo" --draft=false --latest
else
    ok "$tag is already published"
fi

if [ -n "$tap_commit" ]; then
    step "publishing the Homebrew cask"
    git -C "$tap_dir" push origin HEAD:main
fi
tap_commit="$(git -C "$tap_dir" rev-parse HEAD)"

latest="$(gh api "repos/$repo/releases/latest" --jq .tag_name)"
[ "$latest" = "$tag" ] || die "GitHub does not report $tag as the latest release"
remote_cask="$(gh api "repos/$tap_repo/contents/$tap_cask" --jq .content | base64 --decode)"
printf '%s\n' "$remote_cask" | grep "version \"$version\"" >/dev/null || \
    die "the remote Homebrew tap does not report $version"
printf '%s\n' "$remote_cask" | grep "sha256 \"$mac_sha\"" >/dev/null || \
    die "the remote Homebrew tap does not report the verified checksum"

ok "$tag is published and the Homebrew tap is updated"
printf '  https://github.com/%s/releases/tag/%s\n' "$repo" "$tag"
printf '  https://github.com/%s/commit/%s\n' "$tap_repo" "$tap_commit"
