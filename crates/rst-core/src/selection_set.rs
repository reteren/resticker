//! Множество выделенных стикеров и его геометрия (ROADMAP.md M2
//! «Мультивыделение: рамка, Shift+клик, Ctrl+A»; SPEC.md, раздел 3.2).
//!
//! Чистая логика, без окон и рендера: `SelectionSet` хранит только id
//! выделенных стикеров в порядке добавления (детерминированный порядок для
//! тулбара и тестов). Всё остальное — «рамка выделения» (какие AABB
//! пересекает перетаскиваемый прямоугольник), `Shift`+клик, `Ctrl+A` и общий
//! ограничивающий прямоугольник выделения (для общего тулбара над
//! мультивыделением, SPEC.md 3.6) — чистые функции над списком стикеров.
//!
//! Геометрия — в логических пикселях (DIP) относительно левого верхнего угла
//! монитора (ADR-010); для AABB используется [`crate::hittest::aabb`].

use uuid::Uuid;

use crate::hittest::{DipRect, aabb};
use crate::model::Sticker;

/// Множество выделенных стикеров.
///
/// Инвариант: id уникальны; порядок — хронологический (порядок добавления,
/// либо порядок в переданном списке стикеров для «выделить всё»/«рамкой»).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SelectionSet {
    ids: Vec<Uuid>,
}

impl SelectionSet {
    /// Пустое выделение.
    pub fn new() -> Self {
        Self { ids: Vec::new() }
    }

    /// Есть ли выделенные стикеры.
    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    /// Количество выделенных стикеров.
    pub fn len(&self) -> usize {
        self.ids.len()
    }

    /// Выделен ли стикер с данным id.
    pub fn contains(&self, id: Uuid) -> bool {
        self.ids.contains(&id)
    }

    /// id выделенных стикеров в порядке выделения.
    pub fn ids(&self) -> &[Uuid] {
        &self.ids
    }

    /// Снять выделение полностью (клик по пустому месту, SPEC.md 3.2).
    pub fn clear(&mut self) {
        self.ids.clear();
    }

    /// Обычный клик: по стикеру — выделить только его, по пустому месту
    /// (`None`) — снять выделение (SPEC.md 3.2).
    pub fn click(&mut self, id: Option<Uuid>) {
        match id {
            Some(id) => self.ids = vec![id],
            None => self.ids.clear(),
        }
    }

    /// `Shift`+клик: добавить в выделение, если не был выделен, иначе убрать
    /// (SPEC.md 3.2). Дубликатов не возникает.
    pub fn shift_click(&mut self, id: Uuid) {
        if let Some(pos) = self.ids.iter().position(|x| *x == id) {
            self.ids.remove(pos);
        } else {
            self.ids.push(id);
        }
    }

    /// Явно выделить стикер (не влияет на остальных).
    pub fn select(&mut self, id: Uuid) {
        if !self.ids.contains(&id) {
            self.ids.push(id);
        }
    }

    /// Явно снять выделение со стикера (не влияет на остальных).
    pub fn deselect(&mut self, id: Uuid) {
        self.ids.retain(|x| *x != id);
    }

    /// `Ctrl+A`: выделить все стикеры списка (SPEC.md 3.2). Пустой список
    /// снимает выделение.
    pub fn select_all(&mut self, stickers: &[Sticker]) {
        self.ids = stickers.iter().map(|s| s.id).collect();
    }

    /// Рамка выделения: заменить выделение на стикеры, чей ось-выровненный
    /// ограничивающий прямоугольник пересекается с `rect` (SPEC.md 3.2
    /// «Протяжка по пустому месту»). Порядок результата — как в `stickers`.
    ///
    /// `rect` нормализуется: перетаскивание в любом направлении даёт один
    /// результат; вырожденный прямоугольник (нулевые ширина/высота — клик
    /// вместо протяжки) — снимает выделение. Касание границ «впритык» не
    /// считается пересечением.
    pub fn rubber_band(&mut self, stickers: &[Sticker], rect: &DipRect) {
        self.ids = intersecting_ids(stickers, rect);
    }

    /// Общий ограничивающий прямоугольник выделения: объединение AABB всех
    /// выделенных стикеров, которые есть в `stickers` и имеют ненулевой
    /// размер. `None` — выделение пусто либо ни один id не разрешился
    /// (тулбар над мультивыделением не отрисовать, SPEC.md 3.6).
    pub fn bounds(&self, stickers: &[Sticker]) -> Option<DipRect> {
        let mut union: Option<DipRect> = None;
        for id in &self.ids {
            let Some(s) = stickers.iter().find(|s| s.id == *id) else {
                continue;
            };
            if !drawable(s) {
                continue;
            }
            let r = aabb(&s.placement, s.transform.rotation);
            union = Some(match union {
                None => r,
                Some(u) => union_of(&u, &r),
            });
        }
        union
    }

    /// Ссылки на выделенные стикеры в порядке выделения (id, отсутствующие
    /// в `stickers`, пропускаются).
    pub fn selected_stickers<'a>(&self, stickers: &'a [Sticker]) -> Vec<&'a Sticker> {
        self.ids
            .iter()
            .filter_map(|id| stickers.iter().find(|s| s.id == *id))
            .collect()
    }

    /// Убрать из выделения id, которых больше нет в `stickers` (стикер
    /// удалён). Вызывается после удаления, чтобы выделение не ссылалось на
    /// мёртвые id.
    pub fn prune(&mut self, stickers: &[Sticker]) {
        self.ids.retain(|id| stickers.iter().any(|s| s.id == *id));
    }
}

/// Стикер рисуется (и участвует в выделении/bbox), только если у него
/// положительные размеры; `NaN` и `<= 0` — невидимый/вырожденный.
fn drawable(s: &Sticker) -> bool {
    s.placement.w > 0.0 && s.placement.h > 0.0
}

/// Пересечение двух ось-выровненных прямоугольников; касание «впритык»
/// пересечением не считается.
fn intersects(a: &DipRect, b: &DipRect) -> bool {
    a.x < b.x + b.w && b.x < a.x + a.w && a.y < b.y + b.h && b.y < a.y + a.h
}

/// Объединение двух ось-выровненных прямоугольников.
fn union_of(a: &DipRect, b: &DipRect) -> DipRect {
    let x = a.x.min(b.x);
    let y = a.y.min(b.y);
    let right = (a.x + a.w).max(b.x + b.w);
    let bottom = (a.y + a.h).max(b.y + b.h);
    DipRect {
        x,
        y,
        w: right - x,
        h: bottom - y,
    }
}

/// Нормализация прямоугольника протяжки: негативные ширина/высота (драг
/// влево/вверх) приводятся к верхнему левому углу. Вырожденный (нулевой
/// размер или NaN) прямоугольник даёт `None`.
fn normalized(rect: &DipRect) -> Option<DipRect> {
    let (x, right) = if rect.x <= rect.x + rect.w {
        (rect.x, rect.x + rect.w)
    } else {
        (rect.x + rect.w, rect.x)
    };
    let (y, bottom) = if rect.y <= rect.y + rect.h {
        (rect.y, rect.y + rect.h)
    } else {
        (rect.y + rect.h, rect.y)
    };
    let (w, h) = (right - x, bottom - y);
    if w > 0.0 && h > 0.0 && w.is_finite() && h.is_finite() {
        Some(DipRect { x, y, w, h })
    } else {
        None
    }
}

/// id стикеров из `stickers`, чей AABB пересекается с `rect` (в порядке
/// списка). Нормализация и фильтр вырожденных стикеров — здесь.
fn intersecting_ids(stickers: &[Sticker], rect: &DipRect) -> Vec<Uuid> {
    let Some(rect) = normalized(rect) else {
        return Vec::new();
    };
    stickers
        .iter()
        .filter(|s| drawable(s) && intersects(&aabb(&s.placement, s.transform.rotation), &rect))
        .map(|s| s.id)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Placement;
    use std::f64::consts::{FRAC_PI_2, FRAC_PI_4};

    fn sticker(cx: f64, cy: f64, w: f64, h: f64) -> Sticker {
        Sticker {
            id: Uuid::new_v4(),
            placement: Placement {
                cx,
                cy,
                w,
                h,
                ..Placement::default()
            },
            ..Default::default()
        }
    }

    fn rotated(cx: f64, cy: f64, w: f64, h: f64, rotation: f64) -> Sticker {
        let mut s = sticker(cx, cy, w, h);
        s.transform.rotation = rotation;
        s
    }

    fn assert_close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() <= 1e-9,
            "ожидалось {expected}, получено {actual}"
        );
    }

    #[test]
    fn empty_by_default() {
        let s = SelectionSet::new();
        assert!(s.is_empty());
        assert_eq!(s.len(), 0);
        assert!(!s.contains(Uuid::new_v4()));
        assert!(s.ids().is_empty());
        assert_eq!(SelectionSet::default(), SelectionSet::new());
    }

    #[test]
    fn click_selects_only_that_sticker() {
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let mut s = SelectionSet::new();
        s.click(Some(a));
        s.click(Some(b));
        assert_eq!(s.len(), 1);
        assert!(s.contains(b));
        assert!(!s.contains(a));
        assert_eq!(s.ids(), &[b]);
    }

    #[test]
    fn click_on_empty_space_clears() {
        let a = Uuid::new_v4();
        let mut s = SelectionSet::new();
        s.select(a);
        s.click(None);
        assert!(s.is_empty());
    }

    #[test]
    fn click_same_id_twice_keeps_single() {
        let a = Uuid::new_v4();
        let mut s = SelectionSet::new();
        s.click(Some(a));
        s.click(Some(a));
        assert_eq!(s.len(), 1);
        assert_eq!(s.ids(), &[a]);
    }

    #[test]
    fn shift_click_toggles() {
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let mut s = SelectionSet::new();
        s.shift_click(a);
        s.shift_click(b);
        assert!(s.contains(a) && s.contains(b));
        s.shift_click(a);
        assert!(!s.contains(a));
        assert!(s.contains(b));
        assert_eq!(s.len(), 1);
    }

    #[test]
    fn shift_click_no_duplicates() {
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let mut s = SelectionSet::new();
        s.shift_click(a);
        s.shift_click(a);
        s.shift_click(b);
        s.shift_click(a); // снова добавили a — дубликата нет
        assert_eq!(s.len(), 2);
        assert_eq!(s.ids(), &[b, a]);
    }

    #[test]
    fn select_and_deselect_are_explicit() {
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let mut s = SelectionSet::new();
        s.select(a);
        s.select(b);
        s.select(a); // повторное выделение — no-op
        assert_eq!(s.len(), 2);
        s.deselect(a);
        assert!(!s.contains(a));
        assert!(s.contains(b));
        assert_eq!(s.len(), 1);
    }

    #[test]
    fn select_all_in_config_order() {
        let stickers = [
            sticker(0.0, 0.0, 10.0, 10.0),
            sticker(50.0, 50.0, 10.0, 10.0),
        ];
        let mut s = SelectionSet::new();
        s.select(stickers[1].id); // в другом порядке
        s.select_all(&stickers);
        assert_eq!(
            s.ids(),
            &[stickers[0].id, stickers[1].id],
            "порядок — как в списке конфига"
        );
    }

    #[test]
    fn select_all_empty_clears() {
        let a = Uuid::new_v4();
        let mut s = SelectionSet::new();
        s.select(a);
        s.select_all(&[]);
        assert!(s.is_empty());
    }

    #[test]
    fn rubber_band_selects_intersecting_aabbs() {
        let stickers = [
            sticker(100.0, 50.0, 40.0, 20.0),  // AABB: x=80,y=40,w=40,h=20
            sticker(200.0, 100.0, 60.0, 40.0), // AABB: x=170,y=80,w=60,h=40
            sticker(400.0, 400.0, 20.0, 20.0), // вне рамки
        ];
        let rect = DipRect::new(100.0, 50.0, 90.0, 50.0);
        let mut s = SelectionSet::new();
        s.rubber_band(&stickers, &rect);
        assert_eq!(s.ids(), &[stickers[0].id, stickers[1].id]);
        assert!(!s.contains(stickers[2].id));
    }

    #[test]
    fn rubber_band_replaces_previous_selection() {
        let stickers = [
            sticker(100.0, 50.0, 40.0, 20.0),
            sticker(200.0, 100.0, 60.0, 40.0),
        ];
        let mut s = SelectionSet::new();
        s.select(stickers[0].id);
        s.rubber_band(&stickers, &DipRect::new(170.0, 80.0, 60.0, 40.0));
        assert_eq!(s.ids(), &[stickers[1].id]);
    }

    #[test]
    fn rubber_band_uses_aabb_of_rotated_sticker() {
        // Квадрат 100x100, повёрнут на 45°: AABB — 141.4x141.4 вокруг центра.
        let stickers = [rotated(0.0, 0.0, 100.0, 100.0, FRAC_PI_4)];
        let rect = DipRect::new(-60.0, -60.0, 20.0, 20.0);
        let mut s = SelectionSet::new();
        s.rubber_band(&stickers, &rect);
        assert_eq!(s.ids(), &[stickers[0].id]);
    }

    #[test]
    fn rubber_band_rotated_90_swaps_extents() {
        // Стикер 100x40 повёрнут на 90°: AABB 40x100 (x=80,y=50,w=40,h=100).
        let stickers = [rotated(100.0, 100.0, 100.0, 40.0, FRAC_PI_2)];
        let inside = DipRect::new(90.0, 120.0, 20.0, 20.0);
        let outside = DipRect::new(200.0, 200.0, 20.0, 20.0);
        let mut s = SelectionSet::new();
        s.rubber_band(&stickers, &inside);
        assert_eq!(s.ids(), &[stickers[0].id]);
        s.rubber_band(&stickers, &outside);
        assert!(s.is_empty());
    }

    #[test]
    fn rubber_band_normalizes_reversed_drag() {
        let stickers = [sticker(100.0, 50.0, 40.0, 20.0)];
        // «Драг» вверх-влево: отрицательные ширина/высота — тот же результат.
        let rect = DipRect::new(120.0, 60.0, -40.0, -20.0);
        let mut s = SelectionSet::new();
        s.rubber_band(&stickers, &rect);
        assert_eq!(s.ids(), &[stickers[0].id]);
    }

    #[test]
    fn rubber_band_touching_edge_is_not_intersection() {
        let stickers = [sticker(100.0, 50.0, 40.0, 20.0)]; // левый край x=80
        let rect = DipRect::new(40.0, 45.0, 40.0, 10.0); // вплотную к x=80
        let mut s = SelectionSet::new();
        s.rubber_band(&stickers, &rect);
        assert!(s.is_empty());
    }

    #[test]
    fn bounds_single_rotated_uses_aabb() {
        let stickers = [rotated(100.0, 100.0, 100.0, 40.0, FRAC_PI_2)];
        let mut s = SelectionSet::new();
        s.select(stickers[0].id);
        let b = s.bounds(&stickers).expect("bbox есть");
        assert_close(b.x, 80.0);
        assert_close(b.y, 50.0);
        assert_close(b.w, 40.0);
        assert_close(b.h, 100.0);
    }

    #[test]
    fn rubber_band_skips_degenerate_stickers() {
        let normal = sticker(100.0, 50.0, 40.0, 20.0);
        let mut degenerate = sticker(100.0, 50.0, 0.0, 20.0);
        degenerate.id = Uuid::new_v4();
        let stickers = [normal.clone(), degenerate];
        let rect = DipRect::new(80.0, 40.0, 40.0, 20.0);
        let mut s = SelectionSet::new();
        s.rubber_band(&stickers, &rect);
        assert_eq!(s.ids(), &[normal.id]);
    }

    #[test]
    fn bounds_union_of_two_stickers() {
        let stickers = [
            sticker(100.0, 50.0, 40.0, 20.0),  // x=80,y=40,w=40,h=20
            sticker(200.0, 100.0, 60.0, 40.0), // x=170,y=80,w=60,h=40
        ];
        let mut s = SelectionSet::new();
        s.select(stickers[0].id);
        s.select(stickers[1].id);
        let b = s.bounds(&stickers).expect("непустое выделение даёт bbox");
        assert_eq!(b, DipRect::new(80.0, 40.0, 150.0, 80.0));
    }

    #[test]
    fn bounds_single_sticker_is_its_aabb() {
        let stickers = [sticker(100.0, 50.0, 40.0, 20.0)];
        let mut s = SelectionSet::new();
        s.select(stickers[0].id);
        assert_eq!(
            s.bounds(&stickers),
            Some(DipRect::new(80.0, 40.0, 40.0, 20.0))
        );
    }

    #[test]
    fn bounds_empty_is_none() {
        let stickers = [sticker(0.0, 0.0, 10.0, 10.0)];
        let s = SelectionSet::new();
        assert_eq!(s.bounds(&stickers), None);
    }

    #[test]
    fn bounds_ignores_missing_and_degenerate_ids() {
        let stickers = [sticker(100.0, 50.0, 40.0, 20.0)];
        let mut s = SelectionSet::new();
        s.select(Uuid::new_v4()); // id, которого нет в списке
        s.select(stickers[0].id);
        assert_eq!(
            s.bounds(&stickers),
            Some(DipRect::new(80.0, 40.0, 40.0, 20.0))
        );

        s.clear();
        s.select(Uuid::new_v4());
        assert_eq!(s.bounds(&stickers), None, "id вне списка игнорируется");

        s.clear();
        let degenerate = sticker(0.0, 0.0, 0.0, 20.0);
        s.select(degenerate.id);
        assert_eq!(s.bounds(&stickers), None, "вырожденный стикер не даёт bbox");
    }

    #[test]
    fn selected_stickers_follow_selection_order() {
        let stickers = [
            sticker(0.0, 0.0, 10.0, 10.0),
            sticker(50.0, 50.0, 10.0, 10.0),
            sticker(100.0, 100.0, 10.0, 10.0),
        ];
        let mut s = SelectionSet::new();
        s.select(stickers[1].id);
        s.select(stickers[0].id);
        s.select(stickers[2].id);
        let selected = s.selected_stickers(&stickers);
        assert_eq!(
            selected.iter().map(|x| x.id).collect::<Vec<_>>(),
            vec![stickers[1].id, stickers[0].id, stickers[2].id]
        );
    }

    #[test]
    fn selected_stickers_skips_missing_ids() {
        let stickers = [sticker(0.0, 0.0, 10.0, 10.0)];
        let mut s = SelectionSet::new();
        s.select(stickers[0].id);
        s.select(Uuid::new_v4());
        assert_eq!(s.selected_stickers(&stickers).len(), 1);
    }

    #[test]
    fn prune_removes_deleted_ids() {
        let stickers = [
            sticker(0.0, 0.0, 10.0, 10.0),
            sticker(50.0, 50.0, 10.0, 10.0),
        ];
        let mut s = SelectionSet::new();
        s.select(stickers[0].id);
        s.select(stickers[1].id);
        s.select(Uuid::new_v4());
        // «Удалили» первый стикер и неизвестный id.
        s.prune(&[stickers[1].clone()]);
        assert_eq!(s.ids(), &[stickers[1].id]);
    }

    #[test]
    fn ids_keep_deterministic_insertion_order() {
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let c = Uuid::new_v4();
        let mut s = SelectionSet::new();
        s.shift_click(b);
        s.shift_click(a);
        s.shift_click(c);
        s.shift_click(a); // убрали a — порядок сохраняется
        assert_eq!(s.ids(), &[b, c]);
    }
}
