#requires -Version 5.1
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][ValidateNotNullOrEmpty()][string]$BuildId,
    [string]$OutputPath = '',
    [switch]$Force
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
if ($BuildId.Length -gt 58) { throw 'BuildId must contain 1..58 printable ASCII characters' }
foreach ($character in $BuildId.ToCharArray()) {
    if ([int]$character -lt 32 -or [int]$character -gt 126) {
        throw 'BuildId must contain only printable ASCII characters (0x20..0x7E)'
    }
}
[byte[]]$idBytes = [Text.Encoding]::ASCII.GetBytes($BuildId)
[byte[]]$bytes = @([byte]0x4A, [byte]0x4C, [byte]0x49, [byte]0x44, [byte]1, [byte]$idBytes.Length) + $idBytes
$initializer = (@($bytes | ForEach-Object { '0x{0:X2}' -f $_ }) -join ', ')
# Do not insert unescaped user text into a C comment or literal.
$source = @"
/* Generated JLID v1. Use a fresh build ID when firmware changes.
 * Keep this symbol in a loaded, read-only Flash section of the final ELF.
 * GCC/Clang linker scripts using section GC must KEEP(.jlink_mcp_identity).
 */
#include <stdint.h>
#if defined(__ICCARM__)
#define JLINK_IDENTITY_KEEP __root
#elif defined(__GNUC__) || defined(__clang__)
#define JLINK_IDENTITY_KEEP __attribute__((used, section(".jlink_mcp_identity")))
#else
#error Define the compiler-specific retention rule for __jlink_mcp_identity
#endif
JLINK_IDENTITY_KEEP const uint8_t __jlink_mcp_identity[] = {
    $initializer
};
#undef JLINK_IDENTITY_KEEP
"@
if (-not $OutputPath) { $source; return }
$full = [IO.Path]::GetFullPath($OutputPath)
$mode = if ($Force) { [IO.FileMode]::Create } else { [IO.FileMode]::CreateNew }
$stream = [IO.File]::Open($full, $mode, [IO.FileAccess]::Write, [IO.FileShare]::None)
try {
    $encoded = [Text.Encoding]::ASCII.GetBytes($source + [Environment]::NewLine)
    $stream.Write($encoded, 0, $encoded.Length)
    $stream.Flush($true)
}
finally { $stream.Dispose() }
[pscustomobject]@{ path = $full; identity_bytes = $bytes.Length; build_id_length = $idBytes.Length }
