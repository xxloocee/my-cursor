[CmdletBinding()]
param(
    [switch]$Check,
    [string]$TargetRoot = ""
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$toolRoot = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot ".."))
$projectRoot = if ([string]::IsNullOrWhiteSpace($TargetRoot)) {
    $toolRoot
} else {
    [System.IO.Path]::GetFullPath($TargetRoot)
}
$utf8NoBom = New-Object System.Text.UTF8Encoding($false)
$script:changedFiles = 0
$script:virtualFiles = @{}
$script:removedFiles = @{}
$releaseVersion = "0.1.0"
$msixVersion = "$releaseVersion.0"

function Test-ProjectFile {
    param([Parameter(Mandatory = $true, Position = 0)][string]$RelativePath)

    if ($Check -and $script:removedFiles.ContainsKey($RelativePath)) {
        return $false
    }
    if ($Check -and $script:virtualFiles.ContainsKey($RelativePath)) {
        return $true
    }
    return Test-Path -LiteralPath (Join-Path $projectRoot $RelativePath) -PathType Leaf
}

function Read-ProjectFile {
    param([Parameter(Mandatory = $true, Position = 0)][string]$RelativePath)

    if ($Check -and $script:virtualFiles.ContainsKey($RelativePath)) {
        return $script:virtualFiles[$RelativePath]
    }
    $path = Join-Path $projectRoot $RelativePath
    if (-not (Test-ProjectFile $RelativePath)) {
        throw "Missing required file: $RelativePath"
    }
    return [System.IO.File]::ReadAllText($path)
}

function Read-TemplateFile {
    param([Parameter(Mandatory = $true, Position = 0)][string]$RelativePath)

    $path = Join-Path $toolRoot $RelativePath
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
        throw "Missing required template: $RelativePath"
    }
    return [System.IO.File]::ReadAllText($path)
}

function Write-ProjectFile {
    param(
        [Parameter(Mandatory = $true, Position = 0)][string]$RelativePath,
        [Parameter(Mandatory = $true, Position = 1)][string]$Content
    )

    if ($Check) {
        $script:virtualFiles[$RelativePath] = $Content
        $script:removedFiles.Remove($RelativePath) | Out-Null
        Write-Host "[would update] $RelativePath"
        $script:changedFiles++
        return
    }
    $path = Join-Path $projectRoot $RelativePath
    $parent = Split-Path -Parent $path
    if (-not (Test-Path -LiteralPath $parent -PathType Container)) {
        [System.IO.Directory]::CreateDirectory($parent) | Out-Null
    }
    [System.IO.File]::WriteAllText($path, $Content, $utf8NoBom)
    Write-Host "[updated] $RelativePath"
    $script:changedFiles++
}

function Set-ProjectFileFromTemplate {
    param(
        [Parameter(Mandatory = $true, Position = 0)][string]$RelativePath,
        [Parameter(Mandatory = $true, Position = 1)][string]$TemplatePath
    )

    $current = if (Test-ProjectFile $RelativePath) {
        Read-ProjectFile $RelativePath
    } else {
        $null
    }
    $template = Read-TemplateFile $TemplatePath
    if ($current -eq $template) {
        Write-Host "[ok] $RelativePath"
        return
    }
    Write-ProjectFile $RelativePath $template
}

function Set-ProjectFileFromTemplateGuarded {
    param(
        [Parameter(Mandatory = $true, Position = 0)][string]$RelativePath,
        [Parameter(Mandatory = $true, Position = 1)][string]$TemplatePath,
        [Parameter(Mandatory = $true, Position = 2)][string]$ExpectedSourceSHA256
    )

    $current = Read-ProjectFile $RelativePath
    $template = Read-TemplateFile $TemplatePath
    if ($current -eq $template) {
        Write-Host "[ok] $RelativePath"
        return
    }
    $normalized = $current.Replace("`r`n", "`n")
    $bytes = $utf8NoBom.GetBytes($normalized)
    $hasher = [System.Security.Cryptography.SHA256]::Create()
    try {
        $actual = ([System.BitConverter]::ToString($hasher.ComputeHash($bytes))).Replace("-", "").ToLowerInvariant()
    } finally {
        $hasher.Dispose()
    }
    if ($actual -ne $ExpectedSourceSHA256.ToLowerInvariant()) {
        throw "Upstream changed $RelativePath; merge the device-CA cleanup manually before replaying customization"
    }
    Write-ProjectFile $RelativePath $template
}

function Remove-ProjectFile {
    param([Parameter(Mandatory = $true, Position = 0)][string]$RelativePath)

    $path = Join-Path $projectRoot $RelativePath
    if (-not (Test-ProjectFile $RelativePath)) {
        Write-Host "[ok] $RelativePath absent"
        return
    }
    if ($Check) {
        $script:virtualFiles.Remove($RelativePath) | Out-Null
        $script:removedFiles[$RelativePath] = $true
        Write-Host "[would remove] $RelativePath"
        $script:changedFiles++
        return
    }
    Remove-Item -LiteralPath $path -Force
    Write-Host "[removed] $RelativePath"
    $script:changedFiles++
}

function Ensure-ProjectLine {
    param(
        [Parameter(Mandatory = $true, Position = 0)][string]$RelativePath,
        [Parameter(Mandatory = $true, Position = 1)][string]$Line
    )

    $content = Read-ProjectFile $RelativePath
    $lines = $content -split "`r?`n"
    if ($lines -contains $Line) {
        Write-Host "[ok] $RelativePath"
        return
    }
    $separator = if ($content.Contains("`r`n")) { "`r`n" } else { "`n" }
    $next = $content.TrimEnd("`r", "`n") + $separator + $Line + $separator
    Write-ProjectFile $RelativePath $next
}

function Add-ProjectBlock {
    param(
        [Parameter(Mandatory = $true, Position = 0)][string]$RelativePath,
        [Parameter(Mandatory = $true, Position = 1)][string]$Block,
        [Parameter(Mandatory = $true, Position = 2)][string]$AppliedMarker
    )

    $content = Read-ProjectFile $RelativePath
    if ($content.Contains($AppliedMarker)) {
        Write-Host "[ok] $RelativePath"
        return
    }
    $separator = if ($content.Contains("`r`n")) { "`r`n" } else { "`n" }
    $normalizedBlock = $Block.Replace("`r`n", "`n").Replace("`n", $separator).Trim("`r", "`n")
    $next = $content.TrimEnd("`r", "`n") + $separator + $separator + $normalizedBlock + $separator
    Write-ProjectFile $RelativePath $next
}

function Replace-ProjectLiteral {
    param(
        [Parameter(Mandatory = $true, Position = 0)][string]$RelativePath,
        [Parameter(Mandatory = $true, Position = 1)][string]$Before,
        [Parameter(Mandatory = $true, Position = 2)][AllowEmptyString()][string]$After
    )

    $content = Read-ProjectFile $RelativePath
    if ($content.Contains($Before)) {
        Write-ProjectFile $RelativePath ($content.Replace($Before, $After))
        return
    }
    if ($After.Length -gt 0 -and $content.Contains($After)) {
        Write-Host "[ok] $RelativePath"
        return
    }
    throw "Expected source or target was not found in ${RelativePath}: before=[$Before], after=[$After]"
}

function Replace-ProjectBlock {
    param(
        [Parameter(Mandatory = $true, Position = 0)][string]$RelativePath,
        [Parameter(Mandatory = $true, Position = 1)][string]$Before,
        [Parameter(Mandatory = $true, Position = 2)][AllowEmptyString()][string]$After,
        [Parameter(Mandatory = $true, Position = 3)][string]$AppliedMarker
    )

    $content = Read-ProjectFile $RelativePath
    $usesCrlf = $content.Contains("`r`n")
    $normalized = $content.Replace("`r`n", "`n")
    $normalizedBefore = $Before.Replace("`r`n", "`n")
    $normalizedAfter = $After.Replace("`r`n", "`n")
    if ($normalizedAfter.Length -gt 0 -and $normalized.Contains($normalizedAfter)) {
        Write-Host "[ok] $RelativePath"
        return
    }
    if ($normalizedAfter.Length -eq 0 -and $normalized.Contains($AppliedMarker) -and -not $normalized.Contains($normalizedBefore)) {
        Write-Host "[ok] $RelativePath"
        return
    }
    if ($normalized.Contains($normalizedBefore)) {
        $next = $normalized.Replace($normalizedBefore, $normalizedAfter)
        if ($usesCrlf) {
            $next = $next.Replace("`n", "`r`n")
        }
        Write-ProjectFile $RelativePath $next
        return
    }
    throw "Expected source or Ergouzi target block was not found in $RelativePath"
}

function Replace-ProjectRegex {
    param(
        [Parameter(Mandatory = $true, Position = 0)][string]$RelativePath,
        [Parameter(Mandatory = $true, Position = 1)][string]$Pattern,
        [Parameter(Mandatory = $true, Position = 2)][AllowEmptyString()][string]$Replacement,
        [Parameter(Mandatory = $true, Position = 3)][string]$AppliedPattern
    )

    $content = Read-ProjectFile $RelativePath
    if ([regex]::IsMatch($content, $Pattern)) {
        $next = [regex]::Replace($content, $Pattern, $Replacement, 1)
        Write-ProjectFile $RelativePath $next
        return
    }
    if (($Replacement.Length -eq 0 -and -not [regex]::IsMatch($content, $Pattern)) -or
        ($Replacement.Length -gt 0 -and [regex]::IsMatch($content, $AppliedPattern))) {
        Write-Host "[ok] $RelativePath"
        return
    }
    throw "Expected source or target pattern was not found in ${RelativePath}: pattern=[$Pattern], applied=[$AppliedPattern]"
}

function Set-ProjectRegex {
    param(
        [Parameter(Mandatory = $true, Position = 0)][string]$RelativePath,
        [Parameter(Mandatory = $true, Position = 1)][string]$Pattern,
        [Parameter(Mandatory = $true, Position = 2)][string]$Replacement
    )

    $content = Read-ProjectFile $RelativePath
    if (-not [regex]::IsMatch($content, $Pattern)) {
        throw "Expected version field was not found in $RelativePath"
    }
    $next = [regex]::Replace($content, $Pattern, $Replacement, 1)
    if ($next -eq $content) {
        Write-Host "[ok] $RelativePath"
        return
    }
    Write-ProjectFile $RelativePath $next
}

function Assert-ProjectPresent {
    param(
        [Parameter(Mandatory = $true, Position = 0)][string]$RelativePath,
        [Parameter(Mandatory = $true, Position = 1)][string]$Expected
    )

    $content = Read-ProjectFile $RelativePath
    if (-not $content.Contains($Expected)) {
        throw "Required customized content was not found in $RelativePath"
    }
}

function Assert-ProjectAbsent {
    param(
        [Parameter(Mandatory = $true, Position = 0)][string]$RelativePath,
        [Parameter(Mandatory = $true, Position = 1)][string]$Unexpected
    )

    $content = Read-ProjectFile $RelativePath
    if ($content.Contains($Unexpected)) {
        throw "Unexpected upstream content remains in $RelativePath"
    }
}

function Assert-ProjectCount {
    param(
        [Parameter(Mandatory = $true, Position = 0)][string]$RelativePath,
        [Parameter(Mandatory = $true, Position = 1)][string]$Expected,
        [Parameter(Mandatory = $true, Position = 2)][int]$Count
    )

    $content = Read-ProjectFile $RelativePath
    $actual = [regex]::Matches($content, [regex]::Escape($Expected)).Count
    if ($actual -ne $Count) {
        throw "Expected $Count occurrence(s) in $RelativePath, found $actual"
    }
}

$legacyFetchComment = '// FetchURL 保留旧名称表示默认广告位地址。'
$ergouziFetchComment = '// FetchURL 保留旧名称以兼容现有资源加载接口。'
Replace-ProjectLiteral -RelativePath "internal/ads/types.go" -Before $legacyFetchComment -After $ergouziFetchComment
Replace-ProjectLiteral "internal/ads/types.go" 'FetchURL = "https://ads.leokun.cn/1"' 'FetchURL = ""'

Replace-ProjectRegex "internal/ads/types.go" '(?s)var Slots = \[\]Slot\{\s*\{ID: "1", FetchURL: "https://ads\.leokun\.cn/1"\},\s*\{ID: "2", FetchURL: "https://ads\.leokun\.cn/2"\},\s*\{ID: "3", FetchURL: "https://ads\.leokun\.cn/3"\},\s*\}' 'var Slots = []Slot{}' 'var Slots = \[\]Slot\{\}'

Replace-ProjectLiteral "internal/bridge/window_author.go" 'const footerAuthorHomeURL = "https://space.bilibili.com/311706663/upload/video"' 'const footerAuthorHomeURL = "https://ergouzi.life"'
Replace-ProjectLiteral "internal/bridge/window_author.go" 'ButtonText:        "作者 leookun"' 'ButtonText:        "Ergouzi"'
Replace-ProjectLiteral "internal/bridge/window_author.go" 'DialogTitle:       "作者寄语"' 'DialogTitle:       "关于 Ergouzi"'
Replace-ProjectLiteral "internal/bridge/window_author.go" 'DialogContent:     "本软件是纯免费软件，如果你被收费，那大概率就是被骗了。\n欢迎点击访问作者主页 https://space.bilibili.com/311706663/upload/video\n查看更多更新动态、使用分享和后续内容。"' 'DialogContent:     "My Cursor 提供 Cursor 本地服务与自定义模型 API 管理能力。\n访问 https://ergouzi.life 了解更多内容。"'
Replace-ProjectLiteral "internal/bridge/window_author.go" 'DialogConfirmText: "访问主页"' 'DialogConfirmText: "访问 Ergouzi"'

Replace-ProjectLiteral "internal/buildinfo/buildinfo.go" 'ReleaseRepo    = "leookun/cursor-byok"' 'ReleaseRepo    = "xxloocee/my-cursor"'
Replace-ProjectLiteral "internal/buildinfo/buildinfo.go" 'UpdateBaseURL  = "https://github.com/leookun/cursor-byok/releases/latest/download/"' 'UpdateBaseURL  = "https://github.com/xxloocee/my-cursor/releases/latest/download/"'
Replace-ProjectLiteral "internal/buildinfo/buildinfo.go" 'ReleasePageURL = "https://github.com/leookun/cursor-byok/releases"' 'ReleasePageURL = "https://github.com/xxloocee/my-cursor/releases"'

$legacyEmbeddedCABlock = @'
// embeddedCACertPEM 表示当前模块中的 embeddedCACertPEM 状态值。
//
//go:embed ca.crt
var embeddedCACertPEM []byte

// embeddedCAKeyPEM 表示当前模块中的 embeddedCAKeyPEM 状态值。
//
//go:embed ca.key
var embeddedCAKeyPEM []byte
'@
$deviceCABlock = @'
// legacyCACertPEM 仅用于移除旧版本安装到系统信任存储的共享 CA。
//
//go:embed ca.crt
var legacyCACertPEM []byte
'@
Replace-ProjectBlock "internal/certs/ca.go" $legacyEmbeddedCABlock $deviceCABlock 'var legacyCACertPEM []byte'

$legacyEmbeddedCAFunctions = @'
// NewEmbeddedManager 用于处理与 NewEmbeddedManager 相关的逻辑。
func NewEmbeddedManager() (*Manager, error) {
	return NewManagerFromPEM(embeddedCACertPEM, embeddedCAKeyPEM)
}

// EmbeddedCACertPEM 用于处理与 EmbeddedCACertPEM 相关的逻辑。
func EmbeddedCACertPEM() []byte {
	return cloneBytes(embeddedCACertPEM)
}

// EmbeddedCAKeyPEM 用于处理与 EmbeddedCAKeyPEM 相关的逻辑。
func EmbeddedCAKeyPEM() []byte {
	return cloneBytes(embeddedCAKeyPEM)
}
'@
$deviceCAFunctions = @'
// LegacyCACertPEM 返回旧版本共享 CA 的公开证书，用于迁移清理。
func LegacyCACertPEM() []byte {
	return cloneBytes(legacyCACertPEM)
}
'@
Replace-ProjectBlock "internal/certs/ca.go" $legacyEmbeddedCAFunctions $deviceCAFunctions 'func LegacyCACertPEM() []byte'

$legacyManagerReturn = @'
	return &Manager{caCert: caCert, caKey: caKey, cache: make(map[string]*tls.Certificate)}, nil
'@
$validatedManagerReturn = @'
	if err := validateCAPair(caCert, caKey); err != nil {
		return nil, err
	}
	return &Manager{caCert: caCert, caKey: caKey, cache: make(map[string]*tls.Certificate)}, nil
'@
Replace-ProjectBlock "internal/certs/ca.go" $legacyManagerReturn $validatedManagerReturn 'validateCAPair(caCert, caKey)'
$legacyGoDependencies = @'
	github.com/go-chi/chi/v5 v5.2.5
'@
$deviceGoDependencies = @'
	github.com/go-chi/chi/v5 v5.2.5
	github.com/gofrs/flock v0.13.0
'@
Replace-ProjectBlock "go.mod" $legacyGoDependencies $deviceGoDependencies 'github.com/gofrs/flock v0.13.0'
Set-ProjectFileFromTemplate "internal/certs/local_ca.go" "scripts/branding/local-ca.go.template"
Remove-ProjectFile "internal/certs/ca.key"

$legacyCAPathBlock = @'
// CACertFilePath 返回注入给宿主的 CA 文件路径。
func CACertFilePath() string {
	return filepath.Join(DataRootPath(), "ca.crt")
}
'@
$deviceCAPathBlock = @'
// CACertFilePath 返回注入给宿主的 CA 文件路径。
func CACertFilePath() string {
	return filepath.Join(DataRootPath(), "ca.crt")
}

// CAKeyFilePath 返回设备专属 CA 私钥路径。
func CAKeyFilePath() string {
	return filepath.Join(DataRootPath(), "ca.key")
}
'@
Replace-ProjectBlock "internal/appdata/paths.go" $legacyCAPathBlock $deviceCAPathBlock 'func CAKeyFilePath() string'

$legacyCARunner = @'
	embeddedCACertPEM := certs.EmbeddedCACertPEM()
	logEmbeddedCAInfo(embeddedCACertPEM)

	certManager, err := certs.NewEmbeddedManager()
	if err != nil {
		return err
	}
'@
$deviceCARunner = @'
	certManager, localCACertPEM, err := certs.LoadOrCreateManager(
		appdata.CACertFilePath(),
		appdata.CAKeyFilePath(),
	)
	if err != nil {
		return err
	}
	logCAInfo("device CA", localCACertPEM)
'@
Replace-ProjectBlock "internal/app/runner.go" $legacyCARunner $deviceCARunner 'certs.LoadOrCreateManager('
Replace-ProjectLiteral "internal/app/runner.go" 'bridge.NewProxyService(proxyServer, certManager, embeddedCACertPEM)' 'bridge.NewProxyService(proxyServer, certManager, localCACertPEM)'

$legacyCALogging = @'
// logEmbeddedCAInfo 用于处理与 logEmbeddedCAInfo 相关的逻辑。
func logEmbeddedCAInfo(certPEM []byte) {
	if len(certPEM) == 0 {
		logger.Errorf("embedded CA is empty")
		return
	}
	cert, err := parseEmbeddedCert(certPEM)
	if err != nil {
		logger.Errorf("parse embedded CA failed: %v", err)
		return
	}
	sum := sha256.Sum256(cert.Raw)
	logger.Infof(
		"embedded CA loaded: sha256=%s subject=%s valid=%s~%s",
		strings.ToUpper(hex.EncodeToString(sum[:])),
		cert.Subject.String(),
		cert.NotBefore.Format(time.RFC3339),
		cert.NotAfter.Format(time.RFC3339),
	)
}

// parseEmbeddedCert 用于处理与 parseEmbeddedCert 相关的逻辑。
func parseEmbeddedCert(data []byte) (*x509.Certificate, error) {
'@
$deviceCALogging = @'
// logCAInfo 记录 CA 的公开信息，不输出私钥内容。
func logCAInfo(label string, certPEM []byte) {
	if len(certPEM) == 0 {
		logger.Errorf("%s is empty", label)
		return
	}
	cert, err := parseCert(certPEM)
	if err != nil {
		logger.Errorf("parse %s failed: %v", label, err)
		return
	}
	sum := sha256.Sum256(cert.Raw)
	logger.Infof(
		"%s loaded: sha256=%s subject=%s valid=%s~%s",
		label,
		strings.ToUpper(hex.EncodeToString(sum[:])),
		cert.Subject.String(),
		cert.NotBefore.Format(time.RFC3339),
		cert.NotAfter.Format(time.RFC3339),
	)
}

// parseCert 解析 PEM 或 DER 证书。
func parseCert(data []byte) (*x509.Certificate, error) {
'@
Replace-ProjectBlock "internal/app/runner.go" $legacyCALogging $deviceCALogging 'func logCAInfo(label string, certPEM []byte)'

Set-ProjectFileFromTemplateGuarded "internal/client/cursor.go" "scripts/branding/client-cursor.go.template" "9f8ee0fddd4de511faadd854ffbaf122c04126ea1c4d7d8c5c4d65438b5a4dc4"

$windowsCARemoval = @'
// RemoveCACertInstalled 从 Windows 系统信任存储移除指定 CA。
func RemoveCACertInstalled(certPEM []byte) error {
	installed, err := isCACertInstalled(certPEM)
	if err != nil {
		return fmt.Errorf("检查系统证书安装状态失败: %w", err)
	}
	if !installed {
		return nil
	}
	thumbprint, err := getCertThumbprint(certPEM)
	if err != nil {
		return fmt.Errorf("获取证书指纹失败: %w", err)
	}
	if err := runElevatedCertutil("-delstore", windowsRootStoreName, thumbprint); err != nil {
		return err
	}
	installed, err = isCACertInstalled(certPEM)
	if err != nil {
		return fmt.Errorf("验证系统证书移除状态失败: %w", err)
	}
	if installed {
		return fmt.Errorf("证书删除命令已执行，但系统信任存储中仍存在该证书")
	}
	logger.Infof("removeCACertInstalled: cert removed from system store, thumbprint=%s", thumbprint)
	return nil
}
'@
Add-ProjectBlock "internal/cursor/windows_cert.go" $windowsCARemoval 'func RemoveCACertInstalled(certPEM []byte) error'

$darwinCARemoval = @'
// RemoveCACertInstalled 从 macOS 登录钥匙串移除指定 CA。
func RemoveCACertInstalled(certPEM []byte) error {
	installed, err := isCACertInstalled(certPEM)
	if err != nil {
		return fmt.Errorf("检查 macOS 证书安装状态失败: %w", err)
	}
	if !installed {
		return nil
	}
	fingerprint, err := getCertSHA1Fingerprint(certPEM)
	if err != nil {
		return fmt.Errorf("获取证书指纹失败: %w", err)
	}
	out, err := exec.Command(
		darwinSecurityExe,
		"delete-certificate",
		"-Z", fingerprint,
		darwinLoginKeychainName,
	).CombinedOutput()
	if err != nil {
		return fmt.Errorf("从 macOS 登录钥匙串移除 CA 失败: %w: %s", err, strings.TrimSpace(string(out)))
	}
	installed, err = isCACertInstalled(certPEM)
	if err != nil {
		return fmt.Errorf("验证 macOS 证书移除状态失败: %w", err)
	}
	if installed {
		return fmt.Errorf("证书删除命令已执行，但 macOS 登录钥匙串中仍存在该证书")
	}
	logger.Infof("removeCACertInstalled: cert removed from macOS login keychain, fingerprint=%s", fingerprint)
	return nil
}
'@
Add-ProjectBlock "internal/cursor/darwin_cert.go" $darwinCARemoval 'func RemoveCACertInstalled(certPEM []byte) error'

$otherPlatformCARemoval = @'
// RemoveCACertInstalled 非 Windows/macOS 平台无需操作系统证书清理。
func RemoveCACertInstalled(_ []byte) error {
	return nil
}
'@
Add-ProjectBlock "internal/cursor/windows_cert_stub.go" $otherPlatformCARemoval 'func RemoveCACertInstalled(_ []byte) error'

Replace-ProjectLiteral "internal/cursor/settings.go" "读取内置 CA 证书失败" "读取设备 CA 证书失败"
Replace-ProjectLiteral "internal/cursor/settings.go" "写入内置 CA 证书失败" "写入设备 CA 证书失败"
Replace-ProjectLiteral "internal/certs/doc.go" "本地 MITM 所需的内置 CA 证书" "本地 MITM 所需的设备 CA 证书"

$legacyBackendCAPaths = @'
- `~/.cursor-local-assistant-v2/data/ca.crt`
- `~/.cursor-local-assistant-v2/data/ads/`
'@
$deviceBackendCAPaths = @'
- `~/.cursor-local-assistant-v2/data/ca.crt`
- `~/.cursor-local-assistant-v2/data/ca.key`
- `~/.cursor-local-assistant-v2/data/ads/`
'@
Replace-ProjectBlock "internal/backend/README.md" $legacyBackendCAPaths $deviceBackendCAPaths '`~/.cursor-local-assistant-v2/data/ca.key`'
Replace-ProjectLiteral "internal/backend/README.md" '- `data/ca.crt` 是注入给宿主的 CA 证书' '- `data/ca.crt` 与 `data/ca.key` 是首次运行时生成的设备专属 CA；私钥只保存在本地数据目录'

Replace-ProjectLiteral "Taskfile.yml" 'RELEASE_REPO: "leookun/cursor-byok"' 'RELEASE_REPO: "xxloocee/my-cursor"'
Replace-ProjectLiteral "Taskfile.yml" 'summary: 安装广告页依赖' 'summary: 安装上游推荐页依赖'
Replace-ProjectLiteral "Taskfile.yml" 'summary: 启动广告页开发服务（http://127.0.0.1:5174/ad/）' 'summary: 启动上游推荐页开发服务（http://127.0.0.1:5174/ad/）'
Replace-ProjectLiteral "Taskfile.yml" 'summary: 构建广告 ZIP 产物' 'summary: 构建上游推荐页 ZIP 产物'
Replace-ProjectLiteral "Taskfile.yml" 'RELEASE_BASE_NAME: "cursor-byok"' 'RELEASE_BASE_NAME: "my-cursor"'
Replace-ProjectLiteral "scripts/release/main.go" 'flags.String("base-name", "cursor-byok", "release asset basename")' 'flags.String("base-name", "my-cursor", "release asset basename")'

$displayNameFiles = @(
    "frontend/index.html",
    "frontend/src/layouts/MainLayout.vue",
    "frontend/src/router/index.js",
    "internal/app/runner.go",
    "build/config.yml",
    "build/windows/info.json",
    "build/dmg-extras/提示损坏？点我.command"
)
foreach ($displayNameFile in $displayNameFiles) {
    Replace-ProjectLiteral $displayNameFile "Cursor助手" "My Cursor"
}

$legacyProductNameCatalogEntry = @'

    "991e374fce0f4492": {
      "source": "Cursor助手",
      "kind": "text",
      "placeholders": 0,
      "refs": [
        {
          "file": "src/layouts/MainLayout.vue",
          "line": 16,
          "column": 50
        },
        {
          "file": "src/router/index.js",
          "line": 12,
          "column": 38
        }
      ]
    },
'@
Replace-ProjectBlock "frontend/src/i18n/generated/catalog.json" $legacyProductNameCatalogEntry '' '"9970736b36ff2b68"'
foreach ($localeFile in @(
    "frontend/src/i18n/locales/zh-CN.json",
    "frontend/src/i18n/locales/en-US.json",
    "frontend/src/i18n/locales/ja-JP.json"
)) {
    Replace-ProjectRegex $localeFile '(?m)^\s*"991e374fce0f4492":\s*"[^"]*",\r?\n' '' '(?s)^(?!.*"991e374fce0f4492").*'
}

$legacyTaskNames = @'
  APP_NAME: "Cursor助手"
'@
$ergouziTaskNames = @'
  APP_NAME: "my-cursor"
  DISPLAY_NAME: "My Cursor"
'@
Replace-ProjectBlock "Taskfile.yml" $legacyTaskNames $ergouziTaskNames 'DISPLAY_NAME: "My Cursor"'
Replace-ProjectLiteral "Taskfile.yml" "windows-32" "my-cursor-windows-386"
Replace-ProjectLiteral "Taskfile.yml" "windows-64" "my-cursor"
Replace-ProjectLiteral "Taskfile.yml" 'OUTPUT: ''{{.BIN_DIR}}/{{.RELEASE_BASE_NAME}}-windows-amd64.exe''' 'OUTPUT: ''{{.BIN_DIR}}/my-cursor.exe'''
Replace-ProjectBlock "Taskfile.yml" @'
  release:build:macos:amd64:
    internal: true
    cmds:
'@ @'
  release:build:macos:amd64:
    cmds:
'@ 'release:build:macos:amd64:'
foreach ($releaseTask in @(
    "release:build:macos:arm64",
    "release:build:macos:amd64",
    "release:build:linux:amd64"
)) {
    $releaseTaskPattern = '(?m)(?<prefix>^  ' + [regex]::Escape($releaseTask) + ':\r?\n    cmds:\r?\n)(?!      - task: release:ensure:dir\r?\n)'
    $releaseTaskReplacement = '${prefix}      - task: release:ensure:dir' + "`n"
    Replace-ProjectRegex "Taskfile.yml" $releaseTaskPattern $releaseTaskReplacement ('(?m)^  ' + [regex]::Escape($releaseTask) + ':\r?\n    cmds:\r?\n      - task: release:ensure:dir\r?$')
}
$legacyWindowsReleaseTask = @'
  release:build:windows:amd64:
    cmds:
      - task: windows:create:zip
'@
$ergouziWindowsReleaseTask = @'
  release:build:windows:amd64:
    cmds:
      - task: release:ensure:dir
      - task: windows:create:zip
'@
Replace-ProjectBlock "Taskfile.yml" $legacyWindowsReleaseTask $ergouziWindowsReleaseTask $ergouziWindowsReleaseTask
Replace-ProjectLiteral "Taskfile.yml" '"{{.BIN_DIR}}/{{.APP_NAME}}.app" "{{.BIN_DIR}}/{{.APP_NAME}}.dev.app"' '"{{.BIN_DIR}}/{{.DISPLAY_NAME}}.app" "{{.BIN_DIR}}/{{.DISPLAY_NAME}}.dev.app"'
Replace-ProjectLiteral "Taskfile.yml" "APP_BUNDLE: '{{.APP_NAME}}.app'" "APP_BUNDLE: '{{.DISPLAY_NAME}}.app'"

Replace-ProjectRegex "Taskfile.yml" '(?ms)^  release:sync:readme:\r?\n.*?(?=^  release:github:)' '' '(?s)^(?!.*release:sync:readme:).*'
Replace-ProjectRegex "Taskfile.yml" '(?m)^\s*- task: release:sync:readme\r?\n' '' '(?s)^(?!.*- task: release:sync:readme).*'

Replace-ProjectLiteral "build/Taskfile.yml" '-name "{{.DISPLAY_NAME}}" -binaryname "{{.APP_NAME}}"' '-name "{{.APP_NAME}}" -binaryname "{{.APP_NAME}}"'

$legacyWindowsZipSummary = 'summary: 生成 Windows ZIP 包（包含 exe 与 certs）'
$ergouziWindowsZipSummary = 'summary: 生成 Windows ZIP 包（包含 exe 与 LICENSE）'
Replace-ProjectLiteral "build/windows/Taskfile.yml" $legacyWindowsZipSummary $ergouziWindowsZipSummary

$legacyWindowsZipCommands = @'
      - rm -rf "{{.STAGING_DIR}}"
      - mkdir -p "{{.STAGING_DIR}}"
      - cp "{{.OUTPUT}}" "{{.STAGING_DIR}}/"
      - (cd "{{.STAGING_DIR}}" && zip -qry "../{{.ZIP_NAME}}" .)
      - rm -rf "{{.STAGING_DIR}}"
      - rm -f "{{.OUTPUT}}"
'@
$ergouziWindowsZipCommands = @'
      - cmd: powershell -NoProfile -Command "if (Test-Path -LiteralPath '{{.STAGING_DIR}}') { Remove-Item -LiteralPath '{{.STAGING_DIR}}' -Recurse -Force }"
        platforms: [windows]
      - cmd: rm -rf "{{.STAGING_DIR}}"
        platforms: [darwin, linux]
      - mkdir -p "{{.STAGING_DIR}}"
      - cp "{{.OUTPUT}}" "{{.STAGING_DIR}}/"
      - cp LICENSE "{{.STAGING_DIR}}/LICENSE"
      - cmd: powershell -NoProfile -Command "Compress-Archive -Path '{{.STAGING_DIR}}\*' -DestinationPath '{{.BIN_DIR}}\{{.ZIP_NAME}}' -Force"
        platforms: [windows]
      - cmd: (cd "{{.STAGING_DIR}}" && zip -qry "../{{.ZIP_NAME}}" .)
        platforms: [darwin, linux]
      - cmd: powershell -NoProfile -Command "Remove-Item -LiteralPath '{{.STAGING_DIR}}' -Recurse -Force"
        platforms: [windows]
      - cmd: rm -rf "{{.STAGING_DIR}}"
        platforms: [darwin, linux]
      - cmd: powershell -NoProfile -Command "Remove-Item -LiteralPath '{{.OUTPUT}}' -Force"
        platforms: [windows]
      - cmd: rm -f "{{.OUTPUT}}"
        platforms: [darwin, linux]
'@
Replace-ProjectBlock "build/windows/Taskfile.yml" $legacyWindowsZipCommands $ergouziWindowsZipCommands 'cp LICENSE "{{.STAGING_DIR}}/LICENSE"'

Replace-ProjectLiteral "build/darwin/Taskfile.yml" '(printf "%s.app" .APP_NAME)' '(printf "%s.app" .DISPLAY_NAME)'
Replace-ProjectLiteral "build/darwin/Taskfile.yml" '"{{.STAGING_DIR}}/{{.APP_NAME}}.app"' '"{{.STAGING_DIR}}/{{.DISPLAY_NAME}}.app"'
Replace-ProjectLiteral "build/darwin/Taskfile.yml" '--volname "{{.APP_NAME}}"' '--volname "{{.DISPLAY_NAME}}"'
Replace-ProjectLiteral "build/darwin/Taskfile.yml" 'hdiutil create -volname "{{.APP_NAME}}"' 'hdiutil create -volname "{{.DISPLAY_NAME}}"'
Replace-ProjectLiteral "build/darwin/Taskfile.yml" '"{{.BIN_DIR}}/{{.APP_NAME}}.dev.app' '"{{.BIN_DIR}}/{{.DISPLAY_NAME}}.dev.app'
Replace-ProjectLiteral "build/darwin/Taskfile.yml" "'{{.BIN_DIR}}/{{.APP_NAME}}.dev.app/Contents/MacOS/{{.EXECUTABLE_NAME}}'" "'{{.BIN_DIR}}/{{.DISPLAY_NAME}}.dev.app/Contents/MacOS/{{.EXECUTABLE_NAME}}'"
$legacyDarwinLicense = @'
      - cp build/darwin/icons.icns "{{.BIN_DIR}}/{{.APP_BUNDLE}}/Contents/Resources"
      - cp "{{.BINARY_PATH}}" "{{.BIN_DIR}}/{{.APP_BUNDLE}}/Contents/MacOS/{{.EXECUTABLE_NAME}}"
'@
$ergouziDarwinLicense = @'
      - cp build/darwin/icons.icns "{{.BIN_DIR}}/{{.APP_BUNDLE}}/Contents/Resources"
      - cp LICENSE "{{.BIN_DIR}}/{{.APP_BUNDLE}}/Contents/Resources/LICENSE"
      - cp "{{.BINARY_PATH}}" "{{.BIN_DIR}}/{{.APP_BUNDLE}}/Contents/MacOS/{{.EXECUTABLE_NAME}}"
'@
Replace-ProjectBlock "build/darwin/Taskfile.yml" $legacyDarwinLicense $ergouziDarwinLicense 'cp LICENSE "{{.BIN_DIR}}/{{.APP_BUNDLE}}/Contents/Resources/LICENSE"'

$legacyDarwinExecutable = @'
		<key>CFBundleExecutable</key>
		<string>Cursor助手</string>
'@
$ergouziDarwinExecutable = @'
		<key>CFBundleExecutable</key>
		<string>my-cursor</string>
'@
Replace-ProjectBlock "build/darwin/Info.plist" $legacyDarwinExecutable $ergouziDarwinExecutable '<string>my-cursor</string>'
Replace-ProjectBlock "build/darwin/Info.dev.plist" $legacyDarwinExecutable $ergouziDarwinExecutable '<string>my-cursor</string>'
Replace-ProjectLiteral "build/darwin/Info.plist" "Cursor助手" "My Cursor"
Replace-ProjectLiteral "build/darwin/Info.dev.plist" "Cursor助手" "My Cursor"

Replace-ProjectLiteral "build/linux/desktop" 'Exec=/usr/local/bin/Cursor助手 %u' 'Exec=/usr/local/bin/my-cursor %u'
Replace-ProjectLiteral "build/linux/desktop" 'Icon=Cursor助手' 'Icon=my-cursor'
Replace-ProjectLiteral "build/linux/desktop" 'StartupWMClass=Cursor助手' 'StartupWMClass=my-cursor'
Replace-ProjectLiteral "build/linux/desktop" "Cursor助手" "My Cursor"

Replace-ProjectLiteral "build/linux/nfpm/nfpm.yaml" 'name: "Cursor助手"' 'name: "my-cursor"'
Replace-ProjectLiteral "build/linux/nfpm/nfpm.yaml" 'description: "Cursor助手"' 'description: "My Cursor"'
Replace-ProjectLiteral "build/linux/nfpm/nfpm.yaml" 'vendor: "Cursor助手"' 'vendor: "My Cursor"'
Replace-ProjectLiteral "build/linux/nfpm/nfpm.yaml" '"./bin/Cursor助手"' '"./bin/my-cursor"'
Replace-ProjectLiteral "build/linux/nfpm/nfpm.yaml" '"/usr/local/bin/Cursor助手"' '"/usr/local/bin/my-cursor"'
Replace-ProjectLiteral "build/linux/nfpm/nfpm.yaml" '"/usr/share/icons/hicolor/128x128/apps/Cursor助手.png"' '"/usr/share/icons/hicolor/128x128/apps/my-cursor.png"'
Replace-ProjectLiteral "build/linux/nfpm/nfpm.yaml" '"./build/linux/Cursor助手.desktop"' '"./build/linux/desktop"'
Replace-ProjectLiteral "build/linux/nfpm/nfpm.yaml" '"/usr/share/applications/Cursor助手.desktop"' '"/usr/share/applications/my-cursor.desktop"'

$legacyNfpmDesktop = @'
  - src: "./build/linux/desktop"
    dst: "/usr/share/applications/my-cursor.desktop"
'@
$ergouziNfpmDesktop = @'
  - src: "./build/linux/desktop"
    dst: "/usr/share/applications/my-cursor.desktop"
  - src: "./LICENSE"
    dst: "/usr/share/licenses/my-cursor/LICENSE"
'@
Replace-ProjectBlock "build/linux/nfpm/nfpm.yaml" $legacyNfpmDesktop $ergouziNfpmDesktop $ergouziNfpmDesktop

$legacyLinuxArchive = @'
      - cp "{{.BINARY_PATH}}" "{{.STAGING_DIR}}/{{.ARCHIVE_BINARY_NAME}}"
      - chmod +x "{{.STAGING_DIR}}/{{.ARCHIVE_BINARY_NAME}}"
      - rm -f "{{.ARCHIVE_PATH}}"
      - env LC_ALL=C tar -C "{{.STAGING_DIR}}" -czf "{{.ARCHIVE_PATH}}" "{{.ARCHIVE_BINARY_NAME}}"
'@
$ergouziLinuxArchive = @'
      - cp "{{.BINARY_PATH}}" "{{.STAGING_DIR}}/{{.ARCHIVE_BINARY_NAME}}"
      - chmod +x "{{.STAGING_DIR}}/{{.ARCHIVE_BINARY_NAME}}"
      - mkdir -p "{{.STAGING_DIR}}/licenses"
      - cp LICENSE "{{.STAGING_DIR}}/licenses/LICENSE"
      - rm -f "{{.ARCHIVE_PATH}}"
      - env LC_ALL=C tar -C "{{.STAGING_DIR}}" -czf "{{.ARCHIVE_PATH}}" "{{.ARCHIVE_BINARY_NAME}}" licenses
'@
Replace-ProjectBlock "build/linux/Taskfile.yml" $legacyLinuxArchive $ergouziLinuxArchive 'cp LICENSE "{{.STAGING_DIR}}/licenses/LICENSE"'

Replace-ProjectLiteral "build/windows/nsis/wails_tools.nsh" '!define INFO_PROJECTNAME "Cursor助手"' '!define INFO_PROJECTNAME "my-cursor"'
Replace-ProjectLiteral "build/windows/nsis/wails_tools.nsh" "Cursor助手" "My Cursor"

Set-ProjectRegex "build/config.yml" '(?m)^(?<prefix>  version: ")[^"\r\n]+(?<suffix>")' ('${prefix}' + $releaseVersion + '${suffix}')
Set-ProjectRegex "build/windows/info.json" '(?m)^(?<prefix>\s*"file_version":\s*")[^"\r\n]+(?<suffix>")' ('${prefix}' + $releaseVersion + '${suffix}')
Set-ProjectRegex "build/windows/info.json" '(?m)^(?<prefix>\s*"ProductVersion":\s*")[^"\r\n]+(?<suffix>")' ('${prefix}' + $releaseVersion + '${suffix}')
Set-ProjectRegex "build/windows/wails.exe.manifest" '(?<prefix><assemblyIdentity type="win32" name="com\.cursor\.wuxianxubei" version=")[^"]+(?<suffix>")' ('${prefix}' + $releaseVersion + '${suffix}')
Set-ProjectRegex "build/windows/nsis/wails_tools.nsh" '(?m)^(?<prefix>\s*!define INFO_PRODUCTVERSION ")[^"\r\n]+(?<suffix>")' ('${prefix}' + $releaseVersion + '${suffix}')
Set-ProjectRegex "build/darwin/Info.plist" '(?<prefix><key>CFBundleShortVersionString</key>\s*<string>)[^<]+(?<suffix></string>)' ('${prefix}' + $releaseVersion + '${suffix}')
Set-ProjectRegex "build/darwin/Info.plist" '(?<prefix><key>CFBundleVersion</key>\s*<string>)[^<]+(?<suffix></string>)' ('${prefix}' + $releaseVersion + '${suffix}')
Set-ProjectRegex "build/darwin/Info.dev.plist" '(?<prefix><key>CFBundleShortVersionString</key>\s*<string>)[^<]+(?<suffix></string>)' ('${prefix}' + $releaseVersion + '${suffix}')
Set-ProjectRegex "build/darwin/Info.dev.plist" '(?<prefix><key>CFBundleVersion</key>\s*<string>)[^<]+(?<suffix></string>)' ('${prefix}' + $releaseVersion + '${suffix}')
Set-ProjectRegex "build/linux/nfpm/nfpm.yaml" '(?m)^(?<prefix>version: ")[^"\r\n]+(?<suffix>")' ('${prefix}' + $releaseVersion + '${suffix}')
Set-ProjectRegex "build/windows/msix/app_manifest.xml" '(?m)(?<prefix>^\s*Version=")[^"]+(?<suffix>")' ('${prefix}' + $msixVersion + '${suffix}')
Set-ProjectRegex "build/windows/msix/template.xml" '(?m)(?<prefix>^\s*Version=")[^"]+(?<suffix>")' ('${prefix}' + $msixVersion + '${suffix}')

Replace-ProjectLiteral "build/windows/msix/app_manifest.xml" 'Executable="Cursor助手"' 'Executable="my-cursor"'
Replace-ProjectLiteral "build/windows/msix/app_manifest.xml" "Cursor助手" "My Cursor"

Replace-ProjectLiteral "build/windows/msix/template.xml" 'Path="Cursor助手"' 'Path="my-cursor"'
Replace-ProjectLiteral "build/windows/msix/template.xml" 'InstallLocation="C:\Program Files\Cursor助手\Cursor助手"' 'InstallLocation="C:\Program Files\My Cursor\my-cursor"'
Replace-ProjectLiteral "build/windows/msix/template.xml" 'PackageName="Cursor助手"' 'PackageName="my-cursor"'
Replace-ProjectLiteral "build/windows/msix/template.xml" 'ExecutableName="Cursor助手"' 'ExecutableName="my-cursor"'
Replace-ProjectLiteral "build/windows/msix/template.xml" 'PackagePath="Cursor助手.msix"' 'PackagePath="my-cursor.msix"'
Replace-ProjectLiteral "build/windows/msix/template.xml" "Cursor助手" "My Cursor"

Replace-ProjectRegex "frontend/src/App.vue" '(?m)^\s*<AdModelProvider\b[^\r\n]*\r?\n' '' '(?s)^(?!.*<AdModelProvider\b).*'
Replace-ProjectRegex "frontend/src/App.vue" '(?m)^import AdModelProvider[^\r\n]*\r?\n' '' '(?s)^(?!.*import AdModelProvider).*'

Replace-ProjectRegex "frontend/src/views/Home.vue" '(?m)^import \{ getAdRuntime \} from "@/services/clientApi";\r?\n' '' '(?s)^(?!.*import \{ getAdRuntime \}).*'
Replace-ProjectLiteral "frontend/src/views/Home.vue" 'import { Events } from "@wailsio/runtime";' 'import { Browser } from "@wailsio/runtime";'
Replace-ProjectLiteral "frontend/src/views/Home.vue" 'import { computed, onBeforeUnmount, onMounted, ref } from "vue";' 'import { computed } from "vue";'

$legacyHomeAdBlock = @'
const AD_UPDATED_EVENT = "ad:updated";
const OPEN_AD_EVENT = "cursor:open-ad";

const adRuntime = ref(null);
let unsubscribeAdUpdated = null;

function asString(value) {
  if (typeof value === "string") {
    return value.trim();
  }
  if (typeof value === "number" || typeof value === "boolean") {
    return String(value);
  }
  return "";
}

function asBoolean(value) {
  return value === true || value === "true" || value === 1 || value === "1";
}

const homeAds = computed(() => {
  const runtime = adRuntime.value && typeof adRuntime.value === "object" ? adRuntime.value : {};
  const slots = Array.isArray(runtime.slots) && runtime.slots.length > 0 ? runtime.slots : [runtime];
  return slots
    .map((slot, index) => {
      const item = slot && typeof slot === "object" ? slot : {};
      const home = item.home && typeof item.home === "object" ? item.home : {};
      const title = asString(home.title);
      if (
        !title ||
        !asBoolean(item.available) ||
        !asBoolean(item.enabled) ||
        !asString(item.packageHash)
      ) {
        return null;
      }
      return {
        id: asString(item.id) || String(index + 1),
        title,
        subtitle: asString(home.subtitle),
      };
    })
    .filter(Boolean);
});

async function syncAdRuntimeQuietly() {
  try {
    adRuntime.value = await getAdRuntime();
  } catch (_error) {
    adRuntime.value = null;
  }
}

function handleAdUpdated() {
  void syncAdRuntimeQuietly();
}

function handleOpenHomeAd(slotId) {
  window.dispatchEvent(new CustomEvent(OPEN_AD_EVENT, { detail: { slotId: asString(slotId) } }));
}
'@

$homeBrandBlock = @'
const homeAds = [
  {
    id: "ergouzi",
    title: "Ergouzi",
    subtitle: "ergouzi.life",
  },
];

async function handleOpenHomeAd() {
  try {
    await Browser.OpenURL("https://ergouzi.life");
  } catch (error) {
    await showActionError("打开 Ergouzi 失败", error);
  }
}
'@
Replace-ProjectBlock "frontend/src/views/Home.vue" $legacyHomeAdBlock $homeBrandBlock 'const homeAds = ['

$legacyHomeLifecycle = @'

onMounted(() => {
  unsubscribeAdUpdated = Events.On(AD_UPDATED_EVENT, handleAdUpdated);
  void syncAdRuntimeQuietly();
});

onBeforeUnmount(() => {
  if (unsubscribeAdUpdated) {
    unsubscribeAdUpdated();
  }
});
'@
Replace-ProjectBlock "frontend/src/views/Home.vue" $legacyHomeLifecycle '' 'const homeAds = ['

Replace-ProjectLiteral "frontend/src/layouts/MainLayout.vue" 'const usageDocsURL = "https://docs.leokun.cn";' 'const usageDocsURL = "https://github.com/xxloocee/my-cursor#readme";'
Replace-ProjectLiteral "frontend/src/layouts/MainLayout.vue" 'icon-[ant-design--bilibili-outlined]' 'icon-[mdi--web]'

Replace-ProjectLiteral "internal/backend/README.md" '- Pro / `cursor-byok`' '- 旧版 Pro 协议'
Replace-ProjectLiteral "internal/backend/README.md" '- `data/ads/` 是广告包与资源缓存目录' '- `data/ads/` 是兼容资源缓存目录'

Set-ProjectFileFromTemplate "README.md" "scripts/branding/README.template.md"
Set-ProjectFileFromTemplate "release-notes.md" "scripts/branding/release-notes.template.md"
Set-ProjectFileFromTemplate ".github/workflows/release.yml" "scripts/branding/release-workflow.template.yml"

Ensure-ProjectLine ".gitignore" ".catpaw/"
Ensure-ProjectLine ".gitignore" "/internal/certs/*.key"

Assert-ProjectAbsent "internal/ads/types.go" "ads.leokun.cn"
Assert-ProjectPresent "internal/ads/types.go" "var Slots = []Slot{}"
Assert-ProjectAbsent "internal/bridge/window_author.go" "space.bilibili.com/311706663"
Assert-ProjectPresent "internal/bridge/window_author.go" 'const footerAuthorHomeURL = "https://ergouzi.life"'
Assert-ProjectPresent "internal/buildinfo/buildinfo.go" 'ReleaseRepo    = "xxloocee/my-cursor"'
Assert-ProjectPresent "Taskfile.yml" 'RELEASE_BASE_NAME: "my-cursor"'
Assert-ProjectAbsent "Taskfile.yml" "release:sync:readme:"
Assert-ProjectPresent "build/Taskfile.yml" '-name "{{.APP_NAME}}" -binaryname "{{.APP_NAME}}"'
Assert-ProjectCount "build/Taskfile.yml" '-name "{{.APP_NAME}}" -binaryname "{{.APP_NAME}}"' 1
Assert-ProjectPresent "build/windows/Taskfile.yml" 'cp LICENSE "{{.STAGING_DIR}}/LICENSE"'
Assert-ProjectPresent "build/windows/nsis/wails_tools.nsh" '!define INFO_PROJECTNAME "my-cursor"'
Assert-ProjectPresent "build/darwin/Taskfile.yml" 'cp LICENSE "{{.BIN_DIR}}/{{.APP_BUNDLE}}/Contents/Resources/LICENSE"'
Assert-ProjectPresent "build/linux/Taskfile.yml" 'cp LICENSE "{{.STAGING_DIR}}/licenses/LICENSE"'
Assert-ProjectPresent "build/linux/nfpm/nfpm.yaml" 'src: "./build/linux/desktop"'
Assert-ProjectPresent "build/linux/nfpm/nfpm.yaml" 'dst: "/usr/share/licenses/my-cursor/LICENSE"'
Assert-ProjectAbsent "frontend/src/App.vue" "AdModelProvider"
Assert-ProjectAbsent "frontend/src/views/Home.vue" "getAdRuntime"
Assert-ProjectAbsent "frontend/src/views/Home.vue" "AD_UPDATED_EVENT"
Assert-ProjectAbsent "frontend/src/views/Home.vue" "OPEN_AD_EVENT"
Assert-ProjectPresent "frontend/src/views/Home.vue" 'await Browser.OpenURL("https://ergouzi.life")'
Assert-ProjectCount "frontend/src/views/Home.vue" 'await Browser.OpenURL("https://ergouzi.life")' 1
Assert-ProjectAbsent "frontend/src/layouts/MainLayout.vue" "https://docs.leokun.cn"
Assert-ProjectPresent "frontend/src/layouts/MainLayout.vue" 'const usageDocsURL = "https://github.com/xxloocee/my-cursor#readme";'
Assert-ProjectAbsent "release-notes.md" "QQ"
Assert-ProjectPresent "build/config.yml" '  version: "0.1.0"'
Assert-ProjectCount "build/windows/info.json" "0.1.0" 2
Assert-ProjectCount "build/darwin/Info.plist" "0.1.0" 2
Assert-ProjectCount "build/darwin/Info.dev.plist" "0.1.0" 2
Assert-ProjectPresent "build/linux/nfpm/nfpm.yaml" 'version: "0.1.0"'
Assert-ProjectCount "build/windows/msix/app_manifest.xml" "0.1.0.0" 1
Assert-ProjectCount "build/windows/msix/template.xml" "0.1.0.0" 1
Assert-ProjectPresent "release-notes.md" "# My Cursor 0.1.0"
Assert-ProjectPresent ".github/workflows/release.yml" "needs: [windows-amd64, linux-amd64, macos]"
Assert-ProjectPresent ".gitignore" "/internal/certs/*.key"
Assert-ProjectAbsent "internal/certs/ca.go" "go:embed ca.key"
Assert-ProjectPresent "internal/certs/ca.go" "func LegacyCACertPEM() []byte"
Assert-ProjectPresent "internal/certs/local_ca.go" "func LoadOrCreateManager(certPath, keyPath string)"
Assert-ProjectPresent "internal/client/cursor.go" "cursor.RemoveCACertInstalled(s.caCertPEM)"
Assert-ProjectPresent "internal/cursor/windows_cert.go" 'runElevatedCertutil("-delstore", windowsRootStoreName, thumbprint)'
Assert-ProjectPresent "internal/cursor/darwin_cert.go" '"delete-certificate"'
if (Test-ProjectFile "internal/certs/ca.key") {
    throw "Shared CA private key must not exist in the repository"
}

if ($script:changedFiles -eq 0) {
    Write-Host "Ergouzi customization is already applied."
} elseif ($Check) {
    Write-Host "$script:changedFiles customization change(s) would be applied."
    exit 2
} else {
    Write-Host "Ergouzi customization applied with $script:changedFiles change(s)."
}
