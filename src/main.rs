use std::sync::Arc;
use std::path::Path;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::collections::{HashMap, VecDeque};
use azalea::prelude::*;
use azalea::interact::SwingArmEvent;
use parking_lot::Mutex;
use parking_lot::RwLock;
use tokio::io::{AsyncBufReadExt, BufReader};
use serde::{Deserialize, Serialize};
use axum::{
    routing::{get, post},
    Json, Router, extract::State as AxumState, response::Html,
};

#[derive(Default, Clone, Component)]
pub struct BotState {
    pub look_direction: Arc<Mutex<Option<(f32, f32)>>>, // Option<(y_rot, x_rot)>
    pub use_interval: Arc<Mutex<Option<usize>>>, // None = off, Some(0) = every tick (hold), Some(n) = every n ticks
    pub atk_interval: Arc<Mutex<Option<usize>>>, // None = off, Some(0) = every tick (hold), Some(n) = every n ticks
    pub use_counter: Arc<Mutex<usize>>,
    pub atk_counter: Arc<Mutex<usize>>,
    pub global_state: Option<Arc<RwLock<GlobalAppState>>>,
    pub cmd_tx: Option<tokio::sync::mpsc::UnboundedSender<BotCommand>>,
    pub alias: Option<String>,
    pub server: Option<String>,
}

// YAML 配置文件结构
#[derive(Debug, Deserialize, Serialize, Clone, Default)]
pub struct BotConfig {
    pub look: Option<String>,
    #[serde(rename = "use", default)]
    pub use_mode: Option<serde_yaml::Value>,
    #[serde(rename = "atk", default)]
    pub atk_mode: Option<serde_yaml::Value>,
}

pub type AppConfig = std::collections::HashMap<String, std::collections::HashMap<String, BotConfig>>;

#[derive(Clone, Serialize)]
pub struct WebBotStatus {
    pub online: bool,
    pub connecting: bool,
    pub delay: u64,
    pub pos: String,
}

#[derive(Clone, Serialize)]
pub struct LogMessage {
    pub time: String,
    pub alias: String,
    pub message: String,
    pub is_error: bool,
    pub is_system: bool,
}

pub struct GlobalAppState {
    pub configs: HashMap<String, BotConfig>,
    pub status: HashMap<String, WebBotStatus>,
    pub logs: VecDeque<LogMessage>,
}

#[derive(Clone)]
struct WebContext {
    _state: Arc<RwLock<GlobalAppState>>,
    cmd_tx: tokio::sync::mpsc::UnboundedSender<BotCommand>,
}

pub enum BotCommand {
    Start {
        server: String,
        alias: String,
    },
    Stop {
        alias: String,
    },
    SendChat {
        alias: String,
        message: String,
    },
    RegisterActiveClient {
        alias: String,
        client: Client,
    },
    RemoveActiveClient {
        alias: String,
    }
}

fn parse_mode(val_opt: &Option<serde_yaml::Value>) -> Option<usize> {
    match val_opt {
        None => None, // 缺失字段，表示禁用
        Some(val) => {
            if val.is_null() {
                Some(0) // 字段存在但为空，表示持续使用/长按
            } else if let Some(n) = val.as_u64() {
                Some(n as usize)
            } else {
                Some(0) // 其他非法值也默认长按/持续
            }
        }
    }
}

// 辅助反序列化提取缓存邮箱
#[derive(Deserialize)]
struct MiniCachedAccount {
    #[serde(alias = "email")]
    cache_key: String,
}

// 获取文件创建时间（秒）
async fn get_file_created_secs(cache_file: &Path) -> Option<u64> {
    if cache_file.exists() {
        if let Ok(metadata) = tokio::fs::metadata(cache_file).await {
            if let Ok(created) = metadata.created() {
                if let Ok(duration) = created.duration_since(std::time::UNIX_EPOCH) {
                    return Some(duration.as_secs());
                }
            }
        }
    }
    None
}

fn decrypt_bytes(encrypted_bytes: &[u8], key_bytes: &[u8]) -> Vec<u8> {
    let mut decrypted_bytes = encrypted_bytes.to_vec();
    if !key_bytes.is_empty() {
        for (i, byte) in decrypted_bytes.iter_mut().enumerate() {
            *byte ^= key_bytes[i % key_bytes.len()];
        }
    }
    decrypted_bytes
}

fn decrypt_and_parse_cache<T>(
    contents: &[u8],
    created_secs: Option<u64>,
) -> Option<T>
where
    T: serde::de::DeserializeOwned,
{
    // 1. 尝试直接作为明文 JSON 反序列化
    if let Ok(data) = serde_json::from_slice::<T>(contents) {
        return Some(data);
    }

    // 2. 尝试使用固定的混淆密钥解密
    let fixed_key = b"azalea-auth-cache-obfuscation-key";
    let decrypted = decrypt_bytes(contents, fixed_key);
    if let Ok(data) = serde_json::from_slice::<T>(&decrypted) {
        return Some(data);
    }

    // 3. 尝试使用当前文件的创建时间解密
    if let Some(secs) = created_secs {
        let key_str = secs.to_string();
        let decrypted = decrypt_bytes(contents, key_str.as_bytes());
        if let Ok(data) = serde_json::from_slice::<T>(&decrypted) {
            return Some(data);
        }
    }

    // 4. 尝试使用 "0"（对应创建时间获取失败的情况）解密
    let decrypted = decrypt_bytes(contents, b"0");
    if let Ok(data) = serde_json::from_slice::<T>(&decrypted) {
        return Some(data);
    }

    // 5. 尝试通过已知明文前缀模板，针对 10 位数字时间戳进行恢复
    if contents.len() >= 10 {
        // 模板 A: pretty print 格式 (即 `[\n  {\n    ` -> `[91, 10, 32, 32, 123, 10, 32, 32, 32, 32]`)
        let template_pretty = [91, 10, 32, 32, 123, 10, 32, 32, 32, 32];
        let mut possible_key = [0u8; 10];
        for i in 0..10 {
            possible_key[i] = contents[i] ^ template_pretty[i];
        }
        if possible_key.iter().all(|&b| b.is_ascii_digit()) {
            let decrypted = decrypt_bytes(contents, &possible_key);
            if let Ok(data) = serde_json::from_slice::<T>(&decrypted) {
                return Some(data);
            }
        }

        // 模板 B: compact 格式 (即 `[{"cache_ke` -> `[91, 123, 34, 99, 97, 99, 104, 101, 95, 107]`)
        let template_compact = [91, 123, 34, 99, 97, 99, 104, 101, 95, 107];
        let mut possible_key = [0u8; 10];
        for i in 0..10 {
            possible_key[i] = contents[i] ^ template_compact[i];
        }
        if possible_key.iter().all(|&b| b.is_ascii_digit()) {
            let decrypted = decrypt_bytes(contents, &possible_key);
            if let Ok(data) = serde_json::from_slice::<T>(&decrypted) {
                return Some(data);
            }
        }
    }

    None
}

fn strip_ansi_codes(input: &str) -> String {
    let mut result = String::new();
    let mut in_code = false;
    for c in input.chars() {
        if c == '\x1b' {
            in_code = true;
        } else if in_code {
            if c.is_ascii_alphabetic() {
                in_code = false;
            }
        } else {
            result.push(c);
        }
    }
    result
}

fn add_log(state: &Arc<RwLock<GlobalAppState>>, alias: &str, msg: &str, is_error: bool, is_system: bool) {
    let now = chrono::Local::now().format("%H:%M:%S").to_string();
    let mut lock = state.write();
    lock.logs.push_back(LogMessage {
        time: now,
        alias: alias.to_string(),
        message: msg.to_string(),
        is_error,
        is_system,
    });
    if lock.logs.len() > 1000 {
        lock.logs.pop_front();
    }
}

// 全局活跃机器人管理系统，防 stdin 并发竞争
static ACTIVE_BOTS: OnceLock<Mutex<Vec<Client>>> = OnceLock::new();
static WELCOME_PRINTED: AtomicBool = AtomicBool::new(false);

fn add_active_bot(bot: Client) {
    let lock = ACTIVE_BOTS.get_or_init(|| Mutex::new(Vec::new()));
    let mut bots = lock.lock();
    if !bots.iter().any(|b| b.uuid() == bot.uuid()) {
        bots.push(bot);
    }
}

fn remove_active_bot(uuid: &uuid::Uuid) {
    if let Some(lock) = ACTIVE_BOTS.get() {
        let mut bots = lock.lock();
        bots.retain(|b| b.uuid() != *uuid);
    }
}

async fn handle_web_bot(bot: Client, event: Event, state: BotState) -> eyre::Result<()> {
    let bot_key = format!("{}|{}", state.server.as_deref().unwrap_or(""), state.alias.as_deref().unwrap_or(""));
    let alias = state.alias.clone().unwrap_or_default();
    
    match event {
        Event::Spawn => {
            let username = bot.username();
            if let Some(ref g_state) = state.global_state {
                add_log(g_state, &alias, &format!("机器人 {} 已进入游戏世界！", username), false, true);
            }
            
            if let Some(ref cmd_tx) = state.cmd_tx {
                let _ = cmd_tx.send(BotCommand::RegisterActiveClient {
                    alias: alias.clone(),
                    client: bot.clone(),
                });
            }

            if let Some(ref g_state) = state.global_state {
                let mut lock = g_state.write();
                if let Some(status) = lock.status.get_mut(&bot_key) {
                    status.online = true;
                    status.connecting = false;
                }
            }
        }
        Event::Chat(m) => {
            let clean_msg = strip_ansi_codes(&m.message().to_ansi());
            if let Some(ref g_state) = state.global_state {
                add_log(g_state, &alias, &clean_msg, false, false);
            }
        }
        Event::Tick => {
            if let Some((y_rot, x_rot)) = *state.look_direction.lock() {
                let _ = bot.query_self::<&mut azalea::entity::LookDirection, _>(|mut look| {
                    look.update(azalea::entity::LookDirection::new(y_rot, x_rot));
                });
            }

            let mut use_counter = state.use_counter.lock();
            let use_interval = *state.use_interval.lock();
            if let Some(interval) = use_interval {
                if interval == 0 {
                    bot.start_use_item();
                } else {
                    *use_counter += 1;
                    if *use_counter >= interval {
                        bot.start_use_item();
                        *use_counter = 0;
                    }
                }
            }

            let mut atk_counter = state.atk_counter.lock();
            let atk_interval = *state.atk_interval.lock();
            if let Some(interval) = atk_interval {
                if interval == 0 {
                    do_attack(&bot);
                } else {
                    *atk_counter += 1;
                    if *atk_counter >= interval {
                        do_attack(&bot);
                        *atk_counter = 0;
                    }
                }
            }

            if bot.ticks_connected() % 10 == 0 {
                let pos_str = bot.query_self::<&azalea::entity::Position, _>(|p| format!("{:.1}, {:.1}, {:.1}", p.x, p.y, p.z)).unwrap_or_else(|_| "--".to_string());
                if let Some(ref g_state) = state.global_state {
                    let mut lock = g_state.write();
                    if let Some(status) = lock.status.get_mut(&bot_key) {
                        status.pos = pos_str;
                        status.delay = bot.ticks_connected();
                    }
                }
            }
        }
        Event::Disconnect(reason) => {
            if let Some(ref g_state) = state.global_state {
                add_log(g_state, &alias, &format!("已断开连接: {:?}", reason), true, true);
            }
            if let Some(ref cmd_tx) = state.cmd_tx {
                let _ = cmd_tx.send(BotCommand::RemoveActiveClient { alias: alias.clone() });
            }
            if let Some(ref g_state) = state.global_state {
                let mut lock = g_state.write();
                if let Some(status) = lock.status.get_mut(&bot_key) {
                    status.online = false;
                    status.connecting = false;
                }
            }
        }
        _ => {}
    }
    Ok(())
}

#[tokio::main]
async fn main() -> AppExit {
    // 屏蔽 azalea 底层和 Bevy 的所有 warn / error 日志输出，保持控制台极简干净
    unsafe {
        std::env::set_var("RUST_LOG", "off");
    }

    let mut exe_dir = std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("."));
    exe_dir.pop();

    let args: Vec<String> = std::env::args().collect();

    // 启动全局 Stdin 任务，统一处理控制台输入并广播给所有已连接的机器人
    tokio::spawn(async move {
        let mut reader = BufReader::new(tokio::io::stdin()).lines();
        while let Ok(Some(line)) = reader.next_line().await {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if let Some(lock) = ACTIVE_BOTS.get() {
                let bots = lock.lock();
                for bot in bots.iter() {
                    bot.chat(line);
                }
            }
        }
    });

    // 1. 直接运行 (不带参数)：读取或创建 config.yml，并拉起网页服务器与多开任务
    if args.len() == 1 {
        let config_path = exe_dir.join("config.yml");
        if !config_path.exists() {
            // 自动生成默认 config.yml 模板文件
            let mut servers = std::collections::HashMap::new();
            let mut bots = std::collections::HashMap::new();
            bots.insert("bot1".to_string(), BotConfig {
                look: Some("-23.4 10.8".to_string()),
                use_mode: Some(serde_yaml::Value::Number(10.into())),
                atk_mode: Some(serde_yaml::Value::Number(6.into())),
            });
            bots.insert("bot2".to_string(), BotConfig {
                look: None,
                use_mode: None,
                atk_mode: Some(serde_yaml::Value::Null), // 留空代表持续连点
            });
            servers.insert("localhost:25565".to_string(), bots);

            let yaml_str = serde_yaml::to_string(&servers).unwrap();
            let yaml_str = yaml_str.replace("null", "");
            let commented_yaml = format!(
                "# 挂机机器人自动配置文件\n\
                 # 直接运行 azalea_bot.exe 不加任何参数时，将读取此配置自动连接挂机。\n\
                 # 注意: 每个玩家别名(例如 bot1)必须首先通过命令行登录一次，在同级目录下生成授权 key 文件。\n\
                 # 示例登录命令: azalea_bot.exe localhost:25565 your_email@outlook.com bot1\n\n\
                 # 配置说明:\n\
                 # 1. look: 视角朝向偏角，格式为 \"yaw pitch\"（例如 \"-23.4 10.8\"），如果不配置则不锁定视角。\n\
                 # 2. use / atk: 右键使用 / 左键攻击。如果不配置该字段，则不开启；\n\
                 #    如果配置但留空（例如 `atk:`），代表持续长按使用或长按连点；\n\
                 #    如果配置了具体数字（例如 `use: 10`），代表每隔指定 tick 触发一次。\n\n\
                 {}",
                yaml_str
            );
            std::fs::write(&config_path, commented_yaml).expect("写入默认 config.yml 失败");
            println!("未找到配置文件。已自动为您在同级目录下创建了 config.yml 默认模板，请修改配置后再运行！");
            std::process::exit(0);
        }

        // 解析并读取 config.yml
        let file_content = std::fs::read_to_string(&config_path).expect("读取 config.yml 失败");
        let config: AppConfig = match serde_yaml::from_str(&file_content) {
            Ok(cfg) => cfg,
            Err(e) => {
                eprintln!("[错误] 解析 config.yml 配置文件出错: {:?}", e);
                std::process::exit(1);
            }
        };

        println!("已载入配置文件，正在初始化挂机控制中心 (Web UI)...");

        // 初始化全局状态
        let mut configs_map = HashMap::new();
        let mut status_map = HashMap::new();

        for (server_address, bots) in &config {
            for (alias, bot_config) in bots {
                let key = format!("{}|{}", server_address, alias);
                configs_map.insert(key.clone(), bot_config.clone());
                status_map.insert(key, WebBotStatus {
                    online: false,
                    connecting: false,
                    delay: 0,
                    pos: "--".to_string(),
                });
            }
        }

        let global_state = Arc::new(RwLock::new(GlobalAppState {
            configs: configs_map,
            status: status_map,
            logs: VecDeque::new(),
        }));

        let (cmd_tx, mut cmd_rx) = tokio::sync::mpsc::unbounded_channel::<BotCommand>();

        // 启动独立的单线程后台 OS 线程运行 LocalSet 处理机器人事件
        let local_state = global_state.clone();
        let local_exe_dir = exe_dir.clone();
        let local_cmd_tx = cmd_tx.clone();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();

            let local = tokio::task::LocalSet::new();
            local.block_on(&rt, async move {
                let mut running_bots: HashMap<String, Client> = HashMap::new();

                while let Some(cmd) = cmd_rx.recv().await {
                    match cmd {
                        BotCommand::Start { server, alias } => {
                            let key = format!("{}|{}", server, alias);
                            if running_bots.contains_key(&alias) {
                                continue;
                            }

                            {
                                let mut lock = local_state.write();
                                if let Some(status) = lock.status.get_mut(&key) {
                                    status.connecting = true;
                                }
                            }

                            let state_for_bot = local_state.clone();
                            let exe_dir_clone = local_exe_dir.clone();
                            let cmd_tx_clone = local_cmd_tx.clone();
                            let alias_clone = alias.clone();
                            let server_clone = server.clone();

                            let (look_target, use_interval, atk_interval) = {
                                let lock = local_state.read();
                                let bot_cfg = lock.configs.get(&key).cloned().unwrap_or_default();
                                let look = bot_cfg.look.as_ref().and_then(|s| {
                                    let parts: Vec<&str> = s.split_whitespace().collect();
                                    if parts.len() == 2 {
                                        if let (Ok(y), Ok(p)) = (parts[0].parse::<f32>(), parts[1].parse::<f32>()) {
                                            return Some((y, p));
                                        }
                                    }
                                    None
                                });
                                (look, parse_mode(&bot_cfg.use_mode), parse_mode(&bot_cfg.atk_mode))
                            };

                            tokio::task::spawn_local(async move {
                                let cache_file_path = exe_dir_clone.join(format!("{}.key", alias_clone));
                                if !cache_file_path.exists() {
                                    let err_msg = format!("找不到别名 '{}' 对应的授权文件 '{}.key'，无法连接 {}！", alias_clone, alias_clone, server_clone);
                                    add_log(&state_for_bot, &alias_clone, &err_msg, true, true);
                                    let mut lock = state_for_bot.write();
                                    if let Some(status) = lock.status.get_mut(&key) {
                                        status.connecting = false;
                                    }
                                    return;
                                }

                                let created_secs = get_file_created_secs(&cache_file_path).await;

                                let encrypted_bytes = match std::fs::read(&cache_file_path) {
                                    Ok(b) => b,
                                    Err(e) => {
                                        let err_msg = format!("读取授权文件 {}.key 失败: {:?}", alias_clone, e);
                                        add_log(&state_for_bot, &alias_clone, &err_msg, true, true);
                                        return;
                                    }
                                };

                                let mini_cache: Vec<MiniCachedAccount> = match decrypt_and_parse_cache(&encrypted_bytes, created_secs) {
                                    Some(c) => c,
                                    None => {
                                        let err_msg = format!("别名 '{}' 的授权文件 {}.key 解密失败！创建时间可能已被系统修改。", alias_clone, alias_clone);
                                        add_log(&state_for_bot, &alias_clone, &err_msg, true, true);
                                        return;
                                    }
                                };

                                if mini_cache.is_empty() {
                                    let err_msg = format!("授权文件 {}.key 中未找到已缓存的账号！", alias_clone);
                                    add_log(&state_for_bot, &alias_clone, &err_msg, true, true);
                                    return;
                                }

                                let email = mini_cache[0].cache_key.clone();
                                let mut auth_opts = azalea::account::microsoft::MicrosoftAccountOpts::default();
                                auth_opts.cache_file = Some(cache_file_path);

                                add_log(&state_for_bot, &alias_clone, &format!("正在登录账号: {}", email), false, true);

                                let account = match Account::microsoft_with_opts(&email, auth_opts).await {
                                    Ok(acc) => acc,
                                    Err(e) => {
                                        let err_msg = format!("登录失败: {:?}", e);
                                        add_log(&state_for_bot, &alias_clone, &err_msg, true, true);
                                        let mut lock = state_for_bot.write();
                                        if let Some(status) = lock.status.get_mut(&key) {
                                            status.connecting = false;
                                        }
                                        return;
                                    }
                                };

                                let initial_state = BotState {
                                    look_direction: Arc::new(Mutex::new(look_target)),
                                    use_interval: Arc::new(Mutex::new(use_interval)),
                                    atk_interval: Arc::new(Mutex::new(atk_interval)),
                                    global_state: Some(state_for_bot.clone()),
                                    cmd_tx: Some(cmd_tx_clone.clone()),
                                    alias: Some(alias_clone.clone()),
                                    server: Some(server_clone.clone()),
                                    ..Default::default()
                                };

                                let _ = ClientBuilder::new()
                                    .set_handler(handle_web_bot)
                                    .set_state(initial_state)
                                    .start(account, server_clone.as_str())
                                    .await;
                            });
                        }
                        BotCommand::Stop { alias } => {
                            if let Some(client) = running_bots.remove(&alias) {
                                add_log(&local_state, &alias, "正在主动断开连接...", false, true);
                                client.disconnect();
                            }
                        }
                        BotCommand::SendChat { alias, message } => {
                            if let Some(client) = running_bots.get(&alias) {
                                client.chat(&message);
                            }
                        }
                        BotCommand::RegisterActiveClient { alias, client } => {
                            running_bots.insert(alias, client);
                        }
                        BotCommand::RemoveActiveClient { alias } => {
                            running_bots.remove(&alias);
                        }
                    }
                }
            });
        });

        // 绑定 Axum API 服务
        let web_ctx = WebContext {
            _state: global_state.clone(),
            cmd_tx,
        };

        async fn handle_index() -> Html<&'static str> {
            Html(include_str!("index.html"))
        }

        #[derive(Serialize)]
        struct StatusResponse {
            configs: HashMap<String, BotConfig>,
            status: HashMap<String, WebBotStatus>,
        }

        async fn get_status(AxumState(state): AxumState<Arc<RwLock<GlobalAppState>>>) -> Json<StatusResponse> {
            let lock = state.read();
            Json(StatusResponse {
                configs: lock.configs.clone(),
                status: lock.status.clone(),
            })
        }

        async fn get_logs(AxumState(state): AxumState<Arc<RwLock<GlobalAppState>>>) -> Json<Vec<LogMessage>> {
            let mut lock = state.write();
            let logs = lock.logs.drain(..).collect::<Vec<_>>();
            Json(logs)
        }

        #[derive(Deserialize)]
        struct ActionPayload {
            server: Option<String>,
            alias: String,
        }

        async fn post_start(
            AxumState(ctx): AxumState<WebContext>,
            Json(payload): Json<ActionPayload>,
        ) -> Result<&'static str, &'static str> {
            let server = payload.server.ok_or("缺少 server 参数")?;
            let _ = ctx.cmd_tx.send(BotCommand::Start {
                server,
                alias: payload.alias,
            });
            Ok("已发送启动指令")
        }

        async fn post_stop(
            AxumState(ctx): AxumState<WebContext>,
            Json(payload): Json<ActionPayload>,
        ) -> Result<&'static str, &'static str> {
            let _ = ctx.cmd_tx.send(BotCommand::Stop {
                alias: payload.alias,
            });
            Ok("已发送停止指令")
        }

        #[derive(Deserialize)]
        struct ChatPayload {
            alias: String,
            message: String,
        }

        async fn post_send(
            AxumState(ctx): AxumState<WebContext>,
            Json(payload): Json<ChatPayload>,
        ) -> Result<&'static str, &'static str> {
            let _ = ctx.cmd_tx.send(BotCommand::SendChat {
                alias: payload.alias,
                message: payload.message,
            });
            Ok("已发送聊天内容")
        }

        let app = Router::new()
            .route("/", get(handle_index))
            .route("/api/status", get(get_status).with_state(global_state.clone()))
            .route("/api/logs", get(get_logs).with_state(global_state))
            .route("/api/start", post(post_start))
            .route("/api/stop", post(post_stop))
            .route("/api/send", post(post_send))
            .with_state(web_ctx);

        let port = 14217;
        let listener = match tokio::net::TcpListener::bind(format!("0.0.0.0:{}", port)).await {
            Ok(l) => l,
            Err(e) => {
                eprintln!("[错误] 绑定 Web 端口 {} 失败！可能是该端口正被占用。具体错误: {:?}", port, e);
                std::process::exit(1);
            }
        };

        println!("==================================================");
        println!("  Azalea 挂机机器人 Web 控制台启动成功！");
        println!("  请在手机浏览器（Termux）或电脑浏览器中访问：");
        println!("  👉 http://localhost:{} 👈", port);
        println!("==================================================");

        axum::serve(listener, app).await.unwrap();
        std::process::exit(0);
    }

    // 2. 带参数运行：格式解析（向下兼容原本的命令行操作）
    if args.len() < 3 {
        eprintln!("[错误] 缺少必要参数。");
        eprintln!("命令行用法说明:");
        eprintln!("  1. 仅登录绑定别名 (仅生成 <别名>.key 授权文件，不需要服务器IP):");
        eprintln!("     azalea_bot.exe <微软邮箱> <别名>");
        eprintln!("  2. 使用别名启动 (自动多开，读取 config.yml，无需参数):");
        eprintln!("     azalea_bot.exe");
        eprintln!("  3. 命令行直接连接服务器并启动:");
        eprintln!("     azalea_bot.exe <服务器地址> <别名> [look <y> <p>] [use [<ticks>]] [atk [<ticks>]]");
        std::process::exit(1);
    }

    // 格式 1: 仅登录并生成玩家别名授权文件 (不需要服务器 IP)
    if args.len() == 3 && args[1].contains('@') {
        let email = args[1].clone();
        let alias = args[2].clone();
        println!("正在通过微软账号登录并生成别名 '{}' 的授权 Key 文件...", alias);
        let cache_file_path = exe_dir.join(format!("{}.key", alias));
        let mut auth_opts = azalea::account::microsoft::MicrosoftAccountOpts::default();
        auth_opts.cache_file = Some(cache_file_path);
        match Account::microsoft_with_opts(&email, auth_opts).await {
            Ok(_) => {
                println!("成功！已生成别名 '{}' 的授权文件。现可将其写入 config.yml 配置文件中进行自动多开挂机！", alias);
                std::process::exit(0);
            }
            Err(e) => {
                eprintln!("[错误] 微软账号授权登录失败: {:?}", e);
                std::process::exit(1);
            }
        }
    }

    let server_address = &args[1];
    let email: String;
    let alias: String;
    let mut command_start_index = 3;

    if args[2].contains('@') {
        if args.len() < 4 {
            eprintln!("[错误] 首次登录绑定别名必须提供别名参数！");
            std::process::exit(1);
        }
        email = args[2].clone();
        alias = args[3].clone();
        command_start_index = 4;
    } else {
        alias = args[2].clone();
        let cache_file_path = exe_dir.join(format!("{}.key", alias));
        if !cache_file_path.exists() {
            eprintln!("[错误] 找不到别名 '{}' 对应的授权文件 '{}.key'！", alias, alias);
            std::process::exit(1);
        }

        let created_secs = get_file_created_secs(&cache_file_path).await;

        let encrypted_bytes = match std::fs::read(&cache_file_path) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("[错误] 读取授权文件 {}.key 失败: {:?}", alias, e);
                std::process::exit(1);
            }
        };

        let mini_cache: Vec<MiniCachedAccount> = match decrypt_and_parse_cache(&encrypted_bytes, created_secs) {
            Some(c) => c,
            None => {
                eprintln!("[错误] 授权文件 {}.key 解密失败！", alias);
                std::process::exit(1);
            }
        };

        if mini_cache.is_empty() {
            eprintln!("[错误] 授权文件中未找到已缓存的账号！");
            std::process::exit(1);
        }

        email = mini_cache[0].cache_key.clone();
    }

    let mut look_target = None;
    let mut use_interval = None;
    let mut atk_interval = None;

    let mut i = command_start_index;
    while i < args.len() {
        if args[i] == "look" {
            if i + 2 < args.len() {
                if let (Ok(y_rot), Ok(x_rot)) = (args[i + 1].parse::<f32>(), args[i + 2].parse::<f32>()) {
                    look_target = Some((y_rot, x_rot));
                    i += 3;
                } else {
                    eprintln!("[错误] 视角偏角必须是数字。");
                    std::process::exit(1);
                }
            } else {
                eprintln!("[错误] 'look' 参数不完整。");
                std::process::exit(1);
            }
        } else if args[i] == "use" {
            if i + 1 < args.len() && args[i + 1].parse::<usize>().is_ok() {
                let ticks = args[i + 1].parse::<usize>().unwrap();
                use_interval = Some(ticks);
                i += 2;
            } else {
                use_interval = Some(0);
                i += 1;
            }
        } else if args[i] == "atk" {
            if i + 1 < args.len() && args[i + 1].parse::<usize>().is_ok() {
                let ticks = args[i + 1].parse::<usize>().unwrap();
                atk_interval = Some(ticks);
                i += 2;
            } else {
                atk_interval = Some(0);
                i += 1;
            }
        } else {
            eprintln!("[错误] 未知的命令参数: '{}'", args[i]);
            std::process::exit(1);
        }
    }

    let cache_file_path = exe_dir.join(format!("{}.key", alias));
    let mut auth_opts = azalea::account::microsoft::MicrosoftAccountOpts::default();
    auth_opts.cache_file = Some(cache_file_path);

    let account = match Account::microsoft_with_opts(&email, auth_opts).await {
        Ok(acc) => acc,
        Err(e) => {
            eprintln!("[错误] 微软账号登录失败: {:?}", e);
            std::process::exit(1);
        }
    };

    let initial_state = BotState {
        look_direction: Arc::new(Mutex::new(look_target)),
        use_interval: Arc::new(Mutex::new(use_interval)),
        atk_interval: Arc::new(Mutex::new(atk_interval)),
        ..Default::default()
    };

    ClientBuilder::new()
        .set_handler(handle)
        .set_state(initial_state)
        .start(account, server_address.as_str())
        .await
}

async fn handle(bot: Client, event: Event, state: BotState) -> eyre::Result<()> {
    match event {
        Event::Spawn => {
            let username = bot.username();
            println!("机器人 {} 已成功在世界中生成！", username);
            add_active_bot(bot.clone());
            if !WELCOME_PRINTED.swap(true, Ordering::SeqCst) {
                println!("挂机控制台就绪！输入任何内容并回车，都将直接发送到服务器聊天中。");
            }
        }
        Event::Chat(m) => {
            println!("{}", m.message().to_ansi());
        }
        Event::Tick => {
            if let Some((y_rot, x_rot)) = *state.look_direction.lock() {
                let _ = bot.query_self::<&mut azalea::entity::LookDirection, _>(|mut look| {
                    look.update(azalea::entity::LookDirection::new(y_rot, x_rot));
                });
            }

            let mut use_counter = state.use_counter.lock();
            let use_interval = *state.use_interval.lock();
            if let Some(interval) = use_interval {
                if interval == 0 {
                    bot.start_use_item();
                } else {
                    *use_counter += 1;
                    if *use_counter >= interval {
                        bot.start_use_item();
                        *use_counter = 0;
                    }
                }
            }

            let mut atk_counter = state.atk_counter.lock();
            let atk_interval = *state.atk_interval.lock();
            if let Some(interval) = atk_interval {
                if interval == 0 {
                    do_attack(&bot);
                } else {
                    *atk_counter += 1;
                    if *atk_counter >= interval {
                        do_attack(&bot);
                        *atk_counter = 0;
                    }
                }
            }
        }
        Event::Disconnect(reason) => {
            println!("已与服务器断开连接: {:?}", reason);
            remove_active_bot(&bot.uuid());
        }
        _ => {}
    }
    Ok(())
}

fn do_attack(bot: &Client) {
    if let Ok(hit) = bot.hit_result() {
        if let azalea::core::hit_result::HitResult::Entity(entity_hit) = hit {
            bot.attack(entity_hit.entity);
        } else {
            bot.ecs.write().trigger(SwingArmEvent {
                entity: bot.entity,
            });
        }
    } else {
        bot.ecs.write().trigger(SwingArmEvent {
            entity: bot.entity,
        });
    }
}
