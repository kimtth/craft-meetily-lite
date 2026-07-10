@echo off
call "%ProgramFiles(x86)%\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat" >nul
if errorlevel 1 exit /b %errorlevel%

set "LIBCLANG_PATH=%ProgramFiles%\LLVM\bin"
set "PATH=%ProgramFiles%\CMake\bin;%ProgramFiles%\LLVM\bin;%PATH%"
cargo check --quiet --workspace --message-format=json --all-targets --keep-going