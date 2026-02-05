use clap::Parser;
use std::env;
use std::fs;
use std::io::Write;
use std::process::Command;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Запустить режим установки (настройка прав и групп)
    #[arg(long)]
    install: bool,
}

const GROUP_NAME: &str = "portal-admins";
const SUDOERS_FILE: &str = "/etc/sudoers.d/portal-daemon";
const DOAS_CONF: &str = "/etc/doas.conf";

fn main() {
    let args = Args::parse();

    if args.install {
        run_installation();
    } else {
        run_daemon();
    }
}

fn run_installation() {
    println!("🚀 Запуск мастера установки Portal Daemon...");

    // 1. Проверка Root
    let output = Command::new("id").arg("-u").output().expect("Не удалось выполнить id");
    let uid_str = String::from_utf8_lossy(&output.stdout).trim().to_string();
    
    if uid_str != "0" {
        eprintln!("❌ Ошибка: Запустите через root (sudo/doas ./portal_daemon --install)");
        std::process::exit(1);
    }

    // 2. Определение реального пользователя (Sudo vs Doas)
    let real_user = match env::var("SUDO_USER") {
        Ok(u) => Some(u),
        Err(_) => env::var("DOAS_USER").ok(), // Пробуем найти пользователя Doas
    };

    // 3. Создание группы
    let status = Command::new("groupadd").arg("-f").arg(GROUP_NAME).status().expect("Ошибка groupadd");
    if status.success() {
        println!("✅ Группа {} проверена.", GROUP_NAME);
    }

    // 4. Добавление пользователя в группу
    if let Some(user) = real_user {
        let status = Command::new("usermod").args(["-aG", GROUP_NAME, &user]).status().expect("Ошибка usermod");
        if status.success() {
            println!("✅ Пользователь {} добавлен в группу {}.", user, GROUP_NAME);
        }
    } else {
        println!("⚠️  Не удалось определить реального пользователя. Добавьте себя в группу '{}' вручную.", GROUP_NAME);
    }

    // 5. Поиск путей к бинарникам
    let rtcwake = find_binary("rtcwake").expect("❌ rtcwake не найден!");
    let nmcli = find_binary("nmcli").expect("❌ nmcli не найден!");
    println!("✅ Утилиты найдены:\n   {}\n   {}", rtcwake, nmcli);

    // 6. ВЫБОР СТРАТЕГИИ: DOAS или SUDO
    if Path::new(DOAS_CONF).exists() {
        println!("🦅 Обнаружен Doas. Применяем конфигурацию для Gentoo/BSD style...");
        setup_doas(&rtcwake, &nmcli);
    } else if find_binary("visudo").is_some() {
        println!("🐧 Обнаружен Sudo. Применяем стандартную конфигурацию...");
        setup_sudo(&rtcwake, &nmcli);
    } else {
        eprintln!("❌ Не найдено ни sudo (visudo), ни doas.conf. Не могу настроить права.");
        std::process::exit(1);
    }
}

// --- ЛОГИКА DOAS ---
fn setup_doas(rtcwake: &str, nmcli: &str) {
    // В Doas нет директории .d (обычно), пишем в основной файл, но делаем бэкап.
    let backup_path = format!("{}.bak", DOAS_CONF);
    fs::copy(DOAS_CONF, &backup_path).expect("Не удалось создать бэкап doas.conf");
    println!("📦 Создан бэкап конфигурации: {}", backup_path);

    // Читаем текущий конфиг, чтобы не дублировать строки
    let current_conf = fs::read_to_string(DOAS_CONF).unwrap_or_default();
    
    // Формируем правила. Синтаксис: permit nopass :group cmd /path/to/bin
    // Важно: Doas требует отдельные строки для каждой команды (обычно)
    let rule_rtc = format!("permit nopass :{} cmd {}", GROUP_NAME, rtcwake);
    let rule_net = format!("permit nopass :{} cmd {}", GROUP_NAME, nmcli);

    let mut new_conf = current_conf.clone();
    let mut changed = false;

    if !new_conf.contains(&rule_rtc) {
        new_conf.push_str(&format!("\n{}\n", rule_rtc));
        changed = true;
    }
    if !new_conf.contains(&rule_net) {
        new_conf.push_str(&format!("{}\n", rule_net));
        changed = true;
    }

    if changed {
        // Проверяем конфиг перед записью (doas -C conf_file)
        let temp_file = "/tmp/doas_check.conf";
        fs::write(temp_file, &new_conf).expect("Ошибка записи врем. файла");

        let check = Command::new("doas").args(["-C", temp_file]).status();
        
        // doas -C может не быть на старых версиях, но если есть - проверим
        if check.is_ok() && !check.unwrap().success() {
             eprintln!("❌ Ошибка валидации doas.conf! Отмена.");
             return;
        }

        fs::write(DOAS_CONF, new_conf).expect("Ошибка записи doas.conf");
        println!("✅ Правила успешно добавлены в {}", DOAS_CONF);
    } else {
        println!("ℹ️  Правила для Doas уже существуют.");
    }
}

// --- ЛОГИКА SUDO ---
fn setup_sudo(rtcwake: &str, nmcli: &str) {
    let rule = format!(
        "%{} ALL=(root) NOPASSWD: {}, {}\n",
        GROUP_NAME, rtcwake, nmcli
    );

    let temp_file = "/tmp/portal_sudoers_check";
    fs::write(temp_file, rule).expect("Ошибка записи");

    let check = Command::new("visudo").args(["-c", "-f", temp_file]).output().expect("Ошибка visudo");

    if check.status.success() {
        fs::set_permissions(temp_file, fs::Permissions::from_mode(0o440)).unwrap();
        Command::new("mv").args([temp_file, SUDOERS_FILE]).status().expect("Ошибка mv");
        println!("✅ Правила Sudo успешно применены.");
    } else {
        eprintln!("❌ Ошибка валидации sudoers!");
    }
}

fn find_binary(bin_name: &str) -> Option<String> {
    let output = Command::new("which").arg(bin_name).output().ok()?;
    if output.status.success() {
        let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if path.is_empty() { None } else { Some(path) }
    } else {
        None
    }
}

fn run_daemon() {
    println!("👻 Portal Daemon запущен...");
    // Тут код проверки маяка
}
