//! Опознание окон группы после перезапуска (модель [`crate::model::WindowGroup`],
//! члены — [`crate::model::GroupMember`]). Группа живёт в config.json и
//! обязана пережить перезагрузку, а `HWND` после неё другой — поэтому член
//! группы хранит не хэндл, а приметы окна: путь к exe, заголовок, класс.
//! Платформенно-чистый модуль (CONTRIBUTING.md, «Правило зависимостей»):
//! живые окна приходят уже переведёнными в [`LiveWindow`] координатором
//! (`rst_win32::window_enum::WindowInfo` этому крейту не виден).
//!
//! Задача сопоставления — решить, какое живое окно какому члену группы
//! принадлежит. Заголовки меняются на лету (браузер, редактор пишут туда
//! имя документа), поэтому точное совпадение — не единственное правило,
//! но и не последнее: слабые правила способны украсть окно у сильных, а
//! одно живое окно не может достаться двум членам группы. Отсюда жадное
//! присваивание по убыванию силы совпадения (см. [`match_members`]).

use std::path::{Path, PathBuf};

/// Что запомнили о члене группы при её создании — приметы для опознания
/// после перезапуска. Поля повторяют [`crate::model::GroupMember`] без
/// `place`: место окна к опознанию отношения не имеет, а зависимость от
/// модели здесь не нужна — это чистый вход сопоставления, который может
/// собрать и Win32-слой, не зная про раскладки.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberKey {
    /// Полный путь к exe процесса-владельца — главная примета окна.
    pub exe_path: PathBuf,
    /// Заголовок на момент запоминания. Сравнивается нестрого: правило 2
    /// допускает, что заголовок сменился, пока exe и класс на месте.
    pub title: String,
    /// Класс окна: у одного процесса отличает главное окно от
    /// вспомогательных, а меняется он куда реже заголовка.
    pub class: String,
}

/// Живое окно из перечисления Win32-слоя — кандидат на опознание.
/// `hwnd` — числовой ключ, как в `rst_win32::window_enum::WindowInfo`;
/// после сопоставления координатор управляет окном именно по нему.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveWindow {
    pub hwnd: usize,
    pub exe_path: PathBuf,
    pub title: String,
    pub class: String,
}

/// Порог похожести заголовков для правила 2 (см. [`match_members`]).
///
/// 0.5 — «в запомненном заголовке уцелела хотя бы половина»: у двух разных
/// окон одного приложения почти всегда есть общий «хвост» с именем
/// приложения (« — Google Chrome»), и чем ниже порог, тем охотнее член
/// группы крадёт чужое окно. Выше порога перестаёт опознаваться обычный
/// сценарий перезапуска: браузер добавляет имя вкладки, редактор — имя
/// документа, и стабильная часть заголовка как раз порядка половины.
pub const TITLE_SIMILARITY_THRESHOLD: f64 = 0.5;

/// Сопоставить запомненные приметы членов группы с живыми окнами: для
/// каждого члена — индекс его окна в `live` или `None`.
///
/// Три правила по убыванию силы:
/// 1. совпали exe, заголовок и класс — точное совпадение;
/// 2. совпали exe и класс, заголовок похож ([`title_similarity`] ≥
///    [`TITLE_SIMILARITY_THRESHOLD`]) — заголовки меняются на лету;
/// 3. совпали только exe и класс — берём, если такой кандидат ровно один.
///
/// Главный инвариант — взаимная однозначность: одно живое окно не может
/// достаться двум членам группы. Пять окон одного браузера обязаны уйти
/// пяти членам, а не все одному. Поэтому это жадное присваивание по
/// убыванию силы совпадения: сначала раздаются все точные совпадения
/// (член за членом в порядке `members`, окно — первое свободное в порядке
/// `live`), затем похожие (пары «член × окно» обрабатываются по убыванию
/// похожести — слабую, но единственную пару не может украсть сильная пара
/// другого члена), и каждое отданное окно вычёркивается из кандидатов для
/// всех следующих.
///
/// Правило 3 — самая слабая ставка и применяется последним: кандидат
/// должен остаться единственным среди НЕотданных окон, иначе два члена
/// одного приложения не смогут угадать, кто чей, и любое присваивание
/// было бы гаданием.
///
/// Пустые входы — корректный `Vec` из `None` (без паники). Порядок окна
/// внутри `live` — тай-брейк: при равной силе побеждает более ранний.
pub fn match_members(members: &[MemberKey], live: &[LiveWindow]) -> Vec<Option<usize>> {
    let mut assigned = vec![None; members.len()];
    let mut taken = vec![false; live.len()];

    // Правило 1: точные совпадения — самая сильная ставка, раздаётся
    // первой и вся целиком, чтобы слабые правила не отобрали у неё окна.
    for (m, member) in members.iter().enumerate() {
        for (w, window) in live.iter().enumerate() {
            if !taken[w] && exact_match(member, window) {
                assigned[m] = Some(w);
                taken[w] = true;
                break;
            }
        }
    }

    // Правило 2: похожие заголовки. Сначала собираются все пары
    // «свободный член × свободное окно», затем отдаются по убыванию
    // похожести. Сортировка важнее, чем кажется: без неё первым членам
    // достались бы их «средние» пары, а у последних не осталось бы
    // единственно возможных — порядок отдачи должен следовать силе.
    let mut similar: Vec<(usize, usize, f64)> = Vec::new();
    for (m, member) in members.iter().enumerate() {
        if assigned[m].is_some() {
            continue;
        }
        for (w, window) in live.iter().enumerate() {
            if taken[w] || !exe_class_match(member, window) {
                continue;
            }
            let sim = title_similarity(&member.title, &window.title);
            if sim >= TITLE_SIMILARITY_THRESHOLD {
                similar.push((m, w, sim));
            }
        }
    }
    similar
        .sort_by(|(ma, wa, sa), (mb, wb, sb)| sb.total_cmp(sa).then(ma.cmp(mb)).then(wa.cmp(wb)));
    for (m, w, _) in similar {
        if assigned[m].is_none() && !taken[w] {
            assigned[m] = Some(w);
            taken[w] = true;
        }
    }

    // Правило 3: exe + класс без заголовка. Берём, только если среди
    // НЕотданных окон такой кандидат ровно один — при двух и более
    // угадать, кто чей, нельзя, и окно остаётся неопознанным.
    for (m, member) in members.iter().enumerate() {
        if assigned[m].is_some() {
            continue;
        }
        let mut sole: Option<usize> = None;
        let mut ambiguous = false;
        for (w, window) in live.iter().enumerate() {
            if !taken[w] && exe_class_match(member, window) {
                if sole.is_some() {
                    ambiguous = true;
                    break;
                }
                sole = Some(w);
            }
        }
        if let (Some(w), false) = (sole, ambiguous) {
            assigned[m] = Some(w);
            taken[w] = true;
        }
    }

    assigned
}

/// Точное совпадение (правило 1): exe, заголовок и класс — все три равны
/// без учёта регистра.
fn exact_match(member: &MemberKey, window: &LiveWindow) -> bool {
    exe_class_match(member, window) && titles_equal(&member.title, &window.title)
}

/// Совпадение exe и класса — основа правил 2 и 3.
fn exe_class_match(member: &MemberKey, window: &LiveWindow) -> bool {
    paths_equal(&member.exe_path, &window.exe_path) && classes_equal(&member.class, &window.class)
}

fn titles_equal(a: &str, b: &str) -> bool {
    a.to_lowercase() == b.to_lowercase()
}

/// Регистронезависимое равенство классов окон. Классы в Win32 формально
/// регистрозависимы (`RegisterClass`), но реальные классы приложений
/// (`Chrome_WidgetWin_1`, `Notepad`) пишутся стабильно, а двух классов
/// одного приложения, различающихся только регистром, не бывает — строгое
/// сравнение дало бы ложный промах после перезапуска, не дав ничего взамен.
fn classes_equal(a: &str, b: &str) -> bool {
    a.to_lowercase() == b.to_lowercase()
}

/// Регистронезависимое равенство путей к exe: файловые системы Windows
/// регистронезависимы, и конфиг мог сохранить путь в одном регистре,
/// а перечисление отдать в другом. Нормализация (`\\?\`-префиксы, 8.3-имена,
/// `..`) — вне скоупа: оба пути приходят из одного источника и совпадают
/// как есть.
///
/// Пустой путь не матчится никогда: пустой `exe_path` у живого окна — это
/// protected process, чей путь не виден (`rst_win32::window_enum`), и такое
/// окно нельзя безопасно отличить от другого такого же — все они слились
/// бы в одного члена группы.
fn paths_equal(a: &Path, b: &Path) -> bool {
    let a = a.to_string_lossy();
    let b = b.to_string_lossy();
    !a.is_empty() && !b.is_empty() && a.to_lowercase() == b.to_lowercase()
}

/// Похожесть двух заголовков: доля более короткого заголовка, сохранившаяся
/// в обоих как общий префикс ИЛИ общий суффикс, 0.0..=1.0.
///
/// Делим на длину БОЛЕЕ КОРОТКОГО, а не более длинного: браузер после
/// перезапуска дописывает к заголовку имя вкладки («Google Chrome» →
/// «Stack Overflow — Google Chrome»), и доля от длинного заголовка упала бы
/// до ~0.4 — ниже любого разумного порога, то есть правило 2 умерло бы в
/// самом частом сценарии. Доля от короткого — «сколько от запомненного
/// заголовка уцелело», и тот же случай даёт ровно 1.0.
///
/// Только префикс или суффикс, не произвольная подстрока: имя документа
/// вставляется приложениями с краю («имя — приложение» или «приложение —
/// имя»), середина же меняется целиком. Сравнение регистронезависимое:
/// приложения сами не стабильны в регистре («chrome — Google Chrome» против
/// «Chrome — Google Chrome»).
pub fn title_similarity(remembered: &str, live: &str) -> f64 {
    let a: Vec<char> = remembered.to_lowercase().chars().collect();
    let b: Vec<char> = live.to_lowercase().chars().collect();
    let min_len = a.len().min(b.len());
    if min_len == 0 {
        return 0.0;
    }
    let prefix = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let suffix = a
        .iter()
        .rev()
        .zip(b.iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    prefix.max(suffix) as f64 / min_len as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(exe: &str, title: &str, class: &str) -> MemberKey {
        MemberKey {
            exe_path: PathBuf::from(exe),
            title: title.to_string(),
            class: class.to_string(),
        }
    }

    fn live(hwnd: usize, exe: &str, title: &str, class: &str) -> LiveWindow {
        LiveWindow {
            hwnd,
            exe_path: PathBuf::from(exe),
            title: title.to_string(),
            class: class.to_string(),
        }
    }

    const CHROME: &str = r"C:\apps\chrome.exe";
    const NOTEPAD: &str = r"C:\apps\notepad.exe";
    const CHROME_CLASS: &str = "Chrome_WidgetWin_1";
    const NOTEPAD_CLASS: &str = "Notepad";

    // --- title_similarity ---

    #[test]
    fn title_similarity_counts_common_suffix() {
        // «Google Chrome» целиком сохранился суффиксом — похожесть 1.0,
        // хотя впереди дописано имя вкладки (браузер после перезапуска).
        assert_eq!(
            title_similarity("Google Chrome", "Stack Overflow - Google Chrome"),
            1.0
        );
    }

    #[test]
    fn title_similarity_counts_common_prefix() {
        // «Untitled - Notepad» → «Untitled - Paint»: уцелел общий префикс,
        // суффикс ничего не дал. 11 общих символов из 16 у более короткого
        // заголовка (делим на длину более короткого, см. доккомент
        // [`title_similarity`]).
        assert_eq!(
            title_similarity("Untitled - Notepad", "Untitled - Paint"),
            11.0 / 16.0
        );
    }

    #[test]
    fn title_similarity_without_common_edge_is_zero() {
        assert_eq!(title_similarity("aaa", "bbb"), 0.0);
    }

    #[test]
    fn title_similarity_empty_side_is_zero() {
        assert_eq!(title_similarity("", "Notepad"), 0.0);
        assert_eq!(title_similarity("Notepad", ""), 0.0);
        assert_eq!(title_similarity("", ""), 0.0);
    }

    #[test]
    fn title_similarity_is_case_insensitive() {
        // Регистр в заголовке не стабилен даже у одного окна — сравнение
        // не должно от этого зависеть.
        assert_eq!(
            title_similarity("Google Chrome", "googLe CHROME - Stack Overflow"),
            1.0
        );
    }

    // --- match_members: правила по отдельности ---

    #[test]
    fn exact_match_returns_the_window_index() {
        let members = [member(CHROME, "Google Chrome", CHROME_CLASS)];
        let windows = [live(0x100, CHROME, "Google Chrome", CHROME_CLASS)];
        assert_eq!(match_members(&members, &windows), vec![Some(0)]);
    }

    #[test]
    fn changed_title_still_finds_window_via_similarity() {
        // Браузер пережил перезапуск с восстановленными вкладками: в
        // заголовок дописалось имя страницы, «хвост» с именем приложения
        // уцелел — окно опознаётся правилом 2.
        let members = [member(CHROME, "Google Chrome", CHROME_CLASS)];
        let windows = [live(
            0x100,
            CHROME,
            "Stack Overflow - Google Chrome",
            CHROME_CLASS,
        )];
        assert_eq!(match_members(&members, &windows), vec![Some(0)]);
    }

    #[test]
    fn exe_and_class_only_match_when_exactly_one_candidate() {
        // Единственное живое окно приложения, заголовок сменился целиком
        // (похожести нет) — окно опознаётся правилом 3.
        let members = [member(r"C:\apps\calc.exe", "Расчёт", "CalcFrame")];
        let windows = [live(
            0x400,
            r"C:\apps\calc.exe",
            "Инженерный калькулятор",
            "CalcFrame",
        )];
        assert_eq!(match_members(&members, &windows), vec![Some(0)]);
    }

    #[test]
    fn exe_and_class_only_is_refused_when_candidates_are_many() {
        // Два живых окна приложения, заголовки не похожи ни на запомненный,
        // ни друг на друга: угадать, кто чей, нельзя — член получает None,
        // а не произвольное окно.
        let members = [member(r"C:\apps\calc.exe", "Расчёт", "CalcFrame")];
        let windows = [
            live(0x401, r"C:\apps\calc.exe", "Конвертер валют", "CalcFrame"),
            live(0x402, r"C:\apps\calc.exe", "Графики", "CalcFrame"),
        ];
        assert_eq!(match_members(&members, &windows), vec![None]);
    }

    #[test]
    fn empty_titles_match_exactly_when_both_empty() {
        // Служебные окна без заголовка опознаются точным совпадением;
        // похожесть при этом 0, но она и не нужна — правило 1 сработало.
        let members = [member(r"C:\apps\panel.exe", "", "WorkerW")];
        let windows = [live(0x800, r"C:\apps\panel.exe", "", "WorkerW")];
        assert_eq!(match_members(&members, &windows), vec![Some(0)]);
    }

    // --- match_members: взаимная однозначность ---

    #[test]
    fn five_windows_of_one_exe_are_assigned_one_to_one() {
        // Пять вкладок одного браузера — пять членов группы: каждое окно
        // обязано уйти ровно одному члену. Порядок живых окон перемешан
        // относительно членов — соответствие должно найтись, а не
        // «повезти» с порядком.
        let members = [
            member(CHROME, "Page A - Google Chrome", CHROME_CLASS),
            member(CHROME, "Page B - Google Chrome", CHROME_CLASS),
            member(CHROME, "Page C - Google Chrome", CHROME_CLASS),
            member(CHROME, "Page D - Google Chrome", CHROME_CLASS),
            member(CHROME, "Page E - Google Chrome", CHROME_CLASS),
        ];
        let windows = [
            live(1, CHROME, "Page C - Google Chrome", CHROME_CLASS),
            live(2, CHROME, "Page A - Google Chrome", CHROME_CLASS),
            live(3, CHROME, "Page E - Google Chrome", CHROME_CLASS),
            live(4, CHROME, "Page B - Google Chrome", CHROME_CLASS),
            live(5, CHROME, "Page D - Google Chrome", CHROME_CLASS),
        ];
        let result = match_members(&members, &windows);
        // Каждому члену досталось окно, и все индексы попарно различны.
        let got: Vec<usize> = result
            .iter()
            .map(|r| r.expect("каждому члену — своё окно"))
            .collect();
        let mut sorted = got.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted, vec![0, 1, 2, 3, 4], "окна не должны повторяться");
        assert_eq!(got.len(), sorted.len(), "окна не должны повторяться");
    }

    #[test]
    fn same_window_is_never_assigned_to_two_members() {
        // Два члена с одинаковыми приметами (задвоенный член группы) и
        // одно окно: окно уходит первому, второй получает None — инвариант
        // взаимной однозначности держится и в вырожденном случае.
        let members = [
            member(NOTEPAD, "todo.txt - Notepad", NOTEPAD_CLASS),
            member(NOTEPAD, "todo.txt - Notepad", NOTEPAD_CLASS),
        ];
        let windows = [live(0x600, NOTEPAD, "todo.txt - Notepad", NOTEPAD_CLASS)];
        assert_eq!(match_members(&members, &windows), vec![Some(0), None]);
    }

    #[test]
    fn similarity_prefers_the_stronger_match() {
        // Оба окна похожи на запомненный заголовок, но одно — сильнее
        // («Notepad» сохранился целиком против общего «хвоста» с именем
        // приложения): жадное присваивание по убыванию силы отдаёт его.
        let members = [member(NOTEPAD, "Untitled - Notepad", NOTEPAD_CLASS)];
        let windows = [
            live(0x701, NOTEPAD, "резюме.txt - Notepad", NOTEPAD_CLASS),
            live(0x702, NOTEPAD, "Notepad", NOTEPAD_CLASS),
        ];
        assert_eq!(match_members(&members, &windows), vec![Some(1)]);
    }

    #[test]
    fn exact_match_wins_over_similar_window() {
        // Член A узнаёт своё окно точно; второе живое окно тоже похоже на
        // A по «хвосту», но это окно члена B. Правило 1 раздаётся раньше
        // правила 2, поэтому A не крадёт похожее окно у B.
        let members = [
            member(NOTEPAD, "todo.txt - Notepad", NOTEPAD_CLASS),
            member(NOTEPAD, "notes.txt - Notepad", NOTEPAD_CLASS),
        ];
        let windows = [
            live(0x501, NOTEPAD, "todo.txt - Notepad", NOTEPAD_CLASS),
            live(0x502, NOTEPAD, "учебник.txt - Notepad", NOTEPAD_CLASS),
        ];
        assert_eq!(match_members(&members, &windows), vec![Some(0), Some(1)]);
    }

    #[test]
    fn closed_window_yields_none_not_a_foreign_window() {
        // Окно члена A закрыто; живёт только окно члена B. A не должен
        // увести его: после того как B забрал своё окно (точным или
        // похожим совпадением), для A среди неотданных кандидатов не
        // осталось ни одного.
        let members = [
            member(NOTEPAD, "resume.txt - Notepad", NOTEPAD_CLASS),
            member(NOTEPAD, "todo.txt - Notepad", NOTEPAD_CLASS),
        ];
        let windows = [live(0x200, NOTEPAD, "todo.txt - Notepad", NOTEPAD_CLASS)];
        assert_eq!(match_members(&members, &windows), vec![None, Some(0)]);
    }

    #[test]
    fn member_without_candidate_gets_none_and_does_not_steal() {
        // Член A потерял своё окно, но его exe и класс совпадают с окном
        // члена B, а заголовок B сменился (похож на B, но не на A). A не
        // должен украсть окно B: пары обрабатываются по убыванию похожести,
        // и слабая пара A никогда не отберёт окно у сильной пары B.
        let members = [
            member(NOTEPAD, "старая заметка.txt - Notepad", NOTEPAD_CLASS),
            member(NOTEPAD, "todo.txt - Notepad", NOTEPAD_CLASS),
        ];
        let windows = [live(
            0x300,
            NOTEPAD,
            "план на неделю - Notepad",
            NOTEPAD_CLASS,
        )];
        assert_eq!(match_members(&members, &windows), vec![None, Some(0)]);
    }

    // --- match_members: сравнение примет ---

    #[test]
    fn exe_paths_match_case_insensitively() {
        // Конфиг мог сохранить «C:\Apps\Chrome.exe», а перечисление отдать
        // «c:\apps\chrome.exe» — регистр пути не должен ломать опознание.
        let members = [member(r"C:\Apps\Chrome.exe", "Google Chrome", CHROME_CLASS)];
        let windows = [live(
            1,
            r"c:\apps\chrome.exe",
            "Google Chrome",
            CHROME_CLASS,
        )];
        assert_eq!(match_members(&members, &windows), vec![Some(0)]);
    }

    #[test]
    fn window_classes_match_case_insensitively() {
        let members = [member(CHROME, "Google Chrome", CHROME_CLASS)];
        let windows = [live(1, CHROME, "Google Chrome", "chrome_widgetwin_1")];
        assert_eq!(match_members(&members, &windows), vec![Some(0)]);
    }

    #[test]
    fn empty_exe_path_never_matches() {
        // Protected process: путь не виден (rst_win32::window_enum), пустой
        // exe-путь нельзя ни с чем сопоставлять — иначе все защищённые
        // окна слились бы в одного члена группы.
        let members = [member("", "System", "Shell_SystemTray")];
        let windows = [live(1, "", "System", "Shell_SystemTray")];
        assert_eq!(match_members(&members, &windows), vec![None]);
    }

    // --- match_members: пустые и вырожденные входы ---

    #[test]
    fn empty_inputs_do_not_panic() {
        assert_eq!(match_members(&[], &[]), Vec::<Option<usize>>::new());
        assert_eq!(
            match_members(&[], &[live(1, "a", "b", "c")]),
            Vec::<Option<usize>>::new()
        );
        let members = [member("a", "b", "c")];
        assert_eq!(match_members(&members, &[]), vec![None]);
    }

    #[test]
    fn restart_scenario_mixes_all_rules() {
        // Полный сценарий перезапуска: браузер вернулся с другой вкладкой
        // (правило 2), редактор — точь-в-точь (правило 1), калькулятор
        // одинок с новым заголовком (правило 3), а окно терминала закрыто
        // вовсе (None). Каждый член получает своё.
        let members = [
            member(CHROME, "Google Chrome", CHROME_CLASS),
            member(NOTEPAD, "todo.txt - Notepad", NOTEPAD_CLASS),
            member(r"C:\apps\calc.exe", "Расчёт", "CalcFrame"),
            member(
                r"C:\apps\wt.exe",
                "Мой терминал",
                "CASCADIA_HOSTING_WINDOW_CLASS",
            ),
        ];
        let windows = [
            live(0x10, CHROME, "Stack Overflow - Google Chrome", CHROME_CLASS),
            live(0x11, NOTEPAD, "todo.txt - Notepad", NOTEPAD_CLASS),
            live(
                0x12,
                r"C:\apps\calc.exe",
                "Инженерный калькулятор",
                "CalcFrame",
            ),
        ];
        assert_eq!(
            match_members(&members, &windows),
            vec![Some(0), Some(1), Some(2), None]
        );
    }
}
