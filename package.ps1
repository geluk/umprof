$ErrorActionPreference = "Stop"

$root = $PSScriptRoot
$dist = Join-Path $root "dist"

$cargoToml = Get-Content (Join-Path $root "umprof\Cargo.toml") -Raw
if ($cargoToml -notmatch '(?m)^version\s*=\s*"([^"]+)"') {
    throw "Could not find the version in umprof\Cargo.toml."
}
$version = $Matches[1]

Write-Host "Building umprof $version..."
cargo build --release --manifest-path (Join-Path $root "umprof\Cargo.toml")
if ($LASTEXITCODE -ne 0) {
    throw "Building umprof failed."
}

$staging = Join-Path $dist "umprof-$version"
if (Test-Path $staging) {
    Remove-Item -Recurse -Force $staging
}
New-Item -ItemType Directory $staging | Out-Null
New-Item -ItemType Directory (Join-Path $staging "profiles") | Out-Null

Copy-Item (Join-Path $root "umprof\target\release\umprof.exe") $staging
Copy-Item (Join-Path $root "record.ps1") $staging
Copy-Item (Join-Path $root "USAGE.md") $staging
Copy-Item (Join-Path $root "profiles\*.wprp") (Join-Path $staging "profiles")

$zip = Join-Path $dist "umprof-$version.zip"
if (Test-Path $zip) {
    Remove-Item -Force $zip
}
Compress-Archive -Path (Join-Path $staging "*") -DestinationPath $zip

Remove-Item -Recurse -Force $staging

Write-Host "Created $zip"
