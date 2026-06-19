# Azalea 挂机机器人 (Docker 部署指南)

本项目支持通过云端自动打包的轻量级 Distroless 容器进行部署。由于微软账户正版登录需要进行一次性的网页授权，我们推荐以下**“先注册生成密钥 (Key) 与配置，再用 Docker Compose 后台托管”**的步骤进行部署。

---

## 🚀 部署四步走

### 第一步：交互式登录，生成玩家别名授权文件 (`.key`)
因为微软 OAuth 登录需要你在浏览器中输入设备代码完成授权，所以**必须在交互模式 (`-it`) 下运行容器**完成首次登录。此步骤**不需要**指定任何服务器 IP 与端口。

运行以下命令（请根据你的实际情况替换参数）：
```bash
docker run -it --rm -v ./data:/app ghcr.io/sayayai/azalea_bot:latest <你的微软邮箱> <玩家别名>
```
**示例：**
```bash
docker run -it --rm -v ./data:/app ghcr.io/sayayai/azalea_bot:latest player1@outlook.com bot1
```

**登录操作步骤：**
1. 终端会输出类似如下的提示：
   ```text
   To sign in, use a web browser to open the page https://microsoft.com/devicelogin and enter the code XXXXXXXXX to authenticate.
   ```
2. 打开浏览器访问 `https://microsoft.com/devicelogin`，输入终端中显示的验证码，并登录你的微软正版账号。
3. 网页端提示授权成功后，终端会提示成功并**自动退出容器**，同时在你的主机 `./data` 目录下生成名为 `bot1.key` 的加密授权文件。
4. *(如果有多个账号，可以重复该步骤，为不同的别名如 `bot2`、`bot3` 生成对应的 `.key` 授权文件)*。

---

### 第二步：生成默认配置文件 `config.yml`
授权密钥生成后，我们需要初始化一份配置文件模板。直接运行容器（不带任何参数）：

```bash
docker run --rm -v ./data:/app ghcr.io/sayayai/azalea_bot:latest
```
运行后，程序会检测到没有配置文件，并自动在主机的 `./data` 目录下创建 `config.yml` 默认模板，随后退出。

---

### 第三步：修改 `config.yml` 配置文件
打开主机的 `./data/config.yml`，根据你刚才生成的玩家别名进行配置。

**`config.yml` 配置示例：**
```yaml
# 挂机机器人自动配置文件
# 直接运行 azalea_bot 时，将读取此配置自动多开并连接挂机。
# 注意: 每个玩家别名(例如 bot1)必须首先通过命令行登录一次，生成授权 key 文件。

localhost:25565:          # 连接的服务器地址
  bot1:                  # 别名1 (必须已存在 bot1.key)
    look: -23.4 10.8     # 视角偏角，格式为 "yaw pitch"，不配置则不锁定视角
    use: 10              # 右键使用周期（单位：tick，每 10 ticks 触发一次）
    atk: 6               # 左键攻击周期（单位：tick，每 6 ticks 触发一次）
  bot2:                  # 别名2 (必须已存在 bot2.key)
    atk:                 # 留空代表持续连点/挥臂挂机模式
```

---

### 第四步：使用 Docker Compose 后台常驻运行
在项目目录下创建 `docker-compose.yml` 配置文件：

```yaml
version: '3.8'

services:
  azalea_bot:
    image: ghcr.io/sayayai/azalea_bot:latest
    container_name: azalea_bot
    restart: always
    volumes:
      - ./data:/app
    # 保持标准输入打开和分配伪终端，防止程序因为 stdin 关闭而退出
    tty: true
    stdin_open: true
```

启动容器：
```bash
docker compose up -d
```

查看挂机运行状态和聊天输出：
```bash
docker compose logs -f
```

停止挂机：
```bash
docker compose down
```

---

## 💡 提示与注意事项
1. **容器挂载点**：容器内部的工作目录为 `/app`，因此必须将主机的 `./data`（包含 `.key` 授权文件和 `config.yml`）挂载到容器的 `/app`。
2. **多开限制**：你可以配置任意多个别名和多台服务器，只要 `./data` 下存在对应的 `<别名>.key` 文件即可。
3. **命令行交互**：若需要在容器后台运行时向游戏内发送指令，可通过 `docker attach azalea_bot` 附加到容器输入聊天信息，使用 `Ctrl + P, Ctrl + Q` 安全退出附加，切勿使用 `Ctrl + C`（会导致容器停止）。
