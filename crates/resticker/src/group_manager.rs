//! Панель менеджера групп окон (запрос пользователя 2026-08-25: «кнопка в
//! режиме редактирования, по которой вылезет менеджер окон» — с прямым
//! указанием на панель «Presets» как на образец).
//!
//! Показывает список групп, а по клику по строке — её состав: какие окна
//! внутри. Оттуда же группа удаляется и переименовывается.
//!
//! Родственник [`crate::preset_picker`] намеренно: та же геометрия, тот же
//! способ отдавать нажатия наружу, тот же приём с черновиком имени. Разница
//! одна — строка группы РАСКРЫВАЕТСЯ, а не применяется: применить группу
//! можно хоткеем `Ctrl+Shift+<номер>`, а вот заглянуть внутрь больше негде.
//!
//! Строитель чистый: на вход срез групп и что раскрыто, на выход [`Panel`].
//! Состояние живёт в координаторе.

use rst_core::model::WindowGroup;
use rst_render::{
    Box2D, Button, ButtonContent, Divider, LINE_HEIGHT, Label, Panel, WidgetId, WidgetStyle,
    text_size, theme,
};

use crate::window_picker::truncate_to_width;

/// Идентификатор панели. Диапазон 900+: тулбар 0-8, панель у курсора 100+,
/// панель выбора окон 200+, пресеты 400+, лента окон 600+, лента раскладок
/// 800+. Цифра монитора занимает 902, поэтому начинаем с 910.
pub const PANEL_ID: WidgetId = 910;
/// Строка группы: `ROW_BASE + индекс` в срезе `groups`. Клик раскрывает или
/// сворачивает состав.
pub const ROW_BASE: WidgetId = 911;
/// Кнопка удаления группы: `DELETE_BASE + индекс`.
pub const DELETE_BASE: WidgetId = 940;
// Поля имени и кнопки переименования здесь НЕТ намеренно: группы только
// нумеруются (решение пользователя 2026-08-26 — «нахрена мне вообще функция
// названия групп»). Номер — это цифра хоткея `Ctrl+Shift+N`, им группу и
// зовут; произвольное имя добавляло бы второй способ называть то же самое.
/// Кнопка «закрыть панель».
pub const BTN_CLOSE: WidgetId = 972;
/// Кнопка «собрать новую группу» — открывает меню редактирования групп.
pub const BTN_NEW: WidgetId = 976;
/// Кнопка «править состав раскрытой группы» — открывает то же меню, но с
/// уже отмеченными окнами этой группы.
pub const BTN_EDIT: WidgetId = 977;
/// Заголовок панели.
const ID_TITLE: WidgetId = 973;
/// Подпись пустого списка.
const ID_EMPTY: WidgetId = 974;
/// Разделитель между списком и действиями.
const ID_DIVIDER: WidgetId = 975;
/// Первая строка состава раскрытой группы.
const MEMBER_BASE: WidgetId = 980;

/// Ширина панели, DIP. Шире, чем у пресетов: строка несёт номер, имя и
/// счётчик окон, а строки состава — заголовки чужих окон, которые обрезать
/// хочется как можно позже.
pub const WIDTH: f64 = 380.0;
/// Внутренний отступ, DIP.
const PAD: f64 = 12.0;
/// Зазор между строками, DIP.
const ROW_GAP: f64 = 4.0;
/// Зазор между блоками, DIP.
const SECTION_GAP: f64 = 10.0;
/// Высота строки и кнопок, DIP.
const ROW_H: f64 = 28.0;
/// Высота строки состава: она мельче строки группы — это подпись, а не
/// кнопка.
const MEMBER_H: f64 = 20.0;
/// Сторона кнопки удаления, DIP.
const DELETE_W: f64 = 28.0;
/// Отступ строк состава от левого края: вложенность должна читаться глазом.
const MEMBER_INDENT: f64 = 18.0;

/// Групп не больше девяти (по числу цифровых хоткеев), поэтому прокрутка
/// списку не нужна — в отличие от пресетов, которых бывают десятки.
pub const MAX_ROWS: usize = 9;
/// Окон в группе не больше восьми ([`rst_core::model::MAX_GROUP_MEMBERS`]).
const MAX_MEMBERS: usize = 8;

const TITLE_LABEL: &str = "Window groups";
const EMPTY_LABEL: &str = "No groups yet — press Alt+Shift+G to build one.";
const CLOSE_LABEL: &str = "Close";
/// Слово «группа» в подписи строки — подпись должна читаться, а не считываться.
const GROUP_WORD: &str = "Group";
const WINDOW_WORD: &str = "window";
const WINDOWS_WORD: &str = "windows";
const NEW_LABEL: &str = "New group";
const EDIT_LABEL: &str = "Edit windows";
const DELETE_MARK: &str = "x";
/// Цвет подписи кнопки удаления — тот же приглушённо-красный, что у
/// удаления пресета: одинаковое действие обязано выглядеть одинаково.
const DANGER_TEXT: [u8; 3] = [0xff, 0xb0, 0xb0];

/// Сколько строк занимает список: пустой список занимает одну строку под
/// подпись.
fn visible_rows(count: usize) -> usize {
    count.clamp(1, MAX_ROWS)
}

/// Высота панели под `count` групп, из которых одна раскрыта на
/// `expanded_members` окон.
///
/// Считается формулой из тех же отступов, что и раскладка: иначе правка
/// любого зазора разъезжается с рамкой, и заметить это можно только глазами.
pub fn height(count: usize, expanded_members: usize) -> f64 {
    let rows = visible_rows(count) as f64;
    let members = expanded_members.min(MAX_MEMBERS) as f64;
    2.0 * PAD
        + LINE_HEIGHT
        + SECTION_GAP
        + rows * ROW_H
        + (rows - 1.0) * ROW_GAP
        + members * (MEMBER_H + ROW_GAP)
        + SECTION_GAP
        + 1.0
        + SECTION_GAP
        + ROW_H
}

/// Собрать панель.
///
/// `expanded` — индекс раскрытой группы в срезе `groups` (`None` — все
/// свёрнуты). Раскрыта не больше одной: две раскрытые группы по восемь окон
/// уже не помещаются на экран, а выбирать, какую обрезать, — решение, которое
/// пользователю объяснить нечем.
pub fn build(groups: &[WindowGroup], expanded: Option<usize>, frame: Box2D) -> Panel {
    let mut panel = Panel::new(PANEL_ID, frame)
        .with_style(WidgetStyle::Settings)
        .with_corner_radius(theme::settings::CORNER_RADIUS);
    let left = frame.cx - frame.w / 2.0 + PAD;
    let right = frame.cx + frame.w / 2.0 - PAD;
    let top = frame.cy - frame.h / 2.0 + PAD;
    let content_w = right - left;

    let title_cy = top + LINE_HEIGHT / 2.0;
    panel.add_widget(Label::new(ID_TITLE, left, title_cy, TITLE_LABEL));

    let list_top = title_cy + LINE_HEIGHT / 2.0 + SECTION_GAP;
    if groups.is_empty() {
        let mut empty = Label::new(
            ID_EMPTY,
            left,
            list_top + ROW_H / 2.0,
            &truncate_to_width(EMPTY_LABEL, content_w),
        );
        empty.set_dim(true);
        panel.add_widget(empty);
    }

    // Список групп. Строки состава вставляются СРАЗУ под своей группой,
    // поэтому вертикальная позиция считается накопительно, а не по индексу:
    // раскрытая группа сдвигает всё, что ниже неё.
    let mut cy = list_top + ROW_H / 2.0;
    for (i, group) in groups.iter().take(MAX_ROWS).enumerate() {
        let row_w = content_w - DELETE_W - ROW_GAP;
        // Подпись читается словами, а не набором чисел: «Group 3 — 4 windows».
        // Раньше здесь было «3 3 (4)» — номер, имя (совпадавшее с номером) и
        // счётчик в скобках, и понять это было нельзя (репорт 2026-08-26).
        let label = format!(
            "{} {} — {} {}",
            GROUP_WORD,
            group.number,
            group.members.len(),
            if group.members.len() == 1 {
                WINDOW_WORD
            } else {
                WINDOWS_WORD
            }
        );
        panel.add_widget(
            Button::new(
                ROW_BASE + i as WidgetId,
                Box2D {
                    cx: left + row_w / 2.0,
                    cy,
                    w: row_w,
                    h: ROW_H,
                    rotation: 0.0,
                },
                ButtonContent::Label(truncate_to_width(&label, row_w - 2.0 * theme::BUTTON_PAD)),
            )
            .with_style(WidgetStyle::Settings),
        );
        panel.add_widget(
            Button::new(
                DELETE_BASE + i as WidgetId,
                Box2D {
                    cx: right - DELETE_W / 2.0,
                    cy,
                    w: DELETE_W,
                    h: ROW_H,
                    rotation: 0.0,
                },
                ButtonContent::Label(DELETE_MARK.to_string()),
            )
            .with_style(WidgetStyle::Settings)
            .with_label_color(DANGER_TEXT),
        );
        cy += ROW_H / 2.0;

        if expanded == Some(i) {
            for (m, member) in group.members.iter().take(MAX_MEMBERS).enumerate() {
                cy += ROW_GAP + MEMBER_H / 2.0;
                // Слот виден числом: он объясняет, почему окна встают именно
                // так, — первое в списке попадает в главный слот раскладки.
                let text = format!("{}. {}", m + 1, member.title);
                let mut label = Label::new(
                    MEMBER_BASE + m as WidgetId,
                    left + MEMBER_INDENT,
                    cy,
                    &truncate_to_width(&text, content_w - MEMBER_INDENT),
                );
                label.set_dim(true);
                panel.add_widget(label);
                cy += MEMBER_H / 2.0;
            }
        }
        cy += ROW_GAP + ROW_H / 2.0;
    }

    // Разделитель ставится под фактическим низом списка, а не по формуле:
    // раскрытая группа сдвигает его вниз, и второй счёт разъехался бы с
    // первым.
    let list_bottom = cy - ROW_H / 2.0 - ROW_GAP;
    let divider_cy = list_bottom + SECTION_GAP;
    panel.add_widget(Divider::new(ID_DIVIDER, frame.cx, divider_cy, content_w));

    // Нижний ряд действий — сразу под разделителем: строки переименования
    // между ними больше нет.
    let actions_cy = divider_cy + SECTION_GAP + ROW_H / 2.0;
    let new_w = text_size(NEW_LABEL).0 + 2.0 * theme::FIELD_PAD + 12.0;
    panel.add_widget(
        Button::new(
            BTN_NEW,
            Box2D {
                cx: left + new_w / 2.0,
                cy: actions_cy,
                w: new_w,
                h: ROW_H,
                rotation: 0.0,
            },
            ButtonContent::Label(NEW_LABEL.to_string()),
        )
        .with_style(WidgetStyle::Settings),
    );
    // «Править состав» есть только при раскрытой группе: без неё непонятно,
    // чей состав правим, а неактивная кнопка в этой панели ничем не
    // отличается от активной — стиль такого состояния не поддерживает.
    if expanded.is_some() {
        let edit_w = text_size(EDIT_LABEL).0 + 2.0 * theme::FIELD_PAD + 12.0;
        panel.add_widget(
            Button::new(
                BTN_EDIT,
                Box2D {
                    cx: left + new_w + ROW_GAP + edit_w / 2.0,
                    cy: actions_cy,
                    w: edit_w,
                    h: ROW_H,
                    rotation: 0.0,
                },
                ButtonContent::Label(EDIT_LABEL.to_string()),
            )
            .with_style(WidgetStyle::Settings),
        );
    }
    let close_w = text_size(CLOSE_LABEL).0 + 2.0 * theme::FIELD_PAD + 12.0;
    panel.add_widget(
        Button::new(
            BTN_CLOSE,
            Box2D {
                cx: right - close_w / 2.0,
                cy: actions_cy,
                w: close_w,
                h: ROW_H,
                rotation: 0.0,
            },
            ButtonContent::Label(CLOSE_LABEL.to_string()),
        )
        .with_style(WidgetStyle::Settings),
    );

    panel
}

#[cfg(test)]
mod tests {
    use super::*;
    use rst_core::model::{GroupMember, MAX_GROUP_MEMBERS};
    use rst_render::Widget;
    use std::path::PathBuf;
    use uuid::Uuid;

    fn member(title: &str) -> GroupMember {
        GroupMember {
            exe_path: PathBuf::from("app.exe"),
            title: title.to_string(),
            class: "Class".to_string(),
            place: None,
        }
    }

    fn group(number: u8, members: usize) -> WindowGroup {
        WindowGroup {
            id: Uuid::new_v4(),
            number,
            name: format!("группа {number}"),
            members: (0..members).map(|i| member(&format!("окно {i}"))).collect(),
            gap_pct: 5,
        }
    }

    fn frame(count: usize, expanded: usize) -> Box2D {
        Box2D {
            cx: 1280.0,
            cy: 720.0,
            w: WIDTH,
            h: height(count, expanded),
            rotation: 0.0,
        }
    }

    /// Все виджеты панели должны лежать внутри её рамки.
    fn assert_inside(panel: &Panel, f: Box2D, ids: &[WidgetId]) {
        let top = f.cy - f.h / 2.0;
        let bottom = f.cy + f.h / 2.0;
        for id in ids {
            let Some(b) = panel.widget::<Button>(*id).map(Widget::bounds) else {
                continue;
            };
            assert!(
                b.cy - b.h / 2.0 >= top - 0.5 && b.cy + b.h / 2.0 <= bottom + 0.5,
                "виджет {id} вылез за панель: {b:?} при рамке {top}..{bottom}"
            );
        }
    }

    #[test]
    fn empty_list_still_builds_a_usable_panel() {
        // Первый запуск: групп нет. Панель обязана открыться и объяснить,
        // что делать, а не показать пустоту.
        let f = frame(0, 0);
        let panel = build(&[], None, f);
        assert!(panel.widget::<Label>(ID_EMPTY).is_some());
        assert!(panel.widget::<Button>(BTN_CLOSE).is_some());
    }

    #[test]
    fn every_group_gets_its_own_row_and_delete_button() {
        let groups: Vec<WindowGroup> = (1..=3).map(|n| group(n, 2)).collect();
        let f = frame(groups.len(), 0);
        let panel = build(&groups, None, f);
        for i in 0..groups.len() {
            assert!(
                panel.widget::<Button>(ROW_BASE + i as WidgetId).is_some(),
                "нет строки группы {i}"
            );
            assert!(
                panel
                    .widget::<Button>(DELETE_BASE + i as WidgetId)
                    .is_some(),
                "нет кнопки удаления группы {i}"
            );
        }
    }

    #[test]
    fn a_collapsed_group_does_not_show_its_windows() {
        let groups = vec![group(1, 3)];
        let panel = build(&groups, None, frame(1, 0));
        assert!(
            panel.widget::<Label>(MEMBER_BASE).is_none(),
            "свёрнутая группа не должна показывать состав"
        );
    }

    #[test]
    fn an_expanded_group_lists_every_window_inside_it() {
        let groups = vec![group(1, 4)];
        let panel = build(&groups, Some(0), frame(1, 4));
        for m in 0..4 {
            assert!(
                panel.widget::<Label>(MEMBER_BASE + m as WidgetId).is_some(),
                "нет строки окна {m}"
            );
        }
    }

    #[test]
    fn nine_groups_with_a_full_one_expanded_still_fit_the_panel() {
        // Предельный случай: девять групп (по числу хоткеев) и раскрытая
        // группа из восьми окон. Формула высоты обязана его учитывать,
        // иначе кнопки уезжают за нижний край.
        let groups: Vec<WindowGroup> = (1..=MAX_ROWS as u8)
            .map(|n| group(n, MAX_GROUP_MEMBERS))
            .collect();
        let f = frame(groups.len(), MAX_GROUP_MEMBERS);
        let panel = build(&groups, Some(0), f);
        let mut ids: Vec<WidgetId> = vec![BTN_NEW, BTN_CLOSE];
        for i in 0..groups.len() {
            ids.push(ROW_BASE + i as WidgetId);
            ids.push(DELETE_BASE + i as WidgetId);
        }
        assert_inside(&panel, f, &ids);
    }

    #[test]
    fn expanding_a_group_pushes_the_rows_below_it_down() {
        // Строки состава вставляются под своей группой, а не поверх — иначе
        // раскрытая группа накрыла бы соседнюю.
        let groups: Vec<WindowGroup> = (1..=3).map(|n| group(n, 3)).collect();
        let collapsed = build(&groups, None, frame(3, 0));
        let expanded = build(&groups, Some(0), frame(3, 3));
        let row1_collapsed = collapsed
            .widget::<Button>(ROW_BASE + 1)
            .map(Widget::bounds)
            .expect("строка группы 2");
        let row1_expanded = expanded
            .widget::<Button>(ROW_BASE + 1)
            .map(Widget::bounds)
            .expect("строка группы 2");
        assert!(
            row1_expanded.cy > row1_collapsed.cy,
            "раскрытие первой группы обязано сдвинуть вторую вниз"
        );
    }

    #[test]
    fn the_row_label_reads_as_words_not_as_a_row_of_numbers() {
        // Раньше подпись была «3 3 (4)» — номер, имя (совпадавшее с номером)
        // и счётчик в скобках, и понять её было нельзя (репорт 2026-08-26).
        // Теперь она читается словами и несёт номер — цифру хоткея.
        let groups = vec![group(7, 2)];
        let panel = build(&groups, None, frame(1, 0));
        let mut prims = Vec::new();
        panel.draw(&mut prims);
        let readable = prims.iter().any(|p| match p {
            rst_render::Primitive::Text { text, .. } => {
                let t = text.trim_start();
                t.starts_with(GROUP_WORD) && t.contains('7')
            }
            _ => false,
        });
        assert!(
            readable,
            "подпись строки обязана читаться словами и нести номер группы"
        );
    }

    #[test]
    fn a_degenerate_frame_does_not_panic() {
        // Гонка переподключения монитора: экран может прийти вырожденным.
        let f = Box2D {
            cx: 0.0,
            cy: 0.0,
            w: 1.0,
            h: 1.0,
            rotation: 0.0,
        };
        let groups = vec![group(1, 2)];
        let _ = build(&groups, Some(0), f);
    }

    #[test]
    fn a_group_with_more_windows_than_the_table_allows_is_clamped() {
        // Конфиг правят руками: девять окон в группе — не повод рисовать
        // строку за краем панели.
        let mut g = group(1, MAX_GROUP_MEMBERS);
        g.members.push(member("лишнее окно"));
        let f = frame(1, MAX_GROUP_MEMBERS);
        let panel = build(&[g], Some(0), f);
        assert!(
            panel
                .widget::<Label>(MEMBER_BASE + MAX_MEMBERS as WidgetId)
                .is_none(),
            "девятое окно рисовать некуда"
        );
        assert_inside(&panel, f, &[BTN_NEW, BTN_CLOSE]);
    }
}
