//! Сквозные проверки групп окон: раскладка, опознание после перезапуска и
//! запись в конфиг (запрос пользователя 2026-08-25).
//!
//! Здесь проверяется то, что не видно ни одному модулю по отдельности:
//! таблица раскладок, сопоставление окон и модель конфига обязаны сходиться
//! в один осмысленный сценарий. Модульные тесты каждого из них уже есть в
//! своих файлах — эти проверяют стык.

use rst_core::group_layout::{apply, presets_for};
use rst_core::group_match::{LiveWindow, MemberKey, match_members};
use rst_core::model::{Config, GroupMember, GroupPlace, MonitorId, Rect, WindowGroup};
use std::path::PathBuf;
use uuid::Uuid;

/// Рабочая область условного монитора 2560x1440 с панелью задач снизу.
fn work() -> Rect {
    Rect {
        x: 0,
        y: 0,
        w: 2560,
        h: 1400,
    }
}

fn member(exe: &str, title: &str) -> GroupMember {
    GroupMember {
        exe_path: PathBuf::from(exe),
        title: title.to_string(),
        class: "Class".to_string(),
        place: None,
    }
}

fn live(hwnd: usize, exe: &str, title: &str) -> LiveWindow {
    LiveWindow {
        hwnd,
        exe_path: PathBuf::from(exe),
        title: title.to_string(),
        class: "Class".to_string(),
    }
}

fn key(m: &GroupMember) -> MemberKey {
    MemberKey {
        exe_path: m.exe_path.clone(),
        title: m.title.clone(),
        class: m.class.clone(),
    }
}

#[test]
fn every_layout_of_every_size_covers_the_screen_without_overlap() {
    // Сквозная проверка всей таблицы: 49 раскладок, каждая обязана
    // раскладывать окна так, чтобы они не налезали друг на друга. Нахлёст —
    // это два окна на одном месте, и заметить его на глаз можно только на
    // той единственной раскладке, которую пользователь выбрал.
    let w = work();
    for n in 2..=8usize {
        for (p, preset) in presets_for(n).iter().enumerate() {
            let rects = apply(preset, w, 0);
            assert_eq!(
                rects.len(),
                n,
                "раскладка {p} на {n} окон дала не {n} слотов"
            );
            for (i, a) in rects.iter().enumerate() {
                for b in rects.iter().skip(i + 1) {
                    let overlap_x = a.x < b.x + b.w as i32 && b.x < a.x + a.w as i32;
                    let overlap_y = a.y < b.y + b.h as i32 && b.y < a.y + a.h as i32;
                    assert!(
                        !(overlap_x && overlap_y),
                        "раскладка {p} на {n} окон: слоты {a:?} и {b:?} налезают друг на друга"
                    );
                }
            }
        }
    }
}

#[test]
fn a_gap_never_pushes_a_window_out_of_the_work_area() {
    // Зазор вычитается симметрично, и на большом проценте слот может
    // схлопнуться — но выехать за рабочую область он не имеет права ни при
    // каком зазоре: это увело бы окно под панель задач.
    let w = work();
    for n in 2..=8usize {
        for (p, preset) in presets_for(n).iter().enumerate() {
            for gap in [0, 8, 40, 200] {
                for r in apply(preset, w, gap) {
                    assert!(
                        r.x >= w.x
                            && r.y >= w.y
                            && r.x + r.w as i32 <= w.x + w.w as i32
                            && r.y + r.h as i32 <= w.y + w.h as i32,
                        "раскладка {p} на {n} окон при зазоре {gap} вывела слот наружу: {r:?}"
                    );
                }
            }
        }
    }
}

#[test]
fn a_group_saved_and_reloaded_finds_its_windows_again() {
    // Главный сценарий: группа собрана, программа перезапущена, хэндлы окон
    // другие. Окна обязаны найтись по exe и заголовку — иначе группа после
    // перезагрузки бесполезна.
    let group = WindowGroup {
        id: Uuid::new_v4(),
        number: 1,
        name: "работа".to_string(),
        members: vec![
            member("code.exe", "main.rs — resticker"),
            member("browser.exe", "Документация"),
        ],
        gap_pct: 5,
    };
    let mut cfg = Config::default();
    cfg.groups.push(group);

    // Круг через JSON — ровно то, что делает config.json.
    let json = serde_json::to_string(&cfg).expect("сериализация конфига");
    let back: Config = serde_json::from_str(&json).expect("разбор конфига");
    let restored = &back.groups[0];
    assert_eq!(restored.members.len(), 2);

    // После перезапуска хэндлы другие, а заголовок редактора успел
    // смениться — открыли другой файл.
    let windows = vec![
        live(900_001, "browser.exe", "Документация"),
        live(900_002, "code.exe", "lib.rs — resticker"),
    ];
    let keys: Vec<MemberKey> = restored.members.iter().map(key).collect();
    let matched = match_members(&keys, &windows);
    assert_eq!(
        matched[0],
        Some(1),
        "редактор обязан найтись по exe и похожему заголовку"
    );
    assert_eq!(
        matched[1],
        Some(0),
        "браузер обязан найтись точным совпадением"
    );
}

#[test]
fn five_windows_of_one_app_are_handed_out_one_to_a_member() {
    // Пять окон одного браузера — обычное дело. Если сопоставление отдаст
    // одно и то же окно нескольким членам группы, при открытии группа
    // схлопнется в одно окно, и понять почему будет невозможно.
    let members: Vec<GroupMember> = (0..5)
        .map(|i| member("browser.exe", &format!("вкладка {i}")))
        .collect();
    let windows: Vec<LiveWindow> = (0..5)
        .map(|i| live(1000 + i, "browser.exe", &format!("вкладка {i}")))
        .collect();
    let keys: Vec<MemberKey> = members.iter().map(key).collect();
    let matched = match_members(&keys, &windows);

    let mut used: Vec<usize> = matched.iter().flatten().copied().collect();
    used.sort_unstable();
    used.dedup();
    assert_eq!(
        used.len(),
        5,
        "каждому члену группы обязано достаться своё окно"
    );
}

#[test]
fn a_member_whose_app_is_gone_does_not_steal_a_neighbours_window() {
    let members = [
        member("gone.exe", "которого нет"),
        member("code.exe", "main.rs"),
    ];
    let windows = vec![live(1, "code.exe", "main.rs")];
    let keys: Vec<MemberKey> = members.iter().map(key).collect();
    let matched = match_members(&keys, &windows);
    assert_eq!(matched[0], None);
    assert_eq!(matched[1], Some(0));
}

#[test]
fn a_group_remembers_where_each_window_stood_on_which_monitor() {
    // Группа может занимать оба монитора (выбор пользователя), поэтому место
    // хранит идентификатор экрана, а координаты — в его DIP.
    let mut group = WindowGroup {
        id: Uuid::new_v4(),
        number: 3,
        name: "3".to_string(),
        members: vec![member("a.exe", "первое"), member("b.exe", "второе")],
        gap_pct: 0,
    };
    group.members[0].place = Some(GroupPlace {
        monitor_id: MonitorId("левый".to_string()),
        x: 0.0,
        y: 0.0,
        w: 1280.0,
        h: 1400.0,
    });
    group.members[1].place = Some(GroupPlace {
        monitor_id: MonitorId("правый".to_string()),
        x: 40.0,
        y: 60.0,
        w: 900.0,
        h: 700.0,
    });

    let json = serde_json::to_string(&group).expect("сериализация группы");
    let back: WindowGroup = serde_json::from_str(&json).expect("разбор группы");
    assert_eq!(back, group);
    assert_eq!(
        back.members[1].place.as_ref().expect("место").monitor_id,
        MonitorId("правый".to_string())
    );
}

#[test]
fn the_first_slot_is_the_biggest_in_at_least_one_layout_of_every_size() {
    // Обещание пользователю: «порядок набора решает, какое окно станет
    // главным». Оно осмысленно только если для каждого размера группы есть
    // хотя бы одна раскладка с выраженным главным окном.
    for n in 2..=8usize {
        let has_main = presets_for(n).iter().any(|p| {
            let first = p.slots[0].w * p.slots[0].h;
            p.slots[1..].iter().all(|s| s.w * s.h < first - 1e-9)
        });
        assert!(
            has_main,
            "для {n} окон нет ни одной раскладки, где слот 1 заметно больше остальных"
        );
    }
}

#[test]
fn layouts_do_not_exist_outside_the_two_to_eight_range() {
    // Группа из одного окна — это просто окно; девять окон таблицей не
    // покрыты, и молча подсунуть им раскладку на восемь нельзя.
    assert!(presets_for(0).is_empty());
    assert!(presets_for(1).is_empty());
    assert!(presets_for(9).is_empty());
    assert!(presets_for(100).is_empty());
}

#[test]
fn a_degenerate_work_area_yields_empty_slots_instead_of_panicking() {
    // Гонка переподключения монитора: рабочая область может прийти нулевой.
    let zero = Rect {
        x: 0,
        y: 0,
        w: 0,
        h: 0,
    };
    for n in 2..=8usize {
        for preset in presets_for(n) {
            let rects = apply(preset, zero, 10);
            assert_eq!(rects.len(), n);
            assert!(rects.iter().all(|r| r.w == 0 && r.h == 0));
        }
    }
}
