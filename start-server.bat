@echo off
chcp 65001 >nul
cd /d "%~dp0"
title DroidBridge 服务
rem 无界面运行（供浏览器访问或被其他电脑添加）。需要令牌时：droidbridge-server.exe --token 你的令牌
droidbridge-server.exe %*
pause
