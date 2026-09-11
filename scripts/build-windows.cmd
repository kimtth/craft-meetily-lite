@echo off
rem Keep vcvars and command chaining inside cmd, not PowerShell task quoting.
call "%ProgramFiles(x86)%\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat"
if errorlevel 1 exit /b %errorlevel%

cd /d "%~dp0.."
cargo test --manifest-path src-tauri/Cargo.toml --lib
if errorlevel 1 exit /b %errorlevel%

call npm run tauri:build
exit /b %errorlevel%