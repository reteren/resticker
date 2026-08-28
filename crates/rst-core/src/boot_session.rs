//! Привязка групп окон к сеансу загрузки Windows (запрос пользователя
//! 2026-08-27: «чтобы при перезапуске компьютера мои группы пропадали, а при
//! простом перезапуске программы всё оставалось»).
//!
//! Группы живут в config.json — иначе они не пережили бы перезапуск самой
//! программы, а это ровно то, что просили сохранить. Значит, стирать их
//! должен не выход из процесса, а СМЕНА СЕАНСА ЗАГРУЗКИ: конфиг несёт
//! отметку сеанса, в котором писался, и при старте отметка сверяется с
//! текущей.
//!
//! Сама отметка снимается платформенным слоем ([`rst_win32::boot_session`]),
//! здесь только сравнение — крейт платформенно-чистый.

use serde::{Deserialize, Serialize};

use crate::model::Config;

/// Отметка сеанса загрузки ОС.
///
/// Два независимых признака, потому что ни одного по отдельности не хватает:
///
/// * `shutdown_tag` — метка последнего выключения из реестра. Признак ТОЧНЫЙ
///   (в течение сеанса не меняется, при каждом выключении становится другим)
///   и, в отличие от счётчика времени работы, ловит выключение при
///   включённом «быстром запуске» Windows, где счётчик продолжает идти
///   сквозь выключение.
/// * `booted_at` — момент старта системы. Ловит случай, которого не ловит
///   метка: аварийная перезагрузка (питание, синий экран) метку не пишет,
///   потому что записать её некому.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct BootStamp {
    /// Момент старта системы, секунды Unix UTC.
    pub booted_at: i64,
    /// Метка последнего выключения, шестнадцатеричной строкой. `None` —
    /// прочитать не удалось; тогда решает один `booted_at`.
    pub shutdown_tag: Option<String>,
}

/// Допуск при сравнении моментов старта, секунды.
///
/// Момент старта вычисляется как «сейчас минус время работы», поэтому любая
/// подводка часов (NTP) слегка сдвигает вычисленное значение внутри одного и
/// того же сеанса. Допуск закрывает эту дрожь.
///
/// Десять секунд, а не минута: разница между отметками при настоящей
/// перезагрузке — это как минимум время выключения плюс время старта плюс
/// запуск самой программы, то есть заведомо десятки секунд. Более широкий
/// допуск начал бы считать быструю перезагрузку продолжением сеанса.
pub const BOOT_DRIFT_TOLERANCE_SECS: i64 = 10;

/// Один ли это сеанс загрузки.
pub fn is_same_session(saved: &BootStamp, current: &BootStamp) -> bool {
    // Точный признак сильнее приблизительного: если метки выключения обе
    // известны и различаются, система с тех пор выключалась — независимо от
    // того, что показывает счётчик времени работы.
    if let (Some(saved_tag), Some(current_tag)) = (&saved.shutdown_tag, &current.shutdown_tag) {
        if saved_tag != current_tag {
            return false;
        }
    }
    saved.booted_at.abs_diff(current.booted_at) <= BOOT_DRIFT_TOLERANCE_SECS.unsigned_abs()
}

/// Что сделано с группами при старте.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupsOnStart {
    /// Тот же сеанс загрузки — группы сохранены (перезапуск программы).
    Kept,
    /// Другой сеанс — группы забыты; сколько именно.
    Forgotten(usize),
}

/// Забыть группы, собранные в другом сеансе загрузки, и проштамповать конфиг
/// текущим сеансом.
///
/// Отметка ставится ВСЕГДА, в том числе когда групп нет: иначе группа,
/// собранная позже в этом же сеансе, оказалась бы без привязки и пережила бы
/// перезагрузку.
///
/// Конфиг без отметки (написан сборкой до этой возможности) считается чужим:
/// узнать, в каком сеансе он писался, неоткуда, а из двух ошибок «забыть
/// лишний раз» отвечает просьбе пользователя, «оставить после перезагрузки» —
/// нет.
pub fn adopt_boot_session(cfg: &mut Config, current: &BootStamp) -> GroupsOnStart {
    let same = cfg
        .boot_stamp
        .as_ref()
        .is_some_and(|saved| is_same_session(saved, current));
    cfg.boot_stamp = Some(current.clone());
    if same {
        return GroupsOnStart::Kept;
    }
    let forgotten = cfg.groups.len();
    cfg.groups.clear();
    GroupsOnStart::Forgotten(forgotten)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::WindowGroup;

    fn stamp(booted_at: i64, tag: Option<&str>) -> BootStamp {
        BootStamp {
            booted_at,
            shutdown_tag: tag.map(str::to_owned),
        }
    }

    fn cfg_with_groups(n: usize) -> Config {
        Config {
            groups: (0..n)
                .map(|i| WindowGroup {
                    number: i as u8 + 1,
                    ..WindowGroup::default()
                })
                .collect(),
            ..Config::default()
        }
    }

    #[test]
    fn restarting_the_program_keeps_the_groups() {
        // Перезапуск программы: сеанс тот же, отметка та же.
        let now = stamp(1_787_835_575, Some("fa042ebbd435dd01"));
        let mut cfg = cfg_with_groups(2);
        cfg.boot_stamp = Some(now.clone());
        assert_eq!(adopt_boot_session(&mut cfg, &now), GroupsOnStart::Kept);
        assert_eq!(cfg.groups.len(), 2);
    }

    #[test]
    fn restarting_the_computer_forgets_the_groups() {
        let mut cfg = cfg_with_groups(3);
        cfg.boot_stamp = Some(stamp(1_787_835_575, Some("fa042ebbd435dd01")));
        let after_reboot = stamp(1_787_900_000, Some("0011223344556677"));
        assert_eq!(
            adopt_boot_session(&mut cfg, &after_reboot),
            GroupsOnStart::Forgotten(3)
        );
        assert!(cfg.groups.is_empty());
        assert_eq!(cfg.boot_stamp, Some(after_reboot));
    }

    #[test]
    fn a_shutdown_the_uptime_counter_did_not_notice_still_forgets() {
        // «Быстрый запуск» Windows: счётчик времени работы продолжает идти
        // сквозь выключение, и вычисленный момент старта не меняется. Смену
        // сеанса видно только по метке выключения.
        let saved = stamp(1_787_835_575, Some("fa042ebbd435dd01"));
        let current = stamp(1_787_835_575, Some("0011223344556677"));
        assert!(!is_same_session(&saved, &current));
    }

    #[test]
    fn a_crash_reboot_that_wrote_no_shutdown_tag_still_forgets() {
        // Пропало питание: метку выключения записать было некому, она та же.
        // Смену сеанса видно по моменту старта.
        let saved = stamp(1_787_835_575, Some("fa042ebbd435dd01"));
        let current = stamp(1_787_900_000, Some("fa042ebbd435dd01"));
        assert!(!is_same_session(&saved, &current));
    }

    #[test]
    fn clock_drift_within_a_session_is_not_a_reboot() {
        let saved = stamp(1_787_835_575, Some("fa042ebbd435dd01"));
        let current = stamp(1_787_835_575 + BOOT_DRIFT_TOLERANCE_SECS, None);
        assert!(is_same_session(&saved, &current));
        let too_far = stamp(1_787_835_575 + BOOT_DRIFT_TOLERANCE_SECS + 1, None);
        assert!(!is_same_session(&saved, &too_far));
    }

    #[test]
    fn a_config_without_a_stamp_is_treated_as_another_session() {
        let mut cfg = cfg_with_groups(1);
        assert_eq!(cfg.boot_stamp, None);
        assert_eq!(
            adopt_boot_session(&mut cfg, &stamp(1_787_835_575, None)),
            GroupsOnStart::Forgotten(1)
        );
        assert!(cfg.groups.is_empty());
    }

    #[test]
    fn the_stamp_is_written_even_when_there_are_no_groups() {
        // Иначе группа, собранная позже в этом сеансе, осталась бы без
        // привязки и пережила бы перезагрузку.
        let mut cfg = Config::default();
        let now = stamp(1_787_835_575, Some("fa042ebbd435dd01"));
        assert_eq!(
            adopt_boot_session(&mut cfg, &now),
            GroupsOnStart::Forgotten(0)
        );
        assert_eq!(cfg.boot_stamp, Some(now));
    }
}
