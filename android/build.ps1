# Builds MapCast.apk with the Android SDK's own tools (no Gradle): aapt2, javac, d8, zipalign, apksigner.
# Needs the Android SDK (Android Studio installs it) and a JDK (Android Studio's own is used if found).
# Usage: powershell -ExecutionPolicy Bypass -File android\build.ps1
$ErrorActionPreference = 'Stop'
$here = $PSScriptRoot

$sdk = if ($env:ANDROID_HOME) { $env:ANDROID_HOME } else { Join-Path $env:LOCALAPPDATA 'Android\Sdk' }
$buildTools = Get-ChildItem (Join-Path $sdk 'build-tools') | Sort-Object { [version]($_.Name -replace '[^\d.]', '') } -Descending | Select-Object -First 1
$platform = Get-ChildItem (Join-Path $sdk 'platforms') -Directory | Where-Object { Test-Path (Join-Path $_.FullName 'android.jar') } |
    Sort-Object { [version](($_.Name -replace 'android-', '') + '.0') } -Descending | Select-Object -First 1
$androidJar = Join-Path $platform.FullName 'android.jar'
if (-not $env:JAVA_HOME) { $env:JAVA_HOME = 'C:\Program Files\Android\Android Studio\jbr' }
$env:PATH = (Join-Path $env:JAVA_HOME 'bin') + ';' + $env:PATH
$bt = $buildTools.FullName

# the app's version follows the companion's
$version = (Select-String -Path (Join-Path $here '..\companion\Cargo.toml') -Pattern '^version = "(.+)"').Matches[0].Groups[1].Value
$parts = $version.Split('.') | ForEach-Object { [int]$_ }
$code = $parts[0] * 10000 + $parts[1] * 100 + $parts[2]

$out = Join-Path $here 'build'
if (Test-Path $out) { Remove-Item -Recurse -Force $out }
New-Item -ItemType Directory -Force (Join-Path $out 'classes'), (Join-Path $out 'gen'), (Join-Path $out 'dex') | Out-Null

Write-Host "MapCast $version for Android: build-tools $($buildTools.Name), $($platform.Name)"
& (Join-Path $bt 'aapt2.exe') compile --dir (Join-Path $here 'res') -o (Join-Path $out 'res.zip')
if ($LASTEXITCODE) { throw 'aapt2 compile failed' }
& (Join-Path $bt 'aapt2.exe') link -o (Join-Path $out 'base.apk') -I $androidJar --manifest (Join-Path $here 'AndroidManifest.xml') `
    --java (Join-Path $out 'gen') --min-sdk-version 24 --target-sdk-version 35 --version-code $code --version-name $version `
    (Join-Path $out 'res.zip')
if ($LASTEXITCODE) { throw 'aapt2 link failed' }

$sources = @(Get-ChildItem -Recurse -Filter *.java (Join-Path $here 'src')) + @(Get-ChildItem -Recurse -Filter *.java (Join-Path $out 'gen'))
& javac -encoding UTF-8 -source 1.8 -target 1.8 -Xlint:-options -bootclasspath $androidJar -d (Join-Path $out 'classes') ($sources | ForEach-Object { $_.FullName })
if ($LASTEXITCODE) { throw 'javac failed' }
$classes = Get-ChildItem -Recurse -Filter *.class (Join-Path $out 'classes') | ForEach-Object { $_.FullName }
& (Join-Path $bt 'd8.bat') --release --min-api 24 --lib $androidJar --output (Join-Path $out 'dex') $classes
if ($LASTEXITCODE) { throw 'd8 failed' }

# classes.dex into the APK, then align and sign (the developer's debug key, so updates install over)
Copy-Item (Join-Path $out 'base.apk') (Join-Path $out 'unsigned.apk')
Push-Location (Join-Path $out 'dex')
& jar uf (Join-Path $out 'unsigned.apk') classes.dex
$jarExit = $LASTEXITCODE
Pop-Location
if ($jarExit) { throw 'adding classes.dex failed' }
& (Join-Path $bt 'zipalign.exe') -p -f 4 (Join-Path $out 'unsigned.apk') (Join-Path $out 'aligned.apk')
if ($LASTEXITCODE) { throw 'zipalign failed' }

$keystore = if ($env:MAPCAST_KEYSTORE) { $env:MAPCAST_KEYSTORE } else { Join-Path $env:USERPROFILE '.android\debug.keystore' }
if (-not (Test-Path $keystore)) {
    & keytool -genkeypair -keystore $keystore -storepass android -keypass android -alias androiddebugkey `
        -keyalg RSA -keysize 2048 -validity 10000 -dname 'CN=Android Debug,O=Android,C=US'
}
$apk = Join-Path $out 'MapCast.apk'
& (Join-Path $bt 'apksigner.bat') sign --ks $keystore --ks-pass pass:android --key-pass pass:android --out $apk (Join-Path $out 'aligned.apk')
if ($LASTEXITCODE) { throw 'apksigner failed' }
& (Join-Path $bt 'apksigner.bat') verify $apk
if ($LASTEXITCODE) { throw 'the signed APK does not verify' }
Write-Host "Built $apk ($([math]::Round((Get-Item $apk).Length / 1KB)) KB)"
