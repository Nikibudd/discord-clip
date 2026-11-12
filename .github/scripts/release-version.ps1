# Works out the release version requested in a pull request description.
#
# The description needs a line like one of these (case and markdown bold are
# ignored):
#   Release: patch     -> bump the patch number of the latest release
#   Release: minor     -> bump minor, reset patch
#   Release: major     -> bump major, reset minor and patch
#   Release: 1.4.0     -> exactly this version (also 1.4.0-beta.1 for a pre-release)
#   Release: none      -> don't publish (same as no Release line)
#
# "Latest release" is the highest vX.Y.Z tag (pre-releases don't count), or
# 0.0.0 if there is none. Writes `version` (empty = no release) to
# $GITHUB_OUTPUT and a short explanation to the job summary.

param([string]$Body = '')
$ErrorActionPreference = 'Stop'

function Set-Output($name, $value) {
    if ($env:GITHUB_OUTPUT) { "$name=$value" >> $env:GITHUB_OUTPUT }
}
function Write-Summary($text) {
    if ($env:GITHUB_STEP_SUMMARY) { $text >> $env:GITHUB_STEP_SUMMARY }
    Write-Host $text
}
function Fail($text) {
    Write-Summary ":x: $text"
    Write-Host "::error::$text"
    exit 1
}

$clean = $Body -replace '[*_`]', ''
$match = [regex]::Match($clean, '(?im)^\s*release\s*:\s*(\S+)\s*$')
$request = if ($match.Success) { $match.Groups[1].Value.ToLower() } else { '' }

$latest = git tag --list 'v*' |
    ForEach-Object { $_.Substring(1) } |
    Where-Object { $_ -match '^\d+\.\d+\.\d+$' } |
    Sort-Object { [version]$_ } |
    Select-Object -Last 1
if (-not $latest) { $latest = '0.0.0' }
$prev = [version]$latest

if ($request -in @('', 'none', 'no', '-')) {
    Set-Output 'version' ''
    Write-Summary ":information_source: No release: add a ``Release: patch|minor|major|X.Y.Z`` line to the PR description to publish one. Latest release: $latest."
    exit 0
}

switch ($request) {
    'major' { $version = "$($prev.Major + 1).0.0" }
    'minor' { $version = "$($prev.Major).$($prev.Minor + 1).0" }
    'patch' { $version = "$($prev.Major).$($prev.Minor).$($prev.Build + 1)" }
    default {
        $version = $request.TrimStart('v')
        if ($version -notmatch '^\d+\.\d+\.\d+(-[0-9a-z]+(\.[0-9a-z]+)*)?$') {
            Fail "``Release: $request`` isn't patch, minor, major, none or a version like 1.2.3 / 1.2.3-beta.1."
        }
    }
}

$core = [version]($version.Split('-')[0])
if ($latest -ne '0.0.0' -and $core -le $prev) {
    Fail "Release $version must be newer than the latest release $latest."
}
git rev-parse -q --verify "refs/tags/v$version" *> $null
if ($LASTEXITCODE -eq 0) {
    Fail "Tag v$version already exists."
}

Set-Output 'version' $version
Write-Summary ":rocket: Merging this PR publishes **v$version** (``Release: $request``, latest release: $latest)."
exit 0
