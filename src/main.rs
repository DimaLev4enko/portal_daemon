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
    #[arg(long)]
    install: bool,
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

    let config = if args.configure || !Path::new(CONFIG_FILE).exists() {
        run_interactive_wizard()
    } else {
        load_config()
    };

    run_daemon(config);
}

// === МАСТЕР НАСТРОЙКИ ===
fn run_interactive_wizard() -> PortalConfig {
    println!("\n🔧 --- МАСТЕР НАСТРОЙКИ PORTAL ---");
    
    // ШАГ 1: IP Маяка
    let mut ip = String::new();
    println!("Выбери способ поиска Маяка:");
    println!(" [1] Найти Шлюз (через NetworkManager)");
    println!(" [2] Ввести IP вручную");
    
    let choice = prompt("Твой выбор [1/2]: ");
    
    if choice.trim() == "1" {
        println!("🔍 Сканирую через nmcli...");
        if let Some(gateway) = get_default_gateway() {
            println!("✅ NetworkManager нашел шлюз: {}", gateway);
            let confirm = prompt("Использовать этот IP? [Y/n]: ");
            if confirm.trim().eq_ignore_ascii_case("n") {
                 ip = prompt("Введи IP вручную: ");
            } else {
                 ip = gateway;
            }
        } else {
            println!("❌ nmcli не вернул шлюз (или сеть не поднята).");
            ip = prompt("Введи IP вручную: ");
        }
    } else {
        ip = prompt("Введи IP Маяка (например, 192.168.1.1): ");
    }
    
    if ip.trim().is_empty() { ip = "192.168.1.1".to_string(); }

    // ШАГ 2 и 3
    let sleep_str = prompt("\nНа сколько МИНУТ засыпать? [60]: ");
    let sleep_minutes: u64 = sleep_str.parse().unwrap_or(60);

    let grace_str = prompt("Грейс-период (сек) перед сном? [300]: ");
    let grace_period_sec: u64 = grace_str.parse().unwrap_or(300);

    let config = PortalConfig {
        lighthouse_ip: ip,
        sleep_minutes,
        grace_period_sec,
    };

    let json = serde_json::to_string_pretty(&config).expect("Fail json");
    fs::write(CONFIG_FILE, json).expect("Fail write");
    println!("✅ Настройки сохранены!\n");
    config
}

// --- НОВАЯ ЛОГИКА ПОИСКА (NMCLI) ---
fn get_default_gateway() -> Option<String> {
    // Выполняем: nmcli dev show
    let output = Command::new("nmcli")
        .args(["dev", "show"])
        .output()
        .ok()?;
        
    if !output.status.success() { return None; }
    
    let out_str = String::from_utf8_lossy(&output.stdout);
    
    // Ищем строку вида: "IP4.GATEWAY: 192.168.1.1"
    for line in out_str.lines() {
        if line.contains("IP4.GATEWAY") {
            // Разбиваем строку по пробелам и берем последнее значение
            if let Some(value) = line.split_whitespace().last() {
                // nmcli иногда пишет "--", если шлюза нет
                if value != "--" && !value.is_empty() {
                    return Some(value.to_string());
                }
            }
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
    let data = fs::read_to_string(CONFIG_FILE).expect("Config fail");
    serde_json::from_str(&data).expect("Json fail")
}

// === ДЕМОН ===
fn run_daemon(cfg: PortalConfig) {
    let sleep_seconds = cfg.sleep_minutes * 60;
    println!("👻 Portal Daemon: START");
    println!("🎯 Маяк: {}", cfg.lighthouse_ip);

    loop {
        if check_ping(&cfg.lighthouse_ip) {
            thread::sleep(Duration::from_secs(60)); 
        } else {
            println!("⚠️  Связь потеряна. Ждем {} сек...", cfg.grace_period_sec);
            thread::sleep(Duration::from_secs(cfg.grace_period_sec));

            if check_ping(&cfg.lighthouse_ip) {
                println!("✅ Связь восстановлена.");
            } else {
                println!("🌑 Света нет. Сон {} мин.", cfg.sleep_minutes);
                enter_hibernation(sleep_seconds);
                println!("☀️  Проснулись. Ждем сеть 15 сек...");
                thread::sleep(Duration::from_secs(15));
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

// === SYSTEM INSTALL ===
fn run_system_install() {
    println!("🚀 Setup permissions...");
    let out = Command::new("id").arg("-u").output().unwrap();
    if String::from_utf8_lossy(&out.stdout).trim() != "0" {
        eprintln!("Need root!"); std::process::exit(1);
    }

    let rtc = find_binary("rtcwake").expect("No rtcwake");
    let net = find_binary("nmcli").expect("No nmcli");

    Command::new("groupadd").arg("-f").arg(GROUP_NAME).status().unwrap();
    
    let user = env::var("SUDO_USER").ok().or(env::var("DOAS_USER").ok());
    if let Some(u) = user {
        Command::new("usermod").args(["-aG", GROUP_NAME, &u]).status().unwrap();
    }

    if Path::new(DOAS_CONF).exists() { setup_doas(&rtc, &net); } 
    else { setup_sudo(&rtc, &net); }
    println!("🎉 Done.");
}

fn find_binary(bin: &str) -> Option<String> {
    let out = Command::new("which").arg(bin).output().ok()?;
    if out.status.success() { Some(String::from_utf8_lossy(&out.stdout).trim().to_string()) } else { None }
}

fn setup_doas(rtc: &str, net: &str) {
    let r1 = format!("permit nopass :{} cmd {}", GROUP_NAME, rtc);
    let r2 = format!("permit nopass :{} cmd {}", GROUP_NAME, net);
    let mut c = fs::read_to_string(DOAS_CONF).unwrap_or_default();
    if !c.contains(&r1) || !c.contains(&r2) { fs::copy(DOAS_CONF, format!("{}.bak", DOAS_CONF)).ok(); }
    if !c.contains(&r1) { c.push_str(&format!("\n{}\n", r1)); }
    if !c.contains(&r2) { c.push_str(&format!("{}\n", r2)); }
    fs::write(DOAS_CONF, c).unwrap();
}

fn setup_sudo(rtc: &str, net: &str) {
    let r = format!("%{} ALL=(root) NOPASSWD: {}, {}\n", GROUP_NAME, rtc, net);
    let t = "/tmp/portal_check";
    fs::write(t, r).unwrap();
    if Command::new("visudo").args(["-c", "-f", t]).status().unwrap().success() {
        fs::set_permissions(t, fs::Permissions::from_mode(0o440)).unwrap();
        Command::new("mv").args([t, SUDOERS_FILE]).status().unwrap();
    }
}
