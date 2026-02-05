use clap::Parser;
use dialoguer::{Input, Select, theme::ColorfulTheme};
use serde::{Deserialize, Serialize};
use std::env;
use std::fs;
use std::io::Write; // Нужно для записи файлов
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;
use std::thread;
use std::time::{Duration, SystemTime};

// --- КОНФИГУРАЦИЯ И ПУТИ ---
const CONFIG_DIR: &str = "/etc/portal_daemon";
const CONFIG_FILE: &str = "/etc/portal_daemon/config.json";
const PAUSE_FILE: &str = "/tmp/portal.pause";

// Для установки
const BINARY_DEST: &str = "/usr/local/bin/portal_daemon";
const GROUP_NAME: &str = "portal-admins";
const DOAS_CONF: &str = "/etc/doas.conf";
const SUDOERS_FILE: &str = "/etc/sudoers.d/portal-daemon";

#[derive(Serialize, Deserialize, Debug, Clone, Copy)]
enum Language {
    En,
    Ru,
}

#[derive(Serialize, Deserialize, Debug)]
struct PortalConfig {
    language: Language,
    lighthouse_ip: String,
    target_ssid: String,
    sleep_minutes: u64,
    grace_period_sec: u64,
    wakeup_wait_sec: u64,
    scan_interval_sec: u64,
}

impl Default for PortalConfig {
    fn default() -> Self {
        Self {
            language: Language::En,
            lighthouse_ip: "192.168.1.1".to_string(),
            target_ssid: "Unknown".to_string(),
            sleep_minutes: 60,
            grace_period_sec: 300,
            wakeup_wait_sec: 30,
            scan_interval_sec: 60,
        }
    }
}

// --- АРГУМЕНТЫ ---
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    #[arg(long)]
    install: bool,
    #[arg(long)]
    configure: bool,
    #[arg(long)]
    off: bool,
}

fn main() {
    let args = Args::parse();

    // 1. Установка (требует root)
    if args.install {
        run_system_install();
        return;
    }

    // Загружаем конфиг (если есть), чтобы знать язык для меню
    let mut temp_lang = Language::En;
    if let Ok(cfg) = load_config_safe() {
        temp_lang = cfg.language;
    }

    // 2. Меню управления (выключить/пауза)
    if args.off {
        run_control_menu(temp_lang);
        return;
    }

    // 3. Логика загрузки конфига или визарда
    // Если конфига нет ИЛИ явно попросили --configure
    let config = if args.configure || !Path::new(CONFIG_FILE).exists() {
        // Проверяем права, так как писать будем в /etc
        if !is_root() {
            println!(
                "⚠️  Config setup requires ROOT permissions to write to {}.",
                CONFIG_FILE
            );
            println!("⚠️  Please run with sudo/doas.");
            std::process::exit(1);
        }
        run_interactive_wizard()
    } else {
        load_config_safe().unwrap_or_default()
    };

    // 4. Запуск демона
    run_daemon(config);
}

// --- СЛОВАРЬ (LOCALIZATION) ---
struct Locales {
    wizard_title: String,
    scan_msg: String,
    scan_fail: String,
    enter_ip_manual: String,
    select_net: String,
    selected_net_log: String,
    enter_ip_prompt: String,
    sleep_mins_prompt: String,
    grace_sec_prompt: String,
    wakeup_sec_prompt: String,
    scan_int_prompt: String,
    settings_saved: String,

    daemon_start: String,
    daemon_net: String,
    daemon_interval: String,
    conn_lost: String,
    conn_restored: String,
    no_light_sleep: String,
    waking_up: String,

    ctrl_title: String,
    ctrl_action: String,
    ctrl_pause: String,
    ctrl_resume: String,
    ctrl_kill: String,
    ctrl_exit: String,
    pause_prompt: String,
    pause_activated: String,
    pause_removed: String,
    process_killed: String,
}

impl Locales {
    fn new(lang: Language) -> Self {
        match lang {
            Language::En => Locales {
                wizard_title: "\n🔧 --- PORTAL SETUP WIZARD ---".into(),
                scan_msg: "🔍 Scanning networks...".into(),
                scan_fail: "❌ No networks found.".into(),
                enter_ip_manual: "Enter Lighthouse IP Manually".into(),
                select_net: "Select Network:".into(),
                selected_net_log: "✅ Selected Network:".into(),
                enter_ip_prompt: "Enter Lighthouse IP".into(),
                sleep_mins_prompt: "Minutes to sleep without light?".into(),
                grace_sec_prompt: "Grace period (sec) before sleep?".into(),
                wakeup_sec_prompt: "Wait (sec) after waking up?".into(),
                scan_int_prompt: "Scan interval (sec)?".into(),
                settings_saved: format!("✅ Settings saved to {}!", CONFIG_FILE),

                daemon_start: "👻 Portal Daemon: START".into(),
                daemon_net: "📡 Network:".into(),
                daemon_interval: "⏱ Interval:".into(),
                conn_lost: "⚠️  Connection lost. Waiting".into(),
                conn_restored: "✅ Connection restored.".into(),
                no_light_sleep: "🌑 No light. Sleeping".into(),
                waking_up: "☀️  Woke up. Waiting".into(),

                ctrl_title: "\n🎮 --- PORTAL CONTROL ---".into(),
                ctrl_action: "Action?".into(),
                ctrl_pause: "⏸  PAUSE (Disable sleep for X mins)".into(),
                ctrl_resume: "▶️  RESUME (Enable sleep mode)".into(),
                ctrl_kill: "🛑  KILL Process".into(),
                ctrl_exit: "❌  Exit".into(),
                pause_prompt: "Pause for how many MINUTES?".into(),
                pause_activated: "✅ Pause activated for".into(),
                pause_removed: "✅ Pause removed.".into(),
                process_killed: "💀 Process stopped.".into(),
            },
            Language::Ru => Locales {
                wizard_title: "\n🔧 --- МАСТЕР НАСТРОЙКИ PORTAL ---".into(),
                scan_msg: "🔍 Сканирую сети...".into(),
                scan_fail: "❌ Сети не найдены.".into(),
                enter_ip_manual: "Ввести IP Маяка вручную".into(),
                select_net: "Выбери сеть:".into(),
                selected_net_log: "✅ Выбрана сеть:".into(),
                enter_ip_prompt: "Введи IP Маяка".into(),
                sleep_mins_prompt: "Сколько МИНУТ спать без света?".into(),
                grace_sec_prompt: "Грейс-период (сек) перед сном?".into(),
                wakeup_sec_prompt: "Ждать сек. после включения?".into(),
                scan_int_prompt: "Интервал проверки (сек)?".into(),
                settings_saved: format!("✅ Настройки сохранены в {}!", CONFIG_FILE),

                daemon_start: "👻 Portal Daemon: ЗАПУСК".into(),
                daemon_net: "📡 Сеть:".into(),
                daemon_interval: "⏱ Интервал:".into(),
                conn_lost: "⚠️  Потеря связи. Ждем".into(),
                conn_restored: "✅ Связь вернулась.".into(),
                no_light_sleep: "🌑 Света нет. Сон".into(),
                waking_up: "☀️  Проснулись. Ждем".into(),

                ctrl_title: "\n🎮 --- УПРАВЛЕНИЕ PORTAL ---".into(),
                ctrl_action: "Действие?".into(),
                ctrl_pause: "⏸  Поставить на ПАУЗУ".into(),
                ctrl_resume: "▶️  Снять с паузы".into(),
                ctrl_kill: "🛑  Убить процесс (Kill)".into(),
                ctrl_exit: "❌  Выход".into(),
                pause_prompt: "На сколько МИНУТ?".into(),
                pause_activated: "✅ Пауза активирована на".into(),
                pause_removed: "✅ Пауза снята.".into(),
                process_killed: "💀 Процесс остановлен.".into(),
            },
        }
    }
}

// === МЕНЮ УПРАВЛЕНИЯ ===
fn run_control_menu(lang: Language) {
    let t = Locales::new(lang);
    println!("{}", t.ctrl_title);

    let selections = vec![&t.ctrl_pause, &t.ctrl_resume, &t.ctrl_kill, &t.ctrl_exit];
    let selection = Select::with_theme(&ColorfulTheme::default())
        .with_prompt(&t.ctrl_action)
        .default(0)
        .items(&selections)
        .interact()
        .unwrap();

    match selection {
        0 => {
            let mins: u64 = Input::with_theme(&ColorfulTheme::default())
                .with_prompt(&t.pause_prompt)
                .default(60)
                .interact_text()
                .unwrap();
            let end = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_secs()
                + (mins * 60);
            fs::write(PAUSE_FILE, end.to_string()).ok();
            println!("{} {} min.", t.pause_activated, mins);
        }
        1 => {
            fs::remove_file(PAUSE_FILE).ok();
            println!("{}", t.pause_removed);
        }
        2 => {
            Command::new("pkill")
                .args(["-f", "portal_daemon"])
                .status()
                .ok();
            fs::remove_file(PAUSE_FILE).ok();
            println!("{}", t.process_killed);
        }
        _ => {}
    }
}

// === МАСТЕР НАСТРОЙКИ ===
fn run_interactive_wizard() -> PortalConfig {
    // Создаем директорию конфига, если нет
    if !Path::new(CONFIG_DIR).exists() {
        println!("📂 Creating config directory: {}", CONFIG_DIR);
        fs::create_dir_all(CONFIG_DIR).expect("Failed to create config dir");
    }

    let langs = &["English (Default)", "Русский"];
    let lang_sel = Select::with_theme(&ColorfulTheme::default())
        .with_prompt("Select Language / Выберите язык")
        .default(0)
        .items(&langs[..])
        .interact()
        .unwrap();

    let lang = if lang_sel == 1 {
        Language::Ru
    } else {
        Language::En
    };
    let t = Locales::new(lang);

    println!("{}", t.wizard_title);

    let mut final_ip = String::new();
    let mut final_ssid = "Manual".to_string();

    println!("{}", t.scan_msg);
    let networks = scan_networks();

    if networks.is_empty() {
        println!("{}", t.scan_fail);
        final_ip = Input::with_theme(&ColorfulTheme::default())
            .with_prompt(&t.enter_ip_manual)
            .default("192.168.1.1".into())
            .interact_text()
            .unwrap();
    } else {
        let mut options: Vec<String> = networks
            .iter()
            .map(|n| format!("{} (GW: {})", n.ssid, n.gateway))
            .collect();
        options.push(t.enter_ip_manual.clone());

        let sel = Select::with_theme(&ColorfulTheme::default())
            .with_prompt(&t.select_net)
            .default(0)
            .items(&options)
            .interact()
            .unwrap();
        if sel < networks.len() {
            final_ip = networks[sel].gateway.clone();
            final_ssid = networks[sel].ssid.clone();
            println!(
                "{} {} -> Target IP: {}",
                t.selected_net_log, final_ssid, final_ip
            );
        } else {
            final_ip = Input::with_theme(&ColorfulTheme::default())
                .with_prompt(&t.enter_ip_prompt)
                .interact_text()
                .unwrap();
        }
    }

    let sleep_minutes: u64 = Input::with_theme(&ColorfulTheme::default())
        .with_prompt(&t.sleep_mins_prompt)
        .default(60)
        .interact_text()
        .unwrap();
    let grace_period_sec: u64 = Input::with_theme(&ColorfulTheme::default())
        .with_prompt(&t.grace_sec_prompt)
        .default(300)
        .interact_text()
        .unwrap();
    let wakeup_wait_sec: u64 = Input::with_theme(&ColorfulTheme::default())
        .with_prompt(&t.wakeup_sec_prompt)
        .default(30)
        .interact_text()
        .unwrap();
    let scan_interval_sec: u64 = Input::with_theme(&ColorfulTheme::default())
        .with_prompt(&t.scan_int_prompt)
        .default(60)
        .interact_text()
        .unwrap();

    let config = PortalConfig {
        language: lang,
        lighthouse_ip: final_ip,
        target_ssid: final_ssid,
        sleep_minutes,
        grace_period_sec,
        wakeup_wait_sec,
        scan_interval_sec,
    };

    let json = serde_json::to_string_pretty(&config).expect("Fail json");
    fs::write(CONFIG_FILE, json).expect("Fail write");
    println!("{}\n", t.settings_saved);
    config
}

// === ДЕМОН ===
fn run_daemon(cfg: PortalConfig) {
    let t = Locales::new(cfg.language);
    let sleep_seconds = cfg.sleep_minutes * 60;

    println!("{}", t.daemon_start);
    println!("{} {}", t.daemon_net, cfg.target_ssid);
    println!("{} {} sec", t.daemon_interval, cfg.scan_interval_sec);

    loop {
        if check_pause() {
            thread::sleep(Duration::from_secs(cfg.scan_interval_sec));
            continue;
        }

        if check_ping(&cfg.lighthouse_ip) {
            thread::sleep(Duration::from_secs(cfg.scan_interval_sec));
        } else {
            println!("{} {} sec...", t.conn_lost, cfg.grace_period_sec);
            thread::sleep(Duration::from_secs(cfg.grace_period_sec));
            if check_pause() {
                continue;
            }

            if check_ping(&cfg.lighthouse_ip) {
                println!("{}", t.conn_restored);
            } else {
                println!("{} {} min.", t.no_light_sleep, cfg.sleep_minutes);
                enter_hibernation(sleep_seconds);
                println!("{} {} sec...", t.waking_up, cfg.wakeup_wait_sec);
                thread::sleep(Duration::from_secs(cfg.wakeup_wait_sec));
            }
        }
    }
}

// === УТИЛИТЫ ===
fn load_config_safe() -> Result<PortalConfig, ()> {
    if let Ok(d) = fs::read_to_string(CONFIG_FILE) {
        if let Ok(c) = serde_json::from_str(&d) {
            return Ok(c);
        }
    }
    Err(())
}

fn check_pause() -> bool {
    if Path::new(PAUSE_FILE).exists() {
        if let Ok(c) = fs::read_to_string(PAUSE_FILE) {
            if let Ok(end) = c.trim().parse::<u64>() {
                let now = SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .unwrap()
                    .as_secs();
                if now < end {
                    return true;
                } else {
                    fs::remove_file(PAUSE_FILE).ok();
                    return false;
                }
            }
        }
        fs::remove_file(PAUSE_FILE).ok();
    }
    false
}

fn scan_networks() -> Vec<NetworkInfo> {
    let mut r = Vec::new();
    let o = Command::new("nmcli")
        .args(["-t", "-f", "NAME,DEVICE", "connection", "show", "--active"])
        .output()
        .ok();
    if let Some(out) = o {
        for l in String::from_utf8_lossy(&out.stdout).lines() {
            let p: Vec<&str> = l.split(':').collect();
            if p.len() >= 2 {
                let (s, d) = (p[0], p[1]);
                if d == "lo" || s.is_empty() {
                    continue;
                }
                if let Some(gw) = get_gateway_for_device(d) {
                    r.push(NetworkInfo {
                        ssid: s.to_string(),
                        device: d.to_string(),
                        gateway: gw,
                    });
                }
            }
        }
    }
    r
}

fn get_gateway_for_device(dev: &str) -> Option<String> {
    let o = Command::new("nmcli")
        .args(["-t", "dev", "show", dev])
        .output()
        .ok()?;
    for l in String::from_utf8_lossy(&o.stdout).lines() {
        if l.starts_with("IP4.GATEWAY:") {
            let p: Vec<&str> = l.split(':').collect();
            if p.len() >= 2 {
                let gw = p[1].trim();
                if !gw.is_empty() && gw != "--" {
                    return Some(gw.to_string());
                }
            }
        }
    }
    None
}

struct NetworkInfo {
    ssid: String,
    device: String,
    gateway: String,
}

fn check_ping(ip: &str) -> bool {
    Command::new("ping")
        .args(["-c", "1", "-W", "2", ip])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn enter_hibernation(seconds: u64) {
    let priv_cmd = if Path::new(DOAS_CONF).exists() {
        "doas"
    } else {
        "sudo"
    };

    let status_result = Command::new(priv_cmd)
        .args(["rtcwake", "-m", "mem", "-s", &seconds.to_string()])
        .status();

    if let Ok(s) = status_result {
        if s.success() {
            println!("✅ Sleep OK.");
            return;
        }
    }
    eprintln!("❌ Error: rtcwake failed.");
    thread::sleep(Duration::from_secs(60));
}

fn is_root() -> bool {
    let out = Command::new("id").arg("-u").output().unwrap();
    String::from_utf8_lossy(&out.stdout).trim() == "0"
}

// === УСТАНОВКА СИСТЕМЫ И СЕРВИСОВ ===
fn run_system_install() {
    println!("🚀 Starting SYSTEM INSTALL...");
    if !is_root() {
        eprintln!("❌ Error: Install must be run as root (sudo/doas)!");
        std::process::exit(1);
    }

    // 1. Копирование бинарника
    if let Ok(current_exe) = env::current_exe() {
        println!("📦 Copying binary to {}...", BINARY_DEST);
        if let Err(e) = fs::copy(&current_exe, BINARY_DEST) {
            eprintln!("❌ Failed to copy binary: {}", e);
        } else {
            // Делаем исполняемым (на всякий случай)
            fs::set_permissions(BINARY_DEST, fs::Permissions::from_mode(0o755)).unwrap();
        }
    } else {
        eprintln!("❌ Cannot find current executable path.");
    }

    // 2. Настройка прав (sudo/doas)
    let rtc = find_binary("rtcwake").unwrap_or_else(|| "/usr/sbin/rtcwake".to_string());
    let net = find_binary("nmcli").unwrap_or_else(|| "/usr/bin/nmcli".to_string());

    println!("👤 Creating group {}...", GROUP_NAME);
    Command::new("groupadd")
        .arg("-f")
        .arg(GROUP_NAME)
        .status()
        .unwrap();

    if let Some(u) = env::var("SUDO_USER").ok().or(env::var("DOAS_USER").ok()) {
        println!("👤 Adding user '{}' to group...", u);
        Command::new("usermod")
            .args(["-aG", GROUP_NAME, &u])
            .status()
            .unwrap();
    }

    if Path::new(DOAS_CONF).exists() {
        setup_doas(&rtc, &net);
    } else {
        setup_sudo(&rtc, &net);
    }

    // 3. Установка сервиса (Systemd vs OpenRC)
    install_service();

    println!("\n🎉 INSTALLATION COMPLETE!");
    println!("👉 Run 'portal_daemon --configure' to set up IPs.");
}

fn install_service() {
    // Проверяем Systemd
    if Path::new("/run/systemd/system").exists() || Path::new("/usr/lib/systemd").exists() {
        println!("⚙️  Detected Systemd.");
        let service_content = format!(
            r#"[Unit]
Description=Portal Daemon (Network Sleep Manager)
After=network.target

[Service]
ExecStart={}
Restart=always
User=root
Group=root

[Install]
WantedBy=multi-user.target
"#,
            BINARY_DEST
        );

        let service_path = "/etc/systemd/system/portal.service";
        fs::write(service_path, service_content).expect("Failed to write service file");
        println!("   📄 Created {}", service_path);

        Command::new("systemctl")
            .args(["daemon-reload"])
            .status()
            .ok();
        Command::new("systemctl")
            .args(["enable", "--now", "portal"])
            .status()
            .ok();
        println!("   ✅ Service enabled & started.");
    } else {
        // Предполагаем OpenRC (Gentoo/Artix)
        println!("⚙️  Detected OpenRC (or fallback).");
        let openrc_content = format!(
            r#"#!/sbin/openrc-run

name="portal"
description="Portal Daemon"
command="{}"
command_background=true
pidfile="/run/portal.pid"

depend() {{
    need net
}}
"#,
            BINARY_DEST
        );

        let init_path = "/etc/init.d/portal";
        fs::write(init_path, openrc_content).expect("Failed to write init script");
        fs::set_permissions(init_path, fs::Permissions::from_mode(0o755))
            .expect("Failed to chmod init script");
        println!("   📄 Created {} (executable)", init_path);

        Command::new("rc-update")
            .args(["add", "portal", "default"])
            .status()
            .ok();
        Command::new("rc-service")
            .args(["portal", "start"])
            .status()
            .ok();
        println!("   ✅ Service added to default runlevel & started.");
    }
}

fn find_binary(bin: &str) -> Option<String> {
    Command::new("which").arg(bin).output().ok().and_then(|o| {
        if o.status.success() {
            Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
        } else {
            None
        }
    })
}

fn setup_doas(rtc: &str, net: &str) {
    println!("🦅 Configuring Doas...");
    let r1 = format!("permit nopass :{} cmd {}", GROUP_NAME, rtc);
    let r2 = format!("permit nopass :{} cmd {}", GROUP_NAME, net);
    let mut c = fs::read_to_string(DOAS_CONF).unwrap_or_default();

    if !c.contains(&r1) {
        c.push_str(&format!("\n{}\n", r1));
    }
    if !c.contains(&r2) {
        c.push_str(&format!("{}\n", r2));
    }

    fs::write(DOAS_CONF, c).unwrap();
}

fn setup_sudo(rtc: &str, net: &str) {
    println!("🐧 Configuring Sudo...");
    let r = format!("%{} ALL=(root) NOPASSWD: {}, {}\n", GROUP_NAME, rtc, net);
    let t = "/tmp/portal_check";
    fs::write(t, r).unwrap();

    if Command::new("visudo")
        .args(["-c", "-f", t])
        .status()
        .unwrap()
        .success()
    {
        fs::set_permissions(t, fs::Permissions::from_mode(0o440)).unwrap();
        Command::new("mv").args([t, SUDOERS_FILE]).status().unwrap();
    }
}
