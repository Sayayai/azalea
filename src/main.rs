use std::sync::Arc;
use std::path::Path;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use azalea::prelude::*;
use azalea::interact::SwingArmEvent;
use parking_lot::Mutex;
use tokio::io::{AsyncBufReadExt, BufReader};
use serde::{Deserialize, Serialize};

#[derive(Default, Clone, Component)]
pub struct State {
    pub look_direction: Arc<Mutex<Option<(f32, f32)>>>, // Option<(y_rot, x_rot)>
    pub use_interval: Arc<Mutex<Option<usize>>>, // None = off, Some(0) = every tick (hold), Some(n) = every n ticks
    pub atk_interval: Arc<Mutex<Option<usize>>>, // None = off, Some(0) = every tick (hold), Some(n) = every n ticks
    pub use_counter: Arc<Mutex<usize>>,
    pub atk_counter: Arc<Mutex<usize>>,
}

// YAML 配置文件结构
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct BotConfig {
    pub look: Option<String>,
    #[serde(rename = "use", default)]
    pub use_mode: Option<serde_yaml::Value>,
    #[serde(rename = "atk", default)]
    pub atk_mode: Option<serde_yaml::Value>,
}

pub type AppConfig = std::collections::HashMap<String, std::collections::HashMap<String, BotConfig>>;

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

    // 1. 直接运行 (不带参数)：读取或创建 config.yml
    if args.len() == 1 {
        let config_path = exe_dir.join("config.yml");
        if !config_path.exists() {
            // 自动生成默认 of config.yml 模板文件
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
            // 将序列化的 null 关键字替换为空白，生成更清爽的 "atk:"
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

        if config.is_empty() {
            println!("[错误] 配置文件中的服务器列表为空！");
            std::process::exit(1);
        }

        println!("已载入配置文件，正在初始化多开挂机任务...");
        
        let local = tokio::task::LocalSet::new();
        local.run_until(async {
            let mut join_handles = Vec::new();

            for (server_address, bots) in config {
                for (alias, bot_config) in bots {
                    let server_addr = server_address.clone();
                    let alias_name = alias.clone();
                    let exe_dir_clone = exe_dir.clone();

                    // 解析 look 视角偏角
                    let look_target = bot_config.look.as_ref().and_then(|s| {
                        let parts: Vec<&str> = s.split_whitespace().collect();
                        if parts.len() == 2 {
                            if let (Ok(y), Ok(p)) = (parts[0].parse::<f32>(), parts[1].parse::<f32>()) {
                                return Some((y, p));
                            }
                        }
                        None
                    });

                    // 解析 use 和 atk 间隔
                    let use_interval = parse_mode(&bot_config.use_mode);
                    let atk_interval = parse_mode(&bot_config.atk_mode);

                    // 使用 LocalSet 拉起非 Send 的 ClientBuilder::start 任务
                    let handle = tokio::task::spawn_local(async move {
                        let cache_file_path = exe_dir_clone.join(format!("{}.key", alias_name));
                        if !cache_file_path.exists() {
                            eprintln!("[错误] 找不到别名 '{}' 对应的授权文件 '{}.key'，跳过连接 {}！", alias_name, alias_name, server_addr);
                            eprintln!("  请先通过命令行登录注册生成它: azalea_bot.exe {} <微软邮箱> {}", server_addr, alias_name);
                            return;
                        }

                        // 利用该缓存文件的 UTC 创建时间解密并反解出里面的邮箱
                        let created_secs = get_file_created_secs(&cache_file_path).await.unwrap_or(0);
                        let key_str = created_secs.to_string();
                        let key = key_str.as_bytes();

                        let encrypted_bytes = match std::fs::read(&cache_file_path) {
                            Ok(b) => b,
                            Err(e) => {
                                eprintln!("[错误] 读取授权文件 {}.key 失败: {:?}", alias_name, e);
                                return;
                            }
                        };

                        let mut decrypted_bytes = encrypted_bytes.clone();
                        if !key.is_empty() {
                            for (i, byte) in decrypted_bytes.iter_mut().enumerate() {
                                *byte ^= key[i % key.len()];
                            }
                        }

                        let mini_cache: Vec<MiniCachedAccount> = match serde_json::from_slice(&decrypted_bytes) {
                            Ok(c) => c,
                            Err(_) => {
                                eprintln!("[错误] 别名 '{}' 的授权文件 {}.key 解密失败！", alias_name, alias_name);
                                eprintln!("原因: 文件可能损坏，或者它的创建时间被系统重置（例如被复制/拷贝到了其他电脑）。");
                                return;
                            }
                        };

                        if mini_cache.is_empty() {
                            eprintln!("[错误] 授权文件 {}.key 中未找到已缓存的账号！", alias_name);
                            return;
                        }

                        let email = mini_cache[0].cache_key.clone();
                        let mut auth_opts = azalea::account::microsoft::MicrosoftAccountOpts::default();
                        auth_opts.cache_file = Some(cache_file_path);

                        println!("正在拉起任务: 服务器={}, 别名={}, 邮箱={}", server_addr, alias_name, email);
                        let account = match Account::microsoft_with_opts(&email, auth_opts).await {
                            Ok(acc) => acc,
                            Err(e) => {
                                eprintln!("[错误] 别名 '{}' ({}) 登录失败: {:?}", alias_name, email, e);
                                return;
                            }
                        };

                        let initial_state = State {
                            look_direction: Arc::new(Mutex::new(look_target)),
                            use_interval: Arc::new(Mutex::new(use_interval)),
                            atk_interval: Arc::new(Mutex::new(atk_interval)),
                            ..Default::default()
                        };

                        let _ = ClientBuilder::new()
                            .set_handler(handle)
                            .set_state(initial_state)
                            .start(account, server_addr.as_str())
                            .await;
                    });
                    join_handles.push(handle);
                }
            }

            if join_handles.is_empty() {
                eprintln!("[错误] 无可启动的任务，程序退出。");
                std::process::exit(1);
            }

            // 并发等待所有 Client 任务运行
            futures::future::join_all(join_handles).await;
        }).await;
        
        std::process::exit(0);
    }

    // 2. 带参数运行：格式解析
    if args.len() < 3 {
        eprintln!("[错误] 缺少必要参数。");
        eprintln!("命令行用法说明:");
        eprintln!("  1. 仅登录绑定别名 (仅生成 <别名>.key 授权文件，不需要服务器IP):");
        eprintln!("     azalea_bot.exe <微软邮箱> <别名>");
        eprintln!("  2. 使用别名启动 (自动多开，读取 config.yml，无需参数):");
        eprintln!("     azalea_bot.exe");
        eprintln!("  3. 命令行直接连接服务器并启动:");
        eprintln!("     azalea_bot.exe <服务器地址> <别名> [look <y> <p>] [use [<ticks>]] [atk [<ticks>]]");
        eprintln!("示例:");
        eprintln!("  生成 Key: azalea_bot.exe player@outlook.com bot1");
        eprintln!("  连接服务器: azalea_bot.exe localhost:25565 bot1");
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
    
    // 参数提取逻辑
    let email: String;
    let alias: String;
    let mut command_start_index = 3;

    if args[2].contains('@') {
        // 格式 1 (兼容旧版): 首次登录绑定别名并立即连接
        if args.len() < 4 {
            eprintln!("[错误] 首次登录绑定别名必须提供别名参数！");
            eprintln!("用法: azalea_bot.exe <服务器地址> <微软邮箱> <别名> [命令...]");
            std::process::exit(1);
        }
        email = args[2].clone();
        alias = args[3].clone();
        command_start_index = 4;
        println!("正在为邮箱 {} 注册绑定别名: {}", email, alias);
    } else {
        // 格式 2: 直接使用已有别名启动
        alias = args[2].clone();
        let cache_file_path = exe_dir.join(format!("{}.key", alias));
        if !cache_file_path.exists() {
            eprintln!("[错误] 找不到别名 '{}' 对应的授权文件 '{}.key'！", alias, alias);
            eprintln!("提示: 如果是第一次登录，必须同时传入微软邮箱完成绑定！");
            eprintln!("用法: azalea_bot.exe {} <微软邮箱> {} [命令...]", server_address, alias);
            std::process::exit(1);
        }

        // 利用该缓存文件的 UTC 创建时间解密并反解出里面的邮箱
        let created_secs = get_file_created_secs(&cache_file_path).await.unwrap_or(0);
        let key_str = created_secs.to_string();
        let key = key_str.as_bytes();

        let encrypted_bytes = match std::fs::read(&cache_file_path) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("[错误] 读取授权文件 {}.key 失败: {:?}", alias, e);
                std::process::exit(1);
            }
        };

        let mut decrypted_bytes = encrypted_bytes.clone();
        if !key.is_empty() {
            for (i, byte) in decrypted_bytes.iter_mut().enumerate() {
                *byte ^= key[i % key.len()];
            }
        }

        let mini_cache: Vec<MiniCachedAccount> = match serde_json::from_slice(&decrypted_bytes) {
            Ok(c) => c,
            Err(_) => {
                eprintln!("[错误] 授权文件 {}.key 解密失败！", alias);
                eprintln!("原因: 文件可能损坏，或者它的创建时间被系统重置（例如被复制/拷贝到了其他电脑）。");
                std::process::exit(1);
            }
        };

        if mini_cache.is_empty() {
            eprintln!("[错误] 授权文件中未找到已缓存的账号！");
            std::process::exit(1);
        }

        email = mini_cache[0].cache_key.clone();
        println!("别名 '{}' 动态解析成功，正版邮箱为: {}", alias, email);
    }

    // 动态解析指令配置
    let mut look_target = None;
    let mut use_interval = None;
    let mut atk_interval = None;

    let mut i = command_start_index;
    while i < args.len() {
        if args[i] == "look" {
            if i + 2 < args.len() {
                if let (Ok(y_rot), Ok(x_rot)) = (
                    args[i + 1].parse::<f32>(),
                    args[i + 2].parse::<f32>(),
                ) {
                    look_target = Some((y_rot, x_rot));
                    println!("视角锁定配置成功: 水平角度(yaw)={}, 垂直角度(pitch)={}", y_rot, x_rot);
                } else {
                    eprintln!("[错误] 视角偏角必须是数字。");
                    std::process::exit(1);
                }
                i += 3;
            } else {
                eprintln!("[错误] 'look' 参数不完整。正确格式: look <yaw> <pitch>");
                std::process::exit(1);
            }
        } else if args[i] == "use" {
            if i + 1 < args.len() && args[i + 1].parse::<usize>().is_ok() {
                let ticks = args[i + 1].parse::<usize>().unwrap();
                use_interval = Some(ticks);
                println!("右键使用 (use) 周期配置成功: 每 {} tick 触发一次", ticks);
                i += 2;
            } else {
                use_interval = Some(0); // 默认长按
                println!("右键使用 (use) 周期配置成功: 已开启长按右键模式");
                i += 1;
            }
        } else if args[i] == "atk" {
            if i + 1 < args.len() && args[i + 1].parse::<usize>().is_ok() {
                let ticks = args[i + 1].parse::<usize>().unwrap();
                atk_interval = Some(ticks);
                println!("左键攻击 (atk) 周期配置成功: 每 {} tick 触发一次", ticks);
                i += 2;
            } else {
                atk_interval = Some(0); // 默认长按
                println!("左键攻击 (atk) 周期配置成功: 已开启常按连点/挥臂模式");
                i += 1;
            }
        } else {
            eprintln!("[错误] 未知的命令参数: '{}'", args[i]);
            std::process::exit(1);
        }
    }

    println!("正在通过微软账号登录: {}", email);
    
    // 生成对应的 <别名>.key 缓存路径
    let cache_file_path = exe_dir.join(format!("{}.key", alias));

    let mut auth_opts = azalea::account::microsoft::MicrosoftAccountOpts::default();
    auth_opts.cache_file = Some(cache_file_path);

    // Authenticate using Microsoft account.
    let account = match Account::microsoft_with_opts(&email, auth_opts).await {
        Ok(acc) => acc,
        Err(e) => {
            eprintln!("[错误] 微软账号登录失败: {:?}", e);
            std::process::exit(1);
        }
    };

    println!("正在连接服务器: {}", server_address);

    // 构造具有指定参数的状态实例
    let initial_state = State {
        look_direction: Arc::new(Mutex::new(look_target)),
        use_interval: Arc::new(Mutex::new(use_interval)),
        atk_interval: Arc::new(Mutex::new(atk_interval)),
        ..Default::default()
    };

    // Start the client
    ClientBuilder::new()
        .set_handler(handle)
        .set_state(initial_state)
        .start(account, server_address.as_str())
        .await
}

async fn handle(bot: Client, event: Event, state: State) -> eyre::Result<()> {
    match event {
        Event::Init => {
            // 初始化事件
        }
        Event::Spawn => {
            let username = bot.username();
            println!("机器人 {} 已成功在世界中生成！", username);
            add_active_bot(bot.clone());
            if !WELCOME_PRINTED.swap(true, Ordering::SeqCst) {
                println!("挂机控制台就绪！输入任何内容并回车，都将直接发送到服务器聊天中。");
            }
        }
        Event::Chat(m) => {
            // 打印聊天
            println!("{}", m.message().to_ansi());
        }
        Event::Tick => {
            // 锁定朝向偏角
            if let Some((y_rot, x_rot)) = *state.look_direction.lock() {
                let _ = bot.query_self::<&mut azalea::entity::LookDirection, _>(|mut look| {
                    look.update(azalea::entity::LookDirection::new(y_rot, x_rot));
                });
            }

            // 触发右键使用
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

            // 触发左键攻击
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
