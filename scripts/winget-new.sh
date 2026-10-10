#!/usr/bin/env bash
# Submits the first winget version of one of the owner's repos, from the
# windows-msvc zips on its latest release. Later versions are submitted by the
# shared release workflow, which can only update existing packages.
#
#   scripts/winget-new.sh REPO            # write the manifests and validate them
#   scripts/winget-new.sh REPO --submit   # ...and open the PR to winget-pkgs
#
# Needs gh, jq, curl, komac and the token in ~/.config/forge/winget-pat.
# `komac new` would ask which file in the zip to install even with every flag
# given, so the manifests are written here and only submitted with komac.
set -euo pipefail

repo=${1:?usage: winget-new.sh REPO [--submit]}
mode=--dry-run
[[ ${2:-} == --submit ]] && mode=--yes

owner=$(sed -n 's/^owner = "\(.*\)"/\1/p' "$(dirname "$0")/../forge.toml")
full=$owner/$repo
id=$owner.$repo
token=$(cat ~/.config/forge/winget-pat)

release=$(gh api "repos/$full/releases/latest")
tag=$(jq -r .tag_name <<<"$release")
version=${tag#v}
date=$(jq -r '.published_at[:10]' <<<"$release")
info=$(gh api "repos/$full")
cargo_toml=$(gh api "repos/$full/contents/Cargo.toml" -H "Accept: application/vnd.github.raw" 2>/dev/null || true)
description=$(sed -n 's/^description = "\(.*\)"/\1/p' <<<"$cargo_toml" | head -n1)
[[ -n $description ]] || description=$(jq -r '.description // empty' <<<"$info")
[[ -n $description ]] || { echo "error: $full has no description in Cargo.toml or on GitHub" >&2; exit 1; }
license=$(sed -n 's/^license = "\(.*\)"/\1/p' <<<"$cargo_toml" | head -n1)
[[ -n $license ]] || license=$(jq -r '.license.spdx_id // empty' <<<"$info")
[[ -n $license ]] || { echo "error: can't tell $full's license" >&2; exit 1; }

dir=$(mktemp -d)
out=$dir/manifests/${owner:0:1}/$owner/$repo/$version
mkdir -p "$out"
header="# Created with forge's winget-new.sh"

installers=
for pair in x64:x86_64-pc-windows-msvc arm64:aarch64-pc-windows-msvc; do
  arch=${pair%%:*} target=${pair#*:}
  name="$repo-$tag-$target.zip"
  url=$(jq -r --arg n "$name" '.assets[] | select(.name == $n) | .browser_download_url' <<<"$release")
  if [[ -z $url ]]; then
    echo "warning: $tag has no $name; $arch left out" >&2
    continue
  fi
  sha=$(curl -fsSL "$url" | sha256sum | cut -d' ' -f1 | tr a-f A-F)
  installers+="- Architecture: $arch
  InstallerUrl: $url
  InstallerSha256: $sha
"
done
[[ -n $installers ]] || { echo "error: $tag has no windows-msvc zips" >&2; exit 1; }

topics=$(jq -r '.topics[:8][] | "- \(.)"' <<<"$info")

cat >"$out/$id.yaml" <<EOF
$header
# yaml-language-server: \$schema=https://aka.ms/winget-manifest.version.1.12.0.schema.json

PackageIdentifier: $id
PackageVersion: $version
DefaultLocale: en-US
ManifestType: version
ManifestVersion: 1.12.0
EOF

cat >"$out/$id.installer.yaml" <<EOF
$header
# yaml-language-server: \$schema=https://aka.ms/winget-manifest.installer.1.12.0.schema.json

PackageIdentifier: $id
PackageVersion: $version
InstallerType: zip
NestedInstallerType: portable
NestedInstallerFiles:
- RelativeFilePath: $repo.exe
ReleaseDate: $date
Installers:
${installers}ManifestType: installer
ManifestVersion: 1.12.0
EOF

cat >"$out/$id.locale.en-US.yaml" <<EOF
$header
# yaml-language-server: \$schema=https://aka.ms/winget-manifest.defaultLocale.1.12.0.schema.json

PackageIdentifier: $id
PackageVersion: $version
PackageLocale: en-US
Publisher: $owner
PublisherUrl: https://github.com/$owner
PublisherSupportUrl: https://github.com/$full/issues
PackageName: $repo
PackageUrl: https://github.com/$full
License: $license
LicenseUrl: https://github.com/$full#license
ShortDescription: $(jq -Rr @json <<<"$description")
${topics:+Tags:
$topics
}ReleaseNotesUrl: https://github.com/$full/releases/tag/$tag
ManifestType: defaultLocale
ManifestVersion: 1.12.0
EOF

for f in "$out"/*.yaml; do echo "=== $f"; cat "$f"; done
# komac refuses to run without a terminal, even when it asks nothing.
GITHUB_TOKEN=$token script -qec "komac submit $mode '$out'" /dev/null </dev/null \
  | sed 's/\x1b\[[0-9;?]*[a-zA-Z]//g; s/\x1b\]8;;[^\x1b]*\x1b\\//g' \
  | grep -E 'error|Error|warning|Successfully|pull' || true
[[ $mode == --yes ]] || echo "dry run only; rerun with --submit to open the PR"
