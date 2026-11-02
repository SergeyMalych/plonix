# Adds a folder to, or removes it from, the user's Path. Run by the Plonix
# installer (hooks.nsh) to put the plonix command on PATH.
# Usage: user-path.ps1 -Folder <dir> [-Remove]
param([Parameter(Mandatory = $true)][string]$Folder, [switch]$Remove)
$key = Get-Item 'HKCU:\Environment'
# As written, with %VARIABLES% unexpanded, so nothing else in the Path changes.
$path = $key.GetValue('Path', '', [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
$parts = @($path -split ';' | Where-Object { $_ -ne '' -and $_ -ne $Folder })
if (-not $Remove) { $parts += $Folder }
Set-ItemProperty -Path 'HKCU:\Environment' -Name Path -Value ($parts -join ';') -Type ExpandString
# Setting a throwaway variable tells open windows the environment changed.
[Environment]::SetEnvironmentVariable('PLONIX_PATH_CHANGED', '1', 'User')
[Environment]::SetEnvironmentVariable('PLONIX_PATH_CHANGED', $null, 'User')
