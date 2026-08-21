<#
.SYNOPSIS
    Installs ost-mcp: the binary on PATH and the Claude Code skill beside it.

.DESCRIPTION
    Downloads the prebuilt `ost-mcp` binary from a GitHub release, checks it
    against its published SHA-256, then drops `skills/ost-mcp/SKILL.md` into the
    Claude skills directory so a model can query the mailbox without registering
    an MCP server.

    The prebuilt Windows binary links the CRT statically, so it needs no Rust
    toolchain, no MSVC install and no Visual C++ redistributable.

    When no release asset matches this machine, or when -FromSource or a -Ref
    other than `main` is given, the script builds with cargo instead. That path
    does need Rust and the MSVC build tools, and it compiles the DuckDB
    amalgamation, which takes a few minutes.

    Nothing here touches a mailbox. The reader maps a store read-only and has no
    code path that writes to one.

.PARAMETER ReleaseTag
    Which release to download from. Defaults to `latest`.

.PARAMETER FromSource
    Build with cargo instead of downloading a release binary.

.PARAMETER Ref
    Branch to build from when building from source. Defaults to `main`, and any
    other value implies -FromSource. Also selects which branch the skill comes
    from.

.PARAMETER SkillScope
    `user` installs the skill for every session (~/.claude/skills). `project`
    installs it into one repository (.claude/skills), which is what you want when
    the skill should travel with a checkout.

.PARAMETER ProjectPath
    Where a `project` scoped skill goes. Defaults to the current directory.

.PARAMETER InstallPrereqs
    Install a missing Rust toolchain or MSVC build tools with winget. Only the
    source build needs either. Off by default: installing a compiler is not
    something a one-line command should do without being asked.

.PARAMETER Force
    Reinstall the binary even when the same version is already present, and
    continue past the MSVC preflight check.

.PARAMETER SkipSkill
    Install the binary only.

.EXAMPLE
    irm https://raw.githubusercontent.com/3rg0n/ost-mcp/main/install.ps1 | iex

.EXAMPLE
    & ([scriptblock]::Create((irm https://raw.githubusercontent.com/3rg0n/ost-mcp/main/install.ps1))) -FromSource -InstallPrereqs
#>
[CmdletBinding()]
param(
    [string]$ReleaseTag = 'latest',
    [switch]$FromSource,
    [string]$Ref = 'main',
    [ValidateSet('user', 'project')]
    [string]$SkillScope = 'user',
    [string]$ProjectPath = '.',
    [switch]$InstallPrereqs,
    [switch]$Force,
    [switch]$SkipSkill
)

$ErrorActionPreference = 'Stop'

$RepoUrl = 'https://github.com/3rg0n/ost-mcp'
$RawBase = "https://raw.githubusercontent.com/3rg0n/ost-mcp/$Ref"

# ------------------------------------------------------------------ output

function Write-Step { param([string]$Text) Write-Host "==> $Text" -ForegroundColor Cyan }
function Write-Ok { param([string]$Text) Write-Host "    ok  $Text" -ForegroundColor Green }
function Write-Note { param([string]$Text) Write-Host "    --  $Text" -ForegroundColor DarkGray }
function Write-Warn { param([string]$Text) Write-Host "    !!  $Text" -ForegroundColor Yellow }

function Fail {
    param([string]$Text, [string[]]$Remedy)
    Write-Host ""
    Write-Host "ost-mcp install failed: $Text" -ForegroundColor Red
    if ($Remedy) {
        Write-Host ""
        Write-Host "To fix it:" -ForegroundColor Red
        foreach ($line in $Remedy) { Write-Host "  $line" }
    }
    Write-Host ""
    throw $Text
}

function Import-VcVars {
    <#
        Runs vcvars64.bat and copies the variables it sets into this process.

        rustc only picks a linker for itself when the environment is not already a
        developer environment. On a machine with more than one Visual Studio it can
        pick a `link.exe` whose CRT libraries are not on `LIB`, and the build then
        dies on `LNK1104: cannot open file 'msvcrt.lib'`. Activating one toolchain
        up front makes the compiler, the linker and the libraries agree.
    #>
    param([string]$Bat)

    $wanted = @(
        'PATH', 'INCLUDE', 'LIB', 'LIBPATH',
        'VSINSTALLDIR', 'VCINSTALLDIR', 'VCToolsInstallDir', 'VCToolsVersion',
        'WindowsSdkDir', 'WindowsSdkBinPath', 'WindowsSdkVerBinPath',
        'WindowsSDKLibVersion', 'WindowsSDKVersion',
        'UniversalCRTSdkDir', 'UCRTVersion'
    )

    $lines = & cmd.exe /c "call `"$Bat`" >nul 2>&1 && set"
    foreach ($line in $lines) {
        $i = $line.IndexOf('=')
        if ($i -lt 1) { continue }
        $name = $line.Substring(0, $i)
        if ($wanted -notcontains $name) { continue }
        Set-Item -Path "Env:$name" -Value $line.Substring($i + 1)
    }
}

function Add-ToUserPath {
    <#
        Appends a directory to the user's PATH, in the registry and in this
        process. Returns $true when it changed something.

        Not [Environment]::SetEnvironmentVariable: reading PATH through that API
        expands %USERPROFILE% and anything else the value references, and writing
        the expanded string back would freeze another program's variable
        references into literal paths. The registry API can read the raw value
        and preserve the value kind.
    #>
    param([string]$Directory)

    $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Environment', $true)
    try {
        $raw = [string]$key.GetValue(
            'PATH', '', [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
        $kind = [Microsoft.Win32.RegistryValueKind]::ExpandString
        if ($raw) { $kind = $key.GetValueKind('PATH') }

        $parts = @($raw -split ';' | Where-Object { $_ -ne '' })
        if ($parts -contains $Directory) { return $false }

        $value = $Directory
        if ($raw) { $value = "$raw;$Directory" }
        $key.SetValue('PATH', $value, $kind)
    } finally {
        if ($key) { $key.Close() }
    }

    $env:PATH = "$env:PATH;$Directory"
    return $true
}

# ------------------------------------------------------------- preflight

Write-Step "Checking this machine"

if ($env:OS -ne 'Windows_NT') {
    Fail "this installer is for Windows." @(
        "On macOS, use install.sh instead:",
        "curl -fsSL $RawBase/install.sh | bash"
    )
}
Write-Ok "Windows, PowerShell $($PSVersionTable.PSVersion)"

# GitHub refuses TLS below 1.2, and PowerShell 5.1 still defaults lower.
try {
    $tls = [Net.SecurityProtocolType]::Tls12
    try { $tls = $tls -bor [Net.SecurityProtocolType]::Tls13 } catch { }
    [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor $tls
} catch {
    Write-Note "could not raise the TLS version; a download may fail"
}

# Where the binary goes. cargo's bin directory when it exists, so a machine that
# already has a source install gets an upgrade in place rather than a second
# copy on PATH; a dedicated directory otherwise.
$cargoHome = $env:CARGO_HOME
if (-not $cargoHome) { $cargoHome = Join-Path $env:USERPROFILE '.cargo' }
$cargoBin = Join-Path $cargoHome 'bin'

$installDir = $cargoBin
if (-not (Test-Path $cargoBin)) {
    $installDir = Join-Path $env:LOCALAPPDATA 'Programs\ost-mcp'
}
$exe = Join-Path $installDir 'ost-mcp.exe'

# Only the x64 asset is published. Windows on ARM can run it under emulation,
# but a native build is worth the compile, so ARM64 goes to source.
$arch = $env:PROCESSOR_ARCHITECTURE
if (-not $arch) { $arch = 'AMD64' }
$target = 'x86_64-pc-windows-msvc'

# A branch other than main is a request for that branch's code, which a
# published release asset is not.
$useSource = [bool]$FromSource
if ($Ref -ne 'main' -and -not $FromSource) {
    Write-Note "-Ref $Ref given, so the binary is built from that branch rather than downloaded"
    $useSource = $true
}
if ($arch -ne 'AMD64' -and -not $useSource) {
    Write-Note "this is a $arch machine and only an x64 binary is published; building from source"
    $useSource = $true
}

# ------------------------------------------------------- binary: download

function Install-FromRelease {
    <#
        Downloads, checks and unpacks the release asset for this platform.
        Returns $true on success, $false when the release or the asset is not
        there — the caller then builds from source.

        A failed download is a fallback. A checksum mismatch is not: it means
        the bytes that arrived are not the bytes that were published, and
        running them anyway would defeat the point of publishing the hash.
    #>
    param([string]$Destination)

    $asset = "ost-mcp-$target.zip"
    if ($ReleaseTag -eq 'latest') {
        $base = "$RepoUrl/releases/latest/download"
    } else {
        $base = "$RepoUrl/releases/download/$ReleaseTag"
    }

    $tmp = Join-Path $env:TEMP "ost-mcp-install-$PID"
    New-Item -ItemType Directory -Force -Path $tmp | Out-Null
    $zip = Join-Path $tmp $asset

    # Invoke-WebRequest's progress rendering costs more than the transfer on
    # PowerShell 5.1.
    $prevProgress = $ProgressPreference
    $ProgressPreference = 'SilentlyContinue'
    try {
        try {
            Invoke-WebRequest -Uri "$base/$asset" -OutFile $zip -UseBasicParsing
            Invoke-WebRequest -Uri "$base/$asset.sha256" -OutFile "$zip.sha256" -UseBasicParsing
        } catch {
            Write-Warn "no release binary for this platform ($($_.Exception.Message))"
            return $false
        }

        $expected = (Get-Content "$zip.sha256" -Raw).Trim().ToLower()
        $actual = (Get-FileHash -Algorithm SHA256 $zip).Hash.ToLower()
        if ($expected -ne $actual) {
            Fail "the downloaded binary does not match its published SHA-256." @(
                "expected $expected",
                "got      $actual",
                "",
                "Do not run it. Something between you and GitHub changed the bytes.",
                "Build from source instead: re-run with -FromSource"
            )
        }
        Write-Ok "SHA-256 matches the published hash"

        Expand-Archive -Path $zip -DestinationPath $tmp -Force
        $unpacked = Join-Path $tmp 'ost-mcp.exe'
        if (-not (Test-Path $unpacked)) {
            Fail "$asset does not contain ost-mcp.exe." @(
                "The release asset is malformed. Build from source: re-run with -FromSource"
            )
        }

        New-Item -ItemType Directory -Force -Path (Split-Path $Destination) | Out-Null
        try {
            Copy-Item $unpacked $Destination -Force
        } catch {
            Fail "could not write $Destination -- $($_.Exception.Message)" @(
                "A running ost-mcp holds the file open. Close any Claude Code session",
                "with the MCP server attached, then re-run this installer."
            )
        }

        # A checksum proves the bytes are the published ones, not that they run
        # here. Anything that stops a downloaded binary from starting — a
        # missing system library, a policy that blocks it — makes a source
        # build the better answer, so treat it as a failed download.
        $ran = ''
        try { $ran = (& $Destination --version 2>$null | Select-Object -First 1) } catch { }
        if (-not $ran) {
            Write-Warn "the downloaded binary does not run on this machine"
            return $false
        }
        return $true
    } finally {
        $ProgressPreference = $prevProgress
        Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
    }
}

if (-not $useSource) {
    Write-Step "Downloading the ost-mcp binary ($ReleaseTag release, $target)"
    if (Install-FromRelease $exe) {
        Write-Ok "installed $exe"
    } else {
        Write-Note "falling back to a source build"
        $useSource = $true
    }
}

# --------------------------------------------------------- binary: source

if ($useSource) {
    # Rust. Respect CARGO_HOME, because it is not always under the profile.
    $cargo = (Get-Command cargo -ErrorAction SilentlyContinue).Source
    if (-not $cargo) {
        $candidate = Join-Path $cargoBin 'cargo.exe'
        if (Test-Path $candidate) {
            $cargo = $candidate
            $env:PATH = "$cargoBin;$env:PATH"
        }
    }

    if (-not $cargo -and $InstallPrereqs) {
        Write-Step "Installing the Rust toolchain with winget"
        winget install --id Rustlang.Rustup --accept-source-agreements --accept-package-agreements --disable-interactivity
        $env:PATH = "$cargoBin;$env:PATH"
        $cargo = (Get-Command cargo -ErrorAction SilentlyContinue).Source
    }

    if (-not $cargo) {
        Fail "no Rust toolchain found (cargo is not on PATH), and this run needs a source build." @(
            "winget install --id Rustlang.Rustup",
            "or download rustup-init.exe from https://rustup.rs",
            "then re-run this installer in a new shell.",
            "",
            "Or re-run with -InstallPrereqs to let this script do it."
        )
    }
    Write-Ok "cargo at $cargo"

    # MSVC. Building DuckDB needs a C++ toolchain and a linker.
    $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
    $vcInstall = $null
    if (Test-Path $vswhere) {
        $vcInstall = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath 2>$null
        if ($vcInstall) { Write-Ok "MSVC C++ tools at $vcInstall" }
    }
    if (-not $vcInstall) {
        if ($InstallPrereqs) {
            Write-Step "Installing MSVC build tools with winget (this is a large download)"
            winget install --id Microsoft.VisualStudio.2022.BuildTools --override "--quiet --wait --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended" --accept-source-agreements --accept-package-agreements --disable-interactivity
            if (Test-Path $vswhere) {
                $vcInstall = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath 2>$null
            }
        } elseif (-not $Force) {
            Fail "no MSVC C++ build tools found, and DuckDB will not compile without them." @(
                "winget install --id Microsoft.VisualStudio.2022.BuildTools --override `"--quiet --wait --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended`"",
                "",
                "Or re-run with -InstallPrereqs to let this script do it,",
                "or with -Force to try the build anyway."
            )
        } else {
            Write-Warn "no MSVC detected; continuing because -Force was passed"
        }
    }

    if ($vcInstall) {
        $vcvars = Join-Path $vcInstall 'VC\Auxiliary\Build\vcvars64.bat'
        if (Test-Path $vcvars) {
            Import-VcVars $vcvars
            Write-Ok "activated the x64 toolchain from $vcInstall"
        } else {
            Write-Warn "no vcvars64.bat under $vcInstall; letting rustc pick the linker"
        }
    }

    # cargo's own git client cannot read a Windows credential helper, so a private or
    # enterprise fork fails to authenticate. The git CLI can, and is already installed
    # for anything else here to work.
    if (-not $env:CARGO_NET_GIT_FETCH_WITH_CLI -and (Get-Command git -ErrorAction SilentlyContinue)) {
        $env:CARGO_NET_GIT_FETCH_WITH_CLI = 'true'
    }

    # A static CRT, matching the published binary, so a source install has no
    # Visual C++ redistributable dependency either.
    if (-not $env:RUSTFLAGS) { $env:RUSTFLAGS = '-C target-feature=+crt-static' }

    Write-Step "Building the ost-mcp binary (the first build compiles DuckDB; allow a few minutes)"

    $installArgs = @('install', '--git', $RepoUrl, '--branch', $Ref, '--locked', 'ost-mcp')
    if ($Force) { $installArgs += '--force' }

    # Stream cargo's output. An exit code on its own tells the user nothing, and the
    # compiler error is the whole diagnosis.
    $prevEap = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    & $cargo @installArgs 2>&1 | ForEach-Object { Write-Host "    $_" }
    $cargoExit = $LASTEXITCODE
    $ErrorActionPreference = $prevEap

    if ($cargoExit -ne 0) {
        Fail "cargo install exited $cargoExit (the error is above)." @(
            "LNK1104 on a library such as msvcrt.lib means the linker and the CRT came",
            "from different Visual Studio installs. Open a Developer PowerShell for the",
            "toolchain you want and re-run this installer there.",
            "",
            "A git authentication failure means the repository is not readable by this",
            "machine. Authenticate to GitHub first, for example with: gh auth login"
        )
    }

    # cargo installs where cargo wants to, whatever this script picked earlier.
    $exe = Join-Path $cargoBin 'ost-mcp.exe'
    if (-not (Test-Path $exe)) {
        Fail "cargo reported success but $exe is missing." @(
            "Check where cargo installs binaries: cargo install --list"
        )
    }
    $installDir = $cargoBin
    Write-Ok "installed $exe"
}

$version = ''
try { $version = (& $exe --version 2>$null | Select-Object -First 1) } catch { }
if ($version) { Write-Ok $version }

# ------------------------------------------------------------------ PATH

if ($env:PATH -notlike "*$installDir*") {
    $added = $false
    try {
        $added = Add-ToUserPath $installDir
    } catch {
        Write-Warn "could not add $installDir to your PATH -- $($_.Exception.Message)"
    }
    if ($added) {
        Write-Ok "added $installDir to your PATH (open a new shell to run ost-mcp by name)"
    } else {
        Write-Note "$installDir is already on your PATH in the registry"
        $env:PATH = "$env:PATH;$installDir"
    }
}

# ----------------------------------------------------------------- skill

if (-not $SkipSkill) {
    Write-Step "Installing the Claude Code skill"

    if ($SkillScope -eq 'user') {
        $skillDir = Join-Path $env:USERPROFILE '.claude\skills\ost-mcp'
    } else {
        $skillDir = Join-Path (Resolve-Path $ProjectPath).Path '.claude\skills\ost-mcp'
    }

    New-Item -ItemType Directory -Force -Path $skillDir | Out-Null
    $skillFile = Join-Path $skillDir 'SKILL.md'
    $skillUrl = "$RawBase/skills/ost-mcp/SKILL.md"

    try {
        Invoke-WebRequest -Uri $skillUrl -OutFile $skillFile -UseBasicParsing
    } catch {
        Fail "could not download the skill from $skillUrl -- $($_.Exception.Message)" @(
            "The binary is installed and works; only the skill is missing.",
            "Copy it from a clone instead:",
            "  git clone $RepoUrl",
            "  Copy-Item -Recurse ost-mcp\skills\ost-mcp `"$skillDir\..`""
        )
    }

    # A proxy or a login page answers with 200 and the wrong body, and a skill
    # without frontmatter loads as nothing at all.
    $firstLine = (Get-Content $skillFile -TotalCount 1)
    if ($firstLine -ne '---') {
        Fail "the file downloaded from $skillUrl is not a skill (it has no frontmatter)." @(
            "Something between you and GitHub answered instead of GitHub.",
            "Inspect it: $skillFile"
        )
    }

    Write-Ok "skill at $skillFile"
}

# ---------------------------------------------------------------- verify

Write-Step "Checking that it works"

$stores = @()
try {
    # 2>$null, not 2>&1: with $ErrorActionPreference = 'Stop', a native command
    # writing to stderr through a redirect raises NativeCommandError.
    $stores = @(& $exe --list 2>$null | Where-Object { $_ -match '\S' })
} catch {
    Write-Warn "--list failed: $_"
}

if ($stores.Count -gt 0) {
    Write-Ok "$($stores.Count) store(s) found in the Outlook profiles"
    foreach ($s in $stores) { Write-Note $s }
} else {
    Write-Warn "no .ost or .pst found. The binary works, but there is nothing to read."
    Write-Note "Outlook may not be set up on this machine. Pass a file explicitly:"
    Write-Note "  ost-mcp `"D:\archive.pst`" --info"
}

$mounted = $false
try {
    $info = & $exe --info 2>$null | ConvertFrom-Json
    if ($info) {
        Write-Ok "mounted backend $($info.kind), $($info.folders) folders"
        $mounted = $true
    }
} catch {
    Write-Note "could not open a store automatically; not fatal"
}

if ($mounted) {
    try {
        $n = (& $exe --sql "SELECT count(*) AS n FROM messages" 2>$null | ConvertFrom-Json)[0].n
        Write-Ok "queried $n messages"
    } catch {
        Write-Warn "the store opened but a query failed: $_"
    }
}

# ------------------------------------------------------------------ done

Write-Host ""
Write-Host "ost-mcp is installed." -ForegroundColor Green
Write-Host ""
Write-Host "  ost-mcp --info                     what is mounted"
Write-Host "  ost-mcp --sql `"SELECT ...`"          query it"
Write-Host "  ost-mcp --message <nid>            read one message"
Write-Host ""
if (-not $SkipSkill) {
    Write-Host "Restart your Claude Code session to pick up the skill, then just ask about"
    Write-Host "your mail: `"what came in from finance this week`"."
    Write-Host ""
}
