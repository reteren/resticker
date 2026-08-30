//! Дамп растров стекла в сырые RGBA-файлы для ГЛАЗА: юнит-тесты `glass`
//! проверяют отдельные пиксели и не видят картинку целиком, а материал
//! оценивается только взглядом. Не гоняется в обычном прогоне (`#[ignore]`).
//! Запуск:
//! `cargo test -p rst-render --test glass_dump -- --ignored --nocapture`.
use rst_render::glass::{Surface, glass_rgba, glow_pad_px};

#[test]
#[ignore = "ручная визуальная проверка"]
fn dump() {
    let dir = std::env::var("GLASS_DUMP_DIR").expect("GLASS_DUMP_DIR");
    for (name, surface, w, h, r, glow) in [
        ("panel", Surface::Panel, 420u32, 260u32, 18.0, 0.0),
        ("card", Surface::Card, 240, 120, 14.0, 0.0),
        ("control", Surface::Control, 160, 34, 10.0, 0.0),
        ("control_hover", Surface::ControlHover, 160, 34, 10.0, 1.0),
        ("control_active", Surface::ControlActive, 160, 34, 10.0, 1.0),
        ("sunken", Surface::Sunken, 160, 26, 10.0, 0.0),
    ] {
        let pad = glow_pad_px(glow);
        let (fw, fh) = (w + 2 * pad, h + 2 * pad);
        let rgba = glass_rgba(fw, fh, r, surface, glow);
        std::fs::write(format!("{dir}/{name}_{fw}x{fh}.rgba"), &rgba).expect("запись");
        println!("{name} {fw}x{fh}");
    }
}
