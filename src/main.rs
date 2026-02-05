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

// --- КОНФИГУРАЦИЯ ---
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

// --- АРГУМЕНТЫ ---
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

    if args.install {
        run_system_install();
        return;
    }

    // Если конфига нет или просят --configure — запускаем мастер
    let config = if args.configure || !Path::new(CONFIG_FILE).exists() {
        run_interactive_wizard()
    } else {
        load_config()
    };

    run_daemon(config);
}

// === МАСТЕР НАСТРОЙКИ (WIZARD) ===
fn run_interactive_wizard() -> PortalConfig {
    println!("\n🔧 --- МАСТЕР НАСТРОЙКИ PORTAL ---");
    
    // ШАГ 1: Выбор IP
    let mut ip = String::new();
    println!("Выбери способ поиска Маяка (устройства, которое работает от розетки):");
    println!(" [1] Автоматически найти Роутер (Шлюз)");
    println!(" [2] Ввести IP вручную");
    
    let choice = prompt("Твой выбор [1/2]: ");
    
    if choice.trim() == "1" {
        if let Some(gateway) = get_default_gateway() {
            println!("✅ Нашел шлюз: {}", gateway);
            let confirm = prompt("Использовать этот IP? [Y/n]: ");
            if confirm.trim().eq_ignore_ascii_case("n") {
                 ip = prompt("Тогда введи IP вручную: ");
            } else {
                 ip = gateway;
            }
        } else {
            println!("❌ Не удалось найти шлюз автоматически.");
            ip = prompt("Введи IP вручную: ");
        }
    } else {
        ip = prompt("Введи IP Маяка (например, 192.168.1.1): ");
    }
    
    // Если пользователь просто нажал Enter, ставим дефолт
    if ip.trim().is_empty() { ip = "192.168.1.1".to_string(); }

    // ШАГ 2: Время сна
    let sleep_str = prompt("\nНа сколько МИНУТ засыпать при отключении света? [60]: ");
    let sleep_minutes: u64 = sleep_str.parse().unwrap_or(60);

    // ШАГ 3: Задержка (Grace Period)
    println!("\nВведите 'Задержку перед сном' (в секундах).");
    println!("Это время сервер будет ждать после потери связи, вдруг свет просто мигнул.");
    let grace_str = prompt("Сколько ждать? [300 сек = 5 мин]: ");
    let grace_period_sec: u64 = grace_str.parse().unwrap_or(300);

    let config = PortalConfig {
        lighthouse_ip: ip,
        sleep_minutes,
        grace_period_sec,
    };

    // Сохранение
    let json = serde_json::to_string_pretty(&config).expect("Ошибка создания JSON");
    fs::write(CONFIG_FILE, json).expect("Ошибка записи файла");
    
    println!("✅ Настройки сохранены!");
    println!("----------------------------------\n");
    
    config
}

// Попытка найти Default Gateway через команду 'ip route'
fn get_default_gateway() -> Option<String> {
    let output = Command::new("ip")
        .args(["route", "show", "default"])
        .output()
        .ok()?;
        
    if !output.status.success() { return None; }
    
    let out_str = String::from_utf8_lossy(&output.stdout);
    // Вывод выглядит примерно так: "default via 192.168.1.1 dev enp3s0 ..."
    // Нам нужно слово после "via"
    
    let parts: Vec<&str> = out_str.split_whitespace().collect();
    for (i, part) in parts.iter().enumerate() {
        if *part == "via" && i + 1 < parts.len() {
            return Some(parts[i+1].to_string());
        }
    }
    None
}

fn prompt(text: &str) -> String {
    print!("{}", text);
    io::stdout().flush().unwrap();
    let mut buffer = String::new();
    io::stdin().read_line(&mut buffer).unwrap();
    buffer.trim().to_string()
}

fn load_config() -> PortalConfig {
    let data = fs::read_to_string(CONFIG_FILE).expect("Ошибка: Файл конфига поврежден или удален.");
    serde_json::from_str(&data).expect("Ошибка: Неверный формат JSON.")
}

// === ДЕМОН ===
fn run_daemon(cfg: PortalConfig) {
    let sleep_seconds = cfg.sleep_minutes * 60;
    
    println!("👻 Portal Daemon: АВТОНОМНЫЙ РЕЖИМ");
    println!("🎯 Цель (Маяк): {}", cfg.lighthouse_ip);
    println!("⏱ Если света нет: Ждем {} сек, потом спим {} мин.", cfg.grace_period_sec, cfg.sleep_minutes);

    loop {
        if check_ping(&cfg.lighthouse_ip) {
            // Всё ок, спим минуту и проверяем снова
            thread::sleep(Duration::from_secs(60)); 
        } else {
            println!("⚠️  Маяк потерян! Ждем {} сек (проверка на мигание)...", cfg.grace_period_sec);
            thread::sleep(Duration::from_secs(cfg.grace_period_sec));

            // Контрольная проверка
            if check_ping(&cfg.lighthouse_ip) {
                println!("✅ Маяк вернулся. Ложная тревога. Работаем.");
            } else {
                println!("🌑 Света точно нет. Уходим в СОН на {} минут.", cfg.sleep_minutes);
                
                // --- ГИБЕРНАЦИЯ ---
                enter_hibernation(sleep_seconds);
                
                // --- ПРОБУЖДЕНИЕ ---
                println!("☀️  Проснулись. Даем сети 15 сек на поднятие...");
                thread::sleep(Duration::from_secs(15));
            }
        }
    }
}

fn check_ping(ip: &str) -> bool {
    let status = Command::new("ping")
        .args(["-c", "1", "-W", "2", ip]) // 1 пакет, 2 сек таймаут
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
    
    // Пытаемся уснуть
    let status = Command::new(priv_cmd)
        .args(["rtcwake", "-m", "mem", "-s", &seconds.to_string()])
        .status();

    if let Err(e) = status {
        eprintln!("❌ Ошибка сна: {}", e);
        // Если сон не сработал, ждем минуту, чтобы не спамить в лог
        thread::sleep(Duration::from_secs(60));
    }
}

// === СИСТЕМНАЯ УСТАНОВКА ===
fn run_system_install() {
    println!("🚀 Настройка системных прав...");
    
    // Проверка Root
    let output = Command::new("id").arg("-u").output().expect("Fail");
    if String::from_utf8_lossy(&output.stdout).trim() != "0" {
        eprintln!("❌ Запустите с sudo или doas!"); std::process::exit(1);
    }

    let rtcwake = find_binary("rtcwake").expect("rtcwake не найден");
    let nmcli = find_binary("nmcli").expect("nmcli не найден");

    // Создаем группу
    Command::new("groupadd").arg("-f").arg(GROUP_NAME).status().unwrap();
    
    // Ищем пользователя
    let real_user = match env::var("SUDO_USER") {
        Ok(u) => Some(u),
        Err(_) => env::var("DOAS_USER").ok(),
    };

    if let Some(user) = real_user {
        Command::new("usermod").args(["-aG", GROUP_NAME, &user]).status().unwrap();
        println!("✅ Юзер {} добавлен в группу {}.", user, GROUP_NAME);
    }

    // Настройка конфигов
    if Path::new(DOAS_CONF).exists() {
        setup_doas(&rtcwake, &nmcli);
    } else {
        setup_sudo(&rtcwake, &nmcli);
    }
    
    println!("🎉 Готово. Теперь запустите программу без sudo для настройки параметров.");
}

fn find_binary(bin: &str) -> Option<String> {
    let out = Command::new("which").arg(bin).output().ok()?;
    if out.status.success() { Some(String::from_utf8_lossy(&out.stdout).trim().to_string()) } else { None }
}

fn setup_doas(rtc: &str, net: &str) {
    let rule_rtc = format!("permit nopass :{} cmd {}", GROUP_NAME, rtc);
    let rule_net = format!("permit nopass :{} cmd {}", GROUP_NAME, net);
    let mut conf = fs::read_to_string(DOAS_CONF).unwrap_or_default();
    
    if !conf.contains(&rule_rtc) || !conf.contains(&rule_net) {
         fs::copy(DOAS_CONF, format!("{}.bak", DOAS_CONF)).ok();
    }
    if !conf.contains(&rule_rtc) { conf.push_str(&format!("\n{}\n", rule_rtc)); }
    if !conf.contains(&rule_net) { conf.push_str(&format!("{}\n", rule_net)); }
    fs::write(DOAS_CONF, conf).expect("Write fail");
    println!("✅ Doas конфиг обновлен.");
}

fn setup_sudo(rtc: &str, net: &str) {
    let rule = format!("%{} ALL=(root) NOPASSWD: {}, {}\n", GROUP_NAME, rtc, net);
    let temp = "/tmp/portal_check";
    fs::write(temp, rule).unwrap();
    if Command::new("visudo").args(["-c", "-f", temp]).status().unwrap().success() {
        fs::set_permissions(temp, fs::Permissions::from_mode(0o440)).unwrap();
        Command::new("mv").args([temp, SUDOERS_FILE]).status().unwrap();
        println!("✅ Sudo конфиг обновлен.");
    }
}
