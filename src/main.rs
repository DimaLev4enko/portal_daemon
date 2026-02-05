use clap::Parser;
use dialoguer::{theme::ColorfulTheme, Input, Select};
use serde::{Deserialize, Serialize};
use std::env;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::thread;
use std::time::Duration;
use std::os::unix::fs::PermissionsExt;

// --- КОНФИГУРАЦИЯ ---
#[derive(Serialize, Deserialize, Debug)]
struct PortalConfig {
    lighthouse_ip: String,
    target_ssid: String,
    sleep_minutes: u64,
    grace_period_sec: u64,
}

impl Default for PortalConfig {
    fn default() -> Self {
        Self {
            lighthouse_ip: "192.168.1.1".to_string(),
            target_ssid: "Unknown".to_string(),
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

struct NetworkInfo {
    ssid: String,
    device: String,
    gateway: String,
}

// === МАСТЕР НАСТРОЙКИ ===
fn run_interactive_wizard() -> PortalConfig {
    println!("\n🔧 --- МАСТЕР НАСТРОЙКИ PORTAL ---");

    let mut final_ip = String::new();
    let mut final_ssid = "Manual".to_string();

    println!("🔍 Сканирую активные подключения...");
    let networks = scan_networks();

    if networks.is_empty() {
        println!("❌ Авто-скан не нашел шлюзов. Возможно, сеть не настроена или nmcli выдает нестандартный вывод.");
        final_ip = Input::with_theme(&ColorfulTheme::default())
            .with_prompt("Введи IP Маяка (шлюза) вручную")
            .default("192.168.1.1".into())
            .interact_text()
            .unwrap();
    } else {
        let mut options: Vec<String> = networks.iter()
            .map(|n| format!("{} (Dev: {}, GW: {})", n.ssid, n.device, n.gateway))
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

    let config = PortalConfig {
        lighthouse_ip: final_ip,
        target_ssid: final_ssid,
        sleep_minutes,
        grace_period_sec,
    };

    let json = serde_json::to_string_pretty(&config).expect("Fail json");
    fs::write(CONFIG_FILE, json).expect("Fail write");
    println!("✅ Настройки сохранены!\n");
    config
}

// --- НОВАЯ ЛОГИКА СКАНИРОВАНИЯ ---
fn scan_networks() -> Vec<NetworkInfo> {
    let mut results = Vec::new();

    // 1. Получаем список [ИМЯ]:[УСТРОЙСТВО]
    // Твой вывод показал: lox_2.4G:wlp3s0
    let output = Command::new("nmcli")
        .args(["-t", "-f", "NAME,DEVICE", "connection", "show", "--active"])
        .output()
        .ok();

    if let Some(out) = output {
        let stdout = String::from_utf8_lossy(&out.stdout);
        for line in stdout.lines() {
            let parts: Vec<&str> = line.split(':').collect();
            // parts[0] = lox_2.4G, parts[1] = wlp3s0
            if parts.len() >= 2 {
                let ssid = parts[0].to_string();
                let device = parts[1].to_string();

                // Игнорируем loopback (lo) и устройства без имени
                if device == "lo" || ssid.is_empty() { continue; }

                // 2. Ищем шлюз для этого конкретного устройства
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
    // Мы убрали флаг "-f", чтобы не злить твой nmcli.
    // Просто берем ВСЮ инфу: nmcli -t dev show wlp3s0
    let output = Command::new("nmcli")
        .args(["-t", "dev", "show", dev])
        .output()
        .ok()?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    
    // Ищем строку, которая начинается с IP4.GATEWAY
    for line in stdout.lines() {
        if line.starts_with("IP4.GATEWAY:") {
            // Строка выглядит так: "IP4.GATEWAY:192.168.1.1"
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

fn load_config() -> PortalConfig {
    let data = fs::read_to_string(CONFIG_FILE).expect("Config fail");
    serde_json::from_str(&data).expect("Json fail")
}

fn run_daemon(cfg: PortalConfig) {
    let sleep_seconds = cfg.sleep_minutes * 60;
    println!("👻 Portal Daemon: START");
    println!("📡 Сеть: {}", cfg.target_ssid);
    println!("🎯 Маяк: {}", cfg.lighthouse_ip);

    loop {
        if check_ping(&cfg.lighthouse_ip) {
            thread::sleep(Duration::from_secs(60)); 
        } else {
            println!("⚠️  Потеря связи. Ждем {} сек...", cfg.grace_period_sec);
            thread::sleep(Duration::from_secs(cfg.grace_period_sec));

            if check_ping(&cfg.lighthouse_ip) {
                println!("✅ Связь вернулась.");
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
