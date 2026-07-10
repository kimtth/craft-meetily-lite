$ErrorActionPreference = 'Stop'

$repoRoot = Split-Path -Parent $PSScriptRoot
$resourcesDir = Join-Path $repoRoot 'src-tauri\resources'
$ffmpegPath = Join-Path $resourcesDir 'ffmpeg.exe'
$expectedSha256 = '1326DDE4C84FF1F96FE6B8916C5BED29E163E9B5DCCF995F6F3DB069D143EC5E'
$packageUrl = 'https://www.gyan.dev/ffmpeg/builds/packages/ffmpeg-8.1.2-essentials_build.zip'

function Get-Sha256([string]$path) {
    $stream = [System.IO.File]::OpenRead($path)
    $sha256 = [System.Security.Cryptography.SHA256]::Create()
    try {
        return [System.BitConverter]::ToString($sha256.ComputeHash($stream)).Replace('-', '')
    }
    finally {
        $sha256.Dispose()
        $stream.Dispose()
    }
}

if (Test-Path $ffmpegPath) {
    $installedHash = Get-Sha256 $ffmpegPath
    if ($installedHash -eq $expectedSha256) {
        Write-Host 'Bundled FFmpeg 8.1.2 is ready.'
        exit 0
    }
    Write-Warning 'The existing FFmpeg executable has an unexpected checksum and will be replaced.'
}

New-Item -ItemType Directory -Path $resourcesDir -Force | Out-Null
$tempRoot = Join-Path ([System.IO.Path]::GetTempPath()) "MeetlyLite-FFmpeg-$([guid]::NewGuid())"
$archivePath = Join-Path $tempRoot 'ffmpeg.zip'
$extractPath = Join-Path $tempRoot 'extracted'

try {
    New-Item -ItemType Directory -Path $tempRoot -Force | Out-Null
    Write-Host 'Downloading the pinned FFmpeg 8.1.2 essentials build...'
    Invoke-WebRequest -Uri $packageUrl -OutFile $archivePath -UseBasicParsing
    Expand-Archive -Path $archivePath -DestinationPath $extractPath -Force

    $downloadedFfmpeg = Get-ChildItem $extractPath -Filter 'ffmpeg.exe' -File -Recurse |
        Select-Object -First 1
    if (-not $downloadedFfmpeg) {
        throw 'The FFmpeg package did not contain ffmpeg.exe.'
    }

    $downloadedHash = Get-Sha256 $downloadedFfmpeg.FullName
    if ($downloadedHash -ne $expectedSha256) {
        throw "FFmpeg checksum verification failed. Expected $expectedSha256 but received $downloadedHash."
    }

    Copy-Item $downloadedFfmpeg.FullName $ffmpegPath -Force
    Write-Host "Installed verified FFmpeg at $ffmpegPath"
}
finally {
    if (Test-Path $tempRoot) {
        Remove-Item $tempRoot -Recurse -Force
    }
}
