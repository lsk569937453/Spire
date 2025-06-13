@echo off
CHCP 65001 >NUL
setlocal enabledelayedexpansion

:: =============== 配置部分 ===============
set DOCKER_IMAGE_NAME=lsk569937453/spire
set DOCKER_IMAGE_VERSION=0.0.22
set RUST_PROJECT_DIR=rust-proxy
set TARGET=x86_64-unknown-linux-gnu
set BINARY_NAME=spire
set DOCKER_DIR=docker\spires
:: =======================================

:: 1. 构建 Rust 项目
echo 🚀 开始构建 Rust 项目...
cross build --target %TARGET% --manifest-path=%RUST_PROJECT_DIR%\Cargo.toml -r -v
if %errorlevel% neq 0 (
    echo ❌ Rust 项目构建失败
    exit /b 1
)

:: 2. 创建目标目录(如果不存在)
if not exist "%DOCKER_DIR%" (
    echo 📂 创建目录 %DOCKER_DIR%...
    mkdir "%DOCKER_DIR%"
)

:: 3. 复制二进制文件
echo 📂 复制二进制文件...
copy "%RUST_PROJECT_DIR%\target\%TARGET%\release\%BINARY_NAME%" "%DOCKER_DIR%\%BINARY_NAME%" > nul
if %errorlevel% neq 0 (
    echo ❌ 文件复制失败
    exit /b 1
)

:: 4. 构建 Docker 镜像
echo 🐳 构建 Docker 镜像...
cd "%DOCKER_DIR%"
docker build -t %DOCKER_IMAGE_NAME%:%DOCKER_IMAGE_VERSION% .
if %errorlevel% neq 0 (
    echo ❌ Docker 镜像构建失败
    exit /b 1
)

:: 5. 推送 Docker 镜像
echo ⬆️ 推送 Docker 镜像到仓库...
docker push %DOCKER_IMAGE_NAME%:%DOCKER_IMAGE_VERSION%
if %errorlevel% neq 0 (
    echo ❌ Docker 镜像推送失败
    exit /b 1
)

echo ✅ 所有操作成功完成!
pause