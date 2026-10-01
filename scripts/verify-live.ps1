param(
    [string]$Suno = "suno",
    [string]$SourceSong,
    [switch]$AllowCreditUse,
    [string]$EvidenceDir = (Join-Path $PSScriptRoot "..\target\verification")
)

$ErrorActionPreference = "Stop"
$EvidenceDir = [IO.Path]::GetFullPath($EvidenceDir)
New-Item -ItemType Directory -Path $EvidenceDir -Force | Out-Null

function Invoke-Suno([string]$Name, [string[]]$Arguments) {
    $outPath = Join-Path $EvidenceDir "$Name.json"
    $errPath = Join-Path $EvidenceDir "$Name.stderr.json"
    & $Suno --json --quiet --no-browser @Arguments 1> $outPath 2> $errPath
    if ($LASTEXITCODE -ne 0) {
        throw "Suno failed during $Name. Inspect $errPath and suno jobs. The saved request IDs must be retained when resuming."
    }
    return (Get-Content -LiteralPath $outPath -Raw | ConvertFrom-Json).data
}

$null = Invoke-Suno "auth" @("auth")
$credits = Invoke-Suno "credits-before" @("credits")
$models = @(Invoke-Suno "models" @("models"))
if (-not $AllowCreditUse) {
    Write-Output "Authentication and catalogue checked. Pass -AllowCreditUse to render a pair and a cover, or a cover of -SourceSong."
    return
}

$manifestPath = Join-Path $EvidenceDir "run.json"
if (Test-Path -LiteralPath $manifestPath) {
    $run = Get-Content -LiteralPath $manifestPath -Raw | ConvertFrom-Json
    if ($SourceSong -and $run.source_argument -ne $SourceSong) {
        throw "This evidence directory belongs to another source. Use a new EvidenceDir for a different test."
    }
} else {
    $preferred = $models | Where-Object { $_.can_use -and $_.external_key -eq "chirp-hawk" } | Select-Object -First 1
    if (-not $preferred) {
        $preferred = $models | Where-Object { $_.can_use -and $_.external_key -eq "chirp-goose" } | Select-Object -First 1
    }
    if (-not $preferred) { throw "No accessible v6 model found." }
    $run = [pscustomobject]@{
        started_at = [DateTime]::UtcNow.ToString("o")
        model = $(if ($preferred.external_key -eq "chirp-hawk") { "v6" } else { "v6-mini" })
        generation_request_id = [Guid]::NewGuid().ToString()
        cover_request_id = [Guid]::NewGuid().ToString()
        source_argument = $SourceSong
        source_id = ""
        complete = $false
    }
    $run | ConvertTo-Json | Set-Content -LiteralPath $manifestPath -Encoding utf8
}

if (-not $run.source_id) {
    if ($run.source_argument) {
        $source = [string]$run.source_argument
        if ($source.StartsWith("https://")) {
            $uri = [Uri]$source
            if ($uri.Host -notin @("suno.com", "www.suno.com") -or $uri.AbsolutePath -notmatch '^/song/([0-9a-fA-F-]{36})/?$') {
                throw "SourceSong must be a Suno song URL or UUID."
            }
            $source = $Matches[1]
        }
        $run.source_id = [Guid]::Parse($source).ToString()
    } else {
        $lyrics = "[Verse]`nA little melody begins`nA steady light, a song of simple things`n[Chorus]`nCarry the tune through the morning air`nEvery note a little care"
        $generated = @(Invoke-Suno "generation" @("generate", "--request-id", $run.generation_request_id,
            "--model", $run.model, "--title", "CLI verification original", "--tags", "acoustic pop, clear vocals, gentle guitar",
            "--lyrics", $lyrics, "--duration", "30", "--wait", "--download", (Join-Path $EvidenceDir "original")))
        if (-not $generated -or ($generated | Where-Object { $_.status -ne "complete" })) {
            throw "The original generation did not complete. Resume the saved request ID."
        }
        $run.source_id = $generated[0].id
    }
    $run | ConvertTo-Json | Set-Content -LiteralPath $manifestPath -Encoding utf8
}

$sourceInfo = Invoke-Suno "source" @("info", $run.source_id)
if ($sourceInfo.status -ne "complete") { throw "The source song must be complete." }
$covered = @(Invoke-Suno "cover" @("cover", $run.source_id, "--request-id", $run.cover_request_id,
    "--model", $run.model, "--title", "CLI verification jazz cover", "--tags", "jazz trio, soft piano, upright bass, brushed drums",
    "--audio-influence", "70", "--wait", "--download", (Join-Path $EvidenceDir "cover")))
if (-not $covered -or ($covered | Where-Object { $_.status -ne "complete" })) {
    throw "The cover did not complete. Resume the saved request ID."
}

$files = @()
foreach ($clip in $covered) {
    if ($clip.id -eq $run.source_id) { throw "Cover returned the original clip instead of a new clip." }
    if (-not $clip.local_path -or -not (Test-Path -LiteralPath $clip.local_path)) {
        throw "Completed cover has no downloaded file."
    }
    $file = Get-Item -LiteralPath $clip.local_path
    if ($file.Length -lt 1024) { throw "Downloaded cover is unexpectedly small." }
    $files += [pscustomobject]@{ clip_id = $clip.id; path = $file.FullName; bytes = $file.Length;
        sha256 = (Get-FileHash -LiteralPath $file.FullName -Algorithm SHA256).Hash }
}
$files | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $EvidenceDir "download-evidence.json") -Encoding utf8

$decoder = Get-Command ffmpeg -ErrorAction SilentlyContinue
if ($decoder) {
    foreach ($file in $files) {
        & $decoder.Source -v error -i $file.path -f null - 2> (Join-Path $EvidenceDir "$($file.clip_id).decode.txt")
        if ($LASTEXITCODE -ne 0) { throw "Audio decoding failed for $($file.clip_id)." }
    }
}
$after = Invoke-Suno "credits-after" @("credits")
$run.complete = $true
$run | ConvertTo-Json | Set-Content -LiteralPath $manifestPath -Encoding utf8
Write-Output "Creation/cover submission, completion, and file download verified. Evidence: $EvidenceDir"
Write-Output "Observed credit balance change: $($credits.total_credits_left - $after.total_credits_left). Concurrent account use can affect this number."
if (-not $decoder) { Write-Output "FFmpeg was unavailable; file checks passed, audio decoding remains unverified." }
Write-Output "Listen to the cover to assess melody, lyrics, and style; technical checks do not measure musical quality."
