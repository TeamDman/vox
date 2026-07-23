$ErrorActionPreference = "Stop"
$javaRoot = $PSScriptRoot
$repoRoot = Split-Path -Parent (Split-Path -Parent $javaRoot)
$output = Join-Path $javaRoot "target/test-classes"
$sources = @(
    Get-ChildItem -Recurse (Join-Path $javaRoot "runtime/src/main/java") -Filter *.java
    Get-ChildItem -Recurse (Join-Path $javaRoot "runtime/src/test/java") -Filter *.java
) | ForEach-Object FullName

New-Item -ItemType Directory -Force $output | Out-Null
& javac --release 17 -Xlint:all -Werror -d $output $sources
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
& java -ea -cp $output org.facet.phon.PhonConformanceTest $repoRoot
exit $LASTEXITCODE
