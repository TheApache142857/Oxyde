@echo off
rem Starts Oxyde's MCP server for Claude Code (register this file, not the executable).
rem
rem It runs a copy of target\debug\oxyde-mcp.exe kept in %LOCALAPPDATA%\Oxyde\mcp, refreshed
rem whenever the build is newer: a server started straight from target\ keeps the executable
rem open, and Windows then refuses to let cargo replace it, which breaks `cargo test`.
rem Nothing may be written to standard output but the server's own messages.
setlocal
set "SOURCE=%~dp0..\target\debug\oxyde-mcp.exe"
set "COPY=%LOCALAPPDATA%\Oxyde\mcp"
if not exist "%COPY%" mkdir "%COPY%" >nul 2>&1
rem A copy still in use by another session cannot be replaced; that one is run as it is.
if exist "%SOURCE%" xcopy /D /Y /Q "%SOURCE%" "%COPY%\" >nul 2>&1
if not exist "%COPY%\oxyde-mcp.exe" (
    echo oxyde-mcp.exe is not built yet: run cargo build -p oxyde-mcp 1>&2
    exit /b 1
)
rem The OpenCASCADE DLLs, when they are not already on PATH (docs/BUILDING.md).
if defined OXYDE_OCCT_DIR set "PATH=%PATH%;%OXYDE_OCCT_DIR%\bin"
"%COPY%\oxyde-mcp.exe" %*
