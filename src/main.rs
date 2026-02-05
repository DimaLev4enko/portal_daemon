use clap::Parser;
use serde::{Deserialize, Serialize};
use std::env;
use std::fs;
use std::io::{self, Write};
use std::path::Path;
use std::process::Command;
use std::thread;
use std::time::Duration;
use std::os::unix::fs::PermissionsExt;

// --- КОНФИГУРАЦИЯ (JSON) ---
#[derive(Serialize, Deserialize, Debug)]
struct PortalConfig {
    lighthouse_ip: String,
    sleep_minutes: u64,
    grace_period_sec: u64,
}

// Значения по умолчанию
impl Default for PortalConfig {
    fn default() -> Self {
        Self {
            lighthouse_ip: "192.168.1.1".to_string(),
            sleep_minutes: 60,
            grace_period_sec: 300,
        }
    }
}

// --- АРГУМЕНТЫ ЗАПУСКА ---
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Настроить права доступа (root/doas setup)
    #[arg(long)]
    install: bool,

    /// Изменить настройки (IP, Таймеры)
    #[arg(long)]
    configure: bool,
}

const CONFIG_FILE: &str = "portal_config.json";
const GROUP_NAME: &str = "portal-admins";
const DOAS_CONF: &str = "/etc/doas.conf";
const SUDOERS_FILE: &str = "/etc/sudoers.d/portal-daemon";

fn main() {
    let args = Args::parse();

    // 1. Если просят установить системные права
    if args.install {
        run_system_install();
        return;
    }

    // 2. Загружаем конфиг. Если его нет или просят перенастроить — запускаем визард.
    let config = if args.configure || !Path::new(CONFIG_FILE).exists() {
        run_interactive_wizard()
    } else {
        load_config()
    };

    // 3. Запускаем Демона
    run_daemon(config);
}

// === ИНТЕРАКТИВНАЯ НАСТРОЙКА ===
fn run_interactive_wizard() -> PortalConfig {
    println!("\n🔧 --- МАСТЕР НАСТРОЙКИ PORTAL ---");
    println!("Давай настроим параметры выживания.\n");

    let ip = prompt("1. Введи IP Маяка (роутер/удлинитель) [по умолчанию 192.168.1.1]: ");
    let ip = if ip.is_empty() { "192.168.1.1".to_string() } else { ip };

    let sleep_str = prompt("2. На сколько МИНУТ уходить в сон, если света нет? [по умолчанию 60]: ");
    let sleep_minutes: u64 = sleep_str.parse().unwrap_or(60);

    let grace_str = prompt("3. Грейс-период (сек) перед сном (защита от мигания) [по умолчанию 300]: ");
    let grace_period_sec: u64 = grace_str.parse().unwrap_or(300);

    let config = PortalConfig {
        lighthouse_ip: ip,
        sleep_minutes,
        grace_period_sec,
    };

    // Сохраняем в JSON
    let json = serde_json::to_string_pretty(&config).expect("Ошибка сериализации");
    fs::write(CONFIG_FILE, json).expect("Не удалось сохранить конфиг");
    
    println!("✅ Настройки сохранены в файл: {}", CONFIG_FILE);
    println!("----------------------------------\n");
    
    config
}

fn prompt(text: &str) -> String {
    print!("{}", text);
    io::stdout().flush().unwrap();
    let mut buffer = String::new();
    io::stdin().read_line(&mut buffer).unwrap();
    buffer.trim().to_string()
}

fn load_config() -> PortalConfig {
    let data = fs::read_to_string(CONFIG_FILE).expect("Не могу прочитать файл конфига");
    serde_json::from_str(&data).expect("Ошибка формата конфига")
}

// === ЛОГИКА ДЕМОНА ===
fn run_daemon(cfg: PortalConfig) {
    let sleep_seconds = cfg.sleep_minutes * 60;
    
    println!("👻 Portal Daemon: WATCHER запущен.");
    println!("🎯 Цель: {}", cfg.lighthouse_ip);
    println!("⏱ Сон: {} мин | Грейс: {} сек", cfg.sleep_minutes, cfg.grace_period_sec);

    loop {
        if check_ping(&cfg.lighthouse_ip) {
            // Свет есть — проверяем раз в минуту
            thread::sleep(Duration::from_secs(60)); 
        } else {
            println!("⚠️  Маяк потерян! Ждем {} сек...", cfg.grace_period_sec);
            thread::sleep(Duration::from_secs(cfg.grace_period_sec));

            if check_ping(&cfg.lighthouse_ip) {
                println!("✅ Маяк вернулся. Работаем дальше.");
            } else {
                println!("🌑 Света нет. Сон на {} минут.", cfg.sleep_minutes);
                enter_hibernation(sleep_seconds);
                println!("☀️  Проснулись. Ждем сеть 10 сек...");
                thread::sleep(Duration::from_secs(10));
            }
        }
    }
}

fn check_ping(ip: &str) -> bool {
    let status = Command::new("ping")
        .args(["-c", "1", "-W", "2", ip])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    match status {
        Ok(s) => s.success(),
        Err(_) => false,
    }
}

fn enter_hibernation(seconds: u64) {
    let priv_cmd = if Path::new(DOAS_CONF).exists() { "doas" } else { "sudo" };
    
    let status = Command::new(priv_cmd)
        .args(["rtcwake", "-m", "mem", "-s", &seconds.to_string()])
        .status();

    if let Err(e) = status {
        eprintln!("❌ Ошибка сна: {}", e);
        thread::sleep(Duration::from_secs(60));
    }
}

// === СИСТЕМНАЯ УСТАНОВКА (То, что мы уже отладили) ===
fn run_system_install() {
    println!("🚀 Настройка системных прав (требуется root)...");
    
    let output = Command::new("id").arg("-u").output().expect("Fail");
    if String::from_utf8_lossy(&output.stdout).trim() != "0" {
        eprintln!("❌ Запустите с sudo/doas!"); std::process::exit(1);
    }

    let rtcwake = find_binary("rtcwake").expect("No rtcwake");
    let nmcli = find_binary("nmcli").expect("No nmcli");

    // Создаем группу
    Command::new("groupadd").arg("-f").arg(GROUP_NAME).status().unwrap();
    
    // Ищем юзера
    let real_user = match env::var("SUDO_USER") {
        Ok(u) => Some(u),
        Err(_) => env::var("DOAS_USER").ok(),
    };

    if let Some(user) = real_user {
        Command::new("usermod").args(["-aG", GROUP_NAME, &user]).status().unwrap();
        println!("✅ Юзер {} добавлен в группу.", user);
    }

    // Doas / Sudo config
    if Path::new(DOAS_CONF).exists() {
        setup_doas(&rtcwake, &nmcli);
    } else {
        setup_sudo(&rtcwake, &nmcli);
    }
    
    println!("🎉 Системная настройка завершена. Теперь запустите без sudo для настройки конфига.");
}

// Вспомогательные для установки
fn find_binary(bin: &str) -> Option<String> {
    let out = Command::new("which").arg(bin).output().ok()?;
    if out.status.success() { Some(String::from_utf8_lossy(&out.stdout).trim().to_string()) } else { None }
}

fn setup_doas(rtc: &str, net: &str) {
    let rule_rtc = format!("permit nopass :{} cmd {}", GROUP_NAME, rtc);
    let rule_net = format!("permit nopass :{} cmd {}", GROUP_NAME, net);
    let mut conf = fs::read_to_string(DOAS_CONF).unwrap_or_default();
    
    // Делаем бэкап только если меняем
    if !conf.contains(&rule_rtc) || !conf.contains(&rule_net) {
         fs::copy(DOAS_CONF, format!("{}.bak", DOAS_CONF)).ok();
    }

    if !conf.contains(&rule_rtc) { conf.push_str(&format!("\n{}\n", rule_rtc)); }
    if !conf.contains(&rule_net) { conf.push_str(&format!("{}\n", rule_net)); }
    fs::write(DOAS_CONF, conf).expect("Write fail");
    println!("✅ Doas настроен.");
}

fn setup_sudo(rtc: &str, net: &str) {
    let rule = format!("%{} ALL=(root) NOPASSWD: {}, {}\n", GROUP_NAME, rtc, net);
    let temp = "/tmp/portal_check";
    fs::write(temp, rule).unwrap();
    if Command::new("visudo").args(["-c", "-f", temp]).status().unwrap().success() {
        fs::set_permissions(temp, fs::Permissions::from_mode(0o440)).unwrap();
        Command::new("mv").args([temp, SUDOERS_FILE]).status().unwrap();
        println!("✅ Sudo настроен.");
    }
}
