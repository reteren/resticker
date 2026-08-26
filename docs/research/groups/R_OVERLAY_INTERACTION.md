# Исследование взаимодействия с оверлеем: интерактивная панель групп поверх экрана

**Документ:** `docs/research/groups/R_OVERLAY_INTERACTION.md`  
**Статус:** Разведка (T6), код не пишется  
**Дата:** 2026-08-25  
**Ветка:** `feat/m2-edit-mode`  
**Цель:** Спроектировать появление и работу интерактивного меню редактирования групп (лента снимков окон + слоты раскладки с drag-and-drop) по глобальному хоткею (например, `Alt+Shift+G`) поверх всего экрана БЕЗ перехода в полный режим редактирования стикеров.

---

## 1. Устройство оверлея (`crates/rst-win32/src/overlay.rs`)

### 1.1. Создание и системные стили окна
Окно оверлея создаётся на каждый монитор функцией `create_window(bounds_px: Rect)` ([`crates/rst-win32/src/overlay.rs:938-1044`](file:///C:/resticker/crates/rst-win32/src/overlay.rs#L938-L1044)) на собственном потоке со своим циклом сообщений (`run_message_loop`, lines 748-903).

При создании выставляются следующие расширенные стили Win32 ([lines 991-996](file:///C:/resticker/crates/rst-win32/src/overlay.rs#L991-L996)):
```rust
WS_EX_TOPMOST
    | WS_EX_TOOLWINDOW
    | WS_EX_NOACTIVATE
    | WS_EX_TRANSPARENT
    | WS_EX_NOREDIRECTIONBITMAP
    | WS_EX_LAYERED
```
И вызывается [`SetLayeredWindowAttributes(hwnd, COLORREF(0), 255, LWA_ALPHA)`](file:///C:/resticker/crates/rst-win32/src/overlay.rs#L1023) (lines 1023-1024).

### 1.2. Когда окно кликопрозрачно, а когда принимает мышь
* **Кликопрозрачное состояние (Normal Mode):**
  Включены биты `WS_EX_TRANSPARENT | WS_EX_NOACTIVATE`. В сочетании с `WS_EX_LAYERED` операционная система Windows (DWM) исключает окно из хит-тестинга мыши и пробрасывает все клики и колесо мыши сквозь оверлей на нижележащие окна других процессов (Explorer, браузеры, панель задач).
* **Интерактивное состояние (Interactive / Edit Mode):**
  Бит `WS_EX_TRANSPARENT` снят. Окно оверлея ловит все события мыши в своих границах.

### 1.3. Что именно переключает состояние кликопрозрачности
Переключение выполняется через модификацию `GWL_EXSTYLE`:
1. **`set_click_through(&self, click_through: bool)`** ([lines 395-403](file:///C:/resticker/crates/rst-win32/src/overlay.rs#L395-L403)):
   Переключает биты `WS_EX_TRANSPARENT | WS_EX_NOACTIVATE` через `set_exstyle_bits`. Если `!click_through` (вход в интерактив), дополнительно вызывает `SetForegroundWindow(self.hwnd)` для передачи окну фокуса ввода.
2. **`set_interactive(&self, interactive: bool)`** ([lines 456-458](file:///C:/resticker/crates/rst-win32/src/overlay.rs#L456-L458)):
   То же переключение `WS_EX_TRANSPARENT | WS_EX_NOACTIVATE`, но **без** `SetForegroundWindow`. Используется для второстепенных мониторов при мультимониторной конфигурации, чтобы окна не боролись за фокус.
3. **`set_hover_click_target(&self, target: bool)`** ([lines 472-474](file:///C:/resticker/crates/rst-win32/src/overlay.rs#L472-L474)):
   Снимает **только** `WS_EX_TRANSPARENT`, оставляя `WS_EX_NOACTIVATE`. Применяется для полосы перемотки видео-стикеров вне режима редактирования (позволяет кликать по полосе без потери фокуса активного приложения).
4. **`toggle_exstyle(&self, bits: u32, set: bool)`** ([lines 529-557](file:///C:/resticker/crates/rst-win32/src/overlay.rs#L529-L557)):
   Реализует мутацию стилей через `GetWindowLongPtrW` / `SetWindowLongPtrW` с обязательным вызовом:
   `SetWindowPos(self.hwnd, None, 0, 0, 0, 0, SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE)`.
   Флаг `SWP_FRAMECHANGED` критически важен: без него DWM использует кэшированные стили окна и кликопрозрачность не переключается немедленно.
5. **Вырезы через регионы (`set_hole`)** ([lines 418-448](file:///C:/resticker/crates/rst-win32/src/overlay.rs#L418-L448)):
   Создаёт GDI-регион `CreateRectRgn` и вычитает прямоугольник `CombineRgn(..., RGN_DIFF)`, после чего применяет `SetWindowRgn`. В вырезанной дыре оверлей не рисуется и не ловит ввод даже в интерактивном режиме (используется для окна настроек Tauri).

### 1.4. Проверка в `wndproc` и `HTTRANSPARENT`
* **`HTTRANSPARENT` не используется в проекте.** В кодовой базе нет обработчика `WM_NCHITTEST`.
* Вместо этого в `wndproc` ([lines 1130-1165](file:///C:/resticker/crates/rst-win32/src/overlay.rs#L1130-L1165) и [lines 1166-1190](file:///C:/resticker/crates/rst-win32/src/overlay.rs#L1166-L1190)) стоит жёсткий программный гейт:
  ```rust
  let click_through = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) } as u32 & WS_EX_TRANSPARENT.0 != 0;
  if !click_through {
      if let Some(state) = unsafe { state_ptr.as_mut() } {
          if let Some(event) = state.capture.handle_message(msg, wparam, lparam) {
              let _ = state.tx.send(OverlayEvent::Input(event));
              return LRESULT(0);
          }
      }
  } else if let Some(state) = unsafe { state_ptr.as_mut() } {
      if state.capture.is_captured() {
          state.capture.force_release();
      }
  }
  ```
  Если `WS_EX_TRANSPARENT` выставлен, сообщения мыши игнорируются и уходят в `DefWindowProcW`, а случайный захват принудительно сбрасывается.

---

## 2. Режим редактирования в `crates/resticker/src/overlay_manager.rs`

### 2.1. Вход и выход из режима редактирования
Вход и выход осуществляются функцией `toggle_edit_mode` ([`crates/resticker/src/overlay_manager.rs:4373-4412`](file:///C:/resticker/crates/resticker/src/overlay_manager.rs#L4373-L4412)):
1. Сбрасываются временные панели сеанса через `reset_edit_mode_panels(edit, exiting)` ([lines 4430-4445](file:///C:/resticker/crates/resticker/src/overlay_manager.rs#L4430-L4445)).
2. Инвертируется флаг: `edit.active = !edit.active;` ([line 4375](file:///C:/resticker/crates/resticker/src/overlay_manager.rs#L4375)).
3. Переключается кликопрозрачность окна-инициатора: `overlay.set_click_through(!edit.active);` ([line 4376](file:///C:/resticker/crates/resticker/src/overlay_manager.rs#L4376)).
4. **При входе (`edit.active == true`):**
   * Запускается анимация выезда тулбара у курсора: `edit.cursor_panel_slide = PanelSlide { from: 0.0, to: 1.0, ... }` ([lines 4382-4386](file:///C:/resticker/crates/resticker/src/overlay_manager.rs#L4382-L4386)).
   * Приостанавливается принуждение закреплённых окон: `suspend_pin_enforcement` ([line 4391](file:///C:/resticker/crates/resticker/src/overlay_manager.rs#L4391)) — снимаются замки `lock_move`/`lock_interact` на время редактирования.
5. **При выходе (`edit.active == false`):**
   * Возобновляется принуждение закреплённых окон: `resume_pin_enforcement` ([line 4397](file:///C:/resticker/crates/resticker/src/overlay_manager.rs#L4397)).
   * Сбрасывается Win32-захват мыши: `overlay.force_release_capture()` ([line 4406](file:///C:/resticker/crates/resticker/src/overlay_manager.rs#L4406)).
   * Сохраняется конфигурация: `config::save(cfg, config_path)` ([line 4407](file:///C:/resticker/crates/resticker/src/overlay_manager.rs#L4407)).
6. Синхронизируются остальные мониторы: функция `sync_other_monitors_edit_mode` ([lines 4468-4480](file:///C:/resticker/crates/resticker/src/overlay_manager.rs#L4468-L4480)) вызывает `other_ms.overlay.set_interactive(edit.active)` для всех остальных мониторов (и `force_release_capture()`, если выходим).
7. Пересобираются UI-панели: `rebuild_ui_panels(edit, cfg, monitor_geometry)` ([line 4411](file:///C:/resticker/crates/resticker/src/overlay_manager.rs#L4411)).

### 2.2. Все места проверки `edit.active` в `overlay_manager.rs`
1. **[Line 333, 3762, 3972, 4005]** `sticker_should_tick`: в `!edit.active` скрытые и полностью перекрытые окнами стикеры перестают тикать анимации/видео; в `edit.active` они продолжают тикать.
2. **[Line 2996, 3537, 3604]** `sync_other_monitors_edit_mode`: передача `edit.active` для синхронизации кликопрозрачности других мониторов.
3. **[Line 3226, 3241, 3280]** `rebuild_sprites` / `resync_sprites` / инициализация мониторов: передача `edit.active`.
4. **[Line 3502]** `OverlayMessage::Event(..., Key { .. }) if edit.active`: обработка клавиатуры (Esc, Delete, Ctrl+Z, Ctrl+D) происходит **только** если `edit.active == true`.
5. **[Line 3540]** `OverlayMessage::Event(..., Input(event)) if !edit.active`: вне режима редактирования ввод мыши идёт **только** в `handle_timeline_hover_input` (полоса перемотки).
6. **[Line 3560]** `OverlayMessage::Event(..., Char(ch)) if edit.active`: ввод печатных символов в поля ввода панелей работает **только** если `edit.active == true`.
7. **[Line 3569]** `OverlayMessage::Event(..., Input(event)) if edit.active`: общая обработка ввода мыши (`handle_input`) выполняется **только** если `edit.active == true`.
8. **[Line 3625]** `OverlayMessage::Windows(Changed)`: рантайм-принуждение геометрии пинов (`maintain_pinned_windows`, `enforce_pinned_geometry`, `enforce_snap_gap_on_free_windows`) выполняется **только** `if !edit.active`.
9. **[Line 4087, 6778]** `want_hole`: вырез окна настроек активируется только `if edit.active`.
10. **[Line 4156]** `next_timeline_deadline`: расчёт дедлайна скрытия полосы перемотки отключается в `edit.active`.
11. **[Lines 6390, 6457, 6534]** `update_cursor`: курсор редактирования (стрелки, перемещение, поворот) обновляется только в `edit.active`.
12. **[Lines 6799, 6840, 6900]** `rebuild_ui_panels`: тулбар выделения (`edit.toolbar`) и панель инструментов у курсора (`edit.cursor_panel`) строятся **только** `if edit.active`.
13. **[Lines 10619-10628]** `redraw`: при `edit.active` поверх всего экрана рисуется 25% черное полупрозрачное затемнение (`solid_sprite(black_tex, ..., EDIT_OVERLAY_OPACITY)`).
14. **[Line 10640]** `redraw`: при `edit.active` скрытые стикеры рисуются в кадре с шахматной текстурой (`.filter(|s| s.visible || edit.active)`).
15. **[Line 10655]** `redraw`: при `edit.active` отключается отсечение окклюзией (`let culled = !edit.active && ...`).
16. **[Line 10743]** `redraw`: при `edit.active` рисуются рамки выделения стикеров и ручки ресайза/поворота.
17. **[Line 11100]** `redraw`: **ГЛАВНЫЙ ЭФФЕКТ** — `if edit.active || sticker_mask_slots.is_empty() { renderer.draw(&frame, sync) } else { build mask textures and draw_masked }`. В режиме `edit.active` **маски перекрытия окнами полностью отключаются**, стикеры рисуются поверх всех окон!

---

## 3. Панель Presets (`crates/resticker/src/preset_picker.rs`) как эталон модальной панели

### 3.1. Устройство и построение `Panel`
Модуль `preset_picker.rs` строит retained-панель немедленного режима (`rst_render::Panel`):
* Функция `build(presets: &[Preset], name_draft: &str, frame: Box2D) -> Panel` ([`crates/resticker/src/preset_picker.rs:107-243`](file:///C:/resticker/crates/resticker/src/preset_picker.rs#L107-L243)).
* Использует VGUI-стиль окна настроек: `.with_style(WidgetStyle::Settings)` ([line 109](file:///C:/resticker/crates/rst-render/src/widgets.rs#L109)).
* Набор виджетов:
  1. Заголовок `ID_TITLE` (`Label`, [line 118](file:///C:/resticker/crates/resticker/src/preset_picker.rs#L118)).
  2. Список строк пресетов: кнопки строк `ROW_BASE + i` (`Button`, [line 138](file:///C:/resticker/crates/resticker/src/preset_picker.rs#L138)) и кнопки удаления `DELETE_BASE + i` (`Button`, [line 152](file:///C:/resticker/crates/resticker/src/preset_picker.rs#L152)).
  3. Разделитель `ID_DIVIDER` (`Divider`, [line 171](file:///C:/resticker/crates/resticker/src/preset_picker.rs#L171)).
  4. Поле ввода имени `FIELD_NAME` (`TextField`, [line 179](file:///C:/resticker/crates/resticker/src/preset_picker.rs#L179)) с `.keep_on_blur()` + кнопка `BTN_SAVE` (`Button`, [line 196](file:///C:/resticker/crates/resticker/src/preset_picker.rs#L196)).
  5. Нижний ряд действий: `BTN_IMPORT` ([line 215](file:///C:/resticker/crates/resticker/src/preset_picker.rs#L215)) и `BTN_CLOSE` ([line 228](file:///C:/resticker/crates/resticker/src/preset_picker.rs#L228)).

### 3.2. Где живёт состояние панели
Состояние хранится в структуре `PresetPickerState` ([`crates/resticker/src/overlay_manager.rs:1563-1572`](file:///C:/resticker/crates/resticker/src/overlay_manager.rs#L1563-L1572)):
```rust
struct PresetPickerState {
    name_draft: String,
    panel: Panel,
    monitor_id: MonitorId,
}
```
Экземпляр живёт в `EditState.preset_picker: Option<PresetPickerState>` ([line 1253](file:///C:/resticker/crates/resticker/src/overlay_manager.rs#L1253)).

### 3.3. Приём событий мыши и клавиатуры
1. **Открытие:** функция `open_preset_picker` ([lines 7839-7861](file:///C:/resticker/crates/resticker/src/overlay_manager.rs#L7839-L7861)) центрирует панель на экране целевого монитора и создаёт `PresetPickerState`.
2. **`MouseDown` в `handle_input` ([lines 9346-9358](file:///C:/resticker/crates/resticker/src/overlay_manager.rs#L9346-L9358)):**
   Событие передаётся в `picker.panel.pointer_event(PointerEvent::Down { pos })`. Если клик был мимо панели или на чужом мониторе (`!hit`), панель закрывается: `edit.preset_picker = None; return true;`.
3. **`MouseMove` в `handle_input` ([lines 9654-9660](file:///C:/resticker/crates/resticker/src/overlay_manager.rs#L9654-L9660)):**
   Передаёт `PointerEvent::Move { pos }` в панель для hover-эффектов кнопок и поглощает событие (`return true;`), блокируя сцену стикеров.
4. **`MouseUp` в `handle_input` ([lines 9970-9986](file:///C:/resticker/crates/resticker/src/overlay_manager.rs#L9970-L9986)):**
   Вызывает `handle_preset_picker_up` ([lines 7903-8032](file:///C:/resticker/crates/resticker/src/overlay_manager.rs#L7903-L8032)), где опрашиваются клики через `take_panel_click(picker, ID)`:
   * `BTN_CLOSE` → закрывает панель (`edit.preset_picker = None`).
   * `DELETE_BASE + i` → удаляет пресет, сохраняет конфиг и вызывает `rebuild_preset_picker`.
   * `BTN_SAVE` → сохраняет пресет из `name_draft`, очищает черновик и вызывает `rebuild_preset_picker`.
   * `BTN_IMPORT` → открывает диалог выбора файла `pick_preset_file`, импортирует и пересобирает панель.
   * `ROW_BASE + i` → применяет пресет, обновляет окклюдеры, закрывает панель.
5. **Клавиатура:**
   * `handle_char` ([lines 5404-5422](file:///C:/resticker/crates/resticker/src/overlay_manager.rs#L5404-L5422)): перенаправляет символы в `TextField` и обновляет `state.name_draft`.
   * `handle_key` ([lines 5085-5113](file:///C:/resticker/crates/resticker/src/overlay_manager.rs#L5085-L5113)): отдаёт Backspace/Enter/стрелки в `TextField`. Если клавиша не поглощена полем ввода, `VK_ESCAPE` закрывает панель (`edit.preset_picker = None`).

### 3.4. Как панель закрывается
Панель закрывается обнулением `edit.preset_picker = None`:
* Нажатием кнопки `BTN_CLOSE` ([line 7926](file:///C:/resticker/crates/resticker/src/overlay_manager.rs#L7926)).
* Кликом мимо панели на любом мониторе в `MouseDown` ([line 9355](file:///C:/resticker/crates/resticker/src/overlay_manager.rs#L9355)).
* Нажатием клавиши `Esc` в `handle_key` ([line 5108](file:///C:/resticker/crates/resticker/src/overlay_manager.rs#L5108)).
* Применением пресета по клику на строку ([line 8019](file:///C:/resticker/crates/resticker/src/overlay_manager.rs#L8019)).
* Любым переключением режима редактирования в `reset_edit_mode_panels` ([line 4436](file:///C:/resticker/crates/resticker/src/overlay_manager.rs#L4436)).

---

## 4. ГЛАВНЫЙ ВОПРОС: Интерактивная панель БЕЗ входа в режим редактирования стикеров

### 4.1. Что произойдёт, если просто выставить `edit.active = true`
Попытка использовать `edit.active = true` для показа меню групп приведёт к каскаду нежелательных побочных эффектов:
1. **Экран затемнится на 25%:** `redraw` ([line 10619](file:///C:/resticker/crates/resticker/src/overlay_manager.rs#L10619)) нарисует черный оверлей `EDIT_OVERLAY_OPACITY`.
2. **Скрытые стикеры станут видимыми:** `redraw` ([line 10640](file:///C:/resticker/crates/resticker/src/overlay_manager.rs#L10640)) отрисует все скрытые стикеры с шахматной текстурой.
3. **ОККЛЮЗИЯ И ВЫРЕЗЫ ПОЛНОСТЬЮ ОТКЛЮЧАТСЯ:** в строках [10655](file:///C:/resticker/crates/resticker/src/overlay_manager.rs#L10655) и [11100](file:///C:/resticker/crates/resticker/src/overlay_manager.rs#L11100) маски окклюзии отключаются при `edit.active`. Все стикеры, которые должны были прятаться под окнами браузера или IDE, **внезапно вылезут поверх окон**.
4. **Снимутся замки пинов:** `suspend_pin_enforcement` ([line 4391](file:///C:/resticker/crates/resticker/src/overlay_manager.rs#L4391)) и гейт `if !edit.active` ([line 3625](file:///C:/resticker/crates/resticker/src/overlay_manager.rs#L3625)) отключат удержание геометрии и замки взаимодействия закреплённых окон.
5. **Появится тулбар редактирования:** `rebuild_ui_panels` ([line 6799](file:///C:/resticker/crates/resticker/src/overlay_manager.rs#L6799)) создаст `cursor_panel` (кнопки открытия файлов, настроек, выхода) и тулбар выделения.
6. **Клики мимо панели пойдут в стикеры:** клик в свободное место начнёт протяжку рамки выделения (`Marquee`) или выбор стикера на заднем плане ([lines 9540-9560](file:///C:/resticker/crates/resticker/src/overlay_manager.rs#L9540-L9560)).

### 4.2. Узкий и правильный путь: Рекомендация
Показать интерактивную панель меню групп поверх экрана **можно и нужно БЕЗ включения `edit.active`**.

#### Конкретные шаги реализации и строки кода:
1. **Состояние в `EditState`:**
   В `EditState` (`crates/resticker/src/overlay_manager.rs`, рядом с `preset_picker` в line 1253) добавить:
   ```rust
   pub group_editor: Option<GroupEditorState>,
   ```
2. **Хоткей в `rst-win32`:**
   В `crates/rst-win32/src/overlay.rs` зарегистрировать новый глобальный хоткей `GROUP_EDITOR_HOTKEY_ID` (например, 8) с комбинацией по умолчанию `Alt+Shift+G` (по паттерну `EDIT_HOTKEY_ID` в line 67 и `pin_focused_hotkey` в line 820). В цикле сообщений (line 879) отправлять `OverlayEvent::ToggleGroupEditor`.
3. **Управление кликопрозрачностью окон оверлея при открытии/закрытии меню групп:**
   * **При открытии (`open_group_editor`):**
     * `edit.active` остаётся `false`!
     * Вызвать `overlay.set_interactive(true)` на мониторе с панелью и на остальных мониторах (чтобы клик мимо панели на любом мониторе закрывал её).
     * Вызвать `SetForegroundWindow(overlay.hwnd())`, чтобы окно оверлея получило фокус клавиатуры для обработки `Esc` и хоткеев.
     * Вызвать `overlay.raise_above_pinned(&edit.pinned_windows)` ([line 606](file:///C:/resticker/crates/rst-win32/src/overlay.rs#L606)), чтобы меню не перекрывалось закреплёнными topmost-окнами.
   * **При закрытии (`close_group_editor`):**
     * Выставить `edit.group_editor = None;`.
     * Если `!edit.active`, вернуть кликопрозрачность: `overlay.set_click_through(true)` и `sync_other_monitors_edit_mode(&monitors_map, &monitor_id, false)`.
     * Обязательно вызвать `overlay.force_release_capture()` на случай отпускания мыши.
4. **Маршрутизация событий в главном цикле `overlay_manager.rs::run()`:**
   * **Ввод мыши при `!edit.active` ([line 3540](file:///C:/resticker/crates/resticker/src/overlay_manager.rs#L3540)):**
     Расширить ветку:
     ```rust
     OverlayMessage::Event(monitor_id, OverlayEvent::Input(event)) if !edit.active => {
         if edit.group_editor.is_some() {
             need_redraw = handle_group_editor_input(
                 event,
                 scale,
                 &ms.overlay,
                 &mut edit,
                 &monitor_id,
                 &window_snapshot,
                 &monitor_bounds,
                 &monitor_geometry,
             );
         } else if let Some(scale) = scale {
             if handle_timeline_hover_input(&mut edit, &mut videos, event, scale, &monitor_id) {
                 need_redraw = true;
             }
         }
     }
     ```
   * **Клавиатура ([line 3502](file:///C:/resticker/crates/resticker/src/overlay_manager.rs#L3502)):**
     Изменить условие с `if edit.active` на `if edit.active || edit.group_editor.is_some()`. В `handle_key` добавить обработку `Esc` для закрытия `group_editor`.
   * **Символы `Char` ([line 3560](file:///C:/resticker/crates/resticker/src/overlay_manager.rs#L3560)):**
     Изменить условие с `if edit.active` на `if edit.active || edit.group_editor.is_some()`.
5. **Отрисовка в `redraw()` ([line 11020](file:///C:/resticker/crates/resticker/src/overlay_manager.rs#L11020)):**
   Встроить отрисовку панели в общий пайплайн (поверх сцены, рядом с `preset_picker`):
   ```rust
   if let Some(state) = &edit.group_editor {
       if state.monitor_id == *monitor_id {
           let mut prims = Vec::new();
           state.panel.draw(&mut prims);
           // Отрисовка перетаскиваемой карточки окна поверх панели (если активен драг)
           if let Some(drag) = &state.drag {
               draw_dragged_window_card(&mut prims, drag);
           }
           primitives_to_sprites(&prims, ui_cache, renderer, monitor_id, text_scale, &mut frame);
       }
   }
   ```
   **Результат:** Все стикеры остаются под своими окклюдерами, скрытые стикеры не видны, черного затемнения нет, пины заблокированы и удерживаются, а панель меню групп полностью интерактивна.

---

## 5. Перетаскивание (Drag-and-Drop) внутри панели

### 5.1. Анализ `crates/rst-render/src/widgets.rs`
В модуле `widgets.rs` **НЕТ готового механизма Drag-and-Drop** между виджетами (перетаскивание карточки из ленты в слот).
* Есть только базовое перечисление `PointerEvent` ([lines 667-675](file:///C:/resticker/crates/rst-render/src/widgets.rs#L667-L675)):
  ```rust
  pub enum PointerEvent {
      Down { pos: Point },
      Move { pos: Point },
      Up { pos: Point },
  }
  ```
* Единственные виджеты с внутренним драгом — `Slider` ([line 960](file:///C:/resticker/crates/rst-render/src/widgets.rs#L960)) и `ScrollBar` ([line 1213](file:///C:/resticker/crates/rst-render/src/widgets.rs#L1213)), которые хранят приватный флаг `dragging: bool` для одномерного смещения ползунка.

### 5.2. Как перетаскивание сделано у стикеров и закреплённых окон
В `overlay_manager.rs` перетаскивание построено через автомат жестов:
1. Определение `enum Gesture` ([lines 1129-1155](file:///C:/resticker/crates/resticker/src/overlay_manager.rs#L1129-L1155)):
   `Gesture::Drag { start: GestureStart, grab_dx: f64, grab_dy: f64 }`.
2. В `MouseDown` ([line 9495](file:///C:/resticker/crates/resticker/src/overlay_manager.rs#L9495)): определяется хит-тест тела стикера (`Zone::StickerBody`), вычисляется смещение курсора относительно центра `(grab_dx, grab_dy)` и создаётся `edit.gesture = Some(Gesture::Drag { ... })`.
3. В `MouseMove` ([line 9623](file:///C:/resticker/crates/resticker/src/overlay_manager.rs#L9623)): позиция обновляется `(dip_x - grab_dx, dip_y - grab_dy)`, вызывается снап и перерисовка `need_redraw = true`.
4. В `MouseUp` ([line 9940](file:///C:/resticker/crates/resticker/src/overlay_manager.rs#L9940)): положение фиксируется, коммитится шаг undo, `edit.gesture = None`.
5. В `CaptureLost` / `force_release_capture`: жест откатывается к `GestureStart`.

Аналогично сделан `PinnedGesture::Drag` ([line 9517](file:///C:/resticker/crates/resticker/src/overlay_manager.rs#L9517)) для перетаскивания сторонних окон.

### 5.3. Применимость приёма для меню групп
**Этот приём идеально подходит** для Drag-and-Drop карточек окон в слоты раскладки.

Рекомендуемая структура состояния драга внутри `GroupEditorState`:
```rust
pub struct GroupCardDrag {
    pub hwnd: usize,
    pub title: String,
    pub icon: Option<(u64, WindowIcon)>,
    pub grab_offset: (f64, f64),
    pub current_pos: (f64, f64),
    pub source_slot: Option<usize>, // None если тащим из нижней ленты
}
```
**Логика работы:**
1. **`MouseDown`:**
   * Если курсор попал по карточке окна в нижней ленте открытых окон (или по уже заполненному слоту раскладки) — создаётся `state.drag = Some(GroupCardDrag { ... })`.
2. **`MouseMove`:**
   * Если `state.drag.is_some()`:
     * Обновляется `drag.current_pos = (dip_x, dip_y)`.
     * Выполняется хит-тест слотов верхней раскладки (`box_contains`). При наведении на слот выставляется визуальная подсветка слота (hover drop-target).
     * `need_redraw = true`.
3. **`MouseUp`:**
   * Если `state.drag` активен:
     * Проверяется, над каким слотом раскладки отпустили кнопку мыши.
     * Если над слотом `K`: окно `drag.hwnd` назначается в слот `K`.
     * Если отпустили в пустое место: сброс драга (карточка возвращается в ленту).
     * `state.drag = None; need_redraw = true;`.
4. **Отрисовка:**
   * При активном драге поверх панели рисуется полупрозрачная плавающая миниатюра карточки под `drag.current_pos` через `Primitive::Fill` / `Primitive::Rgba` / `Primitive::Text`.

---

## 6. Риски и подводные камни

### 6.1. Захват мыши (Mouse Capture) и «залипание» ввода
* **Риск:** Если пользователь нажал ЛКМ по карточке и начал драг, окно оверлея захватывает мышь (`MouseCapture::handle_message` зовёт `SetCapture`). Если во время перетаскивания нажать `Esc` или переключить окно через `Alt+Tab`, захват мыши может остаться на оверлее, заблокировав мышь всей ОС.
* **Решение:** В любых точках закрытия меню групп (`Esc`, потеря фокуса, `ToggleGroupEditor`, клик мимо) **обязательно** вызывать `overlay.force_release_capture()` ([line 663](file:///C:/resticker/crates/rst-win32/src/overlay.rs#L663)).

### 6.2. Z-Order и наложение на закреплённые окна (`Pinned Windows`)
* **Риск:** В обычном режиме работы (`!edit.active`) закреплённые окна (`WindowPins`) находятся в системной полосе `WS_EX_TOPMOST`. Активное закреплённое окно может оказаться выше окна оверлея по z-order, и меню групп частично окажется под чужим окном.
* **Решение:** При показе меню групп вызывать `overlay.raise_above_pinned(&edit.pinned_windows)` ([line 606](file:///C:/resticker/crates/rst-win32/src/overlay.rs#L606)) или `SetWindowPos(self.hwnd, Some(HWND_TOPMOST), ...)`, чтобы оверлей гарантированно лёг поверх всех окон.

### 6.3. Производительность снимков окон и зависшие приложения
* **Риск:** В требованиях сказано: «лента снимков открытых окон». Если пытаться делать честные попиксельные скриншоты каждого окна (`PrintWindow` / `BitBlt` / Desktop Duplication) синхронно в момент нажатия `Alt+Shift+G`:
  1. Зависшее приложение другого процесса (Notepad, зависший Chrome) заблокирует вызов `PrintWindow` на несколько секунд, заморозив весь оверлей.
  2. Захват 20-30 окон создаст ощутимый фриз (50-200 мс) при открытии меню.
* **Рекомендация:**
  * Использовать `window_snapshot` из `WindowTracker` (`rst_win32::window_enum::WindowInfo`), где уже есть кэшированные иконки (`WindowIcon`), заголовки и пути процессов.
  * Если требуются именно графические миниатюры (thumbnails): использовать DWM Thumbnails API (`DwmRegisterThumbnail`), либо асинхронный пул захвата с таймаутом через `SendMessageTimeoutW` (по аналогии с [line 173](file:///C:/resticker/crates/resticker/src/overlay_manager.rs#L173) в `window_enum.rs`), либо генерировать схематичные превью с иконкой и заголовком окна (как в `window_pick_list.rs`).

### 6.4. Жизненный цикл окон во время открытого меню
* **Риск:** Пользователь открыл меню групп, и в этот момент стороннее окно закрылось или свернулось.
* **Решение:** `overlay_manager.rs` получает `OverlayMessage::Windows(Changed(windows))` на изменения в системе. Меню групп должно валидировать список слотов и ленту против актуального `window_snapshot`, удаляя окна, чьи `HWND` больше не существуют.

### 6.5. Коллизия пространств `WidgetId`
* **Риск:** В `rst-render` идентификаторы виджетов `WidgetId` (u32) разделены по статическим диапазонам:
  * Тулбар: `0..8`
  * Панель у курсора: `100+`
  * Панель слоёв видимости (`window_picker`): `200+`
  * Панель пресетов (`preset_picker`): `400+`
  * Список окон для закрепления (`window_pick_list`): `500+`
  * Баннер сообщений: `900+`
* **Решение:** Для панели меню групп выделить диапазон `600..799` (например, `PANEL_ID = 600`, `LAYOUT_SLOT_BASE = 601`, `WINDOW_CARD_BASE = 650`, `BTN_APPLY = 700`, `BTN_CLOSE = 701`).

### 6.6. Пересборка масок окклюзии при 60 FPS во время Drag-and-Drop
* **Риск:** Когда `edit.active == false`, функция `redraw()` при каждом `MouseMove` строит GPU-текстуры масок окклюзии для всех стикеров на мониторе ([lines 11118-11124](file:///C:/resticker/crates/resticker/src/overlay_manager.rs#L11118-L11124)). Во время активного перетаскивания карточки это может создавать избыточную нагрузку на D3D11 при большом числе стикеров.
* **Примечание:** Для типичного числа стикеров (10-30 шт.) это укладывается в бюджет кадра, но координатору стоит учесть возможность пропуска пересборки масок, пока активно меню групп.

---

## 7. Сводная таблица архитектурного решения

| Компонент | Текущее решение (Presets / Edit Mode) | Проектное решение для Меню Групп |
|---|---|---|
| **Глобальный хоткей** | `F2` / `EDIT_HOTKEY_ID` (`ToggleEditMode`) | `Alt+Shift+G` / `GROUP_EDITOR_HOTKEY_ID` (`ToggleGroupEditor`) |
| **Флаг `edit.active`** | `true` (полный режим редактирования) | `false` (режим редактирования стикеров НЕ включается) |
| **Состояние панели** | `EditState.preset_picker` | `EditState.group_editor: Option<GroupEditorState>` |
| **Кликопрозрачность** | `set_click_through(false)` на всех экранах | `set_interactive(true)` на время открытого меню; возврат `set_click_through(true)` при закрытии |
| **Маски окклюзии** | Отключаются (`if edit.active`) | **Остаются включёнными** (стикеры за окнами не вылезают) |
| **Фоновое затемнение** | 25% черный оверлей на весь экран | Локальное затемнение меню (или без затемнения экрана) |
| **Перетаскивание (DnD)** | Жесты `Gesture::Drag` в `EditState` | Внутренний `GroupCardDrag` внутри `GroupEditorState` на событиях `PointerEvent` |
| **Диапазон `WidgetId`** | `400..499` | `600..799` |

Отчёт подготовлен для использования координатором перед началом реализации компонента групп.
