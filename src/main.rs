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
    wakeup_wait_sec: u64,
    scan_interval_sec: u64,
}

impl Default for PortalConfig {
    fn default() -> Self {
        Self {
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

const CONFIG_FILE: &str = "portal_config.json";
const PAUSE_FILE: &str = "/tmp/portal.pause";
const GROUP_NAME: &str = "portal-admins";
const DOAS_CONF: &str = "/etc/doas.conf";
const SUDOERS_FILE: &str = "/etc/sudoers.d/portal-daemon";

fn main() {
    let args = Args::parse();

    if args.off {
        run_control_menu();
        return;
    }
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

// === МЕНЮ УПРАВЛЕНИЯ ===
fn run_control_menu() {
    println!("\n🎮 --- УПРАВЛЕНИЕ PORTAL DAEMON ---");
    let selections = &[
        "⏸  Поставить на ПАУЗУ",
        "▶️  Снять с паузы",
        "🛑  Kill Process",
        "❌  Выход",
    ];
    let selection = Select::with_theme(&ColorfulTheme::default())
        .with_prompt("Действие?")
        .default(0)
        .items(&selections[..])
        .interact()
        .unwrap();

    match selection {
        0 => {
            let mins: u64 = Input::with_theme(&ColorfulTheme::default())
                .with_prompt("На сколько МИНУТ?")
                .default(60)
                .interact_text()
                .unwrap();
            let end = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_secs()
                + (mins * 60);
            fs::write(PAUSE_FILE, end.to_string()).ok();
            println!("✅ Пауза активирована на {} мин.", mins);
        }
        1 => {
            fs::remove_file(PAUSE_FILE).ok();
            println!("✅ Пауза снята. Демон работает.");
        }
        2 => {
            Command::new("pkill")
                .args(["-f", "portal_daemon"])
                .status()
                .ok();
            fs::remove_file(PAUSE_FILE).ok();
            println!("💀 Процесс остановлен.");
        }
        _ => {}
    }
}

// === МАСТЕР НАСТРОЙКИ ===
fn run_interactive_wizard() -> PortalConfig {
    println!("\n🔧 --- МАСТЕР НАСТРОЙКИ PORTAL ---");

    let mut final_ip = String::new();
    let mut final_ssid = "Manual".to_string();

    println!("🔍 Сканирую сети (nmcli)...");
    let networks = scan_networks();

    if networks.is_empty() {
        println!("❌ Сети не найдены или вывод nmcli пуст.");
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

        let sel = Select::with_theme(&ColorfulTheme::default())
            .with_prompt("Выбери сеть:")
            .default(0)
            .items(&options)
            .interact()
            .unwrap();
        if sel < networks.len() {
            final_ip = networks[sel].gateway.clone();
            final_ssid = networks[sel].ssid.clone();
            println!("✅ Выбрана сеть: {} -> Target IP: {}", final_ssid, final_ip);
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
    let wakeup_wait_sec: u64 = Input::with_theme(&ColorfulTheme::default())
        .with_prompt("Ждать сек. после включения?")
        .default(30)
        .interact_text()
        .unwrap();
    let scan_interval_sec: u64 = Input::with_theme(&ColorfulTheme::default())
        .with_prompt("Интервал проверки (сек)?")
        .default(60)
        .interact_text()
        .unwrap();

    let config = PortalConfig {
        lighthouse_ip: final_ip,
        target_ssid: final_ssid,
        sleep_minutes,
        grace_period_sec,
        wakeup_wait_sec,
        scan_interval_sec,
    };

    let json = serde_json::to_string_pretty(&config).expect("Fail json");
    fs::write(CONFIG_FILE, json).expect("Fail write");
    println!("✅ Настройки сохранены в {}\n", CONFIG_FILE);
    config
}

// === ДЕМОН ===
fn run_daemon(cfg: PortalConfig) {
    let sleep_seconds = cfg.sleep_minutes * 60;
    println!("👻 Portal Daemon: START");
    println!("📡 Сеть: {}", cfg.target_ssid);
    println!("⏱ Интервал: {} сек", cfg.scan_interval_sec);

    loop {
        if check_pause() {
            thread::sleep(Duration::from_secs(cfg.scan_interval_sec));
            continue;
        }

        if check_ping(&cfg.lighthouse_ip) {
            thread::sleep(Duration::from_secs(cfg.scan_interval_sec));
        } else {
            println!("⚠️  Потеря связи. Ждем {} сек...", cfg.grace_period_sec);
            thread::sleep(Duration::from_secs(cfg.grace_period_sec));
            if check_pause() {
                continue;
            }

            if check_ping(&cfg.lighthouse_ip) {
                println!("✅ Связь вернулась.");
            } else {
                println!("🌑 Света нет. Сон {} мин.", cfg.sleep_minutes);
                enter_hibernation(sleep_seconds);
                println!("☀️  Проснулись. Ждем {} сек...", cfg.wakeup_wait_sec);
                thread::sleep(Duration::from_secs(cfg.wakeup_wait_sec));
            }
        }
    }
}

// === УТИЛИТЫ ===
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

fn load_config() -> PortalConfig {
    let d = fs::read_to_string(CONFIG_FILE).expect("Config fail");
    serde_json::from_str(&d).expect("Json fail")
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

    let success = match status_result {
        Ok(s) if s.success() => {
            println!("✅ Уснули успешно.");
            true
        }
        Ok(_) => {
            eprintln!("❌ Ошибка: rtcwake. Требуется пароль? Проверь права!");
            false
        }
        Err(e) => {
            eprintln!("❌ Ошибка запуска команды: {}", e);
            false
        }
    };
    if !success {
        thread::sleep(Duration::from_secs(60));
    }
}

// === УСТАНОВКА СИСТЕМНЫХ ПРАВ (ТЕПЕРЬ ПОДРОБНАЯ) ===
fn run_system_install() {
    println!("🚀 Начало настройки системных прав...");

    // 1. Проверка ROOT
    let out = Command::new("id").arg("-u").output().unwrap();
    if String::from_utf8_lossy(&out.stdout).trim() != "0" {
        eprintln!("❌ Ошибка: Установщик должен быть запущен от root (sudo/doas).");
        std::process::exit(1);
    }

    // 2. Поиск утилит
    println!("🔎 Ищем системные утилиты...");
    let rtc = find_binary("rtcwake").expect("❌ rtcwake не найден! Установите util-linux.");
    let net = find_binary("nmcli").expect("❌ nmcli не найден! Установите networkmanager.");
    println!("   ✅ rtcwake найден по пути: {}", rtc);
    println!("   ✅ nmcli найден по пути:   {}", net);

    // 3. Создание группы
    println!("👤 Проверка группы {}...", GROUP_NAME);
    let g_status = Command::new("groupadd")
        .arg("-f")
        .arg(GROUP_NAME)
        .status()
        .unwrap();
    if g_status.success() {
        println!("   ✅ Группа существует или была создана.");
    } else {
        eprintln!("   ❌ Не удалось создать группу!");
    }

    // 4. Добавление пользователя
    if let Some(u) = env::var("SUDO_USER").ok().or(env::var("DOAS_USER").ok()) {
        println!("👤 Добавляем пользователя '{}' в группу...", u);
        let u_status = Command::new("usermod")
            .args(["-aG", GROUP_NAME, &u])
            .status()
            .unwrap();
        if u_status.success() {
            println!("   ✅ Пользователь добавлен.");
        } else {
            eprintln!("   ❌ Ошибка при добавлении пользователя.");
        }
    } else {
        println!("⚠️  Не удалось определить реального пользователя (SUDO_USER/DOAS_USER пуст).");
    }

    // 5. Обновление конфигов (Sudo или Doas)
    if Path::new(DOAS_CONF).exists() {
        setup_doas(&rtc, &net);
    } else {
        setup_sudo(&rtc, &net);
    }

    println!(
        "\n🎉 Установка завершена. \n⚠️  ВАЖНО: Перелогиньтесь или перезагрузите сервер, чтобы группа применилась!"
    );
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
    println!("🦅 Обнаружен Doas. Проверяем {}...", DOAS_CONF);

    let r1 = format!("permit nopass :{} cmd {}", GROUP_NAME, rtc);
    let r2 = format!("permit nopass :{} cmd {}", GROUP_NAME, net);

    let mut c = fs::read_to_string(DOAS_CONF).unwrap_or_default();
    let mut changed = false;

    if !c.contains(&r1) {
        println!("   ➕ Добавляю правило: {}", r1);
        c.push_str(&format!("\n{}\n", r1));
        changed = true;
    } else {
        println!("   ✅ Правило для rtcwake уже есть.");
    }

    if !c.contains(&r2) {
        println!("   ➕ Добавляю правило: {}", r2);
        c.push_str(&format!("{}\n", r2));
        changed = true;
    } else {
        println!("   ✅ Правило для nmcli уже есть.");
    }

    if changed {
        let backup = format!("{}.bak", DOAS_CONF);
        println!("📦 Создаю бэкап: {}", backup);
        fs::copy(DOAS_CONF, &backup).ok();

        fs::write(DOAS_CONF, c).unwrap();
        println!("📝 Конфигурация Doas успешно обновлена.");
    } else {
        println!("ℹ️  Изменения не требуются.");
    }
}

fn setup_sudo(rtc: &str, net: &str) {
    println!("🐧 Обнаружен Sudo. Генерируем правила...");
    let r = format!("%{} ALL=(root) NOPASSWD: {}, {}\n", GROUP_NAME, rtc, net);
    println!("   📄 Содержимое правила:\n{}", r.trim());

    let t = "/tmp/portal_check";
    fs::write(t, r).unwrap();

    println!("⚙️  Проверка синтаксиса (visudo)...");
    if Command::new("visudo")
        .args(["-c", "-f", t])
        .status()
        .unwrap()
        .success()
    {
        fs::set_permissions(t, fs::Permissions::from_mode(0o440)).unwrap();
        Command::new("mv").args([t, SUDOERS_FILE]).status().unwrap();
        println!("✅ Правила успешно записаны в {}", SUDOERS_FILE);
    } else {
        eprintln!("❌ Ошибка валидации! Файл не был применен.");
    }
}
