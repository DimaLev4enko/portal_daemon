use clap::Parser;
use dialoguer::{Input, Select, theme::ColorfulTheme}; // Подключаем библиотеку меню
use serde::{Deserialize, Serialize};
use std::env;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;
use std::thread;
use std::time::Duration;

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

// === МАСТЕР НАСТРОЙКИ (TUI) ===
fn run_interactive_wizard() -> PortalConfig {
    println!("\n🔧 --- МАСТЕР НАСТРОЙКИ PORTAL ---");

    // Меню выбора метода
    let selections = &[
        "Ввести IP вручную (Рекомендуется)",
        "Найти шлюз автоматически (через nmcli)",
    ];

    let selection = Select::with_theme(&ColorfulTheme::default())
        .with_prompt("Как будем искать Маяк (Удлинитель/Роутер)?")
        .default(0) // По умолчанию - первый пункт (Ручной)
        .items(&selections[..])
        .interact()
        .unwrap();

    let ip: String;

    if selection == 0 {
        // Ручной ввод
        ip = Input::with_theme(&ColorfulTheme::default())
            .with_prompt("Введи IP адрес Маяка")
            .default("192.168.1.1".into())
            .interact_text()
            .unwrap();
    } else {
        // Автоматика
        println!("🔍 Сканирую сеть через nmcli...");
        if let Some(gateway) = get_default_gateway() {
            println!("✅ Найден шлюз: {}", gateway);

            // Спрашиваем подтверждение
            let confirm_selections = &["Да, использовать этот IP", "Нет, ввести другой вручную"];
            let confirm = Select::with_theme(&ColorfulTheme::default())
                .with_prompt("Использовать этот IP?")
                .default(0)
                .items(&confirm_selections[..])
                .interact()
                .unwrap();

            if confirm == 0 {
                ip = gateway;
            } else {
                ip = Input::with_theme(&ColorfulTheme::default())
                    .with_prompt("Введи IP адрес вручную")
                    .interact_text()
                    .unwrap();
            }
        } else {
            println!("❌ Шлюз не найден.");
            ip = Input::with_theme(&ColorfulTheme::default())
                .with_prompt("Введи IP адрес вручную")
                .interact_text()
                .unwrap();
        }
    }

    // Ввод времени сна
    let sleep_minutes: u64 = Input::with_theme(&ColorfulTheme::default())
        .with_prompt("Сколько МИНУТ спать без света?")
        .default(60)
        .interact_text()
        .unwrap();

    // Ввод грейс-периода
    let grace_period_sec: u64 = Input::with_theme(&ColorfulTheme::default())
        .with_prompt("Грейс-период (сек) перед сном?")
        .default(300)
        .interact_text()
        .unwrap();

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

// --- ФУНКЦИИ ---

fn get_default_gateway() -> Option<String> {
    let output = Command::new("nmcli").args(["dev", "show"]).output().ok()?;

    if !output.status.success() {
        return None;
    }

    let out_str = String::from_utf8_lossy(&output.stdout);
    for line in out_str.lines() {
        if line.contains("IP4.GATEWAY") {
            if let Some(value) = line.split_whitespace().last() {
                if value != "--" && !value.is_empty() {
                    return Some(value.to_string());
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
    let priv_cmd = if Path::new(DOAS_CONF).exists() {
        "doas"
    } else {
        "sudo"
    };
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
    let user = env::var("SUDO_USER").ok().or(env::var("DOAS_USER").ok());
    if let Some(u) = user {
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
    let out = Command::new("which").arg(bin).output().ok()?;
    if out.status.success() {
        Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        None
    }
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
