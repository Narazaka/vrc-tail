$ErrorActionPreference = "Stop"
$projectRoot = Split-Path $PSScriptRoot -Parent
$executable = Join-Path $projectRoot "vrc-tail.exe"
if (-not (Test-Path -LiteralPath $executable -PathType Leaf)) {
  throw "Build output not found: $executable"
}

$metadata = cargo metadata --manifest-path (Join-Path $projectRoot "Cargo.toml") --format-version 1 --no-deps | ConvertFrom-Json
if ($LASTEXITCODE -ne 0) { throw "cargo metadata failed" }
$version = ($metadata.packages | Where-Object name -EQ "vrc-tail" | Select-Object -First 1).version
$hash = (Get-FileHash -LiteralPath $executable -Algorithm SHA256).Hash
$source = Join-Path $projectRoot "manifest/winget"
$destination = Join-Path $source "generated"
New-Item -ItemType Directory -Force $destination | Out-Null

@(
  "Narazaka.vrc-tail.yaml",
  "Narazaka.vrc-tail.installer.yaml",
  "Narazaka.vrc-tail.locale.en-US.yaml"
) | ForEach-Object {
  $content = Get-Content -Raw -LiteralPath (Join-Path $source $_)
  $content = $content.Replace("PACKAGE_VERSION", $version).Replace("PACKAGE_HASH", $hash)
  Set-Content -NoNewline -Encoding utf8 -LiteralPath (Join-Path $destination $_) -Value $content
}
