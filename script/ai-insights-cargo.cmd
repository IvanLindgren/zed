@echo off
rem Runs cargo inside the VS 2022 Build Tools environment.
rem Needed on machines that also have a newer VS (e.g. 18 Insiders) without
rem Spectre-mitigated libs: otherwise the cc crate picks the newest VS.
rem Usage: script\ai-insights-cargo.cmd check -p zed
call "C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat" >nul || exit /b 1
cd /d "%~dp0.."
cargo %*
