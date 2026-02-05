use clap::Parser;
use dialoguer::{Input, Select, theme::ColorfulTheme};
use serde::{Deserialize, Serialize};
use std::env;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;
use std::thread;
use std::time::{Duration, SystemTime};

// --- КОНФИГУРАЦИЯ ---
#[derive(Serialize, Deserialize, Debug)]
struct PortalConfig {
    lighthouse_ip: String,
    target_ssid: String,
    sleep_minutes: u64,
    grace_period_sec: u64,
    wakeup_wait_sec: u64, // НОВОЕ: Сколько ждать после пробуждения
}

impl Default for PortalConfig {
    fn default() -> Self {
        Self {
            lighthouse_ip: "192.168.1.1".to_string(),
            target_ssid: "Unknown".to_string(),
            sleep_minutes: 60,
            grace_period_sec: 300,
            wakeup_wait_sec: 30,
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

    /// Управление демоном (Пауза / Стоп)
    #[arg(long)]
    off: bool,
}

const CONFIG_FILE: &str = "portal_config.json";
const PAUSE_FILE: &str = "/tmp/portal.pause"; // Файл-маркер паузы
const GROUP_NAME: &str = "portal-admins";
const DOAS_CONF: &str = "/etc/doas.conf";
const SUDOERS_FILE: &str = "/etc/sudoers.d/portal-daemon";

fn main() {
    let args = Args::parse();

    // 1. Управление (флаг --off)
    if args.off {
        run_control_menu();
        return;
    }

    // 2. Установка прав
    if args.install {
        run_system_install();
        return;
    }

    // 3. Загрузка/Создание конфига
    let config = if args.configure || !Path::new(CONFIG_FILE).exists() {
        run_interactive_wizard()
    } else {
        load_config()
    };

    // 4. Запуск Демона
    run_daemon(config);
}

// === МЕНЮ УПРАВЛЕНИЯ (--off) ===
fn run_control_menu() {
    println!("\n🎮 --- УПРАВЛЕНИЕ PORTAL DAEMON ---");

    let selections = &[
        "⏸  Поставить на ПАУЗУ (не спать определенное время)",
        "▶️  Снять с паузы (продолжить работу)",
        "🛑  ПОЛНОСТЬЮ остановить демон (Kill)",
        "❌  Выход",
    ];

    let selection = Select::with_theme(&ColorfulTheme::default())
        .with_prompt("Что нужно сделать?")
        .default(0)
        .items(&selections[..])
        .interact()
        .unwrap();

    match selection {
        0 => {
            // Пауза
            let minutes: u64 = Input::with_theme(&ColorfulTheme::default())
                .with_prompt("На сколько МИНУТ отключить режим сна?")
                .default(60)
                .interact_text()
                .unwrap();

            // Записываем время окончания паузы в файл
            let end_time = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_secs()
                + (minutes * 60);

            fs::write(PAUSE_FILE, end_time.to_string()).expect("Не удалось создать файл паузы");
            println!("✅ Демон поставлен на паузу на {} минут.", minutes);
        }
        1 => {
            // Снять с паузы
            if Path::new(PAUSE_FILE).exists() {
                fs::remove_file(PAUSE_FILE).expect("Не удалось удалить файл паузы");
                println!("✅ Пауза отменена. Демон снова следит за светом.");
            } else {
                println!("ℹ️  Пауза и так не была активна.");
            }
        }
        2 => {
            // Kill
            println!("💀 Пытаюсь убить процесс portal_daemon...");
            // pkill -f ищет по имени процесса. ВАЖНО: убивает и текущий процесс, но он и так выходит.
            // Используем exclude текущего PID, чтобы не было ошибки, но pkill проще.
            let status = Command::new("pkill").args(["-f", "portal_daemon"]).status();

            match status {
                Ok(_) => println!("✅ Сигнал отправлен."),
                Err(e) => eprintln!("❌ Ошибка при вызове pkill: {}", e),
            }
            // Чистим файл паузы, если был
            if Path::new(PAUSE_FILE).exists() {
                fs::remove_file(PAUSE_FILE).ok();
            }
        }
        _ => {}
    }
}

// === МАСТЕР НАСТРОЙКИ ===
fn run_interactive_wizard() -> PortalConfig {
    println!("\n🔧 --- МАСТЕР НАСТРОЙКИ PORTAL ---");

    let mut final_ip = String::new();
    let mut final_ssid = "Manual".to_string();

    println!("🔍 Сканирую активные подключения...");
    let networks = scan_networks();

    if networks.is_empty() {
        println!("❌ Авто-скан не нашел шлюзов.");
        final_ip = Input::with_theme(&ColorfulTheme::default())
            .with_prompt("Введи IP Маяка вручную")
            .default("192.168.1.1".into())
            .interact_text()
            .unwrap();
    } else {
        let mut options: Vec<String> = networks
            .iter()
            .map(|n| format!("{} (GW: {})", n.ssid, n.gateway))
            .collect();
        options.push("Ввести IP вручную".to_string());

        let selection = Select::with_theme(&ColorfulTheme::default())
            .with_prompt("Выбери сеть:")
            .default(0)
            .items(&options)
            .interact()
            .unwrap();

        if selection < networks.len() {
            let selected = &networks[selection];
            final_ip = selected.gateway.clone();
            final_ssid = selected.ssid.clone();
            println!("✅ Выбрана сеть: {}", final_ssid);
        } else {
            final_ip = Input::with_theme(&ColorfulTheme::default())
                .with_prompt("Введи IP Маяка")
                .interact_text()
                .unwrap();
        }
    }

    let sleep_minutes: u64 = Input::with_theme(&ColorfulTheme::default())
        .with_prompt("Сколько МИНУТ спать без света?")
        .default(60)
        .interact_text()
        .unwrap();

    let grace_period_sec: u64 = Input::with_theme(&ColorfulTheme::default())
        .with_prompt("Грейс-период (сек) перед сном?")
        .default(300)
        .interact_text()
        .unwrap();

    // НОВОЕ ПОЛЕ
    let wakeup_wait_sec: u64 = Input::with_theme(&ColorfulTheme::default())
        .with_prompt("Сколько сек. ждать ПОСЛЕ включения (чтобы сеть поднялась)?")
        .default(30)
        .interact_text()
        .unwrap();

    let config = PortalConfig {
        lighthouse_ip: final_ip,
        target_ssid: final_ssid,
        sleep_minutes,
        grace_period_sec,
        wakeup_wait_sec,
    };

    let json = serde_json::to_string_pretty(&config).expect("Fail json");
    fs::write(CONFIG_FILE, json).expect("Fail write");
    println!("✅ Настройки сохранены!\n");
    config
}

// === ЛОГИКА ДЕМОНА ===
fn run_daemon(cfg: PortalConfig) {
    let sleep_seconds = cfg.sleep_minutes * 60;
    println!("👻 Portal Daemon: START");
    println!("📡 Сеть: {}", cfg.target_ssid);
    println!("🎯 Маяк: {}", cfg.lighthouse_ip);

    loop {
        // 1. Проверка ПАУЗЫ
        if check_pause() {
            // Если пауза активна, просто ждем минуту и не пингуем
            thread::sleep(Duration::from_secs(60));
            continue;
        }

        // 2. Основная работа
        if check_ping(&cfg.lighthouse_ip) {
            thread::sleep(Duration::from_secs(60));
        } else {
            println!("⚠️  Потеря связи. Ждем {} сек...", cfg.grace_period_sec);
            thread::sleep(Duration::from_secs(cfg.grace_period_sec));

            // Повторная проверка паузы перед контрольным выстрелом
            if check_pause() {
                continue;
            }

            if check_ping(&cfg.lighthouse_ip) {
                println!("✅ Связь вернулась.");
            } else {
                println!("🌑 Света нет. Сон {} мин.", cfg.sleep_minutes);

                enter_hibernation(sleep_seconds);

                // ПРОБУЖДЕНИЕ
                println!(
                    "☀️  Проснулись. Ждем {} сек (настройка)...",
                    cfg.wakeup_wait_sec
                );
                thread::sleep(Duration::from_secs(cfg.wakeup_wait_sec));
            }
        }
    }
}

// Проверка файла паузы
fn check_pause() -> bool {
    if Path::new(PAUSE_FILE).exists() {
        // Читаем время окончания
        if let Ok(content) = fs::read_to_string(PAUSE_FILE) {
            if let Ok(end_time) = content.trim().parse::<u64>() {
                let now = SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .unwrap()
                    .as_secs();
                if now < end_time {
                    let left = (end_time - now) / 60;
                    // Чтобы не спамить логами каждую минуту, выводим только если запускаем в консоли
                    // println!("⏸  ПАУЗА АКТИВНА. Осталось {} мин.", left);
                    return true;
                } else {
                    println!("▶️  Время паузы истекло. Возвращаемся к работе.");
                    fs::remove_file(PAUSE_FILE).ok();
                    return false;
                }
            }
        }
        // Если файл битый, удаляем его
        fs::remove_file(PAUSE_FILE).ok();
    }
    false
}

// Остальные функции без изменений...
fn scan_networks() -> Vec<NetworkInfo> {
    let mut results = Vec::new();
    let output = Command::new("nmcli")
        .args(["-t", "-f", "NAME,DEVICE", "connection", "show", "--active"])
        .output()
        .ok();
    if let Some(out) = output {
        let stdout = String::from_utf8_lossy(&out.stdout);
        for line in stdout.lines() {
            let parts: Vec<&str> = line.split(':').collect();
            if parts.len() >= 2 {
                let ssid = parts[0].to_string();
                let device = parts[1].to_string();
                if device == "lo" || ssid.is_empty() {
                    continue;
                }
                if let Some(gw) = get_gateway_for_device(&device) {
                    results.push(NetworkInfo {
                        ssid,
                        device,
                        gateway: gw,
                    });
                }
            }
        }
    }
    results
}

fn get_gateway_for_device(dev: &str) -> Option<String> {
    let output = Command::new("nmcli")
        .args(["-t", "dev", "show", dev])
        .output()
        .ok()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        if line.starts_with("IP4.GATEWAY:") {
            let parts: Vec<&str> = line.split(':').collect();
            if parts.len() >= 2 {
                let gw = parts[1].trim().to_string();
                if !gw.is_empty() && gw != "--" {
                    return Some(gw);
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

fn load_config() -> PortalConfig {
    let data = fs::read_to_string(CONFIG_FILE).expect("Config fail");
    serde_json::from_str(&data).expect("Json fail")
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
    if let Err(e) = Command::new(priv_cmd)
        .args(["rtcwake", "-m", "mem", "-s", &seconds.to_string()])
        .status()
    {
        eprintln!("❌ Ошибка сна: {}", e);
        thread::sleep(Duration::from_secs(60));
    }
}

fn run_system_install() {
    let out = Command::new("id").arg("-u").output().unwrap();
    if String::from_utf8_lossy(&out.stdout).trim() != "0" {
        eprintln!("Need root!");
        std::process::exit(1);
    }
    let rtc = find_binary("rtcwake").expect("No rtcwake");
    let net = find_binary("nmcli").expect("No nmcli");
    Command::new("groupadd")
        .arg("-f")
        .arg(GROUP_NAME)
        .status()
        .unwrap();
    if let Some(u) = env::var("SUDO_USER").ok().or(env::var("DOAS_USER").ok()) {
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
    println!("🎉 Done.");
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
    let r1 = format!("permit nopass :{} cmd {}", GROUP_NAME, rtc);
    let r2 = format!("permit nopass :{} cmd {}", GROUP_NAME, net);
    let mut c = fs::read_to_string(DOAS_CONF).unwrap_or_default();
    if !c.contains(&r1) || !c.contains(&r2) {
        fs::copy(DOAS_CONF, format!("{}.bak", DOAS_CONF)).ok();
    }
    if !c.contains(&r1) {
        c.push_str(&format!("\n{}\n", r1));
    }
    if !c.contains(&r2) {
        c.push_str(&format!("{}\n", r2));
    }
    fs::write(DOAS_CONF, c).unwrap();
}

fn setup_sudo(rtc: &str, net: &str) {
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
