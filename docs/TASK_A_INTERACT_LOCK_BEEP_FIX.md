# Task A — interact-lock beep fix (bugfix round 2)

## Bug report
"Когда блокирую окно любым/обоими замками и кликаю по нему — Windows играет
ошибку/«динг» — очень раздражает, уберите звук при клике по заблокированному окну."

## Confirmed mechanism (empirically, via live test `interact_guard_swallows_real_click_on_locked_window`)
- `EnableWindow(hwnd, FALSE)` ставит `WS_DISABLED`. Клик по такому top-level окну
  ОС обрабатывает **сама, до диспетчеризации**: окну НЕ приходят ни
  `WM_NCHITTEST`, ни `WM_MOUSEACTIVATE`, ни `WM_LBUTTONDOWN` (проверено логом
  wndproc реального окна при реальном клике через SendInput). Клик выбрасывается,
  а win32k играет системный звук.
- resticker не вызывает `MessageBeep`/`Beep` сам (grep по всему workspace — пусто):
  звук целиком провоцируется disabled-состоянием.

## Why the chosen fix (WH_MOUSE_LL) and not the alternatives
- **Subclassing (`SetWindowLongPtrW(GWLP_WNDPROC)`) / WM_MOUSEACTIVATE-перехват —
  НЕВОЗМОЖЕН**: окно не получает вообще никакого сообщения клика/активации
  (проверено), ловить нечего. Плюс смена wndproc для ЧУЖОГО процесса (Notepad,
  браузер — реальные цели пинов) запрещена ОС (`ERROR_ACCESS_DENIED`).
- **`SystemParametersInfo(SPI_SETBEEP)`** — глобальная системная настройка;
  не использована (хук чище и не трогает чужие бипы).
- **Выбранный путь**: глобальный низкоуровневый хук `WH_MOUSE_LL` на собственном
  потоке-помпе. Если нажатие кнопки попало в экранный прямоугольник
  interact-locked окна И это окно — реальная цель клика в точке (верхнее видимое
  НЕ-transparent окно z-order, содержащее точку; `WS_EX_TRANSPARENT`-оверлеи
  пропускаются — как и роутит система), хук возвращает ненулевое значение:
  событие выбрасывается из input-очереди ДО системного hit-testing — ни бипа, ни
  попытки активации, ни доставки клика не происходит вовсе. `EnableWindow(FALSE)`
  остаётся главным механизмом блокировки (клавиатура и сообщения).

## Implementation (all in `crates/rst-win32/src/window_pin.rs`)
- Новый модуль `interact_guard` (под `impl WindowPins`):
  - `WH_MOUSE_LL` на потоке с `GetMessageW`-помпом; `SetWindowsHookExW` ставится
    при первом interact-locked окне, снимается (WM_QUIT + join) при последнем.
  - Решение «кто выигрывает клик» вынесено в чистую функцию `target_wins_click`
    (итератор z-order), реальный обход десктопа — `is_effective_click_target`.
  - Глотаются только down/up кнопок (не `WM_MOUSEMOVE`); down/up-пары
    трекаются атомарным флагом `SWALLOWED_DOWN` (не трогаем UP после драга,
    начавшегося вне заблокированного окна).
  - Колбэк быстрый (короткий лок только на клон множества), try-lock-семантика
    не нужна — лок держится микросекунды.
- `set_interact_lock`/`unpin`/`handle_snapshot` синхронизируют глобальное
  множество хука с `interact_locked`.

## Tests
- Unit (детерминированные, не зависят от живого десктопа): клик по центру
  locked-окна поглощается; вне прямоугольника — нет; enabled-окно поверх locked —
  клик не поглощается; `WS_EX_TRANSPARENT`-оверлей не мешает; чужие disabled-окна
  не трогаются; мёртвое окно игнорируется.
- Live (ignored, запуск вручную):
  `interact_guard_swallows_real_click_on_locked_window` — реальный клик SendInput:
  (a) disabled-окно БЕЗ хука: система сама съедает клик (нет WM_MOUSEACTIVATE /
  WM_NCHITTEST / WM_LBUTTONDOWN — механизм бипа), приходит только служебное
  WM_NCACTIVATE/WM_ACTIVATE; (b) с хуком: НИ ОДНОГО сообщения клика/активации
  не приходит (клик поглощён до системы → бипа нет), foreground не меняется,
  `IsWindowEnabled == false` — ввод по-прежнему заблокирован.
- `real_desktop_zorder_swallow` (ignored) — сквозная проверка живого обхода
  z-order.

## Verification
- `cargo test -p rst-win32 --lib` — 168 passed, 0 failed (16 ignored — ручные).
- `cargo clippy --workspace --all-targets -- -D warnings` — чисто.
- `cargo build --release -p resticker` — собрано.
- Итоговое подтверждение «бип исчез» на слух — за пользователем: по договорённости
  с координатором запущенный resticker (PID 20668) не трогали, человеческое ухо
  на живом десктопе не задействовано. Автоматическое live-доказательство:
  клик, уходящий в систему, полностью перехватывается ДО input-routing
  (0 сообщений клика/активации в wndproc окна) — структурно бипу неоткуда взяться.

## Out of scope / notes
- Move-lock (drag-jitter) — параллельная задача в этом же файле; правки не
  пересекались (координатор разблокировал импорты их теста).
- `SPI_SETBEEP` не использовался — tradeoff не применился.