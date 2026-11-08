@echo off
rem Release build of the ai-insights fork.
rem - "preview" channel: own instance lock, so it runs next to the installed
rem   stable Zed instead of handing files over to it; settings dir is shared.
rem - ZED_UPDATE_EXPLANATION disables auto-update (it would replace this build
rem   with the official Preview). Update by pulling and rebuilding instead.
set "ZED_RELEASE_CHANNEL=preview"
set "ZED_UPDATE_EXPLANATION=Custom ai-insights build: update it by rebuilding from source."
rem Prebuilt WebRTC fetched once with curl (the build script's own download can time out).
if exist "%~dp0..\..\webrtc-prebuilt\win-x64-release\webrtc.ninja" set "LK_CUSTOM_WEBRTC=%~dp0..\..\webrtc-prebuilt\win-x64-release"
call "%~dp0ai-insights-cargo.cmd" build --release -p zed %*
