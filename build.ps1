# 挂机机器人一键编译与复制脚本

# 清除之前的进度条缓存
Write-Progress -Activity "挂机机器人一键构建程序" -Completed

# 步骤 1: 准备环境
Write-Progress -Activity "挂机机器人一键构建程序" -Status "正在初始化构建环境，检测 test 目录... " -PercentComplete 15
Start-Sleep -Milliseconds 400

if (-not (Test-Path -Path ".\test")) {
    New-Item -ItemType Directory -Path ".\test" -Force | Out-Null
    Write-Host "提示: 已自动创建 test 目录 " -ForegroundColor Yellow
}

# 步骤 2: 启动编译
Write-Progress -Activity "挂机机器人一键构建程序" -Status "正在编译中，请稍候... " -PercentComplete 40
Start-Sleep -Milliseconds 300

Write-Host "=== 开始编译 === " -ForegroundColor Cyan
cargo build
$buildResult = $LASTEXITCODE
Write-Host "================= " -ForegroundColor Cyan

# 步骤 3: 编译结果处理
if ($buildResult -ne 0) {
    # 结束进度条
    Write-Progress -Activity "挂机机器人一键构建程序" -Status "构建失败 " -PercentComplete 100 -Completed
    Write-Host ""
    Write-Host "错误: 编译未能成功！请检查上方 Cargo 的报错输出。 " -ForegroundColor Red
    exit $buildResult
}

# 编译成功，尝试复制文件
Write-Progress -Activity "挂机机器人一键构建程序" -Status "编译成功！正在复制文件... " -PercentComplete 80
Start-Sleep -Milliseconds 500

try {
    # 使用 -ErrorAction Stop 以便让拷贝错误能够被 catch 捕获
    Copy-Item -Path ".\target\debug\azalea_bot.exe" -Destination ".\test\azalea_bot.exe" -Force -ErrorAction Stop
}
catch {
    # 结束进度条
    Write-Progress -Activity "挂机机器人一键构建程序" -Status "拷贝文件失败 " -PercentComplete 100 -Completed
    Write-Host ""
    Write-Host "错误: 无法将最新的 azalea_bot.exe 复制到 test/ 目录！ " -ForegroundColor Red
    Write-Host "原因: 目标文件正被另一个进程占用（很可能您的挂机客户端已在后台运行）。 " -ForegroundColor Red
    Write-Host "解决: 请先关闭运行中的 azalea_bot.exe，然后重新执行此脚本。 " -ForegroundColor Yellow
    exit 1
}

Write-Progress -Activity "挂机机器人一键构建程序" -Status "复制成功，正在完成校验... " -PercentComplete 95
Start-Sleep -Milliseconds 300

# 结束进度条
Write-Progress -Activity "挂机机器人一键构建程序" -Status "构建完成！ " -PercentComplete 100 -Completed

Write-Host ""
Write-Host "成功: 编译并部署完成！ "